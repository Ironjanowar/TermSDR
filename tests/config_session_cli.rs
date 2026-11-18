//! CLI contract for native session loading and explicit override precedence.

use clap::Parser;
use termsdr::capture::file::FileSource;
use termsdr::capture::Source;
use termsdr::config::CliArgs;
use termsdr::types::{AppConfig, SourceType};
use termsdr::ui::App;

const BARAJAS_SESSION: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/barajas-session-v1.json"
);

#[test]
fn config_and_config_file_are_equivalent_cli_spellings() {
    let canonical = CliArgs::parse_from(["termsdr", "--config", BARAJAS_SESSION, "--test"]);
    let alias = CliArgs::try_parse_from(["termsdr", "--config-file", BARAJAS_SESSION, "--test"])
        .expect("--config-file must alias --config");

    assert_eq!(canonical.config.as_deref(), Some(BARAJAS_SESSION));
    assert_eq!(alias.config.as_deref(), canonical.config.as_deref());
}

#[test]
fn config_is_the_baseline_when_receiver_flags_are_omitted() {
    let args = CliArgs::parse_from(["termsdr", "--config", BARAJAS_SESSION, "--test"]);
    let config = args.to_config().expect("Barajas session must load");

    assert_eq!(config.center_freq, 118_522_500.0);
    assert_eq!(config.sample_rate, 2_048_000.0);
    assert_eq!(config.fft_size, 4096);
    assert_eq!(config.gain, Some(28.0));
    assert_eq!(config.ppm, -1);
    assert!(matches!(config.source, SourceType::Test { .. }));
}

#[test]
fn only_explicit_receiver_flags_override_the_config_baseline() {
    let args = CliArgs::parse_from([
        "termsdr",
        "--config",
        BARAJAS_SESSION,
        "--test",
        "--frequency",
        "119M",
        "--gain",
        "31.5",
    ]);
    let config = args
        .to_config()
        .expect("config plus overrides must resolve");

    assert_eq!(config.center_freq, 119_000_000.0);
    assert_eq!(config.gain, Some(31.5));
    assert_eq!(config.sample_rate, 2_048_000.0);
    assert_eq!(config.fft_size, 4096);
    assert_eq!(config.ppm, -1);
}

#[test]
fn source_selection_remains_cli_owned_when_loading_a_session() {
    let args = CliArgs::try_parse_from([
        "termsdr",
        "--config-file",
        BARAJAS_SESSION,
        "--file",
        "recordings/tower.iq",
    ])
    .expect("--config-file must alias --config");
    let config = args
        .to_config()
        .expect("session must not select the source");

    assert!(matches!(
        config.source,
        SourceType::File { ref path } if path == "recordings/tower.iq"
    ));
}

#[test]
fn file_replay_metadata_overrides_session_receiver_tuning() {
    let root = std::env::temp_dir().join(format!(
        "termsdr-session-file-metadata-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let data_path = root.join("recording.sigmf-data");
    let meta_path = root.join("recording.sigmf-meta");
    std::fs::write(&data_path, [127_u8, 128]).unwrap();
    let metadata = serde_json::json!({
        "global": {
            "core:datatype": "cu8",
            "core:sample_rate": 1_024_000.0,
            "core:version": "1.2.0"
        },
        "captures": [{"core:sample_start": 0, "core:frequency": 123_450_000.0}],
        "annotations": []
    });
    std::fs::write(&meta_path, metadata.to_string()).unwrap();

    let args = CliArgs::parse_from([
        "termsdr",
        "--config",
        BARAJAS_SESSION,
        "--file",
        data_path.to_str().unwrap(),
    ]);
    let mut config = args.to_config().unwrap();
    assert_eq!(
        config.center_freq, 118_522_500.0,
        "session is the CLI baseline"
    );
    assert_eq!(
        config.sample_rate, 2_048_000.0,
        "session is the CLI baseline"
    );

    // This mirrors the finite file-open boundary in pipeline startup: recording
    // metadata is authoritative once the selected source is opened.
    let source = FileSource::new(data_path.to_str().unwrap(), None, None, None).unwrap();
    config.center_freq = source.center_freq();
    config.sample_rate = source.sample_rate();

    assert_eq!(config.center_freq, 123_450_000.0);
    assert_eq!(config.sample_rate, 1_024_000.0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn applying_startup_session_preserves_explicit_cli_and_file_receiver_authority() {
    let args = CliArgs::parse_from([
        "termsdr",
        "--test",
        "--config",
        BARAJAS_SESSION,
        "--frequency",
        "119M",
    ]);
    let config = args
        .to_config()
        .expect("session plus explicit override must resolve");
    let session = termsdr::session::load(BARAJAS_SESSION).unwrap();

    // Explicit CLI override is the resolved receiver authority; applying the
    // session's durable cursor/audio state must not undo it.
    let mut explicit = App::new(config.clone(), None);
    explicit.apply_startup_session(session.clone());
    assert_eq!(
        explicit.config.center_freq, 119_000_000.0,
        "startup session application overwrote the explicit --frequency override"
    );
    assert_eq!(explicit.cursor_freq, 118_070_000.0);

    // Recording metadata is equally authoritative for file replay: by the time
    // the UI starts, `run_pipeline` has already replaced receiver tuning with the
    // opened source's values, and applying the session must preserve them.
    let replay_config = AppConfig {
        center_freq: 123_450_000.0,
        sample_rate: 1_024_000.0,
        ..config
    };
    let mut replay = App::new(replay_config, None);
    replay.apply_startup_session(session);
    assert_eq!(
        replay.config.center_freq, 123_450_000.0,
        "startup session application overwrote recording center metadata"
    );
    assert_eq!(
        replay.config.sample_rate, 1_024_000.0,
        "startup session application overwrote recording sample-rate metadata"
    );
    assert_eq!(replay.cursor_freq, 118_070_000.0);
}
