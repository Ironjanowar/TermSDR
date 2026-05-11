use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    widgets::Widget,
};

use super::colors::viridis_color;

/// Waterfall display widget — scrolling color heatmap of FFT data over time.
/// Each row represents one FFT frame, newest at top, scrolling down.
pub struct WaterfallWidget<'a> {
    /// History of FFT frames (newest first).
    history: &'a [Vec<f32>],
    min_db: f32,
    max_db: f32,
    /// Cursor position (0.0–1.0), None if hidden.
    cursor_pos: Option<f32>,
    /// Whether cursor is active (solid) or inactive (dashed).
    cursor_active: bool,
}

impl<'a> WaterfallWidget<'a> {
    pub fn new(history: &'a [Vec<f32>], min_db: f32, max_db: f32) -> Self {
        Self {
            history,
            min_db,
            max_db,
            cursor_pos: None,
            cursor_active: true,
        }
    }

    pub fn with_cursor(mut self, pos: Option<f32>, active: bool) -> Self {
        self.cursor_pos = pos;
        self.cursor_active = active;
        self
    }
}

impl<'a> Widget for WaterfallWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 2 || area.height < 1 {
            return;
        }

        let width = area.width as usize;
        let height = area.height as usize;

        for (row_idx, row_data) in self.history.iter().take(height).enumerate() {
            let y = area.y + row_idx as u16;
            if y >= area.y + area.height {
                break;
            }

            // Resample the FFT bins to fit the display width
            let resampled = resample_waterfall_row(row_data, width);

            for (col, &db_val) in resampled.iter().enumerate() {
                let x = area.x + col as u16;
                if x >= area.x + area.width {
                    break;
                }
                let color = viridis_color(db_val, self.min_db, self.max_db);
                buf.set_string(x, y, "█", Style::default().fg(color));
            }
        }

        // Fill remaining rows with dark color if history is shorter than widget height
        if self.history.len() < height {
            let dark = viridis_color(self.min_db, self.min_db, self.max_db);
            for row_idx in self.history.len()..height {
                let y = area.y + row_idx as u16;
                for col in 0..width {
                    let x = area.x + col as u16;
                    buf.set_string(x, y, "█", Style::default().fg(dark));
                }
            }
        }

        // Draw cursor vertical line on waterfall
        if let Some(cursor_pos) = self.cursor_pos {
            let cursor_x = area.x + (cursor_pos * (width - 1) as f32) as u16;
            if cursor_x >= area.x && cursor_x < area.x + area.width {
                use ratatui::style::Color;
                let cursor_color = if self.cursor_active {
                    Color::Yellow
                } else {
                    Color::Rgb(180, 180, 60)
                };
                for row in 0..height {
                    let y = area.y + row as u16;
                    if y >= area.y + area.height {
                        break;
                    }
                    if self.cursor_active {
                        // Solid line
                        buf.set_string(cursor_x, y, "│", Style::default().fg(cursor_color));
                    } else {
                        // Dashed: every other row
                        if row % 2 == 0 {
                            buf.set_string(cursor_x, y, "┆", Style::default().fg(cursor_color));
                        }
                    }
                }
            }
        }
    }
}

/// Resample a row of FFT bins to fit the display width.
fn resample_waterfall_row(bins: &[f32], target_width: usize) -> Vec<f32> {
    if target_width == 0 || bins.is_empty() {
        return vec![-100.0; target_width.max(1)];
    }

    let ratio = bins.len() as f64 / target_width as f64;
    (0..target_width)
        .map(|i| {
            let start = (i as f64 * ratio) as usize;
            let end = ((i + 1) as f64 * ratio) as usize;
            let end = end.min(bins.len());
            if start < end {
                // Average for waterfall (smoother than max-pooling)
                let sum: f32 = bins[start..end].iter().sum();
                sum / (end - start) as f32
            } else {
                bins.get(start).copied().unwrap_or(-100.0)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resample_waterfall_row() {
        let bins = vec![-80.0, -60.0, -40.0, -20.0];
        let result = resample_waterfall_row(&bins, 2);
        assert_eq!(result.len(), 2);
        // Average of pairs
        assert!((result[0] - (-70.0)).abs() < 0.01);
        assert!((result[1] - (-30.0)).abs() < 0.01);
    }

    #[test]
    fn test_resample_waterfall_empty() {
        let result = resample_waterfall_row(&[], 5);
        assert_eq!(result.len(), 5);
    }
}
