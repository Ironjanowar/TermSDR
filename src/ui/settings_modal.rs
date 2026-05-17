//! Settings modal overlay for adjusting runtime parameters.

use crate::audio::AudioControl;
use crate::audio::demod::DemodMode;

/// Unique identifier for each settings item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsItemId {
    Volume,
    Squelch,
    ChannelBw,
    DemodMode,
    AgcEnabled,
    AgcAttack,
    AgcDecay,
    AgcTarget,
    AgcMaxGain,
    BandpassEnabled,
    BandpassLowCut,
    BandpassHighCut,
}

/// The kind of control for a settings item.
#[derive(Debug, Clone)]
pub enum SettingsItemKind {
    Slider {
        value: f64,
        min: f64,
        max: f64,
        fine_step: f64,
        coarse_step: f64,
        unit: &'static str,
    },
    Toggle {
        value: bool,
    },
    Cycle {
        value: usize,
        options: Vec<&'static str>,
    },
}

/// A single item in the settings modal.
#[derive(Debug, Clone)]
pub struct SettingsItem {
    pub label: &'static str,
    pub section: Option<&'static str>,
    pub kind: SettingsItemKind,
    pub id: SettingsItemId,
    pub help_text: &'static str,
    pub default_kind: DefaultKind,
}

/// Stores the default value for resetting.
#[derive(Debug, Clone)]
pub enum DefaultKind {
    Slider(f64),
    Toggle(bool),
    Cycle(usize),
}

/// The settings modal state.
#[derive(Debug)]
pub struct SettingsModal {
    pub visible: bool,
    pub selected_index: usize,
    pub editing: bool,
    pub edit_buffer: String,
    pub items: Vec<SettingsItem>,
    pub showing_help: bool,
}

impl Default for SettingsModal {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsModal {
    /// Create a new settings modal with default items.
    #[must_use] 
    pub fn new() -> Self {
        Self {
            visible: false,
            selected_index: 0,
            editing: false,
            edit_buffer: String::new(),
            items: Self::default_items(),
            showing_help: false,
        }
    }

    #[allow(clippy::too_many_lines)] // declarative settings list
    fn default_items() -> Vec<SettingsItem> {
        vec![
            SettingsItem {
                label: "Volume",
                section: None,
                kind: SettingsItemKind::Slider {
                    value: 80.0, min: 0.0, max: 100.0,
                    fine_step: 1.0, coarse_step: 10.0, unit: "%",
                },
                id: SettingsItemId::Volume,
                help_text: "Master audio output volume. Affects all demodulated audio.",
                default_kind: DefaultKind::Slider(80.0),
            },
            SettingsItem {
                label: "Squelch",
                section: None,
                kind: SettingsItemKind::Slider {
                    value: -120.0, min: -120.0, max: 0.0,
                    fine_step: 1.0, coarse_step: 10.0, unit: "dB",
                },
                id: SettingsItemId::Squelch,
                help_text: "Mutes audio when signal power is below this threshold. Set to a level just above the noise floor to silence static between transmissions.",
                default_kind: DefaultKind::Slider(-120.0),
            },
            SettingsItem {
                label: "Channel BW",
                section: None,
                kind: SettingsItemKind::Slider {
                    value: 8000.0, min: 1000.0, max: 250_000.0,
                    fine_step: 500.0, coarse_step: 5000.0, unit: "Hz",
                },
                id: SettingsItemId::ChannelBw,
                help_text: "RF channel bandwidth. Narrower = less noise but may cut signal. For AM aviation: 6-8 kHz. For FM broadcast: 200 kHz.",
                default_kind: DefaultKind::Slider(8000.0),
            },
            SettingsItem {
                label: "Demod Mode",
                section: None,
                kind: SettingsItemKind::Cycle {
                    value: 0,
                    options: vec!["AM", "FM", "NFM"],
                },
                id: SettingsItemId::DemodMode,
                help_text: "AM: amplitude modulation (aviation, HF). FM: wideband FM (broadcast radio). NFM: narrowband FM (walkie-talkies, PMR446).",
                default_kind: DefaultKind::Cycle(0),
            },
            // AGC section
            SettingsItem {
                label: "AGC Enabled",
                section: Some("AGC"),
                kind: SettingsItemKind::Toggle { value: true },
                id: SettingsItemId::AgcEnabled,
                help_text: "Automatic Gain Control normalizes volume so weak and strong signals sound similar. Essential for aviation monitoring.",
                default_kind: DefaultKind::Toggle(true),
            },
            SettingsItem {
                label: "Attack",
                section: Some("AGC"),
                kind: SettingsItemKind::Slider {
                    value: 10.0, min: 1.0, max: 100.0,
                    fine_step: 1.0, coarse_step: 10.0, unit: "ms",
                },
                id: SettingsItemId::AgcAttack,
                help_text: "How fast AGC reduces gain when a loud signal arrives. Lower = faster response, prevents clipping.",
                default_kind: DefaultKind::Slider(10.0),
            },
            SettingsItem {
                label: "Decay",
                section: Some("AGC"),
                kind: SettingsItemKind::Slider {
                    value: 300.0, min: 50.0, max: 2000.0,
                    fine_step: 10.0, coarse_step: 100.0, unit: "ms",
                },
                id: SettingsItemId::AgcDecay,
                help_text: "How fast AGC increases gain when signal gets weaker. Higher = slower, avoids amplifying noise between words.",
                default_kind: DefaultKind::Slider(300.0),
            },
            SettingsItem {
                label: "Target Level",
                section: Some("AGC"),
                kind: SettingsItemKind::Slider {
                    value: 0.3, min: 0.05, max: 1.0,
                    fine_step: 0.05, coarse_step: 0.1, unit: "",
                },
                id: SettingsItemId::AgcTarget,
                help_text: "Desired RMS output level. Higher = louder normalized output.",
                default_kind: DefaultKind::Slider(0.3),
            },
            SettingsItem {
                label: "Max Gain",
                section: Some("AGC"),
                kind: SettingsItemKind::Slider {
                    value: 50.0, min: 1.0, max: 100.0,
                    fine_step: 1.0, coarse_step: 10.0, unit: "dB",
                },
                id: SettingsItemId::AgcMaxGain,
                help_text: "Maximum amplification AGC can apply. Limits how much background noise gets amplified during silence.",
                default_kind: DefaultKind::Slider(50.0),
            },
            // Audio Filter section
            SettingsItem {
                label: "Bandpass",
                section: Some("Audio Filter"),
                kind: SettingsItemKind::Toggle { value: false },
                id: SettingsItemId::BandpassEnabled,
                help_text: "Voice bandpass filter removes frequencies outside the speech range, reducing noise and improving clarity.",
                default_kind: DefaultKind::Toggle(false),
            },
            SettingsItem {
                label: "Low Cut",
                section: Some("Audio Filter"),
                kind: SettingsItemKind::Slider {
                    value: 300.0, min: 50.0, max: 1000.0,
                    fine_step: 50.0, coarse_step: 100.0, unit: "Hz",
                },
                id: SettingsItemId::BandpassLowCut,
                help_text: "High-pass cutoff frequency. Removes low-frequency rumble and hum below this frequency. Standard voice: 300 Hz.",
                default_kind: DefaultKind::Slider(300.0),
            },
            SettingsItem {
                label: "High Cut",
                section: Some("Audio Filter"),
                kind: SettingsItemKind::Slider {
                    value: 3000.0, min: 1000.0, max: 8000.0,
                    fine_step: 100.0, coarse_step: 500.0, unit: "Hz",
                },
                id: SettingsItemId::BandpassHighCut,
                help_text: "Low-pass cutoff frequency. Removes high-frequency hiss and noise above this frequency. Standard voice: 3400 Hz.",
                default_kind: DefaultKind::Slider(3000.0),
            },
        ]
    }

    /// Open the modal and sync values from app state.
    pub fn open(&mut self, volume: u8, squelch: Option<f32>, channel_bw: f64, demod_mode: DemodMode) {
        self.visible = true;
        self.selected_index = 0;
        self.editing = false;
        self.edit_buffer.clear();

        for item in &mut self.items {
            match item.id {
                SettingsItemId::Volume => {
                    if let SettingsItemKind::Slider { ref mut value, .. } = item.kind {
                        *value = f64::from(volume);
                    }
                }
                SettingsItemId::Squelch => {
                    if let SettingsItemKind::Slider { ref mut value, .. } = item.kind {
                        *value = f64::from(squelch.unwrap_or(-120.0));
                    }
                }
                SettingsItemId::ChannelBw => {
                    if let SettingsItemKind::Slider { ref mut value, .. } = item.kind {
                        *value = channel_bw;
                    }
                }
                SettingsItemId::DemodMode => {
                    if let SettingsItemKind::Cycle { ref mut value, .. } = item.kind {
                        *value = match demod_mode {
                            DemodMode::AM => 0,
                            DemodMode::FM => 1,
                            DemodMode::NFM => 2,
                        };
                    }
                }
                _ => {} // AGC/bandpass values stay at defaults (we don't track them in App yet)
            }
        }
    }

    /// Close the modal.
    pub fn close(&mut self) {
        self.visible = false;
        self.editing = false;
        self.showing_help = false;
        self.edit_buffer.clear();
    }

    /// Toggle help tooltip for the selected item.
    pub fn toggle_help(&mut self) {
        self.showing_help = !self.showing_help;
    }

    /// Reset the selected item to its default value. Returns the `AudioControl` if changed.
    pub fn reset_to_default(&mut self) -> Option<AudioControl> {
        if self.items.is_empty() { return None; }
        let item = &mut self.items[self.selected_index];
        match (&mut item.kind, &item.default_kind) {
            (SettingsItemKind::Slider { value, .. }, DefaultKind::Slider(def)) => {
                *value = *def;
            }
            (SettingsItemKind::Toggle { value }, DefaultKind::Toggle(def)) => {
                *value = *def;
            }
            (SettingsItemKind::Cycle { value, .. }, DefaultKind::Cycle(def)) => {
                *value = *def;
            }
            _ => {}
        }
        self.audio_control_for(self.selected_index)
    }

    /// Move selection up (wraps).
    pub fn select_up(&mut self) {
        if self.items.is_empty() { return; }
        if self.selected_index == 0 {
            self.selected_index = self.items.len() - 1;
        } else {
            self.selected_index -= 1;
        }
    }

    /// Move selection down (wraps).
    pub fn select_down(&mut self) {
        if self.items.is_empty() { return; }
        self.selected_index = (self.selected_index + 1) % self.items.len();
    }

    /// Adjust the selected item by the fine step in the given direction.
    pub fn adjust_fine(&mut self, direction: i8) -> Option<AudioControl> {
        self.adjust(direction, false)
    }

    /// Adjust the selected item by the coarse step in the given direction.
    pub fn adjust_coarse(&mut self, direction: i8) -> Option<AudioControl> {
        self.adjust(direction, true)
    }

    fn adjust(&mut self, direction: i8, coarse: bool) -> Option<AudioControl> {
        if self.items.is_empty() { return None; }
        let item = &mut self.items[self.selected_index];
        match &mut item.kind {
            SettingsItemKind::Slider { value, min, max, fine_step, coarse_step, .. } => {
                let step = if coarse { *coarse_step } else { *fine_step };
                *value = (*value + step * f64::from(direction)).clamp(*min, *max);
            }
            SettingsItemKind::Toggle { value } => {
                *value = !*value;
            }
            SettingsItemKind::Cycle { value, options } => {
                if !options.is_empty() {
                    if direction > 0 {
                        *value = (*value + 1) % options.len();
                    } else {
                        *value = if *value == 0 { options.len() - 1 } else { *value - 1 };
                    }
                }
            }
        }
        self.audio_control_for(self.selected_index)
    }

    /// Handle Enter on the selected item.
    /// For toggle/cycle: toggle/cycle and return the audio control.
    /// For slider: enter edit mode.
    pub fn activate(&mut self) -> Option<AudioControl> {
        if self.items.is_empty() { return None; }
        let item = &mut self.items[self.selected_index];
        match &mut item.kind {
            SettingsItemKind::Toggle { value } => {
                *value = !*value;
                self.audio_control_for(self.selected_index)
            }
            SettingsItemKind::Cycle { value, options } => {
                if !options.is_empty() {
                    *value = (*value + 1) % options.len();
                }
                self.audio_control_for(self.selected_index)
            }
            SettingsItemKind::Slider { value, .. } => {
                self.editing = true;
                self.edit_buffer = format_value(*value);
                None
            }
        }
    }

    /// Confirm the edit buffer value.
    pub fn confirm_edit(&mut self) -> Option<AudioControl> {
        if !self.editing { return None; }
        self.editing = false;
        if let Ok(v) = self.edit_buffer.parse::<f64>() {
            let item = &mut self.items[self.selected_index];
            if let SettingsItemKind::Slider { value, min, max, .. } = &mut item.kind {
                *value = v.clamp(*min, *max);
            }
            return self.audio_control_for(self.selected_index);
        }
        None
    }

    /// Cancel edit mode.
    pub fn cancel_edit(&mut self) {
        self.editing = false;
        self.edit_buffer.clear();
    }

    /// Type a character into the edit buffer.
    pub fn edit_type(&mut self, c: char) {
        if c.is_ascii_digit() || c == '.' || c == '-' {
            self.edit_buffer.push(c);
        }
    }

    /// Backspace in edit buffer.
    pub fn edit_backspace(&mut self) {
        self.edit_buffer.pop();
    }

    /// Get the current selected item (if any).
    #[must_use] 
    pub fn selected_item(&self) -> Option<&SettingsItem> {
        self.items.get(self.selected_index)
    }

    /// Build the `AudioControl` message for a given item index.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // intentional truncation; value is non-negative here
    fn audio_control_for(&self, index: usize) -> Option<AudioControl> {
        let item = &self.items[index];
        match item.id {
            SettingsItemId::Volume => {
                if let SettingsItemKind::Slider { value, .. } = &item.kind {
                    Some(AudioControl::SetVolume(*value as u8))
                } else { None }
            }
            SettingsItemId::Squelch => {
                if let SettingsItemKind::Slider { value, .. } = &item.kind {
                    #[allow(clippy::cast_possible_truncation)] // intentional truncation
                    let sq = if *value <= -120.0 { None } else { Some(*value as f32) };
                    Some(AudioControl::SetSquelch(sq))
                } else { None }
            }
            SettingsItemId::ChannelBw => {
                if let SettingsItemKind::Slider { value, .. } = &item.kind {
                    Some(AudioControl::SetChannelBw(*value))
                } else { None }
            }
            SettingsItemId::DemodMode => {
                if let SettingsItemKind::Cycle { value, .. } = &item.kind {
                    let mode = match value {
                        0 => DemodMode::AM,
                        1 => DemodMode::FM,
                        _ => DemodMode::NFM,
                    };
                    Some(AudioControl::SetDemodMode(mode))
                } else { None }
            }
            SettingsItemId::AgcEnabled => {
                if let SettingsItemKind::Toggle { value } = &item.kind {
                    Some(AudioControl::SetAgcEnabled(*value))
                } else { None }
            }
            SettingsItemId::AgcAttack => {
                if let SettingsItemKind::Slider { value, .. } = &item.kind {
                    Some(AudioControl::SetAgcAttack(*value))
                } else { None }
            }
            SettingsItemId::AgcDecay => {
                if let SettingsItemKind::Slider { value, .. } = &item.kind {
                    Some(AudioControl::SetAgcDecay(*value))
                } else { None }
            }
            SettingsItemId::AgcTarget => {
                if let SettingsItemKind::Slider { value, .. } = &item.kind {
                    Some(AudioControl::SetAgcTarget(*value as f32))
                } else { None }
            }
            SettingsItemId::AgcMaxGain => {
                if let SettingsItemKind::Slider { value, .. } = &item.kind {
                    Some(AudioControl::SetAgcMaxGain(*value as f32))
                } else { None }
            }
            SettingsItemId::BandpassEnabled => {
                if let SettingsItemKind::Toggle { value } = &item.kind {
                    Some(AudioControl::SetBandpassEnabled(*value))
                } else { None }
            }
            SettingsItemId::BandpassLowCut => {
                if let SettingsItemKind::Slider { value, .. } = &item.kind {
                    Some(AudioControl::SetBandpassLowCut(*value))
                } else { None }
            }
            SettingsItemId::BandpassHighCut => {
                if let SettingsItemKind::Slider { value, .. } = &item.kind {
                    Some(AudioControl::SetBandpassHighCut(*value))
                } else { None }
            }
        }
    }

    /// Get the list of sections with their row ranges for rendering.
    #[must_use] 
    pub fn section_headers(&self) -> Vec<(usize, &'static str)> {
        let mut headers = Vec::new();
        let mut last_section: Option<Option<&str>> = None;
        for (i, item) in self.items.iter().enumerate() {
            let current = item.section;
            if last_section.as_ref() != Some(&current) {
                if let Some(s) = current {
                    headers.push((i, s));
                }
                last_section = Some(current);
            }
        }
        headers
    }
}

/// Format a float value nicely (strip trailing zeros).
#[allow(clippy::cast_possible_truncation)] // intentional truncation
fn format_value(v: f64) -> String {
    if v.fract().abs() < 1e-9 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Format a value with its unit for display.
#[must_use] 
pub fn format_display_value(kind: &SettingsItemKind) -> String {
    match kind {
        SettingsItemKind::Slider { value, unit, .. } => {
            if *unit == "Hz" && *value >= 1000.0 {
                format!("{:.1} kHz", value / 1000.0)
            } else {
                format!("{} {}", format_value(*value), unit)
            }
        }
        SettingsItemKind::Toggle { value } => {
            if *value { "[ON]".to_string() } else { "[OFF]".to_string() }
        }
        SettingsItemKind::Cycle { value, options } => {
            options.get(*value).unwrap_or(&"?").to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_has_correct_item_count() {
        let modal = SettingsModal::new();
        assert_eq!(modal.items.len(), 12);
        assert!(!modal.visible);
        assert_eq!(modal.selected_index, 0);
    }

    #[test]
    fn test_navigation_down_wraps() {
        let mut modal = SettingsModal::new();
        assert_eq!(modal.selected_index, 0);
        // Go to last item
        for _ in 0..modal.items.len() {
            modal.select_down();
        }
        assert_eq!(modal.selected_index, 0); // wrapped
    }

    #[test]
    fn test_navigation_up_wraps() {
        let mut modal = SettingsModal::new();
        assert_eq!(modal.selected_index, 0);
        modal.select_up();
        assert_eq!(modal.selected_index, modal.items.len() - 1);
    }

    #[test]
    fn test_navigation_down_then_up() {
        let mut modal = SettingsModal::new();
        modal.select_down();
        modal.select_down();
        assert_eq!(modal.selected_index, 2);
        modal.select_up();
        assert_eq!(modal.selected_index, 1);
    }

    #[test]
    fn test_fine_adjust_slider_increases() {
        let mut modal = SettingsModal::new();
        // Volume is item 0, default 80
        let ctrl = modal.adjust_fine(1);
        if let SettingsItemKind::Slider { value, .. } = &modal.items[0].kind {
            assert!((*value - 81.0).abs() < 0.01);
        } else {
            panic!("Expected slider");
        }
        assert!(matches!(ctrl, Some(AudioControl::SetVolume(81))));
    }

    #[test]
    fn test_fine_adjust_slider_decreases() {
        let mut modal = SettingsModal::new();
        let ctrl = modal.adjust_fine(-1);
        if let SettingsItemKind::Slider { value, .. } = &modal.items[0].kind {
            assert!((*value - 79.0).abs() < 0.01);
        } else {
            panic!("Expected slider");
        }
        assert!(matches!(ctrl, Some(AudioControl::SetVolume(79))));
    }

    #[test]
    fn test_coarse_adjust_slider() {
        let mut modal = SettingsModal::new();
        let ctrl = modal.adjust_coarse(1);
        if let SettingsItemKind::Slider { value, .. } = &modal.items[0].kind {
            assert!((*value - 90.0).abs() < 0.01);
        } else {
            panic!("Expected slider");
        }
        assert!(matches!(ctrl, Some(AudioControl::SetVolume(90))));
    }

    #[test]
    fn test_slider_clamps_at_max() {
        let mut modal = SettingsModal::new();
        // Set volume to 95, coarse +10 should clamp at 100
        if let SettingsItemKind::Slider { ref mut value, .. } = modal.items[0].kind {
            *value = 95.0;
        }
        modal.adjust_coarse(1);
        if let SettingsItemKind::Slider { value, .. } = &modal.items[0].kind {
            assert!((*value - 100.0).abs() < 0.01);
        }
    }

    #[test]
    fn test_slider_clamps_at_min() {
        let mut modal = SettingsModal::new();
        if let SettingsItemKind::Slider { ref mut value, .. } = modal.items[0].kind {
            *value = 5.0;
        }
        modal.adjust_coarse(-1);
        if let SettingsItemKind::Slider { value, .. } = &modal.items[0].kind {
            assert!((*value - 0.0).abs() < 0.01);
        }
    }

    #[test]
    fn test_toggle_behavior() {
        let mut modal = SettingsModal::new();
        // AGC Enabled is item 4
        modal.selected_index = 4;
        assert!(matches!(modal.items[4].kind, SettingsItemKind::Toggle { value: true }));

        let ctrl = modal.adjust_fine(1); // direction doesn't matter for toggle
        assert!(matches!(modal.items[4].kind, SettingsItemKind::Toggle { value: false }));
        assert!(matches!(ctrl, Some(AudioControl::SetAgcEnabled(false))));

        modal.adjust_fine(-1);
        assert!(matches!(modal.items[4].kind, SettingsItemKind::Toggle { value: true }));
    }

    #[test]
    fn test_cycle_behavior() {
        let mut modal = SettingsModal::new();
        // DemodMode is item 3
        modal.selected_index = 3;
        assert!(matches!(modal.items[3].kind, SettingsItemKind::Cycle { value: 0, .. }));

        let ctrl = modal.adjust_fine(1);
        assert!(matches!(modal.items[3].kind, SettingsItemKind::Cycle { value: 1, .. }));
        assert!(matches!(ctrl, Some(AudioControl::SetDemodMode(DemodMode::FM))));

        modal.adjust_fine(1);
        assert!(matches!(modal.items[3].kind, SettingsItemKind::Cycle { value: 2, .. }));

        // Wrap around
        modal.adjust_fine(1);
        assert!(matches!(modal.items[3].kind, SettingsItemKind::Cycle { value: 0, .. }));
    }

    #[test]
    fn test_cycle_backward() {
        let mut modal = SettingsModal::new();
        modal.selected_index = 3;
        // At 0, going backward should wrap to last
        modal.adjust_fine(-1);
        assert!(matches!(modal.items[3].kind, SettingsItemKind::Cycle { value: 2, .. }));
    }

    #[test]
    fn test_edit_mode_entry_for_slider() {
        let mut modal = SettingsModal::new();
        // Volume slider
        let ctrl = modal.activate();
        assert!(modal.editing);
        assert_eq!(modal.edit_buffer, "80");
        assert!(ctrl.is_none()); // No control sent on edit entry
    }

    #[test]
    fn test_edit_mode_confirm() {
        let mut modal = SettingsModal::new();
        modal.activate(); // enter edit mode
        modal.edit_buffer = "42".to_string();
        let ctrl = modal.confirm_edit();
        assert!(!modal.editing);
        if let SettingsItemKind::Slider { value, .. } = &modal.items[0].kind {
            assert!((*value - 42.0).abs() < 0.01);
        }
        assert!(matches!(ctrl, Some(AudioControl::SetVolume(42))));
    }

    #[test]
    fn test_edit_mode_confirm_clamps() {
        let mut modal = SettingsModal::new();
        modal.activate();
        modal.edit_buffer = "999".to_string();
        modal.confirm_edit();
        if let SettingsItemKind::Slider { value, .. } = &modal.items[0].kind {
            assert!((*value - 100.0).abs() < 0.01);
        }
    }

    #[test]
    fn test_edit_mode_cancel() {
        let mut modal = SettingsModal::new();
        modal.activate();
        modal.edit_buffer = "999".to_string();
        modal.cancel_edit();
        assert!(!modal.editing);
        // Value should remain unchanged
        if let SettingsItemKind::Slider { value, .. } = &modal.items[0].kind {
            assert!((*value - 80.0).abs() < 0.01);
        }
    }

    #[test]
    fn test_edit_type_filters_characters() {
        let mut modal = SettingsModal::new();
        modal.editing = true;
        modal.edit_buffer.clear();
        modal.edit_type('1');
        modal.edit_type('a'); // rejected
        modal.edit_type('.');
        modal.edit_type('5');
        modal.edit_type('-');
        modal.edit_type('z'); // rejected
        assert_eq!(modal.edit_buffer, "1.5-");
    }

    #[test]
    fn test_edit_backspace() {
        let mut modal = SettingsModal::new();
        modal.editing = true;
        modal.edit_buffer = "123".to_string();
        modal.edit_backspace();
        assert_eq!(modal.edit_buffer, "12");
    }

    #[test]
    fn test_esc_closes_modal() {
        let mut modal = SettingsModal::new();
        modal.visible = true;
        modal.close();
        assert!(!modal.visible);
    }

    #[test]
    fn test_open_syncs_values() {
        let mut modal = SettingsModal::new();
        modal.open(50, Some(-30.0), 12500.0, DemodMode::FM);
        assert!(modal.visible);

        if let SettingsItemKind::Slider { value, .. } = &modal.items[0].kind {
            assert!((*value - 50.0).abs() < 0.01);
        }
        if let SettingsItemKind::Slider { value, .. } = &modal.items[1].kind {
            assert!((*value - (-30.0)).abs() < 0.01);
        }
        if let SettingsItemKind::Slider { value, .. } = &modal.items[2].kind {
            assert!((*value - 12500.0).abs() < 0.01);
        }
        if let SettingsItemKind::Cycle { value, .. } = &modal.items[3].kind {
            assert_eq!(*value, 1); // FM
        }
    }

    #[test]
    fn test_open_squelch_none() {
        let mut modal = SettingsModal::new();
        modal.open(80, None, 25000.0, DemodMode::AM);
        if let SettingsItemKind::Slider { value, .. } = &modal.items[1].kind {
            assert!((*value - (-120.0)).abs() < 0.01);
        }
    }

    #[test]
    fn test_activate_toggle() {
        let mut modal = SettingsModal::new();
        modal.selected_index = 4; // AGC enabled
        let ctrl = modal.activate();
        assert!(matches!(modal.items[4].kind, SettingsItemKind::Toggle { value: false }));
        assert!(matches!(ctrl, Some(AudioControl::SetAgcEnabled(false))));
        assert!(!modal.editing); // toggle doesn't enter edit mode
    }

    #[test]
    fn test_activate_cycle() {
        let mut modal = SettingsModal::new();
        modal.selected_index = 3; // Demod mode
        let ctrl = modal.activate();
        assert!(matches!(modal.items[3].kind, SettingsItemKind::Cycle { value: 1, .. }));
        assert!(matches!(ctrl, Some(AudioControl::SetDemodMode(DemodMode::FM))));
        assert!(!modal.editing);
    }

    #[test]
    fn test_section_headers() {
        let modal = SettingsModal::new();
        let headers = modal.section_headers();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].1, "AGC");
        assert_eq!(headers[1].1, "Audio Filter");
    }

    #[test]
    fn test_format_display_value_slider() {
        let kind = SettingsItemKind::Slider {
            value: 80.0, min: 0.0, max: 100.0,
            fine_step: 1.0, coarse_step: 10.0, unit: "%",
        };
        assert_eq!(format_display_value(&kind), "80 %");
    }

    #[test]
    fn test_format_display_value_hz_to_khz() {
        let kind = SettingsItemKind::Slider {
            value: 3000.0, min: 1000.0, max: 8000.0,
            fine_step: 100.0, coarse_step: 500.0, unit: "Hz",
        };
        assert_eq!(format_display_value(&kind), "3.0 kHz");
    }

    #[test]
    fn test_format_display_value_toggle() {
        assert_eq!(format_display_value(&SettingsItemKind::Toggle { value: true }), "[ON]");
        assert_eq!(format_display_value(&SettingsItemKind::Toggle { value: false }), "[OFF]");
    }

    #[test]
    fn test_format_display_value_cycle() {
        let kind = SettingsItemKind::Cycle {
            value: 1,
            options: vec!["AM", "FM", "NFM"],
        };
        assert_eq!(format_display_value(&kind), "FM");
    }

    #[test]
    fn test_squelch_audio_control_disabled() {
        let mut modal = SettingsModal::new();
        modal.selected_index = 1; // Squelch
        // Set to -120 (disabled)
        if let SettingsItemKind::Slider { ref mut value, .. } = modal.items[1].kind {
            *value = -120.0;
        }
        let ctrl = modal.audio_control_for(1);
        assert!(matches!(ctrl, Some(AudioControl::SetSquelch(None))));
    }

    #[test]
    fn test_squelch_audio_control_enabled() {
        let mut modal = SettingsModal::new();
        if let SettingsItemKind::Slider { ref mut value, .. } = modal.items[1].kind {
            *value = -50.0;
        }
        let ctrl = modal.audio_control_for(1);
        match ctrl {
            Some(AudioControl::SetSquelch(Some(v))) => assert!((v - (-50.0)).abs() < 0.01),
            _ => panic!("Expected SetSquelch(Some(-50))"),
        }
    }

    #[test]
    fn test_all_items_produce_audio_controls() {
        let modal = SettingsModal::new();
        for (i, item) in modal.items.iter().enumerate() {
            let ctrl = modal.audio_control_for(i);
            assert!(ctrl.is_some(), "Item {:?} should produce an AudioControl", item.id);
        }
    }

    #[test]
    fn test_edit_invalid_input_ignored() {
        let mut modal = SettingsModal::new();
        modal.activate(); // enter edit mode for volume
        modal.edit_buffer = "abc".to_string();
        let ctrl = modal.confirm_edit();
        assert!(ctrl.is_none());
        // Value should be unchanged
        if let SettingsItemKind::Slider { value, .. } = &modal.items[0].kind {
            assert!((*value - 80.0).abs() < 0.01);
        }
    }

    #[test]
    fn test_confirm_edit_when_not_editing() {
        let mut modal = SettingsModal::new();
        let ctrl = modal.confirm_edit();
        assert!(ctrl.is_none());
    }

    #[test]
    fn test_bandpass_controls() {
        let mut modal = SettingsModal::new();
        // BandpassLowCut is item 10
        modal.selected_index = 10;
        let ctrl = modal.adjust_fine(1);
        match ctrl {
            Some(AudioControl::SetBandpassLowCut(v)) => assert!((v - 350.0).abs() < 0.01),
            _ => panic!("Expected SetBandpassLowCut"),
        }

        // BandpassHighCut is item 11
        modal.selected_index = 11;
        let ctrl = modal.adjust_fine(-1);
        match ctrl {
            Some(AudioControl::SetBandpassHighCut(v)) => assert!((v - 2900.0).abs() < 0.01),
            _ => panic!("Expected SetBandpassHighCut"),
        }
    }

    #[test]
    fn test_help_toggle_on_off() {
        let mut modal = SettingsModal::new();
        modal.visible = true;
        assert!(!modal.showing_help);
        modal.toggle_help();
        assert!(modal.showing_help);
        modal.toggle_help();
        assert!(!modal.showing_help);
    }

    #[test]
    fn test_help_dismissed_on_close() {
        let mut modal = SettingsModal::new();
        modal.visible = true;
        modal.showing_help = true;
        modal.close();
        assert!(!modal.showing_help);
    }

    #[test]
    fn test_reset_slider_to_default() {
        let mut modal = SettingsModal::new();
        // Change volume from 80 to 42
        if let SettingsItemKind::Slider { ref mut value, .. } = modal.items[0].kind {
            *value = 42.0;
        }
        let ctrl = modal.reset_to_default();
        if let SettingsItemKind::Slider { value, .. } = &modal.items[0].kind {
            assert!((*value - 80.0).abs() < 0.01);
        } else {
            panic!("Expected slider");
        }
        assert!(matches!(ctrl, Some(AudioControl::SetVolume(80))));
    }

    #[test]
    fn test_reset_toggle_to_default() {
        let mut modal = SettingsModal::new();
        modal.selected_index = 4; // AGC Enabled, default true
        if let SettingsItemKind::Toggle { ref mut value } = modal.items[4].kind {
            *value = false;
        }
        let ctrl = modal.reset_to_default();
        assert!(matches!(modal.items[4].kind, SettingsItemKind::Toggle { value: true }));
        assert!(matches!(ctrl, Some(AudioControl::SetAgcEnabled(true))));
    }

    #[test]
    fn test_reset_cycle_to_default() {
        let mut modal = SettingsModal::new();
        modal.selected_index = 3; // DemodMode, default 0 (AM)
        if let SettingsItemKind::Cycle { ref mut value, .. } = modal.items[3].kind {
            *value = 2; // NFM
        }
        let ctrl = modal.reset_to_default();
        assert!(matches!(modal.items[3].kind, SettingsItemKind::Cycle { value: 0, .. }));
        assert!(matches!(ctrl, Some(AudioControl::SetDemodMode(DemodMode::AM))));
    }

    #[test]
    fn test_all_items_have_help_text() {
        let modal = SettingsModal::new();
        for item in &modal.items {
            assert!(!item.help_text.is_empty(), "Item {:?} should have help text", item.id);
        }
    }

    #[test]
    fn test_default_values_match_initial() {
        let modal = SettingsModal::new();
        for item in &modal.items {
            match (&item.kind, &item.default_kind) {
                (SettingsItemKind::Slider { value, .. }, DefaultKind::Slider(def)) => {
                    assert!((*value - *def).abs() < 0.01, "Item {:?} default mismatch", item.id);
                }
                (SettingsItemKind::Toggle { value }, DefaultKind::Toggle(def)) => {
                    assert_eq!(*value, *def, "Item {:?} default mismatch", item.id);
                }
                (SettingsItemKind::Cycle { value, .. }, DefaultKind::Cycle(def)) => {
                    assert_eq!(*value, *def, "Item {:?} default mismatch", item.id);
                }
                _ => panic!("Item {:?} kind/default mismatch", item.id),
            }
        }
    }
}
