use super::*;
use gpui_kit::TestSupportExt as _;
use std::cell::RefCell;

use page::PageEvent;

/// A virtualized table of tracks with a per-row action menu.
///
/// The list owns which row's menu is open, and a click landing anywhere else
/// dismisses it. Dismissal that no click drives -- a keyboard route change, a
/// scroll outside the table -- is the workspace's to trigger, via
/// `close_menu`, because the menu outlives the page going off screen.
pub(super) struct LoadMoreTracks(pub Range<usize>);

pub(super) struct TrackList {
    /// Set by `show`, which always runs before the list is first painted.
    id: Option<ElementId>,
    tracks: Arc<[model::Track]>,
    playlist_tracks: Option<Rc<RefCell<playlist_buffer::PlaylistBuffer>>>,
    shown_playlist_len: usize,
    /// The row whose action menu is open, keyed by source ID and row index so
    /// the same track appearing twice opens only the row that was clicked.
    menu_open: Option<String>,
    inline_rows: bool,
    queue_mode: bool,
    hide_header: bool,
    playlist_context: Option<(model::Playlist, Option<u32>)>,
    last_viewport: Option<Range<usize>>,
    scroll: gpui_kit::UniformListScrollHandle,
    hover: components::HoverHighlight,
    menu_position: Option<gpui_kit::Point<Pixels>>,
    /// Album already on screen, whose "Go to album" entry would go nowhere.
    current_album_id: Option<String>,
    library: Entity<library::Library>,
    player: Entity<player::Player>,
    image_cache: Entity<image_cache::BoundedImageCache>,
}

impl EventEmitter<PageEvent> for TrackList {}
impl EventEmitter<LoadMoreTracks> for TrackList {}

impl TrackList {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            id: None,
            tracks: Arc::default(),
            playlist_tracks: None,
            shown_playlist_len: 0,
            menu_open: None,
            inline_rows: false,
            queue_mode: false,
            hide_header: false,
            playlist_context: None,
            last_viewport: None,
            scroll: gpui_kit::UniformListScrollHandle::new(),
            hover: components::HoverHighlight::default(),
            menu_position: None,
            current_album_id: None,
            library: services::AppServices::library(cx),
            player: services::AppServices::player(cx),
            image_cache: services::AppServices::image_cache(cx),
        }
    }

    pub(super) fn for_queue(mut self) -> Self {
        self.queue_mode = true;
        self.hide_header = true;
        self
    }

    /// Artist top tracks are a bounded section inside the page's outer list.
    /// Avoid a nested scroll area so wheel input continues into Discography.
    pub(super) fn with_inline_rows(mut self) -> Self {
        self.inline_rows = true;
        self
    }

    pub(super) fn without_header(mut self) -> Self {
        self.hide_header = true;
        self
    }

    pub(super) fn scroll_handle(&self) -> gpui_kit::UniformListScrollHandle {
        self.scroll.clone()
    }

    pub(super) fn restore_scroll(
        &mut self,
        scroll: gpui_kit::UniformListScrollHandle,
        cx: &mut Context<Self>,
    ) {
        self.scroll = scroll;
        cx.notify();
    }

    /// Shows `tracks` under `id`, which pages vary per playlist or album so
    /// that opening a different one starts back at the top of the list.
    ///
    /// Pages call this from `render`, so the early return below is what keeps
    /// the notify cycle finite: callers must pass a stored `Arc` clone, not a
    /// slice rebuilt every frame, or every render schedules another one.
    pub(super) fn show(
        &mut self,
        id: impl Into<ElementId>,
        tracks: Arc<[model::Track]>,
        cx: &mut Context<Self>,
    ) {
        let id = Some(id.into());
        if self.playlist_tracks.is_none() && self.id == id && Arc::ptr_eq(&self.tracks, &tracks) {
            return;
        }
        if self.id != id {
            self.scroll = gpui_kit::UniformListScrollHandle::new();
        }
        self.id = id;
        self.playlist_tracks = None;
        self.tracks = tracks;
        self.last_viewport = None;
        self.menu_open = None;
        cx.notify();
    }

    /// Playlist pagination appends to one UI-thread buffer. The list reads only
    /// visible rows, avoiding a full metadata copy for each fifteen-entry page.
    pub(super) fn show_playlist(
        &mut self,
        id: impl Into<ElementId>,
        tracks: Rc<RefCell<playlist_buffer::PlaylistBuffer>>,
        cx: &mut Context<Self>,
    ) {
        let id = Some(id.into());
        let len = tracks.borrow().revision;
        if self.id == id
            && self
                .playlist_tracks
                .as_ref()
                .is_some_and(|old| Rc::ptr_eq(old, &tracks))
            && self.shown_playlist_len == len
        {
            return;
        }
        if self.id != id {
            self.scroll = gpui_kit::UniformListScrollHandle::new();
        }
        self.id = id;
        self.tracks = Arc::default();
        self.playlist_tracks = Some(tracks);
        self.shown_playlist_len = len;
        self.last_viewport = None;
        self.menu_open = None;
        cx.notify();
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.playlist_tracks = None;
        self.tracks = Arc::default();
        self.id = None;
        self.menu_open = None;
        self.playlist_context = None;
        self.hover = components::HoverHighlight::default();
        cx.notify();
    }

    fn track_count(&self) -> usize {
        self.playlist_tracks
            .as_ref()
            .map_or(self.tracks.len(), |tracks| tracks.borrow().len())
    }

    /// Suppresses the "Go to album" entry for the album the page is showing.
    pub(super) fn set_current_album_id(
        &mut self,
        source_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.current_album_id != source_id {
            self.current_album_id = source_id;
            cx.notify();
        }
    }

    pub(super) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu_open.take().is_some() {
            cx.notify();
        }
    }

    fn row(&mut self, index: usize, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let track = if let Some(tracks) = &self.playlist_tracks {
            tracks.borrow().get(index).cloned()
        } else {
            self.tracks.get(index).cloned()
        };
        let Some(track) = track else {
            let palette = appearance::Appearance::palette(cx);
            let unavailable = self
                .playlist_tracks
                .as_ref()
                .is_some_and(|tracks| tracks.borrow().contains(index));
            return div()
                .h_16()
                .w_full()
                .when(unavailable, |row| {
                    row.px_3()
                        .flex()
                        .items_center()
                        .text_color(rgb(palette.text_muted))
                        .child(format!("{}   Unavailable track", index + 1))
                })
                .when(!unavailable, |row| {
                    row.child(components::list_skeleton_rows(palette, 1))
                })
                .into_any_element();
        };
        let palette = appearance::Appearance::palette(cx);
        let is_current_track = self.player.read(cx).is_current_track(&track);
        let favorite = self.library.read(cx).is_favorite(&track);
        let menu_key = format!("{}:{index}", track.source_id);
        let menu_open = self.menu_open.as_deref() == Some(menu_key.as_str());
        let menu = menu_open
            .then(|| self.action_menu(&track, index, favorite, is_current_track, cx))
            .map(IntoElement::into_any_element);
        // Share one small identity across callbacks, rather than a full Track.
        // Input from a stale frame must never act on a different replacement row.
        let source_id: Rc<str> = Rc::from(track.source_id.as_str());
        let provider = track.provider;
        let can_navigate = track
            .artists
            .iter()
            .any(|artist| artist.source_id.is_some())
            || track
                .album_ref
                .as_ref()
                .is_some_and(|album| album.source_id.is_some());
        track_row::TrackRow::new(index, track, palette, self.image_cache.clone())
            .compact(compact)
            .when(can_navigate, |row| {
                let source_id = source_id.clone();
                row.navigation(cx.listener(
                    move |this, target: &track_row::TrackNavigation, _, cx| {
                        this.navigate_row(index, provider, &source_id, *target, cx);
                    },
                ))
            })
            .current(is_current_track)
            .playback_control(
                self.player.read(cx).playing(),
                cx.listener(|this, _, _, cx| {
                    this.player.update(cx, |player, cx| player.toggle(cx));
                }),
            )
            .favorite(favorite)
            .hover_transition(
                self.hover.transition(index, cx.background_executor().now()),
                cx.listener(move |this, hovered: &bool, _, cx| {
                    this.hover
                        .set(index, *hovered, cx.background_executor().now());
                    cx.notify();
                }),
            )
            .menu(menu)
            .on_play(cx.listener(move |this, _, _, cx| this.play_from(index, cx)))
            .on_favorite(cx.listener(move |this, _, _, cx| {
                // The full metadata copy is needed only when sending a write,
                // not whenever scrolling or hover repaints this visible row.
                let track = if let Some(buffer) = &this.playlist_tracks {
                    buffer
                        .borrow()
                        .get(index)
                        .filter(|track| {
                            track.provider == provider && track.source_id == source_id.as_ref()
                        })
                        .cloned()
                } else {
                    this.tracks
                        .get(index)
                        .filter(|track| {
                            track.provider == provider && track.source_id == source_id.as_ref()
                        })
                        .cloned()
                };
                if let Some(track) = track {
                    this.library
                        .update(cx, |library, cx| library.set_favorite(track, !favorite, cx));
                }
            }))
            .context_menu(
                self.menu_position,
                cx.listener(move |this, event: &gpui_kit::MouseDownEvent, _, cx| {
                    this.menu_open = Some(menu_key.clone());
                    this.menu_position = Some(event.position);
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    /// Resolve references only on activation. Painting links captures small
    /// indices, rather than cloning artist/album metadata on every hover frame.
    fn navigate_row(
        &mut self,
        index: usize,
        provider: model::Provider,
        source_id: &str,
        target: track_row::TrackNavigation,
        cx: &mut Context<Self>,
    ) {
        let event_for = |track: &model::Track| {
            if track.provider != provider || track.source_id != source_id {
                return None;
            }
            match target {
                track_row::TrackNavigation::Artist(index) => track
                    .artists
                    .get(index)
                    .filter(|artist| artist.source_id.is_some())
                    .cloned()
                    .map(PageEvent::OpenArtist),
                track_row::TrackNavigation::Album => track
                    .album_ref
                    .as_ref()
                    .filter(|album| album.source_id.is_some())
                    .cloned()
                    .map(PageEvent::OpenAlbum),
            }
        };
        let event = if let Some(buffer) = &self.playlist_tracks {
            buffer.borrow().get(index).and_then(event_for)
        } else {
            self.tracks.get(index).and_then(event_for)
        };
        if let Some(event) = event {
            self.menu_open = None;
            cx.emit(event);
        }
    }

    pub(super) fn set_playlist_context(
        &mut self,
        playlist: model::Playlist,
        next_offset: Option<u32>,
        loading: bool,
    ) {
        self.playlist_context = Some((playlist, next_offset));
        let _ = loading;
    }

    fn play_from(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.queue_mode {
            self.player
                .update(cx, |player, cx| player.play_queue_item(index, cx));
            return;
        }
        if let (Some(buffer), Some((playlist, _))) = (&self.playlist_tracks, &self.playlist_context)
        {
            if let Some((tracks, index, next)) = buffer.borrow().playback(index) {
                self.player.update(cx, |player, cx| {
                    player.play_playlist(playlist.clone(), tracks, index, next, cx)
                });
            }
        } else {
            let tracks = self.tracks.to_vec();
            self.player
                .update(cx, |player, cx| player.play_context(tracks, index, cx));
        }
    }

    fn action_menu(
        &self,
        track: &model::Track,
        index: usize,
        favorite: bool,
        is_current_track: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let palette = appearance::Appearance::palette(cx);
        let has_playback_context = self.player.read(cx).now_playing().is_some();
        let next_track = track.clone();
        let queue_track = track.clone();
        let radio_track = track.clone();
        let favorite_track = track.clone();
        let spotify_track = track.clone();
        let spotify_liked = self.library.read(cx).is_spotify_liked(track);
        let spotify_like_busy = self.library.read(cx).spotify_like_busy();
        let artist = track
            .artists
            .iter()
            .find(|artist| artist.source_id.is_some())
            .cloned();
        let album = track
            .album_ref
            .clone()
            .filter(|album| album.source_id.is_some())
            .filter(|album| album.source_id != self.current_album_id);
        let track_url = format!("https://open.spotify.com/track/{}", track.source_id);
        let play_id = ("track-menu-play", index);
        let next_id = ("track-menu-next", index);
        let queue_id = ("track-menu-queue", index);
        let separator = || {
            div()
                .mx_1()
                .my_1()
                .border_t_1()
                .border_color(rgb(palette.border))
        };

        components::menu_surface(palette)
            .on_mouse_up_out(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_menu(cx)),
            )
            .child(
                if is_current_track {
                    components::disabled_text_menu_item(palette, play_id, "Play now")
                } else {
                    components::text_menu_item(palette, play_id, "Play now").on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.menu_open = None;
                            this.play_from(index, cx);
                        },
                    ))
                }
                .test_support(),
            )
            .child(
                if has_playback_context {
                    components::text_menu_item(palette, next_id, "Play next").on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.menu_open = None;
                            this.player
                                .update(cx, |player, cx| player.play_next(next_track.clone(), cx));
                            cx.notify();
                        },
                    ))
                } else {
                    components::disabled_text_menu_item(palette, next_id, "Play next")
                }
                .test_support(),
            )
            .child(if has_playback_context {
                components::text_menu_item(palette, queue_id, "Add to queue").on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.menu_open = None;
                        this.player.update(cx, |player, cx| {
                            player.append_to_queue(queue_track.clone(), cx)
                        });
                        cx.notify();
                    },
                ))
            } else {
                components::disabled_text_menu_item(palette, queue_id, "Add to queue")
            })
            .child(
                components::text_menu_item(
                    palette,
                    ("track-menu-radio", index),
                    "Start track radio",
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu_open = None;
                    cx.emit(PageEvent::StartRadio(radio_track.clone()));
                    cx.notify();
                })),
            )
            .child(separator())
            .child(
                components::text_menu_item(
                    palette,
                    ("track-menu-favorite", index),
                    if favorite {
                        "Remove from favorites"
                    } else {
                        "Add to favorites"
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu_open = None;
                    this.library.update(cx, |library, cx| {
                        library.set_favorite(favorite_track.clone(), !favorite, cx)
                    });
                    cx.notify();
                })),
            )
            .when(track.provider == model::Provider::Spotify, |menu| {
                let label = if spotify_like_busy {
                    "Updating Spotify library…"
                } else if spotify_liked {
                    "Remove from Liked Songs"
                } else {
                    "Add to Liked Songs"
                };
                if spotify_like_busy {
                    menu.child(components::disabled_text_menu_item(
                        palette,
                        ("track-menu-spotify-like", index),
                        label,
                    ))
                } else {
                    menu.child(
                        components::text_menu_item(
                            palette,
                            ("track-menu-spotify-like", index),
                            label,
                        )
                        .test_support()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.menu_open = None;
                            this.library.update(cx, |library, cx| {
                                library.set_spotify_liked(spotify_track.clone(), !spotify_liked, cx)
                            });
                            cx.notify();
                        })),
                    )
                }
            })
            .child(separator())
            .when_some(artist, |menu, artist| {
                menu.child(
                    components::text_menu_item(
                        palette,
                        ("track-menu-artist", index),
                        "Go to artist",
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu_open = None;
                        cx.emit(PageEvent::OpenArtist(artist.clone()));
                    })),
                )
            })
            .when_some(album, |menu, album| {
                menu.child(
                    components::text_menu_item(palette, ("track-menu-album", index), "Go to album")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.menu_open = None;
                            cx.emit(PageEvent::OpenAlbum(album.clone()));
                        })),
                )
            })
            .child(
                components::text_menu_item(
                    palette,
                    ("track-menu-spotify", index),
                    "Open track in Spotify",
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu_open = None;
                    cx.open_url(&track_url);
                    cx.notify();
                })),
            )
    }
}

impl Render for TrackList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let compact = self.queue_mode || is_compact_content_layout(window);
        if self.inline_rows {
            let rows = (0..self.track_count())
                .map(|index| self.row(index, compact, cx))
                .collect::<Vec<_>>();
            return div()
                .id("track-list")
                .w_full()
                .flex_none()
                .flex()
                .flex_col()
                .children(rows);
        }
        div()
            .id("track-list")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .relative()
            .when(!self.hide_header, |list| {
                list.child(
                    div()
                        .pr(px(16.))
                        .child(track_row::track_list_header(palette, compact)),
                )
            })
            .child(
                div()
                    .id("track-scroll-viewport")
                    .test_support()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list(
                            self.id
                                .clone()
                                .expect("show sets the id before first paint"),
                            self.track_count(),
                            cx.processor(move |this, range: Range<usize>, _, cx| {
                                range
                                    .map(|index| {
                                        if index < this.track_count() {
                                            this.row(index, compact, cx)
                                        } else {
                                            div()
                                                .h_16()
                                                .w_full()
                                                .child(components::list_skeleton_rows(palette, 1))
                                                .into_any_element()
                                        }
                                    })
                                    .collect()
                            }),
                        )
                        .when(self.playlist_tracks.is_some(), |list| {
                            list.with_decoration(PlaylistViewport {
                                list: cx.entity().downgrade(),
                            })
                        })
                        .track_scroll(&self.scroll)
                        .flex_1()
                        .min_h_0()
                        .mr(px(16.)),
                    )
                    .child(
                        gpui_kit::base::Scrollbar::vertical(&self.scroll)
                            .viewport_from_layout()
                            .mode(gpui_kit::base::ScrollbarMode::Always),
                    ),
            )
    }
}

// GPUI invokes the row renderer while measuring row zero as well as while
// drawing the viewport. Observe the actual visible range here, so measurement
// cannot reset the debounce or accidentally fetch page zero on a distant jump.
struct PlaylistViewport {
    list: gpui_kit::WeakEntity<TrackList>,
}
impl gpui_kit::UniformListDecoration for PlaylistViewport {
    fn compute(
        &self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: gpui_kit::Point<Pixels>,
        _: Pixels,
        _: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let _ = self.list.update(cx, |list, cx| {
            if list.last_viewport.as_ref() != Some(&range) {
                list.last_viewport = Some(range.clone());
                cx.emit(LoadMoreTracks(range));
            }
        });
        div().into_any_element()
    }
}

/// A virtualized table of playlists.
pub(super) struct PlaylistList {
    /// Set by `show`, which always runs before the list is first painted.
    id: Option<ElementId>,
    playlists: Arc<[model::Playlist]>,
    image_cache: Entity<image_cache::BoundedImageCache>,
}

impl EventEmitter<PageEvent> for PlaylistList {}

impl PlaylistList {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            id: None,
            playlists: Arc::default(),
            image_cache: services::AppServices::image_cache(cx),
        }
    }

    /// Shows `playlists` under `id`, which pages vary per content so that a
    /// new set starts back at the top. Same render-time contract as
    /// [`TrackList::show`]: pass a stored `Arc` clone, not a fresh slice.
    pub(super) fn show(
        &mut self,
        id: impl Into<ElementId>,
        playlists: Arc<[model::Playlist]>,
        cx: &mut Context<Self>,
    ) {
        let id = Some(id.into());
        if self.id == id && Arc::ptr_eq(&self.playlists, &playlists) {
            return;
        }
        self.id = id;
        self.playlists = playlists;
        cx.notify();
    }
}

impl Render for PlaylistList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                uniform_list(
                    self.id
                        .clone()
                        .expect("show sets the id before first paint"),
                    self.playlists.len(),
                    cx.processor(move |this, range: Range<usize>, _, cx| {
                        let playlists = this.playlists.clone();
                        range
                            .filter_map(|index| {
                                playlists.get(index).cloned().map(|playlist| {
                                    let selected = playlist.clone();
                                    track_row::PlaylistRow::new(
                                        index,
                                        playlist,
                                        palette,
                                        this.image_cache.clone(),
                                    )
                                    .on_open(cx.listener(move |_, _, _, cx| {
                                        cx.emit(PageEvent::OpenPlaylist(selected.clone()));
                                    }))
                                    .into_any_element()
                                })
                            })
                            .collect()
                    }),
                )
                .flex_1()
                .min_h_0(),
            )
    }
}
