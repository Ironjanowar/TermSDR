use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};

use super::spectrum::format_frequency;

/// Source type for display purposes.
#[derive(Debug, Clone)]
pub enum DisplaySource {
    Test,
    File(String),
    RtlSdr,
}

impl std::fmt::Display for DisplaySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DisplaySource::Test => write!(f, "TEST"),
            DisplaySource::File(p) => write!(f, "FILE: {}", p),
            DisplaySource::RtlSdr => write!(f, "RTL-SDR"),
        }
    }
}

/// Status bar widget showing current configuration.
pub struct StatusBarWidget {
    pub center_freq: f64,
    pub sample_rate: f64,
    pub gain: Option<f32>,
    pub source: DisplaySource,
    pub fps: f64,
}

impl StatusBarWidget {
    pub fn new(
        center_freq: f64,
        sample_rate: f64,
        gain: Option<f32>,
        source: DisplaySource,
        fps: f64,
    ) -> Self {
        Self {
            center_freq,
            sample_rate,
            gain,
            source,
            fps,
        }
    }

    /// Format the status bar text.
    pub fn format_status(&self) -> String {
        let freq = format_frequency(self.center_freq);
        let bw = format_frequency(self.sample_rate);
        let gain_str = match self.gain {
            Some(g) => format!("{:.0}dB", g),
            None => "AGC".to_string(),
        };
        format!(
            " ⏣ Freq: {} │ BW: {} │ Gain: {} │ {} │ {:.0} FPS ",
            freq, bw, gain_str, self.source, self.fps
        )
    }
}

impl Widget for StatusBarWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 10 || area.height < 1 {
            return;
        }

        let status = self.format_status();
        let style = Style::default()
            .fg(Color::White)
            .bg(Color::Rgb(30, 30, 60))
            .add_modifier(Modifier::BOLD);

        // Fill the entire status bar area with background
        for x in area.x..area.x + area.width {
            buf.set_string(x, area.y, " ", style);
        }

        // Write the status text
        let truncated = if status.len() > area.width as usize {
            &status[..area.width as usize]
        } else {
            &status
        };
        buf.set_string(area.x, area.y, truncated, style);

        // Right-aligned help hint
        let help = " [q]uit [f]req [←→]tune [↑↓]gain [+-]zoom ";
        if area.width as usize > status.len() + help.len() + 2 {
            let help_x = area.x + area.width - help.len() as u16;
            let help_style = Style::default()
                .fg(Color::DarkGray)
                .bg(Color::Rgb(30, 30, 60));
            buf.set_string(help_x, area.y, help, help_style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_bar_format() {
        let bar = StatusBarWidget::new(
            100_000_000.0,
            2_048_000.0,
            Some(40.0),
            DisplaySource::Test,
            30.0,
        );
        let status = bar.format_status();
        assert!(status.contains("100.000M"));
        assert!(status.contains("2.048M"));
        assert!(status.contains("40dB"));
        assert!(status.contains("TEST"));
        assert!(status.contains("30 FPS"));
    }

    #[test]
    fn test_status_bar_agc() {
        let bar = StatusBarWidget::new(
            100_000_000.0,
            2_048_000.0,
            None,
            DisplaySource::Test,
            60.0,
        );
        let status = bar.format_status();
        assert!(status.contains("AGC"));
    }

    #[test]
    fn test_display_source_format() {
        assert_eq!(format!("{}", DisplaySource::Test), "TEST");
        assert_eq!(format!("{}", DisplaySource::RtlSdr), "RTL-SDR");
        assert_eq!(
            format!("{}", DisplaySource::File("test.iq".to_string())),
            "FILE: test.iq"
        );
    }
}
