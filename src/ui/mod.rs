pub mod colors;
pub mod command_palette;
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

use crate::audio::AudioControl;
use crate::types::{AppConfig, FftResult, SourceType};
use command_palette::{CommandAction, CommandPalette};
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
    /// Command palette active.
    pub command_palette_active: bool,
    /// Command palette state.
    pub command_palette: CommandPalette,
    /// Audio volume (0-100).
    pub volume: u8,
    /// Squelch threshold in dB (None = disabled).
    pub squelch: Option<f32>,
    /// Audio control channel sender.
    pub audio_control_tx: Option<crossbeam_channel::Sender<AudioControl>>,
    /// Frame timing.
    frame_count: u64,
    last_fps_update: Instant,
    last_frame_count: u64,
}

impl App {
    pub fn new(config: AppConfig, audio_control_tx: Option<crossbeam_channel::Sender<AudioControl>>) -> Self {
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
            command_palette_active: false,
            command_palette: CommandPalette::new(),
            volume: 80,
            squelch: None,
            audio_control_tx,
            frame_count: 0,
            last_fps_update: Instant::now(),
            last_frame_count: 0,
        }
    }

    /// Send an audio control message (fire-and-forget).
    fn send_audio_control(&self, msg: AudioControl) {
        if let Some(ref tx) = self.audio_control_tx {
            let _ = tx.send(msg);
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

        // Command palette handling
        if self.command_palette_active {
            match key.code {
                KeyCode::Esc => {
                    self.command_palette_active = false;
                }
                KeyCode::Enter => {
                    match self.command_palette.execute() {
                        Ok(action) => {
                            self.command_palette_active = false;
                            self.apply_command_action(action);
                        }
                        Err(msg) => {
                            self.command_palette.error = Some(msg);
                        }
                    }
                }
                KeyCode::Up => {
                    self.command_palette.select_up();
                }
                KeyCode::Down => {
                    self.command_palette.select_down();
                }
                KeyCode::Tab => {
                    self.command_palette.autocomplete();
                }
                KeyCode::Backspace => {
                    self.command_palette.backspace();
                }
                KeyCode::Char(c) => {
                    self.command_palette.type_char(c);
                }
                _ => {}
            }
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
                    self.send_audio_control(AudioControl::SetCursorFreq(self.cursor_freq));
                } else {
                    // Fine tune: visual scroll right (lower freq)
                    self.config.center_freq += 25_000.0;
                }
            }
            KeyCode::Right => {
                if self.cursor_active {
                    // Move cursor right by 2% of visible bandwidth
                    self.cursor_freq += 0.02 * self.config.sample_rate;
                    self.send_audio_control(AudioControl::SetCursorFreq(self.cursor_freq));
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
                self.send_audio_control(AudioControl::Mute(!self.cursor_active));
            }
            KeyCode::Char('?') => {
                self.show_help = true;
            }
            KeyCode::Char('w') => {
                self.channel_bw = (self.channel_bw * 2.0).min(1_000_000.0);
                self.send_audio_control(AudioControl::SetChannelBw(self.channel_bw));
            }
            KeyCode::Char('W') => {
                self.channel_bw = (self.channel_bw / 2.0).max(1_000.0);
                self.send_audio_control(AudioControl::SetChannelBw(self.channel_bw));
            }
            KeyCode::Char('v') => {
                self.volume = (self.volume + 5).min(100);
                self.send_audio_control(AudioControl::SetVolume(self.volume));
            }
            KeyCode::Char('V') => {
                self.volume = self.volume.saturating_sub(5);
                self.send_audio_control(AudioControl::SetVolume(self.volume));
            }
            KeyCode::Char('/') => {
                self.command_palette.reset();
                self.command_palette_active = true;
            }
            _ => {}
        }
    }

    /// Apply a command action from the command palette.
    fn apply_command_action(&mut self, action: CommandAction) {
        match action {
            CommandAction::SetFrequency(f) => self.config.center_freq = f,
            CommandAction::SetBandwidth(bw) => self.config.sample_rate = bw,
            CommandAction::SetChannelBw(bw) => {
                self.channel_bw = bw;
                self.send_audio_control(AudioControl::SetChannelBw(self.channel_bw));
            }
            CommandAction::WidenChannelBw => {
                self.channel_bw = (self.channel_bw * 2.0).min(1_000_000.0);
                self.send_audio_control(AudioControl::SetChannelBw(self.channel_bw));
            }
            CommandAction::CenterCursor => {
                self.cursor_freq = self.config.center_freq;
                self.send_audio_control(AudioControl::SetCursorFreq(self.cursor_freq));
            }
            CommandAction::GainUp => {
                let g = self.config.gain.unwrap_or(0.0);
                self.config.gain = Some((g + 1.0).min(50.0));
            }
            CommandAction::GainDown => {
                let g = self.config.gain.unwrap_or(0.0);
                self.config.gain = Some((g - 1.0).max(0.0));
            }
            CommandAction::ShowHelp => self.show_help = true,
            CommandAction::Quit => self.should_quit = true,
            CommandAction::ToggleCursor => self.cursor_active = !self.cursor_active,
            CommandAction::TuneUp => self.config.center_freq += 25_000.0,
            CommandAction::TuneDown => self.config.center_freq -= 25_000.0,
            CommandAction::TuneCoarseUp => self.config.center_freq += 1_000_000.0,
            CommandAction::TuneCoarseDown => self.config.center_freq -= 1_000_000.0,
            CommandAction::ZoomIn => self.config.sample_rate = (self.config.sample_rate * 0.8).max(256_000.0),
            CommandAction::ZoomOut => self.config.sample_rate = (self.config.sample_rate * 1.25).min(3_200_000.0),
            CommandAction::SetVolume(v) => {
                self.volume = v;
                self.send_audio_control(AudioControl::SetVolume(self.volume));
            }
            CommandAction::SetSquelch(sq) => {
                self.squelch = sq;
                self.send_audio_control(AudioControl::SetSquelch(self.squelch));
            }
        }
    }

    /// Get the display source type.
    pub fn display_source(&self) -> DisplaySource {
        match &self.config.source {
            SourceType::Test { .. } => DisplaySource::Test,
            SourceType::File { path } => DisplaySource::File(path.clone()),
            SourceType::RtlSdr { .. } => DisplaySource::RtlSdr,
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
    .with_channel_bw(app.channel_bw)
    .with_audio(app.volume, app.cursor_active, app.squelch);
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

    // Command palette modal
    if app.command_palette_active {
        draw_command_palette(frame, size, &app.command_palette);
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
        Line::from(vec![
            Span::styled("    v / V     ", Style::default().fg(Color::Cyan)),
            Span::styled("Volume up/down", Style::default().fg(Color::White)),
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
            Span::styled("    /         ", Style::default().fg(Color::Cyan)),
            Span::styled("Open command palette (/volume, /squelch)", Style::default().fg(Color::White)),
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

/// Draw the command palette modal.
fn draw_command_palette(frame: &mut Frame, size: Rect, palette: &CommandPalette) {
    let filtered = palette.filtered();
    
    // Calculate height: input(1) + error?(1) + separator(1) + results + border(2)
    let max_visible = 12;
    let result_count = filtered.len().min(max_visible);
    let has_error = palette.error.is_some();
    let error_rows: u16 = if has_error { 1 } else { 0 };
    let total_height = (3 + error_rows + result_count as u16 + 2).min(size.height);
    let width = (size.width * 60 / 100).max(40).min(size.width);
    
    let area = Rect {
        x: (size.width - width) / 2,
        y: size.height / 5,
        width,
        height: total_height,
    };
    
    let block = Block::default()
        .title(" / Command Palette ")
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Cyan).bg(Color::Black));
    
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    
    // Inner area (inside borders)
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    
    // Input line
    if inner.height > 0 {
        let input_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        };
        let input_line = Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
            Span::styled(&palette.input, Style::default().fg(Color::White)),
            Span::styled("█", Style::default().fg(Color::Yellow)),
        ]));
        frame.render_widget(input_line, input_area);
    }
    
    // Error message (if any)
    let error_offset: u16 = if has_error {
        if inner.height > 1 {
            let error_area = Rect {
                x: inner.x,
                y: inner.y + 1,
                width: inner.width,
                height: 1,
            };
            let error_text = palette.error.as_deref().unwrap_or("");
            let error_line = Paragraph::new(Line::from(
                Span::styled(format!("  ⚠ {}", error_text), Style::default().fg(Color::Red))
            ));
            frame.render_widget(error_line, error_area);
        }
        1
    } else {
        0
    };
    
    // Separator
    if inner.height > 1 + error_offset {
        let sep_area = Rect {
            x: inner.x,
            y: inner.y + 1 + error_offset,
            width: inner.width,
            height: 1,
        };
        let sep = Paragraph::new(Line::from(
            Span::styled("─".repeat(inner.width as usize), Style::default().fg(Color::DarkGray))
        ));
        frame.render_widget(sep, sep_area);
    }
    
    // Command list
    for (i, cmd) in filtered.iter().enumerate().take(max_visible) {
        let row_y = inner.y + 2 + error_offset + i as u16;
        if row_y >= inner.y + inner.height {
            break;
        }
        let row_area = Rect {
            x: inner.x,
            y: row_y,
            width: inner.width,
            height: 1,
        };
        
        let is_selected = i == palette.selected;
        let (name_style, desc_style, hotkey_style) = if is_selected {
            (
                Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
                Style::default().fg(Color::Black).bg(Color::Yellow),
                Style::default().fg(Color::DarkGray).bg(Color::Yellow),
            )
        } else {
            (
                Style::default().fg(Color::Cyan),
                Style::default().fg(Color::White),
                Style::default().fg(Color::DarkGray),
            )
        };
        
        // Format: "  command_name    description    [hotkey]"
        let name_width = 18;
        let padded_name = format!("  {:<width$}", cmd.name, width = name_width);
        let hotkey_text = format!("  [{}]", cmd.hotkey);
        let desc_width = inner.width as usize - padded_name.len() - hotkey_text.len();
        let padded_desc = if desc_width > 0 {
            format!("{:<width$}", cmd.description, width = desc_width)
        } else {
            cmd.description.to_string()
        };
        
        let line = Line::from(vec![
            Span::styled(padded_name, name_style),
            Span::styled(padded_desc, desc_style),
            Span::styled(hotkey_text, hotkey_style),
        ]);
        let row = Paragraph::new(line);
        if is_selected {
            // Fill background for selected row
            frame.render_widget(Clear, row_area);
        }
        frame.render_widget(row, row_area);
    }
}

/// Show the source selection screen and return the user's choice.
pub fn show_source_selection(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<Option<SourceSelection>> {
    // Enumerate RTL-SDR devices
    use crate::capture::rtlsdr;
    let device_count = rtlsdr::get_device_count();
    let mut devices: Vec<(i32, String)> = Vec::new();
    for i in 0..device_count {
        devices.push((i, rtlsdr::get_device_name(i)));
    }

    loop {
        let devices_ref = &devices;
        terminal.draw(|frame| {
            let size = frame.area();
            let block = Block::default()
                .title(" 📡 TermSDR — Source Selection ")
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::Cyan));

            let mut lines = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Detected RTL-SDR devices:", Style::default().fg(Color::White)),
                ]),
            ];

            if devices_ref.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("    (No RTL-SDR devices detected)", Style::default().fg(Color::DarkGray)),
                ]));
            } else {
                for (idx, name) in devices_ref.iter() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("    [{}] ", idx + 1),
                            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(name.clone(), Style::default().fg(Color::White)),
                    ]));
                }
            }

            lines.extend([
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
            ]);

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
                        let path = show_file_input(terminal)?;
                        if let Some(p) = path {
                            return Ok(Some(SourceSelection::File(p)));
                        }
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                        let num = c.to_digit(10).unwrap() as i32;
                        if num <= device_count {
                            return Ok(Some(SourceSelection::RtlSdr((num - 1) as usize)));
                        }
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
        let mut app = App::new(config, None);

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
        let mut app = App::new(config, None);
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
        let mut app = App::new(config, None);

        app.handle_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn test_app_handle_esc() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.should_quit);
    }

    #[test]
    fn test_app_handle_tune_right() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
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
        let mut app = App::new(config, None);
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
        let mut app = App::new(config, None);
        assert!(app.cursor_active);
        let orig = app.cursor_freq;

        app.handle_key(KeyEvent::from(KeyCode::Right));
        assert!(app.cursor_freq > orig);
    }

    #[test]
    fn test_app_cursor_move_left() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        assert!(app.cursor_active);
        let orig = app.cursor_freq;

        app.handle_key(KeyEvent::from(KeyCode::Left));
        assert!(app.cursor_freq < orig);
    }

    #[test]
    fn test_app_cursor_toggle() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        assert!(app.cursor_active);

        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(!app.cursor_active);

        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(app.cursor_active);
    }

    #[test]
    fn test_app_help_toggle() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
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
        let app = App::new(config, None);
        // Cursor at 0.5 should be at center frequency
        let cursor_freq = app.cursor_frequency();
        assert!((cursor_freq - app.config.center_freq).abs() < 1.0);
    }

    #[test]
    fn test_app_cursor_db() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
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
        let mut app = App::new(config, None);
        let orig = app.config.center_freq;

        app.handle_key(KeyEvent::from(KeyCode::PageUp));
        assert!((app.config.center_freq - (orig + 1_000_000.0)).abs() < 1.0);
    }

    #[test]
    fn test_app_handle_gain_up() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        app.handle_key(KeyEvent::from(KeyCode::Up));
        assert_eq!(app.config.gain, Some(1.0));
    }

    #[test]
    fn test_app_freq_input_mode() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

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
        let mut app = App::new(config, None);
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
        let mut app = App::new(config, None);

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
        let mut app = App::new(config, None);
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
        let mut app = App::new(config, None);

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
        let mut app = App::new(config, None);

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
        let mut app = App::new(config, None);
        assert!((app.channel_bw - 25_000.0).abs() < 1.0);
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        assert!((app.channel_bw - 50_000.0).abs() < 1.0);
    }

    #[test]
    fn test_app_channel_bw_decrease() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.handle_key(KeyEvent::from(KeyCode::Char('W')));
        assert!((app.channel_bw - 12_500.0).abs() < 1.0);
    }

    #[test]
    fn test_app_channel_bw_max() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.channel_bw = 512_000.0;
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        assert!((app.channel_bw - 1_000_000.0).abs() < 1.0);
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        assert!((app.channel_bw - 1_000_000.0).abs() < 1.0); // capped
    }

    #[test]
    fn test_app_channel_bw_min() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.channel_bw = 2_000.0;
        app.handle_key(KeyEvent::from(KeyCode::Char('W')));
        assert!((app.channel_bw - 1_000.0).abs() < 1.0);
        app.handle_key(KeyEvent::from(KeyCode::Char('W')));
        assert!((app.channel_bw - 1_000.0).abs() < 1.0); // capped
    }

    // === Edge-case tests ===

    #[test]
    fn test_cursor_pos_outside_left() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        // Move cursor way to the left of visible range
        app.cursor_freq = app.config.center_freq - app.config.sample_rate * 2.0;
        let pos = app.cursor_pos();
        assert!(pos < 0.0); // outside visible range
    }

    #[test]
    fn test_cursor_pos_outside_right() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.cursor_freq = app.config.center_freq + app.config.sample_rate * 2.0;
        let pos = app.cursor_pos();
        assert!(pos > 1.0); // outside visible range
    }

    #[test]
    fn test_cursor_db_with_empty_fft_bins() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        let fft = FftResult {
            bins: vec![],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        app.latest_fft = Some(fft);
        assert!(app.cursor_db().is_none());
    }

    #[test]
    fn test_cursor_db_cursor_at_extremes() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        let mut bins = vec![-50.0_f32; 100];
        bins[0] = -10.0;
        bins[99] = -20.0;
        let fft = FftResult {
            bins,
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        app.push_fft(fft);

        // Cursor at left edge
        app.cursor_freq = app.config.center_freq - app.config.sample_rate / 2.0;
        let db = app.cursor_db();
        assert!(db.is_some());
    }

    #[test]
    fn test_freq_input_suffix_only() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        let orig = app.config.center_freq;

        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        app.handle_key(KeyEvent::from(KeyCode::Char('M')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        // parse_frequency("M") fails, center_freq unchanged
        assert!(!app.freq_input_mode);
        assert!((app.config.center_freq - orig).abs() < 1.0);
    }

    #[test]
    fn test_bw_input_invalid_value() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        let orig = app.config.sample_rate;

        app.handle_key(KeyEvent::from(KeyCode::Char('b')));
        app.handle_key(KeyEvent::from(KeyCode::Char('x')));
        app.handle_key(KeyEvent::from(KeyCode::Char('y')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(!app.bw_input_mode);
        assert!((app.config.sample_rate - orig).abs() < 1.0);
    }

    #[test]
    fn test_push_many_fft_frames() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        for i in 0..500 {
            let fft = FftResult {
                bins: vec![-50.0 + (i as f32 * 0.1); 100],
                center_freq: 100e6,
                sample_rate: 2.048e6,
            };
            app.push_fft(fft);
        }
        assert!(app.waterfall_history.len() <= app.max_waterfall_rows);
    }

    #[test]
    fn test_auto_range_db_extreme_values() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        let fft = FftResult {
            bins: vec![-200.0, 100.0],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        app.push_fft(fft);
        // Should not panic, and range should be at least 20 dB
        assert!(app.max_db - app.min_db >= 20.0);
    }

    #[test]
    fn test_tune_negative_frequency() {
        let mut config = AppConfig::default();
        config.center_freq = 500_000.0; // low frequency
        let mut app = App::new(config, None);
        // Tune down past zero
        for _ in 0..100 {
            app.handle_key(KeyEvent::from(KeyCode::PageDown));
        }
        // Should not panic; negative freq is technically allowed
        assert!(app.config.center_freq < 500_000.0);
    }

    #[test]
    fn test_tune_very_high_frequency() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        for _ in 0..1000 {
            app.handle_key(KeyEvent::from(KeyCode::PageUp));
        }
        assert!(app.config.center_freq > 100e6);
    }

    #[test]
    fn test_zoom_bandwidth_extremes_repeatedly() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        // Zoom in repeatedly
        for _ in 0..50 {
            app.handle_key(KeyEvent::from(KeyCode::Char('+')));
        }
        assert!(app.config.sample_rate >= 256_000.0); // min clamp

        // Zoom out repeatedly
        for _ in 0..50 {
            app.handle_key(KeyEvent::from(KeyCode::Char('-')));
        }
        assert!(app.config.sample_rate <= 3_200_000.0); // max clamp
    }

    #[test]
    fn test_help_closes_on_any_key() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.show_help = true;
        // 'q' should close help, NOT quit
        app.handle_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(!app.show_help);
        assert!(!app.should_quit);
    }

    #[test]
    fn test_freq_input_backspace() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        app.handle_key(KeyEvent::from(KeyCode::Char('1')));
        app.handle_key(KeyEvent::from(KeyCode::Char('2')));
        app.handle_key(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.freq_input_buf, "1");
    }

    #[test]
    fn test_gain_clamped_to_max() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.config.gain = Some(49.5);
        app.handle_key(KeyEvent::from(KeyCode::Up));
        assert_eq!(app.config.gain, Some(50.0));
        app.handle_key(KeyEvent::from(KeyCode::Up));
        assert_eq!(app.config.gain, Some(50.0)); // capped
    }

    #[test]
    fn test_gain_clamped_to_min() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.config.gain = Some(0.5);
        app.handle_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(app.config.gain, Some(0.0));
        app.handle_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(app.config.gain, Some(0.0)); // capped
    }

    // === Command palette integration tests ===

    #[test]
    fn test_slash_opens_command_palette() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        assert!(app.command_palette_active);
    }

    #[test]
    fn test_slash_not_in_freq_input_mode() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.freq_input_mode = true;
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        assert!(!app.command_palette_active);
        assert_eq!(app.freq_input_buf, "/");
    }

    #[test]
    fn test_slash_not_in_bw_input_mode() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.bw_input_mode = true;
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        assert!(!app.command_palette_active);
        assert_eq!(app.bw_input_buf, "/");
    }

    #[test]
    fn test_esc_closes_command_palette() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        assert!(app.command_palette_active);
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(!app.command_palette_active);
        assert!(!app.should_quit); // Esc in palette should NOT quit
    }

    #[test]
    fn test_command_palette_quit() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        // Type "quit" and press enter
        for c in "quit".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(!app.command_palette_active);
        assert!(app.should_quit);
    }

    #[test]
    fn test_command_palette_channelbw_with_param() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        for c in "channelbw 50k".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(!app.command_palette_active);
        assert!((app.channel_bw - 50_000.0).abs() < 1.0);
    }

    #[test]
    fn test_command_palette_frequency_with_param() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        for c in "frequency 118.1M".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(!app.command_palette_active);
        assert!((app.config.center_freq - 118_100_000.0).abs() < 1.0);
    }

    #[test]
    fn test_command_palette_center() {
        let mut config = AppConfig::default();
        config.center_freq = 100_000_000.0;
        let mut app = App::new(config, None);
        app.cursor_freq = 99_000_000.0; // offset cursor
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        for c in "center".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!((app.cursor_freq - 100_000_000.0).abs() < 1.0);
    }

    #[test]
    fn test_command_palette_typing_does_not_leak() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        let orig_freq = app.config.center_freq;
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        // Type 'q' — should NOT quit, should go into palette input
        app.handle_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(!app.should_quit);
        assert!(app.command_palette_active);
        assert_eq!(app.command_palette.input, "q");
        // Press Esc to cancel
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(!app.command_palette_active);
        assert!((app.config.center_freq - orig_freq).abs() < 1.0);
    }

    #[test]
    fn test_command_palette_tab_autocomplete() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        for c in "ch".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        // Should autocomplete to "channelbw " (with space since it accepts params)
        assert_eq!(app.command_palette.input, "channelbw ");
    }
}
