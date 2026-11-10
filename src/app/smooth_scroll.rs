//! Smooth only discrete wheel steps. Precise input and scrollbar dragging keep
//! their native path. One fixed-size accumulator and one demand-driven callback
//! serve the whole window; there are no timers, per-row tasks, or idle frames.
use super::*;
use gpui_kit::{DispatchPhase, PlatformInput, Point, ScrollDelta, ScrollWheelEvent};
use std::{cell::RefCell, time::Instant};

const SETTLE: Duration = Duration::from_millis(80);
const RESPONSE_SECONDS: f32 = 0.024;
const FIRST_FRAME_FRACTION: f32 = 0.45;
const POINTER_TOLERANCE: Pixels = px(8.);
fn within_pointer_tolerance(a: Point<Pixels>, b: Point<Pixels>) -> bool {
    (a.x - b.x).abs().max((a.y - b.y).abs()) <= POINTER_TOLERANCE
}

#[derive(Default)]
pub(super) struct WheelScroll {
    event: Option<ScrollWheelEvent>,
    remaining: gpui_kit::Point<f32>,
    fresh: gpui_kit::Point<f32>,
    last_input: Option<Instant>,
    last_step: Option<Instant>,
    queued: bool,
    replaying: bool,
}

impl WheelScroll {
    fn cancel(&mut self) {
        self.event = None;
        self.remaining = point(0., 0.);
        self.fresh = point(0., 0.);
        self.last_input = None;
        self.last_step = None;
    }

    fn push(&mut self, event: &ScrollWheelEvent, now: Instant) {
        let ScrollDelta::Lines(delta) = event.delta else {
            return;
        };
        if self.event.as_ref().is_some_and(|old| {
            !within_pointer_tolerance(old.position, event.position)
                || old.modifiers != event.modifiers
        }) {
            self.cancel();
        }
        for (remaining, fresh, incoming) in [
            (&mut self.remaining.x, &mut self.fresh.x, delta.x),
            (&mut self.remaining.y, &mut self.fresh.y, delta.y),
        ] {
            // Reversals respond without finishing an old impulse.
            if incoming * *remaining < 0. {
                *remaining = 0.;
                *fresh = 0.;
            }
            *remaining += incoming;
            *fresh += incoming;
        }
        self.event = Some(event.clone());
        self.last_input = Some(now);
        self.last_step.get_or_insert(now);
    }

    fn step(&mut self, now: Instant, reduce_motion: bool) -> Option<ScrollWheelEvent> {
        let last_input = self.last_input?;
        let elapsed = now.saturating_duration_since(self.last_step?).as_secs_f32();
        let finished = reduce_motion || now.saturating_duration_since(last_input) >= SETTLE;
        let fraction = if finished {
            1.
        } else {
            // Frame-time based decay: incoming wheel ticks never restart it.
            -(-elapsed / RESPONSE_SECONDS).exp_m1()
        };
        let advance = |remaining: f32, fresh: f32| {
            let distance = (remaining.abs() * fraction)
                .max(fresh.abs() * FIRST_FRAME_FRACTION)
                .min(remaining.abs());
            distance.copysign(remaining)
        };
        let delta = point(
            advance(self.remaining.x, self.fresh.x),
            advance(self.remaining.y, self.fresh.y),
        );
        self.remaining.x -= delta.x;
        self.remaining.y -= delta.y;
        self.fresh = point(0., 0.);
        self.last_step = Some(now);
        let mut event = self.event.clone()?;
        event.delta = ScrollDelta::Lines(delta);
        if finished {
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
        if state.borrow().last_input.is_some() {
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
                        .is_some_and(|old| !within_pointer_tolerance(old.position, event.position))
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
    fn first_frame_responds_even_when_the_last_wheel_tick_arrives_just_before_it() {
        let now = Instant::now();
        let mut wheel = WheelScroll::default();
        for ms in [0, 2, 4, 6, 8, 10, 12, 14, 15] {
            wheel.push(&event(-1.), now + Duration::from_millis(ms));
        }
        let ScrollDelta::Lines(delta) = wheel
            .step(now + Duration::from_millis(16), false)
            .unwrap()
            .delta
        else {
            panic!()
        };
        assert!(
            delta.y <= -9. * FIRST_FRAME_FRACTION,
            "new input must move on its first frame"
        );
        assert_eq!(wheel.last_step, Some(now + Duration::from_millis(16)));
        let ScrollDelta::Lines(tail) = wheel
            .step(now + Duration::from_millis(95), false)
            .unwrap()
            .delta
        else {
            panic!()
        };
        assert!((delta.y + tail.y + 9.).abs() < 0.00001);
        assert!(wheel.last_input.is_none());
    }

    #[test]
    fn reversal_moves_in_the_new_direction_on_its_first_frame() {
        let now = Instant::now();
        let mut wheel = WheelScroll::default();
        wheel.push(&event(-3.), now);
        wheel.step(now + Duration::from_millis(16), false).unwrap();
        wheel.push(&event(3.), now + Duration::from_millis(20));
        let ScrollDelta::Lines(delta) = wheel
            .step(now + Duration::from_millis(21), false)
            .unwrap()
            .delta
        else {
            panic!()
        };
        assert!(
            delta.y >= 3. * FIRST_FRAME_FRACTION,
            "old-direction tail cannot resist reversal"
        );
        assert!(wheel.remaining.y > 0.);
    }

    #[test]
    fn sustained_input_preserves_distance_and_does_not_accumulate_a_long_tail() {
        let now = Instant::now();
        let mut wheel = WheelScroll::default();
        let mut total = 0.;
        for ms in (0..10_000).step_by(2) {
            wheel.push(&event(-1.), now + Duration::from_millis(ms));
            if ms % 16 == 0 {
                let ScrollDelta::Lines(delta) = wheel
                    .step(now + Duration::from_millis(ms), false)
                    .unwrap()
                    .delta
                else {
                    panic!()
                };
                total += delta.y;
                assert!(
                    wheel.remaining.y.abs() < 32.,
                    "pending movement must stay bounded during a burst"
                );
            }
        }
        let ScrollDelta::Lines(delta) = wheel
            .step(now + Duration::from_millis(10_080), false)
            .unwrap()
            .delta
        else {
            panic!()
        };
        total += delta.y;
        assert!(
            (total + 5000.).abs() < 0.01,
            "same-direction wheel distance must be conserved"
        );
        assert!(
            wheel
                .step(now + Duration::from_millis(10_100), false)
                .is_none()
        );
    }

    #[test]
    fn small_pointer_jitter_keeps_pending_distance_but_moving_away_cancels_it() {
        let now = Instant::now();
        let mut wheel = WheelScroll::default();
        wheel.push(&event(-3.), now);
        let mut jitter = event(-3.);
        jitter.position = point(px(1.), px(2.));
        wheel.push(&jitter, now);
        assert_eq!(wheel.remaining.y, -6.);
        let mut away = event(-3.);
        away.position = point(px(30.), px(2.));
        wheel.push(&away, now);
        assert_eq!(wheel.remaining.y, -3.);
    }

    #[test]
    fn cancellation_and_reduced_motion_leave_no_tail() {
        let now = Instant::now();
        let mut wheel = WheelScroll::default();
        wheel.push(&event(-3.), now);
        assert!(wheel.step(now, true).is_some());
        assert!(wheel.last_input.is_none());
        wheel.push(&event(-3.), now);
        wheel.cancel();
        assert!(wheel.step(now + SETTLE, false).is_none());
    }
}
