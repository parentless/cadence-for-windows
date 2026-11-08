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
        .border_b_1()
        .border_color(rgb(palette.border))
        .px_3()
        .flex()
        .items_center()
        .bg(rgb(palette.canvas))
        .text_size(tokens::CAPTION_TEXT)
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(rgb(palette.text_muted))
        .child(div().w(INDEX_COLUMN_WIDTH).flex_none().child("#"))
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

type RowCallback = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
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
    current: bool,
    favorite: bool,
    /// Anchored to the pointer while the context menu is open.
    menu: Option<AnyElement>,
    on_play: Option<RowCallback>,
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
            current: false,
            favorite: false,
            menu: None,
            on_play: None,
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

    /// Marks the row as the track currently playing, which also stops it
    /// responding to clicks.
    pub(super) fn current(mut self, current: bool) -> Self {
        self.current = current;
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
        self.on_play = Some(Box::new(handler));
        self
    }

    pub(super) fn on_favorite(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_favorite = Some(Box::new(handler));
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
        let row = components::button(palette, ("spotify-track", index))
            .test_support()
            .w_full()
            .h_16()
            .px_3()
            .rounded_none()
            .justify_start()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(if self.current {
                palette.selection
            } else {
                palette.surface
            }))
            .when_some(self.on_hover, |row, handler| row.on_hover(handler))
            .child(
                div()
                    .w(INDEX_COLUMN_WIDTH)
                    .flex_none()
                    .text_size(tokens::BODY_TEXT)
                    .text_color(rgb(palette.text_muted))
                    .child((index + 1).to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pr(COLUMN_GUTTER)
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .child(components::artwork(
                        palette,
                        &self.image_cache,
                        self.track.artwork_url.as_deref(),
                        tokens::TRACK_ARTWORK,
                        CadenceIcon::MusicNote,
                    ))
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
                                ellipsized_line()
                                    .text_xs()
                                    .text_color(rgb(palette.text_muted))
                                    .child(self.track.artist.clone()),
                            ),
                    ),
            )
            .when(!self.compact, |row| {
                row.child(
                    div().flex_1().min_w_0().flex().flex_col().child(
                        ellipsized_line()
                            .text_size(tokens::BODY_TEXT)
                            .text_color(rgb(palette.text))
                            .child(self.track.album.clone()),
                    ),
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
            .when_some(self.on_play.filter(|_| !self.current), |row, handler| {
                row.on_click(move |event, window, cx| {
                    if event.is_keyboard() || event.click_count() == 2 {
                        handler(event, window, cx);
                    }
                })
            });
        if let Some(transition) = self.hover_transition {
            transition.apply(row, index, base, palette.selection, cx.reduce_motion())
        } else {
            row.into_any_element()
        }
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
        self.on_open = Some(Box::new(handler));
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
            .rounded_none()
            .border_b_1()
            .border_color(rgb(palette.border))
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
            .when_some(self.on_open, |row, handler| row.on_click(handler))
    }
}
