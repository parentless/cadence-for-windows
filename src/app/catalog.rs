use super::*;
use gpui_kit::TestSupportExt as _;
use std::cell::RefCell;

use page::PageEvent;
use track_list::TrackList;

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
    results_query: SharedString,
    tracks: Arc<[model::Track]>,
    albums: Arc<[model::Album]>,
    artists: Arc<[model::Artist]>,
    loaded: bool,
    searching: bool,
    error: Option<SharedString>,
    request: Option<gpui_kit::Task<()>>,
    track_list: Entity<TrackList>,
    scroll: gpui_kit::ScrollHandle,
    content_width: Pixels,
    _list_subscription: Subscription,
}

impl EventEmitter<PageEvent> for SearchPage {}

impl SearchPage {
    pub(super) fn new(backend: BackendHandle, cx: &mut Context<Self>) -> Self {
        let track_list = cx.new(|cx| TrackList::new(cx).with_inline_rows());
        Self {
            backend,
            query: String::new(),
            results_query: SharedString::default(),
            tracks: Arc::default(),
            albums: Arc::default(),
            artists: Arc::default(),
            loaded: false,
            searching: false,
            error: None,
            request: None,
            scroll: gpui_kit::ScrollHandle::new(),
            content_width: px(800.),
            _list_subscription: page::forward(&track_list, cx),
            track_list,
        }
    }

    pub(super) fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.query = query;
        cx.notify();
    }

    /// Runs the trimmed query, reporting whether there was one to run.
    pub(super) fn submit(&mut self, cx: &mut Context<Self>) -> bool {
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            return false;
        }
        self.clear(cx);
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
                    Ok(mut results) => {
                        results.artists.truncate(model::SEARCH_ARTIST_LIMIT);
                        results.albums.truncate(model::SEARCH_ALBUM_LIMIT);
                        results.tracks.truncate(model::SEARCH_TRACK_LIMIT);
                        page.results_query = query.into();
                        page.tracks = results.tracks.into();
                        page.albums = results.albums.into();
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
        self.results_query = SharedString::default();
        self.track_list.update(cx, |list, cx| list.clear(cx));
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.tracks = Arc::default();
        self.albums = Arc::default();
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
                if page.error.is_some() || page.play_on_load {
                    return;
                }
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
        let visible = self
            .tracks
            .borrow_mut()
            .missing_offset(self.visible_range.clone());
        let offset = visible.or_else(|| self.tracks.borrow().missing_start());
        if let (Some(playlist), Some(offset)) = (self.selected.clone(), offset) {
            self.viewport_request = None;
            self.error = None;
            self.fetch_page(playlist, offset, cx);
            cx.notify();
        }
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
/// Equal breathing room above and below detail-page artwork.
const DETAIL_HEADER_PADDING: Rems = Rems(3.5);

pub(super) struct ArtistPage {
    backend: BackendHandle,
    reference: Option<model::ArtistRef>,
    artist: Option<model::Artist>,
    tracks: Arc<[model::Track]>,
    albums: Arc<[model::Album]>,
    content_state: gpui_kit::ListState,
    content_layout: Option<(usize, usize, usize, Pixels, bool)>,
    content_columns: usize,
    pub(super) header_motion: Entity<artist_header::ArtistHeader>,
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
        let track_list = cx.new(|cx| TrackList::new(cx).with_inline_rows());
        let header_motion = cx.new(|cx| artist_header::ArtistHeader::new(cx));
        let content_state = gpui_kit::ListState::new(0, gpui_kit::ListAlignment::Top, px(256.));
        content_state.set_scroll_handler({
            let page = cx.weak_entity();
            move |_, window, cx| {
                let page = page.clone();
                let rem = window.rem_size();
                // Defer until the list releases its internal mutable borrow.
                // A weak page avoids a ListState -> handler -> ListState cycle.
                cx.defer(move |cx| {
                    let _ = page.update(cx, |page, cx| {
                        let offset = -page.content_state.scroll_px_offset_for_scrollbar().y / rem;
                        let available = page.content_state.max_offset_for_scrollbar().y / rem;
                        page.header_motion
                            .update(cx, |header, cx| header.scroll(offset, available, cx));
                    });
                });
            }
        });
        Self {
            backend,
            reference: None,
            artist: None,
            tracks: Arc::default(),
            albums: Arc::default(),
            content_state,
            header_motion,
            content_layout: None,
            content_columns: 1,
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
            self.header_motion.update(cx, |header, cx| header.reset(cx));
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
    fn content_item(
        &mut self,
        index: usize,
        columns: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if index == 0 {
            let header = self.header(window, cx);
            return if self.loaded && self.error.is_none() {
                div()
                    .relative()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(header)
                    .child(self.popular(cx))
                    .into_any_element()
            } else {
                header
            };
        }
        if !self.loaded {
            return div()
                .px_8()
                .child(components::list_skeleton(appearance::Appearance::palette(
                    cx,
                )))
                .into_any_element();
        }
        if let Some(error) = &self.error {
            return components::empty_state(
                appearance::Appearance::palette(cx),
                format!("Unable to load artist: {error}"),
            )
            .into_any_element();
        }
        let heading = |id, title| {
            div()
                .id(id)
                .test_support()
                .text_xl()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(rgb(appearance::Appearance::palette(cx).text_primary))
                .child(title)
        };
        if index == 1 {
            return div()
                .px_8()
                .w_full()
                .pt_6()
                .pb_3()
                .flex()
                .flex_col()
                .gap_3()
                .child(heading("artist-discography-heading", "Discography"))
                .when(self.albums.is_empty(), |section| {
                    section.child(components::page_detail(
                        appearance::Appearance::palette(cx),
                        "No releases available",
                    ))
                })
                .into_any_element();
        }
        let start = (index - 2) * columns;
        let end = (start + columns).min(self.albums.len());
        let mut cards = div()
            .px_8()
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

    fn popular(&self, cx: &App) -> AnyElement {
        let heading = |id, title| {
            div()
                .id(id)
                .test_support()
                .text_xl()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(rgb(appearance::Appearance::palette(cx).text_primary))
                .child(title)
        };
        div()
            .px_8()
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(heading("artist-popular-heading", "Popular"))
            .when(self.tracks.is_empty(), |section| {
                section.child(components::page_detail(
                    appearance::Appearance::palette(cx),
                    "No popular tracks available",
                ))
            })
            .when(!self.tracks.is_empty(), |section| {
                section.child(self.track_list.clone())
            })
            .into_any_element()
    }

    fn album_card(&self, album: &model::Album, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let palette = appearance::Appearance::palette(cx);
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

impl SearchPage {
    fn artist_card(&self, index: usize, width: Rems, cx: &mut Context<Self>) -> AnyElement {
        let palette = appearance::Appearance::palette(cx);
        let artist = &self.artists[index];
        let cache = services::AppServices::image_cache(cx);
        components::button(palette, ("search-artist", index))
            .test_support()
            .w(width + Rems(0.75))
            .min_w_0()
            .p_1p5()
            .flex_col()
            .items_start()
            .justify_start()
            .gap_2()
            .rounded_xl()
            .hover(|style| style.bg(rgb(palette.surface_hover)))
            .child(components::rectangular_artwork(
                palette,
                &cache,
                artist.artwork_url.as_deref(),
                tokens::SEARCH_ARTIST_ARTWORK,
                width,
                CadenceIcon::Person,
            ))
            .child(
                div()
                    .w_full()
                    .truncate()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(artist.name.clone()),
            )
            .child(components::page_detail(palette, "Artist"))
            // Capture just the index, resolving the current metadata on activation.
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(artist) = this.artists.get(index) {
                    cx.emit(PageEvent::OpenArtist(model::ArtistRef {
                        name: artist.name.clone(),
                        source_id: Some(artist.source_id.clone()),
                        spotify_uri: artist.spotify_uri.clone(),
                    }));
                }
            }))
            .into_any_element()
    }

    fn album_card(&self, index: usize, width: Rems, cx: &mut Context<Self>) -> AnyElement {
        let palette = appearance::Appearance::palette(cx);
        let album = &self.albums[index];
        let cache = services::AppServices::image_cache(cx);
        let mut detail = album
            .artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if let Some(year) = album.release_date.as_deref().and_then(|date| date.get(..4)) {
            if !detail.is_empty() {
                detail.push_str(" · ");
            }
            detail.push_str(year);
        }
        components::button(palette, ("search-album", index))
            .test_support()
            .w(width + Rems(0.75))
            .min_w_0()
            .p_1p5()
            .flex_col()
            .items_start()
            .justify_start()
            .gap_2()
            .rounded_xl()
            .hover(|style| style.bg(rgb(palette.surface_hover)))
            .child(components::artwork(
                palette,
                &cache,
                album.artwork_url.as_deref(),
                tokens::ArtworkSize {
                    size: width,
                    radius: Rems(0.625),
                },
                CadenceIcon::MusicNote,
            ))
            .child(
                div()
                    .w_full()
                    .truncate()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(album.name.clone()),
            )
            .child(components::page_detail(palette, detail).w_full().truncate())
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

impl Render for SearchPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let heading = |id, text| {
            div()
                .id(id)
                .test_support()
                .text_xl()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(rgb(palette.text_primary))
                .child(text)
        };
        let available = (self.content_width / window.rem_size()).max(1.);
        let artist_columns = if available < 26. {
            1
        } else {
            model::SEARCH_ARTIST_LIMIT
        };
        let album_columns = if available < 26. {
            2
        } else {
            model::SEARCH_ALBUM_LIMIT
        };
        let card_width = |columns: usize| {
            Rems(((available - (columns - 1) as f32) / columns as f32 - 0.75).max(1.))
        };
        let mut content = div().flex().flex_col().w_full().gap_6();
        if self.searching {
            content = content.child(components::list_skeleton(palette));
        } else if self.error.is_some() {
            content = content.child(components::empty_state(palette, "Unable to search Spotify"));
        } else if !self.loaded {
            content = content.child(components::empty_state(palette, "Press Return to search"));
        } else if self.artists.is_empty() && self.albums.is_empty() && self.tracks.is_empty() {
            content = content.child(components::empty_state(palette, "No results found"));
        } else {
            let mut artists = div().flex().flex_wrap().gap_4();
            for index in 0..self.artists.len() {
                artists = artists.child(self.artist_card(index, card_width(artist_columns), cx));
            }
            let mut albums = div().flex().flex_wrap().gap_4();
            for index in 0..self.albums.len() {
                albums = albums.child(self.album_card(index, card_width(album_columns), cx));
            }
            self.track_list.update(cx, |list, cx| {
                list.show(
                    (ElementId::from("search-tracks"), self.results_query.clone()),
                    self.tracks.clone(),
                    cx,
                );
            });
            content = content
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(heading("search-artists-heading", "Top Artists"))
                        .when(self.artists.is_empty(), |section| {
                            section.child(components::page_detail(palette, "No artists found"))
                        })
                        .child(artists),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(heading("search-albums-heading", "Top Albums"))
                        .when(self.albums.is_empty(), |section| {
                            section.child(components::page_detail(palette, "No albums found"))
                        })
                        .child(albums),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(heading("search-songs-heading", "Songs"))
                        .when(self.tracks.is_empty(), |section| {
                            section.child(components::page_detail(palette, "No songs found"))
                        })
                        .when(!self.tracks.is_empty(), |section| {
                            section.child(self.track_list.clone())
                        }),
                );
        }
        // All bounded sections share one scroll recipient. No per-card tasks,
        // nested scrolling, or discography/album-track prefetch is needed.
        div()
            .id("search-page")
            .size_full()
            .min_h_0()
            .relative()
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.text))
            .child(
                div()
                    .id("search-scroll")
                    .test_support()
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .p_8()
                            .pt_3()
                            .pr(px(48.))
                            .w_full()
                            .child(components::page_heading(
                                palette,
                                "Search results",
                                format!("Results for {}", self.query),
                            ))
                            .child(
                                content.relative().child(
                                    gpui_kit::canvas(
                                        {
                                            let page = cx.weak_entity();
                                            move |bounds, _, cx| {
                                                let width = bounds.size.width;
                                                if page
                                                    .read_with(cx, |page, _| {
                                                        page.content_width != width
                                                    })
                                                    .unwrap_or(false)
                                                {
                                                    let page = page.clone();
                                                    cx.defer(move |cx| {
                                                        let _ = page.update(cx, |page, cx| {
                                                            if page.content_width != width {
                                                                page.content_width = width;
                                                                cx.notify();
                                                            }
                                                        });
                                                    });
                                                }
                                            }
                                        },
                                        |_, _, _, _| {},
                                    )
                                    .absolute()
                                    .inset_0()
                                    .size_full(),
                                ),
                            ),
                    ),
            )
            .child(
                gpui_kit::base::Scrollbar::vertical(&self.scroll)
                    .viewport_from_layout()
                    .mode(gpui_kit::base::ScrollbarMode::Always),
            )
    }
}

impl Render for PlaylistPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tracks = self.tracks.clone();
        let loaded = self.loaded;
        let empty = tracks.borrow().is_empty();
        let has_tracks = tracks.borrow().tracks().next().is_some();
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
        let palette = appearance::Appearance::palette(cx);
        let list = if let Some(error) = self.error.as_deref().filter(|_| !has_tracks) {
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
            self.error.is_some(),
            |container| {
                container.child(
                    components::button(palette, "playlist-retry-page")
                        .test_support()
                        .h_10()
                        .child("Retry loading tracks")
                        .tooltip({
                            let error = self.error.clone().unwrap_or_default();
                            move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(error.clone())
                                    .build(window, cx)
                            }
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.load_more(cx);
                        })),
                )
            },
        );
        let playback_tracks = tracks;

        components::page("playlist-page")
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.text))
            .pt(DETAIL_HEADER_PADDING)
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

impl ArtistPage {
    fn header(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let palette = appearance::Appearance::palette(cx);
        let artwork_url = self
            .artist
            .as_ref()
            .and_then(|artist| artist.artwork_url.clone());
        let detail = if self.loaded && self.error.is_none() {
            format!("Spotify artist · {} releases", self.albums.len())
        } else {
            "Spotify artist".to_owned()
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
        let header_height = (tokens::ARTIST_ARTWORK.size + DETAIL_HEADER_PADDING * 2.)
            .to_pixels(window.rem_size())
            + overlay_inset;
        let backdrop_height = header_height + Rems(2.875).to_pixels(window.rem_size());
        // One full-size gradient avoids joins between separately rasterized
        // bands at fractional Windows DPI/zoom. Finish early on page colour;
        // the borderless end fill below handles renderer colour rounding.
        let background = div()
            .id("artist-backdrop")
            .test_support()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(backdrop_height)
            .overflow_hidden()
            .children(backdrop.map(|image| div().absolute().inset_0().size_full().child(image)))
            .child(div().absolute().inset_0().bg(gpui_kit::rgba(0x10101455)))
            .child(div().absolute().inset_0().bg(gpui_kit::linear_gradient(
                180.,
                gpui_kit::linear_color_stop(rgb(palette.surface).opacity(0.), 0.58),
                gpui_kit::linear_color_stop(rgb(palette.surface), 0.96),
            )))
            // Keep both fills unrounded and borderless: Windows can use its
            // hard-edged quad path instead of antialiasing over the image.
            // This fill starts after the fade is already opaque.
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(2.))
                    .bg(rgb(palette.surface)),
            );
        let header = div()
            .id("artist-header")
            .test_support()
            .relative()
            .w_full()
            .h(header_height)
            .flex_none()
            .child(background)
            .child(
                div()
                    .absolute()
                    .left(Rems(13.5))
                    .bottom(DETAIL_HEADER_PADDING)
                    .child(components::page_detail(palette, detail)),
            );
        header.into_any_element()
    }
}

impl Render for ArtistPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let name = self
            .artist
            .as_ref()
            .map(|a| a.name.clone())
            .or_else(|| self.reference.as_ref().map(|a| a.name.clone()))
            .unwrap_or_else(|| "Artist".into());
        let artwork = self.artist.as_ref().and_then(|a| a.artwork_url.clone());
        self.header_motion.update(cx, |header, cx| {
            if header.name != name || header.artwork != artwork {
                header.name = name;
                header.artwork = artwork;
                cx.notify();
            }
        });
        let columns = self.content_columns;
        let layout = (
            self.tracks.len(),
            self.albums.len(),
            columns,
            window.rem_size(),
            self.loaded && self.error.is_none(),
        );
        if self.content_layout != Some(layout) {
            let count = if layout.4 {
                2 + self.albums.len().div_ceil(columns)
            } else {
                2
            };
            if let Some(old) = self.content_layout.filter(|old| old.4 && layout.4) {
                // Resizing, zooming or refreshing a loaded page must not reset
                // to its header. Anchor the same release when columns change.
                let mut anchor = self.content_state.logical_scroll_top();
                if anchor.item_ix >= 2 {
                    anchor.item_ix = 2 + (anchor.item_ix - 2) * old.2 / columns;
                }
                anchor.item_ix = anchor.item_ix.min(count.saturating_sub(1));
                anchor.offset_in_item = anchor.offset_in_item / old.3 * layout.3;
                let previous_count = self.content_state.item_count();
                if previous_count != count {
                    self.content_state.splice(2..previous_count, count - 2);
                }
                self.content_state.remeasure();
                self.content_state.scroll_to(anchor);
            } else {
                self.content_state.reset(count);
                self.header_motion.update(cx, |header, cx| header.reset(cx));
            }
            self.content_layout = Some(layout);
        }
        let source_id = self
            .reference
            .as_ref()
            .and_then(|a| a.source_id.clone())
            .unwrap_or_default();
        self.track_list.update(cx, |list, cx| {
            list.show(
                (ElementId::from("artist-popular"), source_id),
                self.tracks.clone(),
                cx,
            )
        });
        let content = gpui_kit::list(
            self.content_state.clone(),
            cx.processor(move |this, index, window, cx| {
                this.content_item(index, columns, window, cx)
            }),
        )
        .flex_1()
        .min_h_0();
        div()
            .id("artist-page")
            .relative()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.text))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .flex()
                    .flex_col()
                    .child(content)
                    // Measure the actual content width, including sidebar animation
                    // and zoom. Only a column-count change schedules another layout.
                    .child(
                        gpui_kit::canvas(
                            {
                                let page = cx.weak_entity();
                                let self_header = self.header_motion.clone();
                                move |bounds, window, cx| {
                                    let origin = bounds.origin;
                                    let width = bounds.size.width;
                                    let geometry_changed = {
                                        let header = self_header.read(cx);
                                        header.origin != origin || header.viewport_width != width
                                    };
                                    if geometry_changed {
                                        let header = self_header.clone();
                                        cx.defer(move |cx| {
                                            header.update(cx, |header, cx| {
                                                if header.origin != origin
                                                    || header.viewport_width != width
                                                {
                                                    header.origin = origin;
                                                    header.viewport_width = width;
                                                    cx.notify();
                                                }
                                            });
                                        });
                                    }
                                    let gap = Rems(0.5).to_pixels(window.rem_size());
                                    let stride = (tokens::ALBUM_CARD_ARTWORK.size + Rems(1.5))
                                        .to_pixels(window.rem_size());
                                    let available =
                                        bounds.size.width - Rems(4.).to_pixels(window.rem_size());
                                    let fits =
                                        ((available + gap) / stride).floor().max(1.) as usize;
                                    if fits != columns {
                                        let page = page.clone();
                                        cx.defer(move |cx| {
                                            let _ = page.update(cx, |page, cx| {
                                                if page.content_columns != fits {
                                                    page.content_columns = fits;
                                                    cx.notify();
                                                }
                                            });
                                        });
                                    }
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0()
                        .size_full(),
                    ),
            )
            .child(self.header_motion.clone())
    }
}

impl Render for AlbumPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let palette = appearance::Appearance::palette(cx);
        let mut detail = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .text_size(tokens::BODY_TEXT)
            .text_color(rgb(palette.text_muted));
        if let Some(album) = &self.album {
            for (index, artist) in album.artists.iter().enumerate() {
                if index > 0 {
                    detail = detail.child(",");
                }
                detail = detail.child(if artist.source_id.is_some() {
                    components::button(palette, ("album-artist", index))
                        .test_support()
                        .min_w_0()
                        .hover(|style| style.underline().text_color(rgb(palette.text_primary)))
                        .child(artist.name.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(artist) = this
                                .album
                                .as_ref()
                                .and_then(|album| album.artists.get(index))
                            {
                                cx.emit(PageEvent::OpenArtist(artist.clone()));
                            }
                        }))
                        .into_any_element()
                } else {
                    div().child(artist.name.clone()).into_any_element()
                });
            }
            if let Some(year) = album.release_date.as_deref().and_then(|date| date.get(..4)) {
                if !album.artists.is_empty() {
                    detail = detail.child("·");
                }
                detail = detail.child(year.to_owned());
            }
            if let Some(count) = album.track_count {
                if !album.artists.is_empty() || album.release_date.is_some() {
                    detail = detail.child("·");
                }
                detail = detail.child(format!("{count} tracks"));
            }
        } else {
            detail = detail.child("Spotify album");
        }
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
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.text))
            .pt(DETAIL_HEADER_PADDING)
            .child(
                page_header()
                    .child(
                        div()
                            .flex_none()
                            .rounded(Rems(1.75))
                            .shadow(vec![
                                gpui_kit::BoxShadow::new(
                                    px(0.),
                                    px(10.),
                                    gpui_kit::rgba(0x00000055).into(),
                                )
                                .blur_radius(px(24.)),
                                gpui_kit::BoxShadow::new(
                                    px(0.),
                                    px(2.),
                                    gpui_kit::rgba(0x00000033).into(),
                                )
                                .blur_radius(px(6.)),
                            ])
                            .child(components::artwork(
                                palette,
                                &self.image_cache,
                                artwork_url.as_deref(),
                                tokens::COLLECTION_ARTWORK,
                                CadenceIcon::MusicNote,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_start()
                            .gap_2()
                            .child(components::page_title(palette, name))
                            .child(detail)
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

/// The artwork-and-title band a detail page opens with.
fn page_header() -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_7()
        .mb(DETAIL_HEADER_PADDING)
}

#[cfg(test)]
mod retained_page_tests {
    use super::*;
    #[test]
    fn clearing_or_replacing_search_releases_the_hidden_track_list_and_all_result_arrays() {
        let mut cx = gpui_kit::HeadlessAppContext::with_asset_source(
            Arc::new(gpui_kit::NoopTextSystem),
            Arc::new(assets::CadenceAssets),
        );
        cx.update(|cx| {
            let _probe = crate::app::test_support::initialize(cx, ThemePreference::Dark);
            let page = cx.new(|cx| SearchPage::new(services::AppServices::backend(cx), cx));
            for replacing in [false, true] {
                let tracks: Arc<[model::Track]> = vec![crate::app::test_support::track(0)].into();
                let weak = Arc::downgrade(&tracks);
                let results = crate::app::test_support::gallery_results();
                let artists: Arc<[model::Artist]> = results.artists.into();
                let albums: Arc<[model::Album]> = results.albums.into();
                let weak_artists = Arc::downgrade(&artists);
                let weak_albums = Arc::downgrade(&albums);
                page.update(cx, |page, cx| {
                    page.artists = artists;
                    page.albums = albums;
                    page.tracks = tracks;
                    page.track_list
                        .update(cx, |list, cx| list.show("search", page.tracks.clone(), cx));
                    if replacing {
                        page.set_query("next".into(), cx);
                        assert!(page.submit(cx));
                    } else {
                        page.clear(cx);
                    }
                });
                assert!(
                    weak.upgrade().is_none(),
                    "hidden track list retained a cleared search"
                );
                assert!(weak_artists.upgrade().is_none());
                assert!(weak_albums.upgrade().is_none());
            }
        });
    }

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
