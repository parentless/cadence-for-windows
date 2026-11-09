use super::*;
use gpui_kit::TestSupportExt as _;
use std::cell::RefCell;

use page::PageEvent;
use track_list::{PlaylistList, TrackList};

/// Sends `command` and hands back the reply channel to await.
///
/// Dropping the returned future is what cancels the request: the page holds it
/// in a `Task`, so starting a new request drops the previous one and its answer
/// is discarded rather than overwriting fresher state.
fn request<T, C>(
    backend: &BackendHandle,
    command: C,
) -> impl Future<Output = Result<T, SharedString>> + use<T, C>
where
    T: Send + 'static,
    C: FnOnce(Reply<T>) -> BackendCommand,
{
    let (respond, reply) = tokio::sync::oneshot::channel();
    let sent = backend.send(command(respond));
    async move {
        if !sent {
            return Err(SharedString::new_static(BACKEND_UNAVAILABLE));
        }
        match reply.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(format!("{error:#}").into()),
            Err(_) => Err(SharedString::new_static(
                "Cadence backend stopped before answering",
            )),
        }
    }
}

/// Search results for the current query.
pub(super) struct SearchPage {
    backend: BackendHandle,
    query: String,
    /// The query the current results answer, keying the result lists so a
    /// new search starts back at the top.
    results_query: String,
    kind: SearchKind,
    tracks: Arc<[model::Track]>,
    playlists: Arc<[model::Playlist]>,
    artists: Arc<[model::Artist]>,
    loaded: bool,
    searching: bool,
    error: Option<SharedString>,
    request: Option<gpui_kit::Task<()>>,
    track_list: Entity<TrackList>,
    playlist_list: Entity<PlaylistList>,
    _list_subscriptions: [Subscription; 2],
}

impl EventEmitter<PageEvent> for SearchPage {}

impl SearchPage {
    pub(super) fn new(backend: BackendHandle, cx: &mut Context<Self>) -> Self {
        let track_list = cx.new(|cx| TrackList::new(cx));
        let playlist_list = cx.new(|cx| PlaylistList::new(cx));
        Self {
            backend,
            query: String::new(),
            results_query: String::new(),
            kind: SearchKind::Tracks,
            tracks: Arc::default(),
            playlists: Arc::default(),
            artists: Arc::default(),
            loaded: false,
            searching: false,
            error: None,
            request: None,
            _list_subscriptions: [
                page::forward(&track_list, cx),
                page::forward(&playlist_list, cx),
            ],
            track_list,
            playlist_list,
        }
    }

    pub(super) fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.query = query;
        cx.notify();
    }

    pub(super) fn set_kind(&mut self, kind: SearchKind, cx: &mut Context<Self>) {
        self.kind = kind;
        cx.notify();
    }

    /// Runs the trimmed query, reporting whether there was one to run.
    pub(super) fn submit(&mut self, cx: &mut Context<Self>) -> bool {
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            return false;
        }
        self.loaded = false;
        self.searching = true;
        self.error = None;
        let reply = request(&self.backend, {
            let query = query.clone();
            |respond| BackendCommand::SearchCatalog { query, respond }
        });
        self.request = Some(cx.spawn(async move |this, cx| {
            let result = reply.await;
            let _ = this.update(cx, |page, cx| {
                page.request = None;
                page.searching = false;
                page.loaded = true;
                match result {
                    Ok(results) => {
                        page.results_query = query;
                        page.tracks = results.tracks.into();
                        page.playlists = results.playlists.into();
                        page.artists = results.artists.into();
                        page.error = None;
                        cx.emit(PageEvent::Loaded);
                    }
                    Err(error) => {
                        page.error = Some(error.clone());
                        cx.emit(PageEvent::Failed(error));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
        true
    }

    /// Takes down any open row menu, for a route change no click drove.
    pub(super) fn close_menus(&mut self, cx: &mut Context<Self>) {
        self.track_list.update(cx, |list, cx| list.close_menu(cx));
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.request = None;
        self.results_query.clear();
        self.tracks = Arc::default();
        self.playlists = Arc::default();
        self.artists = Arc::default();
        self.loaded = false;
        self.searching = false;
        self.error = None;
        cx.notify();
    }
}

// One inactive page per kind, plus the visible page. Metadata only; artwork
// stays in the shared bounded image cache. Large pages are deliberately not
// retained, and stale metadata follows the existing catalog freshness policy.
const RETAINED_PAGE_TRACK_LIMIT: usize = 1_024;

const RETAINED_PAGE_BYTES: usize = 2 * 1024 * 1024;

fn can_retain_tracks(tracks: &[model::Track]) -> bool {
    can_retain_iter(tracks.iter())
}

fn can_retain_iter<'a>(tracks: impl Iterator<Item = &'a model::Track>) -> bool {
    let text_bytes = |value: &Option<String>| value.as_ref().map_or(0, String::capacity);
    let mut bytes: usize = 0;
    for (index, track) in tracks.enumerate() {
        if index >= RETAINED_PAGE_TRACK_LIMIT {
            return false;
        }
        bytes = bytes.saturating_add(std::mem::size_of::<model::Track>());
        bytes = bytes
            .saturating_add(track.source_id.capacity())
            .saturating_add(track.title.len())
            .saturating_add(track.artist.len())
            .saturating_add(track.album.len())
            .saturating_add(text_bytes(&track.spotify_uri))
            .saturating_add(text_bytes(&track.isrc))
            .saturating_add(text_bytes(&track.artwork_url))
            .saturating_add(track.artists.capacity() * std::mem::size_of::<model::ArtistRef>());
        for artist in &track.artists {
            bytes = bytes
                .saturating_add(artist.name.len())
                .saturating_add(text_bytes(&artist.source_id))
                .saturating_add(text_bytes(&artist.spotify_uri));
        }
        if let Some(album) = &track.album_ref {
            bytes = bytes
                .saturating_add(album.name.len())
                .saturating_add(text_bytes(&album.source_id))
                .saturating_add(text_bytes(&album.spotify_uri))
                .saturating_add(text_bytes(&album.artwork_url));
        }
        if bytes > RETAINED_PAGE_BYTES {
            return false;
        }
    }
    true
}

struct PlaylistSnapshot {
    playlist: model::Playlist,
    tracks: Rc<RefCell<playlist_buffer::PlaylistBuffer>>,
    next_offset: Option<u32>,
    scroll: gpui_kit::UniformListScrollHandle,
    loaded_at: SystemTime,
}

struct AlbumSnapshot {
    reference: model::AlbumRef,
    album: Option<model::Album>,
    tracks: Arc<[model::Track]>,
    scroll: gpui_kit::UniformListScrollHandle,
    loaded_at: SystemTime,
}

/// The tracks of one Spotify playlist.
pub(super) struct PlaylistPage {
    viewport_request: Option<gpui_kit::Task<()>>,
    visible_range: Range<usize>,
    request_offset: Option<u32>,
    play_on_load: bool,
    previous: Option<PlaylistSnapshot>,
    loaded_at: Option<SystemTime>,
    backend: BackendHandle,
    pub(super) selected: Option<model::Playlist>,
    tracks: Rc<RefCell<playlist_buffer::PlaylistBuffer>>,
    loaded: bool,
    next_offset: Option<u32>,
    _pagination_subscription: Subscription,
    error: Option<SharedString>,
    request: Option<gpui_kit::Task<()>>,
    library: Entity<library::Library>,
    player: Entity<player::Player>,
    image_cache: Entity<image_cache::BoundedImageCache>,
    track_list: Entity<TrackList>,
    _list_subscription: Subscription,
}

impl EventEmitter<PageEvent> for PlaylistPage {}

impl PlaylistPage {
    pub(super) fn new(backend: BackendHandle, cx: &mut Context<Self>) -> Self {
        let track_list = cx.new(|cx| TrackList::new(cx));
        Self {
            previous: None,
            loaded_at: None,
            viewport_request: None,
            visible_range: 0..0,
            request_offset: None,
            play_on_load: false,
            backend,
            selected: None,
            tracks: Rc::default(),
            loaded: false,
            next_offset: None,
            _pagination_subscription: cx.subscribe(
                &track_list,
                |this, _, range: &track_list::LoadMoreTracks, cx| {
                    this.request_visible(range.0.clone(), cx)
                },
            ),
            error: None,
            request: None,
            library: services::AppServices::library(cx),
            player: services::AppServices::player(cx),
            image_cache: services::AppServices::image_cache(cx),
            _list_subscription: page::forward(&track_list, cx),
            track_list,
        }
    }

    /// Takes down any open row menu, for a route change no click drove.
    pub(super) fn close_menus(&mut self, cx: &mut Context<Self>) {
        self.track_list.update(cx, |list, cx| list.close_menu(cx));
    }

    /// Starts the page's contents from the top, if there is anything to play.
    pub(super) fn play(
        &mut self,
        _: &Rc<RefCell<playlist_buffer::PlaylistBuffer>>,
        cx: &mut Context<Self>,
    ) {
        let playback = self.tracks.borrow().first_playback();
        if let (Some((tracks, index, next)), Some(playlist)) = (playback, self.selected.clone()) {
            self.player.update(cx, |player, cx| {
                player.play_playlist(playlist, tracks, index, next, cx)
            });
        } else {
            let missing = self.tracks.borrow().missing_start();
            if let (Some(playlist), Some(offset)) = (self.selected.clone(), missing) {
                self.play_on_load = true;
                self.viewport_request = None;
                self.fetch_page(playlist, offset, cx);
            }
        }
    }

    pub(super) fn open(&mut self, playlist: model::Playlist, cx: &mut Context<Self>) {
        if self
            .selected
            .as_ref()
            .is_some_and(|old| old.source_id == playlist.source_id)
            && self.error.is_none()
            && (self.request.is_some() || catalog_data_is_fresh(self.loaded_at))
        {
            return;
        }
        self.viewport_request = None;
        self.request_offset = None;
        self.visible_range = 0..0;
        self.play_on_load = false;
        let previous = self.previous.take();
        self.previous = self
            .selected
            .clone()
            .filter(|_| {
                self.loaded
                    && self.error.is_none()
                    && can_retain_iter(self.tracks.borrow().tracks())
            })
            .zip(self.loaded_at)
            .map(|(playlist, loaded_at)| PlaylistSnapshot {
                playlist,
                tracks: self.tracks.clone(),
                next_offset: self.next_offset,
                scroll: self.track_list.read(cx).scroll_handle(),
                loaded_at,
            });
        self.request = None;
        if let Some(saved) = previous.filter(|saved| {
            saved.playlist.source_id == playlist.source_id
                && catalog_data_is_fresh(Some(saved.loaded_at))
        }) {
            self.selected = Some(saved.playlist.clone());
            self.tracks = saved.tracks;
            self.next_offset = saved.next_offset;
            self.loaded_at = Some(saved.loaded_at);
            self.loaded = true;
            self.error = None;
            self.track_list.update(cx, |list, cx| {
                list.show_playlist(
                    (ElementId::from("playlist-tracks"), playlist.source_id),
                    self.tracks.clone(),
                    cx,
                );
                list.set_playlist_context(saved.playlist, self.next_offset, false);
                list.restore_scroll(saved.scroll, cx);
            });
            cx.notify();
            return;
        }
        self.loaded_at = None;
        self.selected = Some(playlist.clone());
        self.tracks = Rc::new(RefCell::new(playlist_buffer::PlaylistBuffer::new(
            playlist.track_count as usize,
        )));
        self.track_list.update(cx, |list, cx| list.clear(cx));
        self.loaded = false;
        self.error = None;
        self.request = None;
        self.next_offset = Some(0);
        self.fetch_page(playlist, 0, cx);
        cx.notify();
    }

    fn request_visible(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.visible_range = range.clone();
        if self.play_on_load || self.error.is_some() {
            return;
        }
        let missing = self.tracks.borrow_mut().missing_offset(range);
        self.viewport_request = None;
        let Some(offset) = missing else {
            return;
        };
        if self.request_offset == Some(offset) {
            return;
        }
        self.viewport_request = Some(cx.spawn(async move |page, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let _ = page.update(cx, |page, cx| {
                page.viewport_request = None;
                let missing = page
                    .tracks
                    .borrow_mut()
                    .missing_offset(page.visible_range.clone());
                if let (Some(playlist), Some(offset)) = (page.selected.clone(), missing) {
                    page.error = None;
                    page.fetch_page(playlist, offset, cx);
                }
            });
        }));
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        self.request_visible(self.visible_range.clone(), cx);
    }

    fn fetch_page(&mut self, playlist: model::Playlist, offset: u32, cx: &mut Context<Self>) {
        self.request = None;
        self.request_offset = Some(offset);
        let source_id = playlist.source_id.clone();
        let reply = request(&self.backend, move |respond| BackendCommand::LoadPlaylist {
            playlist,
            offset,
            respond,
        });
        self.request = Some(cx.spawn(async move |this, cx| {
            let result = reply.await;
            let _ = this.update(cx, |page, cx| {
                if page
                    .selected
                    .as_ref()
                    .is_none_or(|playlist| playlist.source_id != source_id)
                {
                    return;
                }
                if page.request_offset != Some(offset) {
                    return;
                }
                page.request = None;
                page.request_offset = None;
                page.loaded = true;
                let mut play_start = None;
                match result {
                    Ok(result) => {
                        if !result.tracks.is_empty() {
                            play_start =
                                Some(result.source_offsets.first().copied().unwrap_or(offset)
                                    as usize);
                        }
                        page.loaded_at = Some(SystemTime::now());
                        page.next_offset = result.next_offset;
                        if let Some(playlist) = page.selected.as_mut() {
                            playlist.track_count = result.total;
                        }
                        page.tracks.borrow_mut().insert(offset, result);
                        page.error = None;
                        cx.emit(PageEvent::Loaded);
                    }
                    Err(error) => {
                        page.play_on_load = false;
                        page.error = Some(error.clone());
                        cx.emit(PageEvent::Failed(error));
                    }
                }
                if page.error.is_none() {
                    if page.play_on_load {
                        if let (Some(index), Some(playlist)) = (play_start, page.selected.clone()) {
                            let playback = page.tracks.borrow().playback(index);
                            if let Some((tracks, index, next)) = playback {
                                page.play_on_load = false;
                                page.player.update(cx, |player, cx| {
                                    player.play_playlist(playlist, tracks, index, next, cx)
                                });
                            }
                        } else if let (Some(playlist), Some(next)) =
                            (page.selected.clone(), page.next_offset)
                        {
                            page.fetch_page(playlist, next, cx);
                            cx.notify();
                            return;
                        } else {
                            page.play_on_load = false;
                        }
                    }
                    page.request_visible(page.visible_range.clone(), cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.viewport_request = None;
        self.request_offset = None;
        self.play_on_load = false;
        self.visible_range = 0..0;
        self.previous = None;
        self.loaded_at = None;
        self.request = None;
        self.selected = None;
        self.tracks = Rc::default();
        self.track_list.update(cx, |list, cx| list.clear(cx));
        self.loaded = false;
        self.next_offset = None;
        self.error = None;
        cx.notify();
    }
}

/// One artist, their popular tracks and their discography.
const ARTIST_BACKGROUND: u32 = 0x242426;
const ARTIST_PALETTE: CadencePalette = CadencePalette {
    canvas: ARTIST_BACKGROUND,
    surface: ARTIST_BACKGROUND,
    surface_raised: 0x303033,
    surface_hover: 0x2D2D30,
    ..CadencePalette::DARK
};

pub(super) struct ArtistPage {
    backend: BackendHandle,
    reference: Option<model::ArtistRef>,
    artist: Option<model::Artist>,
    tracks: Arc<[model::Track]>,
    albums: Arc<[model::Album]>,
    content_state: gpui_kit::ListState,
    content_layout: Option<(usize, usize, usize, Pixels)>,
    loaded: bool,
    error: Option<SharedString>,
    loaded_at: Option<SystemTime>,
    request: Option<gpui_kit::Task<()>>,
    image_cache: Entity<image_cache::BoundedImageCache>,
    track_list: Entity<TrackList>,
    _list_subscription: Subscription,
}

impl EventEmitter<PageEvent> for ArtistPage {}

impl ArtistPage {
    pub(super) fn new(backend: BackendHandle, cx: &mut Context<Self>) -> Self {
        let track_list = cx.new(|cx| {
            TrackList::new(cx)
                .with_palette(ARTIST_PALETTE)
                .with_inline_rows()
        });
        Self {
            backend,
            reference: None,
            artist: None,
            tracks: Arc::default(),
            albums: Arc::default(),
            content_state: gpui_kit::ListState::new(0, gpui_kit::ListAlignment::Top, px(256.)),
            content_layout: None,
            loaded: false,
            error: None,
            loaded_at: None,
            request: None,
            image_cache: services::AppServices::image_cache(cx),
            _list_subscription: page::forward(&track_list, cx),
            track_list,
        }
    }

    /// Takes down any open row menu, for a route change no click drove.
    pub(super) fn close_menus(&mut self, cx: &mut Context<Self>) {
        self.track_list.update(cx, |list, cx| list.close_menu(cx));
    }

    /// Shows `artist`, refetching unless the cached copy is still fresh.
    /// Reports whether this is a different artist than the one already shown.
    pub(super) fn open(&mut self, artist: model::ArtistRef, cx: &mut Context<Self>) -> bool {
        let Some(source_id) = artist.source_id.clone() else {
            return false;
        };
        let same_artist = self
            .reference
            .as_ref()
            .and_then(|artist| artist.source_id.as_deref())
            == Some(source_id.as_str());
        let retrying_failure = same_artist && self.error.is_some();
        let loading = self.request.is_some();
        let should_refresh = !same_artist || (!loading && !catalog_data_is_fresh(self.loaded_at));
        self.reference = Some(artist);
        self.error = None;
        if !same_artist {
            self.artist = None;
            self.tracks = Arc::default();
            self.albums = Arc::default();
            self.loaded = false;
            self.loaded_at = None;
            self.content_layout = None;
        } else if retrying_failure {
            self.loaded = false;
        }
        if should_refresh {
            let reply = request(&self.backend, |respond| BackendCommand::LoadArtist {
                source_id,
                respond,
            });
            self.request = Some(cx.spawn(async move |this, cx| {
                let result = reply.await;
                let _ = this.update(cx, |page, cx| {
                    page.request = None;
                    match result {
                        Ok((artist, tracks, albums)) => {
                            page.artist = Some(artist);
                            page.tracks = tracks.into();
                            page.albums = albums.into();
                            page.content_layout = None;
                            page.loaded = true;
                            page.error = None;
                            page.loaded_at = Some(SystemTime::now());
                            cx.emit(PageEvent::Loaded);
                        }
                        Err(error) => {
                            if !page.loaded {
                                page.loaded = true;
                                page.error = Some(error.clone());
                            }
                            cx.emit(PageEvent::Failed(error));
                        }
                    }
                    cx.notify();
                });
            }));
        }
        cx.notify();
        !same_artist
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.request = None;
        self.reference = None;
        self.artist = None;
        self.tracks = Arc::default();
        self.albums = Arc::default();
        self.loaded = false;
        self.error = None;
        self.loaded_at = None;
        cx.notify();
    }

    /// Popular tracks form one small, naturally sized block. Release rows are
    /// virtualized in the same scroll area, so a large discography stays cheap.
    fn content_item(&mut self, index: usize, columns: usize, cx: &mut Context<Self>) -> AnyElement {
        let heading = |id, title| {
            div()
                .id(id)
                .test_support()
                .text_xl()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(rgb(ARTIST_PALETTE.text_primary))
                .child(title)
        };
        if index == 0 {
            return div()
                .w_full()
                .flex()
                .flex_col()
                .gap_3()
                .child(heading("artist-popular-heading", "Popular"))
                .when(self.tracks.is_empty(), |section| {
                    section.child(components::page_detail(
                        ARTIST_PALETTE,
                        "No popular tracks available",
                    ))
                })
                .when(!self.tracks.is_empty(), |section| {
                    section.child(self.track_list.clone())
                })
                .into_any_element();
        }
        if index == 1 {
            return div()
                .w_full()
                .pt_6()
                .pb_3()
                .flex()
                .flex_col()
                .gap_3()
                .child(heading("artist-discography-heading", "Discography"))
                .when(self.albums.is_empty(), |section| {
                    section.child(components::page_detail(
                        ARTIST_PALETTE,
                        "No releases available",
                    ))
                })
                .into_any_element();
        }
        let start = (index - 2) * columns;
        let end = (start + columns).min(self.albums.len());
        let mut cards = div()
            .h(tokens::ALBUM_CARD_HEIGHT + Rems(0.5))
            .w_full()
            .flex()
            .items_start()
            .gap_2();
        for album_index in start..end {
            cards = cards.child(self.album_card(&self.albums[album_index], album_index, cx));
        }
        for _ in end..start + columns {
            cards = cards.child(div().w(tokens::ALBUM_CARD_ARTWORK.size + Rems(1.)));
        }
        cards.into_any_element()
    }

    fn album_card(&self, album: &model::Album, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let palette = ARTIST_PALETTE;
        let year = album
            .release_date
            .as_deref()
            .and_then(|date| date.get(..4))
            .unwrap_or("Release")
            .to_owned();

        components::button(palette, ("artist-album", index))
            .test_support()
            .w(tokens::ALBUM_CARD_ARTWORK.size + Rems(1.))
            .flex_none()
            .min_w_0()
            .h(tokens::ALBUM_CARD_HEIGHT)
            .p_2()
            .flex_col()
            .items_center()
            .justify_start()
            .rounded_2xl()
            .hover(|style| style.bg(rgb(palette.surface_hover)))
            .child(
                div()
                    .w(tokens::ALBUM_CARD_ARTWORK.size)
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_2p5()
                    .child(components::artwork(
                        palette,
                        &self.image_cache,
                        album.artwork_url.as_deref(),
                        tokens::ALBUM_CARD_ARTWORK,
                        CadenceIcon::MusicNote,
                    ))
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text_primary))
                            .child(album.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.text_muted))
                            .child(year),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(album) = this.albums.get(index) {
                    cx.emit(PageEvent::OpenAlbum(model::AlbumRef {
                        name: album.name.clone(),
                        source_id: Some(album.source_id.clone()),
                        spotify_uri: album.spotify_uri.clone(),
                        artwork_url: album.artwork_url.clone(),
                    }));
                }
            }))
            .into_any_element()
    }
}

/// One album and its tracks.
pub(super) struct AlbumPage {
    previous: Option<AlbumSnapshot>,
    backend: BackendHandle,
    reference: Option<model::AlbumRef>,
    album: Option<model::Album>,
    tracks: Arc<[model::Track]>,
    loaded: bool,
    error: Option<SharedString>,
    loaded_at: Option<SystemTime>,
    request: Option<gpui_kit::Task<()>>,
    player: Entity<player::Player>,
    image_cache: Entity<image_cache::BoundedImageCache>,
    track_list: Entity<TrackList>,
    _list_subscription: Subscription,
}

impl EventEmitter<PageEvent> for AlbumPage {}

impl AlbumPage {
    pub(super) fn new(backend: BackendHandle, cx: &mut Context<Self>) -> Self {
        let track_list = cx.new(|cx| TrackList::new(cx));
        Self {
            previous: None,
            backend,
            reference: None,
            album: None,
            tracks: Arc::default(),
            loaded: false,
            error: None,
            loaded_at: None,
            request: None,
            player: services::AppServices::player(cx),
            image_cache: services::AppServices::image_cache(cx),
            _list_subscription: page::forward(&track_list, cx),
            track_list,
        }
    }

    /// Takes down any open row menu, for a route change no click drove.
    pub(super) fn close_menus(&mut self, cx: &mut Context<Self>) {
        self.track_list.update(cx, |list, cx| list.close_menu(cx));
    }

    /// Starts the page's contents from the top, if there is anything to play.
    pub(super) fn play(&mut self, tracks: &Arc<[model::Track]>, cx: &mut Context<Self>) {
        if tracks.is_empty() {
            return;
        }
        let tracks = tracks.to_vec();
        self.player
            .update(cx, |player, cx| player.play_context(tracks, 0, cx));
    }

    /// Shows `album`, refetching unless the cached copy is still fresh.
    /// Reports whether this is a different album than the one already shown.
    pub(super) fn open(&mut self, album: model::AlbumRef, cx: &mut Context<Self>) -> bool {
        let Some(source_id) = album.source_id.clone() else {
            return false;
        };
        let same_album = self
            .reference
            .as_ref()
            .and_then(|album| album.source_id.as_deref())
            == Some(source_id.as_str());
        if !same_album {
            let previous = self.previous.take();
            self.previous = self
                .reference
                .clone()
                .filter(|_| self.loaded && self.error.is_none() && can_retain_tracks(&self.tracks))
                .zip(self.loaded_at)
                .map(|(reference, loaded_at)| AlbumSnapshot {
                    reference,
                    album: self.album.clone(),
                    tracks: self.tracks.clone(),
                    scroll: self.track_list.read(cx).scroll_handle(),
                    loaded_at,
                });
            self.request = None;
            if let Some(saved) = previous.filter(|saved| {
                saved.reference.source_id == album.source_id
                    && catalog_data_is_fresh(Some(saved.loaded_at))
            }) {
                self.reference = Some(album);
                self.album = saved.album;
                self.tracks = saved.tracks;
                self.loaded_at = Some(saved.loaded_at);
                self.loaded = true;
                self.error = None;
                self.track_list.update(cx, |list, cx| {
                    list.show(
                        (ElementId::from("album-tracks"), source_id.clone()),
                        self.tracks.clone(),
                        cx,
                    );
                    list.set_current_album_id(Some(source_id), cx);
                    list.restore_scroll(saved.scroll, cx);
                });
                cx.notify();
                return true;
            }
        }
        let retrying_failure = same_album && self.error.is_some();
        let loading = self.request.is_some();
        let should_refresh = !same_album || (!loading && !catalog_data_is_fresh(self.loaded_at));
        self.reference = Some(album);
        self.error = None;
        self.track_list.update(cx, |list, cx| {
            list.set_current_album_id(Some(source_id.clone()), cx)
        });
        if !same_album {
            self.track_list.update(cx, |list, cx| list.clear(cx));
            self.album = None;
            self.tracks = Arc::default();
            self.loaded = false;
            self.loaded_at = None;
        } else if retrying_failure {
            self.loaded = false;
        }
        if should_refresh {
            let reply = request(&self.backend, |respond| BackendCommand::LoadAlbum {
                source_id,
                respond,
            });
            self.request = Some(cx.spawn(async move |this, cx| {
                let result = reply.await;
                let _ = this.update(cx, |page, cx| {
                    page.request = None;
                    match result {
                        Ok((album, tracks)) => {
                            page.album = Some(album);
                            page.tracks = tracks.into();
                            page.loaded = true;
                            page.error = None;
                            page.loaded_at = Some(SystemTime::now());
                            cx.emit(PageEvent::Loaded);
                        }
                        Err(error) => {
                            if !page.loaded {
                                page.loaded = true;
                                page.error = Some(error.clone());
                            }
                            cx.emit(PageEvent::Failed(error));
                        }
                    }
                    cx.notify();
                });
            }));
        }
        cx.notify();
        !same_album
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.previous = None;
        self.track_list.update(cx, |list, cx| list.clear(cx));
        self.request = None;
        self.reference = None;
        self.album = None;
        self.tracks = Arc::default();
        self.loaded = false;
        self.error = None;
        self.loaded_at = None;
        self.track_list
            .update(cx, |list, cx| list.set_current_album_id(None, cx));
        cx.notify();
    }
}

impl Render for SearchPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let kind = self.kind;
        let query = self.query.clone();
        let tracks = self.tracks.clone();
        let playlists = self.playlists.clone();
        let searching = self.searching;
        let loaded = self.loaded;
        let results = if self.error.is_some() {
            components::empty_state(palette, "Unable to search Spotify").into_any_element()
        } else if kind == SearchKind::Tracks && !tracks.is_empty() {
            let list_id = (ElementId::from("search-tracks"), self.results_query.clone());
            self.track_list
                .update(cx, |list, cx| list.show(list_id, tracks, cx));
            self.track_list.clone().into_any_element()
        } else if kind == SearchKind::Playlists && !playlists.is_empty() {
            let list_id = (
                ElementId::from("search-playlists"),
                self.results_query.clone(),
            );
            self.playlist_list
                .update(cx, |list, cx| list.show(list_id, playlists, cx));
            self.playlist_list.clone().into_any_element()
        } else {
            let message = match (loaded, searching) {
                (true, _) if kind == SearchKind::Tracks => "No tracks found",
                (true, _) => "No playlists found",
                (_, true) => "Searching Spotify…",
                _ => "Press Return to search",
            };
            components::empty_state(palette, message).into_any_element()
        };

        let results =
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .when(
                    kind == SearchKind::Tracks && !self.artists.is_empty() && !searching,
                    |page| {
                        let cache = services::AppServices::image_cache(cx);
                        page.child(
                            div()
                                .mb_3()
                                .child(components::section_label(palette, "Artists"))
                                .child(div().mt_2().flex().gap_2().children(
                                    self.artists.iter().take(3).enumerate().map(
                                        |(index, artist)| {
                                            let reference = model::ArtistRef {
                                                name: artist.name.clone(),
                                                source_id: Some(artist.source_id.clone()),
                                                spotify_uri: artist.spotify_uri.clone(),
                                            };
                                            components::button(palette, ("search-artist", index))
                                                .test_support()
                                                .hover(|style| style.bg(rgb(palette.control)))
                                                .h_20()
                                                .flex_1()
                                                .min_w_0()
                                                .px_2()
                                                .gap_2()
                                                .rounded_lg()
                                                .justify_start()
                                                .child(components::artwork(
                                                    palette,
                                                    &cache,
                                                    artist.artwork_url.as_deref(),
                                                    tokens::SEARCH_ARTIST_ARTWORK,
                                                    CadenceIcon::Person,
                                                ))
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .text_ellipsis()
                                                        .line_clamp(1)
                                                        .child(artist.name.clone()),
                                                )
                                                .on_click(cx.listener(move |_, _, _, cx| {
                                                    cx.emit(PageEvent::OpenArtist(
                                                        reference.clone(),
                                                    ))
                                                }))
                                        },
                                    ),
                                )),
                        )
                    },
                )
                .child(results);

        components::page("search-page")
            .pt_3()
            .child(components::page_heading(
                palette,
                "Search results",
                format!("Results for {query}"),
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mb_5()
                    .child(
                        components::pill(
                            palette,
                            "tab-tracks",
                            "Tracks",
                            kind == SearchKind::Tracks,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.set_kind(SearchKind::Tracks, cx)),
                        ),
                    )
                    .child(
                        components::pill(
                            palette,
                            "tab-playlists",
                            "Playlists",
                            kind == SearchKind::Playlists,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.set_kind(SearchKind::Playlists, cx)),
                        ),
                    ),
            )
            .child(results)
    }
}

impl Render for PlaylistPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tracks = self.tracks.clone();
        let loaded = self.loaded;
        let empty = tracks.borrow().is_empty();
        let pinned = self
            .selected
            .as_ref()
            .is_some_and(|playlist| self.library.read(cx).is_playlist_pinned(playlist));
        let (name, detail) = self.selected.as_ref().map_or_else(
            || {
                (
                    SharedString::new_static("Playlist"),
                    "Spotify playlist".to_owned(),
                )
            },
            |playlist| {
                (
                    playlist.name.clone(),
                    format!("Spotify playlist · {} tracks", playlist.track_count),
                )
            },
        );
        let artwork_url = self
            .selected
            .as_ref()
            .and_then(|playlist| playlist.artwork_url.clone());
        let palette = collection_palette(artwork_url.as_deref(), &self.image_cache, window, cx);
        self.track_list
            .update(cx, |list, cx| list.set_palette(palette, cx));
        let list = if let Some(error) = self.error.as_deref().filter(|_| empty) {
            components::empty_state(palette, format!("Unable to load playlist: {error}"))
                .into_any_element()
        } else if let Some(playlist) = self.selected.as_ref().filter(|_| !empty) {
            let list_id = (
                ElementId::from("playlist-tracks"),
                playlist.source_id.clone(),
            );
            self.track_list.update(cx, |list, cx| {
                list.set_playlist_context(
                    playlist.clone(),
                    self.next_offset,
                    self.request.is_some() || self.error.is_some(),
                );
                list.show_playlist(list_id, tracks.clone(), cx);
            });
            self.track_list.clone().into_any_element()
        } else if self.selected.is_none() {
            components::empty_state(palette, "No playlist selected").into_any_element()
        } else if loaded && self.next_offset.is_none() {
            components::empty_state(palette, "This playlist is empty").into_any_element()
        } else {
            components::list_skeleton(palette).into_any_element()
        };
        let list = div().flex_1().min_h_0().flex().flex_col().child(list).when(
            self.error.is_some() && !empty,
            |container| {
                container.child(
                    components::button(palette, "playlist-retry-page")
                        .test_support()
                        .h_10()
                        .child("Couldn't load more tracks · Retry")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.error = None;
                            this.load_more(cx);
                        })),
                )
            },
        );
        let playback_tracks = tracks;

        components::page("playlist-page")
            .bg(rgb(palette.canvas))
            .text_color(rgb(palette.text))
            .pt_2()
            .child(
                page_header()
                    .child(components::artwork(
                        palette,
                        &self.image_cache,
                        artwork_url.as_deref(),
                        tokens::COLLECTION_ARTWORK,
                        CadenceIcon::Playlist,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .items_start()
                            .gap_2()
                            .child(
                                components::page_title(palette, name)
                                    .w_full()
                                    .text_ellipsis()
                                    .line_clamp(2),
                            )
                            .child(components::page_detail(palette, detail))
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .mt_2()
                                    .child(
                                        components::pill(palette, "playlist-play", "Play", true)
                                            .test_support()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.play(&playback_tracks, cx);
                                            })),
                                    )
                                    .child(
                                        components::icon_button(
                                            palette,
                                            "playlist-pin",
                                            if pinned {
                                                CadenceIcon::PinFilled
                                            } else {
                                                CadenceIcon::Pin
                                            },
                                        )
                                        .bg(rgb(if pinned {
                                            palette.selection
                                        } else {
                                            palette.control
                                        }))
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                if let Some(playlist) = this.selected.clone() {
                                                    this.library.update(cx, |library, cx| {
                                                        library.set_playlist_pinned(
                                                            playlist, !pinned, cx,
                                                        )
                                                    });
                                                }
                                                cx.notify();
                                            }),
                                        ),
                                    ),
                            ),
                    ),
            )
            .child(list)
    }
}

impl Render for ArtistPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = ARTIST_PALETTE;
        let name = self
            .artist
            .as_ref()
            .map(|artist| artist.name.clone())
            .or_else(|| self.reference.as_ref().map(|artist| artist.name.clone()))
            .unwrap_or_else(|| SharedString::new_static("Artist"));
        let artwork_url = self
            .artist
            .as_ref()
            .and_then(|artist| artist.artwork_url.clone());
        let source_id = self
            .reference
            .as_ref()
            .and_then(|artist| artist.source_id.clone())
            .unwrap_or_default();
        let detail = if self.loaded && self.error.is_none() {
            format!("Spotify artist · {} releases", self.albums.len())
        } else {
            "Spotify artist".to_owned()
        };
        let tracks = self.tracks.clone();
        let columns = if is_compact_content_layout(window) {
            3
        } else {
            4
        };
        let content = if let Some(error) = self.error.as_ref() {
            components::empty_state(palette, format!("Unable to load artist: {error}"))
                .into_any_element()
        } else if !self.loaded {
            components::list_skeleton(palette).into_any_element()
        } else {
            let layout = (tracks.len(), self.albums.len(), columns, window.rem_size());
            if self.content_layout != Some(layout) {
                self.content_state
                    .reset(2 + self.albums.len().div_ceil(columns));
                self.content_layout = Some(layout);
            }
            let list_id = (ElementId::from("artist-popular"), source_id);
            self.track_list
                .update(cx, |list, cx| list.show(list_id, tracks, cx));
            gpui_kit::list(
                self.content_state.clone(),
                cx.processor(move |this, index, _, cx| this.content_item(index, columns, cx)),
            )
            .flex_1()
            .min_h_0()
            .into_any_element()
        };

        let overlay_inset = tokens::TOOLBAR_HEIGHT.to_pixels(window.rem_size());

        let backdrop = artwork_url.clone().map(|url| {
            let cache = self.image_cache.clone();
            img(move |window: &mut Window, cx: &mut App| {
                cache.update(cx, |cache, cx| cache.artist_backdrop(&url, window, cx))
            })
            .absolute()
            .inset_0()
            .size_full()
            .aspect_ratio(1.)
            .flex_none()
            .object_fit(gpui_kit::ObjectFit::Cover)
            .id("artist-backdrop-image")
            .test_support()
        });
        let header = div()
            .id("artist-header")
            .test_support()
            .relative()
            .w_full()
            .h(Rems(13.).to_pixels(window.rem_size()) + overlay_inset)
            .flex_none()
            .overflow_hidden()
            .children(backdrop.map(|image| div().absolute().inset_0().size_full().child(image)))
            .child(div().absolute().inset_0().bg(gpui_kit::rgba(0x10101455)))
            .child(div().absolute().inset_0().bg(gpui_kit::linear_gradient(
                180.,
                gpui_kit::linear_color_stop(rgb(ARTIST_BACKGROUND).opacity(0.05), 0.),
                gpui_kit::linear_color_stop(rgb(ARTIST_BACKGROUND), 1.),
            )))
            .child(
                page_header()
                    .relative()
                    .px_8()
                    .pt(Rems(1.5).to_pixels(window.rem_size()) + overlay_inset)
                    .items_end()
                    .child(
                        div()
                            .id("artist-portrait")
                            .test_support()
                            .size(tokens::ARTIST_ARTWORK.size)
                            .flex_none()
                            .child(components::artwork(
                                palette,
                                &self.image_cache,
                                artwork_url.as_deref(),
                                tokens::ARTIST_ARTWORK,
                                CadenceIcon::Person,
                            )),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(components::page_title(palette, name))
                            .child(components::page_detail(palette, detail)),
                    ),
            );
        div()
            .id("artist-page")
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(ARTIST_BACKGROUND))
            .text_color(rgb(palette.text))
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_8()
                    .pb_8()
                    .flex()
                    .flex_col()
                    .child(content),
            )
    }
}

impl Render for AlbumPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .album
            .as_ref()
            .map(|album| album.name.clone())
            .or_else(|| self.reference.as_ref().map(|album| album.name.clone()))
            .unwrap_or_else(|| SharedString::new_static("Album"));
        let artwork_url = self
            .album
            .as_ref()
            .and_then(|album| album.artwork_url.clone())
            .or_else(|| {
                self.reference
                    .as_ref()
                    .and_then(|album| album.artwork_url.clone())
            });
        let palette = collection_palette(artwork_url.as_deref(), &self.image_cache, window, cx);
        self.track_list
            .update(cx, |list, cx| list.set_palette(palette, cx));
        let detail = self.album.as_ref().map_or_else(
            || "Spotify album".to_owned(),
            |album| {
                let artists = album
                    .artists
                    .iter()
                    .map(|artist| artist.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut details = vec![artists];
                if let Some(year) = album.release_date.as_deref().and_then(|date| date.get(..4)) {
                    details.push(year.to_owned());
                }
                if let Some(track_count) = album.track_count {
                    details.push(format!("{track_count} tracks"));
                }
                details.join(" · ")
            },
        );
        let tracks = self.tracks.clone();
        let loaded = self.loaded;
        let list = if let Some(error) = self.error.as_ref() {
            components::empty_state(palette, format!("Unable to load album: {error}"))
                .into_any_element()
        } else if !tracks.is_empty() {
            let source_id = self
                .reference
                .as_ref()
                .and_then(|album| album.source_id.clone())
                .expect("open only loads albums that have a source id");
            let list_id = (ElementId::from("album-tracks"), source_id);
            self.track_list
                .update(cx, |list, cx| list.show(list_id, tracks.clone(), cx));
            self.track_list.clone().into_any_element()
        } else if loaded {
            components::empty_state(palette, "This album has no playable tracks").into_any_element()
        } else {
            components::empty_state(palette, "Loading album…").into_any_element()
        };
        let playback_tracks = tracks;

        components::page("album-page")
            .bg(rgb(palette.canvas))
            .text_color(rgb(palette.text))
            .pt_2()
            .child(
                page_header()
                    .child(components::artwork(
                        palette,
                        &self.image_cache,
                        artwork_url.as_deref(),
                        tokens::COLLECTION_ARTWORK,
                        CadenceIcon::MusicNote,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_start()
                            .gap_2()
                            .child(components::page_title(palette, name))
                            .child(components::page_detail(palette, detail))
                            .child(
                                components::pill(palette, "album-play", "Play", true).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.play(&playback_tracks, cx);
                                    }),
                                ),
                            ),
                    ),
            )
            .child(list)
    }
}

fn collection_palette(
    url: Option<&str>,
    cache: &Entity<image_cache::BoundedImageCache>,
    window: &mut Window,
    cx: &mut App,
) -> CadencePalette {
    let background = url
        .and_then(|url| cache.update(cx, |cache, cx| cache.artwork_tint(url, window, cx)))
        .unwrap_or(ARTIST_BACKGROUND);
    CadencePalette {
        canvas: background,
        surface: background,
        text_muted: 0xB8B4AF,
        ..CadencePalette::DARK
    }
}

/// The artwork-and-title band a detail page opens with.
fn page_header() -> Div {
    div().flex().flex_none().items_center().gap_7().mb_6()
}

#[cfg(test)]
mod retained_page_tests {
    use super::*;

    #[test]
    fn navigating_away_releases_oversized_album_metadata_even_before_the_next_reply() {
        let mut cx = gpui_kit::HeadlessAppContext::with_asset_source(
            Arc::new(gpui_kit::NoopTextSystem),
            Arc::new(assets::CadenceAssets),
        );
        cx.update(|cx| {
            let _probe = crate::app::test_support::initialize(cx, ThemePreference::Dark);
            let page = cx.new(|cx| AlbumPage::new(services::AppServices::backend(cx), cx));
            let tracks: Arc<[model::Track]> =
                vec![crate::app::test_support::track(0); RETAINED_PAGE_TRACK_LIMIT + 1].into();
            let weak = Arc::downgrade(&tracks);
            page.update(cx, |page, cx| {
                page.reference = Some(model::AlbumRef {
                    source_id: Some("old".into()),
                    ..Default::default()
                });
                page.loaded = true;
                page.loaded_at = Some(SystemTime::now());
                page.tracks = tracks;
                page.track_list
                    .update(cx, |list, cx| list.show("old", page.tracks.clone(), cx));
                page.open(
                    model::AlbumRef {
                        source_id: Some("new".into()),
                        ..Default::default()
                    },
                    cx,
                );
                assert!(page.previous.is_none());
            });
            assert!(
                weak.upgrade().is_none(),
                "hidden list must not retain an evicted album"
            );
        });
    }

    #[test]
    fn inactive_pages_are_bounded_by_track_count_and_metadata_bytes() {
        let mut track = crate::app::test_support::track(0);
        assert!(can_retain_tracks(std::slice::from_ref(&track)));
        track.source_id = "x".repeat(RETAINED_PAGE_BYTES);
        assert!(!can_retain_tracks(std::slice::from_ref(&track)));
        let tracks = vec![crate::app::test_support::track(0); RETAINED_PAGE_TRACK_LIMIT + 1];
        assert!(!can_retain_tracks(&tracks));
    }
}
