use super::*;
use gpui_kit::TestSupportExt as _;

use gpui_kit::ClickEvent;

/// Breathing room between the Title column and whatever follows it.
const COLUMN_GUTTER: Rems = Rems(1.);
/// The fixed columns; Title and Album flex to share whatever remains.
const INDEX_COLUMN_WIDTH: Rems = Rems(2.75);
const STAR_COLUMN_WIDTH: Rems = Rems(2.25);
const TIME_COLUMN_WIDTH: Rems = Rems(3.75);
/// How far the actions menu drops below its trigger, and the room it keeps
/// from the window edges.
const ACTIONS_MENU_OFFSET: Rems = Rems(0.25);
const ACTIONS_MENU_WINDOW_MARGIN: Rems = Rems(0.5);

/// The column header for a track list. Lives beside `TrackRow` so the fixed
/// columns cannot drift out of step with the rows they label.
pub(super) fn track_list_header(palette: CadencePalette, compact: bool) -> impl IntoElement {
    div()
        .id("track-list-header")
        .test_support()
        .h_10()
        .flex_none()
        .px_3()
        .flex()
        .items_center()
        .text_size(tokens::BODY_TEXT)
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(rgb(palette.text_muted))
        .child(
            div()
                .w(INDEX_COLUMN_WIDTH)
                .flex_none()
                .text_center()
                .child("#"),
        )
        .child(div().flex_1().min_w_0().pr(COLUMN_GUTTER).child("Title"))
        .when(!compact, |header| {
            header.child(div().flex_1().min_w_0().child("Album"))
        })
        .child(
            div()
                .w(STAR_COLUMN_WIDTH)
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(components::icon(
                    CadenceIcon::Star,
                    tokens::COLUMN_HEADER_ICON,
                    palette.text_muted,
                )),
        )
        .child(
            div()
                .w(TIME_COLUMN_WIDTH)
                .flex_none()
                .flex()
                .items_center()
                .justify_end()
                .pr_2()
                .child("Time"),
        )
}

/// A single line that ellipsizes at the column edge. Wrapping text with a
/// one-line clamp rather than `.truncate()`: GPUI's text-measure cache
/// never recomputes truncation for nowrap text first measured at indefinite
/// width (as happens inside nested flex), while a wrap-width change between
/// measure passes forces the recompute.
fn ellipsized_line() -> Div {
    div().text_ellipsis().line_clamp(1)
}

#[derive(Clone, Copy)]
pub(super) enum TrackNavigation {
    Artist(usize),
    Album,
}

type NavigationCallback = Rc<dyn Fn(&TrackNavigation, &mut Window, &mut App)>;

type RowCallback = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
type HoverCallback = Box<dyn Fn(&bool, &mut Window, &mut App) + 'static>;
type ContextCallback = Box<dyn Fn(&gpui_kit::MouseDownEvent, &mut Window, &mut App) + 'static>;

/// One track in a list.
///
/// Stateless: the list that shows it decides whether it is current, favorited
/// or has its menu open, and supplies the behaviour as callbacks. The row never
/// reaches for the player or the library itself.
#[derive(IntoElement)]
pub(super) struct TrackRow {
    index: usize,
    track: model::Track,
    palette: CadencePalette,
    image_cache: Entity<image_cache::BoundedImageCache>,
    compact: bool,
    dense: bool,
    hide_artwork: bool,
    current: bool,
    playing: bool,
    on_toggle_playback: Option<RowCallback>,
    favorite: bool,
    /// Anchored to the pointer while the context menu is open.
    menu: Option<AnyElement>,
    on_play: Option<RowCallback>,
    on_navigate: Option<NavigationCallback>,
    on_favorite: Option<RowCallback>,
    on_context_menu: Option<ContextCallback>,
    hover_transition: Option<components::HoverTransition>,
    on_hover: Option<HoverCallback>,
    menu_position: Option<gpui_kit::Point<Pixels>>,
}

impl TrackRow {
    pub(super) fn new(
        index: usize,
        track: model::Track,
        palette: CadencePalette,
        image_cache: Entity<image_cache::BoundedImageCache>,
    ) -> Self {
        Self {
            index,
            track,
            palette,
            image_cache,
            compact: false,
            dense: false,
            hide_artwork: false,
            current: false,
            playing: false,
            on_toggle_playback: None,
            favorite: false,
            menu: None,
            on_play: None,
            on_navigate: None,
            on_favorite: None,
            on_context_menu: None,
            hover_transition: None,
            on_hover: None,
            menu_position: None,
        }
    }

    pub(super) fn hover_transition(
        mut self,
        transition: Option<components::HoverTransition>,
        callback: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.hover_transition = transition;
        self.on_hover = Some(Box::new(callback));
        self
    }

    pub(super) fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    pub(super) fn hide_artwork(mut self, hide: bool) -> Self {
        self.hide_artwork = hide;
        self
    }

    pub(super) fn dense(mut self, dense: bool) -> Self {
        self.dense = dense;
        self
    }

    /// Marks the row as the track currently playing, which also stops it
    /// responding to clicks.
    pub(super) fn current(mut self, current: bool) -> Self {
        self.current = current;
        self
    }

    pub(super) fn playback_control(
        mut self,
        playing: bool,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.playing = playing;
        self.on_toggle_playback = Some(Rc::new(handler));
        self
    }

    pub(super) fn favorite(mut self, favorite: bool) -> Self {
        self.favorite = favorite;
        self
    }

    pub(super) fn menu(mut self, menu: Option<AnyElement>) -> Self {
        self.menu = menu;
        self
    }

    pub(super) fn on_play(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_play = Some(Rc::new(handler));
        self
    }

    pub(super) fn navigation(
        mut self,
        handler: impl Fn(&TrackNavigation, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_navigate = Some(Rc::new(handler));
        self
    }

    fn navigation_link(
        &self,
        id: impl Into<ElementId>,
        target: TrackNavigation,
        text: AnyElement,
        window: &Window,
        keyboard: bool,
    ) -> AnyElement {
        let Some(handler) = self.on_navigate.clone() else {
            return text;
        };
        components::link(self.palette, id, window)
            .min_w_0()
            .justify_start()
            .tab_stop(keyboard)
            .on_click(move |event, window, cx| {
                cx.stop_propagation();
                if event.is_keyboard() || event.click_count() == 1 {
                    handler(&target, window, cx);
                }
            })
            .child(text)
            .into_any_element()
    }

    fn artist_line(&self, window: &Window) -> AnyElement {
        let text = |name: SharedString| ellipsized_line().child(name).into_any_element();
        if self.track.artists.is_empty() {
            return text(self.track.artist.clone());
        }
        let mut next = next_artist_credit(&self.track, 0);
        if next.is_none() {
            return text(self.track.artist.clone());
        }
        let fragment = |value: &str| {
            if value == ", " {
                SharedString::new_static(", ")
            } else {
                value.to_owned().into()
            }
        };
        let mut credits = div()
            .id(("track-artists", self.index))
            .flex()
            .min_w_0()
            .overflow_hidden();
        let mut cursor = 0;
        // Preserve the displayed credit verbatim, even when older metadata
        // contains references for only some collaborators. Match whole credits,
        // including artists whose actual names contain commas.
        while let Some((start, index)) = next {
            if start > cursor {
                credits = credits.child(text(fragment(&self.track.artist[cursor..start])));
            }
            let artist = &self.track.artists[index];
            credits = credits.child(self.navigation_link(
                ("track-artist", index),
                TrackNavigation::Artist(index),
                text(artist.name.clone()),
                window,
                true,
            ));
            cursor = start + artist.name.len();
            next = next_artist_credit(&self.track, cursor);
        }
        if cursor < self.track.artist.len() {
            credits = credits.child(text(self.track.artist[cursor..].to_owned().into()));
        }
        credits.into_any_element()
    }

    pub(super) fn on_favorite(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_favorite = Some(Rc::new(handler));
        self
    }

    pub(super) fn context_menu(
        mut self,
        position: Option<gpui_kit::Point<Pixels>>,
        handler: impl Fn(&gpui_kit::MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.menu_position = position;
        self.on_context_menu = Some(Box::new(handler));
        self
    }
}

/// The next reference that matches a whole displayed credit. Borrowed string
/// scans avoid fetching metadata or retaining another copy of a track's names.
fn next_artist_credit(track: &model::Track, cursor: usize) -> Option<(usize, usize)> {
    track
        .artists
        .iter()
        .enumerate()
        .filter(|(_, artist)| artist.source_id.is_some() && !artist.name.is_empty())
        .filter_map(|(index, artist)| {
            track.artist[cursor..]
                .match_indices(artist.name.as_ref())
                .map(|(offset, _)| cursor + offset)
                .find(|start| {
                    let end = start + artist.name.len();
                    (*start == 0 || track.artist[..*start].ends_with(", "))
                        && (end == track.artist.len() || track.artist[end..].starts_with(", "))
                })
                .map(|start| (start, index))
        })
        .min_by_key(|(start, _)| *start)
}

impl RenderOnce for TrackRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = self.palette;
        let index = self.index;
        let menu_position = self.menu_position;
        let rem_size = window.rem_size();
        let base = if self.current {
            palette.selection
        } else {
            palette.surface
        };
        let artists = self.artist_line(window);
        let album_text = ellipsized_line()
            .text_size(tokens::BODY_TEXT)
            .text_color(rgb(palette.text))
            .child(self.track.album.clone())
            .into_any_element();
        let album = self
            .track
            .album_ref
            .as_ref()
            .filter(|album| album.source_id.is_some());
        let album_label = if self.compact {
            album_text
        } else if album.is_some() {
            self.navigation_link(
                ("track-album", index),
                TrackNavigation::Album,
                album_text,
                window,
                true,
            )
        } else {
            album_text
        };
        let artwork = (!self.hide_artwork).then(|| {
            components::artwork(
                palette,
                &self.image_cache,
                self.track.artwork_url.as_deref(),
                tokens::TRACK_ARTWORK,
                CadenceIcon::MusicNote,
            )
        });
        let on_play = self.on_play;
        let row = components::row_button(palette, ("spotify-track", index))
            .test_support()
            .w_full()
            .h(if self.dense { Rems(3.25) } else { Rems(3.75) })
            .px_3()
            .rounded_lg()
            .justify_start()
            .bg(rgb(if self.current {
                palette.selection
            } else {
                palette.surface
            }))
            .when_some(self.on_hover, |row, handler| row.on_hover(handler))
            .child(
                div()
                    .id(("track-index", index))
                    .test_support()
                    .group("track-index")
                    .relative()
                    .w(INDEX_COLUMN_WIDTH)
                    .h_8()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(tokens::BODY_TEXT)
                    .text_color(rgb(palette.text_muted))
                    .when(!self.current, |column| {
                        column.child(
                            div()
                                .group_hover("track-index", |style| style.opacity(0.))
                                .child((index + 1).to_string()),
                        )
                    })
                    .child(
                        components::button(
                            palette,
                            (
                                if self.current {
                                    "track-playback-toggle"
                                } else {
                                    "track-play"
                                },
                                index,
                            ),
                        )
                        .test_support()
                        .aria_label(if self.current && self.playing {
                            "Pause"
                        } else {
                            "Play"
                        })
                        .absolute()
                        .inset_0()
                        .rounded_md()
                        .when(!self.current, |button| {
                            // The row already provides keyboard activation. The
                            // pointer-only glyph adds no extra Tab stop per track.
                            button
                                .tab_stop(false)
                                .opacity(0.)
                                .group_hover("track-index", |style| style.opacity(1.))
                        })
                        .child(components::icon(
                            if self.current && self.playing {
                                CadenceIcon::Pause
                            } else {
                                CadenceIcon::Play
                            },
                            tokens::CONTROL_ICON,
                            palette.text_primary,
                        ))
                        .when_some(
                            if self.current {
                                self.on_toggle_playback
                            } else {
                                on_play.clone()
                            },
                            |button, handler| {
                                button.on_click(move |event, window, cx| {
                                    cx.stop_propagation();
                                    if event.is_keyboard() || event.click_count() == 1 {
                                        handler(event, window, cx);
                                    }
                                })
                            },
                        ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pr(COLUMN_GUTTER)
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .children(artwork.map(|artwork| {
                        div()
                            .id(("track-cover", index))
                            .test_support()
                            .flex_none()
                            .child(artwork)
                    }))
                    .child(
                        // Cross-axis stretch (the default) hands each line a
                        // definite width, which text layout needs to ellipsize
                        // instead of clipping mid-glyph.
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                ellipsized_line()
                                    .text_size(tokens::BODY_TEXT)
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(rgb(palette.text_primary))
                                    .child(self.track.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette.text_muted))
                                    .child(artists),
                            ),
                    ),
            )
            .when(!self.compact, |row| {
                row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(album_label),
                )
            })
            .child(
                components::button(palette, ("spotify-favorite", index))
                    .test_support()
                    .size(STAR_COLUMN_WIDTH)
                    .flex_none()
                    .rounded_full()
                    .hover(|style| style.bg(rgb(palette.control)))
                    .child(components::icon(
                        if self.favorite {
                            CadenceIcon::StarFilled
                        } else {
                            CadenceIcon::Star
                        },
                        tokens::MENU_ICON,
                        if self.favorite {
                            palette.text_primary
                        } else {
                            palette.text
                        },
                    ))
                    .when_some(self.on_favorite, |button, handler| {
                        button.on_click(move |event, window, cx| {
                            cx.stop_propagation();
                            handler(event, window, cx);
                        })
                    }),
            )
            .child(
                div()
                    .id(("track-duration", index))
                    .test_support()
                    .w(TIME_COLUMN_WIDTH)
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_end()
                    .pr_2()
                    .text_size(tokens::BODY_TEXT)
                    .text_color(rgb(palette.text_muted))
                    .child(format_duration(self.track.duration_ms)),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .when_some(self.menu, |anchor, menu| {
                        let menu_offset = point(px(0.), ACTIONS_MENU_OFFSET.to_pixels(rem_size));
                        let menu_window_margin = ACTIONS_MENU_WINDOW_MARGIN.to_pixels(rem_size);
                        anchor.child(deferred(
                            anchored()
                                .offset(menu_offset)
                                .anchor(Anchor::TopRight)
                                .when_some(menu_position, |menu, position| {
                                    menu.position(position)
                                        .anchor(Anchor::TopLeft)
                                        .offset(point(px(0.), px(0.)))
                                })
                                .snap_to_window_with_margin(menu_window_margin)
                                .child(menu),
                        ))
                    }),
            )
            .when_some(self.on_context_menu, |row, handler| {
                row.on_mouse_down(gpui_kit::MouseButton::Right, move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx);
                })
            })
            .when_some(on_play.filter(|_| !self.current), |row, handler| {
                row.on_click(move |event, window, cx| {
                    if event.is_keyboard() || event.click_count() == 2 {
                        handler(event, window, cx);
                    }
                })
            });
        let row = if let Some(transition) = self.hover_transition.filter(|_| !self.current) {
            transition.apply(
                row,
                window,
                base,
                components::hover_fill(palette.selection),
                cx.reduce_motion(),
            )
        } else {
            row.into_any_element()
        };
        // Permanent spacing, including at rest: hover cannot move any rows.
        div()
            .w_full()
            .flex_none()
            .py(Rems(0.125))
            .child(row)
            .into_any_element()
    }
}

/// One playlist in a list. Stateless, like `TrackRow`.
#[derive(IntoElement)]
pub(super) struct PlaylistRow {
    index: usize,
    playlist: model::Playlist,
    palette: CadencePalette,
    image_cache: Entity<image_cache::BoundedImageCache>,
    on_open: Option<RowCallback>,
}

impl PlaylistRow {
    pub(super) fn new(
        index: usize,
        playlist: model::Playlist,
        palette: CadencePalette,
        image_cache: Entity<image_cache::BoundedImageCache>,
    ) -> Self {
        Self {
            index,
            playlist,
            palette,
            image_cache,
            on_open: None,
        }
    }

    pub(super) fn on_open(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for PlaylistRow {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let palette = self.palette;
        let detail = format!(
            "{} tracks · {}",
            self.playlist.track_count, self.playlist.owner
        );
        components::button(palette, ("spotify-playlist", self.index))
            .w_full()
            .h(tokens::PLAYLIST_ROW_HEIGHT)
            .px_3()
            .justify_start()
            .gap_3p5()
            .rounded_lg()
            .hover(|style| style.bg(rgb(palette.surface_hover)))
            .child(components::artwork(
                palette,
                &self.image_cache,
                self.playlist.artwork_url.as_deref(),
                tokens::PLAYLIST_ROW_ARTWORK,
                CadenceIcon::Playlist,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text_primary))
                            .child(self.playlist.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.text_muted))
                            .child(detail),
                    ),
            )
            .when_some(self.on_open, |row, handler| {
                row.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}

#[cfg(test)]
mod credit_tests {
    use super::*;
    #[test]
    fn credit_matching_preserves_missing_references_and_names_with_commas() {
        let mut track = test_support::track(0);
        track.artist = "Tiffany Day, Tyler, The Creator, glaive".into();
        track.artists = ["glaive", "Tyler, The Creator"]
            .into_iter()
            .map(|name| model::ArtistRef {
                name: name.into(),
                source_id: Some(name.into()),
                spotify_uri: None,
            })
            .collect();
        assert_eq!(next_artist_credit(&track, 0), Some((13, 1)));
        assert_eq!(next_artist_credit(&track, 31), Some((33, 0)));
        track.artist = "glaive fan club".into();
        assert_eq!(next_artist_credit(&track, 0), None);
    }
}
