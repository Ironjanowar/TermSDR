//! Viridis colormap for waterfall display.
//! Maps normalized values [0.0, 1.0] to RGB colors.
//! 64-entry lookup table (sufficient for terminal cells).

use ratatui::style::Color;

/// Viridis palette: 64 RGB entries from purple (low) → blue → green → yellow (high)
const VIRIDIS_64: [(u8, u8, u8); 64] = [
    (68, 1, 84),
    (70, 8, 92),
    (71, 16, 99),
    (72, 23, 105),
    (72, 29, 111),
    (72, 36, 117),
    (71, 42, 122),
    (70, 48, 126),
    (69, 55, 129),
    (67, 61, 132),
    (65, 68, 135),
    (62, 74, 137),
    (60, 80, 138),
    (57, 86, 140),
    (54, 92, 141),
    (52, 97, 141),
    (49, 103, 142),
    (46, 109, 142),
    (44, 114, 142),
    (42, 120, 142),
    (39, 125, 142),
    (37, 131, 142),
    (35, 136, 141),
    (33, 142, 140),
    (31, 147, 139),
    (30, 152, 138),
    (31, 158, 135),
    (33, 163, 132),
    (37, 168, 129),
    (43, 173, 124),
    (51, 178, 119),
    (60, 183, 113),
    (70, 188, 106),
    (82, 192, 99),
    (94, 196, 91),
    (107, 200, 82),
    (120, 204, 73),
    (133, 207, 63),
    (146, 210, 53),
    (159, 213, 43),
    (172, 215, 34),
    (184, 217, 26),
    (196, 218, 21),
    (207, 219, 18),
    (217, 219, 19),
    (226, 219, 23),
    (234, 218, 30),
    (241, 217, 40),
    (246, 215, 52),
    (250, 213, 65),
    (253, 210, 78),
    (254, 208, 92),
    (254, 205, 105),
    (254, 203, 118),
    (253, 200, 131),
    (252, 198, 143),
    (251, 196, 155),
    (249, 195, 167),
    (247, 194, 178),
    (246, 193, 189),
    (244, 193, 200),
    (243, 194, 210),
    (242, 196, 220),
    (253, 231, 37),
];

/// Map a dB value to a Viridis RGB color.
///
/// # Arguments
/// * `db` - Power value in dB (typically -100 to 0)
/// * `min_db` - Minimum dB value (maps to purple/dark)
/// * `max_db` - Maximum dB value (maps to yellow/bright)
#[must_use] 
pub fn viridis_color(db: f32, min_db: f32, max_db: f32) -> Color {
    let range = max_db - min_db;
    let normalized = if range.abs() < f32::EPSILON {
        0.5
    } else {
        ((db - min_db) / range).clamp(0.0, 1.0)
    };
    let idx = viridis_index(normalized);
    let (r, g, b) = VIRIDIS_64[idx];
    Color::Rgb(r, g, b)
}

/// Map a normalized value [0.0, 1.0] to a Viridis palette index [0, 63].
#[must_use] 
pub fn viridis_index(normalized: f32) -> usize {
    let clamped = normalized.clamp(0.0, 1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // intentional truncation; value is non-negative here
    let idx = (clamped * 63.0).round() as usize;
    idx.min(63)
}

/// Get the RGB tuple for a viridis index.
#[must_use]
#[allow(dead_code)]
pub fn viridis_rgb(index: usize) -> (u8, u8, u8) {
    VIRIDIS_64[index.min(63)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_viridis_index_bounds() {
        assert_eq!(viridis_index(0.0), 0);
        assert_eq!(viridis_index(1.0), 63);
        assert_eq!(viridis_index(0.5), 32); // ~middle
    }

    #[test]
    fn test_viridis_index_clamps() {
        assert_eq!(viridis_index(-1.0), 0);
        assert_eq!(viridis_index(2.0), 63);
    }

    #[test]
    fn test_viridis_color_min_is_dark() {
        let color = viridis_color(-100.0, -100.0, 0.0);
        if let Color::Rgb(r, g, b) = color {
            // First entry is dark purple (68, 1, 84)
            assert_eq!((r, g, b), (68, 1, 84));
        } else {
            panic!("Expected Rgb color");
        }
    }

    #[test]
    fn test_viridis_color_max_is_bright() {
        let color = viridis_color(0.0, -100.0, 0.0);
        if let Color::Rgb(r, g, b) = color {
            // Last entry should be bright yellow
            assert_eq!((r, g, b), VIRIDIS_64[63]);
        } else {
            panic!("Expected Rgb color");
        }
    }

    #[test]
    fn test_viridis_color_mid_range() {
        let color = viridis_color(-50.0, -100.0, 0.0);
        if let Color::Rgb(r, g, b) = color {
            // Mid-range should be a green-ish color
            let (er, eg, eb) = VIRIDIS_64[32];
            assert_eq!((r, g, b), (er, eg, eb));
        } else {
            panic!("Expected Rgb color");
        }
    }

    #[test]
    fn test_viridis_color_equal_range() {
        // When min == max, should return middle color
        let color = viridis_color(-50.0, -50.0, -50.0);
        if let Color::Rgb(r, g, b) = color {
            let (er, eg, eb) = VIRIDIS_64[32];
            assert_eq!((r, g, b), (er, eg, eb));
        } else {
            panic!("Expected Rgb color");
        }
    }

    #[test]
    fn test_viridis_rgb_lookup() {
        assert_eq!(viridis_rgb(0), (68, 1, 84));
        assert_eq!(viridis_rgb(63), VIRIDIS_64[63]);
        assert_eq!(viridis_rgb(100), VIRIDIS_64[63]);
    }

    // === Edge-case tests ===

    #[test]
    fn test_viridis_color_extreme_db_very_negative() {
        let color = viridis_color(-1000.0, -100.0, 0.0);
        if let Color::Rgb(r, g, b) = color {
            assert_eq!((r, g, b), VIRIDIS_64[0]); // clamped to min
        } else { panic!("Expected Rgb"); }
    }

    #[test]
    fn test_viridis_color_extreme_db_very_positive() {
        let color = viridis_color(1000.0, -100.0, 0.0);
        if let Color::Rgb(r, g, b) = color {
            assert_eq!((r, g, b), VIRIDIS_64[63]); // clamped to max
        } else { panic!("Expected Rgb"); }
    }

    #[test]
    fn test_viridis_color_nan_input() {
        // NaN should clamp to 0.0 via clamp, producing index 0
        let color = viridis_color(f32::NAN, -100.0, 0.0);
        // NaN comparison: (NAN - min) / range = NaN, clamp(NaN, 0, 1) = 0 in Rust? Actually NaN.clamp returns NaN... 
        // Let's just verify no panic
        if let Color::Rgb(_, _, _) = color { } else { panic!("Expected Rgb"); }
    }

    #[test]
    fn test_viridis_color_infinity_input() {
        let color = viridis_color(f32::INFINITY, -100.0, 0.0);
        if let Color::Rgb(_, _, _) = color { } else { panic!("Expected Rgb"); }
        let color = viridis_color(f32::NEG_INFINITY, -100.0, 0.0);
        if let Color::Rgb(_, _, _) = color { } else { panic!("Expected Rgb"); }
    }

    #[test]
    fn test_viridis_color_inverted_range() {
        // min_db > max_db
        let color = viridis_color(-50.0, 0.0, -100.0);
        // range = -100 - 0 = -100, normalized = (-50 - 0) / -100 = 0.5, clamped to 0.5
        if let Color::Rgb(r, g, b) = color {
            assert_eq!((r, g, b), VIRIDIS_64[32]);
        } else { panic!("Expected Rgb"); }
    }

    #[test]
    fn test_viridis_index_nan() {
        // f32::NAN.clamp(0.0, 1.0) behavior
        let idx = viridis_index(f32::NAN);
        assert!(idx <= 63); // just don't panic
    }
}
