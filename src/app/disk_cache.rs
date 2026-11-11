//! A small, bounded on-disk cache of downloaded artwork bytes.
//!
//! Trade-offs, chosen so the cache never costs more than it saves:
//! * Bounded: at most `cap` bytes; the oldest files are removed first, and
//!   only when a write pushes the total over the cap.
//! * Reads never write: no timestamp touching, so a cache hit is one file read.
//! * Everything runs on the HTTP runtime's blocking pool, never the UI thread.
//! * Best effort: any I/O problem falls back to the network without noise.
//! * Stores the encoded download, not decoded pixels, so entries stay small.
use md5::{Digest, Md5};
use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

/// Most of what is needed to repaint a screen of covers, while staying small.
pub(super) const DEFAULT_CAP_BYTES: u64 = 48 * 1024 * 1024;
const TEMP_EXTENSION: &str = "tmp";

/// The first byte of each file names its format, so a hit can rebuild the
/// `Content-Type` without a second lookup.
const FORMATS: [(u8, &str, &[u8]); 4] = [
    (1, "image/jpeg", &[0xFF, 0xD8, 0xFF]),
    (2, "image/png", &[0x89, b'P', b'N', b'G']),
    (3, "image/webp", b"RIFF"),
    (4, "image/gif", b"GIF8"),
];

pub(super) struct DiskCache {
    dir: PathBuf,
    cap: u64,
    total: OnceLock<AtomicU64>,
}

impl DiskCache {
    pub(super) fn new(dir: PathBuf, cap: u64) -> Self {
        Self {
            dir,
            cap,
            total: OnceLock::new(),
        }
    }

    /// The cache in the user's cache directory, or `None` when there is none.
    pub(super) fn default_location() -> Option<Self> {
        let dirs = directories::ProjectDirs::from("dev", "twoducks", "cadence")?;
        Some(Self::new(
            dirs.cache_dir().join("artwork"),
            DEFAULT_CAP_BYTES,
        ))
    }

    fn path(&self, url: &str) -> PathBuf {
        let digest = Md5::digest(url.as_bytes());
        let name = digest
            .iter()
            .fold(String::with_capacity(32), |mut hex, byte| {
                use std::fmt::Write as _;
                let _ = write!(hex, "{byte:02x}");
                hex
            });
        self.dir.join(name)
    }

    /// Bytes on disk. Counted once per run, then kept current by writes.
    fn total(&self) -> &AtomicU64 {
        self.total.get_or_init(|| {
            let mut total = 0;
            if let Ok(entries) = fs::read_dir(&self.dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if is_temp(&path) {
                        // Left behind by a crash mid-write.
                        let _ = fs::remove_file(&path);
                    } else if let Ok(metadata) = entry.metadata() {
                        total += metadata.len();
                    }
                }
            }
            AtomicU64::new(total)
        })
    }

    /// The cached download and its content type. Blocking: call from the
    /// blocking pool.
    pub(super) fn load(&self, url: &str) -> Option<(Vec<u8>, &'static str)> {
        let path = self.path(url);
        let mut bytes = fs::read(&path).ok()?;
        let format = bytes.first().copied()?;
        let (_, mime, magic) = FORMATS.iter().find(|(id, ..)| *id == format)?;
        if !bytes[1..].starts_with(magic) {
            // Never serve something that cannot be an image; drop it and refetch.
            self.remove(&path, bytes.len() as u64);
            return None;
        }
        bytes.remove(0);
        Some((bytes, mime))
    }

    /// Saves a finished download. Blocking. Returns whether it was stored.
    pub(super) fn store(&self, url: &str, content_type: &str, bytes: &[u8]) -> bool {
        let Some((id, _, magic)) = FORMATS
            .iter()
            .find(|(_, mime, _)| content_type.starts_with(mime))
        else {
            return false;
        };
        let size = bytes.len() as u64 + 1;
        // One file should never be able to flush the whole cache.
        if !bytes.starts_with(magic)
            || size > self.cap / 8
            || fs::create_dir_all(&self.dir).is_err()
        {
            return false;
        }
        let path = self.path(url);
        let temp = path.with_extension(format!("{}.{TEMP_EXTENSION}", std::process::id()));
        let written = (|| -> std::io::Result<()> {
            let mut file = fs::File::create(&temp)?;
            file.write_all(&[*id])?;
            file.write_all(bytes)?;
            file.flush()
        })();
        let replaced = fs::metadata(&path).map_or(0, |metadata| metadata.len());
        if written.is_err() || fs::rename(&temp, &path).is_err() {
            let _ = fs::remove_file(&temp);
            return false;
        }
        let total = self.total();
        total.fetch_sub(
            replaced.min(total.load(Ordering::Relaxed)),
            Ordering::Relaxed,
        );
        if total.fetch_add(size, Ordering::Relaxed) + size > self.cap {
            self.evict();
        }
        true
    }

    fn remove(&self, path: &Path, size: u64) {
        if fs::remove_file(path).is_ok() {
            let total = self.total();
            total.fetch_sub(size.min(total.load(Ordering::Relaxed)), Ordering::Relaxed);
        }
    }

    /// Removes the oldest files until the cache is at three quarters of its
    /// cap, so the next several writes do not each trigger a sweep.
    fn evict(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut files: Vec<(std::time::SystemTime, PathBuf, u64)> = entries
            .flatten()
            .filter(|entry| !is_temp(&entry.path()))
            .filter_map(|entry| {
                let metadata = entry.metadata().ok()?;
                Some((metadata.modified().ok()?, entry.path(), metadata.len()))
            })
            .collect();
        let mut total: u64 = files.iter().map(|(_, _, len)| len).sum();
        files.sort_by_key(|(modified, ..)| *modified);
        let target = self.cap / 4 * 3;
        for (_, path, len) in files {
            if total <= target {
                break;
            }
            if fs::remove_file(path).is_ok() {
                total = total.saturating_sub(len);
            }
        }
        self.total().store(total, Ordering::Relaxed);
    }

    /// Deletes every cached file. Blocking.
    pub(super) fn clear(&self) {
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let _ = fs::remove_file(entry.path());
            }
        }
        self.total().store(0, Ordering::Relaxed);
    }
}

fn is_temp(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == TEMP_EXTENSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cadence-disk-cache-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn jpeg(len: usize) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
        bytes.resize(len, 7);
        bytes
    }

    #[test]
    fn a_stored_image_round_trips_with_its_content_type() {
        let dir = scratch("roundtrip");
        let cache = DiskCache::new(dir.clone(), 1024 * 1024);
        assert!(cache.load("https://i.example/a").is_none());
        assert!(cache.store("https://i.example/a", "image/jpeg", &jpeg(500)));
        let (bytes, mime) = cache.load("https://i.example/a").unwrap();
        assert_eq!(bytes, jpeg(500));
        assert_eq!(mime, "image/jpeg");
        assert!(cache.load("https://i.example/other").is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn only_real_images_are_stored() {
        let dir = scratch("reject");
        let cache = DiskCache::new(dir.clone(), 1024 * 1024);
        assert!(!cache.store("u", "text/html", &jpeg(100)));
        assert!(!cache.store("u", "image/jpeg", b"<html>not an image</html>"));
        assert!(
            !cache.store("u", "image/jpeg", &jpeg(200_000)),
            "over cap/8"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_file_is_dropped_instead_of_served() {
        let dir = scratch("damaged");
        let cache = DiskCache::new(dir.clone(), 1024 * 1024);
        assert!(cache.store("u", "image/jpeg", &jpeg(100)));
        fs::write(cache.path("u"), [1, 0, 0, 0, 0]).unwrap();
        assert!(cache.load("u").is_none());
        assert!(!cache.path("u").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn exceeding_the_cap_removes_the_oldest_files_first() {
        let dir = scratch("evict");
        let cache = DiskCache::new(dir.clone(), 7_300);
        for n in 0..12 {
            assert!(cache.store(&format!("u{n}"), "image/jpeg", &jpeg(900)));
            // Distinct modification times so "oldest" is well defined.
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        let on_disk: u64 = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.metadata().unwrap().len())
            .sum();
        assert!(on_disk <= 7_300, "{on_disk} bytes on disk");
        assert!(cache.load("u0").is_none(), "oldest was evicted");
        assert!(cache.load("u11").is_some(), "newest survives");
        assert_eq!(cache.total().load(Ordering::Relaxed), on_disk);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn clear_empties_the_cache_and_the_count() {
        let dir = scratch("clear");
        let cache = DiskCache::new(dir.clone(), 1024 * 1024);
        cache.store("a", "image/jpeg", &jpeg(300));
        cache.store("b", "image/png", &{
            let mut png = vec![0x89, b'P', b'N', b'G'];
            png.resize(300, 1);
            png
        });
        cache.clear();
        assert!(cache.load("a").is_none() && cache.load("b").is_none());
        assert_eq!(cache.total().load(Ordering::Relaxed), 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn crash_leftovers_are_not_counted_and_are_removed() {
        let dir = scratch("temp");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("abc.123.tmp"), [0u8; 1000]).unwrap();
        let cache = DiskCache::new(dir.clone(), 1024 * 1024);
        assert_eq!(cache.total().load(Ordering::Relaxed), 0);
        assert!(!dir.join("abc.123.tmp").exists());
        let _ = fs::remove_dir_all(dir);
    }
}
