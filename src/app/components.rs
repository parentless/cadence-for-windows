//! Shared building blocks for Cadence views.
//!
//! These are free functions rather than methods so that every view entity can
//! reach them, and they take the palette by value so a view always draws with
//! the appearance resolved for the frame it is rendering.

use super::*;

/// The keyboard focus ring's stroke, the same physical width as `border_2`.
const FOCUS_RING_WIDTH: Pixels = px(2.);

/// Makes an identified element a control the keyboard can reach: a Tab stop
/// that Enter and Space activate. Like a macOS button, it does not take
/// keyboard focus from a pointer press, so Space after a click still controls
/// playback.
fn keyboard_control(element: Stateful<Div>) -> Stateful<Div> {
    element
        .focusable()
        .tab_stop(true)
        .key_context(CONTROL_KEY_CONTEXT)
        .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, _| {
            window.prevent_default();
        })
}

/// A keyboard control that draws the focus ring inside its own edge, so the
/// ring follows the control's radius, takes no layout space and no clipping
/// ancestor can hide it.
pub(super) fn control(palette: CadencePalette, element: Stateful<Div>) -> Stateful<Div> {
    keyboard_control(element).focus_visible(|style| style.shadow(inset_focus_ring(palette)))
}

/// A centered control without a drawn focus state, for a control that shows
/// focus on an inner surface.
pub(super) fn bare_button(id: impl Into<ElementId>) -> Stateful<Div> {
    keyboard_control(div().id(id))
        .flex()
        .items_center()
        .justify_center()
        .active(|style| style.opacity(0.82))
}

pub(super) fn row_button(palette: CadencePalette, id: impl Into<ElementId>) -> Stateful<Div> {
    control(palette, div().id(id))
        .flex()
        .items_center()
        .justify_center()
        .active(|style| style.opacity(0.82))
}

/// Static pointer feedback: no timer, animation wrapper, or layout changes.
pub(super) fn button(palette: CadencePalette, id: impl Into<ElementId>) -> Stateful<Div> {
    row_button(palette, id)
}

/// A button whose own fill covers it. The ring sits outside the edge, where it
/// contrasts with the surface around the button rather than with the fill.
pub(super) fn filled_button(palette: CadencePalette, id: impl Into<ElementId>) -> Stateful<Div> {
    bare_button(id).focus_visible(|style| style.shadow(outset_focus_ring(palette)))
}

pub(super) fn inset_focus_ring(palette: CadencePalette) -> Vec<gpui_kit::BoxShadow> {
    vec![focus_ring_shadow(palette).inset()]
}

/// Painted under the element, so only an element with an opaque fill shows it
/// as a ring.
fn outset_focus_ring(palette: CadencePalette) -> Vec<gpui_kit::BoxShadow> {
    vec![focus_ring_shadow(palette)]
}

fn focus_ring_shadow(palette: CadencePalette) -> gpui_kit::BoxShadow {
    gpui_kit::BoxShadow::new(px(0.), px(0.), rgb(palette.focus_ring).into())
        .spread_radius(FOCUS_RING_WIDTH)
}

/// Whether a control that draws its own ring should show it: focused, and
/// reached from the keyboard.
pub(super) fn is_focus_visible(focus_handle: &FocusHandle, window: &Window) -> bool {
    focus_handle.is_focused(window) && window.last_input_was_keyboard()
}

/// The bordered frame of a text field, ringed while `input` has focus, however
/// focus got there, as a macOS text field does. The frame keeps its one-pixel
/// border and the ring grows inward from it, so the text does not move.
pub(super) fn text_field_frame(
    palette: CadencePalette,
    input: &Entity<InputState>,
    window: &Window,
    cx: &App,
) -> Div {
    let focused = input.read(cx).focus_handle(cx).is_focused(window);
    div()
        .flex()
        .items_center()
        .px_3p5()
        .rounded_xl()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .when(focused, |frame| {
            frame
                .border_color(rgb(palette.focus_ring))
                .shadow(inset_focus_ring(palette))
        })
}

/// A button that sits flush with the content around it: its padding makes
/// room for the focus ring and its negative margin takes that room back.
pub(super) fn flush_button(palette: CadencePalette, id: impl Into<ElementId>) -> Stateful<Div> {
    button(palette, id)
        .px(tokens::FOCUS_RING_CLEARANCE)
        .mx(-tokens::FOCUS_RING_CLEARANCE)
}

/// Rings a component that draws no focus state of its own while the keyboard
/// focus is inside it. The wrapper tracks `focus_handle` only to observe that
/// focus, so it must not be a Tab stop itself.
pub(super) fn focus_ring_around(
    palette: CadencePalette,
    focus_handle: &FocusHandle,
    window: &Window,
    cx: &App,
    component: impl IntoElement,
) -> Div {
    let focus_visible = is_focus_visible_within(focus_handle, window, cx);
    div()
        .track_focus(focus_handle)
        .key_context(CONTROL_KEY_CONTEXT)
        .rounded_full()
        .when(focus_visible, |wrapper| {
            wrapper.shadow(outset_focus_ring(palette))
        })
        .child(component)
}

/// Whether [`focus_ring_around`] draws its ring for `focus_handle`.
pub(super) fn is_focus_visible_within(
    focus_handle: &FocusHandle,
    window: &Window,
    cx: &App,
) -> bool {
    focus_handle.contains_focused(window, cx) && window.last_input_was_keyboard()
}

/// A link that reads like the copy around it until the pointer or keyboard
/// focus reaches it, so a line of credits stays quiet at rest.
///
/// Built on the base button with the link role because gpui-kit 0.7 cannot
/// observe its base link in tests. The button's neutral line height is set
/// back to the window's so the text keeps its metrics.
pub(super) fn link(
    palette: CadencePalette,
    id: impl Into<ElementId>,
    window: &Window,
) -> gpui_kit::base::Button {
    gpui_kit::base::Button::new(id)
        .role(gpui_kit::Role::Link)
        .line_height(window.text_style().line_height)
        .cursor_pointer()
        .hover(|style| style.underline().text_color(rgb(palette.text_primary)))
        .focus_visible(|style| style.underline().text_color(rgb(palette.text_primary)))
}

/// The transient banner for things that finished without a page to say so.
pub(super) fn action_notice_banner(
    palette: CadencePalette,
    message: SharedString,
    on_dismiss: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    deferred(
        div()
            .occlude()
            .absolute()
            .top(tokens::NOTICE_BANNER_TOP)
            .right_6()
            .w(tokens::NOTICE_BANNER_WIDTH)
            .min_h_12()
            .px_3p5()
            .py_2()
            .rounded(tokens::CONTAINER_RADIUS)
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface_raised))
            .shadow_lg()
            .flex()
            .items_center()
            .gap_2p5()
            .text_size(tokens::BODY_TEXT)
            .text_color(rgb(palette.text_primary))
            .child(div().flex_1().child(message))
            .child(
                icon_button(palette, "dismiss-action-notice", CadenceIcon::Close)
                    .size_8()
                    .on_click(on_dismiss),
            ),
    )
    .into_any_element()
}

pub(super) fn icon(glyph: CadenceIcon, size: Rems, color: u32) -> Icon {
    Icon::new(glyph).size(size).text_color(rgb(color))
}

pub(super) fn pill(
    palette: CadencePalette,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    primary: bool,
) -> Stateful<Div> {
    let hover_background = if primary {
        rgb(palette.accent_hover)
    } else {
        rgb(palette.control_hover)
    };
    pill_surface(palette, filled_button(palette, id), primary)
        .hover(move |style| style.bg(hover_background))
        .child(label.into())
}

/// A primary pill for an action already under way. It looks like [`pill`] at
/// rest but is not a control, so it is neither a Tab stop nor hoverable.
pub(super) fn pending_pill(
    palette: CadencePalette,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
) -> Stateful<Div> {
    let frame = div().id(id).flex().items_center().justify_center();
    pill_surface(palette, frame, true).child(label.into())
}

fn pill_surface(palette: CadencePalette, frame: Stateful<Div>, primary: bool) -> Stateful<Div> {
    let (background, foreground) = if primary {
        (rgb(palette.text_primary), rgb(palette.surface))
    } else {
        (rgb(palette.control), rgb(palette.text_primary))
    };
    frame
        .h_10()
        .px_4()
        .rounded_full()
        .bg(background)
        .text_color(foreground)
        .text_size(tokens::PILL_TEXT)
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
}

pub(super) fn icon_button(
    palette: CadencePalette,
    id: impl Into<ElementId>,
    glyph: CadenceIcon,
) -> Stateful<Div> {
    icon_button_sized(palette, id, glyph, tokens::CONTROL_ICON)
}

pub(super) fn icon_button_sized(
    palette: CadencePalette,
    id: impl Into<ElementId>,
    glyph: CadenceIcon,
    size: Rems,
) -> Stateful<Div> {
    button(palette, id)
        .size_10()
        .flex_none()
        .rounded_full()
        .text_color(rgb(palette.text_primary))
        .hover(|style| style.bg(rgb(palette.control)))
        .child(icon(glyph, size, palette.text_primary))
}

pub(super) fn menu_item(
    palette: CadencePalette,
    id: impl Into<ElementId>,
    glyph: CadenceIcon,
    label: &'static str,
    destructive: bool,
) -> Stateful<Div> {
    let color = if destructive {
        palette.danger
    } else {
        palette.text
    };
    button(palette, id)
        .w_full()
        .h_9()
        .px_2p5()
        .justify_start()
        .gap_2p5()
        .rounded_lg()
        .text_size(tokens::BODY_TEXT)
        .text_color(rgb(color))
        .hover(|style| style.bg(rgb(palette.control_hover)))
        .child(icon(glyph, tokens::MENU_ICON, color))
        .child(label)
}

pub(super) fn text_menu_item(
    palette: CadencePalette,
    id: impl Into<ElementId>,
    label: &'static str,
) -> Stateful<Div> {
    text_menu_row(button(palette, id), label, palette.text)
        .hover(|style| style.bg(rgb(palette.control_hover)))
}

/// A menu item for an action that does not apply right now: muted, not
/// hoverable and not a Tab stop.
pub(super) fn disabled_text_menu_item(
    palette: CadencePalette,
    id: impl Into<ElementId>,
    label: &'static str,
) -> Stateful<Div> {
    let row = div().id(id).flex().items_center();
    text_menu_row(row, label, palette.text_muted)
}

fn text_menu_row(row: Stateful<Div>, label: &'static str, color: u32) -> Stateful<Div> {
    let glyph = match label {
        "Play now" => CadenceIcon::Play,
        "Play next" => CadenceIcon::SkipForward,
        "Add to queue" => CadenceIcon::Queue,
        "Go to artist" => CadenceIcon::Person,
        "Go to album" => CadenceIcon::MusicNote,
        "Open track in Spotify" => CadenceIcon::ArrowUpRight,
        "Add to Liked Songs" | "Remove from Liked Songs" | "Updating Spotify library…" => {
            CadenceIcon::Heart
        }
        "Add to favorites" | "Remove from favorites" => CadenceIcon::Star,
        _ => CadenceIcon::MusicNote,
    };
    row.w_full()
        .h_9()
        .px_3()
        .justify_start()
        .gap_2p5()
        .rounded_lg()
        .text_size(tokens::BODY_TEXT)
        .text_color(rgb(color))
        .child(icon(glyph, tokens::MENU_ICON, color))
        .child(label)
}

/// The outlined secondary button used by the setup screens and settings.
pub(super) fn settings_button(
    palette: CadencePalette,
    id: impl Into<ElementId>,
    label: &'static str,
) -> Stateful<Div> {
    filled_button(palette, id)
        .h_10()
        .px_3p5()
        .rounded(tokens::CONTROL_RADIUS)
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .text_size(tokens::BODY_TEXT)
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(rgb(palette.text_primary))
        .hover(|style| style.bg(rgb(palette.control_hover)))
        .child(label)
}

/// Static translucent materials reuse the already-blurred artist backdrop;
/// no per-frame blur pass, animation, or image allocation is needed.
fn glass_frame(frame: Div, palette: CadencePalette, menu: bool) -> Div {
    let light = palette.canvas == CadencePalette::LIGHT.canvas;
    let (top, bottom, rim) = match (light, menu) {
        (true, _) => (0xFFFFFFEE, 0xF2F0EDE0, 0xFFFFFFCC),
        (false, true) => (0x38363EEB, 0x26252BE8, 0xFFFFFF44),
        (false, false) => (0xFFFFFF2C, 0xFFFFFF12, 0xFFFFFF55),
    };
    frame
        .rounded_2xl()
        .border_1()
        .bg(gpui_kit::linear_gradient(
            165.,
            gpui_kit::linear_color_stop(gpui_kit::rgba(top), 0.),
            gpui_kit::linear_color_stop(gpui_kit::rgba(bottom), 1.),
        ))
        .border_color(gpui_kit::rgba(rim))
        .shadow(vec![
            gpui_kit::BoxShadow::new(px(0.), px(4.), gpui_kit::rgba(0x00000028).into())
                .blur_radius(px(12.)),
            gpui_kit::BoxShadow::new(px(0.), px(1.), gpui_kit::rgba(0xFFFFFF22).into()).inset(),
        ])
}

pub(super) fn glass_search_frame(
    palette: CadencePalette,
    input: &Entity<InputState>,
    window: &Window,
    cx: &App,
) -> Div {
    glass_frame(div().flex().items_center().px_3p5(), palette, false).when(
        input.read(cx).focus_handle(cx).is_focused(window),
        |frame| {
            frame
                .border_color(rgb(palette.focus_ring))
                .shadow(inset_focus_ring(palette))
        },
    )
}

pub(super) fn menu_surface(palette: CadencePalette) -> Div {
    glass_frame(div(), palette, true)
        .occlude()
        .w(tokens::MENU_WIDTH)
        .p_1p5()
        .flex()
        .flex_col()
        .text_color(rgb(palette.text))
        .text_size(tokens::BODY_TEXT)
}

/// A page subtitle that reports a background refresh without replacing the
/// contents already on screen.
pub(super) fn revalidating_detail(detail: impl Into<String>, refreshing: bool) -> String {
    let detail = detail.into();
    if refreshing {
        format!("{detail} · refreshing…")
    } else {
        detail
    }
}

pub(super) fn section_label(palette: CadencePalette, text: impl Into<SharedString>) -> Div {
    div()
        .text_size(tokens::BODY_TEXT)
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(rgb(palette.text_muted))
        .child(text.into())
}

pub(super) fn empty_state(palette: CadencePalette, text: impl Into<SharedString>) -> Div {
    div()
        .p_6()
        .rounded(tokens::PANEL_RADIUS)
        .border_1()
        .border_color(rgb(palette.border))
        .text_color(rgb(palette.text_muted))
        .child(text.into())
}

pub(super) fn artwork(
    palette: CadencePalette,
    image_cache: &Entity<image_cache::BoundedImageCache>,
    url: Option<&str>,
    artwork_size: tokens::ArtworkSize,
    fallback_icon: CadenceIcon,
) -> gpui_kit::AnyElement {
    rectangular_artwork(
        palette,
        image_cache,
        url,
        artwork_size,
        artwork_size.size,
        fallback_icon,
    )
}

/// Fixed external dimensions keep source image proportions out of layout.
/// Search portraits use a wide frame; cache decoding still follows its short edge.
pub(super) fn rectangular_artwork(
    palette: CadencePalette,
    image_cache: &Entity<image_cache::BoundedImageCache>,
    url: Option<&str>,
    artwork_size: tokens::ArtworkSize,
    width: Rems,
    fallback_icon: CadenceIcon,
) -> gpui_kit::AnyElement {
    let frame = div()
        .w(width)
        .h(artwork_size.size)
        .relative()
        .flex_none()
        .rounded(artwork_size.radius)
        .overflow_hidden();
    if let Some(url) = url {
        let cache = image_cache.clone();
        let resource = gpui_kit::Resource::Uri(url.to_owned().into());
        frame
            .child(
                gpui_kit::canvas(
                    move |bounds, window, cx| {
                        if !bounds.intersects(&window.content_mask().bounds) {
                            return None;
                        }
                        let physical = f32::from(bounds.size.width.min(bounds.size.height))
                            * window.scale_factor();
                        cache.update(cx, |cache, cx| {
                            cache
                                .load_visible_sized(&resource, physical.ceil() as u32, window, cx)
                                .and_then(Result::ok)
                        })
                    },
                    move |bounds, image: Option<Arc<gpui_kit::RenderImage>>, window, _| {
                        let radius = artwork_size.radius.to_pixels(window.rem_size());
                        if let Some(image) = image.filter(|image| image.frame_count() > 0) {
                            let fitted =
                                gpui_kit::ObjectFit::Cover.get_bounds(bounds, image.size(0));
                            if let Err(error) = window.paint_image(
                                bounds,
                                fitted,
                                gpui_kit::Corners::all(radius),
                                image,
                                0,
                                false,
                            ) {
                                log::warn!("could not paint artwork: {error}");
                            }
                        } else {
                            // A placeholder replaces the missing image; it is
                            // never an extra opaque quad behind a loaded cover.
                            window.paint_quad(
                                gpui_kit::fill(bounds, rgb(palette.selection)).corner_radii(radius),
                            );
                        }
                    },
                )
                .absolute()
                .inset_0()
                .size_full(),
            )
            .into_any_element()
    } else {
        frame
            .bg(rgb(palette.selection))
            .flex()
            .items_center()
            .justify_center()
            .child(icon(
                fallback_icon,
                artwork_size.size * 0.3,
                palette.text_primary,
            ))
            .into_any_element()
    }
}

pub(super) fn profile_avatar(
    url: Option<&str>,
    initials: impl Into<SharedString>,
    window: &Window,
) -> gpui_kit::AnyElement {
    let avatar_size = tokens::AVATAR_SIZE.to_pixels(window.rem_size());
    let avatar = Avatar::new()
        .with_size(avatar_size)
        .border_0()
        .name(initials);
    if let Some(url) = url {
        avatar.src(url.to_owned()).into_any_element()
    } else {
        avatar.into_any_element()
    }
}

pub(super) fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|part| part.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

/// The large title a page opens with.
pub(super) fn page_title(palette: CadencePalette, title: impl Into<SharedString>) -> Div {
    div()
        .text_size(tokens::PAGE_TITLE_TEXT)
        .line_height(tokens::PAGE_TITLE_LINE_HEIGHT)
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(rgb(palette.text_primary))
        .child(title.into())
}

/// The subtitle line under a page title.
pub(super) fn page_detail(palette: CadencePalette, detail: impl Into<SharedString>) -> Div {
    div()
        .text_sm()
        .text_color(rgb(palette.text_muted))
        .child(detail.into())
}

/// The title and subtitle every page opens with.
pub(super) fn page_heading(
    palette: CadencePalette,
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
) -> Div {
    div()
        .flex()
        .items_end()
        .justify_between()
        .gap_6()
        .mb_6()
        .child(
            div()
                .flex()
                .flex_col()
                .gap(tokens::PAGE_HEADING_GAP)
                .child(page_title(palette, title))
                .child(page_detail(palette, detail)),
        )
}

/// The frame every page's contents sit in.
pub(super) fn page(id: impl Into<ElementId>) -> Stateful<Div> {
    div().id(id).size_full().min_h_0().flex().flex_col().p_8()
}

/// Opaque native menus; the search field retains its separate glass treatment.
pub(super) fn solid_menu_surface(palette: CadencePalette) -> Div {
    div()
        .occlude()
        .w(tokens::MENU_WIDTH)
        .p_1p5()
        .flex()
        .flex_col()
        .rounded_xl()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface_raised))
        .shadow_md()
        .text_color(rgb(palette.text))
        .text_size(tokens::BODY_TEXT)
}

/// Static placeholders have no shimmer timer and disappear when data arrives.
pub(super) fn list_skeleton(palette: CadencePalette) -> Div {
    list_skeleton_rows(palette, 8)
}

pub(super) fn list_skeleton_rows(palette: CadencePalette, count: usize) -> Div {
    div()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .children((0..count).map(|index| {
            div()
                .h_16()
                .flex_none()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .child(div().size_10().rounded_md().bg(rgb(palette.control)))
                .child(
                    div()
                        .flex_1()
                        .child(
                            div()
                                .h_3()
                                .w(relative(if index % 2 == 0 { 0.45 } else { 0.65 }))
                                .rounded_md()
                                .bg(rgb(palette.control)),
                        )
                        .child(
                            div()
                                .mt_2()
                                .h_2()
                                .w(relative(0.28))
                                .rounded_md()
                                .bg(rgb(palette.control)),
                        ),
                )
        }))
}

const HOVER_FADE: Duration = Duration::from_millis(140);

/// A list retains two row indices, never an entry per track. Only pointer changes
/// start a short repaint; no timer runs while the pointer is still or absent.
#[derive(Default)]
pub(super) struct HoverHighlight {
    hovered: Option<usize>,
    previous: Option<(usize, f32)>,
    started: Option<std::time::Instant>,
}

impl HoverHighlight {
    fn progress(&self, now: std::time::Instant) -> f32 {
        self.started.map_or(1., |start| {
            (now.saturating_duration_since(start).as_secs_f32() / HOVER_FADE.as_secs_f32())
                .clamp(0., 1.)
        })
    }

    pub(super) fn set(&mut self, index: usize, hovered: bool, now: std::time::Instant) {
        if hovered && self.hovered == Some(index) || !hovered && self.hovered != Some(index) {
            return;
        }
        let progress = self.progress(now);
        self.previous = self
            .hovered
            .map(|old| {
                let from = self
                    .previous
                    .filter(|(id, _)| *id == old)
                    .map_or(0., |(_, alpha)| alpha);
                (old, from + (1. - from) * progress)
            })
            .or_else(|| {
                self.previous
                    .map(|(old, alpha)| (old, alpha * (1. - progress)))
            });
        self.hovered = hovered.then_some(index);
        self.started = Some(now);
    }

    pub(super) fn transition(
        &self,
        index: usize,
        now: std::time::Instant,
    ) -> Option<HoverTransition> {
        let target = if self.hovered == Some(index) { 1. } else { 0. };
        let from = self
            .previous
            .filter(|(old, _)| *old == index)
            .map_or(0., |(_, alpha)| alpha);
        if target == 0. && from == 0. {
            return None;
        }
        let progress = self.progress(now);
        Some(HoverTransition {
            from: from + (target - from) * progress,
            target,
            remaining: HOVER_FADE.mul_f32(1. - progress),
        })
    }
}

pub(super) struct HoverTransition {
    from: f32,
    target: f32,
    remaining: Duration,
}

impl HoverTransition {
    pub(super) fn color(
        self,
        window: &mut Window,
        base: u32,
        highlight: u32,
        reduce_motion: bool,
    ) -> gpui_kit::Rgba {
        if !reduce_motion && !self.remaining.is_zero() {
            let view = window.current_view();
            window.on_next_frame(move |_, cx| cx.notify(view));
        }
        let alpha = if reduce_motion {
            self.target
        } else {
            self.from
        };
        let channel = |shift: u32| {
            let a = ((base >> shift) & 255u32) as f32;
            let b = ((highlight >> shift) & 255u32) as f32;
            (a + (b - a) * alpha).round() as u32
        };
        rgb((channel(16) << 16) | (channel(8) << 8) | channel(0))
    }

    pub(super) fn apply<T: Styled + IntoElement + 'static>(
        self,
        row: T,
        window: &mut Window,
        base: u32,
        highlight: u32,
        reduce_motion: bool,
    ) -> AnyElement {
        // Only the color changes: pointer tracking and nested controls retain identity.
        row.bg(self.color(window, base, highlight, reduce_motion))
            .into_any_element()
    }
}

/// A lighter sibling of the selected fill, without an animated material pass.
pub(super) fn hover_fill(selected: u32) -> u32 {
    let lift = if ((selected >> 16) & 255) + ((selected >> 8) & 255) + (selected & 255) > 480 {
        50
    } else {
        14
    };
    let channel = |shift: u32| {
        let value = (selected >> shift) & 255u32;
        value + (255 - value) * lift / 100
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

#[cfg(test)]
mod hover_tests {
    use super::*;

    #[test]
    fn hover_is_lighter_than_every_selected_colour_in_both_appearances() {
        let luminance =
            |rgb: u32| ((rgb >> 16) & 255) * 2126 + ((rgb >> 8) & 255) * 7152 + (rgb & 255) * 722;
        for dark in [false, true] {
            for colour in crate::storage::SelectionColor::ALL {
                let selected = colour.fill(dark);
                assert!(luminance(super::hover_fill(selected)) > luminance(selected));
            }
        }
    }

    #[test]
    fn interrupted_hover_is_continuous_and_settles_without_frame_demand() {
        let mut hover = HoverHighlight::default();
        let start = std::time::Instant::now();
        hover.set(0, true, start);
        let halfway = start + HOVER_FADE / 2;
        let before = hover.transition(0, halfway).unwrap().from;
        hover.set(0, false, halfway);
        assert_eq!(hover.transition(0, halfway).unwrap().from, before);
        let returning = halfway + HOVER_FADE / 2;
        let faded = hover.transition(0, returning).unwrap().from;
        hover.set(0, true, returning);
        assert_eq!(hover.transition(0, returning).unwrap().from, faded);
        let settled = hover.transition(0, returning + HOVER_FADE).unwrap();
        assert_eq!(settled.from, 1.);
        assert!(settled.remaining.is_zero());
        // Repeated hover reports don't restart a completed transition.
        hover.set(0, true, returning + HOVER_FADE);
        assert!(
            hover
                .transition(0, returning + HOVER_FADE)
                .unwrap()
                .remaining
                .is_zero()
        );
        hover.set(0, false, returning + HOVER_FADE);
        assert_eq!(
            hover
                .transition(0, returning + HOVER_FADE * 2)
                .unwrap()
                .from,
            0.
        );
    }
}

/// Local pointer feedback keeps the control's layout and hit target fixed.
/// Only release interpolates; keyboard activation never enters pointer state.
#[derive(gpui_kit::IntoElement)]
pub(super) struct PressFeedback {
    id: ElementId,
    control: AnyElement,
}
#[derive(Default)]
struct PointerPress {
    down: bool,
}

pub(super) fn press_feedback(id: impl Into<ElementId>, control: impl IntoElement) -> PressFeedback {
    PressFeedback {
        id: id.into(),
        control: control.into_any_element(),
    }
}

impl gpui_kit::RenderOnce for PressFeedback {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self.id.clone();
        window.with_id(self.id, |window| {
            let state = window.use_keyed_state("pointer-press", cx, |_, _| PointerPress::default());
            if !window.is_window_active() && state.read(cx).down {
                state.update(cx, |state, _| state.down = false);
            }
            let down = state.read(cx).down;
            let opacity = gpui_kit::base::transition(
                "press-release",
                if down { 0.90 } else { 1. },
                gpui_kit::base::Transition::new(if down {
                    Duration::ZERO
                } else {
                    Duration::from_millis(100)
                })
                .ease(gpui_kit::ease_out_quint()),
                window,
                cx,
            );
            let view = window.current_view();
            let pressed = state.clone();
            let released = state.clone();
            let departed = state.clone();
            div()
                .id(id)
                .flex_none()
                .flex()
                .items_center()
                .opacity(opacity)
                .on_mouse_down(gpui_kit::MouseButton::Left, move |_, _, cx| {
                    pressed.update(cx, |state, _| state.down = true);
                    cx.notify(view);
                })
                .on_mouse_up(gpui_kit::MouseButton::Left, move |_, _, cx| {
                    released.update(cx, |state, _| state.down = false);
                    cx.notify(view);
                })
                .on_hover(move |hovered, _, cx| {
                    if !*hovered && departed.read(cx).down {
                        departed.update(cx, |state, _| state.down = false);
                        cx.notify(view);
                    }
                })
                .on_mouse_up_out(gpui_kit::MouseButton::Left, move |_, _, cx| {
                    if state.read(cx).down {
                        state.update(cx, |state, _| state.down = false);
                        cx.notify(view);
                    }
                })
                .child(self.control)
        })
    }
}
