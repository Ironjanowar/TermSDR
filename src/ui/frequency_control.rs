//! Pure frequency-navigation calculations.

use crossterm::event::KeyModifiers;

/// Fine cursor movement step in Hz.
pub(crate) const CURSOR_STEP_FINE_HZ: f64 = 1_000.0;

/// Coarse cursor movement step in Hz.
pub(crate) const CURSOR_STEP_COARSE_HZ: f64 = 100_000.0;

/// Zoom step per keypress: one press halves (`in`) or doubles (`out`) the span.
pub(crate) const ZOOM_STEP: f64 = 0.5;

/// Smallest window worth rendering, in bins of the captured frame.
///
/// Zooming past a few bins stretches interpolation, not information: a single bin
/// smeared across the whole chart tells you no more than its own dB reading.
pub(crate) const MIN_VIEW_BINS: f64 = 4.0;

/// Cursor and viewport frequencies resulting from a navigation action.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FrequencyUpdate {
    pub(crate) cursor_freq: f64,
    pub(crate) center_freq: f64,
    pub(crate) center_changed: bool,
}

/// Compare frequencies while ignoring a few floating-point representation ULPs.
fn is_nearly_equal(left: f64, right: f64) -> bool {
    left.is_finite()
        && right.is_finite()
        && (left - right).abs() <= 4.0 * f64::EPSILON * left.abs().max(right.abs()).max(1.0)
}

/// The frequency window represented by one captured FFT frame.
///
/// The capture is itself a window, so every displayed view is either the whole
/// frame (unzoomed) or a sub-window of it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CapturedFrame {
    pub(crate) center_freq: f64,
    pub(crate) span_hz: f64,
    pub(crate) bin_count: usize,
}

impl CapturedFrame {
    pub(crate) fn left(self) -> f64 {
        self.center_freq - self.span_hz / 2.0
    }

    pub(crate) fn right(self) -> f64 {
        self.center_freq + self.span_hz / 2.0
    }

    /// Frequency width of a single FFT bin.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // bin counts are far below f64's exact integer range
    pub(crate) fn bin_width_hz(self) -> f64 {
        if self.bin_count == 0 {
            return self.span_hz;
        }
        self.span_hz / self.bin_count as f64
    }

    /// Narrowest span that still covers [`MIN_VIEW_BINS`] real bins.
    #[must_use]
    pub(crate) fn min_span_hz(self) -> f64 {
        (self.bin_width_hz() * MIN_VIEW_BINS).min(self.span_hz)
    }

    /// Whether two frames describe the same frequency window.
    ///
    /// Ignores `bin_count`, which changes resolution without moving the axis.
    #[must_use]
    pub(crate) fn same_geometry(self, other: Self) -> bool {
        is_nearly_equal(self.center_freq, other.center_freq)
            && is_nearly_equal(self.span_hz, other.span_hz)
    }
}

/// The window of frequencies currently on screen, as a sub-window of a capture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ViewWindow {
    pub(crate) center_freq: f64,
    pub(crate) span_hz: f64,
}

impl ViewWindow {
    pub(crate) fn left(self) -> f64 {
        self.center_freq - self.span_hz / 2.0
    }

    pub(crate) fn right(self) -> f64 {
        self.center_freq + self.span_hz / 2.0
    }

    /// Fraction (0.0–1.0) of the window occupied by `freq`, or `None` off-window.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // display precision is sufficient
    pub(crate) fn fraction_of(self, freq: f64) -> Option<f32> {
        if !self.span_hz.is_finite() || self.span_hz <= 0.0 || !freq.is_finite() {
            return None;
        }
        let fraction = ((freq - self.left()) / self.span_hz) as f32;
        fraction
            .is_finite()
            .then_some(fraction)
            .filter(|f| (0.0..=1.0).contains(f))
    }
}

/// Clamp a window center so a window of `span_hz` stays inside `frame`.
fn clamp_center(frame: CapturedFrame, span_hz: f64, center_freq: f64) -> f64 {
    let half = span_hz / 2.0;
    let lowest = frame.left() + half;
    let highest = frame.right() - half;
    if lowest >= highest {
        return frame.center_freq;
    }
    center_freq.clamp(lowest, highest)
}

/// Redisplay `frame` as a sub-window of `span_hz` while holding `anchor_freq` at
/// the screen position it already occupies.
///
/// Holding the anchor's *relative* position (not the window center) is what keeps
/// the signal under the cursor from sliding sideways when you zoom.
fn resize(
    frame: CapturedFrame,
    window: ViewWindow,
    anchor_freq: f64,
    requested_span: f64,
) -> ViewWindow {
    let span_hz = requested_span.clamp(frame.min_span_hz(), frame.span_hz);
    // An anchor off the current window (a cursor outside the capture) falls back to
    // the center, the only position that means anything then.
    let fraction = window.fraction_of(anchor_freq).map_or(0.5, f64::from);

    ViewWindow {
        center_freq: clamp_center(frame, span_hz, anchor_freq - (fraction - 0.5) * span_hz),
        span_hz,
    }
}

/// The display zoom: which captured frame is on screen, and how much of it.
///
/// Display-only state. Capture and audio run at the receiver's own rate, so
/// zooming never retunes hardware nor interrupts audio; it only re-windows the
/// bins of frames that arrive anyway.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ZoomState {
    pub(crate) frame: CapturedFrame,
    pub(crate) window: ViewWindow,
}

impl ZoomState {
    /// Show the whole of `frame`.
    #[must_use]
    pub(crate) fn from_frame(frame: CapturedFrame) -> Self {
        Self {
            frame,
            window: ViewWindow {
                center_freq: frame.center_freq,
                span_hz: frame.span_hz,
            },
        }
    }

    /// Halve the span, holding `cursor_freq` where it is on screen.
    pub(crate) fn zoom_in(&mut self, cursor_freq: f64) {
        self.window = resize(
            self.frame,
            self.window,
            cursor_freq,
            self.window.span_hz * ZOOM_STEP,
        );
    }

    /// Double the span, holding `cursor_freq` where it is on screen.
    pub(crate) fn zoom_out(&mut self, cursor_freq: f64) {
        self.window = resize(
            self.frame,
            self.window,
            cursor_freq,
            self.window.span_hz / ZOOM_STEP,
        );
    }

    /// Show an exact span around `anchor_freq` (the `b` bandwidth prompt).
    ///
    /// Non-finite and non-positive requests are ignored, so a mistyped prompt
    /// leaves the view untouched instead of blanking it.
    pub(crate) fn set_span(&mut self, span_hz: f64, anchor_freq: f64) {
        if !span_hz.is_finite() || span_hz <= 0.0 || !anchor_freq.is_finite() {
            return;
        }
        self.window = resize(self.frame, self.window, anchor_freq, span_hz);
    }

    /// Follow a newly captured frame, keeping `anchor_freq` on screen.
    ///
    /// A capture at a new rate resets the zoom: its bins cover different
    /// frequencies, so a window chosen for the old axis would mean something else.
    /// A capture that only moved keeps the span and re-centers on the anchor,
    /// which is the cursor that just forced the pan.
    pub(crate) fn follow_capture(&mut self, frame: CapturedFrame, anchor_freq: f64) {
        if self.frame.same_geometry(frame) {
            return;
        }
        let same_rate = is_nearly_equal(self.frame.span_hz, frame.span_hz);
        self.frame = frame;
        self.window = if same_rate {
            ViewWindow {
                center_freq: clamp_center(frame, self.window.span_hz, anchor_freq),
                span_hz: self.window.span_hz,
            }
        } else {
            ViewWindow {
                center_freq: frame.center_freq,
                span_hz: frame.span_hz,
            }
        };
    }

    /// The bin range of the captured frame visible in `window`, as `start..end`.
    ///
    /// Returned in the frame's own bin order, so a widget can slice the captured
    /// bins and resample only what the operator can actually see.
    #[must_use]
    pub(crate) fn visible_bin_range(self) -> std::ops::Range<usize> {
        let total = self.frame.bin_count;
        if total == 0 || !self.frame.span_hz.is_finite() || self.frame.span_hz <= 0.0 {
            return 0..total;
        }
        // Bin counts are far below f64's exact integer range, and both ends below
        // are clamped into `0..=total`, so neither conversion can round astray.
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let bin_of = |freq: f64| -> usize {
            let total_f = total as f64;
            let offset = ((freq - self.frame.left()) / self.frame.bin_width_hz()).floor();
            offset.clamp(0.0, total_f) as usize
        };
        bin_of(self.window.left())..bin_of(self.window.right()).max(bin_of(self.window.left()))
    }
}

/// Return the cursor movement step in Hz for the active modifiers.
pub(crate) fn cursor_step(modifiers: KeyModifiers) -> f64 {
    if modifiers.contains(KeyModifiers::SHIFT) {
        CURSOR_STEP_COARSE_HZ
    } else {
        CURSOR_STEP_FINE_HZ
    }
}

/// Move the cursor by `delta_hz`, minimally panning a valid visible range when needed.
pub(crate) fn move_cursor(
    cursor_freq: f64,
    center_freq: f64,
    visible_bw: f64,
    delta_hz: f64,
) -> FrequencyUpdate {
    let cursor_freq = cursor_freq + delta_hz;
    if !visible_bw.is_finite() || visible_bw <= 0.0 {
        return FrequencyUpdate {
            cursor_freq,
            center_freq,
            center_changed: false,
        };
    }

    let half_bw = visible_bw / 2.0;
    let left_edge = center_freq - half_bw;
    let right_edge = center_freq + half_bw;
    let (new_center, center_changed) = if cursor_freq < left_edge {
        (cursor_freq + half_bw, true)
    } else if cursor_freq > right_edge {
        (cursor_freq - half_bw, true)
    } else {
        (center_freq, false)
    };

    FrequencyUpdate {
        cursor_freq,
        center_freq: new_center,
        center_changed,
    }
}

/// Preserve the cursor frequency and center the viewport on it.
pub(crate) fn center_on_cursor(cursor_freq: f64) -> FrequencyUpdate {
    FrequencyUpdate {
        cursor_freq,
        center_freq: cursor_freq,
        center_changed: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;
    use crossterm::event::KeyModifiers;

    const FREQUENCY_EPSILON_HZ: f64 = 1.0e-6;

    fn assert_frequency_eq(actual: f64, expected: f64) {
        assert_abs_diff_eq!(actual, expected, epsilon = FREQUENCY_EPSILON_HZ);
    }

    fn assert_update(
        actual: FrequencyUpdate,
        expected_cursor: f64,
        expected_center: f64,
        expected_center_changed: bool,
    ) {
        assert_frequency_eq(actual.cursor_freq, expected_cursor);
        assert_frequency_eq(actual.center_freq, expected_center);
        assert_eq!(actual.center_changed, expected_center_changed);
    }

    #[test]
    fn fine_movement_is_exactly_one_kilohertz_in_both_directions() {
        assert_frequency_eq(cursor_step(KeyModifiers::NONE), 1_000.0);

        let right = move_cursor(100_000_000.0, 100_000_000.0, 2_000_000.0, 1_000.0);
        let left = move_cursor(100_000_000.0, 100_000_000.0, 2_000_000.0, -1_000.0);

        assert_frequency_eq(right.cursor_freq, 100_001_000.0);
        assert_frequency_eq(left.cursor_freq, 99_999_000.0);
    }

    #[test]
    fn coarse_movement_is_exactly_one_hundred_kilohertz_in_both_directions() {
        assert_frequency_eq(cursor_step(KeyModifiers::SHIFT), 100_000.0);

        let right = move_cursor(100_000_000.0, 100_000_000.0, 2_000_000.0, 100_000.0);
        let left = move_cursor(100_000_000.0, 100_000_000.0, 2_000_000.0, -100_000.0);

        assert_frequency_eq(right.cursor_freq, 100_100_000.0);
        assert_frequency_eq(left.cursor_freq, 99_900_000.0);
    }

    #[test]
    fn shift_selects_coarse_step_with_additional_modifiers() {
        let modifiers = KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT;

        assert_frequency_eq(cursor_step(modifiers), 100_000.0);
    }

    #[test]
    fn movement_inside_visible_range_leaves_center_unchanged() {
        let update = move_cursor(100_000_000.0, 100_000_000.0, 2_000_000.0, 500_000.0);

        assert_update(update, 100_500_000.0, 100_000_000.0, false);
    }

    #[test]
    fn crossing_left_edge_pans_center_only_enough_to_keep_cursor_visible() {
        let update = move_cursor(99_100_000.0, 100_000_000.0, 2_000_000.0, -200_000.0);

        assert_update(update, 98_900_000.0, 99_900_000.0, true);
    }

    #[test]
    fn crossing_right_edge_pans_center_only_enough_to_keep_cursor_visible() {
        let update = move_cursor(100_900_000.0, 100_000_000.0, 2_000_000.0, 200_000.0);

        assert_update(update, 101_100_000.0, 100_100_000.0, true);
    }

    #[test]
    fn centering_preserves_cursor_and_sets_center_to_it() {
        let update = center_on_cursor(118_100_000.0);

        assert_update(update, 118_100_000.0, 118_100_000.0, true);
    }

    #[test]
    fn non_default_bandwidth_only_affects_edge_detection() {
        let inside = move_cursor(50_000_000.0, 50_000_000.0, 400_000.0, 199_000.0);
        let outside = move_cursor(50_000_000.0, 50_000_000.0, 400_000.0, 201_000.0);

        assert_frequency_eq(inside.cursor_freq, 50_199_000.0);
        assert_frequency_eq(inside.center_freq, 50_000_000.0);
        assert!(!inside.center_changed);
        assert_frequency_eq(outside.cursor_freq, 50_201_000.0);
        assert_frequency_eq(outside.center_freq, 50_001_000.0);
        assert!(outside.center_changed);
    }

    #[test]
    fn invalid_or_non_positive_bandwidth_does_not_pan() {
        for visible_bw in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let update = move_cursor(100_000_000.0, 100_000_000.0, visible_bw, 2_000_000.0);

            assert_frequency_eq(update.cursor_freq, 102_000_000.0);
            assert_frequency_eq(update.center_freq, 100_000_000.0);
            assert!(!update.center_changed);
        }
    }

    // === Visualization zoom ===

    /// The capture every zoom test works against: 2.048 MS/s around 100 MHz.
    const FRAME_CENTER_HZ: f64 = 100_000_000.0;
    const FRAME_SPAN_HZ: f64 = 2_048_000.0;

    fn frame() -> CapturedFrame {
        CapturedFrame {
            center_freq: FRAME_CENTER_HZ,
            span_hz: FRAME_SPAN_HZ,
            bin_count: 2048,
        }
    }

    /// Where `freq` sits across `window`, as a 0.0–1.0 fraction.
    fn screen_fraction(window: ViewWindow, freq: f64) -> f64 {
        (freq - window.left()) / window.span_hz
    }

    #[test]
    fn zoom_in_holds_the_cursor_frequency_at_its_screen_position() {
        let cursor = 100_400_000.0;
        let mut view = ZoomState::from_frame(frame());
        let before = screen_fraction(view.window, cursor);

        view.zoom_in(cursor);

        assert_abs_diff_eq!(
            screen_fraction(view.window, cursor),
            before,
            epsilon = 1.0e-9
        );
        assert_abs_diff_eq!(view.window.span_hz, FRAME_SPAN_HZ * 0.5, epsilon = 1.0e-6);
    }

    #[test]
    fn zoom_out_holds_the_cursor_frequency_at_its_screen_position() {
        let cursor = 99_800_000.0;
        let mut view = ZoomState::from_frame(frame());
        view.zoom_in(cursor);
        let before = screen_fraction(view.window, cursor);

        view.zoom_out(cursor);

        assert_abs_diff_eq!(
            screen_fraction(view.window, cursor),
            before,
            epsilon = 1.0e-9
        );
        assert_abs_diff_eq!(view.window.span_hz, FRAME_SPAN_HZ, epsilon = 1.0e-6);
    }

    #[test]
    fn zoom_anchored_at_a_frame_edge_stays_inside_the_capture() {
        // A cursor on the left edge: an unclamped zoom would hang the window half
        // outside the frame its bins come from.
        let mut view = ZoomState::from_frame(frame());
        view.zoom_in(frame().left());

        assert!(view.window.left() >= frame().left() - 1.0e-6);
        assert!(view.window.right() <= frame().right() + 1.0e-6);
        assert_abs_diff_eq!(view.window.span_hz, FRAME_SPAN_HZ * 0.5, epsilon = 1.0e-6);
    }

    #[test]
    fn zoom_never_narrows_past_a_few_real_bins() {
        let mut view = ZoomState::from_frame(frame());
        for _ in 0..80 {
            view.zoom_in(view.window.center_freq);
        }

        let bin_width_hz = FRAME_SPAN_HZ / 2048.0;
        assert_abs_diff_eq!(
            view.window.span_hz,
            bin_width_hz * MIN_VIEW_BINS,
            epsilon = 1.0e-6
        );
    }

    #[test]
    fn zoom_out_never_widens_past_the_capture() {
        let mut view = ZoomState::from_frame(frame());
        view.zoom_in(FRAME_CENTER_HZ);
        for _ in 0..20 {
            view.zoom_out(FRAME_CENTER_HZ);
        }

        assert_abs_diff_eq!(view.window.span_hz, FRAME_SPAN_HZ, epsilon = 1.0e-6);
        assert_abs_diff_eq!(view.window.center_freq, FRAME_CENTER_HZ, epsilon = 1.0e-6);
    }

    #[test]
    fn a_zoomed_view_never_extends_past_the_capture() {
        let mut view = ZoomState::from_frame(frame());
        // Walk the cursor to the right edge, zooming at every step: each zoom
        // re-anchors the window, and the sequence must never leave the capture.
        for _ in 0..6 {
            view.zoom_in(frame().right());
            assert!(view.window.right() <= frame().right() + 1.0e-6);
            assert!(view.window.left() >= frame().left() - 1.0e-6);
        }
    }

    #[test]
    fn a_capture_at_a_new_rate_resets_the_zoom() {
        let mut view = ZoomState::from_frame(frame());
        view.zoom_in(FRAME_CENTER_HZ);
        assert!(view.window.span_hz < FRAME_SPAN_HZ);

        // Bins at a new rate cover different frequencies; keeping the old window
        // would draw them under an axis chosen for the previous rate.
        view.follow_capture(
            CapturedFrame {
                center_freq: 100_000_000.0,
                span_hz: 1_024_000.0,
                bin_count: 2048,
            },
            FRAME_CENTER_HZ,
        );

        assert_abs_diff_eq!(view.window.span_hz, 1_024_000.0, epsilon = 1.0e-6);
    }

    #[test]
    fn a_capture_that_only_moved_keeps_the_zoom_and_centers_on_the_anchor() {
        let mut view = ZoomState::from_frame(frame());
        for _ in 0..3 {
            view.zoom_in(FRAME_CENTER_HZ);
        }
        let span = view.window.span_hz;

        // The receiver paned to follow the cursor: same rate, new center.
        let moved = CapturedFrame {
            center_freq: 101_000_000.0,
            span_hz: FRAME_SPAN_HZ,
            bin_count: 2048,
        };
        view.follow_capture(moved, 100_900_000.0);

        assert_abs_diff_eq!(view.window.span_hz, span, epsilon = 1.0e-6);
        assert_abs_diff_eq!(view.window.center_freq, 100_900_000.0, epsilon = 1.0e-6);
        assert!(view.window.left() >= moved.left() - 1.0e-6);
    }

    #[test]
    fn a_capture_with_the_same_geometry_preserves_the_zoom() {
        let mut view = ZoomState::from_frame(frame());
        for _ in 0..3 {
            view.zoom_in(FRAME_CENTER_HZ);
        }
        let span = view.window.span_hz;

        view.follow_capture(frame(), FRAME_CENTER_HZ);

        assert_abs_diff_eq!(view.window.span_hz, span, epsilon = 1.0e-6);
    }

    #[test]
    fn an_exact_span_request_is_honoured_and_clamped_to_the_capture() {
        // `b 20k` on a 2.048 MS/s capture must be exact: this is the case a user
        // reaches for when a 6 kHz signal fills too little of the screen.
        let mut view = ZoomState::from_frame(frame());
        view.set_span(20_000.0, FRAME_CENTER_HZ + 100_000.0);
        assert_abs_diff_eq!(view.window.span_hz, 20_000.0, epsilon = 1.0e-6);

        // A request wider than the capture clamps to the capture, not to infinity.
        view.set_span(50_000_000.0, FRAME_CENTER_HZ);
        assert_abs_diff_eq!(view.window.span_hz, FRAME_SPAN_HZ, epsilon = 1.0e-6);
    }

    #[test]
    fn a_non_finite_span_request_is_rejected() {
        for span in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1_000.0] {
            let mut view = ZoomState::from_frame(frame());
            view.zoom_in(FRAME_CENTER_HZ);
            let span_before = view.window.span_hz;

            view.set_span(span, FRAME_CENTER_HZ);

            assert_abs_diff_eq!(view.window.span_hz, span_before, epsilon = 1.0e-6);
        }
    }

    #[test]
    fn an_exact_span_never_exceeds_the_capture_even_when_anchored_at_an_edge() {
        let mut view = ZoomState::from_frame(frame());
        view.set_span(100_000.0, frame().right());

        assert!(view.window.right() <= frame().right() + 1.0e-6);
        assert_abs_diff_eq!(view.window.span_hz, 100_000.0, epsilon = 1.0e-6);
    }

    #[test]
    fn a_zoomed_window_maps_frequencies_to_screen_fractions() {
        let mut view = ZoomState::from_frame(frame());
        view.set_span(10_000.0, FRAME_CENTER_HZ);

        let left = view.window.left();
        assert_abs_diff_eq!(
            f64::from(view.window.fraction_of(left).unwrap()),
            0.0,
            epsilon = 1.0e-6
        );
        assert_abs_diff_eq!(
            f64::from(view.window.fraction_of(FRAME_CENTER_HZ).unwrap()),
            0.5,
            epsilon = 1.0e-6
        );
        assert_eq!(view.window.fraction_of(FRAME_CENTER_HZ + 10_000.0), None);
    }

    #[test]
    fn the_visible_bin_range_covers_only_the_zoomed_window() {
        let mut view = ZoomState::from_frame(frame());
        view.set_span(FRAME_SPAN_HZ / 4.0, FRAME_CENTER_HZ);

        assert_eq!(view.visible_bin_range(), 768..1280);
    }

    #[test]
    fn the_visible_bin_range_of_a_full_view_is_every_bin() {
        assert_eq!(ZoomState::from_frame(frame()).visible_bin_range(), 0..2048);
    }

    #[test]
    fn the_visible_bin_range_never_leaves_the_captured_bins() {
        let mut view = ZoomState::from_frame(frame());
        view.set_span(10_000.0, frame().right());

        let range = view.visible_bin_range();
        assert!(range.end <= 2048, "range {range:?} past the captured bins");
        assert!(range.start < range.end, "empty range {range:?}");
    }

    #[test]
    fn a_capture_with_no_bins_shows_every_bin_of_nothing() {
        let empty = CapturedFrame {
            center_freq: FRAME_CENTER_HZ,
            span_hz: FRAME_SPAN_HZ,
            bin_count: 0,
        };
        assert_eq!(ZoomState::from_frame(empty).visible_bin_range(), 0..0);
    }
}
