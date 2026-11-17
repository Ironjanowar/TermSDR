//! Pure frequency-navigation calculations.

use crossterm::event::KeyModifiers;

/// Fine cursor movement step in Hz.
pub(crate) const CURSOR_STEP_FINE_HZ: f64 = 1_000.0;

/// Coarse cursor movement step in Hz.
pub(crate) const CURSOR_STEP_COARSE_HZ: f64 = 100_000.0;

/// Frequency geometry used by the frame currently displayed on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Viewport {
    pub(crate) center_freq: f64,
    pub(crate) sample_rate: f64,
}

impl Viewport {
    /// Compare frame metadata while ignoring a few floating-point representation ULPs.
    pub(crate) fn same_geometry(self, other: Self) -> bool {
        fn nearly_equal(left: f64, right: f64) -> bool {
            left.is_finite()
                && right.is_finite()
                && (left - right).abs() <= 4.0 * f64::EPSILON * left.abs().max(right.abs()).max(1.0)
        }

        nearly_equal(self.center_freq, other.center_freq)
            && nearly_equal(self.sample_rate, other.sample_rate)
    }
}

/// Cursor and viewport frequencies resulting from a navigation action.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FrequencyUpdate {
    pub(crate) cursor_freq: f64,
    pub(crate) center_freq: f64,
    pub(crate) center_changed: bool,
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
}
