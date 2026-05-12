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
}

/// The settings modal state.
#[derive(Debug)]
pub struct SettingsModal {
    pub visible: bool,
    pub selected_index: usize,
    pub editing: bool,
    pub edit_buffer: String,
    pub items: Vec<SettingsItem>,
}

impl Default for SettingsModal {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsModal {
    /// Create a new settings modal with default items.
    pub fn new() -> Self {
        Self {
            visible: false,
            selected_index: 0,
            editing: false,
            edit_buffer: String::new(),
            items: Self::default_items(),
        }
    }

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
            },
            SettingsItem {
                label: "Squelch",
                section: None,
                kind: SettingsItemKind::Slider {
                    value: -120.0, min: -120.0, max: 0.0,
                    fine_step: 1.0, coarse_step: 10.0, unit: "dB",
                },
                id: SettingsItemId::Squelch,
            },
            SettingsItem {
                label: "Channel BW",
                section: None,
                kind: SettingsItemKind::Slider {
                    value: 8000.0, min: 1000.0, max: 250000.0,
                    fine_step: 500.0, coarse_step: 5000.0, unit: "Hz",
                },
                id: SettingsItemId::ChannelBw,
            },
            SettingsItem {
                label: "Demod Mode",
                section: None,
                kind: SettingsItemKind::Cycle {
                    value: 0,
                    options: vec!["AM", "FM", "NFM"],
                },
                id: SettingsItemId::DemodMode,
            },
            // AGC section
            SettingsItem {
                label: "AGC Enabled",
                section: Some("AGC"),
                kind: SettingsItemKind::Toggle { value: true },
                id: SettingsItemId::AgcEnabled,
            },
            SettingsItem {
                label: "Attack",
                section: Some("AGC"),
                kind: SettingsItemKind::Slider {
                    value: 10.0, min: 1.0, max: 100.0,
                    fine_step: 1.0, coarse_step: 10.0, unit: "ms",
                },
                id: SettingsItemId::AgcAttack,
            },
            SettingsItem {
                label: "Decay",
                section: Some("AGC"),
                kind: SettingsItemKind::Slider {
                    value: 300.0, min: 50.0, max: 2000.0,
                    fine_step: 10.0, coarse_step: 100.0, unit: "ms",
                },
                id: SettingsItemId::AgcDecay,
            },
            SettingsItem {
                label: "Target Level",
                section: Some("AGC"),
                kind: SettingsItemKind::Slider {
                    value: 0.3, min: 0.05, max: 1.0,
                    fine_step: 0.05, coarse_step: 0.1, unit: "",
                },
                id: SettingsItemId::AgcTarget,
            },
            SettingsItem {
                label: "Max Gain",
                section: Some("AGC"),
                kind: SettingsItemKind::Slider {
                    value: 50.0, min: 1.0, max: 100.0,
                    fine_step: 1.0, coarse_step: 10.0, unit: "dB",
                },
                id: SettingsItemId::AgcMaxGain,
            },
            // Audio Filter section
            SettingsItem {
                label: "Bandpass",
                section: Some("Audio Filter"),
                kind: SettingsItemKind::Toggle { value: false },
                id: SettingsItemId::BandpassEnabled,
            },
            SettingsItem {
                label: "Low Cut",
                section: Some("Audio Filter"),
                kind: SettingsItemKind::Slider {
                    value: 300.0, min: 50.0, max: 1000.0,
                    fine_step: 50.0, coarse_step: 100.0, unit: "Hz",
                },
                id: SettingsItemId::BandpassLowCut,
            },
            SettingsItem {
                label: "High Cut",
                section: Some("Audio Filter"),
                kind: SettingsItemKind::Slider {
                    value: 3000.0, min: 1000.0, max: 8000.0,
                    fine_step: 100.0, coarse_step: 500.0, unit: "Hz",
                },
                id: SettingsItemId::BandpassHighCut,
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
                        *value = volume as f64;
                    }
                }
                SettingsItemId::Squelch => {
                    if let SettingsItemKind::Slider { ref mut value, .. } = item.kind {
                        *value = squelch.unwrap_or(-120.0) as f64;
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
        self.edit_buffer.clear();
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
                *value = (*value + step * direction as f64).clamp(*min, *max);
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
    pub fn selected_item(&self) -> Option<&SettingsItem> {
        self.items.get(self.selected_index)
    }

    /// Build the AudioControl message for a given item index.
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
fn format_value(v: f64) -> String {
    if v.fract().abs() < 1e-9 {
        format!("{}", v as i64)
    } else {
        let s = format!("{:.2}", v);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Format a value with its unit for display.
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
}
