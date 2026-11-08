//! Lightweight Discord IPC; no SDK, network polling, or audio-thread work.
use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    io, thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, watch},
    time::{Instant, timeout},
};

const MAX_FRAME_BYTES: usize = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(1);
const UPDATE_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Config {
    pub enabled: bool,
    pub application_id: String,
}

pub(crate) fn valid_application_id(id: &str) -> bool {
    (17..=20).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_digit())
        && id.parse::<u64>().is_ok_and(|number| number > 0)
}

/// At most one pending activity. Repeated updates replace stale data rather
/// than growing a queue while Discord is unavailable or slow.
pub(crate) struct Presence {
    sender: watch::Sender<Option<Value>>,
    last: Option<Published>,
}

struct Published {
    track: crate::model::Track,
    position_ms: u32,
    at: std::time::Instant,
}

impl Presence {
    pub(crate) fn start(id: String) -> Result<Self> {
        ensure!(
            valid_application_id(&id),
            "enter a valid Discord Application ID"
        );
        let (sender, receiver) = watch::channel(None);
        thread::Builder::new()
            .name("cadence-discord".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(worker(id, receiver)),
                    Err(error) => log::warn!("could not start Discord IPC runtime: {error}"),
                }
            })
            .context("could not start Discord presence worker")?;
        Ok(Self { sender, last: None })
    }

    pub(crate) fn sync(
        &mut self,
        track: Option<&crate::model::Track>,
        playing: bool,
        position_ms: u32,
    ) {
        let Some(track) = track.filter(|_| playing) else {
            if self.last.take().is_some() {
                self.sender.send_replace(None);
            }
            return;
        };
        let now = std::time::Instant::now();
        if self.last.as_ref().is_some_and(|previous| {
            let elapsed =
                u32::try_from(now.duration_since(previous.at).as_millis()).unwrap_or(u32::MAX);
            previous.track == *track
                && position_ms.abs_diff(previous.position_ms.saturating_add(elapsed)) < 2_000
        }) {
            return;
        }
        let wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let wall_ms = u64::try_from(wall_ms).unwrap_or(u64::MAX);
        self.sender
            .send_replace(Some(activity(track, position_ms, wall_ms)));
        // Only one track is retained; no history, artwork download, or image cache.
        self.last = Some(Published {
            track: track.clone(),
            position_ms,
            at: now,
        });
    }
}

// Dropping the only watch sender wakes the worker and closes IPC. We do not
// join on the UI thread: a slow Discord client must never block playback/UI.

fn text(value: &str, fallback: &str) -> String {
    let value = if value.trim().is_empty() {
        fallback
    } else {
        value
    };
    let mut end = value.len().min(128);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut result = value[..end].replace('\0', "");
    if result.chars().count() < 2 {
        result.push(' ');
    }
    result
}

fn activity(track: &crate::model::Track, position_ms: u32, now_ms: u64) -> Value {
    // RPC timestamps are Unix seconds (unlike Gateway activity timestamps).
    let position_ms = position_ms.min(track.duration_ms);
    let start = now_ms.saturating_sub(u64::from(position_ms)) / 1000;
    let mut activity = json!({
        "type": 2,
        "details": text(&track.title, "Unknown track"),
        "state": text(&track.artist, "Unknown artist"),
        "timestamps": {"start": start},
        "instance": false,
    });
    if track.duration_ms > 0 {
        activity["timestamps"]["end"] = json!(start + u64::from(track.duration_ms) / 1000);
    }
    if let Some(cover) = track.artwork_url.as_deref().filter(|url| {
        url.len() <= 1024
            && url::Url::parse(url)
                .is_ok_and(|url| url.scheme() == "https" && url.host_str().is_some())
    }) {
        activity["assets"] =
            json!({"large_image": cover, "large_text": text(&track.album, "Album")});
    }
    activity
}

trait IpcStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> IpcStream for T {}
type Stream = Box<dyn IpcStream>;

async fn connect() -> io::Result<Stream> {
    #[cfg(windows)]
    for index in 0..10 {
        if let Ok(pipe) = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(format!(r"\\?\pipe\discord-ipc-{index}"))
        {
            return Ok(Box::new(pipe));
        }
    }
    #[cfg(unix)]
    {
        let mut directories: Vec<std::path::PathBuf> = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
            .iter()
            .filter_map(std::env::var_os)
            .map(Into::into)
            .collect();
        directories.push("/tmp".into());
        directories.dedup();
        for directory in directories {
            for index in 0..10 {
                if let Ok(Ok(socket)) = timeout(
                    Duration::from_millis(100),
                    tokio::net::UnixStream::connect(directory.join(format!("discord-ipc-{index}"))),
                )
                .await
                {
                    return Ok(Box::new(socket));
                }
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "Discord desktop IPC is unavailable",
    ))
}

async fn worker(id: String, mut desired: watch::Receiver<Option<Value>>) {
    let mut retry = Duration::from_secs(2);
    loop {
        if desired.borrow().is_none() {
            if desired.changed().await.is_err() {
                return;
            }
            continue;
        }
        let connection = tokio::select! {
            connection = connect() => connection,
            changed = desired.changed() => { if changed.is_err() { return; } continue; }
        };
        if let Ok(stream) = connection {
            // Session consumes changes itself and returns when Discord disconnects.
            if let Err(error) = session(stream, &id, &mut desired).await {
                log::debug!("Discord presence disconnected: {error}");
            }
            if desired.has_changed().is_err() {
                return;
            }
        }
        // No busy polling; a track change or shutdown interrupts this wait.
        tokio::select! {
            _ = tokio::time::sleep(retry) => {},
            changed = desired.changed() => { if changed.is_err() { return; } }
        }
        retry = (retry * 2).min(Duration::from_secs(30));
    }
}

struct ReaderTask(tokio::task::JoinHandle<()>);
impl Drop for ReaderTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn session(
    stream: Stream,
    id: &str,
    desired: &mut watch::Receiver<Option<Value>>,
) -> Result<()> {
    let (mut read, mut write) = tokio::io::split(stream);
    let (incoming, mut frames) = mpsc::channel(4);
    // A dedicated reader preserves partial frames when a watch update wins
    // select!: cancelling read_exact midway would corrupt the IPC stream.
    let _reader = ReaderTask(tokio::spawn(async move {
        loop {
            let frame = read_frame(&mut read).await;
            let failed = frame.is_err();
            if incoming.send(frame).await.is_err() || failed {
                break;
            }
        }
    }));
    send_frame(
        &mut write,
        0,
        &serde_json::to_vec(&json!({"v": 1, "client_id": id}))?,
    )
    .await?;
    let mut ready = false;
    let mut pending = false;
    let mut next_send = Instant::now();
    let handshake_deadline = Instant::now() + Duration::from_secs(3);
    let mut nonce = 0u64;
    loop {
        tokio::select! {
            changed = desired.changed() => {
                if changed.is_err() {
                    if ready { let _ = send_activity(&mut write, None, &mut nonce).await; }
                    return Ok(());
                }
                pending = true;
                if desired.borrow().is_none() { next_send = Instant::now(); }
            },
            frame = frames.recv() => {
                let (opcode, payload) = frame.context("Discord closed IPC")??;
                match opcode {
                    1 => {
                        let value: Value = serde_json::from_slice(&payload)?;
                        if value["evt"] == "ERROR" { bail!("Discord rejected presence (code {})", value["data"]["code"]); }
                        if value["evt"] == "READY" { ready = true; pending = true; }
                    },
                    2 => bail!("Discord closed IPC"),
                    3 => send_frame(&mut write, 4, &payload).await?,
                    4 => {},
                    _ => bail!("invalid Discord opcode"),
                }
            },
            _ = tokio::time::sleep_until(next_send), if ready && pending => {
                let value = desired.borrow_and_update().clone();
                send_activity(&mut write, value, &mut nonce).await?;
                pending = false;
                next_send = Instant::now() + UPDATE_INTERVAL;
            },
            _ = tokio::time::sleep_until(handshake_deadline), if !ready => bail!("Discord handshake timed out"),
        }
    }
}

async fn send_activity<W: AsyncWrite + Unpin>(
    write: &mut W,
    activity: Option<Value>,
    nonce: &mut u64,
) -> Result<()> {
    *nonce = nonce.wrapping_add(1);
    let payload = json!({"cmd": "SET_ACTIVITY", "args": {"pid": std::process::id(), "activity": activity}, "nonce": nonce.to_string()});
    send_frame(write, 1, &serde_json::to_vec(&payload)?).await
}

async fn send_frame<W: AsyncWrite + Unpin>(
    write: &mut W,
    opcode: u32,
    payload: &[u8],
) -> Result<()> {
    ensure!(
        payload.len() <= MAX_FRAME_BYTES,
        "Discord frame exceeds size limit"
    );
    // Windows Discord pipes preserve message boundaries: write the complete
    // header and payload together rather than three separate messages.
    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend_from_slice(&opcode.to_le_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    timeout(IO_TIMEOUT, async {
        write.write_all(&frame).await?;
        write.flush().await
    })
    .await
    .context("Discord IPC write timed out")??;
    Ok(())
}

async fn read_frame<R: AsyncRead + Unpin>(read: &mut R) -> Result<(u32, Vec<u8>)> {
    let mut header = [0; 8];
    read.read_exact(&mut header).await?;
    let opcode = u32::from_le_bytes(header[..4].try_into()?);
    let size = u32::from_le_bytes(header[4..].try_into()?) as usize;
    ensure!(size <= MAX_FRAME_BYTES, "Discord frame exceeds size limit");
    let mut payload = vec![0; size];
    read.read_exact(&mut payload).await?;
    Ok((opcode, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> crate::model::Track {
        crate::model::Track {
            provider: crate::model::Provider::Spotify,
            source_id: "0123456789012345678901".into(),
            spotify_uri: Some("spotify:track:0123456789012345678901".into()),
            isrc: None,
            title: "A song".into(),
            artist: "An artist".into(),
            artists: vec![],
            album: "An album".into(),
            album_ref: None,
            duration_ms: 180_000,
            artwork_url: Some("https://i.scdn.co/image/test".into()),
        }
    }

    #[test]
    fn activity_contains_listening_metadata_and_progress_without_credentials() {
        let activity = activity(&track(), 30_000, 1_800_000_000_000);
        assert_eq!(activity["type"], 2);
        assert_eq!(activity["details"], "A song");
        assert_eq!(activity["state"], "An artist");
        assert_eq!(activity["timestamps"]["start"], 1_799_999_970u64);
        assert_eq!(activity["timestamps"]["end"], 1_800_000_150u64);
        assert_eq!(
            activity["assets"]["large_image"],
            "https://i.scdn.co/image/test"
        );
        assert!(activity.get("buttons").is_none());
    }

    #[test]
    fn unicode_and_identifiers_are_bounded_and_validated() {
        let text = text(&"🎵".repeat(200), "Track");
        assert!(text.len() <= 128);
        assert!(!text.contains('\0'));
        assert!(valid_application_id("123456789012345678"));
        assert!(!valid_application_id("000000000000000000"));
        assert!(!valid_application_id("99999999999999999999"));
        assert!(!valid_application_id("a-discord-bot-token"));
        let mut track = track();
        track.artwork_url = Some("file:///private/file.png".into());
        assert!(activity(&track, 0, 1000).get("assets").is_none());
    }

    #[test]
    fn playback_ticks_are_deduplicated_seeks_update_and_pause_clears() {
        let (sender, mut receiver) = watch::channel(None);
        let mut presence = Presence { sender, last: None };
        let track = track();
        presence.sync(Some(&track), true, 10_000);
        assert!(receiver.borrow_and_update().is_some());
        presence.sync(Some(&track), true, 10_250);
        assert!(!receiver.has_changed().unwrap());
        presence.sync(Some(&track), true, 60_000);
        assert!(receiver.has_changed().unwrap());
        receiver.borrow_and_update();
        presence.sync(Some(&track), false, 60_000);
        assert!(receiver.borrow_and_update().is_none());
        presence.sync(None, false, 0);
        assert!(!receiver.has_changed().unwrap());
    }

    async fn read_json(stream: &mut tokio::io::DuplexStream) -> (u32, Value) {
        let (opcode, payload) = timeout(Duration::from_secs(2), read_frame(stream))
            .await
            .unwrap()
            .unwrap();
        (opcode, serde_json::from_slice(&payload).unwrap())
    }

    #[tokio::test]
    async fn ipc_handles_handshake_partial_frames_ping_pause_and_shutdown() {
        let (client, mut server) = tokio::io::duplex(4096);
        let (sender, mut desired) = watch::channel(Some(activity(&track(), 0, 1_800_000_000_000)));
        let task = tokio::spawn(async move {
            session(Box::new(client), "123456789012345678", &mut desired).await
        });
        let (opcode, handshake) = read_json(&mut server).await;
        assert_eq!(opcode, 0);
        assert_eq!(handshake["client_id"], "123456789012345678");
        send_frame(&mut server, 1, br#"{"evt":"READY"}"#)
            .await
            .unwrap();
        let (_, payload) = read_json(&mut server).await;
        assert_eq!(payload["cmd"], "SET_ACTIVITY");
        assert_eq!(payload["args"]["activity"]["type"], 2);
        // Interrupt an incomplete frame with a new state. Its remaining bytes
        // must still be read correctly rather than lost by select! cancellation.
        let mut header = Vec::new();
        header.extend_from_slice(&3u32.to_le_bytes());
        header.extend_from_slice(&4u32.to_le_bytes());
        server.write_all(&header[..3]).await.unwrap();
        sender.send_replace(Some(json!({"type": 2, "details": "Next track"})));
        tokio::task::yield_now().await;
        server.write_all(&header[3..]).await.unwrap();
        server.write_all(b"ping").await.unwrap();
        let (opcode, payload) = timeout(Duration::from_secs(2), read_frame(&mut server))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(opcode, 4);
        assert_eq!(payload, b"ping");
        // Pause must clear immediately, without waiting for the rate limit.
        sender.send_replace(None);
        let (_, cleared) = read_json(&mut server).await;
        assert!(cleared["args"]["activity"].is_null());
        drop(sender);
        let (_, cleared) = read_json(&mut server).await;
        assert!(cleared["args"]["activity"].is_null());
        timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[derive(Default)]
    struct MessageWriter(Vec<Vec<u8>>);

    impl AsyncWrite for MessageWriter {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            self.0.push(bytes.to_vec());
            std::task::Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn complete_ipc_frame_is_submitted_as_one_message() {
        let mut writer = MessageWriter::default();
        let payload = br#"{"v":1,"client_id":"123456789012345678"}"#;
        send_frame(&mut writer, 0, payload).await.unwrap();
        assert_eq!(
            writer.0.len(),
            1,
            "Windows message pipes require a complete RPC frame per write"
        );
        let frame = &writer.0[0];
        assert_eq!(&frame[..4], &0u32.to_le_bytes());
        assert_eq!(&frame[4..8], &(payload.len() as u32).to_le_bytes());
        assert_eq!(&frame[8..], payload);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_message_pipe_receives_the_entire_discord_frame() {
        use tokio::net::windows::named_pipe::{ClientOptions, PipeMode, ServerOptions};
        let name = format!(
            r"\\.\pipe\cadence-discord-frame-test-{}",
            std::process::id()
        );
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .pipe_mode(PipeMode::Message)
            .create(&name)
            .unwrap();
        let mut client = ClientOptions::new().open(&name).unwrap();
        server.connect().await.unwrap();
        let payload = br#"{"v":1,"client_id":"123456789012345678"}"#;
        send_frame(&mut client, 0, payload).await.unwrap();
        let mut frame = [0; 512];
        let count = timeout(IO_TIMEOUT, server.read(&mut frame))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count, 8 + payload.len());
        assert_eq!(&frame[8..count], payload);
    }

    #[tokio::test]
    async fn oversized_incoming_frames_are_rejected_before_allocating_payload() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        writer.write_all(&1u32.to_le_bytes()).await.unwrap();
        writer
            .write_all(&((MAX_FRAME_BYTES + 1) as u32).to_le_bytes())
            .await
            .unwrap();
        assert!(
            read_frame(&mut reader)
                .await
                .unwrap_err()
                .to_string()
                .contains("size limit")
        );
    }
}
