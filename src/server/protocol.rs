//! Binary and JSON protocol definitions for WebSocket communication.

use crate::audio::demod::DemodMode;
use crate::types::FftResult;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Tag byte for spectrum binary frames.
pub const TAG_SPECTRUM: u8 = 0x01;

/// Tag byte for audio binary frames (Phase 3).
#[allow(dead_code)]
pub const TAG_AUDIO: u8 = 0x02;

/// Encode an `FftResult` into a binary spectrum frame.
///
/// Format: `[0x01][timestamp_ms: u64 LE][num_bins: u16 LE][dB: f32 LE × N]`
#[must_use] 
pub fn encode_spectrum_frame(fft: &FftResult) -> Vec<u8> {
    #[allow(clippy::cast_possible_truncation)] // intentional truncation
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    #[allow(clippy::cast_possible_truncation)] // intentional truncation
    let num_bins = fft.bins.len() as u16;
    let capacity = 1 + 8 + 2 + fft.bins.len() * 4;
    let mut buf = Vec::with_capacity(capacity);

    buf.push(TAG_SPECTRUM);
    buf.extend_from_slice(&timestamp_ms.to_le_bytes());
    buf.extend_from_slice(&num_bins.to_le_bytes());
    for &db in &fft.bins {
        buf.extend_from_slice(&db.to_le_bytes());
    }

    buf
}

/// Decode a binary spectrum frame back into its components.
///
/// Returns `(timestamp_ms, bins)` or `None` if the frame is malformed.
#[must_use]
#[allow(dead_code)]
pub fn decode_spectrum_frame(data: &[u8]) -> Option<(u64, Vec<f32>)> {
    if data.len() < 11 {
        return None;
    }
    if data[0] != TAG_SPECTRUM {
        return None;
    }

    let timestamp_ms = u64::from_le_bytes(data[1..9].try_into().ok()?);
    let num_bins = u16::from_le_bytes(data[9..11].try_into().ok()?) as usize;

    let expected_len = 11 + num_bins * 4;
    if data.len() < expected_len {
        return None;
    }

    let mut bins = Vec::with_capacity(num_bins);
    for i in 0..num_bins {
        let offset = 11 + i * 4;
        let val = f32::from_le_bytes(data[offset..offset + 4].try_into().ok()?);
        bins.push(val);
    }

    Some((timestamp_ms, bins))
}

// ============================================================================
// AGC state sub-message
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgcState {
    pub enabled: bool,
    pub attack_ms: f64,
    pub decay_ms: f64,
    pub target_level: f32,
    pub max_gain_db: f32,
}

impl Default for AgcState {
    fn default() -> Self {
        Self {
            enabled: true,
            attack_ms: 10.0,
            decay_ms: 300.0,
            target_level: 0.3,
            max_gain_db: 50.0,
        }
    }
}

// ============================================================================
// Bandpass state sub-message
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BandpassState {
    pub enabled: bool,
    pub low_hz: f64,
    pub high_hz: f64,
}

impl Default for BandpassState {
    fn default() -> Self {
        Self {
            enabled: true,
            low_hz: 300.0,
            high_hz: 3400.0,
        }
    }
}

// ============================================================================
// VFO state sub-message
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VfoState {
    pub id: usize,
    pub freq: f64,
    pub channel_bw: f64,
    pub mode: String,
    pub color: String,
}

// ============================================================================
// Full State Message
// ============================================================================

/// Server-to-client JSON state message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub center_freq: f64,
    pub sample_rate: f64,
    pub fft_size: usize,
    pub gain: Option<f32>,
    pub gain_mode: String,
    pub cursor_freq: f64,
    pub cursor_active: bool,
    pub demod_mode: String,
    pub channel_bw: f64,
    pub volume: u8,
    pub squelch: f32,
    pub muted: bool,
    pub agc: AgcState,
    pub bandpass: BandpassState,
    pub vfos: Vec<VfoState>,
    pub audio_format: String,
    pub clients_connected: usize,
}

impl StateMessage {
    /// Create a basic state message from config (backward compat for minimal state).
    #[must_use]
    #[allow(dead_code)]
    pub fn from_config(config: &crate::types::AppConfig, clients: usize) -> Self {
        Self {
            msg_type: "state".to_string(),
            center_freq: config.center_freq,
            sample_rate: config.sample_rate,
            fft_size: config.fft_size,
            gain: config.gain,
            gain_mode: if config.gain.is_some() { "manual" } else { "auto" }.to_string(),
            cursor_freq: config.center_freq,
            cursor_active: true,
            demod_mode: "AM".to_string(),
            channel_bw: DemodMode::AM.default_channel_bw(),
            volume: 80,
            squelch: -60.0,
            muted: false,
            agc: AgcState::default(),
            bandpass: BandpassState::default(),
            vfos: Vec::new(),
            audio_format: "opus".to_string(),
            clients_connected: clients,
        }
    }
}

// ============================================================================
// Client-to-server commands
// ============================================================================

/// Client-to-server command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    // Navigation
    SetFrequency { value: f64 },
    Tune { delta: f64 },
    SetBandwidth { value: f64 },
    Zoom { direction: String },

    // Audio
    SetVolume { value: u8 },
    SetSquelch { value: f32 },
    SetDemod { mode: String },
    SetChannelBw { value: f64 },
    ToggleMute,

    // VFOs
    VfoAdd {
        freq: f64,
        channel_bw: f64,
        mode: String,
    },
    VfoRemove { id: usize },
    VfoClear,

    // AGC
    SetAgc {
        enabled: bool,
        #[serde(default = "default_agc_attack")]
        attack_ms: f64,
        #[serde(default = "default_agc_decay")]
        decay_ms: f64,
        #[serde(default = "default_agc_target")]
        target_level: f32,
        #[serde(default = "default_agc_max_gain")]
        max_gain_db: f32,
    },

    // Bandpass
    SetBandpass {
        enabled: bool,
        #[serde(default = "default_bandpass_low")]
        low_hz: f64,
        #[serde(default = "default_bandpass_high")]
        high_hz: f64,
    },

    // System
    SetGain { value: f32 },
    GetState,
    SetAudioFormat { format: String },
}

fn default_agc_attack() -> f64 { 10.0 }
fn default_agc_decay() -> f64 { 300.0 }
fn default_agc_target() -> f32 { 0.3 }
fn default_agc_max_gain() -> f32 { 50.0 }
fn default_bandpass_low() -> f64 { 300.0 }
fn default_bandpass_high() -> f64 { 3400.0 }

/// Server error response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub cmd: String,
    pub message: String,
}

impl ErrorMessage {
    #[must_use] 
    pub fn new(cmd: &str, message: &str) -> Self {
        Self {
            msg_type: "error".to_string(),
            cmd: cmd.to_string(),
            message: message.to_string(),
        }
    }
}

// ============================================================================
// Helper: parse demod mode string
// ============================================================================

#[must_use] 
pub fn parse_demod_mode(s: &str) -> Option<DemodMode> {
    match s.to_uppercase().as_str() {
        "AM" => Some(DemodMode::AM),
        "FM" => Some(DemodMode::FM),
        "NFM" => Some(DemodMode::NFM),
        _ => None,
    }
}

/// Convert a ratatui Color to a string for the protocol.
#[must_use]
#[allow(dead_code)]
pub fn color_to_string(color: ratatui::style::Color) -> String {
    match color {
        ratatui::style::Color::Red => "red".to_string(),
        ratatui::style::Color::Green => "green".to_string(),
        ratatui::style::Color::Blue => "blue".to_string(),
        ratatui::style::Color::Magenta => "magenta".to_string(),
        ratatui::style::Color::Cyan => "cyan".to_string(),
        ratatui::style::Color::LightRed => "lightred".to_string(),
        ratatui::style::Color::LightMagenta => "lightmagenta".to_string(),
        _ => "white".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FftResult;

    #[test]
    fn test_encode_decode_spectrum_frame_roundtrip() {
        let fft = FftResult {
            bins: vec![-80.0, -60.5, -45.2, -30.0],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };

        let encoded = encode_spectrum_frame(&fft);
        assert_eq!(encoded[0], TAG_SPECTRUM);
        assert_eq!(encoded.len(), 27);

        let (ts, bins) = decode_spectrum_frame(&encoded).expect("decode should succeed");
        assert!(ts > 0);
        assert_eq!(bins.len(), 4);
        assert!((bins[0] - (-80.0)).abs() < f32::EPSILON);
        assert!((bins[1] - (-60.5)).abs() < f32::EPSILON);
        assert!((bins[2] - (-45.2)).abs() < f32::EPSILON);
        assert!((bins[3] - (-30.0)).abs() < f32::EPSILON);
    }

    #[test]
    fn test_encode_spectrum_frame_empty_bins() {
        let fft = FftResult {
            bins: vec![],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        let encoded = encode_spectrum_frame(&fft);
        assert_eq!(encoded.len(), 11);
        let (_, bins) = decode_spectrum_frame(&encoded).expect("decode should succeed");
        assert!(bins.is_empty());
    }

    #[test]
    fn test_decode_spectrum_frame_too_short() {
        assert!(decode_spectrum_frame(&[]).is_none());
        assert!(decode_spectrum_frame(&[0x01]).is_none());
        assert!(decode_spectrum_frame(&[0x01; 10]).is_none());
    }

    #[test]
    fn test_decode_spectrum_frame_wrong_tag() {
        let mut data = vec![0x02];
        data.extend_from_slice(&0u64.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        assert!(decode_spectrum_frame(&data).is_none());
    }

    #[test]
    fn test_decode_spectrum_frame_truncated_bins() {
        let mut data = vec![0x01];
        data.extend_from_slice(&0u64.to_le_bytes());
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&(-50.0f32).to_le_bytes());
        assert!(decode_spectrum_frame(&data).is_none());
    }

    #[test]
    fn test_encode_spectrum_frame_large() {
        let fft = FftResult {
            bins: vec![-50.0; 2048],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        let encoded = encode_spectrum_frame(&fft);
        assert_eq!(encoded.len(), 11 + 2048 * 4);
        let (_, bins) = decode_spectrum_frame(&encoded).unwrap();
        assert_eq!(bins.len(), 2048);
    }

    #[test]
    fn test_state_message_serialization() {
        let config = crate::types::AppConfig::default();
        let state = StateMessage::from_config(&config, 3);
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("\"type\":\"state\""));
        assert!(json.contains("\"clients_connected\":3"));
        assert!(json.contains("\"gain_mode\""));
        assert!(json.contains("\"cursor_freq\""));
        assert!(json.contains("\"demod_mode\""));
        assert!(json.contains("\"agc\""));
        assert!(json.contains("\"bandpass\""));
        assert!(json.contains("\"vfos\""));
        assert!(json.contains("\"audio_format\""));

        let parsed: StateMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.clients_connected, 3);
        assert_eq!(parsed.center_freq, config.center_freq);
    }

    #[test]
    fn test_state_message_roundtrip() {
        let config = crate::types::AppConfig::default();
        let original = StateMessage::from_config(&config, 5);
        let json = serde_json::to_string(&original).unwrap();
        let parsed: StateMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.msg_type, "state");
        assert_eq!(parsed.clients_connected, 5);
        assert!((parsed.center_freq - original.center_freq).abs() < 1.0);
        assert!((parsed.sample_rate - original.sample_rate).abs() < 1.0);
        assert_eq!(parsed.fft_size, original.fft_size);
        assert_eq!(parsed.gain, original.gain);
        assert_eq!(parsed.volume, 80);
        assert_eq!(parsed.demod_mode, "AM");
        assert!(parsed.cursor_active);
    }

    // === Command deserialization tests ===

    #[test]
    fn test_command_get_state() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "get_state"}"#).unwrap();
        assert!(matches!(cmd, Command::GetState));
    }

    #[test]
    fn test_command_set_frequency() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "set_frequency", "value": 118100000}"#).unwrap();
        match cmd {
            Command::SetFrequency { value } => assert!((value - 118100000.0).abs() < 1.0),
            _ => panic!("expected SetFrequency"),
        }
    }

    #[test]
    fn test_command_tune() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "tune", "delta": 25000}"#).unwrap();
        match cmd {
            Command::Tune { delta } => assert!((delta - 25000.0).abs() < 1.0),
            _ => panic!("expected Tune"),
        }
    }

    #[test]
    fn test_command_set_bandwidth() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "set_bandwidth", "value": 2048000}"#).unwrap();
        match cmd {
            Command::SetBandwidth { value } => assert!((value - 2048000.0).abs() < 1.0),
            _ => panic!("expected SetBandwidth"),
        }
    }

    #[test]
    fn test_command_zoom() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "zoom", "direction": "in"}"#).unwrap();
        match cmd {
            Command::Zoom { direction } => assert_eq!(direction, "in"),
            _ => panic!("expected Zoom"),
        }
    }

    #[test]
    fn test_command_set_volume() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "set_volume", "value": 80}"#).unwrap();
        match cmd {
            Command::SetVolume { value } => assert_eq!(value, 80),
            _ => panic!("expected SetVolume"),
        }
    }

    #[test]
    fn test_command_set_squelch() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "set_squelch", "value": -60}"#).unwrap();
        match cmd {
            Command::SetSquelch { value } => assert!((value - (-60.0)).abs() < 0.1),
            _ => panic!("expected SetSquelch"),
        }
    }

    #[test]
    fn test_command_set_demod() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "set_demod", "mode": "AM"}"#).unwrap();
        match cmd {
            Command::SetDemod { mode } => assert_eq!(mode, "AM"),
            _ => panic!("expected SetDemod"),
        }
    }

    #[test]
    fn test_command_set_channel_bw() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "set_channel_bw", "value": 8000}"#).unwrap();
        match cmd {
            Command::SetChannelBw { value } => assert!((value - 8000.0).abs() < 1.0),
            _ => panic!("expected SetChannelBw"),
        }
    }

    #[test]
    fn test_command_toggle_mute() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "toggle_mute"}"#).unwrap();
        assert!(matches!(cmd, Command::ToggleMute));
    }

    #[test]
    fn test_command_vfo_add() {
        let cmd: Command = serde_json::from_str(
            r#"{"cmd": "vfo_add", "freq": 118100000, "channel_bw": 8000, "mode": "AM"}"#,
        ).unwrap();
        match cmd {
            Command::VfoAdd { freq, channel_bw, mode } => {
                assert!((freq - 118100000.0).abs() < 1.0);
                assert!((channel_bw - 8000.0).abs() < 1.0);
                assert_eq!(mode, "AM");
            }
            _ => panic!("expected VfoAdd"),
        }
    }

    #[test]
    fn test_command_vfo_remove() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "vfo_remove", "id": 3}"#).unwrap();
        match cmd {
            Command::VfoRemove { id } => assert_eq!(id, 3),
            _ => panic!("expected VfoRemove"),
        }
    }

    #[test]
    fn test_command_vfo_clear() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "vfo_clear"}"#).unwrap();
        assert!(matches!(cmd, Command::VfoClear));
    }

    #[test]
    fn test_command_set_agc() {
        let cmd: Command = serde_json::from_str(
            r#"{"cmd": "set_agc", "enabled": true, "attack_ms": 10, "decay_ms": 300, "target_level": 0.3, "max_gain_db": 50}"#,
        ).unwrap();
        match cmd {
            Command::SetAgc { enabled, attack_ms, decay_ms, target_level, max_gain_db } => {
                assert!(enabled);
                assert!((attack_ms - 10.0).abs() < 0.1);
                assert!((decay_ms - 300.0).abs() < 0.1);
                assert!((target_level - 0.3).abs() < 0.01);
                assert!((max_gain_db - 50.0).abs() < 0.1);
            }
            _ => panic!("expected SetAgc"),
        }
    }

    #[test]
    fn test_command_set_agc_defaults() {
        // Only "enabled" is required; others should use defaults
        let cmd: Command = serde_json::from_str(r#"{"cmd": "set_agc", "enabled": false}"#).unwrap();
        match cmd {
            Command::SetAgc { enabled, attack_ms, decay_ms, target_level, max_gain_db } => {
                assert!(!enabled);
                assert!((attack_ms - 10.0).abs() < 0.1);
                assert!((decay_ms - 300.0).abs() < 0.1);
                assert!((target_level - 0.3).abs() < 0.01);
                assert!((max_gain_db - 50.0).abs() < 0.1);
            }
            _ => panic!("expected SetAgc"),
        }
    }

    #[test]
    fn test_command_set_bandpass() {
        let cmd: Command = serde_json::from_str(
            r#"{"cmd": "set_bandpass", "enabled": true, "low_hz": 300, "high_hz": 3400}"#,
        ).unwrap();
        match cmd {
            Command::SetBandpass { enabled, low_hz, high_hz } => {
                assert!(enabled);
                assert!((low_hz - 300.0).abs() < 0.1);
                assert!((high_hz - 3400.0).abs() < 0.1);
            }
            _ => panic!("expected SetBandpass"),
        }
    }

    #[test]
    fn test_command_set_bandpass_defaults() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "set_bandpass", "enabled": false}"#).unwrap();
        match cmd {
            Command::SetBandpass { enabled, low_hz, high_hz } => {
                assert!(!enabled);
                assert!((low_hz - 300.0).abs() < 0.1);
                assert!((high_hz - 3400.0).abs() < 0.1);
            }
            _ => panic!("expected SetBandpass"),
        }
    }

    #[test]
    fn test_command_set_gain() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "set_gain", "value": 30}"#).unwrap();
        match cmd {
            Command::SetGain { value } => assert!((value - 30.0).abs() < 0.1),
            _ => panic!("expected SetGain"),
        }
    }

    #[test]
    fn test_command_set_audio_format() {
        let cmd: Command = serde_json::from_str(r#"{"cmd": "set_audio_format", "format": "opus"}"#).unwrap();
        match cmd {
            Command::SetAudioFormat { format } => assert_eq!(format, "opus"),
            _ => panic!("expected SetAudioFormat"),
        }
    }

    #[test]
    fn test_command_invalid_json() {
        let result = serde_json::from_str::<Command>(r#"{"cmd": "nonexistent"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_command_missing_required_field() {
        assert!(serde_json::from_str::<Command>(r#"{"cmd": "set_frequency"}"#).is_err());
        assert!(serde_json::from_str::<Command>(r#"{"cmd": "tune"}"#).is_err());
    }

    #[test]
    fn test_command_not_json() {
        assert!(serde_json::from_str::<Command>("hello").is_err());
        assert!(serde_json::from_str::<Command>("").is_err());
        assert!(serde_json::from_str::<Command>("{}").is_err());
    }

    #[test]
    fn test_error_message_serialization() {
        let err = ErrorMessage::new("set_frequency", "Invalid frequency");
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"type\":\"error\""));
        assert!(json.contains("\"cmd\":\"set_frequency\""));
    }

    #[test]
    fn test_decode_spectrum_frame_extra_data_ignored() {
        let mut data = vec![0x01];
        data.extend_from_slice(&1000u64.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&(-42.0f32).to_le_bytes());
        data.extend_from_slice(&[0xFF, 0xFF]);
        let (ts, bins) = decode_spectrum_frame(&data).unwrap();
        assert_eq!(ts, 1000);
        assert_eq!(bins.len(), 1);
        assert!((bins[0] - (-42.0)).abs() < f32::EPSILON);
    }

    #[test]
    fn test_encode_spectrum_frame_special_float_values() {
        let fft = FftResult {
            bins: vec![f32::INFINITY, f32::NEG_INFINITY, f32::NAN, 0.0, -0.0],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        let encoded = encode_spectrum_frame(&fft);
        let (_, bins) = decode_spectrum_frame(&encoded).unwrap();
        assert_eq!(bins.len(), 5);
        assert!(bins[0].is_infinite() && bins[0] > 0.0);
        assert!(bins[1].is_infinite() && bins[1] < 0.0);
        assert!(bins[2].is_nan());
    }

    #[test]
    fn test_parse_demod_mode() {
        assert_eq!(parse_demod_mode("AM"), Some(DemodMode::AM));
        assert_eq!(parse_demod_mode("am"), Some(DemodMode::AM));
        assert_eq!(parse_demod_mode("FM"), Some(DemodMode::FM));
        assert_eq!(parse_demod_mode("fm"), Some(DemodMode::FM));
        assert_eq!(parse_demod_mode("NFM"), Some(DemodMode::NFM));
        assert_eq!(parse_demod_mode("nfm"), Some(DemodMode::NFM));
        assert_eq!(parse_demod_mode("xyz"), None);
    }

    #[test]
    fn test_agc_state_default() {
        let agc = AgcState::default();
        assert!(agc.enabled);
        assert!((agc.attack_ms - 10.0).abs() < 0.1);
        assert!((agc.decay_ms - 300.0).abs() < 0.1);
        assert!((agc.target_level - 0.3).abs() < 0.01);
        assert!((agc.max_gain_db - 50.0).abs() < 0.1);
    }

    #[test]
    fn test_bandpass_state_default() {
        let bp = BandpassState::default();
        assert!(bp.enabled);
        assert!((bp.low_hz - 300.0).abs() < 0.1);
        assert!((bp.high_hz - 3400.0).abs() < 0.1);
    }

    #[test]
    fn test_vfo_state_serialization() {
        let vfo = VfoState {
            id: 1,
            freq: 118100000.0,
            channel_bw: 8000.0,
            mode: "AM".to_string(),
            color: "red".to_string(),
        };
        let json = serde_json::to_string(&vfo).unwrap();
        let parsed: VfoState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, 1);
        assert_eq!(parsed.mode, "AM");
    }

    #[test]
    fn test_state_message_has_all_fields() {
        let config = crate::types::AppConfig::default();
        let state = StateMessage::from_config(&config, 2);
        assert_eq!(state.msg_type, "state");
        assert_eq!(state.gain_mode, "auto"); // default config has gain=None
        assert_eq!(state.demod_mode, "AM");
        assert_eq!(state.volume, 80);
        assert!((state.squelch - (-60.0)).abs() < 0.1);
        assert!(!state.muted);
        assert_eq!(state.audio_format, "opus");
    }

    #[test]
    fn test_color_to_string() {
        assert_eq!(color_to_string(ratatui::style::Color::Red), "red");
        assert_eq!(color_to_string(ratatui::style::Color::Green), "green");
        assert_eq!(color_to_string(ratatui::style::Color::Blue), "blue");
    }
}
