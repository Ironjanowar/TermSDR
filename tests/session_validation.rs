//! Reviewer-blocker regression coverage for native-session validation and atomic rejection.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use clap::Parser;
use crossbeam_channel::unbounded;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{json, Value};
use termsdr::config::CliArgs;
use termsdr::session;
use termsdr::types::AppConfig;
use termsdr::ui::App;

const BARAJAS_SESSION: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/barajas-session-v1.json"
);

static NEXT_FILE: AtomicUsize = AtomicUsize::new(0);

fn temp_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "termsdr-invalid-session-{label}-{}-{}.json",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ))
}

fn write_case(label: &str, document: &Value) -> PathBuf {
    let path = temp_path(label);
    fs::write(&path, serde_json::to_vec(document).unwrap()).unwrap();
    path
}

fn palette_command(app: &mut App, command: &str) {
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(key(KeyCode::Char('/')));
    for character in command.chars() {
        app.handle_key(key(KeyCode::Char(character)));
    }
    app.handle_key(key(KeyCode::Enter));
}

fn changed(base: &Value, path: &[&str], value: Value) -> Value {
    let mut result = base.clone();
    let mut slot = &mut result;
    for key in path {
        slot = if let Ok(index) = key.parse::<usize>() {
            &mut slot[index]
        } else {
            &mut slot[*key]
        };
    }
    *slot = value;
    result
}

fn absurd_receiver_documents() -> Vec<(&'static str, Value)> {
    let base: Value = serde_json::from_slice(&fs::read(BARAJAS_SESSION).unwrap()).unwrap();

    vec![
        // `usize::MAX` FFT bins cannot be allocated; reject before any DSP buffer exists.
        (
            "absurd-fft-size",
            changed(&base, &["receiver", "fft_size"], json!(usize::MAX)),
        ),
        // Finite but physically meaningless tuning. These must not reach hardware.
        (
            "absurd-center-frequency",
            changed(&base, &["receiver", "center_frequency_hz"], json!(f64::MAX)),
        ),
        (
            "absurd-sample-rate",
            changed(&base, &["receiver", "sample_rate_hz"], json!(f64::MAX)),
        ),
        (
            "absurd-gain",
            changed(&base, &["receiver", "gain_db"], json!(f32::MAX)),
        ),
        (
            "ppm-min",
            changed(&base, &["receiver", "ppm"], json!(i32::MIN)),
        ),
        (
            "ppm-max",
            changed(&base, &["receiver", "ppm"], json!(i32::MAX)),
        ),
    ]
}

fn invalid_documents() -> Vec<(&'static str, Value)> {
    let base: Value = serde_json::from_slice(&fs::read(BARAJAS_SESSION).unwrap()).unwrap();
    let first_vfo = base["vfos"][0].clone();
    let mut nine_vfos = base.clone();
    nine_vfos["vfos"] = Value::Array(
        (1..=9)
            .map(|id| {
                let mut vfo = first_vfo.clone();
                vfo["id"] = json!(id);
                vfo
            })
            .collect(),
    );
    let mut duplicate_ids = base.clone();
    duplicate_ids["vfos"][1]["id"] = duplicate_ids["vfos"][0]["id"].clone();

    let mut documents = vec![
        ("nine-vfos", nine_vfos),
        ("duplicate-vfo-id", duplicate_ids),
        (
            "zero-vfo-id",
            changed(&base, &["vfos", "0", "id"], json!(0)),
        ),
        (
            "max-vfo-id",
            changed(&base, &["vfos", "0", "id"], json!(usize::MAX)),
        ),
        // `max id + 1` is the server/UI's next VFO id. `usize::MAX - 1` leaves no
        // safe increment, so the session must be rejected before it is applied.
        (
            "near-max-vfo-id",
            changed(&base, &["vfos", "0", "id"], json!(usize::MAX - 1)),
        ),
        (
            "display-order",
            changed(&base, &["display", "min_db"], json!(-5.0)),
        ),
        (
            "averaging-low",
            changed(&base, &["display", "averaging"], json!(0)),
        ),
        (
            "averaging-high",
            changed(&base, &["display", "averaging"], json!(21)),
        ),
        (
            "waterfall-low",
            changed(&base, &["display", "waterfall_span_seconds"], json!(4.9)),
        ),
        (
            "waterfall-high",
            changed(&base, &["display", "waterfall_span_seconds"], json!(120.1)),
        ),
        (
            "scan-low",
            changed(&base, &["scan", "interval_ms"], json!(49)),
        ),
        (
            "scan-high",
            changed(&base, &["scan", "interval_ms"], json!(5001)),
        ),
        (
            "squelch-low",
            changed(&base, &["audio", "global_squelch_dbfs"], json!(-120.1)),
        ),
        (
            "squelch-high",
            changed(&base, &["audio", "global_squelch_dbfs"], json!(0.1)),
        ),
        (
            "agc-attack-low",
            changed(&base, &["audio", "agc", "attack_ms"], json!(0.9)),
        ),
        (
            "agc-attack-high",
            changed(&base, &["audio", "agc", "attack_ms"], json!(100.1)),
        ),
        (
            "agc-decay-low",
            changed(&base, &["audio", "agc", "decay_ms"], json!(49.9)),
        ),
        (
            "agc-decay-high",
            changed(&base, &["audio", "agc", "decay_ms"], json!(2000.1)),
        ),
        (
            "agc-target-low",
            changed(&base, &["audio", "agc", "target_level"], json!(0.049)),
        ),
        (
            "agc-target-high",
            changed(&base, &["audio", "agc", "target_level"], json!(1.01)),
        ),
        (
            "agc-gain-low",
            changed(&base, &["audio", "agc", "max_gain_db"], json!(0.9)),
        ),
        (
            "agc-gain-high",
            changed(&base, &["audio", "agc", "max_gain_db"], json!(100.1)),
        ),
        (
            "bandpass-low-range",
            changed(&base, &["audio", "bandpass", "low_hz"], json!(49.9)),
        ),
        (
            "bandpass-high-range",
            changed(&base, &["audio", "bandpass", "high_hz"], json!(8000.1)),
        ),
        (
            "bandpass-order",
            changed(&base, &["audio", "bandpass", "low_hz"], json!(4000.0)),
        ),
        (
            "receiver-frequency",
            changed(&base, &["receiver", "center_frequency_hz"], json!(0.0)),
        ),
        (
            "cursor-frequency",
            changed(&base, &["cursor", "frequency_hz"], json!(0.0)),
        ),
        (
            "vfo-frequency",
            changed(&base, &["vfos", "0", "frequency_hz"], json!(0.0)),
        ),
        (
            "cursor-bandwidth-low",
            changed(&base, &["cursor", "channel_bandwidth_hz"], json!(999.0)),
        ),
        (
            "cursor-bandwidth-high",
            changed(&base, &["cursor", "channel_bandwidth_hz"], json!(250_001.0)),
        ),
        (
            "vfo-bandwidth-low",
            changed(&base, &["vfos", "0", "channel_bandwidth_hz"], json!(999.0)),
        ),
        (
            "vfo-bandwidth-high",
            changed(
                &base,
                &["vfos", "0", "channel_bandwidth_hz"],
                json!(250_001.0),
            ),
        ),
    ];
    documents.extend(absurd_receiver_documents());
    documents
}

#[test]
fn invalid_session_values_are_rejected_table_driven() {
    let mut accepted = Vec::new();
    for (label, document) in invalid_documents() {
        let path = write_case(label, &document);
        let result = session::load(&path);
        let _ = fs::remove_file(&path);
        if result.is_ok() {
            accepted.push(label);
        }
    }

    let path = temp_path("nonfinite-squelch");
    let json = fs::read_to_string(BARAJAS_SESSION)
        .unwrap()
        .replace("-60.0", "1e400");
    fs::write(&path, json).unwrap();
    let result = session::load(&path);
    let _ = fs::remove_file(&path);
    if result.is_ok() {
        accepted.push("nonfinite-squelch");
    }

    assert!(
        accepted.is_empty(),
        "invalid session cases were accepted: {accepted:?}"
    );
}

#[test]
fn absurd_receiver_values_are_rejected_by_the_cli_before_startup_config_exists() {
    let mut accepted = Vec::new();
    for (label, document) in absurd_receiver_documents() {
        let path = write_case(label, &document);
        let args = CliArgs::parse_from([
            "termsdr",
            "--test",
            "--config",
            path.to_str().expect("invariant: temp paths are UTF-8"),
        ]);
        let result = args.to_config();
        let _ = fs::remove_file(&path);
        if result.is_ok() {
            accepted.push(label);
        }
    }

    assert!(
        accepted.is_empty(),
        "absurd receiver values produced a runnable AppConfig: {accepted:?}"
    );
}

#[test]
fn near_max_vfo_id_is_rejected_before_next_id_can_overflow() {
    let base: Value = serde_json::from_slice(&fs::read(BARAJAS_SESSION).unwrap()).unwrap();
    let path = write_case(
        "near-max-vfo-id-focused",
        &changed(&base, &["vfos", "0", "id"], json!(usize::MAX - 1)),
    );

    let result = session::load(&path);
    let _ = fs::remove_file(&path);

    assert!(
        result.is_err(),
        "VFO id usize::MAX - 1 must be rejected: max + 1 leaves next_vfo_id at usize::MAX, \
         so the next VFO add overflows"
    );
}

#[test]
fn absurd_receiver_values_are_rejected_by_the_session_loader() {
    let mut accepted = Vec::new();
    for (label, document) in absurd_receiver_documents() {
        let path = write_case(label, &document);
        let result = session::load(&path);
        let _ = fs::remove_file(&path);
        if result.is_ok() {
            accepted.push(label);
        }
    }

    assert!(
        accepted.is_empty(),
        "absurd receiver values survived session validation: {accepted:?}"
    );
}

#[test]
fn ordinary_documented_receiver_values_still_load() {
    let session = session::load(BARAJAS_SESSION).expect("the documented Barajas session must load");
    assert_eq!(session.receiver.center_frequency_hz, 118_522_500.0);
    assert_eq!(session.receiver.sample_rate_hz, 2_048_000.0);
    assert_eq!(session.receiver.fft_size, 4096);
    assert_eq!(session.receiver.gain_db, Some(28.0));
    assert_eq!(session.receiver.ppm, -1);

    let args = CliArgs::parse_from(["termsdr", "--test", "--config", BARAJAS_SESSION]);
    let config = args
        .to_config()
        .expect("ordinary documented receiver values must remain valid");
    assert_eq!(config.center_freq, 118_522_500.0);
    assert_eq!(config.sample_rate, 2_048_000.0);
    assert_eq!(config.fft_size, 4096);
    assert_eq!(config.gain, Some(28.0));
    assert_eq!(config.ppm, -1);
}

#[test]
fn rejected_session_never_partially_mutates_app_or_sends_audio() {
    for (label, document) in invalid_documents() {
        let path = write_case(label, &document);
        let (audio_tx, audio_rx) = unbounded();
        let mut app = App::new(AppConfig::default(), Some(audio_tx));
        app.cursor_freq = 101_700_000.0;
        app.volume = 37;
        app.squelch = Some(-42.0);
        let before = (
            app.config.center_freq,
            app.cursor_freq,
            app.volume,
            app.squelch,
        );

        palette_command(&mut app, &format!("config-load {}", path.display()));
        let _ = fs::remove_file(&path);

        assert_eq!(
            before,
            (
                app.config.center_freq,
                app.cursor_freq,
                app.volume,
                app.squelch,
            ),
            "invalid case {label} partially mutated App"
        );
        assert!(
            audio_rx.try_recv().is_err(),
            "invalid case {label} sent audio"
        );
        assert!(
            app.command_palette_active,
            "invalid case {label} hid its error"
        );
    }
}

#[test]
fn save_rejects_directory_destination_without_damaging_it_or_leaving_temp_files() {
    let root = temp_path("unsafe-destination-root");
    fs::create_dir_all(&root).unwrap();
    let sentinel = root.join("old-session");
    fs::write(&sentinel, b"preserve me").unwrap();
    let loaded = session::load(Path::new(BARAJAS_SESSION)).unwrap();

    assert!(session::save(&root, &loaded).is_err());
    assert_eq!(fs::read(&sentinel).unwrap(), b"preserve me");
    let names: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![sentinel.file_name().unwrap()]);

    fs::remove_dir_all(root).unwrap();
}
