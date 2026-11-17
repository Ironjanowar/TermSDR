//! Status bar widget: frequency, sample rate, and mode display.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
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
            DisplaySource::File(p) => write!(f, "FILE: {p}"),
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
    /// Whether cursor audio is currently unmuted.
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
    #[must_use]
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
            channel_bw: crate::audio::demod::NFM_DEFAULT_CHANNEL_BW,
            volume: crate::ui::DEFAULT_VOLUME,
            audio_active: true,
            squelch: None,
            waterfall_time_span: 20.0,
            demod_mode: "AM".to_string(),
            averaging: 1,
            scan_active: false,
        }
    }

    #[must_use] 
    pub fn with_cursor(mut self, info: Option<(f64, Option<f32>)>) -> Self {
        self.cursor_info = info;
        self
    }

    #[must_use] 
    pub fn with_channel_bw(mut self, bw: f64) -> Self {
        self.channel_bw = bw;
        self
    }

    #[must_use] 
    pub fn with_audio(mut self, volume: u8, audio_active: bool, squelch: Option<f32>) -> Self {
        self.volume = volume;
        self.audio_active = audio_active;
        self.squelch = squelch;
        self
    }

    #[must_use] 
    pub fn with_waterfall_time_span(mut self, span: f64) -> Self {
        self.waterfall_time_span = span;
        self
    }

    #[must_use] 
    pub fn with_demod_mode(mut self, mode: &str) -> Self {
        self.demod_mode = mode.to_string();
        self
    }

    #[must_use] 
    pub fn with_averaging(mut self, count: usize) -> Self {
        self.averaging = count;
        self
    }

    #[must_use] 
    pub fn with_scan(mut self, active: bool) -> Self {
        self.scan_active = active;
        self
    }

    /// Format the status bar text.
    #[must_use] 
    pub fn format_status(&self) -> String {
        let freq = format_frequency(self.center_freq);
        let bw = format_frequency(self.sample_rate);
        let gain_str = match self.gain {
            Some(g) => format!("{g:.0}dB"),
            None => "AGC".to_string(),
        };
        let cursor_str = match &self.cursor_info {
            Some((freq, db)) => {
                let db_str = match db {
                    Some(d) => format!("{d:.1}dB"),
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
            Some(db) => format!(" │ SQ:{db:.0}dB"),
            None => String::new(),
        };
        let scan_str = if self.scan_active { " │ 📡SCAN" } else { "" };
        // Cursor readout comes before AVG/WF/source so it survives realistic
        // terminal widths; the trailing segments are the first to be clipped.
        format!(
            " ⏣ Freq: {} │ BW: {} │ ChBW: {} │ Gain: {} │ {}{}{}{} │ AVG:{} │ WF:{:.0}s │ {} │ {:.0} FPS ",
            freq, bw, bw_str, gain_str, vol_str, sq_str, scan_str, cursor_str, self.averaging, self.waterfall_time_span, self.source, self.fps
        )
    }
}

impl Widget for StatusBarWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 10 || area.height < 1 {
            return;
        }

        let status = self.format_status();
        // Terminal-column width (not byte length): multibyte symbols widen or
        // narrow the visible row, so byte arithmetic would clip or overflow.
        let status_width = Span::raw(status.as_str()).width();
        let style = Style::default()
            .fg(Color::White)
            .bg(Color::Rgb(30, 30, 60))
            .add_modifier(Modifier::BOLD);

        // Fill the entire status bar area with background
        for x in area.x..area.x + area.width {
            buf.set_string(x, area.y, " ", style);
        }

        // Write the status text. `set_stringn` clips per grapheme and stops at a
        // cell boundary, so byte-slicing panics on non-char boundaries are gone.
        buf.set_stringn(area.x, area.y, &status, area.width as usize, style);

        // Right-aligned help hint (ASCII only, so byte length equals width)
        let help = " [?]help [Space]mute ";
        if area.width as usize > status_width + help.len() + 2 {
            #[allow(clippy::cast_possible_truncation)] // intentional truncation
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

    #[test]
    fn render_help_hint_describes_space_as_mute() {
        let backend = ratatui::backend::TestBackend::new(240, 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    StatusBarWidget::new(
                        100_000_000.0,
                        2_000_000.0,
                        None,
                        DisplaySource::Test,
                        30.0,
                    ),
                    frame.area(),
                );
            })
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(rendered.contains("[Space]mute"));
        assert!(!rendered.contains("[Space]cursor"));
    }

    #[test]
    fn format_status_puts_cursor_readout_before_average_waterfall_and_source() {
        let status =
            StatusBarWidget::new(100_000_000.0, 2_048_000.0, None, DisplaySource::Test, 30.0)
                .with_cursor(Some((100_250_000.0, Some(-51.5))))
                .format_status();

        let cursor = status
            .find("Cursor:")
            .expect("cursor readout must be present");
        let average = status
            .find("AVG:")
            .expect("average segment must be present");
        let waterfall = status
            .find("WF:")
            .expect("waterfall segment must be present");
        let source = status.find("TEST").expect("source segment must be present");

        assert!(cursor < average, "cursor readout must precede AVG");
        assert!(cursor < waterfall, "cursor readout must precede WF");
        assert!(cursor < source, "cursor readout must precede the source");
    }

    fn render_status_cells(widget: StatusBarWidget, width: u16) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(width, 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| frame.render_widget(widget, frame.area()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect()
    }

    fn render_status(widget: StatusBarWidget, width: u16) -> String {
        render_status_cells(widget, width).concat()
    }

    fn cursor_status_widget() -> StatusBarWidget {
        StatusBarWidget::new(100_000_000.0, 2_048_000.0, None, DisplaySource::Test, 30.0)
            .with_cursor(Some((100_250_000.0, Some(-51.5))))
            .with_channel_bw(25_000.0)
            .with_audio(80, true, None)
            .with_demod_mode("AM")
            .with_averaging(4)
    }

    #[test]
    fn cursor_readout_survives_at_120_columns() {
        let rendered = render_status(cursor_status_widget(), 120);

        assert!(
            rendered.contains("Cursor: 100.250M @ -51.5dB"),
            "120-column status bar must show the cursor readout, got: {rendered}"
        );
    }

    /// Total number of terminal columns consumed by the buffer row.
    ///
    /// Wide graphemes occupy one cell whose symbol is the grapheme plus one
    /// placeholder cell, so a naive sum of symbol widths over-counts; this walk
    /// skips the placeholder cells the renderer reset.
    fn occupied_columns(cells: &[String]) -> usize {
        let mut columns = 0;
        let mut index = 0;
        while index < cells.len() {
            let symbol_width = ratatui::text::Span::raw(cells[index].as_str()).width();
            columns += symbol_width;
            index += symbol_width.max(1);
        }
        columns
    }

    #[test]
    fn long_multibyte_source_never_panics_and_keeps_cursor_readout() {
        let long_source = "FILE: /señales/日本/estación-🎚️-muy-larga-de-prueba.iq";
        for width in 10..=200 {
            let widget = StatusBarWidget::new(
                100_000_000.0,
                2_048_000.0,
                None,
                DisplaySource::File(long_source.to_string()),
                30.0,
            )
            .with_cursor(Some((100_250_000.0, Some(-51.5))));

            let cells = render_status_cells(widget, width);
            let rendered = cells.concat();

            assert_eq!(
                cells.len(),
                usize::from(width),
                "width {width} must render exactly one row of cells"
            );
            assert_eq!(
                occupied_columns(&cells),
                usize::from(width),
                "width {width} must fill the row without overflowing it"
            );

            if width >= 120 {
                assert!(
                    rendered.contains("Cursor: 100.250M"),
                    "width {width} must keep cursor readout, got: {rendered}"
                );
            }
        }
    }

    #[test]
    fn smoke_render_status_bar() {
        let backend = ratatui::backend::TestBackend::new(120, 3);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| {
            let widget = StatusBarWidget::new(
                100_000_000.0,
                2_048_000.0,
                None,
                DisplaySource::Test,
                30.0,
            ).with_cursor(Some((100e6, Some(-50.0))))
            .with_channel_bw(25_000.0)
            .with_audio(80, true, Some(-60.0))
            .with_demod_mode("AM")
            .with_averaging(4)
            .with_scan(true);
            f.render_widget(widget, f.area());
        }).unwrap();
    }
}
