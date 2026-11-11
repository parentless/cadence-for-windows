//! Last.fm scrobbling.
//!
//! Performance rules: no polling and nothing on the UI or audio thread. One
//! idle worker thread exists only while scrobbling is enabled. It holds a
//! single latest-wins state slot, one timer set to the exact moment a scrobble
//! becomes due, and a small bounded retry queue. Sign-in is a rare one-shot
//! thread, so nothing stays resident for it.
use anyhow::{Context as _, Result, anyhow, bail, ensure};
use keyring::Entry;
use md5::{Digest, Md5};
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::Arc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{oneshot, watch},
    time::{Instant, sleep_until},
};

const API_URL: &str = "https://ws.audioscrobbler.com/2.0/";
const AUTH_URL: &str = "https://www.last.fm/api/auth/";
const KEYCHAIN_SERVICE: &str = "dev.twoducks.cadence";
const KEYCHAIN_ACCOUNT: &str = "lastfm-credentials";
/// Last.fm only counts tracks longer than this.
const MIN_TRACK_MS: u32 = 30_000;
/// A track scrobbles after half its length or four minutes, whichever is first.
const MAX_THRESHOLD_MS: u32 = 240_000;
/// Scrobbles held while Last.fm is unreachable. The oldest is dropped first.
const MAX_QUEUE: usize = 50;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const FIRST_RETRY: Duration = Duration::from_secs(30);
const MAX_RETRY: Duration = Duration::from_secs(600);
/// Same threshold Discord uses: smaller drift is just playback continuing.
const POSITION_TOLERANCE_MS: u32 = 2_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Config {
    pub enabled: bool,
    pub api_key: String,
    /// Empty until an account is connected. The secret and session key live in
    /// the system keychain, never in the preferences database.
    pub username: String,
}

impl Config {
    pub(crate) fn connected(&self) -> bool {
        !self.username.is_empty()
    }
}

/// Last.fm API keys and shared secrets are 32 hexadecimal characters.
pub(crate) fn valid_credential(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// What the worker needs to know about the track.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    key: String,
    artist: String,
    title: String,
    album: String,
    duration_ms: u32,
}

impl Snapshot {
    fn new(track: &crate::model::Track) -> Option<Self> {
        let artist = track.artist.trim();
        let title = track.title.trim();
        if artist.is_empty() || title.is_empty() {
            return None;
        }
        Some(Self {
            key: format!("{:?}:{}", track.provider, track.source_id),
            artist: artist.to_owned(),
            title: title.to_owned(),
            album: track.album.trim().to_owned(),
            duration_ms: track.duration_ms,
        })
    }
}

#[derive(Clone, Debug, Default)]
struct State {
    track: Option<Arc<Snapshot>>,
    playing: bool,
    position_ms: u32,
}

#[derive(Debug, PartialEq, Eq)]
struct Scrobble {
    artist: String,
    title: String,
    album: String,
    duration_s: u32,
    timestamp: u64,
}

struct Current {
    snapshot: Arc<Snapshot>,
    started_unix: u64,
    /// Listening time already banked; excludes time spent paused or seeking.
    played: Duration,
    since: Option<Instant>,
    announced: bool,
    scrobbled: bool,
    last_position_ms: u32,
}

/// Decides when to announce and scrobble. Pure and clock-injected so the rules
/// are testable without a network or real time.
#[derive(Default)]
struct Tracker {
    current: Option<Current>,
}

impl Tracker {
    /// Applies a state change. Returns the track to announce as "now playing".
    fn update(&mut self, state: &State, now: Instant, unix_now: u64) -> Option<Arc<Snapshot>> {
        let Some(track) = state.track.as_ref() else {
            self.current = None;
            return None;
        };
        let replay = self.current.as_ref().is_some_and(|current| {
            current.snapshot.key == track.key
                && current.scrobbled
                && state.playing
                && state.position_ms < 3_000
                && current.last_position_ms > 10_000
        });
        if replay
            || self
                .current
                .as_ref()
                .is_none_or(|c| c.snapshot.key != track.key)
        {
            self.current = Some(Current {
                snapshot: track.clone(),
                started_unix: unix_now.saturating_sub(u64::from(state.position_ms) / 1000),
                played: Duration::ZERO,
                since: None,
                announced: false,
                scrobbled: false,
                last_position_ms: state.position_ms,
            });
        }
        let current = self.current.as_mut()?;
        current.last_position_ms = state.position_ms;
        match (state.playing, current.since) {
            (true, None) => current.since = Some(now),
            (false, Some(since)) => {
                current.played += now.saturating_duration_since(since);
                current.since = None;
            }
            _ => {}
        }
        if state.playing && !current.announced {
            current.announced = true;
            return Some(current.snapshot.clone());
        }
        None
    }

    /// When the current play will have been listened to for long enough.
    fn due(&self) -> Option<Instant> {
        let current = self.current.as_ref()?;
        let since = current.since?;
        if current.scrobbled || current.snapshot.duration_ms < MIN_TRACK_MS {
            return None;
        }
        let threshold = Duration::from_millis(u64::from(
            (current.snapshot.duration_ms / 2).min(MAX_THRESHOLD_MS),
        ));
        Some(since + threshold.saturating_sub(current.played))
    }

    fn take_scrobble(&mut self, now: Instant) -> Option<Scrobble> {
        if self.due()? > now {
            return None;
        }
        let current = self.current.as_mut()?;
        current.scrobbled = true;
        let snapshot = &current.snapshot;
        Some(Scrobble {
            artist: snapshot.artist.clone(),
            title: snapshot.title.clone(),
            album: snapshot.album.clone(),
            duration_s: snapshot.duration_ms / 1000,
            timestamp: current.started_unix,
        })
    }
}

struct Published {
    key: String,
    playing: bool,
    position_ms: u32,
    at: std::time::Instant,
}

/// UI-side handle. Dropping it wakes the worker, which then exits.
pub(crate) struct Scrobbler {
    sender: watch::Sender<State>,
    last: Option<Published>,
    snapshot: Option<Arc<Snapshot>>,
}

impl Scrobbler {
    pub(crate) fn start(config: &Config) -> Result<Self> {
        ensure!(config.connected(), "connect a Last.fm account first");
        ensure!(
            valid_credential(&config.api_key),
            "enter a valid Last.fm API key"
        );
        let api_key = config.api_key.clone();
        let (sender, receiver) = watch::channel(State::default());
        thread::Builder::new()
            .name("cadence-lastfm".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(worker(api_key, receiver)),
                    Err(error) => log::warn!("could not start Last.fm runtime: {error}"),
                }
            })
            .context("could not start Last.fm worker")?;
        Ok(Self {
            sender,
            last: None,
            snapshot: None,
        })
    }

    /// Cheap enough to call on every player change: unchanged state is dropped
    /// here, and a new track is converted once.
    pub(crate) fn sync(
        &mut self,
        track: Option<&crate::model::Track>,
        playing: bool,
        position_ms: u32,
    ) {
        let now = std::time::Instant::now();
        let key = track.map(|track| format!("{:?}:{}", track.provider, track.source_id));
        let unchanged = match (&self.last, &key) {
            (Some(previous), Some(key)) => {
                let elapsed =
                    u32::try_from(now.duration_since(previous.at).as_millis()).unwrap_or(u32::MAX);
                let expected = if previous.playing {
                    previous.position_ms.saturating_add(elapsed)
                } else {
                    previous.position_ms
                };
                previous.key == *key
                    && previous.playing == playing
                    && position_ms.abs_diff(expected) < POSITION_TOLERANCE_MS
            }
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        if self.last.as_ref().map(|last| &last.key) != key.as_ref() {
            self.snapshot = track.and_then(Snapshot::new).map(Arc::new);
        }
        self.last = key.map(|key| Published {
            key,
            playing,
            position_ms,
            at: now,
        });
        self.sender.send_replace(State {
            track: self.snapshot.clone(),
            playing: playing && self.snapshot.is_some(),
            position_ms,
        });
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn worker(api_key: String, mut state: watch::Receiver<State>) {
    let Some((api_secret, session_key)) = load_credentials()
        .map_err(|error| log::warn!("could not read Last.fm credentials: {error}"))
        .ok()
        .flatten()
    else {
        log::warn!("Last.fm is enabled but not signed in");
        return;
    };
    let client = match api_client() {
        Ok(client) => client,
        Err(error) => {
            log::warn!("could not start Last.fm client: {error}");
            return;
        }
    };
    let api = Api {
        client,
        key: api_key,
        secret: api_secret,
    };
    let mut tracker = Tracker::default();
    let mut queue: VecDeque<Scrobble> = VecDeque::new();
    let mut retry_at: Option<Instant> = None;
    let mut backoff = FIRST_RETRY;
    loop {
        let wake = [tracker.due(), retry_at.filter(|_| !queue.is_empty())]
            .into_iter()
            .flatten()
            .min();
        tokio::select! {
            changed = state.changed() => {
                if changed.is_err() {
                    return;
                }
                let current = state.borrow_and_update().clone();
                if let Some(snapshot) = tracker.update(&current, Instant::now(), unix_now()) {
                    // Best effort: a missed "now playing" is never retried.
                    if let Err(error) = api.now_playing(&session_key, &snapshot).await {
                        log::debug!("Last.fm now playing failed: {error}");
                    }
                }
            }
            () = async {
                match wake {
                    Some(at) => sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {}
        }
        if let Some(scrobble) = tracker.take_scrobble(Instant::now()) {
            if queue.len() == MAX_QUEUE {
                queue.pop_front();
            }
            queue.push_back(scrobble);
        }
        if queue.is_empty() || retry_at.is_some_and(|at| at > Instant::now()) {
            continue;
        }
        let batch: Vec<&Scrobble> = queue.iter().take(MAX_QUEUE).collect();
        match api.scrobble(&session_key, &batch).await {
            Ok(()) => {
                let sent = batch.len();
                queue.drain(..sent);
                retry_at = None;
                backoff = FIRST_RETRY;
            }
            Err(error) if error.transient() => {
                log::debug!("Last.fm scrobble will retry: {error}");
                retry_at = Some(Instant::now() + backoff);
                backoff = (backoff * 2).min(MAX_RETRY);
            }
            Err(error) => {
                // Retrying a rejected batch can never succeed; drop it.
                log::warn!("Last.fm rejected scrobbles: {error}");
                let rejected = batch.len();
                queue.drain(..rejected);
                if error.invalid_session() {
                    return;
                }
            }
        }
    }
}

#[derive(Debug)]
enum CallError {
    Api { code: u32, message: String },
    Transport(anyhow::Error),
}

impl CallError {
    /// Service trouble and rate limits clear up; anything else will not.
    fn transient(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::Api { code, .. } => matches!(code, 8 | 11 | 16 | 29),
        }
    }

    fn invalid_session(&self) -> bool {
        matches!(self, Self::Api { code: 9, .. })
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api { code, message } => write!(formatter, "Last.fm error {code}: {message}"),
            Self::Transport(error) => write!(formatter, "{error:#}"),
        }
    }
}

fn api_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .use_rustls_tls()
        .user_agent("Sonder")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(Into::into)
}

struct Api {
    client: reqwest::Client,
    key: String,
    secret: String,
}

impl Api {
    async fn now_playing(&self, session_key: &str, snapshot: &Snapshot) -> Result<(), CallError> {
        let mut params = vec![
            ("method".to_owned(), "track.updateNowPlaying".to_owned()),
            ("artist".to_owned(), snapshot.artist.clone()),
            ("track".to_owned(), snapshot.title.clone()),
            ("sk".to_owned(), session_key.to_owned()),
        ];
        if !snapshot.album.is_empty() {
            params.push(("album".to_owned(), snapshot.album.clone()));
        }
        if snapshot.duration_ms >= 1000 {
            params.push((
                "duration".to_owned(),
                (snapshot.duration_ms / 1000).to_string(),
            ));
        }
        self.call(params).await.map(drop)
    }

    async fn scrobble(&self, session_key: &str, batch: &[&Scrobble]) -> Result<(), CallError> {
        let mut params = vec![
            ("method".to_owned(), "track.scrobble".to_owned()),
            ("sk".to_owned(), session_key.to_owned()),
        ];
        for (index, scrobble) in batch.iter().enumerate() {
            params.push((format!("artist[{index}]"), scrobble.artist.clone()));
            params.push((format!("track[{index}]"), scrobble.title.clone()));
            params.push((
                format!("timestamp[{index}]"),
                scrobble.timestamp.to_string(),
            ));
            if !scrobble.album.is_empty() {
                params.push((format!("album[{index}]"), scrobble.album.clone()));
            }
            if scrobble.duration_s > 0 {
                params.push((
                    format!("duration[{index}]"),
                    scrobble.duration_s.to_string(),
                ));
            }
        }
        self.call(params).await.map(drop)
    }

    async fn call(&self, mut params: Vec<(String, String)>) -> Result<Value, CallError> {
        params.push(("api_key".to_owned(), self.key.clone()));
        let signature = signature(&params, &self.secret);
        params.push(("api_sig".to_owned(), signature));
        params.push(("format".to_owned(), "json".to_owned()));
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(
                params
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            )
            .finish();
        let response = self
            .client
            .post(API_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|error| CallError::Transport(error.into()))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| CallError::Transport(error.into()))?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(CallError::Transport(anyhow!(
                "Last.fm response is too large"
            )));
        }
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => match value.get("error").and_then(Value::as_u64) {
                Some(code) => Err(CallError::Api {
                    code: u32::try_from(code).unwrap_or(u32::MAX),
                    message: value
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("request failed")
                        .to_owned(),
                }),
                None => Ok(value),
            },
            Err(_) => Err(CallError::Transport(anyhow!(
                "Last.fm returned HTTP {status} with an unreadable body"
            ))),
        }
    }
}

/// `api_sig`: parameters sorted by name, name and value concatenated, the
/// shared secret appended, then MD5. `format` and `callback` are not signed.
fn signature(params: &[(String, String)], secret: &str) -> String {
    let mut signed: Vec<&(String, String)> = params
        .iter()
        .filter(|(name, _)| name != "format" && name != "callback")
        .collect();
    signed.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Md5::new();
    for (name, value) in signed {
        hasher.update(name.as_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.update(secret.as_bytes());
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(32), |mut hex, byte| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

// --- Sign-in: one-shot threads, nothing stays resident. ---

pub(crate) struct AuthStart {
    pub token: String,
    pub url: String,
}

fn one_shot<T: Send + 'static>(
    name: &str,
    operation: impl std::future::Future<Output = Result<T>> + Send + 'static,
) -> oneshot::Receiver<Result<T>> {
    let (sender, receiver) = oneshot::channel();
    let spawned = thread::Builder::new().name(name.into()).spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("could not start Last.fm runtime")
            .and_then(|runtime| runtime.block_on(operation));
        let _ = sender.send(result);
    });
    if let Err(error) = spawned {
        log::warn!("could not start Last.fm sign-in: {error}");
    }
    receiver
}

fn friendly(error: CallError) -> anyhow::Error {
    match error {
        CallError::Api { code: 10, .. } => anyhow!("Last.fm rejected that API key"),
        CallError::Api { code: 13, .. } => anyhow!("Last.fm rejected the shared secret"),
        CallError::Api { code: 14, .. } => {
            anyhow!("Approve Sonder on the Last.fm page first, then try again")
        }
        CallError::Api { code: 15, .. } | CallError::Api { code: 4, .. } => {
            anyhow!("That sign-in link expired. Press Connect again")
        }
        other => anyhow!("{other}"),
    }
}

/// Step one: ask Last.fm for a token and return the page to approve it on.
pub(crate) fn begin_auth(
    api_key: String,
    api_secret: String,
) -> oneshot::Receiver<Result<AuthStart>> {
    one_shot("cadence-lastfm-auth", async move {
        ensure!(valid_credential(&api_key), "enter a valid Last.fm API key");
        ensure!(
            valid_credential(&api_secret),
            "enter a valid Last.fm shared secret"
        );
        let api = Api {
            client: api_client()?,
            key: api_key,
            secret: api_secret,
        };
        let reply = api
            .call(vec![("method".to_owned(), "auth.getToken".to_owned())])
            .await
            .map_err(friendly)?;
        let token = reply
            .get("token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .context("Last.fm did not return a token")?
            .to_owned();
        let url = url::Url::parse_with_params(
            AUTH_URL,
            [("api_key", api.key.as_str()), ("token", &token)],
        )?
        .into();
        Ok(AuthStart { token, url })
    })
}

/// Step two: trade the approved token for a session. The secret and session
/// key go to the keychain; only the username is returned for preferences.
pub(crate) fn finish_auth(
    api_key: String,
    api_secret: String,
    token: String,
) -> oneshot::Receiver<Result<String>> {
    one_shot("cadence-lastfm-auth", async move {
        let api = Api {
            client: api_client()?,
            key: api_key,
            secret: api_secret.clone(),
        };
        let reply = api
            .call(vec![
                ("method".to_owned(), "auth.getSession".to_owned()),
                ("token".to_owned(), token),
            ])
            .await
            .map_err(friendly)?;
        let session = reply
            .get("session")
            .context("Last.fm did not return a session")?;
        let username = session
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .context("Last.fm did not return a username")?
            .to_owned();
        let session_key = session
            .get("key")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty())
            .context("Last.fm did not return a session key")?;
        save_credentials(&api_secret, session_key)?;
        Ok(username)
    })
}

/// Removes the stored secret and session key.
pub(crate) fn forget_credentials() -> oneshot::Receiver<Result<()>> {
    one_shot("cadence-lastfm-forget", async {
        match entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => bail!("could not remove Last.fm credentials: {error}"),
        }
    })
}

fn entry() -> Result<Entry> {
    Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).map_err(Into::into)
}

fn save_credentials(api_secret: &str, session_key: &str) -> Result<()> {
    let value = serde_json::json!({"api_secret": api_secret, "session_key": session_key});
    entry()?
        .set_password(&value.to_string())
        .map_err(|error| anyhow!("could not save Last.fm credentials: {error}"))
}

fn load_credentials() -> Result<Option<(String, String)>> {
    match entry()?.get_password() {
        Ok(json) => {
            let value: Value = serde_json::from_str(&json)?;
            let field = |name: &str| value.get(name).and_then(Value::as_str).map(str::to_owned);
            Ok(field("api_secret").zip(field("session_key")))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(anyhow!(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(key: &str, duration_ms: u32) -> Arc<Snapshot> {
        Arc::new(Snapshot {
            key: key.into(),
            artist: "Artist".into(),
            title: key.into(),
            album: "Album".into(),
            duration_ms,
        })
    }

    fn state(track: &Arc<Snapshot>, playing: bool, position_ms: u32) -> State {
        State {
            track: Some(track.clone()),
            playing,
            position_ms,
        }
    }

    #[test]
    fn credentials_must_be_32_hex_characters() {
        assert!(valid_credential("0123456789abcdef0123456789ABCDEF"));
        assert!(!valid_credential("0123456789abcdef"));
        assert!(!valid_credential("0123456789abcdef0123456789abcdeg"));
    }

    #[test]
    fn signature_matches_the_documented_algorithm() {
        // Last.fm's documented example: md5("api_keyxxxxxxxxmethodauth.getSessiontokenyyyyyyyyysecret")
        let params = vec![
            ("method".to_owned(), "auth.getSession".to_owned()),
            ("token".to_owned(), "yyyyyyyy".to_owned()),
            ("api_key".to_owned(), "xxxxxxxx".to_owned()),
            ("format".to_owned(), "json".to_owned()),
        ];
        let mut expected = Md5::new();
        expected.update(b"api_keyxxxxxxxxmethodauth.getSessiontokenyyyyyyyysecret");
        let expected: String = expected
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(signature(&params, "secret"), expected);
        assert_eq!(expected.len(), 32);
    }

    #[test]
    fn scrobbles_after_half_the_track_of_listening_time() {
        let track = snapshot("a", 200_000);
        let start = Instant::now();
        let mut tracker = Tracker::default();
        assert!(
            tracker
                .update(&state(&track, true, 0), start, 1_000)
                .is_some()
        );
        assert_eq!(tracker.due(), Some(start + Duration::from_secs(100)));
        assert!(
            tracker
                .take_scrobble(start + Duration::from_secs(99))
                .is_none()
        );
        let scrobble = tracker
            .take_scrobble(start + Duration::from_secs(100))
            .unwrap();
        assert_eq!(scrobble.timestamp, 1_000);
        assert_eq!(scrobble.duration_s, 200);
        assert!(
            tracker
                .take_scrobble(start + Duration::from_secs(500))
                .is_none()
        );
        assert!(tracker.due().is_none());
    }

    #[test]
    fn long_tracks_scrobble_at_four_minutes() {
        let track = snapshot("a", 20 * 60 * 1000);
        let start = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(&state(&track, true, 0), start, 0);
        assert_eq!(tracker.due(), Some(start + Duration::from_secs(240)));
    }

    #[test]
    fn short_tracks_never_scrobble() {
        let track = snapshot("a", 29_000);
        let mut tracker = Tracker::default();
        tracker.update(&state(&track, true, 0), Instant::now(), 0);
        assert!(tracker.due().is_none());
    }

    #[test]
    fn paused_time_does_not_count_and_the_timer_moves() {
        let track = snapshot("a", 200_000);
        let start = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(&state(&track, true, 0), start, 0);
        tracker.update(
            &state(&track, false, 40_000),
            start + Duration::from_secs(40),
            0,
        );
        assert!(tracker.due().is_none(), "paused: nothing scheduled");
        let resumed = start + Duration::from_secs(1_000);
        assert!(
            tracker
                .update(&state(&track, true, 40_000), resumed, 0)
                .is_none()
        );
        assert_eq!(tracker.due(), Some(resumed + Duration::from_secs(60)));
    }

    #[test]
    fn seeking_forward_does_not_earn_a_scrobble() {
        let track = snapshot("a", 200_000);
        let start = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(&state(&track, true, 0), start, 0);
        tracker.update(
            &state(&track, true, 190_000),
            start + Duration::from_secs(1),
            0,
        );
        assert!(
            tracker
                .take_scrobble(start + Duration::from_secs(2))
                .is_none()
        );
    }

    #[test]
    fn skipping_early_scrobbles_nothing_and_a_new_track_starts_fresh() {
        let first = snapshot("a", 200_000);
        let second = snapshot("b", 180_000);
        let start = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(&state(&first, true, 0), start, 0);
        let announced =
            tracker.update(&state(&second, true, 0), start + Duration::from_secs(10), 0);
        assert_eq!(announced.unwrap().key, "b");
        assert_eq!(tracker.due(), Some(start + Duration::from_secs(100)));
    }

    #[test]
    fn starting_paused_announces_only_when_playback_begins() {
        let track = snapshot("a", 200_000);
        let now = Instant::now();
        let mut tracker = Tracker::default();
        assert!(
            tracker
                .update(&state(&track, false, 5_000), now, 0)
                .is_none()
        );
        assert!(
            tracker
                .update(&state(&track, true, 5_000), now, 0)
                .is_some()
        );
        assert!(
            tracker
                .update(&state(&track, true, 5_500), now, 0)
                .is_none()
        );
    }

    #[test]
    fn replaying_a_scrobbled_track_counts_as_a_new_listen() {
        let track = snapshot("a", 100_000);
        let start = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(&state(&track, true, 0), start, 0);
        assert!(
            tracker
                .take_scrobble(start + Duration::from_secs(50))
                .is_some()
        );
        tracker.update(
            &state(&track, true, 99_000),
            start + Duration::from_secs(99),
            0,
        );
        let again = tracker.update(
            &state(&track, true, 500),
            start + Duration::from_secs(100),
            0,
        );
        assert!(again.is_some(), "replay is announced again");
        assert!(tracker.due().is_some(), "and can scrobble again");
    }

    #[test]
    fn stopping_clears_the_current_listen() {
        let track = snapshot("a", 200_000);
        let mut tracker = Tracker::default();
        tracker.update(&state(&track, true, 0), Instant::now(), 0);
        tracker.update(&State::default(), Instant::now(), 0);
        assert!(tracker.due().is_none());
    }

    #[test]
    fn transient_errors_retry_and_permanent_ones_do_not() {
        let api = |code| CallError::Api {
            code,
            message: String::new(),
        };
        assert!(api(11).transient());
        assert!(api(29).transient());
        assert!(CallError::Transport(anyhow!("offline")).transient());
        assert!(!api(13).transient());
        assert!(api(9).invalid_session());
    }
}
