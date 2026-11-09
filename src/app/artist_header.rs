//! Scroll-driven identity overlay. No task, polling, or image processing per frame.
use super::*;
use gpui_kit::TestSupportExt as _;
use std::time::Instant;

pub(super) struct ArtistHeader {
    pub name: SharedString,
    pub artwork: Option<String>,
    cache: Entity<image_cache::BoundedImageCache>,
    pub target_bounds: Option<Bounds<Pixels>>,
    pub origin: gpui_kit::Point<Pixels>,
    pub viewport_width: Pixels,
    target: f32,
    current: f32,
    previous: Instant,
    changed: Instant,
}

impl ArtistHeader {
    pub fn new(cx: &App) -> Self {
        Self {
            name: "Artist".into(),
            artwork: None,
            cache: services::AppServices::image_cache(cx),
            target_bounds: None,
            origin: point(px(0.), px(0.)),
            viewport_width: px(800.),
            target: 0.,
            current: 0.,
            previous: Instant::now(),
            changed: Instant::now(),
        }
    }

    pub fn scroll(&mut self, offset: f32, cx: &mut Context<Self>) {
        let target = (offset / 16.25).clamp(0., 1.);
        if self.target != target {
            self.target = target;
            self.changed = Instant::now();
            cx.notify();
        }
    }

    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.current = 0.;
        self.target = 0.;
        cx.notify();
    }
}

impl ArtistHeader {
    /// Returns whether another frame is needed; settling is strictly bounded.
    fn advance(&mut self, now: Instant, reduce_motion: bool) -> bool {
        if reduce_motion || now.duration_since(self.changed) >= Duration::from_millis(180) {
            self.current = self.target;
        } else {
            let dt = now.duration_since(self.previous).as_secs_f32().min(0.04);
            self.current += (self.target - self.current) * (1. - (-dt / 0.036).exp());
            if (self.current - self.target).abs() < 0.001 {
                self.current = self.target;
            }
        }
        self.previous = now;
        self.current != self.target
    }
}

impl Render for ArtistHeader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reduce_motion = cx.reduce_motion();
        let moving = self.advance(Instant::now(), reduce_motion);
        if moving {
            let view = cx.entity_id();
            window.on_next_frame(move |_, cx| cx.notify(view));
        }
        let p = self.current;
        let t = if reduce_motion {
            if p >= 1. { 1. } else { 0. }
        } else {
            p * p * (3. - 2. * p)
        };
        let rem = window.rem_size();
        let unit = |value: f32| Rems(value).to_pixels(rem);
        let end = self.target_bounds.unwrap_or(Bounds::new(
            self.origin + point(unit(25.), unit(1.25)),
            size(unit(10.), unit(2.)),
        ));
        let x = end.origin.x - self.origin.x;
        let y = end.origin.y - self.origin.y;
        let lerp = |a: Pixels, b: Pixels| a + (b - a) * t;
        let side = 10. - 8. * t;
        let palette = appearance::Appearance::palette(cx);
        // Reduced motion follows ordinary page scrolling, then snaps to the dock.
        let source_scroll = if reduce_motion && p < 1. {
            unit(p * 16.25)
        } else {
            px(0.)
        };
        let identity_opacity = if reduce_motion && p < 1. { 1. - p } else { 1. };
        div()
            .absolute()
            .inset_0()
            .size_full()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(tokens::TOOLBAR_HEIGHT)
                    .bg(rgb(palette.surface).opacity(((p - 0.5) * 2.).clamp(0., 1.))),
            )
            .child(
                div()
                    .id("artist-portrait")
                    .test_support()
                    .absolute()
                    .left(lerp(unit(2.), x))
                    .top(lerp(unit(8.) - source_scroll, y))
                    .opacity(identity_opacity)
                    .child(components::artwork(
                        palette,
                        &self.cache,
                        self.artwork.as_deref(),
                        tokens::ArtworkSize {
                            size: Rems(side),
                            radius: Rems(1. - 0.5 * t),
                        },
                        CadenceIcon::Person,
                    )),
            )
            .child(
                div()
                    .id("artist-moving-name")
                    .test_support()
                    .absolute()
                    .left(lerp(unit(13.5), x + unit(2.5)))
                    .top(lerp(unit(14.) - source_scroll, y + unit(0.25)))
                    .opacity(identity_opacity)
                    .w(lerp(
                        (self.viewport_width - unit(15.5))
                            .max(unit(1.))
                            .min(unit(30.)),
                        (end.size.width - unit(2.5)).max(unit(1.)),
                    ))
                    .truncate()
                    .text_size(px((40. - 26. * t).round()) * (rem / px(16.)))
                    .line_height(unit(2.75 - 1.25 * t))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(rgb(palette.text_primary))
                    .child(self.name.clone()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn motion_reverses_continuously_and_stops_requesting_frames() {
        let mut cx = gpui_kit::HeadlessAppContext::with_asset_source(
            Arc::new(gpui_kit::NoopTextSystem),
            Arc::new(assets::CadenceAssets),
        );
        cx.update(|cx| {
            let _backend = test_support::initialize(cx, ThemePreference::Dark);
            let mut header = ArtistHeader::new(cx);
            let start = Instant::now();
            header.changed = start;
            header.previous = start;
            header.target = 1.;
            assert!(header.advance(start + Duration::from_millis(16), false));
            let before = header.current;
            assert!(before > 0. && before < 1.);
            header.target = 0.;
            header.changed = start + Duration::from_millis(16);
            assert_eq!(header.current, before);
            assert!(header.advance(start + Duration::from_millis(32), false));
            assert!(header.current < before);
            assert!(!header.advance(start + Duration::from_millis(196), false));
            assert_eq!(header.current, 0.);
            for milliseconds in [200, 500, 10_000] {
                assert!(!header.advance(start + Duration::from_millis(milliseconds), false));
            }
            header.target = 1.;
            assert!(!header.advance(start + Duration::from_millis(10_001), true));
            assert_eq!(header.current, 1.);
        });
    }
}
