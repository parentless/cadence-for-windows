use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Sender as StdSender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use librespot::{core::SpotifyUri, playback::player::PlayerEvent};
use tokio::sync::mpsc::{Receiver, Sender};

use crate::{
    model::{Album, Artist, CachedLibrary, LibraryFingerprint, Playlist, Track, UserProfile},
    playback::{Playback, PlaybackAuthorization, delete_playback_refresh_token},
    spotify::{
        ClientIdSource, Spotify, SpotifyConfiguration, resolve_configuration, valid_client_id,
    },
    storage::{PlaybackSnapshot, Store},
};

const CATALOG_TIMEOUT_SECONDS: u64 = 30;
const LIBRARY_TIMEOUT_SECONDS: u64 = 60;
const COMMAND_CAPACITY: usize = 256;
const CONTROL_CAPACITY: usize = 8;
const PLAYED_TRACK_WINDOW: usize = 50;

struct AuthorizationSuccess {
    playback: Option<PlaybackConnectionRequest>,
}

struct PlaybackConnectionRequest {
    load_saved_token: bool,
    authorization: Option<PlaybackAuthorization>,
}

#[derive(Clone)]
struct BlockingStore {
    jobs: std::sync::mpsc::Sender<StoreJob>,
}

type StoreJob = Box<dyn FnOnce(&mut Store) + Send>;
type RadioTask = tokio::task::JoinHandle<(u64, Result<Vec<Track>>)>;

impl BlockingStore {
    async fn open_default() -> Result<Self> {
        let (jobs, receiver) = std::sync::mpsc::channel::<StoreJob>();
        let (initialized, initialization) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("cadence-storage".to_owned())
            .spawn(move || {
                let mut store = match Store::open_default() {
                    Ok(store) => store,
                    Err(error) => {
                        let _ = initialized.send(Err(error.to_string()));
                        return;
                    }
                };
                let _ = initialized.send(Ok(()));
                while let Ok(job) = receiver.recv() {
                    job(&mut store);
                }
            })
            .context("could not start storage worker")?;
        initialization
            .await
            .context("storage worker stopped during initialization")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self { jobs })
    }

    #[cfg(test)]
    fn from_store(mut store: Store) -> Self {
        let (jobs, receiver) = std::sync::mpsc::channel::<StoreJob>();
        std::thread::spawn(move || {
            while let Ok(job) = receiver.recv() {
                job(&mut store);
            }
        });
        Self { jobs }
    }

    async fn call<T>(
        &self,
        operation: impl FnOnce(&mut Store) -> Result<T> + Send + 'static,
    ) -> Result<T>
    where
        T: Send + 'static,
    {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Box::new(move |store| {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(store)))
                        .map_err(|_| anyhow!("storage operation panicked"))
                        .and_then(|result| result);
                let _ = sender.send(result);
            }))
            .map_err(|_| anyhow!("storage worker is unavailable"))?;
        receiver
            .await
            .context("storage operation did not complete")?
    }

    async fn playback_state(&self) -> Result<Option<PlaybackSnapshot>> {
        self.call(|store| store.playback_state()).await
    }

    async fn local_state(&self) -> Result<(Vec<Track>, Vec<Playlist>, Vec<Track>)> {
        self.call(|store| {
            Ok((
                store.favorites()?,
                store.pinned_playlists()?,
                store.recent_tracks(100)?,
            ))
        })
        .await
    }

    async fn cached_library(&self) -> Result<CachedLibrary> {
        self.call(|store| store.cached_library()).await
    }

    async fn favorites(&self) -> Result<Vec<Track>> {
        self.call(|store| store.favorites()).await
    }

    async fn remove_spotify_client_id(&self) -> Result<()> {
        self.call(|store| store.remove_spotify_client_id()).await
    }

    async fn configure_spotify(&self, client_id: String) -> Result<()> {
        self.call(move |store| store.configure_spotify(&client_id))
            .await
    }

    async fn reset_spotify_configuration(&self) -> Result<()> {
        self.call(|store| store.reset_spotify_configuration()).await
    }

    async fn set_oauth_credentials_invalidated(&self, invalidated: bool) -> Result<()> {
        self.call(move |store| store.set_spotify_oauth_credentials_invalidated(invalidated))
            .await
    }

    async fn set_playback_credentials_invalidated(&self, invalidated: bool) -> Result<()> {
        self.call(move |store| store.set_spotify_playback_credentials_invalidated(invalidated))
            .await
    }

    async fn clear_library_cache(&self) -> Result<()> {
        self.call(|store| store.clear_library_cache()).await
    }

    async fn replace_library_cache_if_current(
        &self,
        liked_tracks: Vec<Track>,
        playlists: Vec<Playlist>,
        fingerprint: LibraryFingerprint,
        current_generation: Arc<AtomicU64>,
        generation: u64,
    ) -> Result<Option<(Vec<Track>, Vec<Playlist>)>> {
        self.call(move |store| {
            if current_generation.load(Ordering::Acquire) != generation {
                return Ok(None);
            }
            store.replace_library_cache(&liked_tracks, &playlists, Some(&fingerprint))?;
            Ok(Some((liked_tracks, playlists)))
        })
        .await
    }

    async fn clear_playback_state(&self) -> Result<()> {
        self.call(|store| store.clear_playback_state()).await
    }

    async fn set_playback_state(
        &self,
        tracks: Vec<Track>,
        index: usize,
        position_ms: u32,
        cursor: Option<(Playlist, u32)>,
    ) -> Result<()> {
        self.call(move |store| {
            store.set_playlist_playback_state(&tracks, index, position_ms, &cursor)
        })
        .await
    }

    async fn update_playback_position(&self, position_ms: u32) -> Result<()> {
        self.call(move |store| store.update_playback_position(position_ms))
            .await
    }

    async fn set_favorite(&self, track: Track, favorite: bool) -> Result<()> {
        self.call(move |store| store.set_favorite(&track, favorite))
            .await
    }

    async fn set_playlist_pinned(&self, playlist: Playlist, pinned: bool) -> Result<()> {
        self.call(move |store| store.set_playlist_pinned(&playlist, pinned))
            .await
    }

    async fn add_history(&self, track: Track) -> Result<()> {
        self.call(move |store| store.add_history(&track)).await
    }
}

/// Where a catalog request sends its answer. Dropping the receiving half
/// cancels the request: the reply simply goes nowhere.
pub type Reply<T> = tokio::sync::oneshot::Sender<Result<T>>;

/// Tracks and playlists, as returned by both search and a library load.
pub type TrackAndPlaylistResults = (Vec<Track>, Vec<Playlist>);

/// A library reload's answer: fresh contents, or proof nothing changed for
/// the price of the two head requests.
#[derive(Debug)]
pub enum LibraryReload {
    Unchanged,
    Fresh(TrackAndPlaylistResults),
}

impl LibraryFingerprint {
    fn new(liked: &(Vec<Track>, u32), playlists: &(Vec<(Playlist, String)>, u32)) -> Self {
        Self {
            liked_head: liked
                .0
                .iter()
                .map(|track| track.source_id.clone())
                .collect(),
            liked_total: liked.1,
            playlist_head: playlists
                .0
                .iter()
                .map(|(playlist, snapshot)| {
                    (
                        playlist.source_id.clone(),
                        playlist.name.to_string(),
                        snapshot.clone(),
                    )
                })
                .collect(),
            playlist_total: playlists.1,
        }
    }
}

type SharedFingerprint = Arc<std::sync::Mutex<Option<LibraryFingerprint>>>;

fn commit_fingerprint(fingerprint: &SharedFingerprint, value: LibraryFingerprint) {
    set_fingerprint(fingerprint, Some(value));
}

fn set_fingerprint(fingerprint: &SharedFingerprint, value: Option<LibraryFingerprint>) {
    *fingerprint.lock().expect("library fingerprint lock") = value;
}
pub type ArtistDetails = (Artist, Vec<Track>, Vec<Album>);
pub type AlbumDetails = (Album, Vec<Track>);

#[derive(Debug)]
pub enum BackendCommand {
    Authenticate {
        generation: u64,
    },
    Logout {
        generation: u64,
    },
    ConfigureSpotify {
        generation: u64,
        client_id: String,
    },
    ResetSpotifyConfiguration {
        generation: u64,
    },
    ReloadLibrary {
        respond: Reply<LibraryReload>,
    },
    SearchCatalog {
        query: String,
        respond: Reply<crate::model::CatalogSearchResults>,
    },
    LoadPlaylist {
        playlist: Playlist,
        offset: u32,
        respond: Reply<crate::model::PlaylistTrackPage>,
    },
    PlayQueueItem {
        offset: usize,
    },
    PlayPlaylistContext {
        playlist: Playlist,
        tracks: Vec<Track>,
        index: usize,
        next_offset: Option<u32>,
    },
    LoadArtist {
        source_id: String,
        respond: Reply<ArtistDetails>,
    },
    LoadAlbum {
        source_id: String,
        respond: Reply<AlbumDetails>,
    },
    StartRadio {
        request_id: u64,
        seed: Track,
    },
    PlayContext {
        tracks: Vec<Track>,
        index: usize,
    },
    PlayNext(Track),
    AppendToQueue(Track),
    /// The track started playing, so it belongs in the listening history.
    MarkPlayed(Track),
    RestorePlayback {
        position_ms: u32,
        playing: bool,
    },
    SetSpotifyLiked {
        track: Track,
        liked: bool,
        respond: Reply<()>,
    },
    SetFavorite {
        track: Track,
        favorite: bool,
    },
    SetPlaylistPinned {
        playlist: Playlist,
        pinned: bool,
    },
    Resume,
    Pause,
    Next,
    Previous,
    Seek(u32),
    SavePlaybackPosition {
        spotify_uri: String,
        position_ms: u32,
    },
    SetShuffle(bool),
    SetVolume(f32),
    Shutdown {
        acknowledged: StdSender<()>,
    },
}

#[derive(Debug)]
pub enum BackendEvent {
    SetupRequired,
    SpotifyConfigured {
        generation: u64,
        client_id: String,
        source: ClientIdSource,
    },
    SpotifyConfigurationFailed {
        generation: u64,
        error: String,
    },
    SpotifyConfigurationResetFailed(String),
    AuthorizationRequired,
    LoggedOut,
    CatalogReady {
        generation: u64,
    },
    PlaybackReady,
    PlaybackReconnecting,
    PlaybackReconnected,
    PlaybackRestored {
        position_ms: u32,
        playing: bool,
    },
    PlaybackSettled,
    QueueEnded,
    LibraryLoaded {
        generation: u64,
        liked_tracks: Vec<Track>,
        playlists: Vec<Playlist>,
    },
    ProfileLoaded {
        generation: u64,
        profile: UserProfile,
    },
    /// The library as the last launch left it, served before Spotify answers.
    CachedLibrary {
        generation: u64,
        liked_tracks: Vec<Track>,
        playlists: Vec<Playlist>,
    },
    /// The boot revalidation matched the cached library; nothing to replace.
    LibraryUnchanged {
        generation: u64,
    },
    LocalStateLoaded {
        favorites: Vec<Track>,
        pinned_playlists: Vec<Playlist>,
        recently_played: Vec<Track>,
    },
    Playing {
        spotify_uri: String,
    },
    Loading {
        spotify_uri: String,
    },
    Paused {
        spotify_uri: String,
    },
    EndOfTrack {
        spotify_uri: String,
    },
    PositionChanged {
        spotify_uri: String,
        position_ms: u32,
    },
    PlaybackContext {
        current: Track,
        next: Vec<Track>,
    },
    PlaybackSnapshotLoaded {
        current: Track,
        next: Vec<Track>,
        position_ms: u32,
    },
    AuthorizationFailed(String),
    CatalogFailed {
        generation: u64,
        error: String,
    },
    PlaybackFailed(String),
    TrackFailed {
        spotify_uri: String,
        error: String,
    },
    RadioFailed {
        request_id: u64,
        error: String,
    },
    RadioStarted {
        request_id: u64,
    },
    RadioCancelled {
        request_id: u64,
    },
    FatalError(String),
    Error(String),
}

const EVENT_CAPACITY: usize = 128;
type PositionUpdate = Option<(String, u32)>;

/// Reliable events apply backpressure; clock updates occupy one replaceable slot.
#[derive(Clone)]
pub(crate) struct EventSender {
    reliable: Sender<BackendEvent>,
    position: tokio::sync::watch::Sender<PositionUpdate>,
    shutdown: tokio::sync::watch::Receiver<bool>,
}

/// Bounded backend notifications with coalesced playback clock updates.
pub struct EventReceiver {
    reliable: Receiver<BackendEvent>,
    position: tokio::sync::watch::Receiver<PositionUpdate>,
}

pub(crate) fn event_channel(
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> (EventSender, EventReceiver) {
    let (sender, reliable) = tokio::sync::mpsc::channel(EVENT_CAPACITY);
    let (position_sender, position) = tokio::sync::watch::channel(None);
    (
        EventSender {
            reliable: sender,
            position: position_sender,
            shutdown,
        },
        EventReceiver { reliable, position },
    )
}

impl EventSender {
    pub(crate) async fn send(&self, event: BackendEvent) -> Result<(), ()> {
        if *self.shutdown.borrow() {
            return Err(());
        }
        if let BackendEvent::PositionChanged {
            spotify_uri,
            position_ms,
        } = event
        {
            return self
                .position
                .send(Some((spotify_uri, position_ms)))
                .map_err(|_| ());
        }
        self.send_ordered(event).await
    }

    async fn send_ordered(&self, event: BackendEvent) -> Result<(), ()> {
        if *self.shutdown.borrow() {
            return Err(());
        }
        if matches!(&event, BackendEvent::PositionChanged { .. }) {
            // Playing/Paused carry an exact position in the reliable stream.
            // Clear older replaceable ticks so they cannot rewind that anchor.
            let _ = self.position.send(None);
        }
        let mut shutdown = self.shutdown.clone();
        tokio::select! {
            result = self.reliable.send(event) => result.map_err(|_| ()),
            _ = wait_for_shutdown(&mut shutdown) => Err(()),
        }
    }
}

impl EventReceiver {
    fn latest_position(&mut self) -> Option<BackendEvent> {
        self.position
            .borrow_and_update()
            .clone()
            .map(|(spotify_uri, position_ms)| BackendEvent::PositionChanged {
                spotify_uri,
                position_ms,
            })
    }

    pub async fn recv(&mut self) -> Option<BackendEvent> {
        loop {
            if let Ok(event) = self.try_recv() {
                return Some(event);
            }
            tokio::select! {
                biased;
                event = self.reliable.recv() => {
                    if event.is_some() { return event; }
                    return if self.position.changed().await.is_ok() { self.latest_position() } else { None };
                }
                changed = self.position.changed() => {
                    if changed.is_err() { return self.reliable.recv().await; }
                    if let Some(position) = self.latest_position() { return Some(position); }
                }
            }
        }
    }

    pub fn try_recv(&mut self) -> Result<BackendEvent, tokio::sync::mpsc::error::TryRecvError> {
        match self.reliable.try_recv() {
            Ok(event) => Ok(event),
            Err(error) => {
                if self.position.has_changed().unwrap_or(false) {
                    self.latest_position().ok_or(error)
                } else {
                    Err(error)
                }
            }
        }
    }
}

struct Senders {
    commands: Sender<BackendCommand>,
    controls: Sender<BackendCommand>,
    volume: tokio::sync::watch::Sender<f32>,
}

/// The sending half of the backend. Holders keep it for the life of the process:
/// restarting the worker redirects the senders in place, so a handle taken
/// before a restart still reaches the worker running after it.
#[derive(Clone)]
pub struct BackendHandle {
    senders: Arc<std::sync::Mutex<Senders>>,
}

impl BackendHandle {
    pub fn send(&self, command: BackendCommand) -> bool {
        let senders = self
            .senders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        send_command(
            &senders.commands,
            &senders.controls,
            &senders.volume,
            command,
        )
    }

    fn redirect(&self, senders: Senders) {
        *self
            .senders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = senders;
    }
}

/// Owns the backend worker thread. Dropping this stops playback, so it is held
/// by a process-wide service rather than by a window.
pub struct Backend {
    handle: BackendHandle,
    /// This worker's own command channel, kept separate from `handle` because a
    /// restart redirects the handle at the replacement worker. Shutting down
    /// through the handle would stop the new worker instead of this one.
    commands: Sender<BackendCommand>,
    shutdown: tokio::sync::watch::Sender<bool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Backend {
    #[cfg(test)]
    pub(crate) fn isolated() -> (
        Self,
        tokio::sync::mpsc::Receiver<BackendCommand>,
        tokio::sync::watch::Receiver<f32>,
    ) {
        let (commands, receiver) = tokio::sync::mpsc::channel(COMMAND_CAPACITY);
        let (volume, volume_receiver) = tokio::sync::watch::channel(0.72);
        let (shutdown, _) = tokio::sync::watch::channel(false);
        let handle = BackendHandle {
            senders: Arc::new(std::sync::Mutex::new(Senders {
                commands: commands.clone(),
                controls: commands.clone(),
                volume,
            })),
        };
        (
            Self {
                handle,
                commands,
                shutdown,
                thread: None,
            },
            receiver,
            volume_receiver,
        )
    }

    pub fn start() -> (Self, EventReceiver) {
        let (senders, shutdown, thread, events) = Self::spawn_worker();
        let commands = senders.commands.clone();
        (
            Self {
                handle: BackendHandle {
                    senders: Arc::new(std::sync::Mutex::new(senders)),
                },
                commands,
                shutdown,
                thread: Some(thread),
            },
            events,
        )
    }

    /// Starts a replacement worker and points `handle`, and every clone of it
    /// already handed out, at the new one.
    pub fn restart(handle: &BackendHandle) -> (Self, EventReceiver) {
        let (senders, shutdown, thread, events) = Self::spawn_worker();
        let commands = senders.commands.clone();
        handle.redirect(senders);
        (
            Self {
                handle: handle.clone(),
                commands,
                shutdown,
                thread: Some(thread),
            },
            events,
        )
    }

    fn spawn_worker() -> (
        Senders,
        tokio::sync::watch::Sender<bool>,
        thread::JoinHandle<()>,
        EventReceiver,
    ) {
        let (commands, command_receiver) = tokio::sync::mpsc::channel(COMMAND_CAPACITY);
        let (controls, control_receiver) = tokio::sync::mpsc::channel(CONTROL_CAPACITY);
        let (volume, volume_receiver) = tokio::sync::watch::channel(0.72);
        let (shutdown, shutdown_receiver) = tokio::sync::watch::channel(false);
        let (event_sender, events) = event_channel(shutdown_receiver.clone());
        let thread = thread::Builder::new()
            .name("cadence-backend".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Runtime::new().expect("could not start Tokio");
                let acknowledged = runtime.block_on(run(
                    command_receiver,
                    control_receiver,
                    event_sender,
                    volume_receiver,
                    shutdown_receiver,
                ));
                runtime.shutdown_timeout(Duration::from_secs(1));
                if let Some(acknowledged) = acknowledged {
                    let _ = acknowledged.send(());
                }
            })
            .expect("could not start the Cadence backend");
        (
            Senders {
                commands,
                controls,
                volume,
            },
            shutdown,
            thread,
            events,
        )
    }

    pub fn handle(&self) -> BackendHandle {
        self.handle.clone()
    }
}

fn send_command(
    commands: &Sender<BackendCommand>,
    controls: &Sender<BackendCommand>,
    volume_sender: &tokio::sync::watch::Sender<f32>,
    command: BackendCommand,
) -> bool {
    if matches!(
        &command,
        BackendCommand::Authenticate { .. } | BackendCommand::Logout { .. }
    ) {
        return controls.try_send(command).is_ok();
    }
    match command {
        BackendCommand::SetVolume(volume) => volume_sender.send(volume).is_ok(),
        command => commands.try_send(command).is_ok(),
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        if self.thread.is_none() {
            return;
        }
        let (acknowledged, acknowledgment) = mpsc::channel();
        let _ = self.shutdown.send(true);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut command = BackendCommand::Shutdown { acknowledged };
        let sent = loop {
            match self.commands.try_send(command) {
                Ok(()) => break true,
                Err(tokio::sync::mpsc::error::TrySendError::Full(returned))
                    if Instant::now() < deadline =>
                {
                    command = returned;
                    thread::sleep(Duration::from_millis(1));
                }
                Err(_) => break false,
            }
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        let stopped = sent && acknowledgment.recv_timeout(remaining).is_ok();
        if stopped && let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Everything `run` needs once Spotify is usable.
struct Startup {
    store: BlockingStore,
    spotify: Spotify,
    configuration: Option<SpotifyConfiguration>,
    playback_snapshot: Option<PlaybackSnapshot>,
    playlist_cursor: Option<(Playlist, u32)>,
    playback_credentials_invalidated: bool,
}

/// The error carries the shutdown acknowledgment when the app quit before
/// setup finished, so the caller can hand it back to whoever asked to stop.
async fn start(
    commands: &mut Receiver<BackendCommand>,
    events: &EventSender,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<Startup, Option<StdSender<()>>> {
    let store = match BlockingStore::open_default().await {
        Ok(store) => store,
        Err(error) => {
            send_fatal_error(events, error).await;
            return Err(None);
        }
    };
    log::info!("startup: store opened");
    let playlist_cursor = store
        .call(|store| store.playback_playlist_cursor())
        .await
        .unwrap_or_else(|error| {
            log::warn!("could not restore playlist continuation: {error}");
            None
        });
    send_local_state(&store, events).await;
    let playback_snapshot = match store.playback_state().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            send_error(events, error).await;
            None
        }
    };
    if let Some(snapshot) = &playback_snapshot
        && let Some(current) = snapshot.tracks.get(snapshot.index)
    {
        let _ = events
            .send(BackendEvent::PlaybackSnapshotLoaded {
                current: current.clone(),
                next: snapshot
                    .tracks
                    .get(snapshot.index + 1..)
                    .unwrap_or_default()
                    .to_vec(),
                position_ms: snapshot.position_ms,
            })
            .await;
    }
    let environment_client_id = std::env::var("SPOTIFY_CLIENT_ID").ok();
    let saved_client_id = match store.call(|store| store.spotify_client_id()).await {
        Ok(client_id) => client_id,
        Err(error) => {
            send_fatal_error(events, error).await;
            return Err(None);
        }
    };
    let oauth_credentials_invalidated = match store
        .call(|store| store.spotify_oauth_credentials_invalidated())
        .await
    {
        Ok(invalidated) => invalidated,
        Err(error) => {
            send_fatal_error(events, error).await;
            return Err(None);
        }
    };
    let mut playback_credentials_invalidated = match store
        .call(|store| store.spotify_playback_credentials_invalidated())
        .await
    {
        Ok(invalidated) => invalidated,
        Err(error) => {
            send_fatal_error(events, error).await;
            return Err(None);
        }
    };
    let mut configuration =
        resolve_configuration(environment_client_id.as_deref(), saved_client_id.as_deref());
    let mut configuration_generation = 0;
    if let Some(configured) = configuration.clone()
        && !valid_client_id(&configured.client_id)
    {
        if configured.source == ClientIdSource::Environment {
            let _ = events
                .send(BackendEvent::SpotifyConfigured {
                    generation: 0,
                    client_id: configured.client_id,
                    source: ClientIdSource::Environment,
                })
                .await;
            let _ = events
                .send(BackendEvent::SpotifyConfigurationFailed {
                    generation: 0,
                    error: "SPOTIFY_CLIENT_ID must contain 32 hexadecimal characters".to_owned(),
                })
                .await;
            while let Some(command) = commands.recv().await {
                if let BackendCommand::Shutdown { acknowledged } = command {
                    return Err(Some(acknowledged));
                }
            }
            return Err(None);
        }
        if let Err(error) = store.call(|store| store.remove_spotify_client_id()).await {
            send_fatal_error(events, error).await;
            return Err(None);
        }
        configuration = None;
    }
    let spotify = loop {
        if let Some(configured) = &configuration {
            let spotify = tokio::select! {
                result = Spotify::from_client_id(
                    &configured.client_id,
                    !oauth_credentials_invalidated,
                ) => result,
                _ = wait_for_shutdown(shutdown) => return Err(receive_shutdown_acknowledgment(commands).await),
            };
            match spotify {
                Ok(spotify) => {
                    log::info!("startup: Spotify client configured");
                    break spotify;
                }
                Err(error) => {
                    let _ = events
                        .send(BackendEvent::SpotifyConfigurationFailed {
                            generation: configuration_generation,
                            error: error.to_string(),
                        })
                        .await;
                    if configured.source == ClientIdSource::Saved {
                        let _ = store.remove_spotify_client_id().await;
                        configuration = None;
                        continue;
                    }
                    return Err(None);
                }
            }
        }

        let _ = events.send(BackendEvent::SetupRequired).await;
        let command = tokio::select! {
            command = commands.recv() => command.ok_or(None)?,
            _ = wait_for_shutdown(shutdown) => return Err(receive_shutdown_acknowledgment(commands).await),
        };
        match command {
            BackendCommand::ConfigureSpotify {
                generation,
                client_id,
            } if valid_client_id(&client_id) => {
                let client_id = client_id.trim().to_owned();
                let candidate = tokio::select! {
                    result = Spotify::from_client_id(&client_id, false) => result,
                    _ = wait_for_shutdown(shutdown) => return Err(receive_shutdown_acknowledgment(commands).await),
                };
                let candidate = match candidate {
                    Ok(candidate) => candidate,
                    Err(error) => {
                        let _ = events
                            .send(BackendEvent::SpotifyConfigurationFailed {
                                generation,
                                error: error.to_string(),
                            })
                            .await;
                        continue;
                    }
                };
                if let Err(error) = store.configure_spotify(client_id.clone()).await {
                    let _ = events
                        .send(BackendEvent::SpotifyConfigurationFailed {
                            generation,
                            error: error.to_string(),
                        })
                        .await;
                    continue;
                }
                configuration = Some(SpotifyConfiguration {
                    client_id,
                    source: ClientIdSource::Saved,
                });
                configuration_generation = generation;
                playback_credentials_invalidated = true;
                break candidate;
            }
            BackendCommand::ConfigureSpotify { generation, .. } => {
                let _ = events
                    .send(BackendEvent::SpotifyConfigurationFailed {
                        generation,
                        error: "Spotify Client ID must contain 32 hexadecimal characters"
                            .to_owned(),
                    })
                    .await;
            }
            BackendCommand::Shutdown { acknowledged } => return Err(Some(acknowledged)),
            _ => {}
        }
    };
    let configured = configuration
        .as_ref()
        .expect("Spotify configuration must exist after setup");
    let _ = events
        .send(BackendEvent::SpotifyConfigured {
            generation: configuration_generation,
            client_id: configured.client_id.clone(),
            source: configured.source,
        })
        .await;
    Ok(Startup {
        store,
        spotify,
        configuration,
        playback_snapshot,
        playlist_cursor,
        playback_credentials_invalidated,
    })
}

async fn run(
    mut commands: Receiver<BackendCommand>,
    mut controls: Receiver<BackendCommand>,
    events: EventSender,
    mut volume: tokio::sync::watch::Receiver<f32>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Option<StdSender<()>> {
    let startup = match start(&mut commands, &events, &mut shutdown).await {
        Ok(startup) => startup,
        Err(acknowledged) => return acknowledged,
    };
    let mut worker = Worker::new(startup, events);
    if let Err(acknowledged) = worker.boot(&mut commands, &mut shutdown).await {
        return acknowledged;
    }
    let mut playback_health = tokio::time::interval(std::time::Duration::from_secs(5));
    playback_health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let command = tokio::select! {
            command = controls.recv() => command?,
            command = commands.recv() => command?,
            _ = wait_for_shutdown(&mut shutdown) => {
                worker.session.finish_pending_logout().await;
                return receive_shutdown_acknowledgment(&mut commands).await;
            }
            changed = volume.changed() => {
                if changed.is_ok() && let Some(player) = &worker.connection.player {
                    player.set_volume(*volume.borrow_and_update());
                }
                continue;
            }
            _ = playback_health.tick() => {
                worker.connection.reconnect_if_dead(&worker.events).await;
                continue;
            }
            reconnected = finished(&mut worker.connection.reconnect) => {
                worker.connection.finish_reconnect(reconnected, &worker.events).await;
                if let Some((tracks, index)) = worker.connection.pending_play.take()
                    && let Err(error) = worker.play_context(tracks, index).await
                {
                    send_error(&worker.events, error).await;
                }
                continue;
            }
            connected = finished(&mut worker.connection.connect) => {
                worker.finish_connect(connected).await;
                continue;
            }
            refreshed = finished(&mut worker.favorites.task) => {
                worker.finish_favorite_refresh(refreshed).await;
                continue;
            }
            page = finished(&mut worker.playlist_prefetch) => {
                worker.finish_playlist_page(page).await;
                continue;
            }
            extended = finished(&mut worker.autoplay.task) => {
                worker.finish_autoplay(extended).await;
                continue;
            }
            radio = finished(&mut worker.radio.task) => {
                worker.finish_radio(radio).await;
                continue;
            }
            authorization = finished(&mut worker.session.authorization) => {
                worker.finish_authorization(authorization).await;
                continue;
            }
            logout = finished(&mut worker.session.logout) => {
                worker.finish_logout(logout).await;
                continue;
            }
        };
        if let Some(acknowledged) = worker.handle_command(command, &mut shutdown).await {
            return Some(acknowledged);
        }
    }
}

/// What a polled task slot produced: `None` when the slot was empty, otherwise
/// the task's output or its cancellation/panic error.
type Finished<T> = Option<Result<T, tokio::task::JoinError>>;

/// The running backend once Spotify is configured: the session state plus the
/// services that own the in-flight work.
struct Worker {
    events: EventSender,
    store: BlockingStore,
    spotify: Spotify,
    configuration: Option<SpotifyConfiguration>,
    playback_credentials_invalidated: bool,
    account_generation: u64,
    catalog_generation: Arc<AtomicU64>,
    queue: PlayQueue,
    shuffle: bool,
    priority_next: usize,
    history_end: usize,
    connection: PlaybackConnection,
    catalog: CatalogFetches,
    favorites: FavoriteRefresh,
    radio: Radio,
    autoplay: Autoplay,
    playlist_cursor: Option<(Playlist, u32)>,
    pending_playlist_cursor: Option<Option<(Playlist, u32)>>,
    playlist_prefetch: Option<tokio::task::JoinHandle<Result<crate::model::PlaylistTrackPage>>>,
    session: SessionTasks,
}

impl Drop for Worker {
    fn drop(&mut self) {
        abort_task(&mut self.playlist_prefetch);
    }
}

impl Worker {
    fn new(startup: Startup, events: EventSender) -> Self {
        let queue = PlayQueue::from_snapshot(startup.playback_snapshot);
        let history_end = queue.index.unwrap_or(0);
        Self {
            events,
            store: startup.store,
            spotify: startup.spotify,
            configuration: startup.configuration,
            playback_credentials_invalidated: startup.playback_credentials_invalidated,
            account_generation: 0,
            catalog_generation: Arc::new(AtomicU64::new(0)),
            queue,
            history_end,
            shuffle: false,
            priority_next: 0,
            connection: PlaybackConnection::default(),
            catalog: CatalogFetches::default(),
            favorites: FavoriteRefresh::default(),
            radio: Radio::default(),
            autoplay: Autoplay::default(),
            playlist_cursor: startup.playlist_cursor,
            pending_playlist_cursor: None,
            playlist_prefetch: None,
            session: SessionTasks::default(),
        }
    }

    /// Kicks off the signed-in account's loads and the playback connection.
    /// The error carries the shutdown acknowledgment when the app quit first.
    async fn boot(
        &mut self,
        commands: &mut Receiver<BackendCommand>,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), Option<StdSender<()>>> {
        let is_authorized = tokio::select! {
            authorized = self.spotify.is_authorized() => match authorized {
                Ok(authorized) => authorized,
                Err(error) => {
                    send_fatal_error(&self.events, error).await;
                    return Err(None);
                }
            },
            _ = wait_for_shutdown(shutdown) => {
                return Err(receive_shutdown_acknowledgment(commands).await);
            }
        };
        if !is_authorized {
            let _ = self.events.send(BackendEvent::AuthorizationRequired).await;
            return Ok(());
        }
        log::info!("startup: account authorized");
        self.serve_cached_library().await;
        self.start_account_loads().await;
        self.connection.begin_connect(PlaybackConnectionRequest {
            load_saved_token: true,
            authorization: None,
        });
        Ok(())
    }

    /// Makes the session usable from the last launch's library before any
    /// request goes out. The fingerprint persisted beside it lets the boot
    /// revalidation answer Unchanged from the head probes alone; without a
    /// cache the load must deliver contents, so the fingerprint is forgotten.
    async fn serve_cached_library(&mut self) {
        let cached = match self.store.cached_library().await {
            Ok(cached) => cached,
            Err(error) => {
                send_error(&self.events, error).await;
                CachedLibrary::default()
            }
        };
        if cached.is_empty() {
            self.catalog.seed_fingerprint(None);
            return;
        }
        log::info!(
            "startup: serving {} liked tracks and {} playlists from the cache",
            cached.liked_tracks.len(),
            cached.playlists.len()
        );
        self.catalog.seed_fingerprint(cached.fingerprint);
        let generation = self.account_generation;
        let _ = self
            .events
            .send(BackendEvent::CachedLibrary {
                generation,
                liked_tracks: cached.liked_tracks,
                playlists: cached.playlists,
            })
            .await;
        let _ = self
            .events
            .send(BackendEvent::CatalogReady { generation })
            .await;
    }

    /// Returns the shutdown acknowledgment once a `Shutdown` command arrives;
    /// every other command is handled in place.
    async fn handle_command(
        &mut self,
        command: BackendCommand,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Option<StdSender<()>> {
        if self.session.logout.is_some()
            && !matches!(
                &command,
                BackendCommand::Logout { .. } | BackendCommand::Shutdown { .. }
            )
        {
            send_error(&self.events, "Spotify logout is still finishing").await;
            return None;
        }
        let result = match command {
            BackendCommand::ResetSpotifyConfiguration { generation } => {
                self.reset_spotify_configuration(generation).await
            }
            BackendCommand::ConfigureSpotify {
                generation,
                client_id,
            } => {
                self.configure_spotify(generation, client_id, shutdown)
                    .await
            }
            BackendCommand::Authenticate { generation } => {
                self.authenticate(generation).await;
                Ok(())
            }
            BackendCommand::Logout { generation } => {
                self.logout(generation).await;
                Ok(())
            }
            BackendCommand::ReloadLibrary { respond } => {
                self.reload_library(respond);
                Ok(())
            }
            BackendCommand::SearchCatalog { query, respond } => {
                self.catalog.search(self.spotify.clone(), query, respond);
                Ok(())
            }
            BackendCommand::LoadPlaylist {
                playlist,
                offset,
                respond,
            } => {
                self.catalog
                    .playlist(self.spotify.clone(), playlist, offset, respond);
                Ok(())
            }
            BackendCommand::LoadArtist { source_id, respond } => {
                self.catalog
                    .artist(self.spotify.clone(), source_id, respond);
                Ok(())
            }
            BackendCommand::LoadAlbum { source_id, respond } => {
                self.catalog.album(self.spotify.clone(), source_id, respond);
                Ok(())
            }
            BackendCommand::StartRadio { request_id, seed } => {
                self.radio
                    .start(
                        request_id,
                        seed,
                        self.connection.player.clone(),
                        self.spotify.clone(),
                        &self.events,
                    )
                    .await;
                Ok(())
            }
            BackendCommand::PlayContext { tracks, index } => {
                self.prepare_playlist_context(None);
                self.play_context(tracks, index).await
            }
            BackendCommand::PlayQueueItem { offset } => {
                let index = self.queue.index.and_then(|index| index.checked_add(offset));
                if let Some(index) = index.filter(|index| *index < self.queue.tracks.len()) {
                    self.load_queue_track(index).await
                } else {
                    Ok(())
                }
            }
            BackendCommand::PlayPlaylistContext {
                playlist,
                tracks,
                index,
                next_offset,
            } => {
                self.prepare_playlist_context(next_offset.map(|offset| (playlist, offset)));
                self.play_context(tracks, index).await
            }
            BackendCommand::PlayNext(track) => self.play_next(track).await,
            BackendCommand::AppendToQueue(track) => self.append_to_queue(track).await,
            BackendCommand::RestorePlayback {
                position_ms,
                playing,
            } => self.restore_playback(position_ms, playing).await,
            BackendCommand::SetSpotifyLiked {
                track,
                liked,
                respond,
            } => {
                let spotify = self.spotify.clone();
                let store = self.store.clone();
                let fingerprint = self.catalog.library_fingerprint.clone();
                let current_generation = self.catalog_generation.clone();
                let generation = self.account_generation;
                if self
                    .catalog
                    .library_write
                    .as_ref()
                    .is_some_and(|task| !task.is_finished())
                {
                    let _ = respond.send(Err(anyhow!(
                        "Another Spotify library update is still running"
                    )));
                    return None;
                }
                CatalogFetches::start(
                    &mut self.catalog.library_write,
                    respond,
                    "Spotify library update",
                    async move {
                        spotify.set_track_liked(&track.source_id, liked).await?;
                        set_fingerprint(&fingerprint, None);
                        if let Err(error) = store
                            .call(move |store| {
                                if current_generation.load(Ordering::Acquire) == generation {
                                    store.set_cached_track_liked(&track, liked)?;
                                }
                                Ok(())
                            })
                            .await
                        {
                            log::warn!(
                                "library: Spotify saved the change but cache update failed: {error:#}"
                            );
                        }
                        Ok(())
                    },
                );
                Ok(())
            }
            BackendCommand::SetFavorite { track, favorite } => {
                self.set_favorite(track, favorite).await
            }
            BackendCommand::SetPlaylistPinned { playlist, pinned } => {
                self.set_playlist_pinned(playlist, pinned).await
            }
            BackendCommand::Resume => self.resume().await,
            BackendCommand::Pause => self.pause().await,
            BackendCommand::MarkPlayed(track) => self.mark_played(track).await,
            BackendCommand::SetShuffle(enabled) => {
                self.shuffle = enabled;
                Ok(())
            }
            BackendCommand::Next => self.next_track().await,
            BackendCommand::Previous => self.previous_track().await,
            BackendCommand::Seek(position_ms) => self.seek(position_ms).await,
            BackendCommand::SavePlaybackPosition {
                spotify_uri,
                position_ms,
            } => self.save_playback_position(spotify_uri, position_ms).await,
            // Unreachable in practice: send_command diverts SetVolume into the
            // volume watch, which the select loop applies directly.
            BackendCommand::SetVolume(volume) => self
                .connected_player()
                .map(|player| player.set_volume(volume)),
            BackendCommand::Shutdown { acknowledged } => {
                self.shutdown().await;
                return Some(acknowledged);
            }
        };
        if let Err(error) = result {
            send_error(&self.events, error).await;
        }
        None
    }

    async fn reset_spotify_configuration(&mut self, generation: u64) -> Result<()> {
        abort_task(&mut self.session.authorization);
        abort_task(&mut self.connection.connect);
        self.catalog_generation.store(generation, Ordering::Release);
        if self.configuration_is_from_environment() {
            let _ = self.events.send(BackendEvent::SpotifyConfigurationResetFailed(
                "SPOTIFY_CLIENT_ID is configured by the environment and cannot be changed in Cadence".to_owned(),
            )).await;
            return Ok(());
        }
        if let Err(error) = self.store.reset_spotify_configuration().await {
            let _ = self
                .events
                .send(BackendEvent::SpotifyConfigurationResetFailed(
                    error.to_string(),
                ))
                .await;
            return Ok(());
        }
        self.abort_account_work().await;
        self.stop_playback_session().await;
        self.playback_credentials_invalidated = true;
        self.configuration = None;
        if let Err(error) = self.spotify.logout().await {
            send_error(&self.events, error).await;
        }
        if let Err(error) = delete_playback_refresh_token().await {
            send_error(&self.events, error).await;
        }
        if let Err(error) = self.store.clear_library_cache().await {
            send_error(&self.events, error).await;
        }
        if let Err(error) = self.store.clear_playback_state().await {
            send_error(&self.events, error).await;
        }
        let _ = self.events.send(BackendEvent::LoggedOut).await;
        let _ = self.events.send(BackendEvent::SetupRequired).await;
        Ok(())
    }

    async fn configure_spotify(
        &mut self,
        generation: u64,
        client_id: String,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        abort_task(&mut self.session.authorization);
        abort_task(&mut self.connection.connect);
        if self.configuration_is_from_environment() {
            self.send_configuration_failed(
                generation,
                "SPOTIFY_CLIENT_ID is configured by the environment and cannot be changed in Cadence",
            ).await;
            return Ok(());
        }
        if self.configuration.is_some() {
            self.send_configuration_failed(
                generation,
                "Remove the current Spotify configuration before replacing it.",
            )
            .await;
            return Ok(());
        }
        if !valid_client_id(&client_id) {
            self.send_configuration_failed(
                generation,
                "Spotify Client ID must contain 32 hexadecimal characters",
            )
            .await;
            return Ok(());
        }
        let client_id = client_id.trim().to_owned();
        let candidate = tokio::select! {
            result = Spotify::from_client_id(&client_id, false) => result,
            _ = wait_for_shutdown(shutdown) => return Ok(()),
        };
        let candidate = match candidate {
            Ok(candidate) => candidate,
            Err(error) => {
                self.send_configuration_failed(generation, error).await;
                return Ok(());
            }
        };
        if let Err(error) = self.store.configure_spotify(client_id.clone()).await {
            self.send_configuration_failed(generation, error).await;
            return Ok(());
        }
        self.spotify = candidate;
        self.playback_credentials_invalidated = true;
        self.configuration = Some(SpotifyConfiguration {
            client_id: client_id.clone(),
            source: ClientIdSource::Saved,
        });
        let _ = self
            .events
            .send(BackendEvent::SpotifyConfigured {
                generation,
                client_id,
                source: ClientIdSource::Saved,
            })
            .await;
        let _ = self.events.send(BackendEvent::AuthorizationRequired).await;
        Ok(())
    }

    async fn authenticate(&mut self, generation: u64) {
        abort_task(&mut self.connection.connect);
        let needs_playback = self
            .connection
            .player
            .as_ref()
            .is_none_or(|player| !player.is_connected());
        self.session.begin_authorization(
            generation,
            self.spotify.clone(),
            needs_playback,
            self.playback_credentials_invalidated,
        );
    }

    async fn logout(&mut self, generation: u64) {
        abort_task(&mut self.session.authorization);
        abort_task(&mut self.connection.connect);
        self.catalog_generation.store(generation, Ordering::Release);
        self.abort_account_work().await;
        self.stop_playback_session().await;
        self.playback_credentials_invalidated = true;
        self.session
            .begin_logout(self.store.clone(), self.spotify.clone());
    }

    fn reload_library(&mut self, respond: Reply<LibraryReload>) {
        // The boot load owns the first fetch; racing it would walk the whole
        // library twice. Answering Unchanged leaves the boot result standing.
        if self
            .catalog
            .library
            .as_ref()
            .is_some_and(|boot| !boot.is_finished())
        {
            let _ = respond.send(Ok(LibraryReload::Unchanged));
            return;
        }
        let spotify = self.spotify.clone();
        let store = self.store.clone();
        let generation = self.account_generation;
        let current_generation = self.catalog_generation.clone();
        let fingerprint = self.catalog.library_fingerprint.clone();
        abort_task(&mut self.catalog.reload);
        self.catalog.reload = Some(tokio::spawn(async move {
            let loaded =
                run_with_timeout(LIBRARY_TIMEOUT_SECONDS, "Spotify library request", async {
                    probe_and_load_library(&spotify, &fingerprint).await
                })
                .await;
            // Keep the on-disk copy in step so the next launch paints
            // the refreshed list before the network answers.
            let loaded = match loaded {
                Ok(ProbedLibrary::Unchanged) => Ok(LibraryReload::Unchanged),
                Ok(ProbedLibrary::Changed {
                    contents: (liked_tracks, playlists),
                    fingerprint: probed,
                }) => match persist_library_cache(
                    &store,
                    liked_tracks,
                    playlists,
                    probed.clone(),
                    current_generation,
                    generation,
                )
                .await
                {
                    Ok(Some(contents)) => {
                        commit_fingerprint(&fingerprint, probed);
                        Ok(LibraryReload::Fresh(contents))
                    }
                    Ok(None) => Err(anyhow!("Spotify account changed while loading")),
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            let _ = respond.send(loaded);
        }));
    }

    async fn play_context(&mut self, tracks: Vec<Track>, index: usize) -> Result<()> {
        self.radio.cancel(&self.events).await;
        if self.connection.is_connecting() {
            self.connection.hold_play(tracks, index);
            return Ok(());
        }
        let spotify_uri = tracks
            .get(index)
            .and_then(|track| track.spotify_uri.clone())
            .unwrap_or_default();
        match load_context_track(&self.connection.player, &tracks, index, &self.events).await {
            Ok(()) => {
                if let Some(cursor) = self.pending_playlist_cursor.take() {
                    self.playlist_cursor = cursor;
                }
                self.queue.tracks = tracks;
                self.priority_next = 0;
                self.history_end = index;
                self.commit_loaded_queue(index).await;
            }
            Err(error) => {
                self.pending_playlist_cursor = None;
                let _ = self
                    .events
                    .send(BackendEvent::TrackFailed {
                        spotify_uri,
                        error: error.to_string(),
                    })
                    .await;
                let _ = self.events.send(BackendEvent::PlaybackSettled).await;
            }
        }
        Ok(())
    }

    async fn play_next(&mut self, track: Track) -> Result<()> {
        let index = self.queue.index.context("Nothing is currently playing")?;
        let mut updated_tracks = self.queue.tracks.clone();
        updated_tracks.insert(index + 1, track);
        self.replace_queue(updated_tracks, index).await?;
        if self.queue.index.unwrap_or(0) < self.history_end {
            // Editing from an earlier track establishes a new upcoming queue.
            self.history_end = self.queue.index.unwrap_or(0);
            self.priority_next = 0;
        }
        self.priority_next += 1;
        Ok(())
    }

    async fn append_to_queue(&mut self, track: Track) -> Result<()> {
        self.play_next(track).await
    }

    async fn replace_queue(&mut self, mut tracks: Vec<Track>, index: usize) -> Result<()> {
        let prune = index.saturating_sub(PLAYED_TRACK_WINDOW);
        tracks.drain(..prune);
        let index = index - prune;
        self.store
            .set_playback_state(
                tracks.clone(),
                index,
                self.queue.position_ms,
                self.playlist_cursor.clone(),
            )
            .await?;
        self.history_end = self.history_end.saturating_sub(prune);
        self.queue.tracks = tracks;
        self.queue.index = Some(index);
        send_playback_context(&self.queue.tracks, index, &self.events).await;
        Ok(())
    }

    async fn restore_playback(&mut self, position_ms: u32, playing: bool) -> Result<()> {
        let result = restore_context_track(
            &self.connection.player,
            &self.queue.tracks,
            self.queue.index,
            position_ms,
            playing,
            &self.events,
        )
        .await;
        if result.is_err() {
            let _ = self.events.send(BackendEvent::PlaybackSettled).await;
            return result;
        }
        let _ = self
            .events
            .send(BackendEvent::PlaybackRestored {
                position_ms,
                playing,
            })
            .await;
        // The restore performed a fresh load; the ended special-casing must
        // not survive it, or seeks would be swallowed against a live player.
        self.queue.ended = false;
        self.queue.position_ms = position_ms;
        if let Err(error) = self.store.update_playback_position(position_ms).await {
            send_error(&self.events, error).await;
        }
        if playing {
            self.maybe_prefetch_autoplay();
        }
        result
    }

    async fn mark_played(&mut self, track: Track) -> Result<()> {
        self.store.add_history(track).await?;
        send_local_state(&self.store, &self.events).await;
        Ok(())
    }

    async fn set_favorite(&mut self, track: Track, favorite: bool) -> Result<()> {
        self.store.set_favorite(track, favorite).await?;
        send_local_state(&self.store, &self.events).await;
        Ok(())
    }

    async fn set_playlist_pinned(&mut self, playlist: Playlist, pinned: bool) -> Result<()> {
        self.store.set_playlist_pinned(playlist, pinned).await?;
        send_local_state(&self.store, &self.events).await;
        Ok(())
    }

    async fn resume(&mut self) -> Result<()> {
        if self.queue.ended {
            // play() is a no-op in librespot's EndOfTrack state: reload the
            // current track at the seeker's position instead.
            let result = restore_context_track(
                &self.connection.player,
                &self.queue.tracks,
                self.queue.index,
                self.queue.position_ms,
                true,
                &self.events,
            )
            .await;
            match &result {
                Ok(()) => {
                    self.queue.ended = false;
                    // Replaying the still-last track re-arms the prefetch a
                    // failed earlier fetch may have left unarmed.
                    self.maybe_prefetch_autoplay();
                }
                Err(_) => {
                    let _ = self.events.send(BackendEvent::PlaybackSettled).await;
                }
            }
            return result;
        }
        let result = self.connected_player().map(|player| player.play());
        if result.is_err() {
            let _ = self.events.send(BackendEvent::PlaybackSettled).await;
        } else {
            self.maybe_prefetch_autoplay();
        }
        result
    }

    async fn pause(&self) -> Result<()> {
        let result = self.connected_player().map(|player| player.pause());
        if result.is_err() {
            let _ = self.events.send(BackendEvent::PlaybackSettled).await;
        }
        result
    }

    async fn next_track(&mut self) -> Result<()> {
        self.radio.cancel(&self.events).await;
        let Some(current) = self.queue.index else {
            let _ = self.events.send(BackendEvent::QueueEnded).await;
            return Ok(());
        };
        let next = current
            .checked_add(1)
            .filter(|index| *index < self.queue.tracks.len());
        let Some(index) = next else {
            // Nothing left to play: keep the last track current, but rewind
            // the seeker so Play visibly means "from the start". Skipping
            // forward mid-song lands here too, so silence the player rather
            // than showing a stopped UI over audio that keeps going.
            if let Ok(player) = self.connected_player() {
                player.stop();
            }
            self.queue.ended = true;
            self.queue.position_ms = 0;
            if let Err(error) = self.store.update_playback_position(0).await {
                send_error(&self.events, error).await;
            }
            let _ = self.events.send(BackendEvent::QueueEnded).await;
            // Late fallback: if the song outran the autoplay prefetch (or
            // none ran), this fetch resumes playback on arrival.
            self.maybe_prefetch_autoplay();
            return Ok(());
        };
        // Shuffle only the loaded lookahead: no playlist-wide fetch, duplicate
        // metadata, audio callback work, or changes to already-played history.
        // Explicit Add to Queue / Play Next entries retain their priority.
        let chosen = if self.shuffle && self.priority_next == 0 && current >= self.history_end {
            fastrand::usize(index..self.queue.tracks.len())
        } else {
            index
        };
        self.queue.tracks[index..=chosen].rotate_right(1);
        let result = self.load_queue_track(index).await;
        if result.is_err() {
            self.queue.tracks[index..=chosen].rotate_left(1);
        }
        result
    }

    async fn previous_track(&mut self) -> Result<()> {
        self.radio.cancel(&self.events).await;
        if let Some(index) = self.queue.index.and_then(|index| index.checked_sub(1)) {
            return self.load_queue_track(index).await;
        }
        let result = self.connected_player().map(|player| player.seek(0));
        let _ = self.events.send(BackendEvent::PlaybackSettled).await;
        if result.is_ok() {
            self.queue.position_ms = 0;
            if let Err(error) = self.store.update_playback_position(0).await {
                send_error(&self.events, error).await;
            }
        }
        result
    }

    /// Loads the queue entry at `index` and persists it as the playing track.
    async fn load_queue_track(&mut self, index: usize) -> Result<()> {
        let result = load_context_track(
            &self.connection.player,
            &self.queue.tracks,
            index,
            &self.events,
        )
        .await;
        if result.is_err() {
            let _ = self.events.send(BackendEvent::PlaybackSettled).await;
            return result;
        }
        self.priority_next = self
            .priority_next
            .saturating_sub(index.saturating_sub(self.history_end));
        self.commit_loaded_queue(index).await;
        result
    }

    /// The invariant after any successful queue-track load: current index,
    /// rewound position, a live (not ended) queue, a persisted snapshot,
    /// and an armed autoplay prefetch when the track is the queue's last.
    async fn commit_loaded_queue(&mut self, index: usize) {
        self.queue.index = Some(index);
        self.queue.position_ms = 0;
        self.queue.ended = false;
        let prune = index.saturating_sub(PLAYED_TRACK_WINDOW);
        self.history_end = self.history_end.max(index).saturating_sub(prune);
        self.queue.prune(prune);
        if let Err(error) = self
            .store
            .set_playback_state(
                self.queue.tracks.clone(),
                index - prune,
                0,
                self.playlist_cursor.clone(),
            )
            .await
        {
            send_error(&self.events, error).await;
        }
        self.maybe_prefetch_autoplay();
    }

    async fn seek(&mut self, position_ms: u32) -> Result<()> {
        // While ended, seek() would be a no-op too; remember the position for
        // the reload that Resume performs.
        if !self.queue.ended {
            self.connected_player()
                .map(|player| player.seek(position_ms))?;
        }
        self.queue.position_ms = position_ms;
        if let Err(error) = self.store.update_playback_position(position_ms).await {
            send_error(&self.events, error).await;
        }
        Ok(())
    }

    async fn save_playback_position(
        &mut self,
        spotify_uri: String,
        position_ms: u32,
    ) -> Result<()> {
        let current_uri = self
            .queue
            .index
            .and_then(|index| self.queue.tracks.get(index))
            .and_then(|track| track.spotify_uri.as_deref());
        if current_uri != Some(spotify_uri.as_str()) {
            return Ok(());
        }
        self.queue.position_ms = position_ms;
        self.store.update_playback_position(position_ms).await
    }

    async fn shutdown(&mut self) {
        self.abort_account_work().await;
        abort_task(&mut self.session.authorization);
        abort_task(&mut self.connection.connect);
        if let Some(task) = self.session.logout.take()
            && let Err(error) = task.await
        {
            send_error(&self.events, error).await;
        }
        self.connection.abort_attempts();
        self.connection.disconnect();
    }

    async fn finish_connect(&mut self, connected: Finished<Result<Playback>>) {
        self.connection.connect = None;
        match connected {
            Some(Ok(Ok(player))) => {
                log::info!("playback: connected");
                self.connection.adopt(player, &self.events);
                self.playback_credentials_invalidated = false;
                if let Err(error) = self.store.set_playback_credentials_invalidated(false).await {
                    send_error(&self.events, error).await;
                }
                let _ = self.events.send(BackendEvent::PlaybackReady).await;
                if let Some((tracks, index)) = self.connection.pending_play.take() {
                    if let Err(error) = self.play_context(tracks, index).await {
                        send_error(&self.events, error).await;
                    }
                } else if self.connection.connect_restoring {
                    let _ = self.events.send(BackendEvent::PlaybackReconnected).await;
                } else if let Err(error) = restore_saved_playback(
                    &self.connection.player,
                    &self.queue.tracks,
                    self.queue.index,
                    self.queue.position_ms,
                    &self.events,
                )
                .await
                {
                    send_error(&self.events, error).await;
                }
            }
            Some(Ok(Err(error))) => {
                self.connection
                    .fail_pending_play(&error, &self.events)
                    .await;
                self.pending_playlist_cursor = None;
                let _ = self
                    .events
                    .send(BackendEvent::PlaybackFailed(error.to_string()))
                    .await;
            }
            Some(Err(error)) => {
                self.connection
                    .fail_pending_play(&error, &self.events)
                    .await;
                self.pending_playlist_cursor = None;
                send_error(&self.events, error).await;
            }
            None => {}
        }
        self.connection.connect_restoring = false;
    }

    async fn finish_favorite_refresh(&mut self, refreshed: Finished<Result<Vec<Track>>>) {
        self.favorites.task = None;
        match refreshed {
            Some(Ok(Ok(tracks))) => {
                let mut changed = false;
                for track in tracks {
                    match self.store.set_favorite(track, true).await {
                        Ok(()) => changed = true,
                        Err(error) => send_error(&self.events, error).await,
                    }
                }
                if changed {
                    send_local_state(&self.store, &self.events).await;
                }
            }
            Some(Ok(Err(error))) => send_error(&self.events, error).await,
            Some(Err(error)) => send_error(&self.events, error).await,
            None => {}
        }
    }

    async fn finish_radio(&mut self, radio: Finished<(u64, Result<Vec<Track>>)>) {
        self.radio.task = None;
        self.radio.request_id = None;
        match radio {
            Some(Ok((request_id, Ok(tracks)))) => {
                match load_context_track(&self.connection.player, &tracks, 0, &self.events).await {
                    Ok(()) => {
                        self.cancel_playlist_continuation();
                        self.queue.tracks = tracks;
                        self.commit_loaded_queue(0).await;
                        let _ = self
                            .events
                            .send(BackendEvent::RadioStarted { request_id })
                            .await;
                    }
                    Err(error) => {
                        let _ = self
                            .events
                            .send(BackendEvent::RadioFailed {
                                request_id,
                                error: error.to_string(),
                            })
                            .await;
                        let _ = self.events.send(BackendEvent::PlaybackSettled).await;
                    }
                }
            }
            Some(Ok((request_id, Err(error)))) => {
                let _ = self
                    .events
                    .send(BackendEvent::RadioFailed {
                        request_id,
                        error: error.to_string(),
                    })
                    .await;
                let _ = self.events.send(BackendEvent::PlaybackSettled).await;
            }
            Some(Err(error)) => send_error(&self.events, error).await,
            None => {}
        }
    }

    async fn finish_authorization(
        &mut self,
        authorization: Finished<(u64, Result<AuthorizationSuccess>)>,
    ) {
        self.session.authorization = None;
        match authorization {
            Some(Ok((generation, Ok(success)))) => {
                if let Err(error) = self.store.set_oauth_credentials_invalidated(false).await {
                    send_error(&self.events, error).await;
                }
                self.account_generation = generation;
                self.catalog_generation.store(generation, Ordering::Release);
                self.abort_account_work().await;
                self.catalog.seed_fingerprint(None);
                self.start_account_loads().await;
                if let Some(request) = success.playback {
                    self.connection.begin_connect(request);
                }
            }
            Some(Ok((_, Err(error)))) => {
                let _ = self
                    .events
                    .send(BackendEvent::AuthorizationFailed(error.to_string()))
                    .await;
            }
            Some(Err(error)) => send_error(&self.events, error).await,
            None => {}
        }
    }

    async fn finish_logout(&mut self, logout: Finished<Result<()>>) {
        self.session.logout = None;
        match logout {
            Some(Ok(Ok(()))) => {
                let _ = self.events.send(BackendEvent::LoggedOut).await;
            }
            Some(Ok(Err(error))) => {
                let _ = self.events.send(BackendEvent::LoggedOut).await;
                send_error(&self.events, error).await;
            }
            Some(Err(error)) => send_error(&self.events, error).await,
            None => {}
        }
    }

    /// Starts the library load and favorite refresh for the signed-in account.
    async fn start_account_loads(&mut self) {
        self.catalog.load_library(
            self.spotify.clone(),
            self.store.clone(),
            self.account_generation,
            self.catalog_generation.clone(),
            self.events.clone(),
        );
        self.favorites
            .start(&self.store, self.spotify.clone(), &self.events)
            .await;
    }

    /// Stops every task tied to the signed-in account: catalog loads, the
    /// favorite refresh, and radio.
    async fn abort_account_work(&mut self) {
        self.cancel_playlist_continuation();
        self.catalog.abort_all();
        self.favorites.abort();
        self.radio.cancel(&self.events).await;
        abort_task(&mut self.autoplay.task);
    }

    /// Drops the live playback session and forgets the queue.
    async fn stop_playback_session(&mut self) {
        self.connection.pending_play = None;
        self.connection.disconnect();
        abort_task(&mut self.connection.reconnect);
        self.connection.reconnect_pending = false;
        self.queue.tracks.clear();
        self.queue.index = None;
        // A fresh session must not inherit the ended special-casing.
        self.queue.ended = false;
    }

    /// Starts a radio prefetch when the playing track is the queue's last,
    /// so autoplay can extend the queue before it runs out. The preference
    /// is read inside the task and re-checked when the result lands, so a
    /// toggle takes effect without replumbing.
    fn prepare_playlist_context(&mut self, cursor: Option<(Playlist, u32)>) {
        abort_task(&mut self.playlist_prefetch);
        abort_task(&mut self.autoplay.task);
        // Commit only after the new track opens. A failed play must leave the
        // previous playlist's continuation attached to its existing queue.
        self.pending_playlist_cursor = Some(cursor);
    }

    fn cancel_playlist_continuation(&mut self) {
        abort_task(&mut self.playlist_prefetch);
        self.playlist_cursor = None;
        self.pending_playlist_cursor = None;
        abort_task(&mut self.autoplay.task);
    }

    /// Keep only a small lookahead in memory. This belongs to the process so
    /// closing/navigating away from the playlist never cuts playback short.
    fn prefetch_playlist(&mut self) {
        if self.playlist_prefetch.is_some() {
            return;
        }
        let Some((playlist, offset)) = self.playlist_cursor.as_ref() else {
            return;
        };
        let spotify = self.spotify.clone();
        let source_id = playlist.source_id.clone();
        let offset = *offset;
        self.playlist_prefetch = Some(tokio::spawn(async move {
            run_with_timeout(
                CATALOG_TIMEOUT_SECONDS,
                "Spotify playlist playback page",
                spotify.playlist_tracks_page(&source_id, offset),
            )
            .await
        }));
    }

    async fn finish_playlist_page(
        &mut self,
        result: Finished<Result<crate::model::PlaylistTrackPage>>,
    ) {
        self.playlist_prefetch = None;
        let page = match result {
            Some(Ok(Ok(page))) => page,
            Some(Ok(Err(error))) => {
                send_error(&self.events, error).await;
                return;
            }
            _ => return,
        };
        let old_cursor = self.playlist_cursor.clone();
        let Some((playlist, _)) = self.playlist_cursor.take() else {
            return;
        };
        self.playlist_cursor = page.next_offset.map(|offset| (playlist, offset));
        let Some(index) = self.queue.index else {
            return;
        };
        let old_len = self.queue.tracks.len();
        let ended = self.queue.ended;
        let mut tracks = self.queue.tracks.clone();
        tracks.extend(page.tracks);
        if let Err(error) = self.replace_queue(tracks, index).await {
            self.playlist_cursor = old_cursor;
            send_error(&self.events, error).await;
            return;
        }
        if ended && self.queue.tracks.len() > old_len {
            if let Err(error) = self.next_track().await {
                send_error(&self.events, error).await;
            }
        } else {
            self.maybe_prefetch_autoplay();
        }
    }

    fn maybe_prefetch_autoplay(&mut self) {
        if self.pending_playlist_cursor.is_some() {
            return;
        }
        if self.playlist_cursor.is_some() {
            if self
                .queue
                .index
                .is_some_and(|index| self.queue.tracks.len().saturating_sub(index + 1) <= 5)
            {
                self.prefetch_playlist();
            }
            return;
        }
        let Some(index) = self.queue.index else {
            return;
        };
        if index + 1 < self.queue.tracks.len() {
            return;
        }
        let Some(seed) = self.queue.tracks.get(index).cloned() else {
            return;
        };
        if self.autoplay.fruitless_seed.as_deref() == Some(seed.source_id.as_str()) {
            return;
        }
        if self.autoplay.task.is_some()
            && self.autoplay.seed_id.as_deref() == Some(seed.source_id.as_str())
        {
            return;
        }
        // Any pending fetch at this point is seeded on music the listener
        // has moved away from; its result must never touch this queue.
        abort_task(&mut self.autoplay.task);
        self.autoplay.seed_id = Some(seed.source_id.clone());
        let player = self.connection.player.clone();
        let spotify = self.spotify.clone();
        let store = self.store.clone();
        self.autoplay.task = Some(tokio::spawn(async move {
            if !store.call(|store| store.preferences()).await?.autoplay {
                return Ok(None);
            }
            run_with_timeout(60, "Spotify autoplay radio", async {
                let player = player.context("Spotify playback is not connected")?;
                let seed_uri = seed
                    .spotify_uri
                    .as_deref()
                    .context("autoplay seed has no Spotify track URI")?;
                let uris = player.radio_track_uris(seed_uri).await?;
                spotify.resolve_playable_track_uris(&uris).await.map(Some)
            })
            .await
        }));
    }

    /// Extends the queue with a finished autoplay prefetch, and picks the
    /// music back up when the song outran the fetch.
    async fn finish_autoplay(&mut self, extended: Finished<Result<Option<Vec<Track>>>>) {
        self.autoplay.task = None;
        let seed_id = self.autoplay.seed_id.take();
        let tracks = match extended {
            Some(Ok(Ok(Some(tracks)))) => tracks,
            // The preference was off when the task looked: not a dry seed.
            Some(Ok(Ok(None))) => return,
            Some(Ok(Err(error))) => {
                log::warn!("autoplay: radio prefetch failed: {error:#}");
                self.autoplay.fruitless_seed = seed_id;
                return;
            }
            Some(Err(error)) => {
                send_error(&self.events, error).await;
                return;
            }
            None => return,
        };
        // The queue may have moved on while the prefetch ran; a result for
        // any other seed than the still-playing last track is stale.
        let Some(index) = self.queue.index else {
            return;
        };
        if index + 1 < self.queue.tracks.len() {
            return;
        }
        let current_seed = self
            .queue
            .tracks
            .get(index)
            .map(|track| track.source_id.as_str());
        if seed_id.as_deref() != current_seed {
            return;
        }
        // The listener may have switched autoplay off while this ran.
        match self.store.call(|store| store.preferences()).await {
            Ok(preferences) if !preferences.autoplay => return,
            Ok(_) => {}
            Err(error) => {
                send_error(&self.events, error).await;
                return;
            }
        }
        let additions: Vec<Track> = {
            let mut known: HashSet<String> = self
                .queue
                .tracks
                .iter()
                .map(|track| track.source_id.clone())
                .collect();
            tracks
                .into_iter()
                .filter(|track| known.insert(track.source_id.clone()))
                .collect()
        };
        if additions.is_empty() {
            // Radio had nothing new for this seed; autoplay rests here the
            // way Spotify's does when its well runs dry.
            self.autoplay.fruitless_seed = seed_id;
            return;
        }
        let mut updated = self.queue.tracks.clone();
        updated.extend(additions);
        let ended = self.queue.ended;
        if let Err(error) = self.replace_queue(updated, index).await {
            send_error(&self.events, error).await;
            return;
        }
        if ended {
            // The song outran the fetch: continue into the extension rather
            // than leaving playback parked on the ended state.
            if let Err(error) = self.next_track().await {
                send_error(&self.events, error).await;
            }
        }
    }

    fn connected_player(&self) -> Result<&Playback> {
        self.connection
            .player
            .as_ref()
            .context("Spotify playback is not connected")
    }

    fn configuration_is_from_environment(&self) -> bool {
        self.configuration
            .as_ref()
            .is_some_and(|configuration| configuration.source == ClientIdSource::Environment)
    }

    async fn send_configuration_failed(&self, generation: u64, error: impl std::fmt::Display) {
        let _ = self
            .events
            .send(BackendEvent::SpotifyConfigurationFailed {
                generation,
                error: error.to_string(),
            })
            .await;
    }
}

/// The queue as last handed to the player and saved to disk.
#[derive(Default)]
struct PlayQueue {
    tracks: Vec<Track>,
    index: Option<usize>,
    position_ms: u32,
    /// The last track finished with nothing after it. librespot sits in
    /// EndOfTrack, where play() and seek() are no-ops; only a fresh load
    /// leaves it, so Resume and Seek take different paths while this is set.
    ended: bool,
}

impl PlayQueue {
    fn prune(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        self.tracks.drain(..count);
        self.index = self.index.map(|index| index - count);
        // Vec::drain retains capacity; release a formerly huge played playlist.
        if self.tracks.capacity() > self.tracks.len().saturating_mul(2) {
            self.tracks.shrink_to_fit();
        }
    }

    fn from_snapshot(snapshot: Option<PlaybackSnapshot>) -> Self {
        match snapshot {
            Some(snapshot) => {
                let mut queue = Self {
                    tracks: snapshot.tracks,
                    index: Some(snapshot.index),
                    position_ms: snapshot.position_ms,
                    ended: false,
                };
                queue.prune(snapshot.index.saturating_sub(PLAYED_TRACK_WINDOW));
                queue
            }
            None => Self::default(),
        }
    }
}

/// The sign-in and sign-out flows, at most one of each in flight.
#[derive(Default)]
struct SessionTasks {
    authorization: Option<tokio::task::JoinHandle<(u64, Result<AuthorizationSuccess>)>>,
    logout: Option<tokio::task::JoinHandle<Result<()>>>,
}

impl SessionTasks {
    fn begin_authorization(
        &mut self,
        generation: u64,
        spotify: Spotify,
        needs_playback: bool,
        playback_credentials_invalidated: bool,
    ) {
        abort_task(&mut self.authorization);
        self.authorization = Some(tokio::spawn(async move {
            let result =
                authorize_account(spotify, needs_playback, playback_credentials_invalidated).await;
            (generation, result)
        }));
    }

    fn begin_logout(&mut self, store: BlockingStore, spotify: Spotify) {
        abort_task(&mut self.logout);
        self.logout = Some(tokio::spawn(logout_account(store, spotify)));
    }

    async fn finish_pending_logout(&mut self) {
        if let Some(task) = self.logout.take() {
            let _ = task.await;
        }
    }
}

/// Favorites saved before tracks carried full catalog references get their
/// missing artist and album ids re-resolved once per sign-in.
#[derive(Default)]
struct FavoriteRefresh {
    task: Option<tokio::task::JoinHandle<Result<Vec<Track>>>>,
}

impl FavoriteRefresh {
    async fn start(&mut self, store: &BlockingStore, spotify: Spotify, events: &EventSender) {
        self.abort();
        let favorites = match store.favorites().await {
            Ok(favorites) => favorites,
            Err(error) => {
                send_error(events, error).await;
                return;
            }
        };
        let uris = favorites
            .into_iter()
            .filter(favorite_needs_catalog_refresh)
            .map(|track| {
                track
                    .spotify_uri
                    .unwrap_or_else(|| format!("spotify:track:{}", track.source_id))
            })
            .collect::<Vec<_>>();
        if uris.is_empty() {
            return;
        }
        self.task = Some(tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(60), spotify.resolve_track_uris(&uris))
                .await
                .context("Spotify favorite refresh timed out")?
        }));
    }

    fn abort(&mut self) {
        abort_task(&mut self.task);
    }
}

/// The in-flight track-radio request. Starting another cancels the previous.
#[derive(Default)]
struct Radio {
    task: Option<RadioTask>,
    request_id: Option<u64>,
}

/// Prefetches a radio continuation while the queue's last track plays, so
/// autoplay can extend the queue before it runs out.
#[derive(Default)]
struct Autoplay {
    /// Resolves to None when the preference is off, so an opt-out fetch is
    /// never mistaken for a dry radio.
    task: Option<tokio::task::JoinHandle<Result<Option<Vec<Track>>>>>,
    /// The source id the in-flight (or last) fetch was seeded with.
    seed_id: Option<String>,
    /// A seed whose fetch came back dry or failed; skipped until the queue
    /// moves to a different last track, so pause/play cannot spam radio.
    fruitless_seed: Option<String>,
}

impl Radio {
    async fn start(
        &mut self,
        request_id: u64,
        seed: Track,
        player: Option<Playback>,
        spotify: Spotify,
        events: &EventSender,
    ) {
        self.cancel(events).await;
        self.request_id = Some(request_id);
        self.task = Some(tokio::spawn(async move {
            let result = tokio::time::timeout(Duration::from_secs(20), async {
                let player = player.context("Spotify playback is not connected")?;
                let seed_uri = seed
                    .spotify_uri
                    .as_deref()
                    .context("radio seed has no Spotify track URI")?;
                let uris = player.radio_track_uris(seed_uri).await?;
                let recommendations = spotify.resolve_playable_track_uris(&uris).await?;
                build_radio_context(seed, recommendations)
            })
            .await
            .unwrap_or_else(|_| Err(anyhow!("Spotify track radio timed out")));
            (request_id, result)
        }));
    }

    async fn cancel(&mut self, events: &EventSender) {
        abort_task(&mut self.task);
        if let Some(request_id) = self.request_id.take() {
            let _ = events
                .send(BackendEvent::RadioCancelled { request_id })
                .await;
        }
    }
}
async fn wait_for_shutdown(shutdown: &mut tokio::sync::watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }
    let _ = shutdown.wait_for(|shutdown| *shutdown).await;
}

/// The in-flight catalog work. Each kind keeps only its newest request:
/// starting another aborts the previous.
#[derive(Default)]
struct CatalogFetches {
    library_write: Option<tokio::task::JoinHandle<()>>,
    library: Option<tokio::task::JoinHandle<()>>,
    reload: Option<tokio::task::JoinHandle<()>>,
    search: Option<tokio::task::JoinHandle<()>>,
    playlist: Option<tokio::task::JoinHandle<()>>,
    artist: Option<tokio::task::JoinHandle<()>>,
    album: Option<tokio::task::JoinHandle<()>>,
    /// Seeded by the first full reload; lets later reloads answer Unchanged
    /// from the head probes alone.
    library_fingerprint: SharedFingerprint,
}

impl CatalogFetches {
    /// Sets what the next probe compares against: the persisted fingerprint
    /// when the cache is being served, `None` when a load must deliver contents.
    fn seed_fingerprint(&mut self, fingerprint: Option<LibraryFingerprint>) {
        set_fingerprint(&self.library_fingerprint, fingerprint);
    }

    fn load_library(
        &mut self,
        spotify: Spotify,
        store: BlockingStore,
        generation: u64,
        current_generation: Arc<AtomicU64>,
        events: EventSender,
    ) {
        abort_task(&mut self.library);
        self.library = Some(spawn_library_load(
            spotify,
            store,
            generation,
            current_generation,
            events,
            self.library_fingerprint.clone(),
        ));
    }

    fn search(
        &mut self,
        spotify: Spotify,
        query: String,
        respond: Reply<crate::model::CatalogSearchResults>,
    ) {
        Self::start(&mut self.search, respond, "Spotify search", async move {
            spotify.search_catalog(&query).await
        });
    }

    fn playlist(
        &mut self,
        spotify: Spotify,
        playlist: Playlist,
        offset: u32,
        respond: Reply<crate::model::PlaylistTrackPage>,
    ) {
        Self::start(
            &mut self.playlist,
            respond,
            "Spotify playlist request",
            async move {
                spotify
                    .playlist_tracks_page(&playlist.source_id, offset)
                    .await
            },
        );
    }

    fn artist(&mut self, spotify: Spotify, source_id: String, respond: Reply<ArtistDetails>) {
        Self::start(
            &mut self.artist,
            respond,
            "Spotify artist request",
            async move { spotify.artist(&source_id).await },
        );
    }

    fn album(&mut self, spotify: Spotify, source_id: String, respond: Reply<AlbumDetails>) {
        Self::start(
            &mut self.album,
            respond,
            "Spotify album request",
            async move { spotify.album(&source_id).await },
        );
    }

    fn start<T: Send + 'static>(
        slot: &mut Option<tokio::task::JoinHandle<()>>,
        mut respond: Reply<T>,
        operation: &'static str,
        request: impl Future<Output = Result<T>> + Send + 'static,
    ) {
        abort_task(slot);
        *slot = Some(tokio::spawn(async move {
            // These are read-only catalog fetches. A page dropping its reply
            // receiver cancels the HTTP future, including a rate-limit wait.
            // Writes use separate workers and must finish even without a view.
            tokio::select! {
                biased;
                _ = respond.closed() => {}
                result = run_with_timeout(CATALOG_TIMEOUT_SECONDS, operation, request) => {
                    let _ = respond.send(result);
                }
            }
        }));
    }

    fn abort_all(&mut self) {
        abort_task(&mut self.library_write);
        abort_task(&mut self.library);
        abort_task(&mut self.reload);
        abort_task(&mut self.search);
        abort_task(&mut self.playlist);
        abort_task(&mut self.artist);
        abort_task(&mut self.album);
        // The next account's library must not compare equal to this one's.
        self.seed_fingerprint(None);
    }
}

/// The live Spotify playback session and the tasks that keep it alive.
#[derive(Default)]
struct PlaybackConnection {
    player: Option<Playback>,
    observer: Option<tokio::task::JoinHandle<()>>,
    reconnect: Option<tokio::task::JoinHandle<Result<Playback>>>,
    connect: Option<tokio::task::JoinHandle<Result<Playback>>>,
    reconnect_pending: bool,
    /// The connect in flight is replacing a dropped session rather than starting
    /// a fresh one, so playback must not be restored on top of it.
    connect_restoring: bool,
    /// A play requested while the session was still connecting, started as
    /// soon as it is up so the first click after launch is not lost.
    pending_play: Option<(Vec<Track>, usize)>,
}

impl PlaybackConnection {
    fn disconnect(&mut self) {
        if let Some(player) = self.player.take() {
            player.stop();
        }
        abort_task(&mut self.observer);
    }

    fn is_connecting(&self) -> bool {
        self.player.is_none() && (self.connect.is_some() || self.reconnect.is_some())
    }

    /// Holds a play until the in-flight connection finishes, replacing any
    /// play held before it.
    fn hold_play(&mut self, tracks: Vec<Track>, index: usize) {
        log::info!("playback: holding play until the session connects");
        self.pending_play = Some((tracks, index));
    }

    /// Reports a held play as failed once the connection it waited on did.
    async fn fail_pending_play(&mut self, error: &dyn std::fmt::Display, events: &EventSender) {
        let Some((tracks, index)) = self.pending_play.take() else {
            return;
        };
        let spotify_uri = tracks
            .get(index)
            .and_then(|track| track.spotify_uri.clone())
            .unwrap_or_default();
        let _ = events
            .send(BackendEvent::TrackFailed {
                spotify_uri,
                error: format!("Spotify playback is not connected: {error}"),
            })
            .await;
        let _ = events.send(BackendEvent::PlaybackSettled).await;
    }

    fn adopt(&mut self, player: Playback, events: &EventSender) {
        self.observer = Some(observe_playback(&player, events));
        self.player = Some(player);
    }

    fn abort_attempts(&mut self) {
        abort_task(&mut self.reconnect);
        abort_task(&mut self.connect);
    }

    /// Replaces the current session with a fresh connection attempt.
    fn begin_connect(&mut self, request: PlaybackConnectionRequest) {
        self.connect_restoring = self.reconnect_pending;
        self.abort_attempts();
        self.reconnect_pending = false;
        self.disconnect();
        self.connect = Some(tokio::spawn(Playback::connect(
            request.load_saved_token,
            request.authorization,
        )));
    }

    /// Starts a reconnect attempt when the session dropped, unless one is
    /// already in flight.
    async fn reconnect_if_dead(&mut self, events: &EventSender) {
        let session_dropped = self.reconnect_pending
            || self
                .player
                .as_ref()
                .is_some_and(|player| !player.is_connected());
        if !session_dropped {
            return;
        }
        self.reconnect_pending = true;
        if self.reconnect.is_none() {
            log::info!("playback: connection lost, reconnecting");
            self.disconnect();
            let _ = events.send(BackendEvent::PlaybackReconnecting).await;
            self.reconnect = Some(tokio::spawn(async {
                tokio::time::timeout(Duration::from_secs(15), Playback::reconnect())
                    .await
                    .context("Spotify playback reconnection timed out")?
            }));
        }
    }

    async fn finish_reconnect(
        &mut self,
        reconnected: Finished<Result<Playback>>,
        events: &EventSender,
    ) {
        self.reconnect = None;
        match reconnected {
            Some(Ok(Ok(player))) => {
                log::info!("playback: reconnected");
                self.adopt(player, events);
                self.reconnect_pending = false;
                let _ = events.send(BackendEvent::PlaybackReconnected).await;
            }
            Some(Ok(Err(error))) => {
                log::warn!("playback: reconnect attempt failed: {error}");
                self.fail_pending_play(&error, events).await;
                let _ = events
                    .send(BackendEvent::PlaybackFailed(format!(
                        "Spotify playback disconnected; reconnecting: {error}"
                    )))
                    .await;
            }
            Some(Err(error)) => send_error(events, error).await,
            None => {}
        }
    }
}

/// Never resolves when the slot is empty, so a `select!` arm can wait on a
/// task that may not exist.
async fn finished<T>(
    task: &mut Option<tokio::task::JoinHandle<T>>,
) -> Option<Result<T, tokio::task::JoinError>> {
    match task.as_mut() {
        Some(task) => Some(task.await),
        None => std::future::pending().await,
    }
}

/// Runs `request`, turning a timeout into an error the caller can show.
async fn run_with_timeout<T>(
    seconds: u64,
    operation: &str,
    request: impl Future<Output = Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(Duration::from_secs(seconds), request).await {
        Ok(result) => result,
        // Keep `Elapsed` in the chain so the error classifies as transient.
        Err(elapsed) => Err(anyhow::Error::new(elapsed).context(format!("{operation} timed out"))),
    }
}

fn abort_task<T>(task: &mut Option<tokio::task::JoinHandle<T>>) {
    if let Some(task) = task.take() {
        task.abort();
    }
}

async fn receive_shutdown_acknowledgment(
    commands: &mut Receiver<BackendCommand>,
) -> Option<StdSender<()>> {
    while let Some(command) = commands.recv().await {
        if let BackendCommand::Shutdown { acknowledged } = command {
            return Some(acknowledged);
        }
    }
    None
}

async fn load_library(spotify: &Spotify) -> Result<(Vec<Track>, Vec<Playlist>)> {
    tokio::try_join!(spotify.liked_tracks(), spotify.playlists())
}

/// What a probed load found. `Changed` hands the caller the fingerprint to
/// commit, which must only happen once the contents are safely persisted: a
/// committed fingerprint over unpersisted contents would answer Unchanged
/// over stale data forever after.
enum ProbedLibrary {
    Unchanged,
    Changed {
        contents: (Vec<Track>, Vec<Playlist>),
        fingerprint: LibraryFingerprint,
    },
}

/// Probes the first pages first: when they and the totals match the last
/// reload, the answer is two requests instead of a full paginated walk.
async fn probe_and_load_library(
    spotify: &Spotify,
    fingerprint: &SharedFingerprint,
) -> Result<ProbedLibrary> {
    let (liked, playlists) =
        tokio::try_join!(spotify.liked_tracks_head(), spotify.playlists_head())?;
    let current = LibraryFingerprint::new(&liked, &playlists);
    let unchanged = fingerprint
        .lock()
        .expect("library fingerprint lock")
        .as_ref()
        == Some(&current);
    if unchanged {
        return Ok(ProbedLibrary::Unchanged);
    }
    let contents = load_library(spotify).await?;
    Ok(ProbedLibrary::Changed {
        contents,
        fingerprint: current,
    })
}

fn spawn_library_load(
    spotify: Spotify,
    store: BlockingStore,
    generation: u64,
    current_generation: Arc<AtomicU64>,
    events: EventSender,
    fingerprint: SharedFingerprint,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let profile_load = async {
            let Ok(Ok(profile)) =
                tokio::time::timeout(Duration::from_secs(30), spotify.profile()).await
            else {
                return;
            };
            if current_generation.load(Ordering::Acquire) == generation {
                let _ = events
                    .send(BackendEvent::ProfileLoaded {
                        generation,
                        profile,
                    })
                    .await;
            }
        };
        let library_load = async {
            let library = tokio::time::timeout(
                Duration::from_secs(60),
                probe_and_load_library(&spotify, &fingerprint),
            )
            .await;
            if current_generation.load(Ordering::Acquire) != generation {
                return;
            }
            match library {
                // Only reachable with a persisted fingerprint seeded from the
                // cache that is already on screen.
                Ok(Ok(ProbedLibrary::Unchanged)) => {
                    log::info!("library: cache matches Spotify");
                    let _ = events
                        .send(BackendEvent::LibraryUnchanged { generation })
                        .await;
                }
                Ok(Ok(ProbedLibrary::Changed {
                    contents: (liked_tracks, playlists),
                    fingerprint: probed,
                })) => {
                    match persist_library_cache(
                        &store,
                        liked_tracks,
                        playlists,
                        probed.clone(),
                        current_generation.clone(),
                        generation,
                    )
                    .await
                    {
                        Ok(Some((liked_tracks, playlists))) => {
                            log::info!(
                                "library: loaded {} liked tracks and {} playlists from Spotify",
                                liked_tracks.len(),
                                playlists.len()
                            );
                            commit_fingerprint(&fingerprint, probed);
                            let _ = events
                                .send(BackendEvent::LibraryLoaded {
                                    generation,
                                    liked_tracks,
                                    playlists,
                                })
                                .await;
                            let _ = events.send(BackendEvent::CatalogReady { generation }).await;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let _ = events
                                .send(BackendEvent::CatalogFailed {
                                    generation,
                                    error: error.to_string(),
                                })
                                .await;
                        }
                    }
                }
                Ok(Err(error)) => {
                    let _ = events
                        .send(BackendEvent::CatalogFailed {
                            generation,
                            error: error.to_string(),
                        })
                        .await;
                }
                Err(_) => {
                    let _ = events
                        .send(BackendEvent::CatalogFailed {
                            generation,
                            error: "Spotify library request timed out".to_owned(),
                        })
                        .await;
                }
            }
        };
        tokio::join!(profile_load, library_load);
    })
}

async fn persist_library_cache(
    store: &BlockingStore,
    liked_tracks: Vec<Track>,
    playlists: Vec<Playlist>,
    fingerprint: LibraryFingerprint,
    current_generation: Arc<AtomicU64>,
    generation: u64,
) -> Result<Option<(Vec<Track>, Vec<Playlist>)>> {
    store
        .replace_library_cache_if_current(
            liked_tracks,
            playlists,
            fingerprint,
            current_generation,
            generation,
        )
        .await
}

async fn authorize_account(
    mut spotify: Spotify,
    needs_playback: bool,
    playback_credentials_invalidated: bool,
) -> Result<AuthorizationSuccess> {
    let playback_authorization = if needs_playback && playback_credentials_invalidated {
        Some(Playback::prepare_authorization().await?)
    } else {
        None
    };
    let playback_authorization_url = playback_authorization
        .as_ref()
        .map(|authorization| authorization.url().to_owned());
    spotify
        .authorize(playback_authorization_url.as_deref())
        .await?;
    let playback = if needs_playback {
        Some(PlaybackConnectionRequest {
            load_saved_token: !playback_credentials_invalidated,
            authorization: playback_authorization,
        })
    } else {
        None
    };
    Ok(AuthorizationSuccess { playback })
}

async fn logout_account(store: BlockingStore, spotify: Spotify) -> Result<()> {
    let mut errors = Vec::new();
    for result in [
        store.set_oauth_credentials_invalidated(true).await,
        store.set_playback_credentials_invalidated(true).await,
        spotify.logout().await,
        delete_playback_refresh_token().await,
        store.clear_library_cache().await,
        store.clear_playback_state().await,
    ] {
        if let Err(error) = result {
            errors.push(error.to_string());
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("logout cleanup failed: {}", errors.join("; ")))
    }
}

fn observe_playback(player: &Playback, events: &EventSender) -> tokio::task::JoinHandle<()> {
    let mut player_events = player.events();
    let event_sender = events.clone();
    tokio::spawn(async move {
        while let Some(event) = player_events.recv().await {
            let event = match event {
                PlayerEvent::Loading { track_id, .. } => Some(BackendEvent::Loading {
                    spotify_uri: track_id.to_string(),
                }),
                PlayerEvent::Playing {
                    track_id,
                    position_ms,
                    ..
                } => {
                    let _ = event_sender
                        .send_ordered(BackendEvent::PositionChanged {
                            spotify_uri: track_id.to_string(),
                            position_ms,
                        })
                        .await;
                    Some(BackendEvent::Playing {
                        spotify_uri: track_id.to_string(),
                    })
                }
                PlayerEvent::Paused {
                    track_id,
                    position_ms,
                    ..
                } => {
                    let _ = event_sender
                        .send_ordered(BackendEvent::PositionChanged {
                            spotify_uri: track_id.to_string(),
                            position_ms,
                        })
                        .await;
                    Some(BackendEvent::Paused {
                        spotify_uri: track_id.to_string(),
                    })
                }
                PlayerEvent::EndOfTrack { track_id, .. } => Some(BackendEvent::EndOfTrack {
                    spotify_uri: track_id.to_string(),
                }),
                PlayerEvent::Unavailable { track_id, .. } => Some(BackendEvent::TrackFailed {
                    spotify_uri: track_id.to_string(),
                    error: "Spotify cannot play this track".to_owned(),
                }),
                PlayerEvent::PositionChanged {
                    track_id,
                    position_ms,
                    ..
                }
                | PlayerEvent::PositionCorrection {
                    track_id,
                    position_ms,
                    ..
                }
                | PlayerEvent::Seeked {
                    track_id,
                    position_ms,
                    ..
                } => Some(BackendEvent::PositionChanged {
                    spotify_uri: track_id.to_string(),
                    position_ms,
                }),
                _ => None,
            };
            if let Some(event) = event {
                let _ = event_sender.send(event).await;
            }
        }
    })
}

async fn load_context_track(
    playback: &Option<Playback>,
    tracks: &[Track],
    index: usize,
    events: &EventSender,
) -> Result<()> {
    let track = tracks
        .get(index)
        .context("playback track index is out of bounds")?;
    let spotify_uri = track
        .spotify_uri
        .as_deref()
        .context("track has no Spotify playback URI")?;
    let spotify_uri = SpotifyUri::from_uri(spotify_uri).context("invalid Spotify track URI")?;
    let player = playback
        .as_ref()
        .context("Spotify playback is not connected")?;
    send_playback_context(tracks, index, events).await;
    player.load(spotify_uri, true, 0);
    Ok(())
}

async fn restore_context_track(
    playback: &Option<Playback>,
    tracks: &[Track],
    index: Option<usize>,
    position_ms: u32,
    playing: bool,
    events: &EventSender,
) -> Result<()> {
    let index = index.context("playback context is not available")?;
    let track = tracks
        .get(index)
        .context("playback track index is out of bounds")?;
    let spotify_uri = track
        .spotify_uri
        .as_deref()
        .context("track has no Spotify playback URI")?;
    let spotify_uri = SpotifyUri::from_uri(spotify_uri).context("invalid Spotify track URI")?;
    let player = playback
        .as_ref()
        .context("Spotify playback is not connected")?;
    send_playback_context(tracks, index, events).await;
    player.load(spotify_uri, playing, position_ms);
    Ok(())
}

async fn restore_saved_playback(
    playback: &Option<Playback>,
    tracks: &[Track],
    index: Option<usize>,
    position_ms: u32,
    events: &EventSender,
) -> Result<()> {
    if index.is_none() {
        return Ok(());
    }
    restore_context_track(playback, tracks, index, position_ms, false, events).await
}

async fn send_playback_context(tracks: &[Track], index: usize, events: &EventSender) {
    if let Some(current) = tracks.get(index) {
        let _ = events
            .send(BackendEvent::PlaybackContext {
                current: current.clone(),
                next: tracks.get(index + 1..).unwrap_or_default().to_vec(),
            })
            .await;
    }
}

async fn send_error(events: &EventSender, error: impl std::fmt::Display) {
    let _ = events.send(BackendEvent::Error(error.to_string())).await;
}

async fn send_fatal_error(events: &EventSender, error: impl std::fmt::Display) {
    let _ = events
        .send(BackendEvent::FatalError(error.to_string()))
        .await;
}

fn favorite_needs_catalog_refresh(track: &Track) -> bool {
    track.provider == crate::model::Provider::Spotify
        && (!track
            .artists
            .iter()
            .any(|artist| artist.source_id.is_some())
            || track
                .album_ref
                .as_ref()
                .and_then(|album| album.source_id.as_ref())
                .is_none())
}

fn build_radio_context(seed: Track, recommendations: Vec<Track>) -> Result<Vec<Track>> {
    let mut seen = HashSet::from([seed.source_id.clone()]);
    let recommendations = recommendations
        .into_iter()
        .filter(|track| {
            track.spotify_uri.as_deref() != seed.spotify_uri.as_deref()
                && track.is_displayable()
                && seen.insert(track.source_id.clone())
        })
        .collect::<Vec<_>>();
    if recommendations.is_empty() {
        return Err(anyhow!("Spotify track radio returned no playable tracks"));
    }
    let mut tracks = Vec::with_capacity(recommendations.len() + 1);
    tracks.push(seed);
    tracks.extend(recommendations);
    Ok(tracks)
}

async fn send_local_state(store: &BlockingStore, events: &EventSender) {
    match store.local_state().await {
        Ok((favorites, pinned_playlists, recently_played)) => {
            let _ = events
                .send(BackendEvent::LocalStateLoaded {
                    favorites,
                    pinned_playlists,
                    recently_played,
                })
                .await;
        }
        Err(error) => send_error(events, error).await,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BackendCommand, BackendEvent, BlockingStore, PlaybackConnection, build_radio_context,
        favorite_needs_catalog_refresh, send_command,
    };
    use crate::model::{AlbumRef, ArtistRef, Provider, Track};
    use crate::storage::Store;

    #[tokio::test]
    async fn abandoning_catalog_reply_cancels_the_request_future() {
        let (reply, receiver) = tokio::sync::oneshot::channel::<anyhow::Result<()>>();
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let (dropped, dropped_rx) = tokio::sync::oneshot::channel();
        struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for DropSignal {
            fn drop(&mut self) {
                let _ = self.0.take().unwrap().send(());
            }
        }
        let mut slot = None;
        super::CatalogFetches::start(&mut slot, reply, "test", async move {
            let _guard = DropSignal(Some(dropped));
            started.send(()).unwrap();
            std::future::pending::<anyhow::Result<()>>().await
        });
        started_rx.await.unwrap();
        drop(receiver);
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .unwrap()
            .unwrap();
        slot.take().unwrap().await.unwrap();
    }

    fn favorite() -> Track {
        Track {
            provider: Provider::Spotify,
            source_id: "track-id".to_owned(),
            spotify_uri: Some("spotify:track:track-id".to_owned()),
            isrc: None,
            title: "Track".into(),
            artist: "Artist".into(),
            artists: Vec::new(),
            album: "Album".into(),
            album_ref: None,
            duration_ms: 180_000,
            artwork_url: None,
        }
    }

    #[tokio::test]
    async fn event_bridge_coalesces_ticks_and_preserves_pause_position() {
        let (_shutdown, shutdown_receiver) = tokio::sync::watch::channel(false);
        let (sender, mut events) = super::event_channel(shutdown_receiver);
        for position_ms in 0..100_000 {
            sender
                .send(BackendEvent::PositionChanged {
                    spotify_uri: "track".into(),
                    position_ms,
                })
                .await
                .unwrap();
        }
        assert_eq!(sender.reliable.capacity(), super::EVENT_CAPACITY);
        assert!(matches!(
            events.recv().await,
            Some(BackendEvent::PositionChanged {
                position_ms: 99_999,
                ..
            })
        ));
        assert!(events.try_recv().is_err());
        sender
            .send(BackendEvent::PositionChanged {
                spotify_uri: "track".into(),
                position_ms: 100_000,
            })
            .await
            .unwrap();
        sender
            .send_ordered(BackendEvent::PositionChanged {
                spotify_uri: "track".into(),
                position_ms: 100_001,
            })
            .await
            .unwrap();
        sender
            .send(BackendEvent::Paused {
                spotify_uri: "track".into(),
            })
            .await
            .unwrap();
        assert!(matches!(
            events.recv().await,
            Some(BackendEvent::PositionChanged {
                position_ms: 100_001,
                ..
            })
        ));
        assert!(matches!(
            events.recv().await,
            Some(BackendEvent::Paused { .. })
        ));
        assert!(events.try_recv().is_err());
        // Even after its sender closes, the final tick must remain available.
        sender
            .send(BackendEvent::PositionChanged {
                spotify_uri: "track".into(),
                position_ms: 100_002,
            })
            .await
            .unwrap();
        drop(sender);
        assert!(matches!(
            events.recv().await,
            Some(BackendEvent::PositionChanged {
                position_ms: 100_002,
                ..
            })
        ));
        assert!(events.recv().await.is_none());
    }

    #[tokio::test]
    async fn full_event_bridge_waits_without_dropping_events_and_shutdown_cancels_wait() {
        let (shutdown, shutdown_receiver) = tokio::sync::watch::channel(false);
        let (sender, mut events) = super::event_channel(shutdown_receiver);
        for _ in 0..super::EVENT_CAPACITY {
            sender.send(BackendEvent::SetupRequired).await.unwrap();
        }
        let send = sender.send(BackendEvent::QueueEnded);
        tokio::pin!(send);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(5), &mut send)
                .await
                .is_err()
        );
        assert!(matches!(
            events.recv().await,
            Some(BackendEvent::SetupRequired)
        ));
        send.await.unwrap();
        for _ in 1..super::EVENT_CAPACITY {
            assert!(matches!(
                events.recv().await,
                Some(BackendEvent::SetupRequired)
            ));
        }
        assert!(matches!(
            events.recv().await,
            Some(BackendEvent::QueueEnded)
        ));
        for _ in 0..super::EVENT_CAPACITY {
            sender.send(BackendEvent::SetupRequired).await.unwrap();
        }
        let send = sender.send(BackendEvent::QueueEnded);
        tokio::pin!(send);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(5), &mut send)
                .await
                .is_err()
        );
        shutdown.send(true).unwrap();
        assert!(send.await.is_err());
    }

    #[test]
    fn autoplay_keeps_only_fifty_played_tracks_over_a_long_session() {
        let mut queue = super::PlayQueue::default();
        for id in 0..10_000 {
            let mut track = favorite();
            track.source_id = id.to_string();
            queue.tracks.push(track);
            let index = queue.tracks.len() - 1;
            queue.index = Some(index);
            queue.prune(index.saturating_sub(super::PLAYED_TRACK_WINDOW));
            assert!(queue.tracks.len() <= 51); // 50 previous entries + current.
        }
        assert_eq!(queue.index, Some(50));
        assert_eq!(queue.tracks.first().unwrap().source_id, "9949");
        assert_eq!(queue.tracks.last().unwrap().source_id, "9999");
    }

    #[test]
    fn restoring_a_large_old_queue_trims_only_played_tracks() {
        let tracks: Vec<_> = (0..500)
            .map(|id| {
                let mut track = favorite();
                track.source_id = id.to_string();
                track
            })
            .collect();
        let queue = super::PlayQueue::from_snapshot(Some(crate::storage::PlaybackSnapshot {
            tracks: tracks.clone(),
            index: 400,
            position_ms: 12_345,
        }));
        assert_eq!(queue.index, Some(50));
        assert_eq!(queue.position_ms, 12_345);
        assert_eq!(queue.tracks, tracks[350..]);
        assert_eq!(queue.tracks[50].source_id, "400");
        let store = Store::in_memory().unwrap();
        store
            .set_playback_state(&queue.tracks, 50, queue.position_ms)
            .unwrap();
        let restored = super::PlayQueue::from_snapshot(store.playback_state().unwrap());
        assert_eq!(restored.tracks, queue.tracks);
        assert_eq!(restored.index, queue.index);
    }

    #[test]
    fn command_ingress_is_bounded_and_volume_is_coalesced() {
        let (commands, mut command_receiver) = tokio::sync::mpsc::channel(1);
        let (controls, mut control_receiver) = tokio::sync::mpsc::channel(1);
        let (volume, mut volume_receiver) = tokio::sync::watch::channel(0.5);

        assert!(send_command(
            &commands,
            &controls,
            &volume,
            BackendCommand::SetVolume(0.2)
        ));
        assert!(send_command(
            &commands,
            &controls,
            &volume,
            BackendCommand::SetVolume(0.8)
        ));
        assert_eq!(*volume_receiver.borrow_and_update(), 0.8);
        assert!(command_receiver.try_recv().is_err());

        assert!(send_command(
            &commands,
            &controls,
            &volume,
            BackendCommand::Pause
        ));
        assert!(!send_command(
            &commands,
            &controls,
            &volume,
            BackendCommand::Resume
        ));
        assert!(matches!(
            command_receiver.try_recv(),
            Ok(BackendCommand::Pause)
        ));

        assert!(send_command(
            &commands,
            &controls,
            &volume,
            BackendCommand::Logout { generation: 1 }
        ));
        assert!(matches!(
            control_receiver.try_recv(),
            Ok(BackendCommand::Logout { generation: 1 })
        ));
    }

    #[tokio::test]
    async fn adding_to_queue_inserts_next_without_replacing_playlist_cursor() {
        let (_, shutdown) = tokio::sync::watch::channel(false);
        let (events, _receiver) = super::event_channel(shutdown);
        let store = BlockingStore::from_store(Store::in_memory().unwrap());
        let playlist = crate::model::Playlist {
            provider: Provider::Spotify,
            source_id: "playlist".into(),
            name: "Playlist".into(),
            owner: "Owner".into(),
            track_count: 100,
            artwork_url: None,
        };
        let mut worker = super::Worker::new(
            super::Startup {
                store: store.clone(),
                spotify: crate::spotify::Spotify::from_client_id(
                    "00000000000000000000000000000000",
                    false,
                )
                .await
                .unwrap(),
                configuration: None,
                playback_snapshot: None,
                playback_credentials_invalidated: false,
                playlist_cursor: Some((playlist.clone(), 15)),
            },
            events,
        );
        let mut first = favorite();
        first.source_id = "current".into();
        let mut upcoming = favorite();
        upcoming.source_id = "upcoming".into();
        let mut added = favorite();
        added.source_id = "added".into();
        worker.queue.tracks = vec![first.clone(), upcoming.clone()];
        worker.queue.index = Some(0);
        worker.queue.position_ms = 4321;
        worker.shuffle = true;
        worker.append_to_queue(added.clone()).await.unwrap();
        assert_eq!(worker.queue.tracks, vec![first, added, upcoming]);
        assert_eq!(worker.priority_next, 1);
        let before_failure = worker.queue.tracks.clone();
        assert!(worker.next_track().await.is_err()); // no audio connection
        assert_eq!(worker.queue.tracks, before_failure);
        assert_eq!(worker.priority_next, 1);
        worker.priority_next = 0;
        for _ in 0..20 {
            assert!(worker.next_track().await.is_err());
            assert_eq!(worker.queue.tracks, before_failure); // random rotation undone
        }
        assert_eq!(worker.queue.index, Some(0));
        assert_eq!(worker.queue.position_ms, 4321);
        assert_eq!(worker.playlist_cursor, Some((playlist.clone(), 15)));
        assert_eq!(
            store.playback_state().await.unwrap().unwrap().tracks,
            worker.queue.tracks
        );
        assert_eq!(
            store
                .call(|store| store.playback_playlist_cursor())
                .await
                .unwrap(),
            Some((playlist, 15))
        );
    }

    #[tokio::test]
    async fn playlist_continuation_preserves_order_duplicates_cursor_and_cancels_pending_work() {
        let (_, shutdown) = tokio::sync::watch::channel(false);
        let (events, _receiver) = super::event_channel(shutdown);
        let store = BlockingStore::from_store(Store::in_memory().unwrap());
        let playlist = crate::model::Playlist {
            provider: Provider::Spotify,
            source_id: "playlist".into(),
            name: "Playlist".into(),
            owner: "Owner".into(),
            track_count: 100,
            artwork_url: None,
        };
        let mut worker = super::Worker::new(
            super::Startup {
                store: store.clone(),
                spotify: crate::spotify::Spotify::from_client_id(
                    "00000000000000000000000000000000",
                    false,
                )
                .await
                .unwrap(),
                configuration: None,
                playback_snapshot: None,
                playback_credentials_invalidated: false,
                playlist_cursor: Some((playlist.clone(), 15)),
            },
            events,
        );
        worker.queue.tracks = vec![favorite(); 15];
        worker.queue.index = Some(0);
        worker
            .finish_playlist_page(Some(Ok(Err(anyhow::anyhow!("offline")))))
            .await;
        assert_eq!(worker.playlist_cursor, Some((playlist.clone(), 15)));
        assert_eq!(worker.queue.tracks.len(), 15);
        assert!(worker.playlist_prefetch.is_none());
        worker.prepare_playlist_context(None);
        worker.play_context(vec![favorite()], 0).await.unwrap(); // no connected player
        assert_eq!(worker.playlist_cursor, Some((playlist.clone(), 15)));
        assert!(worker.pending_playlist_cursor.is_none());
        worker
            .finish_playlist_page(Some(Ok(Ok(crate::model::PlaylistTrackPage {
                source_offsets: vec![],
                tracks: vec![favorite(); 15],
                next_offset: Some(30),
                total: 100,
            }))))
            .await;
        assert_eq!(worker.queue.tracks.len(), 30); // repeated songs are legitimate playlist entries
        assert_eq!(worker.playlist_cursor, Some((playlist.clone(), 30)));
        assert_eq!(
            store
                .call(|store| store.playback_playlist_cursor())
                .await
                .unwrap(),
            Some((playlist, 30))
        );
        assert_eq!(
            store.playback_state().await.unwrap().unwrap().tracks.len(),
            30
        );
        let task = tokio::spawn(std::future::pending());
        let handle = task.abort_handle();
        worker.playlist_prefetch = Some(task);
        worker.cancel_playlist_continuation();
        tokio::task::yield_now().await;
        assert!(handle.is_finished());
        assert!(worker.playlist_cursor.is_none());
    }

    #[tokio::test]
    async fn blocking_store_runs_operations_off_the_async_thread() {
        let caller = std::thread::current().id();
        let store = BlockingStore::from_store(Store::in_memory().unwrap());

        let worker = store
            .call(|_| Ok(std::thread::current().id()))
            .await
            .unwrap();

        assert_ne!(worker, caller);
    }

    #[tokio::test]
    async fn cancelling_a_store_call_does_not_lose_or_reorder_the_store() {
        let store = BlockingStore::from_store(Store::in_memory().unwrap());
        let first_store = store.clone();
        let first = tokio::spawn(async move {
            first_store
                .call(|store| {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    store.set_spotify_client_id("first")
                })
                .await
        });
        tokio::task::yield_now().await;
        first.abort();

        store
            .call(|store| store.set_spotify_client_id("second"))
            .await
            .unwrap();
        let client_id = store.call(|store| store.spotify_client_id()).await.unwrap();

        assert_eq!(client_id.as_deref(), Some("second"));
    }

    #[test]
    fn only_incomplete_spotify_favorites_need_catalog_refresh() {
        let legacy = favorite();
        assert!(favorite_needs_catalog_refresh(&legacy));

        let mut complete = legacy.clone();
        complete.artists.push(ArtistRef {
            name: "Artist".into(),
            source_id: Some("artist-id".to_owned()),
            spotify_uri: Some("spotify:artist:artist-id".to_owned()),
        });
        complete.album_ref = Some(AlbumRef {
            name: "Album".into(),
            source_id: Some("album-id".to_owned()),
            spotify_uri: Some("spotify:album:album-id".to_owned()),
            artwork_url: None,
        });
        assert!(!favorite_needs_catalog_refresh(&complete));

        let mut non_spotify = legacy;
        non_spotify.provider = Provider::Tidal;
        assert!(!favorite_needs_catalog_refresh(&non_spotify));
    }

    #[test]
    fn radio_context_starts_with_seed_and_deduplicates_recommendations() {
        let seed = favorite();
        let mut recommendation = favorite();
        recommendation.source_id = "recommendation".to_owned();
        recommendation.spotify_uri = Some("spotify:track:recommendation".to_owned());
        recommendation.title = "Recommendation".into();

        let tracks = build_radio_context(
            seed.clone(),
            vec![seed.clone(), recommendation.clone(), recommendation.clone()],
        )
        .unwrap();

        assert_eq!(tracks, vec![seed, recommendation]);
    }

    #[test]
    fn radio_context_rejects_an_empty_recommendation_set() {
        let seed = favorite();

        assert!(build_radio_context(seed.clone(), vec![seed]).is_err());
    }

    #[tokio::test]
    async fn play_is_held_only_while_the_session_is_connecting() {
        let mut connection = PlaybackConnection::default();
        assert!(!connection.is_connecting());

        connection.connect = Some(tokio::spawn(std::future::pending()));
        assert!(connection.is_connecting());
        connection.hold_play(vec![favorite()], 0);
        assert!(connection.pending_play.is_some());

        let (_shutdown, shutdown_receiver) = tokio::sync::watch::channel(false);
        let (events, mut received) = super::event_channel(shutdown_receiver);
        connection.fail_pending_play(&"no network", &events).await;
        assert!(connection.pending_play.is_none());
        assert!(matches!(
            received.try_recv(),
            Ok(BackendEvent::TrackFailed { spotify_uri, .. }) if spotify_uri == "spotify:track:track-id"
        ));
        assert!(matches!(
            received.try_recv(),
            Ok(BackendEvent::PlaybackSettled)
        ));
        connection.abort_attempts();
    }
}
