//! Waterfall display widget: scrolling time-frequency power visualization.

/// dB floor value for empty/missing waterfall data.
const WATERFALL_DB_FLOOR: f32 = -100.0;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
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
    /// Channel bandwidth in Hz.
    channel_bw: Option<f64>,
    /// Cursor frequency in Hz.
    cursor_freq: f64,
    /// Center frequency in Hz.
    center_freq: f64,
    /// Sample rate in Hz.
    sample_rate: f64,
    /// Range of captured bins covered by the displayed window.
    bin_range: std::ops::Range<usize>,
    /// VFO markers: (freq, `channel_bw`, color, id).
    vfos: Vec<(f64, f64, ratatui::style::Color, usize)>,
}

impl<'a> WaterfallWidget<'a> {
    #[must_use] 
    pub fn new(history: &'a [Vec<f32>], min_db: f32, max_db: f32) -> Self {
        Self {
            history,
            min_db,
            max_db,
            cursor_pos: None,
            channel_bw: None,
            cursor_freq: 0.0,
            center_freq: 0.0,
            sample_rate: 0.0,
            bin_range: 0..0,
            vfos: Vec::new(),
        }
    }

    /// Draw only `bin_range` of each row, matching the displayed window.
    ///
    /// Every row in the history was captured under the same geometry, so the same
    /// slice applies to all of them.
    #[must_use]
    pub fn with_bin_range(mut self, bin_range: std::ops::Range<usize>) -> Self {
        self.bin_range = bin_range;
        self
    }

    #[must_use] 
    pub fn with_cursor(mut self, pos: Option<f32>) -> Self {
        self.cursor_pos = pos.filter(|pos| pos.is_finite() && (0.0..=1.0).contains(pos));
        self
    }

    #[must_use] 
    pub fn with_bandwidth(
        mut self,
        bw: Option<f64>,
        cursor_freq: f64,
        center_freq: f64,
        sample_rate: f64,
    ) -> Self {
        self.channel_bw = bw;
        self.cursor_freq = cursor_freq;
        self.center_freq = center_freq;
        self.sample_rate = sample_rate;
        self
    }

    #[must_use] 
    pub fn with_vfos(mut self, vfos: &[(f64, f64, ratatui::style::Color, usize)]) -> Self {
        self.vfos = vfos.to_vec();
        self
    }
}

impl Widget for WaterfallWidget<'_> {
    #[allow(clippy::too_many_lines)] // complex rendering widget
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 2 || area.height < 1 {
            return;
        }

        let width = area.width as usize;
        let height = area.height as usize;

        for (row_idx, row_data) in self.history.iter().take(height).enumerate() {
            #[allow(clippy::cast_possible_truncation)] // intentional truncation
            let y = area.y + row_idx as u16;
            if y >= area.y + area.height {
                break;
            }

            // Resample only the bins the displayed window covers.
            let visible = row_data.get(self.bin_range.clone()).unwrap_or(&[]);
            let resampled = resample_waterfall_row(visible, width);

            for (col, &db_val) in resampled.iter().enumerate() {
                #[allow(clippy::cast_possible_truncation)] // intentional truncation
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
                #[allow(clippy::cast_possible_truncation)] // intentional truncation
                let y = area.y + row_idx as u16;
                for col in 0..width {
                    #[allow(clippy::cast_possible_truncation)] // intentional truncation
                    let x = area.x + col as u16;
                    buf.set_string(x, y, "█", Style::default().fg(dark));
                }
            }
        }

        // Draw cursor vertical line on waterfall
        if let Some(cursor_pos) = self.cursor_pos {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_precision_loss,
                clippy::cast_sign_loss
            )]
            // precision loss acceptable for DSP/UI; intentional truncation; value is non-negative here
            let cursor_x = area.x + (cursor_pos * width.saturating_sub(1) as f32) as u16;
            if cursor_x >= area.x && cursor_x < area.x + area.width {
                use ratatui::style::Color;
                // Cursor line is always solid yellow: presentation never depends
                // on audio mute state.
                for row in 0..height {
                    #[allow(clippy::cast_possible_truncation)] // intentional truncation
                    let y = area.y + row as u16;
                    if y >= area.y + area.height {
                        break;
                    }
                    buf.set_string(cursor_x, y, "│", Style::default().fg(Color::Yellow));
                }
            }
        }

        // Draw channel bandwidth lines
        if let Some(bw) = self.channel_bw {
            if self.sample_rate > 0.0 {
                let left_freq = self.cursor_freq - bw / 2.0;
                let right_freq = self.cursor_freq + bw / 2.0;
                let start_freq = self.center_freq - self.sample_rate / 2.0;

                let bw_color = Color::Rgb(60, 180, 180);

                for &edge_freq in &[left_freq, right_freq] {
                    #[allow(clippy::cast_possible_truncation)] // intentional truncation
                    let pos = ((edge_freq - start_freq) / self.sample_rate) as f32;
                    if (0.0..=1.0).contains(&pos) {
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_precision_loss,
                            clippy::cast_sign_loss
                        )]
                        // precision loss acceptable for DSP/UI; intentional truncation; value is non-negative here
                        let edge_x = area.x + (pos * width.saturating_sub(1) as f32) as u16;
                        if edge_x >= area.x && edge_x < area.x + area.width {
                            for row in 0..height {
                                if row % 2 == 0 {
                                    #[allow(clippy::cast_possible_truncation)]
                                    // intentional truncation
                                    let y = area.y + row as u16;
                                    if y < area.y + area.height {
                                        buf.set_string(
                                            edge_x,
                                            y,
                                            "┊",
                                            Style::default().fg(bw_color),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Draw VFO markers
        if self.sample_rate > 0.0 {
            let start_freq = self.center_freq - self.sample_rate / 2.0;
            for &(freq, bw, color, _id) in &self.vfos {
                let left_freq = freq - bw / 2.0;
                let right_freq = freq + bw / 2.0;

                for &edge_freq in &[left_freq, right_freq] {
                    #[allow(clippy::cast_possible_truncation)] // intentional truncation
                    let pos = ((edge_freq - start_freq) / self.sample_rate) as f32;
                    if (0.0..=1.0).contains(&pos) {
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_precision_loss,
                            clippy::cast_sign_loss
                        )]
                        // precision loss acceptable for DSP/UI; intentional truncation; value is non-negative here
                        let edge_x = area.x + (pos * width.saturating_sub(1) as f32) as u16;
                        if edge_x >= area.x && edge_x < area.x + area.width {
                            for row in 0..height {
                                if row % 2 == 0 {
                                    #[allow(clippy::cast_possible_truncation)]
                                    // intentional truncation
                                    let y = area.y + row as u16;
                                    if y < area.y + area.height {
                                        buf.set_string(edge_x, y, "┊", Style::default().fg(color));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Resample a row of FFT bins to fit the display width.
#[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
fn resample_waterfall_row(bins: &[f32], target_width: usize) -> Vec<f32> {
    if target_width == 0 || bins.is_empty() {
        return vec![WATERFALL_DB_FLOOR; target_width.max(1)];
    }

    #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
    let ratio = bins.len() as f64 / target_width as f64;
    (0..target_width)
        .map(|i| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_precision_loss,
                clippy::cast_sign_loss
            )]
            // precision loss acceptable for DSP/UI; intentional truncation; value is non-negative here
            let start = (i as f64 * ratio) as usize;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_precision_loss,
                clippy::cast_sign_loss
            )]
            // precision loss acceptable for DSP/UI; intentional truncation; value is non-negative here
            let end = ((i + 1) as f64 * ratio) as usize;
            let end = end.min(bins.len());
            if start < end {
                // Average for waterfall (smoother than max-pooling)
                let sum: f32 = bins[start..end].iter().sum();
                sum / (end - start) as f32
            } else {
                bins.get(start).copied().unwrap_or(WATERFALL_DB_FLOOR)
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

    // === Edge-case tests ===

    #[test]
    fn test_resample_waterfall_target_zero() {
        let bins = vec![-50.0, -40.0];
        let result = resample_waterfall_row(&bins, 0);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], -100.0);
    }

    #[test]
    fn test_resample_waterfall_target_one() {
        let bins = vec![-80.0, -60.0, -40.0, -20.0];
        let result = resample_waterfall_row(&bins, 1);
        assert_eq!(result.len(), 1);
        // Average of all bins
        let expected = (-80.0 + -60.0 + -40.0 + -20.0) / 4.0;
        assert!((result[0] - expected).abs() < 0.01);
    }

    #[test]
    fn test_resample_waterfall_larger_than_input() {
        let bins = vec![-50.0, -30.0];
        let result = resample_waterfall_row(&bins, 10);
        assert_eq!(result.len(), 10);
    }

    #[test]
    fn test_resample_waterfall_same_size() {
        let bins = vec![-80.0, -60.0, -40.0, -20.0];
        let result = resample_waterfall_row(&bins, 4);
        assert_eq!(result, bins);
    }

    #[test]
    fn test_resample_waterfall_nan_values() {
        let bins = vec![f32::NAN, -50.0];
        let result = resample_waterfall_row(&bins, 2);
        assert_eq!(result.len(), 2);
        // Just verify no panic
    }

    #[test]
    fn offscreen_cursor_is_discarded_before_rendering() {
        let history = vec![vec![-50.0; 64]];
        for pos in [-0.1, 1.1, f32::NAN] {
            let widget = WaterfallWidget::new(&history, -80.0, 0.0).with_cursor(Some(pos));
            assert_eq!(widget.cursor_pos, None);
        }
    }

    #[test]
    fn smoke_render_empty() {
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let history: Vec<Vec<f32>> = Vec::new();
        terminal
            .draw(|f| {
            let widget = WaterfallWidget::new(&history, -80.0, 0.0);
            f.render_widget(widget, f.area());
            })
            .unwrap();
    }

    #[test]
    fn smoke_render_with_data() {
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let history: Vec<Vec<f32>> = (0..10).map(|_| vec![-50.0; 64]).collect();
        terminal
            .draw(|f| {
            let widget = WaterfallWidget::new(&history, -80.0, 0.0)
                .with_cursor(Some(0.5))
                .with_bandwidth(Some(25_000.0), 100e6, 100e6, 2.048e6);
            f.render_widget(widget, f.area());
            })
            .unwrap();
    }
}
