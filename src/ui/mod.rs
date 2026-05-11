pub mod colors;
pub mod spectrum;
pub mod status_bar;
pub mod waterfall;

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame, Terminal,
};

use crate::types::{AppConfig, FftResult, SourceType};
use spectrum::SpectrumWidget;
use status_bar::{DisplaySource, StatusBarWidget};
use waterfall::WaterfallWidget;

/// Source selection at startup.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceSelection {
    Test,
    File(String),
    RtlSdr(usize),
}

/// The main application state for the TUI.
pub struct App {
    /// Current config (may be mutated by controls).
    pub config: AppConfig,
    /// Latest FFT result to display.
    pub latest_fft: Option<FftResult>,
    /// Waterfall history (newest first).
    pub waterfall_history: Vec<Vec<f32>>,
    /// Maximum waterfall rows to keep.
    pub max_waterfall_rows: usize,
    /// FPS counter.
    pub fps: f64,
    /// Whether the app should quit.
    pub should_quit: bool,
    /// dB display range.
    pub min_db: f32,
    pub max_db: f32,
    /// Frequency input mode.
    pub freq_input_mode: bool,
    pub freq_input_buf: String,
    /// Bandwidth input mode.
    pub bw_input_mode: bool,
    pub bw_input_buf: String,
    /// Cursor frequency in Hz (absolute position).
    pub cursor_freq: f64,
    /// Whether cursor is active/visible.
    pub cursor_active: bool,
    /// Whether help modal is shown.
    pub show_help: bool,
    /// Channel bandwidth in Hz.
    pub channel_bw: f64,
    /// Frame timing.
    frame_count: u64,
    last_fps_update: Instant,
    last_frame_count: u64,
}

impl App {
    pub fn new(config: AppConfig) -> Self {
        let center = config.center_freq;
        Self {
            config,
            latest_fft: None,
            waterfall_history: Vec::new(),
            max_waterfall_rows: 200,
            fps: 0.0,
            should_quit: false,
            min_db: -80.0,
            max_db: 0.0,
            freq_input_mode: false,
            freq_input_buf: String::new(),
            bw_input_mode: false,
            bw_input_buf: String::new(),
            cursor_freq: center,
            cursor_active: true,
            show_help: false,
            channel_bw: 25_000.0,
            frame_count: 0,
            last_fps_update: Instant::now(),
            last_frame_count: 0,
        }
    }

    /// Push a new FFT result into the app state.
    pub fn push_fft(&mut self, fft: FftResult) {
        // Update waterfall history
        self.waterfall_history.insert(0, fft.bins.clone());
        if self.waterfall_history.len() > self.max_waterfall_rows {
            self.waterfall_history.truncate(self.max_waterfall_rows);
        }

        // Auto-range dB if we have data
        if !fft.bins.is_empty() {
            let min = fft.bins.iter().cloned().fold(f32::INFINITY, f32::min);
            let max = fft.bins.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            // Smooth the range
            self.min_db = self.min_db * 0.95 + (min - 10.0) * 0.05;
            self.max_db = self.max_db * 0.95 + (max + 5.0) * 0.05;
            // Ensure minimum range
            if self.max_db - self.min_db < 20.0 {
                self.min_db = self.max_db - 20.0;
            }
        }

        self.latest_fft = Some(fft);
    }

    /// Update FPS counter.
    pub fn update_fps(&mut self) {
        self.frame_count += 1;
        let elapsed = self.last_fps_update.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let frames = self.frame_count - self.last_frame_count;
            self.fps = frames as f64 / elapsed.as_secs_f64();
            self.last_fps_update = Instant::now();
            self.last_frame_count = self.frame_count;
        }
    }

    /// Handle keyboard input.
    pub fn handle_key(&mut self, key: KeyEvent) {
        // Help modal takes priority — any key closes it
        if self.show_help {
            self.show_help = false;
            return;
        }

        if self.freq_input_mode {
            match key.code {
                KeyCode::Enter => {
                    if let Ok(freq) = crate::config::parse_frequency(&self.freq_input_buf) {
                        self.config.center_freq = freq;
                    }
                    self.freq_input_mode = false;
                    self.freq_input_buf.clear();
                }
                KeyCode::Esc => {
                    self.freq_input_mode = false;
                    self.freq_input_buf.clear();
                }
                KeyCode::Backspace => {
                    self.freq_input_buf.pop();
                }
                KeyCode::Char(c) => {
                    self.freq_input_buf.push(c);
                }
                _ => {}
            }
            return;
        }

        if self.bw_input_mode {
            match key.code {
                KeyCode::Enter => {
                    if let Ok(bw) = crate::config::parse_frequency(&self.bw_input_buf) {
                        self.config.sample_rate = bw;
                    }
                    self.bw_input_mode = false;
                    self.bw_input_buf.clear();
                }
                KeyCode::Esc => {
                    self.bw_input_mode = false;
                    self.bw_input_buf.clear();
                }
                KeyCode::Backspace => {
                    self.bw_input_buf.pop();
                }
                KeyCode::Char(c) => {
                    self.bw_input_buf.push(c);
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.should_quit = true;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Left => {
                if self.cursor_active {
                    // Move cursor left by 2% of visible bandwidth
                    self.cursor_freq -= 0.02 * self.config.sample_rate;
                } else {
                    // Fine tune: visual scroll right (lower freq)
                    self.config.center_freq += 25_000.0;
                }
            }
            KeyCode::Right => {
                if self.cursor_active {
                    // Move cursor right by 2% of visible bandwidth
                    self.cursor_freq += 0.02 * self.config.sample_rate;
                } else {
                    // Fine tune: visual scroll left (higher freq)
                    self.config.center_freq -= 25_000.0;
                }
            }
            KeyCode::PageUp => {
                // Coarse tune: +1 MHz
                self.config.center_freq += 1_000_000.0;
            }
            KeyCode::PageDown => {
                // Coarse tune: -1 MHz
                self.config.center_freq -= 1_000_000.0;
            }
            KeyCode::Up => {
                // Increase gain
                let g = self.config.gain.unwrap_or(0.0);
                self.config.gain = Some((g + 1.0).min(50.0));
            }
            KeyCode::Down => {
                // Decrease gain
                let g = self.config.gain.unwrap_or(0.0);
                self.config.gain = Some((g - 1.0).max(0.0));
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                // Zoom in (decrease displayed bandwidth)
                self.config.sample_rate = (self.config.sample_rate * 0.8).max(256_000.0);
            }
            KeyCode::Char('-') => {
                // Zoom out (increase displayed bandwidth)
                self.config.sample_rate = (self.config.sample_rate * 1.25).min(3_200_000.0);
            }
            KeyCode::Char('f') => {
                self.freq_input_mode = true;
                self.freq_input_buf.clear();
            }
            KeyCode::Char('b') => {
                self.bw_input_mode = true;
                self.bw_input_buf.clear();
            }
            KeyCode::Char('c') => {
                self.cursor_freq = self.config.center_freq;
            }
            KeyCode::Char(' ') => {
                // Toggle cursor active/inactive
                self.cursor_active = !self.cursor_active;
            }
            KeyCode::Char('?') => {
                self.show_help = true;
            }
            KeyCode::Char('w') => {
                self.channel_bw = (self.channel_bw * 2.0).min(1_000_000.0);
            }
            KeyCode::Char('W') => {
                self.channel_bw = (self.channel_bw / 2.0).max(1_000.0);
            }
            _ => {}
        }
    }

    /// Get the display source type.
    pub fn display_source(&self) -> DisplaySource {
        match &self.config.source {
            SourceType::Test { .. } => DisplaySource::Test,
            SourceType::File { path } => DisplaySource::File(path.clone()),
            SourceType::RtlSdr => DisplaySource::RtlSdr,
        }
    }

    /// Get the relative cursor position (0.0–1.0) within the visible spectrum.
    pub fn cursor_pos(&self) -> f32 {
        let start = self.config.center_freq - self.config.sample_rate / 2.0;
        ((self.cursor_freq - start) / self.config.sample_rate) as f32
    }

    /// Get the frequency at the current cursor position.
    pub fn cursor_frequency(&self) -> f64 {
        self.cursor_freq
    }

    /// Get the dB value at the current cursor position from the latest FFT.
    pub fn cursor_db(&self) -> Option<f32> {
        let pos = self.cursor_pos();
        self.latest_fft.as_ref().and_then(|fft| {
            if fft.bins.is_empty() {
                return None;
            }
            let bin_index = (pos * (fft.bins.len() - 1) as f32) as usize;
            fft.bins.get(bin_index).copied()
        })
    }
}

/// Initialize the terminal for TUI.
pub fn init_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal to normal state.
pub fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Draw the main UI.
pub fn draw_ui(frame: &mut Frame, app: &App) {
    let size = frame.area();

    // Layout: spectrum (40%), waterfall (50%), status bar (10%, min 1 row)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Percentage(55),
            Constraint::Min(1),
        ])
        .split(size);

    // Spectrum
    let spectrum = SpectrumWidget::new(
        app.latest_fft.as_ref(),
        app.min_db,
        app.max_db,
    ).with_cursor(
        Some(app.cursor_pos()),
        app.cursor_active,
        app.cursor_frequency(),
        app.cursor_db(),
    ).with_bandwidth(
        Some(app.channel_bw),
        app.config.center_freq,
        app.config.sample_rate,
    );
    frame.render_widget(spectrum, chunks[0]);

    // Waterfall
    let waterfall = WaterfallWidget::new(
        &app.waterfall_history,
        app.min_db,
        app.max_db,
    ).with_cursor(Some(app.cursor_pos()), app.cursor_active)
    .with_bandwidth(
        Some(app.channel_bw),
        app.cursor_freq,
        app.config.center_freq,
        app.config.sample_rate,
    );
    frame.render_widget(waterfall, chunks[1]);

    // Status bar
    let cursor_info = if app.cursor_active {
        Some((app.cursor_frequency(), app.cursor_db()))
    } else {
        None
    };
    let status = StatusBarWidget::new(
        app.config.center_freq,
        app.config.sample_rate,
        app.config.gain,
        app.display_source(),
        app.fps,
    ).with_cursor(cursor_info)
    .with_channel_bw(app.channel_bw);
    frame.render_widget(status, chunks[2]);

    // Bandwidth input overlay
    if app.bw_input_mode {
        let input_area = Rect {
            x: size.width / 4,
            y: size.height / 2 - 1,
            width: size.width / 2,
            height: 3,
        };
        let block = Block::default()
            .title(" Enter Bandwidth (e.g. 2M, 500k) ")
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Yellow).bg(Color::Black));
        let input_text = Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
            Span::styled(&app.bw_input_buf, Style::default().fg(Color::White)),
            Span::styled("█", Style::default().fg(Color::Yellow)),
        ]))
        .block(block);
        frame.render_widget(Clear, input_area);
        frame.render_widget(input_text, input_area);
    }

    // Frequency input overlay
    if app.freq_input_mode {
        let input_area = Rect {
            x: size.width / 4,
            y: size.height / 2 - 1,
            width: size.width / 2,
            height: 3,
        };
        let block = Block::default()
            .title(" Enter Frequency (e.g. 100M, 118.1M) ")
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Yellow).bg(Color::Black));
        let input_text = Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
            Span::styled(&app.freq_input_buf, Style::default().fg(Color::White)),
            Span::styled("█", Style::default().fg(Color::Yellow)),
        ]))
        .block(block);
        frame.render_widget(Clear, input_area);
        frame.render_widget(input_text, input_area);
    }

    // Help modal
    if app.show_help {
        draw_help_modal(frame, size);
    }
}

/// Draw the help/cheatsheet modal.
fn draw_help_modal(frame: &mut Frame, size: Rect) {
    let block = Block::default()
        .title(" ⌨️  Keyboard Shortcuts ")
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Cyan).bg(Color::Black));

    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Navigation", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("    ← →       ", Style::default().fg(Color::Cyan)),
            Span::styled("Move cursor (when active) / Tune ±25 kHz", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("    PgUp PgDn ", Style::default().fg(Color::Cyan)),
            Span::styled("Tune ±1 MHz", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("    f         ", Style::default().fg(Color::Cyan)),
            Span::styled("Jump to frequency (type e.g. 118.1M)", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("    b         ", Style::default().fg(Color::Cyan)),
            Span::styled("Set bandwidth (type e.g. 2M, 500k)", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("    c         ", Style::default().fg(Color::Cyan)),
            Span::styled("Center cursor on current frequency", Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Display", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("    + -       ", Style::default().fg(Color::Cyan)),
            Span::styled("Zoom bandwidth in/out", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("    ↑ ↓       ", Style::default().fg(Color::Cyan)),
            Span::styled("Adjust gain ±1 dB", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("    Space     ", Style::default().fg(Color::Cyan)),
            Span::styled("Toggle cursor on/off", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("    w / W     ", Style::default().fg(Color::Cyan)),
            Span::styled("Widen/narrow channel bandwidth", Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  General", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("    ?         ", Style::default().fg(Color::Cyan)),
            Span::styled("Show this help", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("    q / Esc   ", Style::default().fg(Color::Cyan)),
            Span::styled("Quit", Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Press any key to close", Style::default().fg(Color::DarkGray)),
        ]),
    ];

    let paragraph = Paragraph::new(lines).block(block);
    let area = centered_rect(60, 70, size);
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

/// Show the source selection screen and return the user's choice.
pub fn show_source_selection(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<Option<SourceSelection>> {
    loop {
        terminal.draw(|frame| {
            let size = frame.area();
            let block = Block::default()
                .title(" 📡 TermSDR — Source Selection ")
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::Cyan));

            let lines = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Detected RTL-SDR devices:", Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("    (No RTL-SDR devices detected)", Style::default().fg(Color::DarkGray)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Other sources:", Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("    [T] ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                    Span::styled("Test signal (synthetic)", Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("    [F] ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                    Span::styled("File replay (IQ file)", Style::default().fg(Color::White)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Press key to select, ", Style::default().fg(Color::DarkGray)),
                    Span::styled("q", Style::default().fg(Color::Yellow)),
                    Span::styled(" to quit", Style::default().fg(Color::DarkGray)),
                ]),
            ];

            let paragraph = Paragraph::new(lines).block(block);
            let area = centered_rect(60, 50, size);
            frame.render_widget(Clear, area);
            frame.render_widget(paragraph, area);
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('t') | KeyCode::Char('T') => {
                        return Ok(Some(SourceSelection::Test));
                    }
                    KeyCode::Char('f') | KeyCode::Char('F') => {
                        // For now, use a hardcoded prompt
                        let path = show_file_input(terminal)?;
                        if let Some(p) = path {
                            return Ok(Some(SourceSelection::File(p)));
                        }
                        // If they cancelled, loop back to selection
                    }
                    KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                        return Ok(None);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Show a file path input dialog.
fn show_file_input(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<Option<String>> {
    let mut input = String::new();
    loop {
        terminal.draw(|frame| {
            let size = frame.area();
            let block = Block::default()
                .title(" Enter IQ file path ")
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::Yellow));

            let lines = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("  > ", Style::default().fg(Color::Cyan)),
                    Span::styled(&input, Style::default().fg(Color::White)),
                    Span::styled("█", Style::default().fg(Color::Yellow)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Enter to confirm, Esc to cancel", Style::default().fg(Color::DarkGray)),
                ]),
            ];

            let paragraph = Paragraph::new(lines).block(block);
            let area = centered_rect(60, 30, size);
            frame.render_widget(Clear, area);
            frame.render_widget(paragraph, area);
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Enter => {
                        if !input.is_empty() {
                            return Ok(Some(input));
                        }
                    }
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Backspace => { input.pop(); }
                    KeyCode::Char(c) => { input.push(c); }
                    _ => {}
                }
            }
        }
    }
}

/// Helper to create a centered rectangle.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_push_fft() {
        let config = AppConfig::default();
        let mut app = App::new(config);

        let fft = FftResult {
            bins: vec![-50.0; 100],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        app.push_fft(fft);

        assert!(app.latest_fft.is_some());
        assert_eq!(app.waterfall_history.len(), 1);
    }

    #[test]
    fn test_app_waterfall_max_rows() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        app.max_waterfall_rows = 5;

        for _ in 0..10 {
            let fft = FftResult {
                bins: vec![-50.0; 100],
                center_freq: 100e6,
                sample_rate: 2.048e6,
            };
            app.push_fft(fft);
        }

        assert_eq!(app.waterfall_history.len(), 5);
    }

    #[test]
    fn test_app_handle_quit() {
        let config = AppConfig::default();
        let mut app = App::new(config);

        app.handle_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn test_app_handle_esc() {
        let config = AppConfig::default();
        let mut app = App::new(config);

        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.should_quit);
    }

    #[test]
    fn test_app_handle_tune_right() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        // Deactivate cursor so arrows tune frequency
        app.cursor_active = false;
        let orig = app.config.center_freq;

        app.handle_key(KeyEvent::from(KeyCode::Right));
        // Right arrow scrolls spectrum right → decreases center_freq
        assert!((app.config.center_freq - (orig - 25_000.0)).abs() < 1.0);
    }

    #[test]
    fn test_app_handle_tune_left() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        // Deactivate cursor so arrows tune frequency
        app.cursor_active = false;
        let orig = app.config.center_freq;

        app.handle_key(KeyEvent::from(KeyCode::Left));
        // Left arrow scrolls spectrum left → increases center_freq
        assert!((app.config.center_freq - (orig + 25_000.0)).abs() < 1.0);
    }

    #[test]
    fn test_app_cursor_move_right() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        assert!(app.cursor_active);
        let orig = app.cursor_freq;

        app.handle_key(KeyEvent::from(KeyCode::Right));
        assert!(app.cursor_freq > orig);
    }

    #[test]
    fn test_app_cursor_move_left() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        assert!(app.cursor_active);
        let orig = app.cursor_freq;

        app.handle_key(KeyEvent::from(KeyCode::Left));
        assert!(app.cursor_freq < orig);
    }

    #[test]
    fn test_app_cursor_toggle() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        assert!(app.cursor_active);

        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(!app.cursor_active);

        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(app.cursor_active);
    }

    #[test]
    fn test_app_help_toggle() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        assert!(!app.show_help);

        app.handle_key(KeyEvent::from(KeyCode::Char('?')));
        assert!(app.show_help);

        // Any key closes help
        app.handle_key(KeyEvent::from(KeyCode::Char('a')));
        assert!(!app.show_help);
    }

    #[test]
    fn test_app_cursor_frequency() {
        let config = AppConfig::default();
        let app = App::new(config);
        // Cursor at 0.5 should be at center frequency
        let cursor_freq = app.cursor_frequency();
        assert!((cursor_freq - app.config.center_freq).abs() < 1.0);
    }

    #[test]
    fn test_app_cursor_db() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        // No FFT data yet
        assert!(app.cursor_db().is_none());

        // Push FFT data
        let fft = FftResult {
            bins: vec![-50.0; 100],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        app.push_fft(fft);
        assert!(app.cursor_db().is_some());
        assert!((app.cursor_db().unwrap() - (-50.0)).abs() < 1.0);
    }

    #[test]
    fn test_app_handle_page_up() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        let orig = app.config.center_freq;

        app.handle_key(KeyEvent::from(KeyCode::PageUp));
        assert!((app.config.center_freq - (orig + 1_000_000.0)).abs() < 1.0);
    }

    #[test]
    fn test_app_handle_gain_up() {
        let config = AppConfig::default();
        let mut app = App::new(config);

        app.handle_key(KeyEvent::from(KeyCode::Up));
        assert_eq!(app.config.gain, Some(1.0));
    }

    #[test]
    fn test_app_freq_input_mode() {
        let config = AppConfig::default();
        let mut app = App::new(config);

        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        assert!(app.freq_input_mode);

        app.handle_key(KeyEvent::from(KeyCode::Char('1')));
        app.handle_key(KeyEvent::from(KeyCode::Char('1')));
        app.handle_key(KeyEvent::from(KeyCode::Char('8')));
        app.handle_key(KeyEvent::from(KeyCode::Char('M')));
        assert_eq!(app.freq_input_buf, "118M");

        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(!app.freq_input_mode);
        assert!((app.config.center_freq - 118_000_000.0).abs() < 1.0);
    }

    #[test]
    fn test_app_freq_input_cancel() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        let orig = app.config.center_freq;

        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        app.handle_key(KeyEvent::from(KeyCode::Char('2')));
        app.handle_key(KeyEvent::from(KeyCode::Esc));

        assert!(!app.freq_input_mode);
        assert!((app.config.center_freq - orig).abs() < 1.0);
    }

    #[test]
    fn test_app_bw_input_mode() {
        let config = AppConfig::default();
        let mut app = App::new(config);

        app.handle_key(KeyEvent::from(KeyCode::Char('b')));
        assert!(app.bw_input_mode);

        app.handle_key(KeyEvent::from(KeyCode::Char('1')));
        app.handle_key(KeyEvent::from(KeyCode::Char('.')));
        app.handle_key(KeyEvent::from(KeyCode::Char('5')));
        app.handle_key(KeyEvent::from(KeyCode::Char('M')));
        assert_eq!(app.bw_input_buf, "1.5M");

        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(!app.bw_input_mode);
        assert!((app.config.sample_rate - 1_500_000.0).abs() < 1.0);
    }

    #[test]
    fn test_app_bw_input_cancel() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        let orig = app.config.sample_rate;

        app.handle_key(KeyEvent::from(KeyCode::Char('b')));
        app.handle_key(KeyEvent::from(KeyCode::Char('5')));
        app.handle_key(KeyEvent::from(KeyCode::Esc));

        assert!(!app.bw_input_mode);
        assert!((app.config.sample_rate - orig).abs() < 1.0);
    }

    #[test]
    fn test_app_center_cursor() {
        let config = AppConfig::default();
        let mut app = App::new(config);

        // Move cursor away from center
        app.handle_key(KeyEvent::from(KeyCode::Right));
        app.handle_key(KeyEvent::from(KeyCode::Right));
        assert!((app.cursor_freq - app.config.center_freq).abs() > 1.0);

        // Press 'c' to center
        app.handle_key(KeyEvent::from(KeyCode::Char('c')));
        assert!((app.cursor_freq - app.config.center_freq).abs() < 1.0);
    }

    #[test]
    fn test_source_selection_enum() {
        let sel = SourceSelection::Test;
        assert_eq!(sel, SourceSelection::Test);
        let sel2 = SourceSelection::File("test.iq".to_string());
        assert_eq!(sel2, SourceSelection::File("test.iq".to_string()));
    }

    #[test]
    fn test_cursor_desync_bug() {
        // Reproduce: cursor at center, deactivate, tune right, reactivate.
        // Cursor should still point at the original frequency.
        let config = AppConfig::default();
        let mut app = App::new(config);

        // Cursor starts at center_freq
        let original_freq = app.cursor_frequency();
        assert!((original_freq - app.config.center_freq).abs() < 1.0);

        // Deactivate cursor
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(!app.cursor_active);

        // Tune right (-25 kHz to center_freq, spectrum scrolls right)
        app.handle_key(KeyEvent::from(KeyCode::Right));
        assert!((app.config.center_freq - (original_freq - 25_000.0)).abs() < 1.0);

        // Reactivate cursor
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(app.cursor_active);

        // Cursor should still be at the original frequency, NOT shifted
        let cursor_freq_after = app.cursor_frequency();
        assert!(
            (cursor_freq_after - original_freq).abs() < 1.0,
            "Cursor desynced! Expected {}, got {}",
            original_freq,
            cursor_freq_after
        );
    }

    #[test]
    fn test_app_channel_bw_increase() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        assert!((app.channel_bw - 25_000.0).abs() < 1.0);
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        assert!((app.channel_bw - 50_000.0).abs() < 1.0);
    }

    #[test]
    fn test_app_channel_bw_decrease() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        app.handle_key(KeyEvent::from(KeyCode::Char('W')));
        assert!((app.channel_bw - 12_500.0).abs() < 1.0);
    }

    #[test]
    fn test_app_channel_bw_max() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        app.channel_bw = 512_000.0;
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        assert!((app.channel_bw - 1_000_000.0).abs() < 1.0);
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        assert!((app.channel_bw - 1_000_000.0).abs() < 1.0); // capped
    }

    #[test]
    fn test_app_channel_bw_min() {
        let config = AppConfig::default();
        let mut app = App::new(config);
        app.channel_bw = 2_000.0;
        app.handle_key(KeyEvent::from(KeyCode::Char('W')));
        assert!((app.channel_bw - 1_000.0).abs() < 1.0);
        app.handle_key(KeyEvent::from(KeyCode::Char('W')));
        assert!((app.channel_bw - 1_000.0).abs() < 1.0); // capped
    }
}
