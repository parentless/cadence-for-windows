//! Smooth only discrete wheel steps. Precise input and scrollbar dragging keep
//! their native path. One fixed-size accumulator and one demand-driven callback
//! serve the whole window; there are no timers, per-row tasks, or idle frames.
use super::*;
use gpui_kit::{DispatchPhase, PlatformInput, ScrollDelta, ScrollWheelEvent};
use std::{cell::RefCell, time::Instant};

const SETTLE: Duration = Duration::from_millis(80);

#[derive(Default)]
pub(super) struct WheelScroll {
    event: Option<ScrollWheelEvent>,
    remaining: gpui_kit::Point<f32>,
    started: Option<Instant>,
    progress: f32,
    queued: bool,
    replaying: bool,
}

impl WheelScroll {
    fn cancel(&mut self) {
        self.event = None;
        self.remaining = point(0., 0.);
        self.started = None;
        self.progress = 0.;
    }

    fn push(&mut self, event: &ScrollWheelEvent, now: Instant) {
        let ScrollDelta::Lines(delta) = event.delta else {
            return;
        };
        if self
            .event
            .as_ref()
            .is_some_and(|old| old.position != event.position || old.modifiers != event.modifiers)
        {
            self.cancel();
        }
        for (remaining, incoming) in [
            (&mut self.remaining.x, delta.x),
            (&mut self.remaining.y, delta.y),
        ] {
            // Reversals respond without finishing an old impulse.
            if incoming * *remaining < 0. {
                *remaining = 0.;
            }
            *remaining += incoming;
        }
        self.event = Some(event.clone());
        self.started = Some(now);
        self.progress = 0.;
    }

    fn step(&mut self, now: Instant, reduce_motion: bool) -> Option<ScrollWheelEvent> {
        let started = self.started?;
        let t = if reduce_motion {
            1.
        } else {
            (now.duration_since(started).as_secs_f32() / SETTLE.as_secs_f32()).min(1.)
        };
        let progress = 1. - (1. - t).powi(3);
        let fraction = if progress >= 1. {
            1.
        } else {
            ((progress - self.progress) / (1. - self.progress)).clamp(0., 1.)
        };
        let delta = point(self.remaining.x * fraction, self.remaining.y * fraction);
        self.remaining.x -= delta.x;
        self.remaining.y -= delta.y;
        self.progress = progress;
        let mut event = self.event.clone()?;
        event.delta = ScrollDelta::Lines(delta);
        if progress >= 1. {
            self.cancel();
        }
        Some(event)
    }
}

fn schedule(state: Rc<RefCell<WheelScroll>>, window: &Window) {
    window.on_next_frame(move |window, cx| {
        let event = state.borrow_mut().step(Instant::now(), cx.reduce_motion());
        if let Some(event) = event {
            // Replay after the input listener returns. Native hit testing, line
            // height, nested scrolling and lazy loading keep their usual path.
            state.borrow_mut().replaying = true;
            window.dispatch_event(PlatformInput::ScrollWheel(event), cx);
            state.borrow_mut().replaying = false;
        }
        if state.borrow().started.is_some() {
            schedule(state.clone(), window);
        } else {
            state.borrow_mut().queued = false;
        }
    });
}

pub(super) fn input(state: Rc<RefCell<WheelScroll>>) -> impl IntoElement {
    gpui_kit::canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            let wheel = state.clone();
            window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture || wheel.borrow().replaying {
                    return;
                }
                if cx.reduce_motion() || !matches!(event.delta, ScrollDelta::Lines(_)) {
                    wheel.borrow_mut().cancel();
                    return;
                }
                let ScrollDelta::Lines(delta) = event.delta else {
                    return;
                };
                if delta.x == 0. && delta.y == 0. {
                    return;
                }
                let mut motion = wheel.borrow_mut();
                motion.push(event, Instant::now());
                if !motion.queued {
                    motion.queued = true;
                    schedule(wheel.clone(), window);
                }
                cx.stop_propagation();
            });
            let wheel = state.clone();
            window.on_mouse_event(move |_: &gpui_kit::MouseDownEvent, phase, _, _| {
                if phase == DispatchPhase::Capture {
                    wheel.borrow_mut().cancel();
                }
            });
            let wheel = state.clone();
            window.on_mouse_event(move |event: &gpui_kit::MouseMoveEvent, phase, _, _| {
                if phase == DispatchPhase::Capture {
                    let mut motion = wheel.borrow_mut();
                    if motion
                        .event
                        .as_ref()
                        .is_some_and(|old| old.position != event.position)
                    {
                        motion.cancel();
                    }
                }
            });
            let wheel = state.clone();
            window.on_key_event(move |_: &gpui_kit::KeyDownEvent, phase, _, _| {
                if phase == DispatchPhase::Capture {
                    wheel.borrow_mut().cancel();
                }
            });
        },
    )
    .absolute()
    .size_0()
}

pub(super) fn cancel(state: &Rc<RefCell<WheelScroll>>) {
    state.borrow_mut().cancel();
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(y: f32) -> ScrollWheelEvent {
        ScrollWheelEvent {
            delta: ScrollDelta::Lines(point(0., y)),
            ..Default::default()
        }
    }
    #[test]
    fn wheel_distance_is_preserved_and_settles_without_idle_work() {
        let now = Instant::now();
        let mut wheel = WheelScroll::default();
        wheel.push(&event(-3.), now);
        let mut total = 0.;
        for ms in [16, 32, 48, 64, 80] {
            let step = wheel.step(now + Duration::from_millis(ms), false).unwrap();
            let ScrollDelta::Lines(delta) = step.delta else {
                panic!()
            };
            assert!(delta.y < 0.);
            total += delta.y;
        }
        assert!((total + 3.).abs() < 0.00001);
        assert!(wheel.step(now + SETTLE, false).is_none());
    }
    #[test]
    fn burst_coalesces_and_reversal_discards_old_direction() {
        let now = Instant::now();
        let mut wheel = WheelScroll::default();
        wheel.push(&event(-3.), now);
        wheel.push(&event(-3.), now);
        assert_eq!(wheel.remaining.y, -6.);
        wheel.push(&event(3.), now);
        let ScrollDelta::Lines(delta) = wheel.step(now + SETTLE, false).unwrap().delta else {
            panic!()
        };
        assert_eq!(delta.y, 3.);
    }
    #[test]
    fn cancellation_and_reduced_motion_leave_no_tail() {
        let now = Instant::now();
        let mut wheel = WheelScroll::default();
        wheel.push(&event(-3.), now);
        assert!(wheel.step(now, true).is_some());
        assert!(wheel.started.is_none());
        wheel.push(&event(-3.), now);
        wheel.cancel();
        assert!(wheel.step(now + SETTLE, false).is_none());
    }
}
