//! Reviewer-blocker regression: startup must resolve the native session exactly
//! once, before any worker thread starts.
//!
//! The current startup path loads the session twice: `CliArgs::to_config` reads
//! it to build the receiver baseline, then `run_pipeline` re-reads the path after
//! the capture/DSP/audio threads have already been spawned. If the file changes
//! or disappears between those reads, startup fails with workers already running
//! (a headless server that never exits).
//!
//! This file is an **API RED**: it calls the intended single-resolution contract,
//! `CliArgs::resolve_startup`, which does not exist yet. The contract is:
//!
//! ```text
//! pub fn resolve_startup(&self)
//!     -> anyhow::Result<(AppConfig, Option<crate::session::Session>)>
//! ```
//!
//! It must parse `--config`/`--config-file` at most once, return both the
//! receiver baseline (with only explicit CLI flags applied) and the parsed
//! session snapshot, and be called by `main` before `run_pipeline`/worker
//! startup. No server or thread is started by this test.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use clap::Parser;
use termsdr::config::CliArgs;
use termsdr::types::SourceType;

const BARAJAS_SESSION: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/barajas-session-v1.json"
);

static NEXT_FILE: AtomicUsize = AtomicUsize::new(0);

fn copy_fixture(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "termsdr-startup-session-{label}-{}-{}.json",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::copy(BARAJAS_SESSION, &path).expect("copy the Barajas fixture");
    path
}

#[test]
fn startup_resolves_the_session_once_and_returns_both_snapshots() {
    let path = copy_fixture("once");
    let path_arg = path.to_str().expect("invariant: temp paths are UTF-8");
    let args = CliArgs::parse_from([
        "termsdr",
        "--test",
        "--config",
        path_arg,
        "--frequency",
        "119M",
    ]);

    let (config, session) = args
        .resolve_startup()
        .expect("the session must resolve before workers start");
    let session = session.expect("--config must produce a parsed startup session");

    // Explicit CLI receiver flags override only their own field.
    assert_eq!(config.center_freq, 119_000_000.0);
    assert_eq!(config.sample_rate, 2_048_000.0);
    assert_eq!(config.fft_size, 4096);
    assert_eq!(config.gain, Some(28.0));
    assert_eq!(config.ppm, -1);
    assert!(matches!(config.source, SourceType::Test { .. }));

    // The session snapshot is returned intact, independent of the override.
    assert_eq!(session.receiver.center_frequency_hz, 118_522_500.0);
    assert_eq!(session.cursor.frequency_hz, 118_070_000.0);
    assert_eq!(session.vfos.len(), 3);

    // Startup consumers receive owned snapshots, so a later path change cannot
    // force (or fail) a second read after workers are running.
    fs::write(&path, b"{ not a session any more }").expect("invalidate the source path");
    assert_eq!(session.receiver.fft_size, 4096);
    assert_eq!(config.fft_size, 4096);

    let _ = fs::remove_file(&path);
}

#[test]
fn config_file_alias_resolves_the_same_single_snapshot() {
    let args = CliArgs::parse_from([
        "termsdr",
        "--test",
        "--config-file",
        BARAJAS_SESSION,
        "--gain",
        "31.5",
    ]);

    let (config, session) = args
        .resolve_startup()
        .expect("--config-file must resolve like --config");
    let session = session.expect("the aliased path must produce a session");

    assert_eq!(config.gain, Some(31.5));
    assert_eq!(config.center_freq, 118_522_500.0);
    assert_eq!(session.cursor.name, "TWR1");
}

#[test]
fn startup_without_config_resolves_defaults_and_no_session() {
    let args = CliArgs::parse_from(["termsdr", "--test"]);

    let (config, session) = args
        .resolve_startup()
        .expect("no session path is not an error");

    assert!(session.is_none(), "no --config must not invent a session");
    assert_eq!(config.center_freq, 100_000_000.0);
    assert_eq!(config.sample_rate, 2_048_000.0);
    assert_eq!(config.fft_size, 2048);
    assert_eq!(config.gain, None);
    assert_eq!(config.ppm, 0);
}
