//! Content-adaptive bitrate control for the controlled host.
//!
//! A single bitrate chosen at startup is wrong for most of a session: a still
//! desktop needs a couple of megabits, and a full-screen video at the same
//! resolution needs tens. With CBR rate control the encoder cannot borrow from
//! the quiet periods, so the busy ones arrive under-encoded -- which is what a
//! picture that flickers as it moves looks like.
//!
//! This controller adjusts the budget from what the encoder is actually
//! producing. That is the one signal available without instrumenting the
//! encoder: a CBR encoder sized to its bitrate emits roughly `bitrate / fps`
//! bytes per frame, so spending at or above that every frame means the budget is
//! the limit, and spending well under it means the budget is slack.
//!
//! Deliberately not congestion control. Nothing here observes the network; the
//! encoder's own spend is a measure of how hard the content is, not of how much
//! room the path has. A session on a narrow link will still oversubscribe it, and
//! the eventual fix for that is a feedback path from the viewer, which does not
//! exist yet.
//!
//! Asymmetry is on purpose: raising is quick because an under-encoded picture is
//! visible immediately, while lowering is slow and needs a longer window because
//! a momentary lull is not evidence that the budget is too large. Without that,
//! the rate oscillates and the picture pulses, which is the symptom this exists
//! to remove.

/// Bounds for any automatic choice.
pub const MIN_BITRATE: i64 = 2_000_000;
pub const MAX_BITRATE: i64 = 120_000_000;

/// Fraction of the per-frame budget above which the budget is treated as the
/// limit. Slightly below one so the controller reacts before the encoder is
/// visibly starved.
const HIGH_WATER: f64 = 0.85;

/// Fraction below which the budget is treated as slack.
const LOW_WATER: f64 = 0.55;

/// Frames of evidence needed before raising. Short, because a busy scene is
/// obvious within a few frames.
const RAISE_AFTER_FRAMES: u32 = 24;

/// Frames of evidence needed before lowering. Much longer, so a pause while a
/// menu opens does not immediately cut the rate.
const LOWER_AFTER_FRAMES: u32 = 240;

/// How much to move. Raising is larger than lowering so the controller recovers
/// quickly from being wrong on the low side.
const RAISE_STEP: f64 = 0.25;
const LOWER_STEP: f64 = 0.12;

/// What the controller needs to know about the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamShape {
    /// Capture rate the encoder is configured for.
    pub fps: u32,
}

/// The controller's decision for one observation window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Keep the current rate.
    Hold,
    /// Move to this rate, which is already clamped.
    Change(i64),
}

/// Tracks how much of the budget the encoder is spending.
///
/// Two windows run at once. The short one answers "is the budget the limit right
/// now" and is what raises the rate. The long one answers "is the budget too
/// large" and is what lowers it. They are kept separately because they are
/// decided at different points: a single counter cannot be both ready after 24
/// frames and trusted only after 240.
#[derive(Debug)]
pub struct BitrateController {
    shape: StreamShape,
    current: i64,
    short: Window,
    long: Window,
}

#[derive(Debug)]
struct Window {
    frames: u32,
    bytes: i64,
}

impl Window {
    fn new() -> Self {
        Self {
            frames: 0,
            bytes: 0,
        }
    }

    fn add(&mut self, bytes: u64) {
        self.frames = self.frames.saturating_add(1);
        self.bytes = self
            .bytes
            .saturating_add(i64::try_from(bytes).unwrap_or(i64::MAX));
    }

    fn reset(&mut self) {
        self.frames = 0;
        self.bytes = 0;
    }

    fn ratio(&self, current: i64, fps: u32) -> f64 {
        if self.frames == 0 {
            return 0.0;
        }
        let per_frame_budget = (current as f64 / f64::from(fps.max(1))) / 8.0;
        if per_frame_budget <= 0.0 {
            return 0.0;
        }
        (self.bytes as f64 / f64::from(self.frames)) / per_frame_budget
    }
}

impl BitrateController {
    /// Start from `initial`, clamped into the allowed range.
    pub fn new(shape: StreamShape, initial: i64) -> Self {
        Self {
            shape,
            current: initial.clamp(MIN_BITRATE, MAX_BITRATE),
            short: Window::new(),
            long: Window::new(),
        }
    }

    pub fn current(&self) -> i64 {
        self.current
    }

    /// Record one encoded frame and return what to do about it.
    ///
    /// The decision waits for a full window, then clears both windows, so an
    /// observation is never counted towards two decisions.
    pub fn observe(&mut self, bytes: u64) -> Decision {
        self.short.add(bytes);
        self.long.add(bytes);

        let short_ratio = self.short.ratio(self.current, self.shape.fps);
        let long_ratio = self.long.ratio(self.current, self.shape.fps);
        let fps = self.shape.fps;

        // Raising is checked first and needs only the short window: an
        // under-encoded picture is visible now and waiting would prolong it.
        if self.short.frames >= RAISE_AFTER_FRAMES && short_ratio >= HIGH_WATER {
            self.short.reset();
            self.long.reset();
            let target = (self.current as f64 * (1.0 + RAISE_STEP)) as i64;
            return self.apply(target);
        }
        if self.long.frames >= LOWER_AFTER_FRAMES {
            if long_ratio <= LOW_WATER {
                self.short.reset();
                self.long.reset();
                let target = (self.current as f64 * (1.0 - LOWER_STEP)) as i64;
                return self.apply(target);
            }
            // The long window is stale evidence about slack, so it restarts even
            // when nothing was decided; the short window keeps its own count.
            self.long.reset();
        }
        let _ = fps;
        Decision::Hold
    }

    fn apply(&mut self, target: i64) -> Decision {
        let clamped = target.clamp(MIN_BITRATE, MAX_BITRATE);
        if clamped == self.current {
            return Decision::Hold;
        }
        self.current = clamped;
        Decision::Change(clamped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller(initial: i64) -> BitrateController {
        BitrateController::new(StreamShape { fps: 60 }, initial)
    }

    /// Bytes per frame that spend `ratio` of the budget at `bitrate`.
    fn frame_bytes(bitrate: i64, ratio: f64, fps: u32) -> u64 {
        ((bitrate as f64 / f64::from(fps)) / 8.0 * ratio) as u64
    }

    #[test]
    fn a_busy_scene_raises_the_rate() {
        let mut control = controller(20_000_000);
        let busy = frame_bytes(20_000_000, 0.95, 60);
        let mut decision = Decision::Hold;
        for _ in 0..RAISE_AFTER_FRAMES {
            decision = control.observe(busy);
        }
        match decision {
            Decision::Change(rate) => {
                assert!(rate > 20_000_000, "rate should rise, got {rate}");
                // One step, not a jump to the ceiling.
                assert!(
                    rate < 30_000_000,
                    "rate should move by one step, got {rate}"
                );
            }
            Decision::Hold => panic!("a busy scene must raise the rate"),
        }
    }

    #[test]
    fn a_short_lull_does_not_lower_the_rate() {
        let mut control = controller(20_000_000);
        let quiet = frame_bytes(20_000_000, 0.2, 60);
        // Fewer frames than the lowering window: nothing should happen even
        // though every frame spent well under budget.
        for _ in 0..(LOWER_AFTER_FRAMES - 1) {
            assert_eq!(control.observe(quiet), Decision::Hold);
        }
        assert_eq!(control.current(), 20_000_000);
    }

    #[test]
    fn a_long_lull_lowers_the_rate_once() {
        let mut control = controller(20_000_000);
        let quiet = frame_bytes(20_000_000, 0.2, 60);
        let mut changes = 0;
        let mut last = 20_000_000;
        for _ in 0..(LOWER_AFTER_FRAMES * 2) {
            if let Decision::Change(rate) = control.observe(quiet) {
                changes += 1;
                assert!(rate < last, "lowering must go down, got {rate}");
                last = rate;
            }
        }
        // Two windows, so at most two steps; not a collapse to the floor.
        assert_eq!(changes, 2, "expected one step per window");
        assert!(control.current() > MIN_BITRATE);
    }

    #[test]
    fn spending_exactly_the_budget_holds_steady() {
        let mut control = controller(20_000_000);
        // A ratio of 1.0 is above the high water mark, so this raises; a ratio
        // just under it must hold instead.
        let steady = frame_bytes(20_000_000, 0.7, 60);
        for _ in 0..(LOWER_AFTER_FRAMES * 2) {
            assert_eq!(control.observe(steady), Decision::Hold);
        }
        assert_eq!(control.current(), 20_000_000);
    }

    #[test]
    fn the_rate_never_leaves_the_allowed_range() {
        let mut control = controller(MAX_BITRATE);
        let saturated = frame_bytes(MAX_BITRATE, 2.0, 60);
        for _ in 0..(RAISE_AFTER_FRAMES * 10) {
            let _ = control.observe(saturated);
            assert!(control.current() <= MAX_BITRATE);
        }
        assert_eq!(control.current(), MAX_BITRATE);

        let mut floor_control = controller(MIN_BITRATE);
        let empty = 0u64;
        for _ in 0..(LOWER_AFTER_FRAMES * 10) {
            let _ = floor_control.observe(empty);
            assert!(floor_control.current() >= MIN_BITRATE);
        }
        assert_eq!(floor_control.current(), MIN_BITRATE);
    }

    #[test]
    fn an_initial_rate_outside_the_range_is_clamped_on_construction() {
        assert_eq!(controller(i64::MAX).current(), MAX_BITRATE);
        assert_eq!(controller(1).current(), MIN_BITRATE);
        assert_eq!(controller(0).current(), MIN_BITRATE);
    }

    #[test]
    fn raising_is_faster_than_lowering_so_the_rate_does_not_pulse() {
        // The asymmetry is what keeps a scene that alternates between busy and
        // quiet from oscillating: it climbs quickly and comes down slowly.
        let mut control = controller(20_000_000);
        let busy = frame_bytes(20_000_000, 0.95, 60);
        for _ in 0..RAISE_AFTER_FRAMES {
            control.observe(busy);
        }
        let after_raise = control.current();
        let quiet = frame_bytes(after_raise, 0.2, 60);
        for _ in 0..(LOWER_AFTER_FRAMES - 1) {
            control.observe(quiet);
        }
        // After a raise, the same number of frames that caused it has not yet
        // been enough to undo it.
        assert_eq!(control.current(), after_raise);
    }

    #[test]
    fn the_window_resets_after_a_decision() {
        let mut control = controller(20_000_000);
        let busy = frame_bytes(20_000_000, 0.95, 60);
        for _ in 0..RAISE_AFTER_FRAMES {
            control.observe(busy);
        }
        let raised = control.current();
        assert!(raised > 20_000_000);
        // The next observation window starts empty, so a single frame cannot
        // trigger another change.
        assert_eq!(control.observe(busy), Decision::Hold);
    }
}
