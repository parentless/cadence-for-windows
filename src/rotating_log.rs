//! A continuously size-limited log with one previous file.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

pub(crate) struct RotatingLog {
    path: PathBuf,
    file: Option<File>,
    length: u64,
    limit: u64,
}

impl RotatingLog {
    pub(crate) fn open(path: PathBuf, limit: u64) -> io::Result<Self> {
        if limit == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "zero log limit",
            ));
        }
        // Older versions could leave oversized files. Keep their newest tail
        // once on startup; normal rotation never needs to read log contents.
        for candidate in [&path, &path.with_extension("previous.log")] {
            if fs::metadata(candidate)
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() > limit)
            {
                let mut old = OpenOptions::new().read(true).write(true).open(candidate)?;
                old.seek(SeekFrom::End(
                    -i64::try_from(limit).map_err(io::Error::other)?,
                ))?;
                let mut tail = Vec::new();
                Read::by_ref(&mut old).take(limit).read_to_end(&mut tail)?;
                old.seek(SeekFrom::Start(0))?;
                old.write_all(&tail)?;
                old.set_len(limit)?;
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let length = file.metadata()?.len();
        let mut writer = Self {
            path,
            file: Some(file),
            length,
            limit,
        };
        if length >= limit {
            writer.rotate()?;
        }
        Ok(writer)
    }

    fn rotate(&mut self) -> io::Result<()> {
        // Close before renaming: Windows does not support replacing an open file
        // with the same sharing behavior as Unix.
        self.file.take();
        let previous = self.path.with_extension("previous.log");
        let removed = match fs::remove_file(&previous) {
            Ok(()) => true,
            Err(error) => error.kind() == io::ErrorKind::NotFound,
        };
        if removed {
            let _ = fs::rename(&self.path, previous);
        }
        // If renaming failed, truncate the current file rather than allowing
        // unlimited growth. Failure to reopen is returned to the logger.
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        self.file = Some(file);
        self.length = 0;
        Ok(())
    }
}

impl Write for RotatingLog {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.length >= self.limit || self.file.is_none() {
            self.rotate()?;
        }
        let available = usize::try_from(self.limit - self.length).unwrap_or(usize::MAX);
        let count = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("log is closed"))?
            .write(&bytes[..bytes.len().min(available)])?;
        self.length += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.file {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "cadence-log-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn continuous_and_oversized_writes_remain_bounded() {
        let directory = TestDirectory::new();
        let path = directory.path().join("cadence.log");
        let previous = path.with_extension("previous.log");
        let mut log = RotatingLog::open(path.clone(), 32).unwrap();
        for _ in 0..1000 {
            log.write_all(&[b'x'; 103]).unwrap();
            assert!(fs::metadata(&path).unwrap().len() <= 32);
            assert!(fs::metadata(&previous).unwrap().len() <= 32);
        }
        log.write_all(b"tail").unwrap();
        log.flush().unwrap();
        assert!(fs::read(path).unwrap().ends_with(b"tail"));
    }

    #[test]
    fn legacy_oversized_logs_keep_only_the_newest_tail() {
        let directory = TestDirectory::new();
        let path = directory.path().join("cadence.log");
        let mut bytes = vec![b'x'; 100];
        bytes.extend_from_slice(b"new-tail");
        fs::write(&path, &bytes).unwrap();
        let _log = RotatingLog::open(path.clone(), 8).unwrap();
        assert_eq!(
            fs::read(path.with_extension("previous.log")).unwrap(),
            b"new-tail"
        );
        assert_eq!(fs::metadata(path).unwrap().len(), 0);
    }

    #[test]
    fn startup_rotation_and_failed_backup_still_limit_current_file() {
        let directory = TestDirectory::new();
        let path = directory.path().join("cadence.log");
        fs::write(&path, [b'x'; 32]).unwrap();
        let mut log = RotatingLog::open(path.clone(), 32).unwrap();
        assert_eq!(
            fs::read(path.with_extension("previous.log")).unwrap(),
            [b'x'; 32]
        );
        log.write_all(&[b'y'; 32]).unwrap();
        fs::remove_file(path.with_extension("previous.log")).unwrap();
        fs::create_dir(path.with_extension("previous.log")).unwrap();
        log.write_all(b"new").unwrap();
        assert_eq!(fs::read(path).unwrap(), b"new");
    }
}
