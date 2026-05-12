//! Command palette: a modal overlay that lets users search and execute commands.

use crate::audio::demod::DemodMode;
use crate::config::parse_frequency;

/// A command that can be executed from the palette.
#[derive(Debug, Clone)]
pub struct Command {
    /// Command name (what the user types).
    pub name: &'static str,
    /// Human-readable description.
    pub description: &'static str,
    /// Whether this command accepts a parameter after the name.
    pub accepts_param: bool,
    /// Whether a parameter is mandatory (subset of accepts_param).
    pub requires_param: bool,
    /// Hotkey equivalent (for display).
    pub hotkey: &'static str,
}

/// Registry of all available commands.
pub struct CommandRegistry {
    commands: Vec<Command>,
}

impl CommandRegistry {
    /// Create a new registry with all built-in commands.
    pub fn new() -> Self {
        let mut commands = vec![
            Command { name: "averaging", description: "Set spectrum averaging (1-20)", accepts_param: true, requires_param: true, hotkey: "-" },
            Command { name: "bandwidth", description: "Set sample rate/bandwidth", accepts_param: true, requires_param: true, hotkey: "b" },
            Command { name: "bookmark-add", description: "Save current freq as bookmark", accepts_param: true, requires_param: true, hotkey: "-" },
            Command { name: "bookmark-remove", description: "Remove user bookmark by name", accepts_param: true, requires_param: true, hotkey: "-" },
            Command { name: "bookmarks", description: "Open bookmarks browser", accepts_param: false, requires_param: false, hotkey: "-" },
            Command { name: "center", description: "Center cursor on screen", accepts_param: false, requires_param: false, hotkey: "c" },
            Command { name: "center-on-cursor", description: "Center bandwidth on cursor (retune)", accepts_param: false, requires_param: false, hotkey: "C" },
            Command { name: "channelbw", description: "Set channel bandwidth", accepts_param: true, requires_param: false, hotkey: "w/W" },
            Command { name: "demod", description: "Set demodulation mode (am/fm/nfm)", accepts_param: true, requires_param: false, hotkey: "d" },
            Command { name: "frequency", description: "Jump to frequency", accepts_param: true, requires_param: true, hotkey: "f" },
            Command { name: "output", description: "Select audio output device", accepts_param: false, requires_param: false, hotkey: "-" },
            Command { name: "gain+", description: "Increase gain by 1 dB", accepts_param: false, requires_param: false, hotkey: "↑" },
            Command { name: "gain-", description: "Decrease gain by 1 dB", accepts_param: false, requires_param: false, hotkey: "↓" },
            Command { name: "help", description: "Show keyboard shortcuts", accepts_param: false, requires_param: false, hotkey: "?" },
            Command { name: "quit", description: "Quit application", accepts_param: false, requires_param: false, hotkey: "q" },
            Command { name: "squelch", description: "Set squelch threshold (dB)", accepts_param: true, requires_param: false, hotkey: "-" },
            Command { name: "toggle-cursor", description: "Toggle cursor on/off", accepts_param: false, requires_param: false, hotkey: "Space" },
            Command { name: "tune+", description: "Tune up 25 kHz", accepts_param: false, requires_param: false, hotkey: "←" },
            Command { name: "tune-", description: "Tune down 25 kHz", accepts_param: false, requires_param: false, hotkey: "→" },
            Command { name: "tune-coarse+", description: "Tune up 1 MHz", accepts_param: false, requires_param: false, hotkey: "PgUp" },
            Command { name: "tune-coarse-", description: "Tune down 1 MHz", accepts_param: false, requires_param: false, hotkey: "PgDn" },
            Command { name: "vfo-add", description: "Add VFO at frequency", accepts_param: true, requires_param: true, hotkey: "m" },
            Command { name: "vfo-clear", description: "Remove all VFOs", accepts_param: false, requires_param: false, hotkey: "-" },
            Command { name: "vfo-remove", description: "Remove VFO by ID", accepts_param: true, requires_param: true, hotkey: "x" },
            Command { name: "volume", description: "Set volume (0-100)", accepts_param: true, requires_param: true, hotkey: "v/V" },
            Command { name: "waterfall", description: "Set waterfall time span (seconds)", accepts_param: true, requires_param: true, hotkey: "t/T" },
            Command { name: "zoom-in", description: "Zoom in (decrease bandwidth)", accepts_param: false, requires_param: false, hotkey: "+" },
            Command { name: "zoom-out", description: "Zoom out (increase bandwidth)", accepts_param: false, requires_param: false, hotkey: "-" },
        ];
        commands.sort_by_key(|c| c.name);
        Self { commands }
    }

    /// Get all commands.
    pub fn all(&self) -> &[Command] {
        &self.commands
    }

    /// Filter commands by prefix (case-insensitive).
    pub fn filter(&self, query: &str) -> Vec<&Command> {
        if query.is_empty() {
            return self.commands.iter().collect();
        }
        let query_lower = query.to_lowercase();
        self.commands
            .iter()
            .filter(|c| c.name.to_lowercase().contains(&query_lower))
            .collect()
    }
}

/// State for the command palette UI.
pub struct CommandPalette {
    /// The text the user has typed.
    pub input: String,
    /// Index of the currently selected item in filtered results.
    pub selected: usize,
    /// The command registry.
    pub registry: CommandRegistry,
    /// Error message to display (clears on next input).
    pub error: Option<String>,
}

impl CommandPalette {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            selected: 0,
            registry: CommandRegistry::new(),
            error: None,
        }
    }

    /// Reset the palette state (called when opening).
    pub fn reset(&mut self) {
        self.input.clear();
        self.selected = 0;
        self.error = None;
    }

    /// Get filtered commands based on current input.
    /// Only the command name part (before space) is used for filtering.
    pub fn filtered(&self) -> Vec<&Command> {
        let query = self.command_part();
        self.registry.filter(query)
    }

    /// Extract the command name part (before the first space).
    fn command_part(&self) -> &str {
        self.input.split_whitespace().next().unwrap_or("")
    }

    /// Extract the parameter part (after the first space), if any.
    fn param_part(&self) -> Option<&str> {
        if let Some(idx) = self.input.find(' ') {
            let param = self.input[idx + 1..].trim();
            if param.is_empty() { None } else { Some(param) }
        } else {
            None
        }
    }

    /// Handle a character being typed.
    pub fn type_char(&mut self, c: char) {
        self.input.push(c);
        self.selected = 0;
        self.error = None;
    }

    /// Handle backspace.
    pub fn backspace(&mut self) {
        self.input.pop();
        self.selected = 0;
        self.error = None;
    }

    /// Move selection up.
    pub fn select_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// Move selection down.
    pub fn select_down(&mut self) {
        let count = self.filtered().len();
        if count > 0 && self.selected < count - 1 {
            self.selected += 1;
        }
    }

    /// Autocomplete: fill input with the selected command name.
    pub fn autocomplete(&mut self) {
        let filtered = self.filtered();
        if let Some(cmd) = filtered.get(self.selected) {
            let name = cmd.name.to_string();
            let has_param = cmd.accepts_param;
            self.input = name;
            if has_param {
                self.input.push(' ');
            }
        }
    }

    /// Get the selected command name (resolved from filtered list or input).
    pub fn selected_command_name(&self) -> Option<&str> {
        let filtered = self.filtered();
        if let Some(cmd) = filtered.get(self.selected) {
            Some(cmd.name)
        } else {
            None
        }
    }

    /// Execute the current command. Returns Ok(action) on success, Err(message) on error.
    pub fn execute(&self) -> Result<CommandAction, String> {
        // If there's an exact match in filtered results, use that
        let filtered = self.filtered();
        let cmd = if let Some(cmd) = filtered.get(self.selected) {
            cmd
        } else {
            return Err("Unknown command".to_string());
        };
        let cmd_name = cmd.name;

        let param = self.param_part();

        // Check if a required parameter is missing
        if cmd.requires_param && param.is_none() {
            return Err(format!("{} requires a value (e.g. {} 100M)", cmd_name, cmd_name));
        }

        match cmd_name {
            "averaging" => {
                let p = param.unwrap();
                match p.parse::<usize>() {
                    Ok(v) if v >= 1 && v <= 20 => Ok(CommandAction::SetAveraging(v)),
                    Ok(_) => Err("Averaging must be 1-20".to_string()),
                    Err(_) => Err(format!("Invalid averaging value: '{}'", p)),
                }
            }
            "bandwidth" => {
                // requires_param check already ensures param is Some
                let p = param.unwrap();
                parse_frequency(p)
                    .map(CommandAction::SetBandwidth)
                    .map_err(|_| format!("Invalid bandwidth: '{}'", p))
            }
            "bookmark-add" => {
                let p = param.unwrap();
                Ok(CommandAction::AddBookmark(p.to_string()))
            }
            "bookmark-remove" => {
                let p = param.unwrap();
                Ok(CommandAction::RemoveBookmark(p.to_string()))
            }
            "bookmarks" => Ok(CommandAction::ShowBookmarks),
            "center" => Ok(CommandAction::CenterCursor),
            "center-on-cursor" => Ok(CommandAction::CenterOnCursor),
            "channelbw" => {
                if let Some(p) = param {
                    parse_frequency(p)
                        .map(CommandAction::SetChannelBw)
                        .map_err(|_| format!("Invalid channel bandwidth: '{}'", p))
                } else {
                    // No param: double channel BW like 'w'
                    Ok(CommandAction::WidenChannelBw)
                }
            }
            "demod" => {
                if let Some(p) = param {
                    match p.to_lowercase().as_str() {
                        "am" => Ok(CommandAction::SetDemodMode(DemodMode::AM)),
                        "fm" => Ok(CommandAction::SetDemodMode(DemodMode::FM)),
                        "nfm" => Ok(CommandAction::SetDemodMode(DemodMode::NFM)),
                        _ => Err("Invalid mode. Use: am, fm, nfm".to_string()),
                    }
                } else {
                    Err("demod requires a mode:\n  am  \u{2014} Amplitude Modulation (aviation 118-137 MHz, HF)\n  fm  \u{2014} Wideband FM (commercial radio 87-108 MHz, BW: 200 kHz)\n  nfm \u{2014} Narrowband FM (walkie-talkies, PMR446, BW: 25 kHz)".to_string())
                }
            }
            "frequency" => {
                // requires_param check already ensures param is Some
                let p = param.unwrap();
                parse_frequency(p)
                    .map(CommandAction::SetFrequency)
                    .map_err(|_| format!("Invalid frequency: '{}'", p))
            }
            "output" => Err("Audio output selection not yet implemented".to_string()),
            "gain+" => Ok(CommandAction::GainUp),
            "gain-" => Ok(CommandAction::GainDown),
            "help" => Ok(CommandAction::ShowHelp),
            "quit" => Ok(CommandAction::Quit),
            "toggle-cursor" => Ok(CommandAction::ToggleCursor),
            "tune+" => Ok(CommandAction::TuneUp),
            "tune-" => Ok(CommandAction::TuneDown),
            "tune-coarse+" => Ok(CommandAction::TuneCoarseUp),
            "tune-coarse-" => Ok(CommandAction::TuneCoarseDown),
            "squelch" => {
                if let Some(p) = param {
                    match p.parse::<f32>() {
                        Ok(db) => Ok(CommandAction::SetSquelch(Some(db))),
                        Err(_) => Err(format!("Invalid squelch value: '{}'", p)),
                    }
                } else {
                    Ok(CommandAction::SetSquelch(None))
                }
            }
            "volume" => {
                let p = param.unwrap();
                match p.parse::<u8>() {
                    Ok(v) if v <= 100 => Ok(CommandAction::SetVolume(v)),
                    Ok(_) => Err("Volume must be 0-100".to_string()),
                    Err(_) => Err(format!("Invalid volume: '{}'", p)),
                }
            }
            "zoom-in" => Ok(CommandAction::ZoomIn),
            "waterfall" => {
                let p = param.unwrap();
                match p.parse::<f64>() {
                    Ok(secs) if secs >= 5.0 && secs <= 120.0 => Ok(CommandAction::SetWaterfallTimeSpan(secs)),
                    Ok(_) => Err("Waterfall time span must be 5-120 seconds".to_string()),
                    Err(_) => Err(format!("Invalid time span: '{}'", p)),
                }
            }
            "zoom-out" => Ok(CommandAction::ZoomOut),
            "vfo-add" => {
                let p = param.unwrap();
                parse_frequency(p)
                    .map(CommandAction::AddVfo)
                    .map_err(|_| format!("Invalid frequency: '{}'", p))
            }
            "vfo-remove" => {
                let p = param.unwrap();
                match p.parse::<usize>() {
                    Ok(id) if id > 0 => Ok(CommandAction::RemoveVfo(id)),
                    _ => Err(format!("Invalid VFO ID: '{}'", p)),
                }
            }
            "vfo-clear" => Ok(CommandAction::ClearVfos),
            _ => Err("Unknown command".to_string()),
        }
    }
}

/// Actions that a command can produce.
#[derive(Debug, PartialEq)]
pub enum CommandAction {
    SetFrequency(f64),
    SetBandwidth(f64),
    SetChannelBw(f64),
    WidenChannelBw,
    CenterCursor,
    CenterOnCursor,
    GainUp,
    GainDown,
    ShowHelp,
    Quit,
    ToggleCursor,
    TuneUp,
    TuneDown,
    TuneCoarseUp,
    TuneCoarseDown,
    ZoomIn,
    ZoomOut,
    SetVolume(u8),
    SetSquelch(Option<f32>),
    SetWaterfallTimeSpan(f64),
    AddVfo(f64),
    RemoveVfo(usize),
    ClearVfos,
    SetDemodMode(DemodMode),
    ShowBookmarks,
    AddBookmark(String),
    RemoveBookmark(String),
    SetAveraging(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    // === CommandRegistry tests ===

    #[test]
    fn test_registry_has_all_commands() {
        let reg = CommandRegistry::new();
        assert_eq!(reg.all().len(), 28);
    }

    #[test]
    fn test_registry_alphabetical_order() {
        let reg = CommandRegistry::new();
        let names: Vec<&str> = reg.all().iter().map(|c| c.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn test_registry_filter_empty_returns_all() {
        let reg = CommandRegistry::new();
        assert_eq!(reg.filter("").len(), 28);
    }

    #[test]
    fn test_registry_filter_by_prefix() {
        let reg = CommandRegistry::new();
        let results = reg.filter("tune");
        assert_eq!(results.len(), 4); // tune+, tune-, tune-coarse+, tune-coarse-
        for cmd in &results {
            assert!(cmd.name.contains("tune"));
        }
    }

    #[test]
    fn test_registry_filter_partial_match() {
        let reg = CommandRegistry::new();
        let results = reg.filter("gain");
        assert_eq!(results.len(), 2); // gain+, gain-
    }

    #[test]
    fn test_registry_filter_no_match() {
        let reg = CommandRegistry::new();
        let results = reg.filter("xyznonexistent");
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_registry_filter_case_insensitive() {
        let reg = CommandRegistry::new();
        let results = reg.filter("ZOOM");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_registry_filter_substring() {
        let reg = CommandRegistry::new();
        // "bw" appears in "channelbw"
        let results = reg.filter("bw");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "channelbw");
    }

    // === CommandPalette state tests ===

    #[test]
    fn test_palette_new_empty() {
        let palette = CommandPalette::new();
        assert_eq!(palette.input, "");
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn test_palette_type_char() {
        let mut palette = CommandPalette::new();
        palette.type_char('f');
        assert_eq!(palette.input, "f");
        palette.type_char('r');
        assert_eq!(palette.input, "fr");
    }

    #[test]
    fn test_palette_type_resets_selection() {
        let mut palette = CommandPalette::new();
        palette.selected = 5;
        palette.type_char('z');
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn test_palette_backspace() {
        let mut palette = CommandPalette::new();
        palette.type_char('a');
        palette.type_char('b');
        palette.backspace();
        assert_eq!(palette.input, "a");
    }

    #[test]
    fn test_palette_backspace_empty() {
        let mut palette = CommandPalette::new();
        palette.backspace(); // should not panic
        assert_eq!(palette.input, "");
    }

    #[test]
    fn test_palette_backspace_resets_selection() {
        let mut palette = CommandPalette::new();
        palette.type_char('x');
        palette.selected = 3;
        palette.backspace();
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn test_palette_select_up_down() {
        let mut palette = CommandPalette::new();
        // All 15 commands visible
        palette.select_down();
        assert_eq!(palette.selected, 1);
        palette.select_down();
        assert_eq!(palette.selected, 2);
        palette.select_up();
        assert_eq!(palette.selected, 1);
        palette.select_up();
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn test_palette_select_up_at_zero() {
        let mut palette = CommandPalette::new();
        palette.select_up();
        assert_eq!(palette.selected, 0); // doesn't underflow
    }

    #[test]
    fn test_palette_select_down_at_end() {
        let mut palette = CommandPalette::new();
        // Go to last item
        for _ in 0..100 {
            palette.select_down();
        }
        assert_eq!(palette.selected, 27); // 28 commands, index 27
    }

    #[test]
    fn test_palette_select_down_no_results() {
        let mut palette = CommandPalette::new();
        palette.input = "xyznonexistent".to_string();
        palette.select_down(); // should not panic
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn test_palette_reset() {
        let mut palette = CommandPalette::new();
        palette.type_char('h');
        palette.selected = 3;
        palette.reset();
        assert_eq!(palette.input, "");
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn test_palette_filtered_with_input() {
        let mut palette = CommandPalette::new();
        palette.input = "zoom".to_string();
        let filtered = palette.filtered();
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].name, "zoom-in");
        assert_eq!(filtered[1].name, "zoom-out");
    }

    #[test]
    fn test_palette_autocomplete() {
        let mut palette = CommandPalette::new();
        palette.input = "ch".to_string();
        // filtered: channelbw (accepts param)
        palette.autocomplete();
        assert_eq!(palette.input, "channelbw ");
    }

    #[test]
    fn test_palette_autocomplete_no_param() {
        let mut palette = CommandPalette::new();
        palette.input = "cen".to_string();
        // filtered: center (no param)
        palette.autocomplete();
        assert_eq!(palette.input, "center");
    }

    #[test]
    fn test_palette_autocomplete_with_selection() {
        let mut palette = CommandPalette::new();
        palette.input = "gain".to_string();
        // filtered: gain+, gain-
        palette.selected = 1;
        palette.autocomplete();
        assert_eq!(palette.input, "gain-");
    }

    // === Command execution tests ===

    #[test]
    fn test_execute_quit() {
        let mut palette = CommandPalette::new();
        palette.input = "quit".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::Quit));
    }

    #[test]
    fn test_execute_center() {
        let mut palette = CommandPalette::new();
        palette.input = "center".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::CenterCursor));
    }

    #[test]
    fn test_execute_help() {
        let mut palette = CommandPalette::new();
        palette.input = "help".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::ShowHelp));
    }

    #[test]
    fn test_execute_toggle_cursor() {
        let mut palette = CommandPalette::new();
        palette.input = "toggle-cursor".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::ToggleCursor));
    }

    #[test]
    fn test_execute_gain_up() {
        let mut palette = CommandPalette::new();
        palette.input = "gain+".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::GainUp));
    }

    #[test]
    fn test_execute_gain_down() {
        let mut palette = CommandPalette::new();
        palette.input = "gain-".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::GainDown));
    }

    #[test]
    fn test_execute_tune_commands() {
        let mut palette = CommandPalette::new();

        palette.input = "tune+".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::TuneUp));

        palette.input = "tune-".to_string();
        palette.selected = 0;
        // "tune-" matches tune-, tune-coarse+, tune-coarse-. First is tune-
        assert_eq!(palette.execute(), Ok(CommandAction::TuneDown));
    }

    #[test]
    fn test_execute_tune_coarse() {
        let mut palette = CommandPalette::new();

        palette.input = "tune-coarse+".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::TuneCoarseUp));

        palette.input = "tune-coarse-".to_string();
        palette.selected = 0;
        assert_eq!(palette.execute(), Ok(CommandAction::TuneCoarseDown));
    }

    #[test]
    fn test_execute_zoom() {
        let mut palette = CommandPalette::new();

        palette.input = "zoom-in".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::ZoomIn));

        palette.input = "zoom-out".to_string();
        palette.selected = 0;
        assert_eq!(palette.execute(), Ok(CommandAction::ZoomOut));
    }

    #[test]
    fn test_execute_frequency_with_param() {
        let mut palette = CommandPalette::new();
        palette.input = "frequency 118.1M".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetFrequency(118_100_000.0)));
    }

    #[test]
    fn test_execute_frequency_without_param_requires_value() {
        let mut palette = CommandPalette::new();
        palette.input = "frequency".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("requires a value"));
    }

    #[test]
    fn test_execute_bandwidth_with_param() {
        let mut palette = CommandPalette::new();
        palette.input = "bandwidth 500k".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetBandwidth(500_000.0)));
    }

    #[test]
    fn test_execute_bandwidth_without_param_requires_value() {
        let mut palette = CommandPalette::new();
        palette.input = "bandwidth".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("requires a value"));
    }

    #[test]
    fn test_execute_channelbw_with_param() {
        let mut palette = CommandPalette::new();
        palette.input = "channelbw 50k".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetChannelBw(50_000.0)));
    }

    #[test]
    fn test_execute_channelbw_25k() {
        let mut palette = CommandPalette::new();
        palette.input = "channelbw 25k".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetChannelBw(25_000.0)));
    }

    #[test]
    fn test_execute_channelbw_without_param() {
        let mut palette = CommandPalette::new();
        palette.input = "channelbw".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::WidenChannelBw));
    }

    #[test]
    fn test_execute_invalid_param_returns_error() {
        let mut palette = CommandPalette::new();
        palette.input = "frequency notanumber".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("Invalid frequency"));
    }

    #[test]
    fn test_execute_no_match_returns_error() {
        let mut palette = CommandPalette::new();
        palette.input = "xyznonexistent".to_string();
        assert!(palette.execute().is_err());
    }

    #[test]
    fn test_execute_partial_match_uses_selected() {
        let mut palette = CommandPalette::new();
        palette.input = "gain".to_string();
        // filtered: gain+, gain-
        palette.selected = 0;
        assert_eq!(palette.execute(), Ok(CommandAction::GainUp));
        palette.selected = 1;
        assert_eq!(palette.execute(), Ok(CommandAction::GainDown));
    }

    // === Error handling tests ===

    #[test]
    fn test_error_clears_on_type() {
        let mut palette = CommandPalette::new();
        palette.error = Some("some error".to_string());
        palette.type_char('a');
        assert!(palette.error.is_none());
    }

    #[test]
    fn test_error_clears_on_backspace() {
        let mut palette = CommandPalette::new();
        palette.input = "f".to_string();
        palette.error = Some("some error".to_string());
        palette.backspace();
        assert!(palette.error.is_none());
    }

    #[test]
    fn test_error_clears_on_reset() {
        let mut palette = CommandPalette::new();
        palette.error = Some("some error".to_string());
        palette.reset();
        assert!(palette.error.is_none());
    }

    #[test]
    fn test_bandwidth_invalid_param_error() {
        let mut palette = CommandPalette::new();
        palette.input = "bandwidth xyz".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("Invalid bandwidth"));
    }

    #[test]
    fn test_channelbw_invalid_param_error() {
        let mut palette = CommandPalette::new();
        palette.input = "channelbw abc".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("Invalid channel bandwidth"));
    }

    #[test]
    fn test_frequency_trailing_space_requires_value() {
        let mut palette = CommandPalette::new();
        palette.input = "frequency ".to_string();
        // trailing space but no param → requires value error
        assert!(palette.execute().is_err());
    }

    #[test]
    fn test_bandwidth_trailing_space_requires_value() {
        let mut palette = CommandPalette::new();
        palette.input = "bandwidth ".to_string();
        assert!(palette.execute().is_err());
    }

    // === Edge cases ===

    #[test]
    fn test_param_with_extra_spaces() {
        let mut palette = CommandPalette::new();
        palette.input = "channelbw   50k".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetChannelBw(50_000.0)));
    }

    #[test]
    fn test_command_with_trailing_space_no_param() {
        let mut palette = CommandPalette::new();
        palette.input = "channelbw ".to_string();
        // trailing space but no actual param — channelbw doesn't require param
        assert_eq!(palette.execute(), Ok(CommandAction::WidenChannelBw));
    }

    #[test]
    fn test_filtered_uses_only_command_part() {
        let mut palette = CommandPalette::new();
        palette.input = "channelbw 50k".to_string();
        let filtered = palette.filtered();
        // Should filter by "channelbw", not "channelbw 50k"
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "channelbw");
    }

    // === Volume command tests ===

    #[test]
    fn test_execute_volume_with_param() {
        let mut palette = CommandPalette::new();
        palette.input = "volume 75".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetVolume(75)));
    }

    #[test]
    fn test_execute_volume_zero() {
        let mut palette = CommandPalette::new();
        palette.input = "volume 0".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetVolume(0)));
    }

    #[test]
    fn test_execute_volume_100() {
        let mut palette = CommandPalette::new();
        palette.input = "volume 100".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetVolume(100)));
    }

    #[test]
    fn test_execute_volume_out_of_range() {
        let mut palette = CommandPalette::new();
        palette.input = "volume 150".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("0-100"));
    }

    #[test]
    fn test_execute_volume_requires_param() {
        let mut palette = CommandPalette::new();
        palette.input = "volume".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("requires a value"));
    }

    #[test]
    fn test_execute_volume_invalid_param() {
        let mut palette = CommandPalette::new();
        palette.input = "volume abc".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("Invalid volume"));
    }

    // === Squelch command tests ===

    #[test]
    fn test_execute_squelch_with_param() {
        let mut palette = CommandPalette::new();
        palette.input = "squelch -60".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetSquelch(Some(-60.0))));
    }

    #[test]
    fn test_execute_squelch_without_param_disables() {
        let mut palette = CommandPalette::new();
        palette.input = "squelch".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetSquelch(None)));
    }

    #[test]
    fn test_execute_squelch_invalid_param() {
        let mut palette = CommandPalette::new();
        palette.input = "squelch abc".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("Invalid squelch"));
    }

    // === Output command tests ===

    #[test]
    fn test_execute_output_not_implemented() {
        let mut palette = CommandPalette::new();
        palette.input = "output".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("not yet implemented"));
    }

    // === Waterfall command tests ===

    #[test]
    fn test_execute_waterfall_with_param() {
        let mut palette = CommandPalette::new();
        palette.input = "waterfall 30".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetWaterfallTimeSpan(30.0)));
    }

    #[test]
    fn test_execute_waterfall_min() {
        let mut palette = CommandPalette::new();
        palette.input = "waterfall 5".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetWaterfallTimeSpan(5.0)));
    }

    #[test]
    fn test_execute_waterfall_max() {
        let mut palette = CommandPalette::new();
        palette.input = "waterfall 120".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetWaterfallTimeSpan(120.0)));
    }

    #[test]
    fn test_execute_waterfall_out_of_range() {
        let mut palette = CommandPalette::new();
        palette.input = "waterfall 200".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("5-120"));
    }

    #[test]
    fn test_execute_waterfall_too_low() {
        let mut palette = CommandPalette::new();
        palette.input = "waterfall 2".to_string();
        assert!(palette.execute().is_err());
    }

    #[test]
    fn test_execute_waterfall_requires_param() {
        let mut palette = CommandPalette::new();
        palette.input = "waterfall".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("requires a value"));
    }

    #[test]
    fn test_execute_waterfall_invalid_param() {
        let mut palette = CommandPalette::new();
        palette.input = "waterfall abc".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("Invalid time span"));
    }

    // === VFO command tests ===

    #[test]
    fn test_execute_vfo_add() {
        let mut palette = CommandPalette::new();
        palette.input = "vfo-add 118.1M".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::AddVfo(118_100_000.0)));
    }

    #[test]
    fn test_execute_vfo_add_requires_param() {
        let mut palette = CommandPalette::new();
        palette.input = "vfo-add".to_string();
        assert!(palette.execute().is_err());
    }

    #[test]
    fn test_execute_vfo_remove() {
        let mut palette = CommandPalette::new();
        palette.input = "vfo-remove 3".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::RemoveVfo(3)));
    }

    #[test]
    fn test_execute_vfo_remove_requires_param() {
        let mut palette = CommandPalette::new();
        palette.input = "vfo-remove".to_string();
        assert!(palette.execute().is_err());
    }

    #[test]
    fn test_execute_vfo_remove_invalid_id() {
        let mut palette = CommandPalette::new();
        palette.input = "vfo-remove abc".to_string();
        assert!(palette.execute().is_err());
    }

    #[test]
    fn test_execute_vfo_remove_zero_invalid() {
        let mut palette = CommandPalette::new();
        palette.input = "vfo-remove 0".to_string();
        assert!(palette.execute().is_err());
    }

    #[test]
    fn test_execute_vfo_clear() {
        let mut palette = CommandPalette::new();
        palette.input = "vfo-clear".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::ClearVfos));
    }

    // === Demod command tests ===

    #[test]
    fn test_execute_demod_am() {
        let mut palette = CommandPalette::new();
        palette.input = "demod am".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetDemodMode(DemodMode::AM)));
    }

    #[test]
    fn test_execute_demod_fm() {
        let mut palette = CommandPalette::new();
        palette.input = "demod fm".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetDemodMode(DemodMode::FM)));
    }

    #[test]
    fn test_execute_demod_nfm() {
        let mut palette = CommandPalette::new();
        palette.input = "demod nfm".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetDemodMode(DemodMode::NFM)));
    }

    #[test]
    fn test_execute_demod_case_insensitive() {
        let mut palette = CommandPalette::new();
        palette.input = "demod FM".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetDemodMode(DemodMode::FM)));
    }

    #[test]
    fn test_execute_demod_no_param_error() {
        let mut palette = CommandPalette::new();
        palette.input = "demod".to_string();
        let result = palette.execute();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("demod requires a mode"));
        assert!(err.contains("am"));
        assert!(err.contains("fm"));
        assert!(err.contains("nfm"));
    }

    #[test]
    fn test_execute_demod_invalid_mode() {
        let mut palette = CommandPalette::new();
        palette.input = "demod xyz".to_string();
        let result = palette.execute();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid mode"));
    }

    // === Averaging command tests ===

    #[test]
    fn test_execute_averaging_valid() {
        let mut palette = CommandPalette::new();
        palette.input = "averaging 5".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetAveraging(5)));
    }

    #[test]
    fn test_execute_averaging_min() {
        let mut palette = CommandPalette::new();
        palette.input = "averaging 1".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetAveraging(1)));
    }

    #[test]
    fn test_execute_averaging_max() {
        let mut palette = CommandPalette::new();
        palette.input = "averaging 20".to_string();
        assert_eq!(palette.execute(), Ok(CommandAction::SetAveraging(20)));
    }

    #[test]
    fn test_execute_averaging_zero_error() {
        let mut palette = CommandPalette::new();
        palette.input = "averaging 0".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("1-20"));
    }

    #[test]
    fn test_execute_averaging_over_max_error() {
        let mut palette = CommandPalette::new();
        palette.input = "averaging 25".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("1-20"));
    }

    #[test]
    fn test_execute_averaging_requires_param() {
        let mut palette = CommandPalette::new();
        palette.input = "averaging".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("requires a value"));
    }

    #[test]
    fn test_execute_averaging_invalid_param() {
        let mut palette = CommandPalette::new();
        palette.input = "averaging abc".to_string();
        assert!(palette.execute().is_err());
        assert!(palette.execute().unwrap_err().contains("Invalid averaging"));
    }
}
