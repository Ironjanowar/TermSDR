//! Terminal user interface: application state, event loop, and rendering.

pub(crate) mod colors;
pub(crate) mod command_palette;
pub(crate) mod settings_modal;
pub(crate) mod spectrum;
pub(crate) mod status_bar;
pub(crate) mod waterfall;

use std::io::{self, Stdout};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
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
use crate::audio::demod::DemodMode;
use crate::bookmarks::{Bookmark, BookmarkStore, default_catalog};
use crate::types::{AppConfig, FftResult, SourceType};
use command_palette::{CommandAction, CommandPalette};
use settings_modal::SettingsModal;
use spectrum::SpectrumWidget;
use status_bar::{DisplaySource, StatusBarWidget};
use waterfall::WaterfallWidget;

/// VFO color palette (8 colors, assigned in order, reused if needed).
const VFO_COLORS: [Color; 8] = [
    Color::Red,
    Color::Green,
    Color::Blue,
    Color::Magenta,
    Color::Cyan,
    Color::LightRed,
    Color::White,
    Color::LightMagenta,
];

/// A pinned VFO (Variable Frequency Oscillator) with its own frequency and demodulator.
#[derive(Debug, Clone)]
pub(crate) struct Vfo {
    pub id: usize,
    pub freq: f64,
    pub channel_bw: f64,
    pub color: Color,
    pub demod_mode: DemodMode,
}

/// Source selection at startup.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceSelection {
    Test,
    File(String),
    RtlSdr(usize),
}

/// The main application state for the TUI.
#[allow(clippy::struct_excessive_bools)] // UI state naturally has many boolean flags
pub struct App {
    /// Current config (may be mutated by controls).
    pub config: AppConfig,
    /// Latest FFT result to display.
    pub latest_fft: Option<FftResult>,
    /// Waterfall history (newest first).
    pub waterfall_history: Vec<Vec<f32>>,
    /// Maximum waterfall rows to keep.
    pub max_waterfall_rows: usize,
    /// Waterfall time span in seconds.
    pub waterfall_time_span: f64,
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
    /// Pinned VFOs.
    pub(crate) vfos: Vec<Vfo>,
    /// Next VFO ID (1-based).
    pub next_vfo_id: usize,
    /// Whether VFO delete modal is shown.
    pub show_vfo_delete_modal: bool,
    /// Bookmark store (user bookmarks).
    pub bookmark_store: BookmarkStore,
    /// Whether bookmarks modal is shown.
    pub show_bookmarks_modal: bool,
    /// Selected index in bookmarks modal.
    pub bookmark_selected: usize,
    /// Flattened bookmark list for the modal (catalog + user).
    pub bookmark_list: Vec<Bookmark>,
    /// Audio control channel sender.
    pub audio_control_tx: Option<crossbeam_channel::Sender<AudioControl>>,
    /// Shared center frequency for `TestSource` reactivity.
    pub shared_center_freq: Option<Arc<AtomicU64>>,
    /// Spectrum averaging count.
    pub averaging: usize,
    /// Shared averaging count for DSP thread.
    pub shared_averaging: Option<Arc<AtomicUsize>>,
    /// Demodulation mode for cursor.
    pub demod_mode: DemodMode,
    /// Scan mode: auto-tune cursor to strongest signal.
    pub scan_active: bool,
    /// Scan interval in milliseconds.
    pub scan_interval_ms: u64,
    /// Last time scan was performed.
    pub last_scan_time: Instant,
    /// Settings modal.
    pub settings_modal: SettingsModal,
    /// Frame timing.
    frame_count: u64,
    last_fps_update: Instant,
    last_frame_count: u64,
}

impl App {
    #[must_use] 
    pub fn new(config: AppConfig, audio_control_tx: Option<crossbeam_channel::Sender<AudioControl>>) -> Self {
        let center = config.center_freq;
        Self {
            config,
            latest_fft: None,
            waterfall_history: Vec::new(),
            max_waterfall_rows: 600,
            waterfall_time_span: 20.0,
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
            channel_bw: DemodMode::AM.default_channel_bw(),
            command_palette_active: false,
            command_palette: CommandPalette::new(),
            volume: 80,
            squelch: None,
            vfos: Vec::new(),
            next_vfo_id: 1,
            show_vfo_delete_modal: false,
            bookmark_store: BookmarkStore::load(),
            show_bookmarks_modal: false,
            bookmark_selected: 0,
            bookmark_list: Vec::new(),
            audio_control_tx,
            shared_center_freq: None,
            averaging: 4,
            shared_averaging: None,
            demod_mode: DemodMode::AM,
            scan_active: false,
            scan_interval_ms: 500,
            last_scan_time: Instant::now(),
            settings_modal: SettingsModal::new(),
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

    /// Sync shared center frequency for capture/DSP threads and audio thread.
    fn sync_shared_center_freq(&self) {
        if let Some(ref freq) = self.shared_center_freq {
            freq.store(self.config.center_freq.to_bits(), Ordering::Relaxed);
        }
        self.send_audio_control(AudioControl::SetCenterFreq(self.config.center_freq));
    }

    /// Sync shared averaging count for DSP thread.
    fn sync_averaging(&self) {
        if let Some(ref avg) = self.shared_averaging {
            avg.store(self.averaging, Ordering::Relaxed);
        }
    }

    /// Rebuild the flattened bookmark list for the modal.
    fn rebuild_bookmark_list(&mut self) {
        let mut list = default_catalog();
        for b in &self.bookmark_store.user_bookmarks {
            let mut b = b.clone();
            b.category = "⭐ My Bookmarks".to_string();
            list.push(b);
        }
        self.bookmark_list = list;
    }

    /// Tune to a bookmark: set freq, BW, mode, send audio controls.
    fn tune_to_bookmark(&mut self, bookmark: &Bookmark) {
        self.config.center_freq = bookmark.freq;
        self.cursor_freq = bookmark.freq;
        self.channel_bw = bookmark.channel_bw;
        self.demod_mode = bookmark.mode;
        self.sync_shared_center_freq();
        self.send_audio_control(AudioControl::SetCursorFreq(self.cursor_freq));
        self.send_audio_control(AudioControl::SetChannelBw(self.channel_bw));
        self.send_audio_control(AudioControl::SetDemodMode(self.demod_mode));
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
            let min = fft.bins.iter().copied().fold(f32::INFINITY, f32::min);
            let max = fft.bins.iter().copied().fold(f32::NEG_INFINITY, f32::max);
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

    /// Auto-scan: move cursor to the strongest signal if scan is active.
    #[allow(clippy::cast_possible_truncation)] // intentional truncation
    pub fn maybe_scan(&mut self) {
        if !self.scan_active { return; }
        if (self.last_scan_time.elapsed().as_millis() as u64) < self.scan_interval_ms { return; }
        self.last_scan_time = Instant::now();

        if let Some(ref fft) = self.latest_fft {
            if fft.bins.is_empty() { return; }
            let (peak_bin, peak_db) = fft.peak();

            // Respect squelch: don't move if peak is below threshold
            if let Some(sq) = self.squelch {
                if peak_db < sq {
                    return;
                }
            }

            // Move cursor to peak frequency (within visible bandwidth only)
            let peak_freq = fft.bin_frequency(peak_bin);
            self.cursor_freq = peak_freq;
            self.send_audio_control(AudioControl::SetCursorFreq(self.cursor_freq));
        }
    }

    /// Update FPS counter.
    #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
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
    ///
    /// # Panics
    /// Panics if internal state (e.g., VFO indices) becomes inconsistent.
    #[allow(clippy::too_many_lines)] // large key event dispatch
    pub fn handle_key(&mut self, key: KeyEvent) {
        // Help modal takes priority — any key closes it
        if self.show_help {
            self.show_help = false;
            return;
        }

        // Settings modal
        if self.settings_modal.visible {
            // While showing help, only ? and Esc dismiss it
            if self.settings_modal.showing_help {
                match key.code {
                    KeyCode::Char('?') | KeyCode::Esc => {
                        self.settings_modal.showing_help = false;
                    }
                    _ => {} // ignore all other keys
                }
                return;
            }
            if self.settings_modal.editing {
                match key.code {
                    KeyCode::Enter => {
                        if let Some(ctrl) = self.settings_modal.confirm_edit() {
                            self.apply_settings_control(ctrl);
                        }
                    }
                    KeyCode::Esc => {
                        self.settings_modal.cancel_edit();
                    }
                    KeyCode::Backspace => {
                        self.settings_modal.edit_backspace();
                    }
                    KeyCode::Char(c) => {
                        self.settings_modal.edit_type(c);
                    }
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Esc => {
                        self.settings_modal.close();
                    }
                    KeyCode::Up => {
                        self.settings_modal.select_up();
                    }
                    KeyCode::Down => {
                        self.settings_modal.select_down();
                    }
                    KeyCode::Left => {
                        if let Some(ctrl) = self.settings_modal.adjust_fine(-1) {
                            self.apply_settings_control(ctrl);
                        }
                    }
                    KeyCode::Right => {
                        if let Some(ctrl) = self.settings_modal.adjust_fine(1) {
                            self.apply_settings_control(ctrl);
                        }
                    }
                    KeyCode::Char('H' | 'h') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        if let Some(ctrl) = self.settings_modal.adjust_coarse(-1) {
                            self.apply_settings_control(ctrl);
                        }
                    }
                    KeyCode::Char('L' | 'l') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        if let Some(ctrl) = self.settings_modal.adjust_coarse(1) {
                            self.apply_settings_control(ctrl);
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(ctrl) = self.settings_modal.activate() {
                            self.apply_settings_control(ctrl);
                        }
                    }
                    KeyCode::Char('?') => {
                        self.settings_modal.toggle_help();
                    }
                    KeyCode::Char('d') => {
                        if let Some(ctrl) = self.settings_modal.reset_to_default() {
                            self.apply_settings_control(ctrl);
                        }
                    }
                    _ => {}
                }
            }
            return;
        }

        // VFO delete modal
        if self.show_vfo_delete_modal {
            match key.code {
                KeyCode::Esc => {
                    self.show_vfo_delete_modal = false;
                }
                KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                    let num = c.to_digit(10).expect("invariant: is_ascii_digit guard") as usize;
                    if let Some(idx) = self.vfos.iter().position(|v| v.id == num) {
                        let vfo = self.vfos.remove(idx);
                        self.send_audio_control(AudioControl::RemoveVfo(vfo.id));
                        self.show_vfo_delete_modal = false;
                    }
                }
                _ => {}
            }
            return;
        }

        // Bookmarks modal
        if self.show_bookmarks_modal {
            match key.code {
                KeyCode::Esc => {
                    self.show_bookmarks_modal = false;
                }
                KeyCode::Up
                    if self.bookmark_selected > 0 => {
                        self.bookmark_selected -= 1;
                    }
                KeyCode::Down
                    if !self.bookmark_list.is_empty() && self.bookmark_selected < self.bookmark_list.len() - 1 => {
                        self.bookmark_selected += 1;
                    }
                KeyCode::Enter => {
                    if let Some(bookmark) = self.bookmark_list.get(self.bookmark_selected).cloned() {
                        self.tune_to_bookmark(&bookmark);
                        self.show_bookmarks_modal = false;
                    }
                }
                KeyCode::Char('m' | 'M') => {
                    if let Some(bookmark) = self.bookmark_list.get(self.bookmark_selected).cloned() {
                        self.tune_to_bookmark(&bookmark);
                        self.show_bookmarks_modal = false;
                        // Also create a VFO
                        if self.vfos.len() < 8 {
                            let id = self.next_vfo_id;
                            let color = VFO_COLORS[self.vfos.len() % 8];
                            let vfo = Vfo {
                                id,
                                freq: self.cursor_freq,
                                channel_bw: self.channel_bw,
                                color,
                                demod_mode: self.demod_mode,
                            };
                            self.vfos.push(vfo);
                            self.next_vfo_id += 1;
                            self.send_audio_control(AudioControl::AddVfo {
                                id,
                                freq: self.cursor_freq,
                                channel_bw: self.channel_bw,
                                mode: self.demod_mode,
                            });
                        }
                    }
                }
                _ => {}
            }
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
                        self.sync_shared_center_freq();
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
            KeyCode::Char('s') => {
                self.settings_modal.open(self.volume, self.squelch, self.channel_bw, self.demod_mode);
            }
            KeyCode::Char('S') => {
                self.scan_active = !self.scan_active;
                if self.scan_active {
                    self.last_scan_time = Instant::now();
                    if !self.cursor_active {
                        self.cursor_active = true;
                        self.send_audio_control(AudioControl::Mute(false));
                    }
                }
            }
            KeyCode::Left => {
                if self.cursor_active {
                    // Move cursor left by 2% of visible bandwidth
                    self.cursor_freq -= 0.02 * self.config.sample_rate;
                    self.scan_active = false;
                    self.send_audio_control(AudioControl::SetCursorFreq(self.cursor_freq));
                } else {
                    // Fine tune: visual scroll right (lower freq)
                    self.config.center_freq += 25_000.0;
                    self.cursor_freq += 25_000.0;
                    self.sync_shared_center_freq();
                    self.send_audio_control(AudioControl::SetCursorFreq(self.cursor_freq));
                }
            }
            KeyCode::Right => {
                if self.cursor_active {
                    // Move cursor right by 2% of visible bandwidth
                    self.cursor_freq += 0.02 * self.config.sample_rate;
                    self.scan_active = false;
                    self.send_audio_control(AudioControl::SetCursorFreq(self.cursor_freq));
                } else {
                    // Fine tune: visual scroll left (higher freq)
                    self.config.center_freq -= 25_000.0;
                    self.cursor_freq -= 25_000.0;
                    self.sync_shared_center_freq();
                    self.send_audio_control(AudioControl::SetCursorFreq(self.cursor_freq));
                }
            }
            KeyCode::PageUp => {
                // Coarse tune: +1 MHz
                self.config.center_freq += 1_000_000.0;
                if !self.cursor_active {
                    self.cursor_freq += 1_000_000.0;
                    self.send_audio_control(AudioControl::SetCursorFreq(self.cursor_freq));
                }
                self.sync_shared_center_freq();
            }
            KeyCode::PageDown => {
                // Coarse tune: -1 MHz
                self.config.center_freq -= 1_000_000.0;
                if !self.cursor_active {
                    self.cursor_freq -= 1_000_000.0;
                    self.send_audio_control(AudioControl::SetCursorFreq(self.cursor_freq));
                }
                self.sync_shared_center_freq();
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
            KeyCode::Char('+' | '=') => {
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
            KeyCode::Char('C') => {
                // Center bandwidth on cursor (retune to cursor frequency)
                self.config.center_freq = self.cursor_freq;
                self.sync_shared_center_freq();
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
            KeyCode::Char('t') => {
                self.waterfall_time_span = (self.waterfall_time_span + 5.0).min(120.0);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // waterfall_time_span is bounded 5..120
                let rows = (self.waterfall_time_span * 30.0) as usize;
                self.max_waterfall_rows = rows;
            }
            KeyCode::Char('T') => {
                self.waterfall_time_span = (self.waterfall_time_span - 5.0).max(5.0);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // waterfall_time_span is bounded 5..120
                let rows = (self.waterfall_time_span * 30.0) as usize;
                self.max_waterfall_rows = rows;
                self.waterfall_history.truncate(self.max_waterfall_rows);
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
            KeyCode::Char('m')
                if self.vfos.len() < 8 => {
                    let id = self.next_vfo_id;
                    let color = VFO_COLORS[self.vfos.len() % 8];
                    let vfo = Vfo {
                        id,
                        freq: self.cursor_freq,
                        channel_bw: self.channel_bw,
                        color,
                        demod_mode: self.demod_mode,
                    };
                    self.vfos.push(vfo);
                    self.next_vfo_id += 1;
                    self.send_audio_control(AudioControl::AddVfo {
                        id,
                        freq: self.cursor_freq,
                        channel_bw: self.channel_bw,
                        mode: self.demod_mode,
                    });
                }
            KeyCode::Char('x')
                if !self.vfos.is_empty() => {
                    self.show_vfo_delete_modal = true;
                }
            KeyCode::Char('d') => {
                // Cycle demod mode: AM → FM → NFM → AM
                let new_mode = self.demod_mode.next();
                self.demod_mode = new_mode;
                self.channel_bw = new_mode.default_channel_bw();
                self.send_audio_control(AudioControl::SetDemodMode(new_mode));
                self.send_audio_control(AudioControl::SetChannelBw(self.channel_bw));
            }
            _ => {}
        }
    }

    /// Apply an audio control from the settings modal, syncing local state.
    fn apply_settings_control(&mut self, ctrl: AudioControl) {
        match &ctrl {
            AudioControl::SetVolume(v) => self.volume = *v,
            AudioControl::SetSquelch(s) => self.squelch = *s,
            AudioControl::SetChannelBw(bw) => self.channel_bw = *bw,
            AudioControl::SetDemodMode(mode) => {
                self.demod_mode = *mode;
                self.channel_bw = mode.default_channel_bw();
                // Sync channel BW back to modal so slider reflects new default
                for item in &mut self.settings_modal.items {
                    if item.id == settings_modal::SettingsItemId::ChannelBw {
                        if let settings_modal::SettingsItemKind::Slider { ref mut value, .. } = item.kind {
                            *value = self.channel_bw;
                        }
                        break;
                    }
                }
            }
            _ => {} // AGC/bandpass don't have App-level state
        }
        self.send_audio_control(ctrl);
    }

    /// Apply a command action from the command palette.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // intentional truncation; value is non-negative here
    #[allow(clippy::too_many_lines)] // large command action dispatch
    fn apply_command_action(&mut self, action: CommandAction) {
        match action {
            CommandAction::SetFrequency(f) => {
                self.config.center_freq = f;
                self.sync_shared_center_freq();
            }
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
            CommandAction::CenterOnCursor => {
                self.config.center_freq = self.cursor_freq;
                self.sync_shared_center_freq();
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
            CommandAction::TuneUp => {
                self.config.center_freq += 25_000.0;
                self.sync_shared_center_freq();
            }
            CommandAction::TuneDown => {
                self.config.center_freq -= 25_000.0;
                self.sync_shared_center_freq();
            }
            CommandAction::TuneCoarseUp => {
                self.config.center_freq += 1_000_000.0;
                self.sync_shared_center_freq();
            }
            CommandAction::TuneCoarseDown => {
                self.config.center_freq -= 1_000_000.0;
                self.sync_shared_center_freq();
            }
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
            CommandAction::SetWaterfallTimeSpan(secs) => {
                self.waterfall_time_span = secs.clamp(5.0, 120.0);
                self.max_waterfall_rows = (self.waterfall_time_span * 30.0) as usize;
                self.waterfall_history.truncate(self.max_waterfall_rows);
            }
            CommandAction::AddVfo(freq) => {
                if self.vfos.len() < 8 {
                    let id = self.next_vfo_id;
                    let color = VFO_COLORS[self.vfos.len() % 8];
                    let vfo = Vfo {
                        id,
                        freq,
                        channel_bw: self.channel_bw,
                        color,
                        demod_mode: self.demod_mode,
                    };
                    self.vfos.push(vfo);
                    self.next_vfo_id += 1;
                    self.send_audio_control(AudioControl::AddVfo {
                        id,
                        freq,
                        channel_bw: self.channel_bw,
                        mode: self.demod_mode,
                    });
                }
            }
            CommandAction::RemoveVfo(id) => {
                if let Some(idx) = self.vfos.iter().position(|v| v.id == id) {
                    self.vfos.remove(idx);
                    self.send_audio_control(AudioControl::RemoveVfo(id));
                }
            }
            CommandAction::ClearVfos => {
                let ids: Vec<usize> = self.vfos.iter().map(|v| v.id).collect();
                self.vfos.clear();
                for id in ids {
                    self.send_audio_control(AudioControl::RemoveVfo(id));
                }
            }
            CommandAction::SetDemodMode(mode) => {
                self.demod_mode = mode;
                self.channel_bw = mode.default_channel_bw();
                self.send_audio_control(AudioControl::SetDemodMode(mode));
                self.send_audio_control(AudioControl::SetChannelBw(self.channel_bw));
            }
            CommandAction::ShowBookmarks => {
                self.rebuild_bookmark_list();
                self.bookmark_selected = 0;
                self.show_bookmarks_modal = true;
            }
            CommandAction::AddBookmark(name) => {
                let bookmark = Bookmark {
                    name,
                    freq: self.cursor_freq,
                    channel_bw: self.channel_bw,
                    mode: self.demod_mode,
                    category: "⭐ My Bookmarks".into(),
                };
                self.bookmark_store.add(bookmark);
            }
            CommandAction::RemoveBookmark(name) => {
                // Try by name first, then by index
                if !self.bookmark_store.remove_by_name(&name) {
                    if let Ok(idx) = name.parse::<usize>() {
                        if idx > 0 {
                            self.bookmark_store.remove(idx - 1);
                        }
                    }
                }
            }
            CommandAction::SetAveraging(count) => {
                self.averaging = count;
                self.sync_averaging();
            }
            CommandAction::ToggleScan => {
                self.scan_active = !self.scan_active;
                if self.scan_active {
                    self.last_scan_time = Instant::now();
                    if !self.cursor_active {
                        self.cursor_active = true;
                        self.send_audio_control(AudioControl::Mute(false));
                    }
                }
            }
            CommandAction::SetScanInterval(ms) => {
                self.scan_interval_ms = ms;
            }
        }
    }

    /// Get the display source type.
    #[must_use] 
    pub(crate) fn display_source(&self) -> DisplaySource {
        match &self.config.source {
            SourceType::Test { .. } => DisplaySource::Test,
            SourceType::File { path } => DisplaySource::File(path.clone()),
            SourceType::RtlSdr { .. } => DisplaySource::RtlSdr,
        }
    }

    /// Get the relative cursor position (0.0–1.0) within the visible spectrum.
    #[must_use] 
    #[allow(clippy::cast_possible_truncation)] // intentional truncation
    pub(crate) fn cursor_pos(&self) -> f32 {
        let start = self.config.center_freq - self.config.sample_rate / 2.0;
        ((self.cursor_freq - start) / self.config.sample_rate) as f32
    }

    /// Get the frequency at the current cursor position.
    #[must_use] 
    pub(crate) fn cursor_frequency(&self) -> f64 {
        self.cursor_freq
    }

    /// Get the dB value at the current cursor position from the latest FFT.
    #[must_use] 
    pub(crate) fn cursor_db(&self) -> Option<f32> {
        let pos = self.cursor_pos();
        self.latest_fft.as_ref().and_then(|fft| {
            if fft.bins.is_empty() {
                return None;
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)] // precision loss acceptable for DSP/UI; intentional truncation; value is non-negative here
            let bin_index = (pos * (fft.bins.len() - 1) as f32) as usize;
            fft.bins.get(bin_index).copied()
        })
    }
}

/// Initialize the terminal for TUI.
///
/// # Errors
/// Returns an error if raw mode cannot be enabled or the alternate screen cannot be entered.
pub fn init_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal to normal state.
///
/// # Errors
/// Returns an error if the terminal cannot be restored.
pub fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Draw the main UI.
#[allow(clippy::too_many_lines)] // complex UI layout composition
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
    let vfo_data: Vec<(f64, f64, Color, usize)> = app.vfos.iter()
        .map(|v| (v.freq, v.channel_bw, v.color, v.id))
        .collect();
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
    ).with_vfos(&vfo_data)
    .with_squelch(app.squelch)
    .with_scan(app.scan_active);
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
    ).with_vfos(&vfo_data);
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
    .with_audio(app.volume, app.cursor_active, app.squelch)
    .with_waterfall_time_span(app.waterfall_time_span)
    .with_demod_mode(app.demod_mode.label())
    .with_averaging(app.averaging)
    .with_scan(app.scan_active);
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

    // VFO delete modal
    if app.show_vfo_delete_modal {
        draw_vfo_delete_modal(frame, size, &app.vfos);
    }

    // Bookmarks modal
    if app.show_bookmarks_modal {
        draw_bookmarks_modal(frame, size, &app.bookmark_list, app.bookmark_selected);
    }

    // Settings modal
    if app.settings_modal.visible {
        draw_settings_modal(frame, size, &app.settings_modal);
    }
}

/// Draw the help/cheatsheet modal.
#[allow(clippy::too_many_lines)] // help text layout
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
        Line::from(vec![
            Span::styled("    m         ", Style::default().fg(Color::Cyan)),
            Span::styled("Pin VFO at cursor position", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("    x         ", Style::default().fg(Color::Cyan)),
            Span::styled("Delete a pinned VFO", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("    d         ", Style::default().fg(Color::Cyan)),
            Span::styled("Cycle demod mode (AM \u{2192} FM \u{2192} NFM)", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("    s         ", Style::default().fg(Color::Cyan)),
            Span::styled("Toggle scan mode (auto-tune to strongest signal)", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("    t / T     ", Style::default().fg(Color::Cyan)),
            Span::styled("Waterfall time span +/- 5s", Style::default().fg(Color::White)),
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
            Span::styled("              ", Style::default().fg(Color::Cyan)),
            Span::styled("/bookmarks, /bookmark-add <name>", Style::default().fg(Color::DarkGray)),
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
#[allow(clippy::too_many_lines)] // command palette UI layout
fn draw_command_palette(frame: &mut Frame, size: Rect, palette: &CommandPalette) {
    let filtered = palette.filtered();
    
    // Calculate height: input(1) + error?(1) + separator(1) + results + border(2)
    let max_visible = 12;
    let result_count = filtered.len().min(max_visible);
    let has_error = palette.error.is_some();
    let error_rows: u16 = u16::from(has_error);
    #[allow(clippy::cast_possible_truncation)] // intentional truncation
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
                Span::styled(format!("  ⚠ {error_text}"), Style::default().fg(Color::Red))
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
        #[allow(clippy::cast_possible_truncation)] // intentional truncation
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

/// Draw the VFO delete modal.
fn draw_vfo_delete_modal(frame: &mut Frame, size: Rect, vfos: &[Vfo]) {
    use spectrum::format_frequency;

    #[allow(clippy::cast_possible_truncation)] // intentional truncation
    let height = (vfos.len() as u16 + 5).min(size.height);
    let width = 50.min(size.width);
    let area = Rect {
        x: (size.width - width) / 2,
        y: (size.height - height) / 2,
        width,
        height,
    };

    let block = Block::default()
        .title(" Delete VFO ")
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Red).bg(Color::Black));

    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    for (i, vfo) in vfos.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)] // intentional truncation
        let y = inner.y + i as u16;
        if y >= inner.y + inner.height.saturating_sub(1) {
            break;
        }
        let freq_str = format_frequency(vfo.freq);
        let bw_str = format_frequency(vfo.channel_bw);
        let label = format!("  [{}] {} {}  BW: {}", vfo.id, vfo.demod_mode.label(), freq_str, bw_str);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(label, Style::default().fg(vfo.color)))),
            Rect { x: inner.x, y, width: inner.width, height: 1 },
        );
    }

    let hint_y = inner.y + inner.height.saturating_sub(1);
    if hint_y < area.y + area.height {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  Press number to delete, Esc to cancel",
                Style::default().fg(Color::DarkGray),
            ))),
            Rect { x: inner.x, y: hint_y, width: inner.width, height: 1 },
        );
    }
}

/// Draw the bookmarks browser modal.
fn draw_bookmarks_modal(frame: &mut Frame, size: Rect, bookmarks: &[Bookmark], selected: usize) {
    use spectrum::format_frequency;

    let area = centered_rect(70, 80, size);
    let block = Block::default()
        .title(" 📡 Bookmarks ")
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Cyan).bg(Color::Black));

    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    if inner.height < 3 {
        return;
    }

    // Group by category with headers
    let mut row_y = inner.y;
    let mut last_category = "";

    // Calculate scroll offset to keep selected visible
    // First pass: count total rows and find selected row
    let mut total_rows: usize = 0;
    let mut selected_row: usize = 0;
    let mut last_cat_pass = "";
    for (i, bm) in bookmarks.iter().enumerate() {
        if bm.category != last_cat_pass {
            total_rows += 1; // category header
            last_cat_pass = &bm.category;
        }
        if i == selected {
            selected_row = total_rows;
        }
        total_rows += 1;
    }

    let visible_rows = (inner.height - 1) as usize; // -1 for hint line
    let scroll_offset = if selected_row >= visible_rows {
        selected_row - visible_rows + 1
    } else {
        0
    };

    let mut current_row: usize = 0;
    for (i, bm) in bookmarks.iter().enumerate() {
        if row_y >= inner.y + inner.height.saturating_sub(1) {
            break;
        }

        // Category header
        if bm.category.as_str() != last_category {
            if current_row >= scroll_offset {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!("  {}", bm.category),
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ))),
                    Rect { x: inner.x, y: row_y, width: inner.width, height: 1 },
                );
                row_y += 1;
                if row_y >= inner.y + inner.height.saturating_sub(1) {
                    break;
                }
            }
            current_row += 1;
            last_category = &bm.category;
        }

        if current_row >= scroll_offset {
            let freq_str = format_frequency(bm.freq);
            let bw_str = format_frequency(bm.channel_bw);
            let is_selected = i == selected;

            let (name_style, info_style) = if is_selected {
                (
                    Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
                    Style::default().fg(Color::Black).bg(Color::Yellow),
                )
            } else {
                (
                    Style::default().fg(Color::White),
                    Style::default().fg(Color::DarkGray),
                )
            };

            let label = format!("    {} {}", freq_str, bm.name);
            let info = format!("  {} BW:{}", bm.mode.label(), bw_str);
            let line = Line::from(vec![
                Span::styled(format!("{label:<40}"), name_style),
                Span::styled(info, info_style),
            ]);

            let row_area = Rect { x: inner.x, y: row_y, width: inner.width, height: 1 };
            if is_selected {
                frame.render_widget(Clear, row_area);
            }
            frame.render_widget(Paragraph::new(line), row_area);
            row_y += 1;
        }
        current_row += 1;
    }

    // Hint line at bottom
    let hint_y = inner.y + inner.height.saturating_sub(1);
    if hint_y < area.y + area.height {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  ↑↓ Navigate  Enter Tune  M Tune+VFO  Esc Close",
                Style::default().fg(Color::DarkGray),
            ))),
            Rect { x: inner.x, y: hint_y, width: inner.width, height: 1 },
        );
    }
}

/// Show the source selection screen and return the user's choice.
///
/// # Errors
/// Returns an error if terminal drawing or input reading fails.
///
/// # Panics
/// Panics if the terminal backend is in an invalid state.
#[allow(clippy::cast_sign_loss)] // value is non-negative here
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
                for (idx, name) in devices_ref {
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
                    KeyCode::Char('t' | 'T') => {
                        return Ok(Some(SourceSelection::Test));
                    }
                    KeyCode::Char('f' | 'F') => {
                        let path = show_file_input(terminal)?;
                        if let Some(p) = path {
                            return Ok(Some(SourceSelection::File(p)));
                        }
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                        #[allow(clippy::cast_possible_wrap)] // value fits in target type
                        let num = c.to_digit(10).expect("invariant: is_ascii_digit guard") as i32;
                        if num <= device_count {
                            return Ok(Some(SourceSelection::RtlSdr((num - 1) as usize)));
                        }
                    }
                    KeyCode::Char('q' | 'Q') | KeyCode::Esc => {
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
                    KeyCode::Enter
                        if !input.is_empty() => {
                            return Ok(Some(input));
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

/// Draw the settings modal.
#[allow(clippy::too_many_lines)] // settings modal UI layout
fn draw_settings_modal(frame: &mut Frame, size: Rect, modal: &SettingsModal) {
    use settings_modal::{SettingsItemKind, format_display_value};

    // Calculate total rows: items + section headers + title + footer
    let section_headers = modal.section_headers();
    let total_item_rows = modal.items.len() + section_headers.len();
    #[allow(clippy::cast_possible_truncation)] // intentional truncation
    let height = (total_item_rows as u16 + 5).min(size.height); // +5 for border, title, footer
    let width = 54.min(size.width);

    let area = Rect {
        x: (size.width - width) / 2,
        y: (size.height - height) / 2,
        width,
        height,
    };

    let block = Block::default()
        .title(" 🔊 Audio Settings ")
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Cyan).bg(Color::Black));

    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    let mut row_y = inner.y;
    let section_starts: Vec<usize> = section_headers.iter().map(|(i, _)| *i).collect();

    for (i, item) in modal.items.iter().enumerate() {
        if row_y >= inner.y + inner.height.saturating_sub(1) {
            break;
        }

        // Section header
        if section_starts.contains(&i) {
            if let Some(section) = item.section {
                let header = format!("── {section} ──");
                let pad = inner.width as usize;
                let centered = format!("{header:^pad$}");
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        centered,
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                    ))),
                    Rect { x: inner.x, y: row_y, width: inner.width, height: 1 },
                );
                row_y += 1;
                if row_y >= inner.y + inner.height.saturating_sub(1) {
                    break;
                }
            }
        }

        let is_selected = i == modal.selected_index;
        let marker = if is_selected { "► " } else { "  " };
        let value_str = if modal.editing && is_selected {
            format!("{}█", modal.edit_buffer)
        } else {
            format_display_value(&item.kind)
        };

        let label_width = 16;
        let label = format!("{}{:<width$}", marker, item.label, width = label_width);

        let (label_style, value_style) = if is_selected {
            (
                Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
                Style::default().fg(Color::Black).bg(Color::Yellow),
            )
        } else {
            (
                Style::default().fg(Color::White),
                Style::default().fg(Color::Cyan),
            )
        };

        // For sliders, add a small bar
        let bar_str = match &item.kind {
            SettingsItemKind::Slider { value, min, max, .. } if !modal.editing || !is_selected => {
                let pct = ((value - min) / (max - min)).clamp(0.0, 1.0);
                let bar_width = 10;
                #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)] // precision loss acceptable for DSP/UI; intentional truncation; value is non-negative here
                let filled = (pct * bar_width as f64) as usize;
                let empty = bar_width - filled;
                format!(" [{}{}]", "█".repeat(filled), "░".repeat(empty))
            }
            _ => String::new(),
        };

        let row_area = Rect { x: inner.x, y: row_y, width: inner.width, height: 1 };
        if is_selected {
            frame.render_widget(Clear, row_area);
        }

        let value_and_bar = format!("{value_str}{bar_str}");
        let line = Line::from(vec![
            Span::styled(label, label_style),
            Span::styled(value_and_bar, value_style),
        ]);
        frame.render_widget(Paragraph::new(line), row_area);
        row_y += 1;
    }

    // Footer with dynamic legend
    let hint_y = inner.y + inner.height.saturating_sub(1);
    if hint_y < area.y + area.height {
        let hint = if modal.editing {
            "  Type value, Enter confirm, Esc cancel".to_string()
        } else if modal.showing_help {
            "  [?] Close help | [Esc] Close help".to_string()
        } else if let Some(item) = modal.selected_item() {
            match &item.kind {
                SettingsItemKind::Slider { fine_step, coarse_step, unit, .. } => {
                    format!("  ←→ ±{}{} | H/L ±{}{} | ? Help | d Default",
                        format_step(*fine_step), unit, format_step(*coarse_step), unit)
                }
                SettingsItemKind::Toggle { .. } => {
                    "  ←→/Enter toggle | ? Help | d Default".to_string()
                }
                SettingsItemKind::Cycle { .. } => {
                    "  ←→/Enter cycle | ? Help | d Default".to_string()
                }
            }
        } else {
            "  Esc close".to_string()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            ))),
            Rect { x: inner.x, y: hint_y, width: inner.width, height: 1 },
        );
    }

    // Help tooltip overlay
    if modal.showing_help {
        if let Some(item) = modal.selected_item() {
            let help = item.help_text;
            // Word-wrap help text to fit tooltip width
            let tooltip_width = (width.saturating_sub(4)) as usize;
            let mut lines: Vec<String> = Vec::new();
            let mut current_line = String::new();
            for word in help.split_whitespace() {
                if current_line.is_empty() {
                    current_line = word.to_string();
                } else if current_line.len() + 1 + word.len() <= tooltip_width.saturating_sub(2) {
                    current_line.push(' ');
                    current_line.push_str(word);
                } else {
                    lines.push(current_line);
                    current_line = word.to_string();
                }
            }
            if !current_line.is_empty() {
                lines.push(current_line);
            }
            #[allow(clippy::cast_possible_truncation)] // intentional truncation
            let tooltip_h = (lines.len() as u16 + 2).min(size.height); // +2 for border
            let tooltip_y = area.y + area.height;
            let tooltip_y = if tooltip_y + tooltip_h > size.height {
                area.y.saturating_sub(tooltip_h)
            } else {
                tooltip_y
            };
            let tooltip_area = Rect {
                x: area.x,
                y: tooltip_y,
                width,
                height: tooltip_h,
            };
            let tooltip_block = Block::default()
                .title(format!(" {} ", item.label))
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::Yellow).bg(Color::Black));
            frame.render_widget(Clear, tooltip_area);
            let help_lines: Vec<Line> = lines.iter().map(|l| {
                Line::from(Span::styled(l.as_str(), Style::default().fg(Color::White)))
            }).collect();
            let help_para = Paragraph::new(help_lines).block(tooltip_block);
            frame.render_widget(help_para, tooltip_area);
        }
    }
}

/// Format a step value nicely.
#[allow(clippy::cast_possible_truncation)] // intentional truncation
fn format_step(v: f64) -> String {
    if v.fract().abs() < 1e-9 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
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
        // When cursor is inactive and user tunes with ←→, cursor_freq should
        // shift along with center_freq so the cursor stays at the same visual
        // position on screen ("sticks" to the view, not to an absolute freq).
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        // Cursor starts at center_freq
        let original_center = app.config.center_freq;
        assert!((app.cursor_frequency() - original_center).abs() < 1.0);

        // Deactivate cursor
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(!app.cursor_active);

        // Tune right (-25 kHz to center_freq, spectrum scrolls right)
        app.handle_key(KeyEvent::from(KeyCode::Right));
        let new_center = original_center - 25_000.0;
        assert!((app.config.center_freq - new_center).abs() < 1.0);

        // Cursor should have shifted by the same amount (stays at same screen pos)
        let expected_cursor = original_center - 25_000.0;
        assert!(
            (app.cursor_frequency() - expected_cursor).abs() < 1.0,
            "Cursor should track center_freq when inactive. Expected {}, got {}",
            expected_cursor,
            app.cursor_frequency()
        );

        // Reactivate cursor — it should still be at the shifted position
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(app.cursor_active);
        assert!(
            (app.cursor_frequency() - expected_cursor).abs() < 1.0,
            "Cursor should remain at tracked position after reactivation",
        );
    }

    #[test]
    fn test_cursor_tracks_center_on_left_arrow_inactive() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        let original = app.config.center_freq;

        app.handle_key(KeyEvent::from(KeyCode::Char(' '))); // deactivate
        app.handle_key(KeyEvent::from(KeyCode::Left));      // +25kHz center

        assert!((app.config.center_freq - (original + 25_000.0)).abs() < 1.0);
        assert!(
            (app.cursor_frequency() - (original + 25_000.0)).abs() < 1.0,
            "Cursor should track center on Left when inactive",
        );
    }

    #[test]
    fn test_cursor_tracks_center_on_pageup_inactive() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        let original = app.config.center_freq;

        app.handle_key(KeyEvent::from(KeyCode::Char(' '))); // deactivate
        app.handle_key(KeyEvent::from(KeyCode::PageUp));     // +1MHz

        assert!((app.config.center_freq - (original + 1_000_000.0)).abs() < 1.0);
        assert!(
            (app.cursor_frequency() - (original + 1_000_000.0)).abs() < 1.0,
            "Cursor should track center on PageUp when inactive",
        );
    }

    #[test]
    fn test_cursor_does_not_track_when_active() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        let original_cursor = app.cursor_frequency();

        // Cursor active: Right should move cursor, not center
        app.handle_key(KeyEvent::from(KeyCode::Right));
        assert!(
            (app.cursor_frequency() - original_cursor).abs() > 1.0,
            "Cursor should move when active",
        );
        // center_freq unchanged when cursor active
        assert!((app.config.center_freq - original_cursor).abs() < 1.0);
    }

    #[test]
    fn test_app_channel_bw_increase() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        assert!((app.channel_bw - 8_000.0).abs() < 1.0);
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        assert!((app.channel_bw - 16_000.0).abs() < 1.0);
    }

    #[test]
    fn test_app_channel_bw_decrease() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.handle_key(KeyEvent::from(KeyCode::Char('W')));
        assert!((app.channel_bw - 4_000.0).abs() < 1.0);
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

    // === Waterfall time span hotkey tests ===

    #[test]
    fn test_waterfall_time_span_default() {
        let config = AppConfig::default();
        let app = App::new(config, None);
        assert!((app.waterfall_time_span - 20.0).abs() < 0.01);
        assert_eq!(app.max_waterfall_rows, 600);
    }

    #[test]
    fn test_waterfall_time_span_increase() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.handle_key(KeyEvent::from(KeyCode::Char('t')));
        assert!((app.waterfall_time_span - 25.0).abs() < 0.01);
        assert_eq!(app.max_waterfall_rows, 750);
    }

    #[test]
    fn test_waterfall_time_span_decrease() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.handle_key(KeyEvent::from(KeyCode::Char('T')));
        assert!((app.waterfall_time_span - 15.0).abs() < 0.01);
        assert_eq!(app.max_waterfall_rows, 450);
    }

    #[test]
    fn test_waterfall_time_span_max_cap() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.waterfall_time_span = 120.0;
        app.handle_key(KeyEvent::from(KeyCode::Char('t')));
        assert!((app.waterfall_time_span - 120.0).abs() < 0.01);
    }

    #[test]
    fn test_waterfall_time_span_min_cap() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.waterfall_time_span = 5.0;
        app.handle_key(KeyEvent::from(KeyCode::Char('T')));
        assert!((app.waterfall_time_span - 5.0).abs() < 0.01);
    }

    #[test]
    fn test_waterfall_decrease_truncates_history() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        // Fill with lots of rows
        for _ in 0..700 {
            let fft = FftResult {
                bins: vec![-50.0; 10],
                center_freq: 100e6,
                sample_rate: 2.048e6,
            };
            app.push_fft(fft);
        }
        assert_eq!(app.waterfall_history.len(), 600);
        // Decrease time span
        app.handle_key(KeyEvent::from(KeyCode::Char('T')));
        assert_eq!(app.max_waterfall_rows, 450);
        assert!(app.waterfall_history.len() <= 450);
    }

    // === VFO tests ===

    #[test]
    fn test_vfo_create_with_m_key() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.cursor_freq = 118_100_000.0;
        app.channel_bw = 25_000.0;

        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        assert_eq!(app.vfos.len(), 1);
        assert_eq!(app.vfos[0].id, 1);
        assert!((app.vfos[0].freq - 118_100_000.0).abs() < 1.0);
        assert!((app.vfos[0].channel_bw - 25_000.0).abs() < 1.0);
        assert_eq!(app.vfos[0].color, Color::Red);
        assert_eq!(app.next_vfo_id, 2);
    }

    #[test]
    fn test_vfo_max_8() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        for _ in 0..10 {
            app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        }
        assert_eq!(app.vfos.len(), 8);
    }

    #[test]
    fn test_vfo_color_assignment() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        assert_eq!(app.vfos[0].color, Color::Red);
        assert_eq!(app.vfos[1].color, Color::Green);
    }

    #[test]
    fn test_vfo_delete_modal_opens() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        // x with no VFOs does nothing
        app.handle_key(KeyEvent::from(KeyCode::Char('x')));
        assert!(!app.show_vfo_delete_modal);

        // Create a VFO, then x opens modal
        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        app.handle_key(KeyEvent::from(KeyCode::Char('x')));
        assert!(app.show_vfo_delete_modal);
    }

    #[test]
    fn test_vfo_delete_by_id() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        app.handle_key(KeyEvent::from(KeyCode::Char('m'))); // id=1
        app.handle_key(KeyEvent::from(KeyCode::Char('m'))); // id=2
        assert_eq!(app.vfos.len(), 2);

        app.show_vfo_delete_modal = true;
        app.handle_key(KeyEvent::from(KeyCode::Char('1')));
        assert_eq!(app.vfos.len(), 1);
        assert_eq!(app.vfos[0].id, 2);
        assert!(!app.show_vfo_delete_modal);
    }

    #[test]
    fn test_vfo_delete_modal_esc() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        app.show_vfo_delete_modal = true;
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(!app.show_vfo_delete_modal);
        assert_eq!(app.vfos.len(), 1);
    }

    #[test]
    fn test_vfo_delete_modal_blocks_other_keys() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        app.show_vfo_delete_modal = true;
        app.handle_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(!app.should_quit);
        assert!(app.show_vfo_delete_modal);
    }

    #[test]
    fn test_vfo_clear_command_action() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        assert_eq!(app.vfos.len(), 3);

        app.apply_command_action(CommandAction::ClearVfos);
        assert_eq!(app.vfos.len(), 0);
    }

    #[test]
    fn test_vfo_add_command_action() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        app.apply_command_action(CommandAction::AddVfo(118_100_000.0));
        assert_eq!(app.vfos.len(), 1);
        assert!((app.vfos[0].freq - 118_100_000.0).abs() < 1.0);
    }

    #[test]
    fn test_vfo_remove_command_action() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        app.apply_command_action(CommandAction::AddVfo(118_100_000.0));
        let id = app.vfos[0].id;
        app.apply_command_action(CommandAction::RemoveVfo(id));
        assert_eq!(app.vfos.len(), 0);
    }

    #[test]
    fn test_space_mute_does_not_affect_vfos() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(!app.cursor_active);
        assert_eq!(app.vfos.len(), 1);
    }

    // === Demod mode tests ===

    #[test]
    fn test_d_key_cycles_demod_mode() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        assert_eq!(app.demod_mode, DemodMode::AM);

        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(app.demod_mode, DemodMode::FM);
        assert!((app.channel_bw - 200_000.0).abs() < 1.0);

        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(app.demod_mode, DemodMode::NFM);
        assert!((app.channel_bw - 25_000.0).abs() < 1.0);

        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(app.demod_mode, DemodMode::AM);
        assert!((app.channel_bw - 8_000.0).abs() < 1.0);
    }

    #[test]
    fn test_vfo_inherits_demod_mode() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        // Switch to FM
        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(app.demod_mode, DemodMode::FM);

        // Create VFO — should inherit FM mode
        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        assert_eq!(app.vfos.len(), 1);
        assert_eq!(app.vfos[0].demod_mode, DemodMode::FM);
    }

    #[test]
    fn test_demod_command_action() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);

        app.apply_command_action(CommandAction::SetDemodMode(DemodMode::NFM));
        assert_eq!(app.demod_mode, DemodMode::NFM);
        assert!((app.channel_bw - 25_000.0).abs() < 1.0);

        app.apply_command_action(CommandAction::SetDemodMode(DemodMode::FM));
        assert_eq!(app.demod_mode, DemodMode::FM);
        assert!((app.channel_bw - 200_000.0).abs() < 1.0);
    }

    #[test]
    fn test_d_key_does_not_work_in_help_modal() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.show_help = true;
        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        // Help should close, but demod mode should NOT change
        assert!(!app.show_help);
        assert_eq!(app.demod_mode, DemodMode::AM);
    }

    // === Scan mode tests ===

    #[test]
    fn test_scan_toggle_with_s_key() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        assert!(!app.scan_active);

        app.handle_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
        assert!(app.scan_active);

        app.handle_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
        assert!(!app.scan_active);
    }

    #[test]
    fn test_scan_activates_cursor_if_inactive() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.cursor_active = false;

        app.handle_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
        assert!(app.scan_active);
        assert!(app.cursor_active);
    }

    #[test]
    fn test_maybe_scan_moves_cursor_to_peak() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.scan_active = true;
        app.last_scan_time = Instant::now() - Duration::from_secs(1); // expired

        let mut bins = vec![-100.0_f32; 1024];
        bins[800] = -10.0; // peak at bin 800
        let fft = FftResult {
            bins,
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        app.push_fft(fft.clone());
        app.maybe_scan();

        let expected_freq = fft.bin_frequency(800);
        assert!((app.cursor_freq - expected_freq).abs() < 1.0);
    }

    #[test]
    fn test_maybe_scan_does_not_move_when_inactive() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.scan_active = false;
        let orig = app.cursor_freq;

        let mut bins = vec![-100.0_f32; 1024];
        bins[800] = -10.0;
        let fft = FftResult {
            bins,
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        app.push_fft(fft);
        app.maybe_scan();

        assert!((app.cursor_freq - orig).abs() < 1.0);
    }

    #[test]
    fn test_maybe_scan_respects_squelch() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.scan_active = true;
        app.squelch = Some(-5.0); // squelch at -5 dB
        app.last_scan_time = Instant::now() - Duration::from_secs(1);
        let orig = app.cursor_freq;

        let mut bins = vec![-100.0_f32; 1024];
        bins[800] = -10.0; // peak at -10 dB, below squelch of -5
        let fft = FftResult {
            bins,
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        app.push_fft(fft);
        app.maybe_scan();

        assert!((app.cursor_freq - orig).abs() < 1.0); // didn't move
    }

    #[test]
    fn test_maybe_scan_respects_interval() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.scan_active = true;
        app.scan_interval_ms = 500;
        app.last_scan_time = Instant::now(); // just scanned
        let orig = app.cursor_freq;

        let mut bins = vec![-100.0_f32; 1024];
        bins[800] = -10.0;
        let fft = FftResult {
            bins,
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        app.push_fft(fft);
        app.maybe_scan();

        assert!((app.cursor_freq - orig).abs() < 1.0); // didn't move, too soon
    }

    #[test]
    fn test_manual_cursor_movement_disables_scan() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.scan_active = true;

        app.handle_key(KeyEvent::from(KeyCode::Right));
        assert!(!app.scan_active);
    }

    #[test]
    fn test_manual_cursor_left_disables_scan() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        app.scan_active = true;

        app.handle_key(KeyEvent::from(KeyCode::Left));
        assert!(!app.scan_active);
    }

    #[test]
    fn test_command_palette_scan_toggle() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        assert!(!app.scan_active);

        app.apply_command_action(CommandAction::ToggleScan);
        assert!(app.scan_active);

        app.apply_command_action(CommandAction::ToggleScan);
        assert!(!app.scan_active);
    }

    #[test]
    fn test_command_palette_scan_interval() {
        let config = AppConfig::default();
        let mut app = App::new(config, None);
        assert_eq!(app.scan_interval_ms, 500);

        app.apply_command_action(CommandAction::SetScanInterval(200));
        assert_eq!(app.scan_interval_ms, 200);
    }
}
