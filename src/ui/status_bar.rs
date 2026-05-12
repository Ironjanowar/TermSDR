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
    /// Cursor info: (frequency, Option<dB>)
    pub cursor_info: Option<(f64, Option<f32>)>,
    /// Channel bandwidth in Hz.
    pub channel_bw: f64,
    /// Volume (0-100).
    pub volume: u8,
    /// Whether audio is active (cursor active).
    pub audio_active: bool,
    /// Squelch threshold.
    pub squelch: Option<f32>,
    /// Waterfall time span in seconds.
    pub waterfall_time_span: f64,
    /// Demodulation mode label.
    pub demod_mode: String,
    /// Spectrum averaging count.
    pub averaging: usize,
    /// Whether scan mode is active.
    pub scan_active: bool,
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
            cursor_info: None,
            channel_bw: 25_000.0,
            volume: 80,
            audio_active: true,
            squelch: None,
            waterfall_time_span: 20.0,
            demod_mode: "AM".to_string(),
            averaging: 1,
            scan_active: false,
        }
    }

    pub fn with_cursor(mut self, info: Option<(f64, Option<f32>)>) -> Self {
        self.cursor_info = info;
        self
    }

    pub fn with_channel_bw(mut self, bw: f64) -> Self {
        self.channel_bw = bw;
        self
    }

    pub fn with_audio(mut self, volume: u8, audio_active: bool, squelch: Option<f32>) -> Self {
        self.volume = volume;
        self.audio_active = audio_active;
        self.squelch = squelch;
        self
    }

    pub fn with_waterfall_time_span(mut self, span: f64) -> Self {
        self.waterfall_time_span = span;
        self
    }

    pub fn with_demod_mode(mut self, mode: &str) -> Self {
        self.demod_mode = mode.to_string();
        self
    }

    pub fn with_averaging(mut self, count: usize) -> Self {
        self.averaging = count;
        self
    }

    pub fn with_scan(mut self, active: bool) -> Self {
        self.scan_active = active;
        self
    }

    /// Format the status bar text.
    pub fn format_status(&self) -> String {
        let freq = format_frequency(self.center_freq);
        let bw = format_frequency(self.sample_rate);
        let gain_str = match self.gain {
            Some(g) => format!("{:.0}dB", g),
            None => "AGC".to_string(),
        };
        let cursor_str = match &self.cursor_info {
            Some((freq, db)) => {
                let db_str = match db {
                    Some(d) => format!("{:.1}dB", d),
                    None => "--".to_string(),
                };
                format!(" │ Cursor: {} @ {}", format_frequency(*freq), db_str)
            }
            None => String::new(),
        };
        let bw_str = if self.channel_bw >= 1_000_000.0 {
            format!("{:.1}M", self.channel_bw / 1_000_000.0)
        } else if self.channel_bw >= 1_000.0 {
            format!("{:.1}k", self.channel_bw / 1_000.0)
        } else {
            format!("{:.0}", self.channel_bw)
        };
        let vol_str = if self.audio_active {
            format!("{} 🔊 {}%", self.demod_mode, self.volume)
        } else {
            format!("{} 🔇 MUTED", self.demod_mode)
        };
        let sq_str = match self.squelch {
            Some(db) => format!(" │ SQ:{:.0}dB", db),
            None => String::new(),
        };
        let scan_str = if self.scan_active { " │ 📡SCAN" } else { "" };
        format!(
            " ⏣ Freq: {} │ BW: {} │ ChBW: {} │ Gain: {} │ {}{}{} │ AVG:{} │ WF:{:.0}s │ {} │ {:.0} FPS{} ",
            freq, bw, bw_str, gain_str, vol_str, sq_str, scan_str, self.averaging, self.waterfall_time_span, self.source, self.fps, cursor_str
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
        let help = " [?]help [Space]cursor ";
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
        assert!(status.contains("AM \u{1f50a} 80%")); // default demod + volume
        assert!(status.contains("AVG:1"));
        assert!(status.contains("WF:20s"));
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
    fn test_status_bar_muted() {
        let mut bar = StatusBarWidget::new(
            100_000_000.0,
            2_048_000.0,
            None,
            DisplaySource::Test,
            30.0,
        );
        bar.audio_active = false;
        let status = bar.format_status();
        assert!(status.contains("AM \u{1f507} MUTED"));
    }

    #[test]
    fn test_status_bar_squelch() {
        let mut bar = StatusBarWidget::new(
            100_000_000.0,
            2_048_000.0,
            None,
            DisplaySource::Test,
            30.0,
        );
        bar.squelch = Some(-60.0);
        let status = bar.format_status();
        assert!(status.contains("SQ:-60dB"));
    }

    #[test]
    fn test_status_bar_no_squelch() {
        let bar = StatusBarWidget::new(
            100_000_000.0,
            2_048_000.0,
            None,
            DisplaySource::Test,
            30.0,
        );
        let status = bar.format_status();
        assert!(!status.contains("SQ:"));
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
