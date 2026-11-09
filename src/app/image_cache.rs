use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::{FutureExt as _, future::Shared};
use gpui_kit::{
    App, AppContext as _, Asset, AssetLogger, Entity, ImageAssetLoader, ImageCache,
    ImageCacheError, RenderImage, Resource, Task, Window,
};

const MAX_IMAGES: usize = 48;
const MAX_DECODED_BYTES: usize = 32 * 1024 * 1024;
const FAILURE_RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct DecodedArtwork {
    image: Arc<RenderImage>,
}

type ImageLoadingTask = Shared<Task<Result<DecodedArtwork, ImageCacheError>>>;

enum CacheEntry {
    Loading {
        image: ImageLoadingTask,
        retry_ready: Arc<AtomicBool>,
        notification: Option<Task<()>>,
    },
    Loaded {
        image: Arc<RenderImage>,
        decoded_bytes: usize,
    },
    Failed {
        error: ImageCacheError,
        retry_ready: Arc<AtomicBool>,
        _notification: Task<()>,
    },
}

impl CacheEntry {
    fn get(&mut self) -> (Option<Result<Arc<RenderImage>, ImageCacheError>>, usize) {
        match self {
            Self::Loading {
                image,
                retry_ready,
                notification,
            } => {
                let Some(result) = image.now_or_never() else {
                    return (None, 0);
                };
                match result {
                    Ok(DecodedArtwork { image }) => {
                        let decoded_bytes = (0..image.frame_count())
                            .filter_map(|frame| image.as_bytes(frame))
                            .map(<[u8]>::len)
                            .sum();
                        *self = Self::Loaded {
                            image: image.clone(),
                            decoded_bytes,
                        };
                        (Some(Ok(image)), decoded_bytes)
                    }
                    Err(error) => {
                        *self = Self::Failed {
                            error: error.clone(),
                            retry_ready: retry_ready.clone(),
                            _notification: notification
                                .take()
                                .expect("image notification task disappeared"),
                        };
                        (Some(Err(error)), 0)
                    }
                }
            }
            Self::Loaded { image, .. } => (Some(Ok(image.clone())), 0),
            Self::Failed { error, .. } => (Some(Err(error.clone())), 0),
        }
    }

    fn retry_ready(&self) -> bool {
        matches!(self, Self::Failed { retry_ready, .. } if retry_ready.load(Ordering::Acquire))
    }
}

pub(super) struct BoundedImageCache {
    items: HashMap<Resource, CacheEntry>,
    recency: VecDeque<Resource>,
    decoded_bytes: usize,
    hits: u64,
    misses: u64,
    retries: u64,
    evictions: u64,
}

impl BoundedImageCache {
    pub(super) fn usage(&self) -> (usize, usize, bool) {
        (self.decoded_bytes, MAX_DECODED_BYTES, self.evictions > 0)
    }

    /// Cancels pending requests and releases decoded/GPU artwork. The cache owns
    /// no credentials or audio files; visible artwork can be requested again.
    pub(super) fn clear(&mut self, window: &mut Window, cx: &mut App) {
        for (_, entry) in self.items.drain() {
            if let CacheEntry::Loaded { image, .. } = entry {
                cx.drop_image(image, Some(window));
            }
        }
        self.recency.clear();
        self.decoded_bytes = 0;
        self.evictions = 0;
    }

    pub(super) fn new(cx: &mut App) -> Entity<Self> {
        let cache = cx.new(|_| Self {
            items: HashMap::new(),
            recency: VecDeque::new(),
            decoded_bytes: 0,
            hits: 0,
            misses: 0,
            retries: 0,
            evictions: 0,
        });
        cx.observe_release(&cache, |cache, cx| {
            for item in cache.items.values_mut() {
                if let CacheEntry::Loaded { image, .. } = item {
                    cx.drop_image(image.clone(), None);
                }
            }
        })
        .detach();
        cache
    }

    #[cfg(test)]
    pub(super) fn seed_artist_fixture(&mut self, url: &str, image: Arc<RenderImage>) {
        let blurred = blur_artist_image(&image).expect("fixture blur");
        for (key, image) in [
            (Resource::Uri(url.to_owned().into()), image),
            (
                Resource::Embedded(format!("__cadence_artist_blur__:{url}").into()),
                blurred,
            ),
        ] {
            let decoded_bytes = image.as_bytes(0).unwrap().len();
            self.decoded_bytes += decoded_bytes;
            self.items.insert(
                key.clone(),
                CacheEntry::Loaded {
                    image,
                    decoded_bytes,
                },
            );
            self.touch(&key);
        }
    }

    /// A static, reduced-resolution derivative sharing the normal LRU budgets.
    /// The portrait and backdrop share a single download and decode.
    pub(super) fn artist_backdrop(
        &mut self,
        url: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = Resource::Embedded(format!("__cadence_artist_blur__:{url}").into());
        if let Some(entry) = self.items.get_mut(&key) {
            let (result, loaded_bytes) = entry.get();
            self.decoded_bytes += loaded_bytes;
            self.hits += 1;
            self.touch(&key);
            self.evict_to_budget(0, window, cx);
            return result;
        }
        let source = match self.load(&Resource::Uri(url.to_owned().into()), window, cx)? {
            Ok(image) => image,
            Err(error) => return Some(Err(error)),
        };
        self.misses += 1;
        self.evict_to_budget(1, window, cx);
        let image = cx
            .background_executor()
            .spawn(async move {
                let image = blur_artist_image(&source)?;
                Ok(DecodedArtwork { image })
            })
            .shared();
        self.insert_loading(&key, image, window, cx);
        None
    }

    fn insert_loading(
        &mut self,
        resource: &Resource,
        image: ImageLoadingTask,
        window: &mut Window,
        cx: &mut App,
    ) {
        let retry_ready = Arc::new(AtomicBool::new(false));
        let entity = window.current_view();
        let notification = window.spawn(cx, {
            let image = image.clone();
            let retry_ready = retry_ready.clone();
            async move |cx| {
                let failed = image.await.is_err();
                cx.on_next_frame(move |_, cx| cx.notify(entity));
                if failed {
                    cx.background_executor().timer(FAILURE_RETRY_DELAY).await;
                    retry_ready.store(true, Ordering::Release);
                    cx.on_next_frame(move |_, cx| cx.notify(entity));
                }
            }
        });
        self.items.insert(
            resource.clone(),
            CacheEntry::Loading {
                image,
                retry_ready,
                notification: Some(notification),
            },
        );
        self.touch(resource);
    }

    fn touch(&mut self, resource: &Resource) {
        self.recency.retain(|cached| cached != resource);
        self.recency.push_back(resource.clone());
    }

    fn evict_to_budget(&mut self, reserved_entries: usize, window: &mut Window, cx: &mut App) {
        while exceeds_cache_budget(self.items.len(), reserved_entries, self.decoded_bytes) {
            let Some(resource) = self.recency.pop_front() else {
                break;
            };
            if let Some(item) = self.items.remove(&resource) {
                if let CacheEntry::Loaded {
                    image,
                    decoded_bytes,
                    ..
                } = item
                {
                    self.decoded_bytes = self.decoded_bytes.saturating_sub(decoded_bytes);
                    cx.drop_image(image, Some(window));
                }
                self.evictions += 1;
            }
        }
    }
}

fn exceeds_cache_budget(item_count: usize, reserved_entries: usize, decoded_bytes: usize) -> bool {
    item_count + reserved_entries > MAX_IMAGES
        || (decoded_bytes > MAX_DECODED_BYTES && item_count > 1)
}

impl ImageCache for BoundedImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        if self
            .items
            .get(resource)
            .is_some_and(CacheEntry::retry_ready)
        {
            self.items.remove(resource);
            self.recency.retain(|cached| cached != resource);
            self.retries += 1;
        }
        if self.items.contains_key(resource) {
            self.hits += 1;
            self.touch(resource);
            let (result, loaded_bytes) = self
                .items
                .get_mut(resource)
                .expect("cached image disappeared")
                .get();
            if loaded_bytes > 0 {
                self.decoded_bytes += loaded_bytes;
                self.evict_to_budget(0, window, cx);
            }
            return result;
        }

        self.misses += 1;
        self.evict_to_budget(1, window, cx);
        let future = AssetLogger::<ImageAssetLoader>::load(resource.clone(), cx);
        let image = cx
            .background_executor()
            .spawn(async move {
                let image = future.await?;
                Ok(DecodedArtwork { image })
            })
            .shared();
        self.insert_loading(resource, image, window, cx);
        None
    }
}

/// Work runs on the background executor, never in layout or paint. At most
/// 512 x 512 pixels are retained, and only the first frame is used.
fn blur_artist_image(source: &RenderImage) -> Result<Arc<RenderImage>, ImageCacheError> {
    let invalid = || ImageCacheError::Other(Arc::new(anyhow::anyhow!("invalid artist image")));
    let size = source.size(0);
    let pixels = image::RgbaImage::from_raw(
        u32::try_from(size.width.0).map_err(|_| invalid())?,
        u32::try_from(size.height.0).map_err(|_| invalid())?,
        source.as_bytes(0).ok_or_else(invalid)?.to_vec(),
    )
    .ok_or_else(invalid)?;
    if pixels.width() == 0 || pixels.height() == 0 {
        return Err(invalid());
    }
    let ratio = (512. / pixels.width().max(pixels.height()) as f32).min(1.);
    let width = ((pixels.width() as f32 * ratio) as u32).max(1);
    let height = ((pixels.height() as f32 * ratio) as u32).max(1);
    let reduced = image::imageops::resize(
        &pixels,
        width,
        height,
        image::imageops::FilterType::Triangle,
    );
    let blurred = image::imageops::blur(&reduced, 6.);
    // GPUI stores BGRA channels. Image operations preserve that channel order.
    Ok(Arc::new(RenderImage::new([image::Frame::new(blurred)])))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(name: &'static str) -> Resource {
        Resource::Embedded(name.into())
    }

    #[test]
    fn artist_blur_is_static_bounded_and_preserves_channel_order() {
        let pixels = image::RgbaImage::from_pixel(1200, 800, image::Rgba([20, 80, 160, 255]));
        let original = RenderImage::new([image::Frame::new(pixels)]);
        let blurred = blur_artist_image(&original).unwrap();
        assert_eq!(blurred.frame_count(), 1);
        assert_eq!(blurred.size(0).width.0, 512);
        assert!(blurred.size(0).height.0 <= 512);
        let pixel = &blurred.as_bytes(0).unwrap()[..4];
        for (actual, expected) in pixel.iter().zip([20_u8, 80, 160, 255]) {
            assert!(actual.abs_diff(expected) <= 1);
        }
        assert!(blurred.as_bytes(0).unwrap().len() <= 512 * 512 * 4);
        assert!(blur_artist_image(&RenderImage::new(Vec::<image::Frame>::new())).is_err());
    }

    #[test]
    fn cache_budget_combines_byte_and_entry_limits() {
        assert!(!exceeds_cache_budget(1, 0, MAX_DECODED_BYTES + 1));
        assert!(exceeds_cache_budget(2, 0, MAX_DECODED_BYTES + 1));
        assert!(exceeds_cache_budget(MAX_IMAGES, 1, 0));
    }

    #[test]
    fn touching_an_item_moves_it_to_the_lru_tail() {
        let first = resource("first");
        let second = resource("second");
        let mut cache = BoundedImageCache {
            items: HashMap::new(),
            recency: VecDeque::from([first.clone(), second.clone()]),
            decoded_bytes: 0,
            hits: 0,
            misses: 0,
            retries: 0,
            evictions: 0,
        };

        cache.touch(&first);

        assert_eq!(cache.recency, VecDeque::from([second, first]));
    }
}
