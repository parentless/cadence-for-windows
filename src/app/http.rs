use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use anyhow::{Result, bail, ensure};
use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use gpui_kit::http_client::{AsyncBody, HttpClient, Inner, Response, Url, http};

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
pub(super) const MAX_IMAGE_DOWNLOAD_BYTES: usize = 10 * 1024 * 1024;

// Dropping a cache request must cancel its Tokio work rather than detach it.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) struct ImageHttpClient {
    client: reqwest::Client,
    user_agent: http::HeaderValue,
    runtime: tokio::runtime::Handle,
    disk: Option<Arc<super::disk_cache::DiskCache>>,
}

/// Where downloaded artwork is kept between launches, shared with Settings so
/// "Clear cache" can empty it.
static DISK_CACHE: OnceLock<Arc<super::disk_cache::DiskCache>> = OnceLock::new();

/// Deletes the on-disk artwork off the UI thread.
pub(super) fn clear_disk_cache() {
    if let Some(cache) = DISK_CACHE.get().cloned() {
        std::thread::spawn(move || cache.clear());
    }
}

impl ImageHttpClient {
    pub(super) fn new() -> Result<Self> {
        let user_agent = http::HeaderValue::from_static("Cadence/0.1");
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .user_agent(user_agent.clone())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()?;
        let runtime = tokio::runtime::Handle::try_current().unwrap_or_else(|_| {
            RUNTIME
                .get_or_init(|| {
                    tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(1)
                        .enable_all()
                        .build()
                        .expect("could not start image HTTP runtime")
                })
                .handle()
                .clone()
        });
        let disk = super::disk_cache::DiskCache::default_location()
            .map(|cache| DISK_CACHE.get_or_init(|| Arc::new(cache)).clone());
        Ok(Self {
            client,
            user_agent,
            runtime,
            disk,
        })
    }
}

impl HttpClient for ImageHttpClient {
    fn user_agent(&self) -> Option<&http::HeaderValue> {
        Some(&self.user_agent)
    }

    fn send(
        &self,
        request: http::Request<AsyncBody>,
    ) -> BoxFuture<'static, Result<Response<AsyncBody>>> {
        let (parts, body) = request.into_parts();
        // Only plain GETs of images are cacheable.
        let cache_key = (parts.method == http::Method::GET && matches!(body.0, Inner::Empty))
            .then(|| parts.uri.to_string())
            .zip(self.disk.clone());
        let mut request = self.client.request(parts.method, parts.uri.to_string());
        request = request.headers(parts.headers);
        request = match body.0 {
            Inner::Empty => request,
            Inner::Bytes(bytes) => request.body(bytes.into_inner()),
            Inner::AsyncReader(_) => {
                return async { bail!("streaming request bodies are unsupported") }.boxed();
            }
        };
        let runtime = self.runtime.clone();
        async move {
            if let Some((url, disk)) = cache_key.clone() {
                // The blocking pool reads the file; the UI thread never touches disk.
                let mut lookup = AbortOnDrop(runtime.spawn_blocking(move || disk.load(&url)));
                if let Ok(Some((bytes, mime))) = (&mut lookup.0).await {
                    return Ok(http::Response::builder()
                        .status(200)
                        .header("content-type", mime)
                        .body(bytes.into())?);
                }
            }
            let mut pending =
                AbortOnDrop(runtime.spawn(async move { request.send().await?.error_for_status() }));
            let response = (&mut pending.0).await??;
            let status = response.status();
            let headers = response.headers().clone();
            if let Some(content_length) = response.content_length() {
                ensure!(
                    content_length <= MAX_IMAGE_DOWNLOAD_BYTES as u64,
                    "image response exceeds {MAX_IMAGE_DOWNLOAD_BYTES} byte limit"
                );
            }
            let mut body = Vec::with_capacity(
                response
                    .content_length()
                    .and_then(|length| usize::try_from(length).ok())
                    .unwrap_or_default()
                    .min(MAX_IMAGE_DOWNLOAD_BYTES),
            );
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                ensure!(
                    body.len().saturating_add(chunk.len()) <= MAX_IMAGE_DOWNLOAD_BYTES,
                    "image response exceeds {MAX_IMAGE_DOWNLOAD_BYTES} byte limit"
                );
                body.extend_from_slice(&chunk);
            }
            if let Some((url, disk)) = cache_key
                && status == http::StatusCode::OK
                && let Some(content_type) = headers
                    .get("content-type")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
            {
                let copy = body.clone();
                // Fire and forget: a lost write only means one more download.
                runtime.spawn_blocking(move || disk.store(&url, &content_type, &copy));
            }
            let mut response = http::Response::builder().status(status);
            *response
                .headers_mut()
                .expect("response builder has headers") = headers;
            Ok(response.body(body.into())?)
        }
        .boxed()
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }
}
