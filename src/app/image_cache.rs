use futures::{AsyncReadExt as _, FutureExt as _, future::Shared};
use gpui_kit::{
    App, AppContext as _, Asset, AssetLogger, Entity, EntityId, ImageAssetLoader, ImageCache,
    ImageCacheError, RenderImage, Resource, Task, WeakEntity, Window,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

// Small rail thumbnails and a dense discography can exceed 48 simultaneously
// visible URLs. Retain more keys while keeping the same combined byte budget.
const MAX_IMAGES: usize = 128;
const MAX_DECODED_BYTES: usize = 32 * 1024 * 1024;
const FAILURE_RETRY_DELAY: Duration = Duration::from_secs(5);
const IMAGE_WORKERS: usize = 2;

#[derive(Clone)]
struct DecodedArtwork {
    image: Arc<RenderImage>,
    source: Option<Arc<[u8]>>,
}
type ImageLoadingTask = Shared<Task<Result<DecodedArtwork, ImageCacheError>>>;

struct CacheEntry {
    target: u32,
    waiters: Vec<EntityId>,
    visible_demands: Vec<(EntityId, u64)>,
    image: Option<Arc<RenderImage>>,
    source: Option<Arc<[u8]>>,
    bytes: usize,
    request: Option<ImageLoadingTask>,
    error: Option<ImageCacheError>,
    retry_ready: Arc<AtomicBool>,
    _notification: Option<Task<()>>,
}
impl CacheEntry {
    fn get(
        &mut self,
        window: &mut Window,
        cx: &mut App,
    ) -> (
        Option<Result<Arc<RenderImage>, ImageCacheError>>,
        usize,
        usize,
    ) {
        let before = self.bytes;
        if let Some(result) = self.request.as_mut().and_then(|task| task.now_or_never()) {
            self.request = None;
            match result {
                Ok(artwork) => {
                    if let Some(old) = self.image.replace(artwork.image) {
                        cx.drop_image(old, Some(window));
                    }
                    self.source = artwork.source;
                    self.bytes = self.image.as_ref().map_or(0, |image| image_bytes(image))
                        + self.source.as_ref().map_or(0, |source| source.len());
                    self.error = None;
                }
                Err(error) => self.error = Some(error),
            }
        }
        let result = self
            .image
            .as_ref()
            .map(|image| Ok(image.clone()))
            .or_else(|| self.error.as_ref().map(|error| Err(error.clone())));
        (result, before, self.bytes)
    }
}
fn image_bytes(image: &RenderImage) -> usize {
    (0..image.frame_count())
        .filter_map(|frame| image.as_bytes(frame))
        .map(<[u8]>::len)
        .sum()
}

/// One decoded size per URL. Compressed sources share the same byte/entry
/// budget and allow a larger view to upgrade without another network request.
pub(super) struct BoundedImageCache {
    items: HashMap<Resource, CacheEntry>,
    recency: VecDeque<Resource>,
    decoded_bytes: usize,
    hits: u64,
    misses: u64,
    retries: u64,
    evictions: u64,
    workers: Arc<tokio::sync::Semaphore>,
    owner: Option<WeakEntity<Self>>,
    visibility_batches: HashMap<EntityId, u64>,
    visibility_generation: u64,
}
impl BoundedImageCache {
    pub(super) fn usage(&self) -> (usize, usize, bool) {
        (self.decoded_bytes, MAX_DECODED_BYTES, self.evictions > 0)
    }
    pub(super) fn clear(&mut self, window: &mut Window, cx: &mut App) {
        for (_, entry) in self.items.drain() {
            if let Some(image) = entry.image {
                cx.drop_image(image, Some(window));
            }
        }
        self.recency.clear();
        self.visibility_batches.clear();
        self.decoded_bytes = 0;
        self.evictions = 0;
    }
    pub(super) fn new(cx: &mut App) -> Entity<Self> {
        let cache = cx.new(|cx| Self {
            items: HashMap::new(),
            recency: VecDeque::new(),
            decoded_bytes: 0,
            hits: 0,
            misses: 0,
            retries: 0,
            evictions: 0,
            workers: Arc::new(tokio::sync::Semaphore::new(IMAGE_WORKERS)),
            owner: Some(cx.weak_entity()),
            visibility_batches: HashMap::new(),
            visibility_generation: 0,
        });
        cx.observe_release(&cache, |cache, cx| {
            for item in cache.items.values() {
                if let Some(image) = &item.image {
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
            let bytes = image_bytes(&image);
            self.decoded_bytes += bytes;
            self.items.insert(
                key.clone(),
                CacheEntry {
                    target: u32::MAX,
                    waiters: Vec::new(),
                    visible_demands: Vec::new(),
                    image: Some(image),
                    source: None,
                    bytes,
                    request: None,
                    error: None,
                    retry_ready: Arc::new(AtomicBool::new(false)),
                    _notification: None,
                },
            );
            self.touch(&key);
        }
    }
    /// Called only after artwork intersects its clipped viewport. One deferred
    /// sweep per drawing view drops its obsolete requests after all visible
    /// canvases have registered; it never requests another frame itself.
    pub(super) fn load_visible_sized(
        &mut self,
        resource: &Resource,
        side: u32,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let view = window.current_view();
        let epoch = if let Some(epoch) = self.visibility_batches.get(&view) {
            *epoch
        } else {
            self.visibility_generation = self.visibility_generation.wrapping_add(1);
            let epoch = self.visibility_generation;
            self.visibility_batches.insert(view, epoch);
            if let Some(owner) = self.owner.clone() {
                cx.defer(move |cx| {
                    let _ = owner.update(cx, |cache, _| cache.finish_visibility(view, epoch));
                });
            }
            epoch
        };
        let result = self.load_sized(resource, side, window, cx);
        if let Some(entry) = self.items.get_mut(resource)
            && entry.request.is_some()
        {
            if let Some(demand) = entry.visible_demands.iter_mut().find(|(id, _)| *id == view) {
                demand.1 = epoch;
            } else {
                entry.visible_demands.push((view, epoch));
            }
        }
        result
    }

    pub(super) fn forget_view(&mut self, view: EntityId) {
        self.visibility_generation = self.visibility_generation.wrapping_add(1);
        let epoch = self.visibility_generation;
        self.visibility_batches.insert(view, epoch);
        self.finish_visibility(view, epoch);
    }

    fn finish_visibility(&mut self, view: EntityId, epoch: u64) {
        if self.visibility_batches.get(&view) != Some(&epoch) {
            return;
        }
        self.visibility_batches.remove(&view);
        self.items.retain(|_, entry| {
            if entry.request.is_some()
                && entry
                    .visible_demands
                    .iter()
                    .any(|&(id, last)| id == view && last != epoch)
            {
                entry.visible_demands.retain(|&(id, _)| id != view);
                entry.waiters.retain(|id| *id != view);
                if entry.waiters.is_empty() {
                    // Drop both owners of the shared future. This cancels queued
                    // semaphore work and the abort-on-drop HTTP guard alike.
                    entry.request = None;
                    entry._notification = None;
                    entry.target = 0;
                    if entry.image.is_none() {
                        return false;
                    }
                }
            }
            true
        });
        self.recency.retain(|key| self.items.contains_key(key));
    }

    pub(super) fn artist_backdrop(
        &mut self,
        url: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = Resource::Embedded(format!("__cadence_artist_blur__:{url}").into());
        if let Some(entry) = self.items.get_mut(&key) {
            if entry.request.is_some() {
                let view = window.current_view();
                if !entry.waiters.contains(&view) {
                    entry.waiters.push(view);
                }
            }
            return self.cached(&key, window, cx);
        }
        let source =
            match self.load_sized(&Resource::Uri(url.to_owned().into()), 512, window, cx)? {
                Ok(image) => image,
                Err(error) => return Some(Err(error)),
            };
        self.misses += 1;
        self.evict_to_budget(1, window, cx);
        let workers = self.workers.clone();
        let task = cx
            .background_executor()
            .spawn(async move {
                let _permit = workers.acquire_owned().await.map_err(artwork_error)?;
                Ok(DecodedArtwork {
                    image: blur_artist_image(&source)?,
                    source: None,
                })
            })
            .shared();
        self.start(&key, 512, task, window, cx);
        None
    }
    fn cached(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let (result, before, after) = self.items.get_mut(resource)?.get(window, cx);
        self.decoded_bytes = self.decoded_bytes.saturating_sub(before) + after;
        self.hits += 1;
        self.touch(resource);
        self.evict_to_budget(0, window, cx);
        result
    }
    /// `side` is the square cover's physical size, including UI zoom and DPI.
    pub(super) fn load_sized(
        &mut self,
        resource: &Resource,
        side: u32,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let mut target = side.clamp(64, 2048).next_power_of_two();
        // Consume a completed smaller request before deciding whether to upgrade.
        if self.items.contains_key(resource) {
            let result = self.cached(resource, window, cx);
            if let Some(entry) = self.items.get_mut(resource) {
                if entry.request.is_some() {
                    let view = window.current_view();
                    if !entry.waiters.contains(&view) {
                        entry.waiters.push(view);
                    }
                    // Finish an in-flight download before upgrading. Its encoded
                    // source can then be reused rather than fetched twice.
                    return result;
                }
                let retry = entry.error.is_some() && entry.retry_ready.load(Ordering::Acquire);
                if entry.target >= target && !retry {
                    return result;
                }
                target = target.max(entry.target);
                if retry {
                    self.retries += 1;
                }
            }
        }
        let encoded = self
            .items
            .get(resource)
            .and_then(|entry| entry.source.clone());
        self.evict_to_budget(usize::from(!self.items.contains_key(resource)), window, cx);
        let fallback = AssetLogger::<ImageAssetLoader>::load(resource.clone(), cx);
        let uri = match resource {
            Resource::Uri(uri) => Some(uri.clone()),
            _ => None,
        };
        let client = cx.http_client();
        let workers = self.workers.clone();
        let task = cx
            .background_executor()
            .spawn(async move {
                let _permit = workers.acquire_owned().await.map_err(artwork_error)?;
                if let Some(uri) = uri {
                    let source = if let Some(source) = encoded {
                        source
                    } else {
                        let mut response = client
                            .get(uri.as_ref(), ().into(), true)
                            .await
                            .map_err(artwork_error)?;
                        if !response.status().is_success() {
                            return Err(artwork_error(anyhow::anyhow!(
                                "artwork request failed: {}",
                                response.status()
                            )));
                        }
                        let mut bytes = Vec::new();
                        response
                            .body_mut()
                            .take((super::http::MAX_IMAGE_DOWNLOAD_BYTES + 1) as u64)
                            .read_to_end(&mut bytes)
                            .await
                            .map_err(artwork_error)?;
                        if bytes.len() > super::http::MAX_IMAGE_DOWNLOAD_BYTES {
                            return Err(artwork_error(anyhow::anyhow!(
                                "artwork download exceeds limit"
                            )));
                        }
                        Arc::from(bytes)
                    };
                    Ok(DecodedArtwork {
                        image: decode_artwork(&source, target)?,
                        source: Some(source),
                    })
                } else {
                    Ok(DecodedArtwork {
                        image: fallback.await?,
                        source: None,
                    })
                }
            })
            .shared();
        self.misses += 1;
        self.start(resource, target, task, window, cx);
        self.cached(resource, window, cx)
    }
    fn start(
        &mut self,
        resource: &Resource,
        target: u32,
        task: ImageLoadingTask,
        window: &mut Window,
        cx: &mut App,
    ) {
        let ready = Arc::new(AtomicBool::new(false));
        let retry_ready = ready.clone();
        let image = task.clone();
        let entity = window.current_view();
        let owner = self.owner.clone();
        let resource_key = resource.clone();
        let generation = ready.clone();
        let notification = window.spawn(cx, async move |cx| {
            let failed = image.await.is_err();
            let mut waiters = vec![entity];
            // Account and evict completed images even when the window is idle.
            // A weak owner avoids retaining the cache after the window closes.
            let _ = cx.update(|window, cx| {
                if let Some(owner) = &owner {
                    let _ = owner.update(cx, |cache, cx| {
                        if let Some(entry) = cache
                            .items
                            .get_mut(&resource_key)
                            .filter(|entry| Arc::ptr_eq(&entry.retry_ready, &generation))
                        {
                            waiters = std::mem::take(&mut entry.waiters);
                            cache.cached(&resource_key, window, cx);
                        }
                    });
                }
            });
            let notified = waiters.clone();
            cx.on_next_frame(move |_, cx| {
                for view in notified {
                    cx.notify(view);
                }
            });
            if failed {
                cx.background_executor().timer(FAILURE_RETRY_DELAY).await;
                retry_ready.store(true, Ordering::Release);
                cx.on_next_frame(move |_, cx| {
                    for view in waiters {
                        cx.notify(view);
                    }
                });
            }
        });
        let entry = self
            .items
            .entry(resource.clone())
            .or_insert_with(|| CacheEntry {
                target,
                waiters: Vec::new(),
                visible_demands: Vec::new(),
                image: None,
                source: None,
                bytes: 0,
                request: None,
                error: None,
                retry_ready: ready.clone(),
                _notification: None,
            });
        // Keep the old thumbnail visible during an upgrade. Replacing the task
        // cancels stale work; successful completion explicitly drops the old GPU image.
        entry.target = target;
        entry.waiters = vec![entity];
        entry.visible_demands.clear();
        entry.request = Some(task);
        entry.error = None;
        entry.retry_ready = ready;
        entry._notification = Some(notification);
        self.touch(resource);
    }
    fn touch(&mut self, resource: &Resource) {
        self.recency.retain(|cached| cached != resource);
        self.recency.push_back(resource.clone());
    }
    fn evict_to_budget(&mut self, reserved: usize, window: &mut Window, cx: &mut App) {
        while exceeds_cache_budget(self.items.len(), reserved, self.decoded_bytes) {
            let Some(key) = self.recency.pop_front() else {
                break;
            };
            if let Some(entry) = self.items.remove(&key) {
                self.decoded_bytes = self.decoded_bytes.saturating_sub(entry.bytes);
                if let Some(image) = entry.image {
                    cx.drop_image(image, Some(window));
                }
                self.evictions += 1;
            }
        }
    }
}
fn exceeds_cache_budget(count: usize, reserved: usize, bytes: usize) -> bool {
    count + reserved > MAX_IMAGES || (bytes > MAX_DECODED_BYTES && count > 1)
}
impl ImageCache for BoundedImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.load_sized(resource, 512, window, cx)
    }
}
fn artwork_error(error: impl Into<anyhow::Error>) -> ImageCacheError {
    ImageCacheError::Other(Arc::new(error.into()))
}
fn decode_artwork(bytes: &[u8], side: u32) -> Result<Arc<RenderImage>, ImageCacheError> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(artwork_error)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(artwork_error)?;
    let short = image.width().min(image.height());
    let image = if short > side {
        image.resize(
            ((u64::from(image.width()) * u64::from(side)) / u64::from(short)) as u32,
            ((u64::from(image.height()) * u64::from(side)) / u64::from(short)) as u32,
            image::imageops::FilterType::Triangle,
        )
    } else {
        image
    };
    let mut pixels = image.into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new([image::Frame::new(pixels)])))
}

/// Work runs on the background executor, never in layout or paint. At most
/// 512 x 512 pixels are retained, and only the first frame is used.
fn blur_artist_image(source: &RenderImage) -> Result<Arc<RenderImage>, ImageCacheError> {
    let invalid = || ImageCacheError::Other(Arc::new(anyhow::anyhow!("invalid artist image")));
    let size = source.size(0);
    let pixels = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(
        u32::try_from(size.width.0).map_err(|_| invalid())?,
        u32::try_from(size.height.0).map_err(|_| invalid())?,
        source.as_bytes(0).ok_or_else(invalid)?,
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

    struct ArtworkProbe {
        cache: Entity<BoundedImageCache>,
        requests: Vec<(Resource, u32)>,
        results: Vec<Option<Result<Arc<RenderImage>, ImageCacheError>>>,
        visible: bool,
    }
    impl gpui_kit::Render for ArtworkProbe {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            self.results = self
                .requests
                .iter()
                .map(|(key, side)| {
                    self.cache.update(cx, |cache, cx| {
                        if self.visible {
                            cache.load_visible_sized(key, *side, window, cx)
                        } else {
                            cache.load_sized(key, *side, window, cx)
                        }
                    })
                })
                .collect();
            gpui_kit::div()
        }
    }
    struct ArtworkFixture {
        window: gpui_kit::WindowHandle<ArtworkProbe>,
        probe: Entity<ArtworkProbe>,
        cache: Entity<BoundedImageCache>,
        // Entity handles must drop before the context audits outstanding handles.
        cx: gpui_kit::HeadlessAppContext,
    }
    impl ArtworkFixture {
        fn new(client: Arc<dyn gpui_kit::http_client::HttpClient>) -> Self {
            let mut cx = gpui_kit::HeadlessAppContext::with_asset_source(
                Arc::new(gpui_kit::NoopTextSystem),
                Arc::new(crate::app::assets::CadenceAssets),
            );
            let cache = cx.update(|cx| {
                cx.set_http_client(client);
                BoundedImageCache::new(cx)
            });
            let mut probe = None;
            let window = cx
                .open_window(
                    gpui_kit::size(gpui_kit::px(100.), gpui_kit::px(100.)),
                    |_, cx| {
                        let entity = cx.new(|_| ArtworkProbe {
                            cache: cache.clone(),
                            requests: Vec::new(),
                            results: Vec::new(),
                            visible: false,
                        });
                        probe = Some(entity.clone());
                        entity
                    },
                )
                .unwrap();
            Self {
                cx,
                window,
                probe: probe.unwrap(),
                cache,
            }
        }
        fn load(&mut self, requests: Vec<(Resource, u32)>) {
            self.cx.update(|cx| {
                self.probe.update(cx, |probe, cx| {
                    probe.requests = requests;
                    cx.notify();
                })
            });
            crate::app::test_support::settle(&mut self.cx, self.window.into());
        }
        fn load_visible(&mut self, requests: Vec<(Resource, u32)>) {
            self.cx
                .update(|cx| self.probe.update(cx, |probe, _| probe.visible = true));
            self.load(requests);
        }
        fn image(&mut self) -> Arc<RenderImage> {
            self.cx
                .update(|cx| self.probe.read(cx).results[0].clone().unwrap().unwrap())
        }
        fn clear(&mut self) {
            self.cx
                .update(|cx| self.probe.update(cx, |probe, _| probe.requests.clear()));
            self.cx
                .update_window(self.window.into(), |_, window, cx| {
                    self.cache.update(cx, |cache, cx| cache.clear(window, cx))
                })
                .unwrap();
            crate::app::test_support::settle(&mut self.cx, self.window.into());
        }
    }

    #[test]
    fn artwork_upgrades_reuse_downloads_and_clear_memory() {
        use std::sync::atomic::AtomicUsize;
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            640,
            640,
            image::Rgb([20, 80, 160]),
        ))
        .write_to(&mut bytes, image::ImageFormat::Jpeg)
        .unwrap();
        let bytes = bytes.into_inner();
        let encoded_size = bytes.len();
        let client = gpui_kit::http_client::FakeHttpClient::create(move |_| {
            count.fetch_add(1, Ordering::Relaxed);
            let bytes = bytes.clone();
            async move {
                Ok(gpui_kit::http_client::Response::builder()
                    .status(200)
                    .body(bytes.into())
                    .unwrap())
            }
        });
        let mut fixture = ArtworkFixture::new(client);
        let resource = Resource::Uri("https://example.test/art.jpg".into());
        fixture.load(vec![(resource.clone(), 32)]);
        assert_eq!(fixture.image().size(0).width.0, 64);
        fixture.load(vec![(resource.clone(), 300)]);
        assert_eq!(fixture.image().size(0).width.0, 512);
        fixture.load(vec![(resource, 32)]);
        assert_eq!(
            fixture.image().size(0).width.0,
            512,
            "small views must not downgrade the cache"
        );
        assert_eq!(
            requests.load(Ordering::Relaxed),
            1,
            "size upgrades reuse compressed data"
        );
        fixture.cx.update(|cx| {
            assert_eq!(
                fixture.cache.read(cx).usage().0,
                512 * 512 * 4 + encoded_size
            )
        });
        fixture.clear();
        fixture
            .cx
            .update(|cx| assert_eq!(fixture.cache.read(cx).usage().0, 0));
    }

    #[test]
    fn artwork_workers_are_bounded_and_clear_cancels_requests() {
        use std::sync::atomic::AtomicUsize;
        struct Active(Arc<AtomicUsize>);
        impl Drop for Active {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Relaxed);
            }
        }
        let active = Arc::new(AtomicUsize::new(0));
        let count = active.clone();
        let client = gpui_kit::http_client::FakeHttpClient::create(move |_| {
            let active = count.clone();
            async move {
                active.fetch_add(1, Ordering::Relaxed);
                let _active = Active(active);
                futures::future::pending::<
                    anyhow::Result<
                        gpui_kit::http_client::Response<gpui_kit::http_client::AsyncBody>,
                    >,
                >()
                .await
            }
        });
        let mut fixture = ArtworkFixture::new(client);
        fixture.load(
            (0..48)
                .map(|i| {
                    (
                        Resource::Uri(format!("https://example.test/{i}.jpg").into()),
                        64,
                    )
                })
                .collect(),
        );
        assert_eq!(active.load(Ordering::Relaxed), 2);
        fixture.clear();
        assert_eq!(
            active.load(Ordering::Relaxed),
            0,
            "clear cancels active requests and releases permits"
        );
    }

    fn resource(name: &'static str) -> Resource {
        Resource::Embedded(name.into())
    }

    #[test]
    fn dense_visible_thumbnails_do_not_thrash_the_entry_ceiling_or_refetch() {
        use std::sync::atomic::AtomicUsize;
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            64,
            64,
            image::Rgb([20, 80, 160]),
        ))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
        let bytes = bytes.into_inner();
        let client = gpui_kit::http_client::FakeHttpClient::create(move |_| {
            count.fetch_add(1, Ordering::Relaxed);
            let bytes = bytes.clone();
            async move {
                Ok(gpui_kit::http_client::Response::builder()
                    .status(200)
                    .body(bytes.into())
                    .unwrap())
            }
        });
        let mut fixture = ArtworkFixture::new(client);
        let urls: Vec<_> = (0..80)
            .map(|i| {
                (
                    Resource::Uri(format!("https://example.test/{i}.png").into()),
                    64,
                )
            })
            .collect();
        fixture.load_visible(urls.clone());
        fixture.load_visible(urls);
        assert_eq!(
            requests.load(Ordering::Relaxed),
            80,
            "a visible cover was evicted and fetched again"
        );
        fixture.cx.update(|cx| {
            let cache = fixture.cache.read(cx);
            assert_eq!(cache.items.len(), 80);
            assert_eq!(cache.evictions, 0);
            assert!(cache.decoded_bytes <= MAX_DECODED_BYTES);
            assert!(cache.visibility_batches.is_empty());
        });
    }

    #[test]
    fn leaving_the_viewport_cancels_old_queued_artwork_before_new_work() {
        use std::sync::Mutex;
        struct Active {
            url: String,
            active: Arc<Mutex<std::collections::HashSet<String>>>,
        }
        impl Drop for Active {
            fn drop(&mut self) {
                self.active.lock().unwrap().remove(&self.url);
            }
        }
        let active = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let observed = active.clone();
        let client = gpui_kit::http_client::FakeHttpClient::create(move |request| {
            let url = request.uri().to_string();
            let active = observed.clone();
            async move {
                active.lock().unwrap().insert(url.clone());
                let _guard = Active { url, active };
                futures::future::pending::<
                    anyhow::Result<
                        gpui_kit::http_client::Response<gpui_kit::http_client::AsyncBody>,
                    >,
                >()
                .await
            }
        });
        let mut fixture = ArtworkFixture::new(client);
        fixture.load_visible(
            (0..12)
                .map(|i| {
                    (
                        Resource::Uri(format!("https://example.test/old-{i}.png").into()),
                        64,
                    )
                })
                .collect(),
        );
        assert_eq!(active.lock().unwrap().len(), IMAGE_WORKERS);
        fixture.load_visible(
            (0..4)
                .map(|i| {
                    (
                        Resource::Uri(format!("https://example.test/new-{i}.png").into()),
                        64,
                    )
                })
                .collect(),
        );
        {
            let active = active.lock().unwrap();
            assert_eq!(active.len(), IMAGE_WORKERS);
            assert!(
                active.iter().all(|url| url.contains("new-")),
                "stale covers kept workers occupied: {active:?}"
            );
        }
        fixture.cx.update(|cx| {
            let cache = fixture.cache.read(cx);
            assert_eq!(cache.items.len(), 4);
            assert!(cache.visibility_batches.is_empty());
        });
        fixture.clear();
        assert!(active.lock().unwrap().is_empty());
    }

    #[test]
    fn removing_one_views_demand_preserves_another_views_shared_request() {
        let client = gpui_kit::http_client::FakeHttpClient::create(move |_| async {
            futures::future::pending::<
                anyhow::Result<gpui_kit::http_client::Response<gpui_kit::http_client::AsyncBody>>,
            >()
            .await
        });
        let mut fixture = ArtworkFixture::new(client);
        let shared = Resource::Uri("https://example.test/shared.png".into());
        fixture.load_visible(vec![(shared.clone(), 64)]);
        let cache = fixture.cache.clone();
        let mut other = None;
        let other_window = fixture
            .cx
            .open_window(
                gpui_kit::size(gpui_kit::px(100.), gpui_kit::px(100.)),
                |_, cx| {
                    let view = cx.new(|_| ArtworkProbe {
                        cache,
                        requests: vec![(shared.clone(), 64)],
                        results: Vec::new(),
                        visible: true,
                    });
                    other = Some(view.clone());
                    view
                },
            )
            .unwrap();
        crate::app::test_support::settle(&mut fixture.cx, other_window.into());
        fixture.load_visible(vec![(
            Resource::Uri("https://example.test/new.png".into()),
            64,
        )]);
        fixture.cx.update(|cx| {
            let cache = fixture.cache.read(cx);
            let entry = cache.items.get(&shared).expect("shared image was canceled");
            assert!(entry.request.is_some());
            assert_eq!(entry.waiters, [other.as_ref().unwrap().entity_id()]);
        });
        fixture.clear();
        drop(other);
    }

    struct ClippedArtwork {
        cache: Entity<BoundedImageCache>,
        top: f32,
    }
    impl gpui_kit::Render for ClippedArtwork {
        fn render(
            &mut self,
            _: &mut Window,
            _: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            use gpui_kit::{ParentElement as _, Styled as _};
            gpui_kit::div()
                .size_full()
                .relative()
                .overflow_hidden()
                .child(
                    gpui_kit::div()
                        .absolute()
                        .top(gpui_kit::px(self.top))
                        .child(super::super::components::artwork(
                            super::super::CadencePalette::DARK,
                            &self.cache,
                            Some("https://example.test/clipped.png"),
                            super::super::tokens::ArtworkSize {
                                size: gpui_kit::Rems(3.),
                                radius: gpui_kit::Rems(0.5),
                            },
                            super::super::CadenceIcon::MusicNote,
                        )),
                )
        }
    }

    #[test]
    fn offscreen_artwork_does_not_start_network_work_during_measurement() {
        use std::sync::atomic::AtomicUsize;
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let client = gpui_kit::http_client::FakeHttpClient::create(move |_| {
            count.fetch_add(1, Ordering::Relaxed);
            async {
                futures::future::pending::<
                    anyhow::Result<
                        gpui_kit::http_client::Response<gpui_kit::http_client::AsyncBody>,
                    >,
                >()
                .await
            }
        });
        let mut fixture = ArtworkFixture::new(client);
        let cache = fixture.cache.clone();
        let mut view = None;
        let window = fixture
            .cx
            .open_window(
                gpui_kit::size(gpui_kit::px(100.), gpui_kit::px(100.)),
                |_, cx| {
                    let entity = cx.new(|_| ClippedArtwork { cache, top: 1000. });
                    view = Some(entity.clone());
                    entity
                },
            )
            .unwrap();
        crate::app::test_support::settle(&mut fixture.cx, window.into());
        assert_eq!(requests.load(Ordering::Relaxed), 0);
        fixture.cx.update(|cx| {
            view.as_ref().unwrap().update(cx, |view, cx| {
                view.top = 0.;
                cx.notify();
            })
        });
        crate::app::test_support::settle(&mut fixture.cx, window.into());
        assert_eq!(requests.load(Ordering::Relaxed), 1);
        // Drop entity handles before the headless context audits them.
        drop(view);
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
    fn decoded_thumbnails_keep_cover_aspect_ratio_and_bgra_channels() {
        for (width, height, expected) in [
            (1200, 800, (96, 64)),
            (800, 1200, (64, 96)),
            (32, 32, (32, 32)),
        ] {
            let original = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                width,
                height,
                image::Rgba([20, 80, 160, 255]),
            ));
            let mut encoded = std::io::Cursor::new(Vec::new());
            original
                .write_to(&mut encoded, image::ImageFormat::Png)
                .unwrap();
            let decoded = decode_artwork(encoded.get_ref(), 64).unwrap();
            assert_eq!(
                (decoded.size(0).width.0, decoded.size(0).height.0),
                expected
            );
            assert_eq!(&decoded.as_bytes(0).unwrap()[..4], &[160, 80, 20, 255]);
            assert_eq!(decoded.frame_count(), 1);
        }
        assert!(decode_artwork(b"invalid artwork", 64).is_err());
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
            workers: Arc::new(tokio::sync::Semaphore::new(IMAGE_WORKERS)),
            owner: None,
            visibility_batches: HashMap::new(),
            visibility_generation: 0,
        };

        cache.touch(&first);

        assert_eq!(cache.recency, VecDeque::from([second, first]));
    }
}
