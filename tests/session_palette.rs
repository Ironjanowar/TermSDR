//! End-to-end command-palette coverage for native versioned JSON sessions.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crossbeam_channel::{unbounded, Receiver};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use termsdr::audio::AudioControl;
use termsdr::types::{AppConfig, FftResult};
use termsdr::ui::App;

const BARAJAS_SESSION: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/barajas-session-v1.json"
);
const UNSUPPORTED_SESSION: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/unsupported-session-v999.json"
);

static NEXT_TEMP_DIR: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let sequence = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "termsdr-session-{label}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn palette_command(app: &mut App, command: &str) {
    app.handle_key(key(KeyCode::Char('/')));
    for character in command.chars() {
        app.handle_key(key(KeyCode::Char(character)));
    }
    app.handle_key(key(KeyCode::Enter));
}

fn audio_messages(receiver: &Receiver<AudioControl>) -> Vec<String> {
    receiver
        .try_iter()
        .map(|message| format!("{message:?}"))
        .collect()
}

#[test]
fn palette_registers_config_load_and_config_save_as_path_commands() {
    let app = App::new(AppConfig::default(), None);
    let commands = app.command_palette.registry.all();

    for name in ["config-load", "config-save"] {
        let command = commands
            .iter()
            .find(|command| command.name == name)
            .unwrap_or_else(|| panic!("palette must register {name}"));
        assert!(command.accepts_param, "{name} must accept an exact path");
        assert!(command.requires_param, "{name} must require an exact path");
    }
}

#[test]
fn config_save_writes_versioned_durable_state_and_excludes_runtime_state() {
    let temp = TempDir::new("save-schema");
    let path = temp.path().join("nested/profiles/tower-session");
    let mut app = App::new(AppConfig::default(), None);

    app.config.center_freq = 118_522_500.0;
    app.config.sample_rate = 2_048_000.0;
    app.config.fft_size = 4096;
    app.config.gain = Some(28.0);
    app.config.ppm = -1;
    app.cursor_freq = 118_070_000.0;
    app.channel_bw = 6_000.0;
    app.audio_muted = true;
    app.volume = 73;
    app.squelch = Some(-60.0);
    app.min_db = -95.0;
    app.max_db = -5.0;
    app.averaging = 6;
    app.waterfall_time_span = 30.0;
    app.scan_active = true;
    app.scan_interval_ms = 750;

    app.latest_fft = Some(FftResult {
        bins: vec![-40.0, -30.0],
        center_freq: 1.0,
        sample_rate: 2.0,
    });
    app.waterfall_history = vec![vec![-77.0]];
    app.fps = 59.9;
    app.show_help = true;
    app.freq_input_mode = true;
    app.freq_input_buf = "must-not-persist".to_string();

    palette_command(&mut app, &format!("config-save {}", path.display()));

    let document: Value =
        serde_json::from_slice(&fs::read(&path).expect("exact save path must exist"))
            .expect("saved session must be JSON");
    assert_eq!(document["version"], 1);
    assert_eq!(document["receiver"]["center_frequency_hz"], 118_522_500.0);
    assert_eq!(document["receiver"]["sample_rate_hz"], 2_048_000.0);
    assert_eq!(document["receiver"]["fft_size"], 4096);
    assert_eq!(document["receiver"]["gain_db"], 28.0);
    assert_eq!(document["receiver"]["ppm"], -1);
    assert_eq!(document["cursor"]["frequency_hz"], 118_070_000.0);
    assert_eq!(document["cursor"]["mode"], "am");
    assert_eq!(document["cursor"]["channel_bandwidth_hz"], 6_000.0);
    assert_eq!(document["cursor"]["muted"], true);
    assert!(document["vfos"].is_array());
    assert_eq!(document["audio"]["volume"], 73);
    assert_eq!(document["audio"]["global_squelch_dbfs"], -60.0);
    assert!(document["audio"]["agc"].is_object());
    assert!(document["audio"]["bandpass"].is_object());
    assert_eq!(document["display"]["min_db"], -95.0);
    assert_eq!(document["display"]["max_db"], -5.0);
    assert_eq!(document["display"]["averaging"], 6);
    assert_eq!(document["display"]["waterfall_span_seconds"], 30.0);
    assert_eq!(document["scan"]["active"], true);
    assert_eq!(document["scan"]["interval_ms"], 750);

    let json = document.to_string();
    for forbidden in [
        "source",
        "device_index",
        "iq_path",
        "latest_fft",
        "waterfall_history",
        "fps",
        "show_help",
        "command_palette",
        "freq_input",
        "input_buf",
    ] {
        assert!(
            !json.contains(forbidden),
            "ephemeral/source key {forbidden:?} leaked"
        );
    }
    assert!(
        !path.with_extension("json").exists(),
        "save must not append an extension"
    );
}

#[test]
fn config_save_creates_parents_and_atomically_overwrites_the_exact_path() {
    let temp = TempDir::new("atomic-overwrite");
    let path = temp.path().join("deep/tree/session.custom");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"old content that is not JSON").unwrap();

    let mut app = App::new(AppConfig::default(), None);
    palette_command(&mut app, &format!("config-save {}", path.display()));

    let bytes = fs::read(&path).expect("save must overwrite the requested path");
    let document: Value =
        serde_json::from_slice(&bytes).expect("overwrite must leave complete JSON");
    assert_eq!(document["version"], 1);
    let siblings: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(siblings, vec![path.file_name().unwrap()]);
}

#[test]
fn runtime_config_load_replaces_mutable_state_and_dispatches_every_audio_update() {
    let (audio_tx, audio_rx) = unbounded();
    let mut initial = AppConfig::default();
    initial.center_freq = 99_900_000.0;
    initial.sample_rate = 1_024_000.0;
    initial.fft_size = 1024;
    initial.gain = None;
    initial.ppm = 7;
    let mut app = App::new(initial, Some(audio_tx));

    palette_command(&mut app, "vfo-add 100.1M");
    let _ = audio_messages(&audio_rx);
    palette_command(&mut app, &format!("config-load {BARAJAS_SESSION}"));

    assert_eq!(app.config.center_freq, 118_522_500.0);
    assert_eq!(app.cursor_freq, 118_070_000.0);
    assert_eq!(format!("{:?}", app.demod_mode), "AM");
    assert_eq!(app.channel_bw, 6_000.0);
    assert!(!app.audio_muted);
    assert_eq!(app.volume, 80);
    assert_eq!(app.squelch, Some(-60.0));
    assert_eq!(app.min_db, -95.0);
    assert_eq!(app.max_db, -5.0);
    assert_eq!(app.averaging, 6);
    assert_eq!(app.waterfall_time_span, 30.0);
    assert!(!app.scan_active);
    assert_eq!(app.scan_interval_ms, 750);

    assert_eq!(
        app.config.sample_rate, 1_024_000.0,
        "startup-only sample rate waits for restart"
    );
    assert_eq!(
        app.config.fft_size, 1024,
        "startup-only FFT size waits for restart"
    );
    assert_eq!(
        app.config.gain, None,
        "startup-only RF gain waits for restart"
    );
    assert_eq!(app.config.ppm, 7, "startup-only PPM waits for restart");

    let messages = audio_messages(&audio_rx).join("\n");
    for expected in [
        "RemoveVfo(1)",
        "SetTuning",
        "SetChannelBw(6000.0)",
        "SetDemodMode(AM)",
        "Mute(false)",
        "SetVolume(80)",
        "SetSquelch(Some(-60.0))",
        "SetAgcEnabled(true)",
        "SetAgcAttack(10.0)",
        "SetAgcDecay(250.0)",
        "SetAgcTarget(0.3)",
        "SetAgcMaxGain(40.0)",
        "SetBandpassEnabled(true)",
        "SetBandpassLowCut(300.0)",
        "SetBandpassHighCut(3000.0)",
        "AddVfo { id: 1, freq: 118150000.0",
        "AddVfo { id: 2, freq: 118670000.0",
        "AddVfo { id: 3, freq: 118975000.0",
    ] {
        assert!(
            messages.contains(expected),
            "missing audio update {expected:?} in:\n{messages}"
        );
    }
}

#[test]
fn load_then_save_preserves_vfo_names_agc_and_bandpass_from_the_session() {
    let temp = TempDir::new("load-save-durable-audio");
    let saved_path = temp.path().join("saved-again.json");
    let mut app = App::new(AppConfig::default(), None);

    palette_command(&mut app, "vfo-add 101.5M");
    palette_command(&mut app, &format!("config-load {BARAJAS_SESSION}"));
    palette_command(&mut app, &format!("config-save {}", saved_path.display()));

    let expected: Value = serde_json::from_slice(&fs::read(BARAJAS_SESSION).unwrap()).unwrap();
    let actual: Value = serde_json::from_slice(&fs::read(saved_path).unwrap()).unwrap();
    assert_eq!(actual["vfos"], expected["vfos"]);
    assert_eq!(actual["audio"]["agc"], expected["audio"]["agc"]);
    assert_eq!(actual["audio"]["bandpass"], expected["audio"]["bandpass"]);
}

#[test]
fn runtime_load_reports_restart_required_for_hardware_bound_receiver_fields() {
    let mut app = App::new(AppConfig::default(), None);
    palette_command(&mut app, &format!("config-load {BARAJAS_SESSION}"));

    let report = app
        .command_palette
        .error
        .as_deref()
        .expect("runtime load must report deferred receiver fields");
    assert!(
        app.command_palette_active,
        "restart warning must remain visible after the load"
    );
    assert!(report.contains("restart required"));
    for field in ["FFT size", "gain", "PPM"] {
        assert!(
            report.contains(field),
            "restart report omitted {field}: {report}"
        );
    }
    assert!(
        !report.contains("sample rate"),
        "restart report listed unchanged sample rate: {report}"
    );
}

#[test]
fn runtime_load_reports_only_startup_fields_that_actually_differ() {
    let mut initial = AppConfig::default();
    initial.sample_rate = 2_048_000.0;
    initial.fft_size = 1024;
    initial.gain = Some(28.0);
    initial.ppm = -1;
    let mut app = App::new(initial, None);

    palette_command(&mut app, &format!("config-load {BARAJAS_SESSION}"));

    let report = app
        .command_palette
        .error
        .as_deref()
        .expect("the differing FFT size requires a visible warning");
    assert!(
        app.command_palette_active,
        "restart warning must remain visible after the load"
    );
    assert!(report.contains("restart required"));
    assert!(report.contains("FFT size"));
    for unchanged in ["sample rate", "gain", "PPM"] {
        assert!(
            !report.contains(unchanged),
            "warning listed unchanged field {unchanged}: {report}"
        );
    }
}

#[test]
fn runtime_load_has_no_restart_warning_when_startup_fields_match() {
    let mut initial = AppConfig::default();
    initial.sample_rate = 2_048_000.0;
    initial.fft_size = 4096;
    initial.gain = Some(28.0);
    initial.ppm = -1;
    let mut app = App::new(initial, None);

    palette_command(&mut app, &format!("config-load {BARAJAS_SESSION}"));

    assert!(
        app.command_palette.error.is_none(),
        "matching startup fields must not produce a restart warning: {:?}",
        app.command_palette.error
    );
}

#[test]
fn runtime_load_clears_waterfall_rows_from_the_old_center_geometry() {
    let mut app = App::new(AppConfig::default(), None);
    app.latest_fft = Some(FftResult {
        bins: vec![-70.0, -60.0],
        center_freq: 100_000_000.0,
        sample_rate: 2_048_000.0,
    });
    app.waterfall_history = vec![vec![-70.0, -60.0], vec![-71.0, -61.0]];

    palette_command(&mut app, &format!("config-load {BARAJAS_SESSION}"));

    assert!(
        app.waterfall_history.is_empty(),
        "rows captured under the old center must disappear immediately"
    );
}

#[test]
fn unsupported_session_version_is_rejected_without_mutating_state() {
    let mut app = App::new(AppConfig::default(), None);
    let before = (
        app.config.center_freq,
        app.cursor_freq,
        app.volume,
        app.squelch,
        app.min_db,
        app.max_db,
    );

    palette_command(&mut app, &format!("config-load {UNSUPPORTED_SESSION}"));

    assert!(
        app.command_palette_active,
        "a failed load must remain visible"
    );
    let error = app
        .command_palette
        .error
        .as_deref()
        .expect("version error must be shown");
    assert!(
        error.contains("unsupported session version 999"),
        "unexpected error: {error}"
    );
    assert_eq!(
        before,
        (
            app.config.center_freq,
            app.cursor_freq,
            app.volume,
            app.squelch,
            app.min_db,
            app.max_db,
        )
    );
}

#[test]
fn barajas_fixture_encodes_the_approved_airband_example_without_a_source() {
    let document: Value = serde_json::from_slice(&fs::read(BARAJAS_SESSION).unwrap()).unwrap();

    assert_eq!(document["version"], 1);
    assert_eq!(document["receiver"]["center_frequency_hz"], 118_522_500.0);
    assert_eq!(document["receiver"]["sample_rate_hz"], 2_048_000.0);
    assert_eq!(document["cursor"]["name"], "TWR1");
    assert_eq!(document["cursor"]["frequency_hz"], 118_070_000.0);
    assert_eq!(document["cursor"]["mode"], "am");
    assert_eq!(document["cursor"]["channel_bandwidth_hz"], 6_000.0);

    let vfos = document["vfos"].as_array().unwrap();
    let expected = [
        ("TWR2", 118_150_000.0),
        ("TWR3", 118_670_000.0),
        ("TWR4", 118_975_000.0),
    ];
    for (vfo, (name, frequency)) in vfos.iter().zip(expected) {
        assert_eq!(vfo["name"], name);
        assert_eq!(vfo["frequency_hz"], frequency);
        assert_eq!(vfo["mode"], "am");
        assert_eq!(vfo["channel_bandwidth_hz"], 6_000.0);
    }
    assert_eq!(document["audio"]["global_squelch_dbfs"], -60.0);

    let json = document.to_string();
    for forbidden in ["source", "device", "iq_path"] {
        assert!(
            !json.contains(forbidden),
            "Barajas fixture leaked {forbidden}"
        );
    }
}
