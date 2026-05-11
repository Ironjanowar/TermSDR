use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::Widget,
};

use crate::types::FftResult;

/// Spectrum display widget — renders FFT bins as a line chart.
/// Shows power (dB) vs frequency using braille-like block characters.
pub struct SpectrumWidget<'a> {
    fft_result: Option<&'a FftResult>,
    min_db: f32,
    max_db: f32,
    /// Cursor position (0.0–1.0), None if cursor hidden entirely.
    cursor_pos: Option<f32>,
    /// Whether cursor is active (solid) or inactive (dashed).
    cursor_active: bool,
    /// Frequency at cursor.
    cursor_freq: f64,
    /// dB value at cursor.
    cursor_db: Option<f32>,
    /// Channel bandwidth in Hz.
    channel_bw: Option<f64>,
    /// Center frequency for bandwidth calculation.
    center_freq: f64,
    /// Sample rate for bandwidth calculation.
    sample_rate: f64,
}

impl<'a> SpectrumWidget<'a> {
    pub fn new(fft_result: Option<&'a FftResult>, min_db: f32, max_db: f32) -> Self {
        Self {
            fft_result,
            min_db,
            max_db,
            cursor_pos: None,
            cursor_active: true,
            cursor_freq: 0.0,
            cursor_db: None,
            channel_bw: None,
            center_freq: 0.0,
            sample_rate: 0.0,
        }
    }

    pub fn with_cursor(mut self, pos: Option<f32>, active: bool, freq: f64, db: Option<f32>) -> Self {
        self.cursor_pos = pos;
        self.cursor_active = active;
        self.cursor_freq = freq;
        self.cursor_db = db;
        self
    }

    pub fn with_bandwidth(mut self, bw: Option<f64>, center_freq: f64, sample_rate: f64) -> Self {
        self.channel_bw = bw;
        self.center_freq = center_freq;
        self.sample_rate = sample_rate;
        self
    }
}

impl<'a> Widget for SpectrumWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 4 || area.height < 3 {
            return;
        }

        let fft = match self.fft_result {
            Some(f) => f,
            None => {
                // Draw empty state
                let msg = "Waiting for data...";
                let x = area.x + (area.width.saturating_sub(msg.len() as u16)) / 2;
                let y = area.y + area.height / 2;
                if y < area.y + area.height && x < area.x + area.width {
                    buf.set_string(x, y, msg, Style::default().fg(Color::DarkGray));
                }
                return;
            }
        };

        // Reserve left margin for dB labels (6 chars) and bottom row for freq labels
        let y_label_width: u16 = 7;
        let chart_x = area.x + y_label_width;
        let chart_width = area.width.saturating_sub(y_label_width);
        let chart_height = area.height.saturating_sub(2); // 1 for border, 1 for freq labels

        if chart_width < 2 || chart_height < 2 {
            return;
        }

        // Draw title
        let title = " Spectrum ";
        let title_x = chart_x + (chart_width.saturating_sub(title.len() as u16)) / 2;
        buf.set_string(title_x, area.y, title, Style::default().fg(Color::Cyan));

        // Resample FFT bins to chart width
        let bins = &fft.bins;
        let resampled = resample_bins(bins, chart_width as usize);

        // Draw dB axis labels
        let db_labels = 5.min(chart_height as usize);
        for i in 0..db_labels {
            let y = area.y + 1 + (i as u16 * (chart_height - 1)) / (db_labels as u16 - 1).max(1);
            let db_val = self.max_db - (i as f32 / (db_labels as f32 - 1.0).max(1.0)) * (self.max_db - self.min_db);
            let label = format!("{:>5.0}dB", db_val);
            if y < area.y + area.height - 1 {
                buf.set_string(area.x, y, &label[..label.len().min(y_label_width as usize)], Style::default().fg(Color::DarkGray));
            }
        }

        // Draw the spectrum line
        let chart_y_start = area.y + 1;
        for col in 0..chart_width as usize {
            let db_val = resampled[col];
            let normalized = ((db_val - self.min_db) / (self.max_db - self.min_db)).clamp(0.0, 1.0);
            let bar_height = (normalized * chart_height as f32) as u16;

            // Draw filled column from bottom up
            for row in 0..chart_height {
                let y = chart_y_start + chart_height - 1 - row;
                let x = chart_x + col as u16;
                if x < area.x + area.width && y < area.y + area.height - 1 {
                    if row < bar_height {
                        // Gradient color based on height
                        let intensity = row as f32 / chart_height as f32;
                        let color = spectrum_gradient(intensity);
                        buf.set_string(x, y, "█", Style::default().fg(color));
                    } else if row == bar_height {
                        // Top of the bar — use a half block for better resolution
                        let color = spectrum_gradient(row as f32 / chart_height as f32);
                        buf.set_string(x, y, "▄", Style::default().fg(color));
                    }
                }
            }
        }

        // Draw frequency labels at bottom
        let freq_y = area.y + area.height - 1;
        if let Some(fft) = self.fft_result {
            let freq_labels = generate_freq_labels(
                fft.center_freq,
                fft.sample_rate,
                chart_width as usize,
            );
            let label_spacing = chart_width as usize / freq_labels.len().max(1);
            for (i, label) in freq_labels.iter().enumerate() {
                let x = chart_x + (i * label_spacing) as u16;
                if x + label.len() as u16 <= area.x + area.width {
                    buf.set_string(x, freq_y, label, Style::default().fg(Color::DarkGray));
                }
            }
        }

        // Draw cursor (vertical line + label)
        if let Some(cursor_pos) = self.cursor_pos {
            let cursor_x = chart_x + (cursor_pos * (chart_width - 1) as f32) as u16;
            if cursor_x >= chart_x && cursor_x < chart_x + chart_width {
                // Vertical line through spectrum
                // Solid line when active, dashed when inactive
                let (active, dash_char) = match self.cursor_active {
                    true => (true, "│"),
                    false => (false, "┆"),
                };
                let cursor_color = if active { Color::Yellow } else { Color::Rgb(180, 180, 60) };
                for row in 0..chart_height {
                    let y = chart_y_start + row;
                    if y < area.y + area.height - 1 {
                        if active {
                            buf.set_string(cursor_x, y, dash_char, Style::default().fg(cursor_color));
                        } else {
                            // Dashed: show every other row
                            if row % 2 == 0 {
                                buf.set_string(cursor_x, y, "┆", Style::default().fg(cursor_color));
                            }
                        }
                    }
                }

                // Cursor info label at top
                let freq_str = format_frequency(self.cursor_freq);
                let db_str = match self.cursor_db {
                    Some(db) => format!("{:.1}dB", db),
                    None => "--dB".to_string(),
                };
                let label = format!(" {} | {} ", freq_str, db_str);
                let label_len = label.len() as u16;
                // Position label to avoid going off-screen
                let label_x = if cursor_x + label_len + 1 < area.x + area.width {
                    cursor_x + 1
                } else {
                    cursor_x.saturating_sub(label_len + 1)
                };
                let label_y = area.y;
                if label_y < area.y + area.height {
                    let label_style = if active {
                        Style::default().fg(Color::Black).bg(Color::Yellow)
                    } else {
                        Style::default().fg(Color::Rgb(180, 180, 60)).bg(Color::Rgb(40, 40, 40))
                    };
                    buf.set_string(
                        label_x,
                        label_y,
                        &label,
                        label_style,
                    );
                }
            }
        }

        // Draw channel bandwidth lines
        if let (Some(cursor_pos), Some(bw)) = (self.cursor_pos, self.channel_bw) {
            if self.sample_rate > 0.0 {
                let cursor_freq_hz = self.center_freq - self.sample_rate / 2.0 + cursor_pos as f64 * self.sample_rate;
                let left_freq = cursor_freq_hz - bw / 2.0;
                let right_freq = cursor_freq_hz + bw / 2.0;
                let start_freq = self.center_freq - self.sample_rate / 2.0;

                let bw_color = Color::Rgb(60, 180, 180);

                for &edge_freq in &[left_freq, right_freq] {
                    let pos = ((edge_freq - start_freq) / self.sample_rate) as f32;
                    if pos >= 0.0 && pos <= 1.0 {
                        let edge_x = chart_x + (pos * (chart_width - 1) as f32) as u16;
                        if edge_x >= chart_x && edge_x < chart_x + chart_width {
                            for row in 0..chart_height {
                                if row % 2 == 0 {
                                    let y = chart_y_start + row;
                                    if y < area.y + area.height - 1 {
                                        buf.set_string(edge_x, y, "┊", Style::default().fg(bw_color));
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

/// Gradient from blue (bottom/quiet) to green (mid) to yellow (top/loud)
fn spectrum_gradient(intensity: f32) -> Color {
    let t = intensity.clamp(0.0, 1.0);
    if t < 0.5 {
        let f = t * 2.0;
        Color::Rgb(
            0,
            (100.0 + 155.0 * f) as u8,
            (200.0 * (1.0 - f)) as u8,
        )
    } else {
        let f = (t - 0.5) * 2.0;
        Color::Rgb(
            (255.0 * f) as u8,
            255,
            0,
        )
    }
}

/// Resample FFT bins to fit chart width using max-pooling.
pub fn resample_bins(bins: &[f32], target_width: usize) -> Vec<f32> {
    if target_width == 0 || bins.is_empty() {
        return vec![-100.0; target_width.max(1)];
    }

    let mut result = Vec::with_capacity(target_width);
    let ratio = bins.len() as f64 / target_width as f64;

    for i in 0..target_width {
        let start = (i as f64 * ratio) as usize;
        let end = ((i + 1) as f64 * ratio) as usize;
        let end = end.min(bins.len());
        if start < end {
            let max_val = bins[start..end]
                .iter()
                .cloned()
                .fold(f32::NEG_INFINITY, f32::max);
            result.push(max_val);
        } else {
            result.push(bins.get(start).copied().unwrap_or(-100.0));
        }
    }

    result
}

/// Generate frequency labels for the X axis.
pub fn generate_freq_labels(center_freq: f64, sample_rate: f64, width: usize) -> Vec<String> {
    let num_labels = (width / 15).max(2).min(7);
    let start_freq = center_freq - sample_rate / 2.0;

    (0..num_labels)
        .map(|i| {
            let freq = start_freq + (i as f64 / (num_labels - 1).max(1) as f64) * sample_rate;
            format_frequency(freq)
        })
        .collect()
}

/// Format a frequency in Hz to a human-readable string.
pub fn format_frequency(freq: f64) -> String {
    if freq >= 1e9 {
        format!("{:.3}G", freq / 1e9)
    } else if freq >= 1e6 {
        format!("{:.3}M", freq / 1e6)
    } else if freq >= 1e3 {
        format!("{:.1}k", freq / 1e3)
    } else {
        format!("{:.0}", freq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_frequency_mhz() {
        assert_eq!(format_frequency(100_000_000.0), "100.000M");
        assert_eq!(format_frequency(118_100_000.0), "118.100M");
    }

    #[test]
    fn test_format_frequency_ghz() {
        assert_eq!(format_frequency(1_420_000_000.0), "1.420G");
    }

    #[test]
    fn test_format_frequency_khz() {
        assert_eq!(format_frequency(433_920.0), "433.9k");
    }

    #[test]
    fn test_generate_freq_labels() {
        let labels = generate_freq_labels(100e6, 2.048e6, 80);
        assert!(labels.len() >= 2);
        // First label should be near start frequency
        assert!(labels[0].contains("M"));
        // Last label should be near end frequency
        assert!(labels.last().unwrap().contains("M"));
    }

    #[test]
    fn test_resample_bins_downscale() {
        let bins = vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0];
        let result = resample_bins(&bins, 4);
        assert_eq!(result.len(), 4);
        // Max-pooling: each output bin is the max of 2 input bins
        assert_eq!(result[0], 1.0);
        assert_eq!(result[1], 3.0);
        assert_eq!(result[2], 5.0);
        assert_eq!(result[3], 7.0);
    }

    #[test]
    fn test_resample_bins_same_size() {
        let bins = vec![1.0, 2.0, 3.0, 4.0];
        let result = resample_bins(&bins, 4);
        assert_eq!(result, bins);
    }

    #[test]
    fn test_resample_bins_empty() {
        let result = resample_bins(&[], 10);
        assert_eq!(result.len(), 10);
    }

    #[test]
    fn test_spectrum_gradient_returns_color() {
        // Just verify it doesn't panic
        let _ = spectrum_gradient(0.0);
        let _ = spectrum_gradient(0.5);
        let _ = spectrum_gradient(1.0);
    }
}
