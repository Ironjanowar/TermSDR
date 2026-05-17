//! Command dispatcher: takes a parsed Command and applies it to shared server state.

use crate::audio::demod::DemodMode;
use crate::audio::AudioControl;

use super::protocol::{
    self, AgcState, BandpassState, Command, ErrorMessage, StateMessage, VfoState,
};

/// VFO color palette (matches TUI's `VFO_COLORS`).
const VFO_COLOR_NAMES: [&str; 8] = [
    "red", "green", "blue", "magenta", "cyan", "lightred", "white", "lightmagenta",
];

/// Minimum allowed sample rate / bandwidth in Hz (256 kHz).
const MIN_BANDWIDTH: f64 = 256_000.0;

/// Maximum allowed sample rate / bandwidth in Hz (3.2 MHz).
const MAX_BANDWIDTH: f64 = 3_200_000.0;

/// Zoom-in bandwidth multiplier (reduce by 20%).
const ZOOM_IN_FACTOR: f64 = 0.8;

/// Zoom-out bandwidth multiplier (increase by 25%).
const ZOOM_OUT_FACTOR: f64 = 1.25;

/// Maximum volume level.
const MAX_VOLUME: u8 = 100;

/// Squelch threshold below which squelch is disabled (dB).
const SQUELCH_OFF_THRESHOLD: f32 = -100.0;

/// Minimum allowed channel bandwidth in Hz (1 kHz).
const MIN_CHANNEL_BW: f64 = 1_000.0;

/// Maximum allowed channel bandwidth in Hz (1 MHz).
const MAX_CHANNEL_BW: f64 = 1_000_000.0;

/// Maximum number of VFOs allowed.
const MAX_VFOS: usize = 8;

/// Shared mutable state for the headless server.
/// Mirrors the fields from `App` that the WS protocol exposes.
#[derive(Debug, Clone)]
pub struct SharedState {
    pub center_freq: f64,
    pub sample_rate: f64,
    pub fft_size: usize,
    pub gain: Option<f32>,
    pub cursor_freq: f64,
    pub cursor_active: bool,
    pub demod_mode: DemodMode,
    pub channel_bw: f64,
    pub volume: u8,
    pub squelch: f32,
    pub muted: bool,
    pub agc: AgcState,
    pub bandpass: BandpassState,
    pub vfos: Vec<VfoEntry>,
    pub next_vfo_id: usize,
    pub audio_format: String,
}

#[derive(Debug, Clone)]
pub struct VfoEntry {
    pub id: usize,
    pub freq: f64,
    pub channel_bw: f64,
    pub mode: DemodMode,
    pub color: String,
}

impl SharedState {
    /// Create from `AppConfig` with defaults.
    #[must_use] 
    pub fn from_config(config: &crate::types::AppConfig) -> Self {
        Self {
            center_freq: config.center_freq,
            sample_rate: config.sample_rate,
            fft_size: config.fft_size,
            gain: config.gain,
            cursor_freq: config.center_freq,
            cursor_active: true,
            demod_mode: DemodMode::AM,
            channel_bw: DemodMode::AM.default_channel_bw(),
            volume: 80,
            squelch: -60.0,
            muted: false,
            agc: AgcState::default(),
            bandpass: BandpassState::default(),
            vfos: Vec::new(),
            next_vfo_id: 1,
            audio_format: "opus".to_string(),
        }
    }

    /// Build a `StateMessage` from current state.
    #[must_use] 
    pub fn to_state_message(&self, clients_connected: usize) -> StateMessage {
        StateMessage {
            msg_type: "state".to_string(),
            center_freq: self.center_freq,
            sample_rate: self.sample_rate,
            fft_size: self.fft_size,
            gain: self.gain,
            gain_mode: if self.gain.is_some() { "manual" } else { "auto" }.to_string(),
            cursor_freq: self.cursor_freq,
            cursor_active: self.cursor_active,
            demod_mode: self.demod_mode.label().to_string(),
            channel_bw: self.channel_bw,
            volume: self.volume,
            squelch: self.squelch,
            muted: self.muted,
            agc: self.agc.clone(),
            bandpass: self.bandpass.clone(),
            vfos: self.vfos.iter().map(|v| VfoState {
                id: v.id,
                freq: v.freq,
                channel_bw: v.channel_bw,
                mode: v.mode.label().to_string(),
                color: v.color.clone(),
            }).collect(),
            audio_format: self.audio_format.clone(),
            clients_connected,
        }
    }
}

/// Result of dispatching a command.
pub enum DispatchResult {
    /// Command succeeded, state was mutated. Broadcast new state to all clients.
    Ok,
    /// Command succeeded but state was NOT mutated (e.g. `get_state`). Just send state to requester.
    StateOnly,
    /// Command failed with an error message. Send error to requester only.
    Error(ErrorMessage),
}

/// Dispatch a command, mutating shared state and returning audio control messages to send.
#[allow(clippy::too_many_lines)] // large match dispatch over all command types
pub fn dispatch(
    cmd: &Command,
    state: &mut SharedState,
    audio_tx: Option<&crossbeam_channel::Sender<AudioControl>>,
) -> (DispatchResult, Vec<AudioControl>) {
    let mut audio_msgs: Vec<AudioControl> = Vec::new();

    let send = |msgs: &mut Vec<AudioControl>, msg: AudioControl| {
        msgs.push(msg);
    };

    match cmd {
        // ── Navigation ──
        Command::SetFrequency { value } => {
            if *value < 0.0 {
                return (DispatchResult::Error(ErrorMessage::new(
                    "set_frequency", "Frequency must be positive"
                )), audio_msgs);
            }
            state.center_freq = *value;
            state.cursor_freq = *value;
            send(&mut audio_msgs, AudioControl::SetCenterFreq(state.center_freq));
            send(&mut audio_msgs, AudioControl::SetCursorFreq(state.cursor_freq));
        }

        Command::Tune { delta } => {
            state.center_freq += delta;
            state.cursor_freq += delta;
            send(&mut audio_msgs, AudioControl::SetCenterFreq(state.center_freq));
            send(&mut audio_msgs, AudioControl::SetCursorFreq(state.cursor_freq));
        }

        Command::SetBandwidth { value } => {
            if *value < MIN_BANDWIDTH || *value > MAX_BANDWIDTH {
                return (DispatchResult::Error(ErrorMessage::new(
                    "set_bandwidth", "Bandwidth must be between 256kHz and 3.2MHz"
                )), audio_msgs);
            }
            state.sample_rate = *value;
        }

        Command::Zoom { direction } => {
            match direction.as_str() {
                "in" => state.sample_rate = (state.sample_rate * ZOOM_IN_FACTOR).max(MIN_BANDWIDTH),
                "out" => state.sample_rate = (state.sample_rate * ZOOM_OUT_FACTOR).min(MAX_BANDWIDTH),
                _ => {
                    return (DispatchResult::Error(ErrorMessage::new(
                        "zoom", "Direction must be 'in' or 'out'"
                    )), audio_msgs);
                }
            }
        }

        // ── Audio ──
        Command::SetVolume { value } => {
            if *value > MAX_VOLUME {
                return (DispatchResult::Error(ErrorMessage::new(
                    "set_volume", "Volume must be 0-100"
                )), audio_msgs);
            }
            state.volume = *value;
            send(&mut audio_msgs, AudioControl::SetVolume(state.volume));
        }

        Command::SetSquelch { value } => {
            state.squelch = *value;
            let sq = if *value <= SQUELCH_OFF_THRESHOLD { None } else { Some(*value) };
            send(&mut audio_msgs, AudioControl::SetSquelch(sq));
        }

        Command::SetDemod { mode } => {
            match protocol::parse_demod_mode(mode) {
                Some(m) => {
                    state.demod_mode = m;
                    state.channel_bw = m.default_channel_bw();
                    send(&mut audio_msgs, AudioControl::SetDemodMode(m));
                    send(&mut audio_msgs, AudioControl::SetChannelBw(state.channel_bw));
                }
                None => {
                    return (DispatchResult::Error(ErrorMessage::new(
                        "set_demod", "Invalid mode. Use: AM, FM, NFM"
                    )), audio_msgs);
                }
            }
        }

        Command::SetChannelBw { value } => {
            if *value < MIN_CHANNEL_BW || *value > MAX_CHANNEL_BW {
                return (DispatchResult::Error(ErrorMessage::new(
                    "set_channel_bw", "Channel BW must be between 1kHz and 1MHz"
                )), audio_msgs);
            }
            state.channel_bw = *value;
            send(&mut audio_msgs, AudioControl::SetChannelBw(state.channel_bw));
        }

        Command::ToggleMute => {
            state.muted = !state.muted;
            state.cursor_active = !state.muted;
            send(&mut audio_msgs, AudioControl::Mute(state.muted));
        }

        // ── VFOs ──
        Command::VfoAdd { freq, channel_bw, mode } => {
            if state.vfos.len() >= MAX_VFOS {
                return (DispatchResult::Error(ErrorMessage::new(
                    "vfo_add", "Maximum 8 VFOs"
                )), audio_msgs);
            }
            let demod = protocol::parse_demod_mode(mode).unwrap_or(state.demod_mode);
            let id = state.next_vfo_id;
            let color = VFO_COLOR_NAMES[state.vfos.len() % 8].to_string();
            state.vfos.push(VfoEntry {
                id,
                freq: *freq,
                channel_bw: *channel_bw,
                mode: demod,
                color,
            });
            state.next_vfo_id += 1;
            send(&mut audio_msgs, AudioControl::AddVfo {
                id,
                freq: *freq,
                channel_bw: *channel_bw,
                mode: demod,
            });
        }

        Command::VfoRemove { id } => {
            if let Some(idx) = state.vfos.iter().position(|v| v.id == *id) {
                state.vfos.remove(idx);
                send(&mut audio_msgs, AudioControl::RemoveVfo(*id));
            } else {
                return (DispatchResult::Error(ErrorMessage::new(
                    "vfo_remove", &format!("VFO {id} not found")
                )), audio_msgs);
            }
        }

        Command::VfoClear => {
            let ids: Vec<usize> = state.vfos.iter().map(|v| v.id).collect();
            state.vfos.clear();
            for id in ids {
                send(&mut audio_msgs, AudioControl::RemoveVfo(id));
            }
        }

        // ── AGC ──
        Command::SetAgc { enabled, attack_ms, decay_ms, target_level, max_gain_db } => {
            state.agc = AgcState {
                enabled: *enabled,
                attack_ms: *attack_ms,
                decay_ms: *decay_ms,
                target_level: *target_level,
                max_gain_db: *max_gain_db,
            };
            send(&mut audio_msgs, AudioControl::SetAgcEnabled(*enabled));
            send(&mut audio_msgs, AudioControl::SetAgcAttack(*attack_ms));
            send(&mut audio_msgs, AudioControl::SetAgcDecay(*decay_ms));
            send(&mut audio_msgs, AudioControl::SetAgcTarget(*target_level));
            send(&mut audio_msgs, AudioControl::SetAgcMaxGain(*max_gain_db));
        }

        // ── Bandpass ──
        Command::SetBandpass { enabled, low_hz, high_hz } => {
            state.bandpass = BandpassState {
                enabled: *enabled,
                low_hz: *low_hz,
                high_hz: *high_hz,
            };
            send(&mut audio_msgs, AudioControl::SetBandpassEnabled(*enabled));
            send(&mut audio_msgs, AudioControl::SetBandpassLowCut(*low_hz));
            send(&mut audio_msgs, AudioControl::SetBandpassHighCut(*high_hz));
        }

        // ── System ──
        Command::SetGain { value } => {
            if *value < 0.0 || *value > 50.0 {
                return (DispatchResult::Error(ErrorMessage::new(
                    "set_gain", "Gain must be 0-50 dB"
                )), audio_msgs);
            }
            state.gain = Some(*value);
        }

        Command::GetState => {
            return (DispatchResult::StateOnly, audio_msgs);
        }

        Command::SetAudioFormat { format } => {
            match format.as_str() {
                "opus" | "pcm" => {
                    state.audio_format.clone_from(format);
                }
                _ => {
                    return (DispatchResult::Error(ErrorMessage::new(
                        "set_audio_format", "Format must be 'opus' or 'pcm'"
                    )), audio_msgs);
                }
            }
        }
    }

    // Send audio control messages
    if let Some(tx) = audio_tx {
        for msg in &audio_msgs {
            let _ = tx.send(msg.clone());
        }
    }

    (DispatchResult::Ok, audio_msgs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AppConfig;

    fn make_state() -> SharedState {
        SharedState::from_config(&AppConfig::default())
    }

    // ── SharedState creation ──

    #[test]
    fn test_shared_state_from_config() {
        let state = make_state();
        assert!((state.center_freq - 100_000_000.0).abs() < 1.0);
        assert_eq!(state.volume, 80);
        assert_eq!(state.demod_mode, DemodMode::AM);
        assert!(state.vfos.is_empty());
        assert_eq!(state.next_vfo_id, 1);
    }

    #[test]
    fn test_shared_state_to_state_message() {
        let state = make_state();
        let msg = state.to_state_message(3);
        assert_eq!(msg.msg_type, "state");
        assert_eq!(msg.clients_connected, 3);
        assert_eq!(msg.demod_mode, "AM");
        assert_eq!(msg.volume, 80);
    }

    // ── Navigation commands ──

    #[test]
    fn test_set_frequency() {
        let mut state = make_state();
        let cmd = Command::SetFrequency { value: 118_100_000.0 };
        let (result, msgs) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!((state.center_freq - 118_100_000.0).abs() < 1.0);
        assert!((state.cursor_freq - 118_100_000.0).abs() < 1.0);
        assert_eq!(msgs.len(), 2); // SetCenterFreq + SetCursorFreq
    }

    #[test]
    fn test_set_frequency_negative_error() {
        let mut state = make_state();
        let cmd = Command::SetFrequency { value: -1.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Error(_)));
    }

    #[test]
    fn test_tune() {
        let mut state = make_state();
        let orig = state.center_freq;
        let cmd = Command::Tune { delta: 25000.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!((state.center_freq - (orig + 25000.0)).abs() < 1.0);
    }

    #[test]
    fn test_tune_negative() {
        let mut state = make_state();
        let orig = state.center_freq;
        let cmd = Command::Tune { delta: -100_000.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!((state.center_freq - (orig - 100_000.0)).abs() < 1.0);
    }

    #[test]
    fn test_set_bandwidth() {
        let mut state = make_state();
        let cmd = Command::SetBandwidth { value: 1_024_000.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!((state.sample_rate - 1_024_000.0).abs() < 1.0);
    }

    #[test]
    fn test_set_bandwidth_out_of_range() {
        let mut state = make_state();
        let cmd = Command::SetBandwidth { value: 100_000.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Error(_)));
    }

    #[test]
    fn test_zoom_in() {
        let mut state = make_state();
        let orig = state.sample_rate;
        let cmd = Command::Zoom { direction: "in".to_string() };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!(state.sample_rate < orig);
    }

    #[test]
    fn test_zoom_out() {
        let mut state = make_state();
        let orig = state.sample_rate;
        let cmd = Command::Zoom { direction: "out".to_string() };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!(state.sample_rate > orig);
    }

    #[test]
    fn test_zoom_invalid_direction() {
        let mut state = make_state();
        let cmd = Command::Zoom { direction: "left".to_string() };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Error(_)));
    }

    // ── Audio commands ──

    #[test]
    fn test_set_volume() {
        let mut state = make_state();
        let cmd = Command::SetVolume { value: 50 };
        let (result, msgs) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert_eq!(state.volume, 50);
        assert!(msgs.iter().any(|m| matches!(m, AudioControl::SetVolume(50))));
    }

    #[test]
    fn test_set_volume_over_100() {
        let mut state = make_state();
        let cmd = Command::SetVolume { value: 150 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Error(_)));
    }

    #[test]
    fn test_set_squelch() {
        let mut state = make_state();
        let cmd = Command::SetSquelch { value: -40.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!((state.squelch - (-40.0)).abs() < 0.1);
    }

    #[test]
    fn test_set_demod_am() {
        let mut state = make_state();
        state.demod_mode = DemodMode::FM;
        let cmd = Command::SetDemod { mode: "AM".to_string() };
        let (result, msgs) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert_eq!(state.demod_mode, DemodMode::AM);
        assert!((state.channel_bw - 8_000.0).abs() < 1.0);
        assert!(msgs.iter().any(|m| matches!(m, AudioControl::SetDemodMode(DemodMode::AM))));
    }

    #[test]
    fn test_set_demod_invalid() {
        let mut state = make_state();
        let cmd = Command::SetDemod { mode: "SSB".to_string() };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Error(_)));
    }

    #[test]
    fn test_set_channel_bw() {
        let mut state = make_state();
        let cmd = Command::SetChannelBw { value: 25_000.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!((state.channel_bw - 25_000.0).abs() < 1.0);
    }

    #[test]
    fn test_set_channel_bw_out_of_range() {
        let mut state = make_state();
        let cmd = Command::SetChannelBw { value: 500.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Error(_)));
    }

    #[test]
    fn test_toggle_mute() {
        let mut state = make_state();
        assert!(!state.muted);
        let cmd = Command::ToggleMute;
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!(state.muted);
        assert!(!state.cursor_active);

        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!(!state.muted);
        assert!(state.cursor_active);
    }

    // ── VFO commands ──

    #[test]
    fn test_vfo_add() {
        let mut state = make_state();
        let cmd = Command::VfoAdd {
            freq: 118_100_000.0,
            channel_bw: 8000.0,
            mode: "AM".to_string(),
        };
        let (result, msgs) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert_eq!(state.vfos.len(), 1);
        assert_eq!(state.vfos[0].id, 1);
        assert!((state.vfos[0].freq - 118_100_000.0).abs() < 1.0);
        assert_eq!(state.vfos[0].color, "red");
        assert_eq!(state.next_vfo_id, 2);
        assert!(msgs.iter().any(|m| matches!(m, AudioControl::AddVfo { id: 1, .. })));
    }

    #[test]
    fn test_vfo_add_max_8() {
        let mut state = make_state();
        for i in 0..8 {
            let cmd = Command::VfoAdd {
                freq: 100_000_000.0 + i as f64 * 1_000_000.0,
                channel_bw: 8000.0,
                mode: "AM".to_string(),
            };
            let (result, _) = dispatch(&cmd, &mut state, None);
            assert!(matches!(result, DispatchResult::Ok));
        }
        assert_eq!(state.vfos.len(), 8);

        let cmd = Command::VfoAdd {
            freq: 200_000_000.0,
            channel_bw: 8000.0,
            mode: "AM".to_string(),
        };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Error(_)));
    }

    #[test]
    fn test_vfo_remove() {
        let mut state = make_state();
        let add = Command::VfoAdd { freq: 100e6, channel_bw: 8000.0, mode: "AM".to_string() };
        dispatch(&add, &mut state, None);
        assert_eq!(state.vfos.len(), 1);

        let cmd = Command::VfoRemove { id: 1 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!(state.vfos.is_empty());
    }

    #[test]
    fn test_vfo_remove_not_found() {
        let mut state = make_state();
        let cmd = Command::VfoRemove { id: 99 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Error(_)));
    }

    #[test]
    fn test_vfo_clear() {
        let mut state = make_state();
        for _ in 0..3 {
            let add = Command::VfoAdd { freq: 100e6, channel_bw: 8000.0, mode: "AM".to_string() };
            dispatch(&add, &mut state, None);
        }
        assert_eq!(state.vfos.len(), 3);

        let cmd = Command::VfoClear;
        let (result, msgs) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!(state.vfos.is_empty());
        assert_eq!(msgs.len(), 3); // 3 RemoveVfo
    }

    // ── AGC commands ──

    #[test]
    fn test_set_agc() {
        let mut state = make_state();
        let cmd = Command::SetAgc {
            enabled: false,
            attack_ms: 20.0,
            decay_ms: 500.0,
            target_level: 0.5,
            max_gain_db: 40.0,
        };
        let (result, msgs) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!(!state.agc.enabled);
        assert!((state.agc.attack_ms - 20.0).abs() < 0.1);
        assert!((state.agc.decay_ms - 500.0).abs() < 0.1);
        assert!((state.agc.target_level - 0.5).abs() < 0.01);
        assert!((state.agc.max_gain_db - 40.0).abs() < 0.1);
        assert_eq!(msgs.len(), 5); // 5 AGC audio controls
    }

    // ── Bandpass commands ──

    #[test]
    fn test_set_bandpass() {
        let mut state = make_state();
        let cmd = Command::SetBandpass {
            enabled: false,
            low_hz: 100.0,
            high_hz: 5000.0,
        };
        let (result, msgs) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!(!state.bandpass.enabled);
        assert!((state.bandpass.low_hz - 100.0).abs() < 0.1);
        assert!((state.bandpass.high_hz - 5000.0).abs() < 0.1);
        assert_eq!(msgs.len(), 3); // 3 bandpass audio controls
    }

    // ── System commands ──

    #[test]
    fn test_set_gain() {
        let mut state = make_state();
        let cmd = Command::SetGain { value: 30.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert_eq!(state.gain, Some(30.0));
    }

    #[test]
    fn test_set_gain_out_of_range() {
        let mut state = make_state();
        let cmd = Command::SetGain { value: 60.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Error(_)));
    }

    #[test]
    fn test_get_state() {
        let mut state = make_state();
        let cmd = Command::GetState;
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::StateOnly));
    }

    #[test]
    fn test_set_audio_format_opus() {
        let mut state = make_state();
        let cmd = Command::SetAudioFormat { format: "opus".to_string() };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert_eq!(state.audio_format, "opus");
    }

    #[test]
    fn test_set_audio_format_pcm() {
        let mut state = make_state();
        let cmd = Command::SetAudioFormat { format: "pcm".to_string() };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert_eq!(state.audio_format, "pcm");
    }

    #[test]
    fn test_set_audio_format_invalid() {
        let mut state = make_state();
        let cmd = Command::SetAudioFormat { format: "mp3".to_string() };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Error(_)));
    }

    // ── Audio control channel integration ──

    #[test]
    fn test_dispatch_sends_audio_controls() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut state = make_state();
        let cmd = Command::SetVolume { value: 42 };
        dispatch(&cmd, &mut state, Some(&tx));

        let msg = rx.recv().unwrap();
        assert!(matches!(msg, AudioControl::SetVolume(42)));
    }

    #[test]
    fn test_dispatch_no_audio_on_error() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut state = make_state();
        let cmd = Command::SetVolume { value: 200 }; // invalid
        dispatch(&cmd, &mut state, Some(&tx));
        assert!(rx.try_recv().is_err()); // no messages sent
    }

    // ── State message roundtrip ──

    // ── Edge case tests ──

    #[test]
    fn test_set_frequency_zero() {
        // 0 Hz is technically positive, should succeed
        let mut state = make_state();
        let cmd = Command::SetFrequency { value: 0.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!((state.center_freq).abs() < 0.1);
    }

    #[test]
    fn test_tune_negative_result() {
        // Tune can push frequency negative — no validation
        let mut state = make_state();
        state.center_freq = 1_000_000.0;
        let cmd = Command::Tune { delta: -2_000_000.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!(state.center_freq < 0.0); // documents current behavior
    }

    #[test]
    fn test_set_volume_boundary_0() {
        let mut state = make_state();
        let cmd = Command::SetVolume { value: 0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert_eq!(state.volume, 0);
    }

    #[test]
    fn test_set_volume_boundary_100() {
        let mut state = make_state();
        let cmd = Command::SetVolume { value: 100 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert_eq!(state.volume, 100);
    }

    #[test]
    fn test_set_volume_boundary_101() {
        let mut state = make_state();
        let cmd = Command::SetVolume { value: 101 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Error(_)));
    }

    #[test]
    fn test_set_bandwidth_boundary_low() {
        let mut state = make_state();
        let cmd = Command::SetBandwidth { value: 256_000.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
    }

    #[test]
    fn test_set_bandwidth_boundary_high() {
        let mut state = make_state();
        let cmd = Command::SetBandwidth { value: 3_200_000.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
    }

    #[test]
    fn test_set_bandwidth_just_below_min() {
        let mut state = make_state();
        let cmd = Command::SetBandwidth { value: 255_999.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Error(_)));
    }

    #[test]
    fn test_set_bandwidth_just_above_max() {
        let mut state = make_state();
        let cmd = Command::SetBandwidth { value: 3_200_001.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Error(_)));
    }

    #[test]
    fn test_set_gain_boundary_0() {
        let mut state = make_state();
        let cmd = Command::SetGain { value: 0.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
    }

    #[test]
    fn test_set_gain_boundary_50() {
        let mut state = make_state();
        let cmd = Command::SetGain { value: 50.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
    }

    #[test]
    fn test_set_gain_negative() {
        let mut state = make_state();
        let cmd = Command::SetGain { value: -1.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Error(_)));
    }

    #[test]
    fn test_set_channel_bw_boundary_low() {
        let mut state = make_state();
        let cmd = Command::SetChannelBw { value: 1_000.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
    }

    #[test]
    fn test_set_channel_bw_boundary_high() {
        let mut state = make_state();
        let cmd = Command::SetChannelBw { value: 1_000_000.0 };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
    }

    #[test]
    fn test_zoom_in_clamps_to_min() {
        let mut state = make_state();
        state.sample_rate = 256_000.0; // already at min
        let cmd = Command::Zoom { direction: "in".to_string() };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!((state.sample_rate - 256_000.0).abs() < 1.0);
    }

    #[test]
    fn test_zoom_out_clamps_to_max() {
        let mut state = make_state();
        state.sample_rate = 3_200_000.0; // already at max
        let cmd = Command::Zoom { direction: "out".to_string() };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!((state.sample_rate - 3_200_000.0).abs() < 1.0);
    }

    #[test]
    fn test_vfo_add_invalid_mode_uses_current() {
        let mut state = make_state();
        state.demod_mode = DemodMode::FM;
        let cmd = Command::VfoAdd {
            freq: 100e6,
            channel_bw: 8000.0,
            mode: "INVALID".to_string(),
        };
        let (result, _) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert_eq!(state.vfos[0].mode, DemodMode::FM); // falls back to current
    }

    #[test]
    fn test_vfo_ids_are_unique_after_remove() {
        let mut state = make_state();
        let add = Command::VfoAdd { freq: 100e6, channel_bw: 8000.0, mode: "AM".to_string() };
        dispatch(&add, &mut state, None); // id=1
        dispatch(&add, &mut state, None); // id=2
        dispatch(&Command::VfoRemove { id: 1 }, &mut state, None);
        dispatch(&add, &mut state, None); // id=3 (not reused)
        assert_eq!(state.vfos.len(), 2);
        assert_eq!(state.vfos[0].id, 2);
        assert_eq!(state.vfos[1].id, 3);
    }

    #[test]
    fn test_squelch_disable_threshold() {
        // Squelch <= -100 means disabled (None)
        let mut state = make_state();
        let cmd = Command::SetSquelch { value: -100.0 };
        let (_, msgs) = dispatch(&cmd, &mut state, None);
        assert!(msgs.iter().any(|m| matches!(m, AudioControl::SetSquelch(None))));
    }

    #[test]
    fn test_squelch_enable_threshold() {
        let mut state = make_state();
        let cmd = Command::SetSquelch { value: -99.0 };
        let (_, msgs) = dispatch(&cmd, &mut state, None);
        assert!(msgs.iter().any(|m| matches!(m, AudioControl::SetSquelch(Some(v)) if (*v - (-99.0)).abs() < 0.1)));
    }

    #[test]
    fn test_get_state_no_side_effects() {
        let mut state = make_state();
        let snapshot_before = state.clone();
        let cmd = Command::GetState;
        let (result, msgs) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::StateOnly));
        assert!(msgs.is_empty());
        // Verify no state mutation
        assert!((state.center_freq - snapshot_before.center_freq).abs() < 0.1);
        assert_eq!(state.volume, snapshot_before.volume);
        assert_eq!(state.muted, snapshot_before.muted);
        assert_eq!(state.vfos.len(), snapshot_before.vfos.len());
    }

    #[test]
    fn test_set_demod_changes_channel_bw() {
        let mut state = make_state();
        // AM default bw
        assert!((state.channel_bw - DemodMode::AM.default_channel_bw()).abs() < 1.0);
        // Switch to FM
        dispatch(&Command::SetDemod { mode: "FM".to_string() }, &mut state, None);
        assert!((state.channel_bw - DemodMode::FM.default_channel_bw()).abs() < 1.0);
        // Switch to NFM
        dispatch(&Command::SetDemod { mode: "NFM".to_string() }, &mut state, None);
        assert!((state.channel_bw - DemodMode::NFM.default_channel_bw()).abs() < 1.0);
    }

    #[test]
    fn test_vfo_clear_on_empty() {
        let mut state = make_state();
        let cmd = Command::VfoClear;
        let (result, msgs) = dispatch(&cmd, &mut state, None);
        assert!(matches!(result, DispatchResult::Ok));
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_state_message_reflects_mutations() {
        let mut state = make_state();
        dispatch(&Command::SetVolume { value: 42 }, &mut state, None);
        dispatch(&Command::SetDemod { mode: "FM".to_string() }, &mut state, None);
        dispatch(&Command::VfoAdd {
            freq: 118.1e6,
            channel_bw: 8000.0,
            mode: "AM".to_string(),
        }, &mut state, None);

        let msg = state.to_state_message(2);
        assert_eq!(msg.volume, 42);
        assert_eq!(msg.demod_mode, "FM");
        assert_eq!(msg.vfos.len(), 1);
        assert_eq!(msg.clients_connected, 2);
    }

    #[test]
    fn test_multiple_commands_sequence() {
        let mut state = make_state();
        dispatch(&Command::SetFrequency { value: 118_100_000.0 }, &mut state, None);
        dispatch(&Command::SetDemod { mode: "AM".to_string() }, &mut state, None);
        dispatch(&Command::SetVolume { value: 50 }, &mut state, None);
        dispatch(&Command::SetSquelch { value: -40.0 }, &mut state, None);
        dispatch(&Command::VfoAdd { freq: 121_500_000.0, channel_bw: 8000.0, mode: "AM".to_string() }, &mut state, None);
        dispatch(&Command::SetAgc { enabled: true, attack_ms: 5.0, decay_ms: 200.0, target_level: 0.4, max_gain_db: 45.0 }, &mut state, None);

        let msg = state.to_state_message(1);
        assert!((msg.center_freq - 118_100_000.0).abs() < 1.0);
        assert_eq!(msg.volume, 50);
        assert!((msg.squelch - (-40.0)).abs() < 0.1);
        assert_eq!(msg.vfos.len(), 1);
        assert!(msg.agc.enabled);
        assert!((msg.agc.attack_ms - 5.0).abs() < 0.1);
    }

    #[test]
    fn test_set_gain_does_not_send_audio_control() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut state = make_state();
        dispatch(&Command::SetGain { value: 25.0 }, &mut state, Some(&tx));
        assert!(rx.try_recv().is_err(), "SetGain should not send AudioControl messages");
        assert_eq!(state.gain, Some(25.0));
    }

    #[test]
    fn test_set_bandwidth_does_not_send_audio_control() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut state = make_state();
        dispatch(&Command::SetBandwidth { value: 1_024_000.0 }, &mut state, Some(&tx));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_zoom_does_not_send_audio_control() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut state = make_state();
        dispatch(&Command::Zoom { direction: "in".to_string() }, &mut state, Some(&tx));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_set_audio_format_does_not_send_audio_control() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut state = make_state();
        dispatch(&Command::SetAudioFormat { format: "pcm".to_string() }, &mut state, Some(&tx));
        assert!(rx.try_recv().is_err());
    }
}
