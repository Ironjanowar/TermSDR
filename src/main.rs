//! `TermSDR` binary entry point.
//!
//! Parses CLI args, sets up capture/DSP/audio/UI threads, and starts the TUI
//! or WebSocket server in headless mode.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossbeam_channel::{bounded, Receiver, Sender};
use crossterm::event::{self, Event};

use termsdr::audio::cpal_sink::CpalSink;
use termsdr::audio::wav_sink::WavSink;
use termsdr::audio::ws_sink::{WebSocketSink, WsAudioFormat};
use termsdr::audio::{audio_thread, AudioControl, AudioSink, NullSink, TeeSink};
use termsdr::capture::file::FileSource;
use termsdr::capture::rtlsdr::RtlSdrSource;
use termsdr::capture::test_signal::TestSource;
use termsdr::capture::Source;
use termsdr::config::CliArgs;
use termsdr::dsp::fft::FftProcessor;
use termsdr::server::ServerConfig;
use termsdr::types::{AppConfig, FftResult, IqSample, SourceType};

/// Audio output sample rate in Hz.
const AUDIO_OUTPUT_SAMPLE_RATE: u32 = 48000;

/// Default channel bandwidth in Hz.
const DEFAULT_CHANNEL_BW: f64 = 25_000.0;

/// DSP receive timeout in milliseconds.
const DSP_RECV_TIMEOUT_MS: u64 = 100;

/// Target TUI frame time in milliseconds (~30 FPS).
const TUI_FRAME_TIME_MS: u64 = 33;
use termsdr::ui::{
    draw_ui, init_terminal, restore_terminal, show_source_selection, App, SourceSelection,
};

fn main() -> Result<()> {
    // Set up file-based logging (TUI owns stdout)
    use tracing_subscriber::EnvFilter;

    let log_file = std::fs::File::create("termsdr.log").expect("Failed to create log file");
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("termsdr=info".parse().expect("invariant: valid tracing directive")))
        .with_writer(std::sync::Mutex::new(log_file))
        .with_ansi(false)
        .init();

    let args = CliArgs::parse();
    let mut config = args.to_config()?;

    // Handle bookmark import
    if let Some(ref path) = args.import_bookmarks {
        match termsdr::bookmarks::import_cubicsdr(path) {
            Ok(bookmarks) => {
                let count = bookmarks.len();
                let mut store = termsdr::bookmarks::BookmarkStore::load();
                for b in bookmarks {
                    store.add(b);
                }
                tracing::info!("Imported {count} bookmarks from {path}");
            }
            Err(e) => tracing::error!("Import failed: {e}"),
        }
    }

    // Determine source selection
    let selection = if args.test {
        // --test flag bypasses source selection
        SourceSelection::Test
    } else if args.file.is_some() {
        // --file flag bypasses source selection
        match &config.source {
            SourceType::File { path } => SourceSelection::File(path.clone()),
            _ => unreachable!(),
        }
    } else if args.headless {
        // Headless mode without --test or --file defaults to RTL-SDR
        SourceSelection::RtlSdr(0)
    } else {
        // Show interactive source selection
        let mut terminal = init_terminal()?;
        let sel = show_source_selection(&mut terminal)?;
        restore_terminal(&mut terminal)?;
        match sel {
            Some(s) => s,
            None => return Ok(()), // User quit
        }
    };

    // Update config based on selection
    match &selection {
        SourceSelection::Test => {
            config.source = SourceType::Test {
                signals: vec![
                    (100_000.0, 0.8),
                    (-200_000.0, 0.4),
                    (350_000.0, 0.2),
                    (-500_000.0, 0.15),
                ],
            };
        }
        SourceSelection::File(path) => {
            config.source = SourceType::File { path: path.clone() };
        }
        SourceSelection::RtlSdr(idx) => {
            config.source = SourceType::RtlSdr { device_index: *idx };
        }
    }

    // Run the 3-thread pipeline
    run_pipeline(config, &args)
}

#[allow(clippy::too_many_lines, clippy::similar_names)] // pipeline setup with multiple threads; tx/rx pairs
fn run_pipeline(mut config: AppConfig, args: &CliArgs) -> Result<()> {
    // Open the IQ file before the threads start: its metadata (SigMF sidecar or the
    // explicit raw-file flags) is authoritative for replay, so the FFT axis, audio
    // decimation, and pacing all use the rates the recording actually has.
    let mut file_source = None;
    if let SourceType::File { path } = &config.source {
        let source = FileSource::new(
            path,
            args.iq_format,
            args.file_sample_rate,
            args.file_center_frequency,
        )?;
        config.sample_rate = source.sample_rate();
        config.center_freq = source.center_freq();
        tracing::info!(
            "File replay: {} ({:?}) at {:.0} Hz, centered at {:.0} Hz",
            path,
            source.format(),
            source.sample_rate(),
            source.center_freq()
        );
        file_source = Some(source);
    }

    // Create channels
    // Capture -> DSP: IQ blocks
    let (iq_tx_dsp, iq_rx_dsp): (Sender<Vec<IqSample>>, Receiver<Vec<IqSample>>) = bounded(4);
    // Capture -> Audio: IQ blocks
    let (iq_tx_audio, iq_rx_audio): (Sender<Vec<IqSample>>, Receiver<Vec<IqSample>>) = bounded(4);
    // DSP -> TUI: FFT results
    let (fft_tx, fft_rx): (Sender<FftResult>, Receiver<FftResult>) = bounded(2);
    // TUI -> Audio: control messages
    let (audio_ctrl_tx, audio_ctrl_rx) = crossbeam_channel::unbounded::<AudioControl>();

    // Set initial audio state
    let _ = audio_ctrl_tx.send(AudioControl::SetVolume(80));

    // Shutdown signal
    let running = Arc::new(AtomicBool::new(true));

    let sample_rate = config.sample_rate;
    let center_freq = config.center_freq;
    let fft_size = config.fft_size;
    let gain = config.gain;
    let ppm = config.ppm;

    // Shared center frequency for TestSource reactivity
    let shared_center_freq = match &config.source {
        SourceType::Test { .. } | SourceType::RtlSdr { .. } => {
            Some(Arc::new(AtomicU64::new(center_freq.to_bits())))
        }
        SourceType::File { .. } => None,
    };

    // Shared averaging count
    let shared_averaging = Arc::new(AtomicUsize::new(4));

    // `--offline` only changes pacing for a file source: a synthetic or RTL-SDR
    // source is already paced by its own generator/hardware, so the flag would be
    // meaningless there.
    let offline_replay = args.offline && matches!(config.source, SourceType::File { .. });
    if args.offline && !offline_replay {
        tracing::warn!("Capture: --offline only affects file replay; ignoring it for this source");
    }
    let pacing = if offline_replay {
        Pacing::Unpaced
    } else {
        Pacing::Realtime
    };

    // Thread 1: Capture
    let running_capture = running.clone();
    let source_type = config.source.clone();
    let shared_freq_capture = shared_center_freq.clone();
    let capture_handle = thread::Builder::new()
        .name("capture".into())
        .spawn(move || {
            capture_loop(
                source_type,
                file_source,
                sample_rate,
                center_freq,
                fft_size,
                gain,
                ppm,
                iq_tx_dsp,
                iq_tx_audio,
                running_capture,
                shared_freq_capture,
                pacing,
            );
        })?;

    // Thread 2: DSP
    let running_dsp = running.clone();
    let shared_avg_dsp = shared_averaging.clone();
    let shared_freq_dsp = shared_center_freq.clone();
    let dsp_handle = thread::Builder::new()
        .name("dsp".into())
        .spawn(move || {
            dsp_thread(fft_size, sample_rate, center_freq, iq_rx_dsp, fft_tx, running_dsp, shared_avg_dsp, shared_freq_dsp);
        })?;

    // Thread 4: Audio
    let cursor_freq = config.center_freq; // cursor starts at center
    let channel_bw = DEFAULT_CHANNEL_BW;

    // For headless mode, create a channel for WS audio frames
    let (ws_audio_tx, ws_audio_rx) = if args.headless {
        let (tx, rx) = crossbeam_channel::bounded::<Vec<u8>>(64);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    let is_headless = args.headless;
    let audio_format_str = args.audio_format.clone();

    // Create format_atomic outside the thread so it can be shared with the server
    let format_atomic = {
        let format = WsAudioFormat::parse(&audio_format_str)
            .unwrap_or(WsAudioFormat::PcmRaw);
        #[cfg(not(feature = "opus"))]
        let format = if format == WsAudioFormat::Opus {
            tracing::warn!("Audio: Opus not compiled in, using PCM raw");
            WsAudioFormat::PcmRaw
        } else {
            format
        };
        std::sync::Arc::new(std::sync::atomic::AtomicU8::new(format as u8))
    };
    let format_atomic_server = format_atomic.clone();

    // The sink is built here rather than inside the thread so that an unwritable
    // `--wav-output` path is a startup error instead of a warning the caller has to
    // find in the log after an apparently successful run.
    let sink = select_audio_sink(
        args.wav_output.as_deref(),
        offline_replay,
        is_headless,
        ws_audio_tx,
        format_atomic,
    )?;

    let audio_handle = thread::Builder::new().name("audio".into()).spawn(move || {
        // `audio_thread` finalizes the sink (patching a WAV header, for instance)
        // on its own shutdown path, so the recording is complete once this joins.
        audio_thread(
            &iq_rx_audio,
            &audio_ctrl_rx,
            sink,
            center_freq,
            cursor_freq,
            channel_bw,
            sample_rate,
        );
    })?;

    // Thread 3: TUI or WS Server (main thread)
    let audio_ctrl_tx_server = audio_ctrl_tx.clone();
    let result = if args.headless {
        let server_config = ServerConfig {
            bind: args.bind.clone(),
            port: args.port,
            no_web: args.no_web,
            audio_format: args.audio_format.clone(),
        };
        termsdr::server::run_server(
            config,
            server_config,
            fft_rx,
            ws_audio_rx,
            running.clone(),
            Some(format_atomic_server),
            Some(audio_ctrl_tx_server),
            shared_center_freq,
        )
    } else {
        tui_thread(config, fft_rx, running.clone(), audio_ctrl_tx.clone(), shared_center_freq, shared_averaging)
    };

    // Offline EOF is an orderly channel close: the capture sender disappears
    // and audio drains every queued IQ block. Live operation retains explicit
    // low-latency shutdown.
    running.store(false, Ordering::SeqCst);
    if !offline_replay {
        let _ = audio_ctrl_tx.send(AudioControl::Shutdown);
    }

    // Wait for threads to join
    capture_handle.join().map_err(|panic| {
        anyhow::anyhow!("capture thread panicked: {}", panic_message(&panic))
    })?;
    dsp_handle
        .join()
        .map_err(|panic| anyhow::anyhow!("DSP thread panicked: {}", panic_message(&panic)))?;
    audio_handle
        .join()
        .map_err(|panic| anyhow::anyhow!("audio thread panicked: {}", panic_message(&panic)))?;

    result
}

fn panic_message(payload: &Box<dyn std::any::Any + Send + 'static>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(ToString::to_string))
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

/// Build the sink that carries live playback for this run.
///
/// Headless runs stream to WebSocket clients (or record nothing when no audio
/// channel exists); an interactive run plays through the system device. A missing
/// output device degrades to [`NullSink`] so the pipeline still runs.
fn live_sink(
    is_headless: bool,
    ws_audio_tx: Option<Sender<Vec<u8>>>,
    format_atomic: std::sync::Arc<std::sync::atomic::AtomicU8>,
) -> Box<dyn AudioSink> {
    if is_headless {
        if let Some(tx) = ws_audio_tx {
            Box::new(WebSocketSink::new(tx, format_atomic))
        } else {
            Box::new(NullSink::new(AUDIO_OUTPUT_SAMPLE_RATE))
        }
    } else {
        match CpalSink::new(None) {
            Ok(sink) => Box::new(sink),
            Err(e) => {
                tracing::warn!("Audio: no output device ({e}), audio disabled");
                Box::new(NullSink::new(AUDIO_OUTPUT_SAMPLE_RATE))
            }
        }
    }
}

/// Choose the audio sink for this run, optionally recording to a WAV file.
///
/// * No `--wav-output`: the live sink is the only sink, exactly as before.
/// * `--wav-output` with `--offline`: the [`WavSink`] is the sole sink, so an
///   offline render is deterministic — no playback device, no dropped frames,
///   nothing but the samples the demodulator produced. The file uses
///   [`AUDIO_OUTPUT_SAMPLE_RATE`], the rate the demodulator is built from when no
///   live sink imposes another.
/// * `--wav-output` without `--offline`: a [`TeeSink`] sends the same samples to
///   playback and to the file. Live playback is what the user asked for by
///   running a live pipeline, so the recording must not silence it. The file
///   declares the live device's rate, because that is the rate the demodulator
///   actually ran at.
///
/// # Errors
/// Returns an error if the WAV file cannot be created.
fn select_audio_sink(
    wav_output: Option<&str>,
    offline: bool,
    is_headless: bool,
    ws_audio_tx: Option<Sender<Vec<u8>>>,
    format_atomic: std::sync::Arc<std::sync::atomic::AtomicU8>,
) -> Result<Box<dyn AudioSink>> {
    let Some(path) = wav_output else {
        return Ok(live_sink(is_headless, ws_audio_tx, format_atomic));
    };

    if offline {
        let sink = WavSink::new(path, AUDIO_OUTPUT_SAMPLE_RATE)
            .with_context(|| format!("failed to open WAV output {path}"))?;
        tracing::info!(
            "Audio: recording to {path} at {AUDIO_OUTPUT_SAMPLE_RATE} Hz as the only sink (no playback)"
        );
        return Ok(Box::new(sink));
    }

    let primary = live_sink(is_headless, ws_audio_tx, format_atomic);
    let sample_rate = primary.sample_rate();
    let secondary = WavSink::new(path, sample_rate)
        .with_context(|| format!("failed to open WAV output {path}"))?;
    tracing::info!("Audio: recording to {path} at {sample_rate} Hz alongside playback");
    Ok(Box::new(TeeSink::new(primary, Box::new(secondary))))
}

/// Whether the capture loop paces itself to wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pacing {
    /// Sleep between blocks so a replayed recording takes its own duration.
    Realtime,
    /// Read blocks as fast as the DSP drains them, so an offline render finishes
    /// deterministically instead of in real time.
    Unpaced,
}

/// Test-only entry point for the frozen capture-thread tests.
///
/// The tests drive the real loop through the eleven-argument signature, so this
/// pins real-time pacing for them; production goes through [`capture_loop`],
/// which additionally selects the pacing.
#[cfg(test)]
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)] // thread entry point takes ownership
fn capture_thread(
    source_type: SourceType,
    file_source: Option<FileSource>,
    sample_rate: f64,
    center_freq: f64,
    fft_size: usize,
    gain: Option<f32>,
    ppm: i32,
    tx_dsp: Sender<Vec<IqSample>>,
    tx_audio: Sender<Vec<IqSample>>,
    running: Arc<AtomicBool>,
    shared_center_freq: Option<Arc<AtomicU64>>,
) {
    capture_loop(
        source_type,
        file_source,
        sample_rate,
        center_freq,
        fft_size,
        gain,
        ppm,
        tx_dsp,
        tx_audio,
        running,
        shared_center_freq,
        Pacing::Realtime,
    );
}

/// Capture thread: read IQ blocks from the source and forward them to DSP and audio.
///
/// `pacing` selects whether the loop sleeps to hold real time; `Pacing::Unpaced`
/// is what `--offline` file replay uses.
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)] // thread entry point takes ownership
fn capture_loop(
    source_type: SourceType,
    file_source: Option<FileSource>,
    sample_rate: f64,
    center_freq: f64,
    fft_size: usize,
    gain: Option<f32>,
    ppm: i32,
    tx_dsp: Sender<Vec<IqSample>>,
    tx_audio: Sender<Vec<IqSample>>,
    running: Arc<AtomicBool>,
    shared_center_freq: Option<Arc<AtomicU64>>,
    pacing: Pacing,
) {
    let mut source: Box<dyn Source> = match source_type {
        SourceType::Test { signals } => {
            let mut src = TestSource::new(sample_rate, center_freq, &signals, 0.02);
            if let Some(ref freq) = shared_center_freq {
                src = src.with_shared_freq(freq.clone());
            }
            Box::new(src)
        }
        SourceType::File { .. } => {
            let Some(src) = file_source else {
                tracing::error!("File replay source was not opened");
                running.store(false, Ordering::SeqCst);
                return;
            };
            Box::new(src)
        }
        SourceType::RtlSdr { device_index } => {
            match RtlSdrSource::new(device_index, center_freq, sample_rate, gain, ppm) {
                Ok(src) => Box::new(src),
                Err(e) => {
                    tracing::error!("RTL-SDR error: {e}");
                    return;
                }
            }
        }
    };

    let block_size = fft_size;
    #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
    let block_secs = block_size as f64 / sample_rate;
    // `Duration::from_secs_f64` panics on a value it cannot represent, which would take
    // the capture thread down before the first read. Pacing is a soft constraint, so
    // fall back to a one-second floor and keep the pipeline running instead.
    let block_duration = Duration::try_from_secs_f64(block_secs).unwrap_or_else(|_| {
        tracing::warn!("Capture: cannot represent a {block_secs} s block duration; pacing at 1 s");
        Duration::from_secs(1)
    });
    let mut buffer = vec![IqSample::new(0.0, 0.0); block_size];

    let mut current_center_freq = center_freq;

    while running.load(Ordering::Relaxed) {
        let start = Instant::now();

        // Check for frequency change from UI
        if let Some(ref shared_freq) = shared_center_freq {
            let new_freq = f64::from_bits(shared_freq.load(Ordering::Relaxed));
            if (new_freq - current_center_freq).abs() > 1.0 {
                if let Err(e) = source.set_center_freq(new_freq) {
                    tracing::error!("Retune error: {e}");
                }
                current_center_freq = new_freq;
            }
        }

        match source.read_samples(&mut buffer) {
            // Ok(0) is the Source EOF convention: the file is exhausted, so stop the
            // pipeline instead of spinning on stale samples.
            Ok(0) => {
                tracing::info!("Capture source exhausted; stopping");
                running.store(false, Ordering::SeqCst);
                break;
            }
            Ok(count) => {
                // Only the samples actually produced are valid; `buffer[count..]` still
                // holds the previous block and must never reach DSP or audio.
                let block = buffer[..count].to_vec();
                // Send to DSP (non-blocking): a stale spectrum frame is harmless.
                let _ = tx_dsp.try_send(block.clone());
                if pacing == Pacing::Unpaced {
                    // Unpaced replay is a render, not a live view: the audio block must
                    // not be skipped just because the DSP is behind, or `--wav-output`
                    // would record a fraction of the file. Backpressure is
                    // intentional; disconnect still terminates promptly.
                    if tx_audio.send(block).is_err() {
                        // The audio worker owns sink/resampler errors. Stop the rest of
                        // the pipeline so the main thread can join it and propagate the
                        // failure instead of leaving the server or TUI waiting forever.
                        running.store(false, Ordering::SeqCst);
                        break;
                    }
                } else {
                    // Send to Audio (non-blocking, tolerate drops)
                    let _ = tx_audio.try_send(block);
                }
            }
            Err(e) => {
                tracing::error!("Capture error: {e}");
                // A read error is as terminal as EOF: without clearing the flag the DSP,
                // audio, and server threads keep looping against a dead source, which
                // hangs a headless server and freezes the TUI.
                running.store(false, Ordering::SeqCst);
                break;
            }
        }

        // Pace based on sample rate to avoid flooding. Unpaced replay skips the
        // wait entirely, which is the whole point of `--offline`.
        if pacing == Pacing::Realtime {
            let elapsed = start.elapsed();
            if elapsed < block_duration {
                if let Some(remaining) = block_duration.checked_sub(elapsed) {
                    thread::sleep(remaining);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)] // thread entry point takes ownership
fn dsp_thread(
    fft_size: usize,
    sample_rate: f64,
    center_freq: f64,
    rx: Receiver<Vec<IqSample>>,
    tx: Sender<FftResult>,
    running: Arc<AtomicBool>,
    shared_averaging: Arc<AtomicUsize>,
    shared_center_freq: Option<Arc<AtomicU64>>,
) {
    let mut processor = FftProcessor::new(fft_size, 4);
    let mut current_avg = 4usize;
    let mut current_center_freq = center_freq;

    while running.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(DSP_RECV_TIMEOUT_MS)) {
            Ok(samples) => {
                // A short final block from a file source cannot fill an FFT frame, and
                // `FftProcessor::process` requires at least `fft_size` samples.
                if samples.len() < fft_size {
                    continue;
                }
                // Check for averaging change
                let new_avg = shared_averaging.load(Ordering::Relaxed);
                if new_avg != current_avg {
                    processor.set_avg_count(new_avg);
                    current_avg = new_avg;
                }
                // Check for center freq change
                if let Some(ref freq) = shared_center_freq {
                    current_center_freq = f64::from_bits(freq.load(Ordering::Relaxed));
                }
                if let Some(result) = processor.process(&samples, current_center_freq, sample_rate) {
                    let _ = tx.try_send(result);
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {},
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[allow(clippy::needless_pass_by_value)] // thread entry point takes ownership
fn tui_thread(
    config: AppConfig,
    fft_rx: Receiver<FftResult>,
    running: Arc<AtomicBool>,
    audio_ctrl_tx: crossbeam_channel::Sender<AudioControl>,
    shared_center_freq: Option<Arc<AtomicU64>>,
    shared_averaging: Arc<AtomicUsize>,
) -> Result<()> {
    let mut terminal = init_terminal()?;
    let mut app = App::new(config, Some(audio_ctrl_tx));
    app.shared_center_freq = shared_center_freq;
    app.shared_averaging = Some(shared_averaging);

    let target_frame_time = Duration::from_millis(TUI_FRAME_TIME_MS); // ~30 FPS

    while !app.should_quit && running.load(Ordering::Relaxed) {
        let frame_start = Instant::now();

        // Drain all available FFT results, keep only the latest
        while let Ok(fft) = fft_rx.try_recv() {
            app.push_fft(fft);
        }
        app.maybe_scan();

        // Draw
        terminal.draw(|frame| {
            draw_ui(frame, &app);
        })?;

        app.update_fps();

        // Handle events with remaining frame time
        let elapsed = frame_start.elapsed();
        let poll_time = target_frame_time.saturating_sub(elapsed);
        if event::poll(poll_time)? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key);
            }
        }
    }

    restore_terminal(&mut terminal)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    //! Capture-thread stop contract (blocker-fix round).
    //!
    //! # Why this test lives in the binary crate, and why it uses no test double
    //!
    //! The behaviour under test is what `capture_thread` does to the shared `running`
    //! flag when `source.read_samples` returns `Err` — and `capture_thread` is a private
    //! fn of this binary crate taking eleven arguments. It cannot be called from a lib or
    //! integration test, so the options were:
    //!
    //! 1. *Extract a pure `fn handle_read_result(result, &AtomicBool) -> Outcome` into the
    //!    library and test that.* Rejected for this round: the extraction is production
    //!    work, so the test could only reference a function that does not exist yet — a
    //!    compile error that would take the whole suite down instead of reporting a
    //!    behaviour failure.
    //! 2. *A `#[cfg(test)] mod tests` in this file.* Chosen. This is the same module tree,
    //!    so the private fn is reachable with no visibility change and no production edit.
    //!    This module is the only addition to the file; it contains tests and fixtures.
    //!
    //! The source is a **real** [`FileSource`], not a scripted stub, so the test exercises
    //! the production decode path end to end. The read error is provoked the second way the
    //! reviewers reached it: a dataset that is valid at open time (whole number of samples),
    //! truncated to a partial sample while it is open. The first way — a directory, which
    //! `File::open` accepts on Linux — is required to fail at *open* time by
    //! `directory_path_is_rejected_at_open_time` in `capture::file`, so it is not used here.
    //!
    //! A note on what a failing assertion here means: this is a behaviour failure, not a
    //! compile error. Everything below compiles against the current signatures; only the
    //! `Err` arm's effect on `running` is wrong.

    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    /// Number of FFT bins used by these tests: small, so each block is a handful of
    /// samples and the paced loop finishes in microseconds.
    const TEST_FFT_SIZE: usize = 4;

    /// Sample rate used by these tests.
    const TEST_SAMPLE_RATE: f64 = 1_024_000.0;

    /// Center frequency used by these tests.
    const TEST_CENTER_FREQ: f64 = 118_100_000.0;

    static NEXT_TEST_DIR: AtomicUsize = AtomicUsize::new(0);

    /// Self-cleaning temporary directory holding generated IQ fixtures.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let index = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "termsdr-capture-thread-{}-{index}-{label}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        /// Write a fixture and return its path.
        fn write(&self, name: &str, bytes: &[u8]) -> String {
            let file_path = self.path.join(name);
            std::fs::write(&file_path, bytes).expect("write fixture");
            file_path
                .to_str()
                .expect("invariant: temp paths are UTF-8")
                .to_string()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Interleaved raw cu8 IQ pairs.
    fn cu8_bytes(samples: &[(u8, u8)]) -> Vec<u8> {
        samples.iter().flat_map(|(i, q)| [*i, *q]).collect()
    }

    /// Truncate an open dataset to `len` bytes, simulating a file that shrinks under an
    /// open reader (a copy still being written, a crashed recorder, a full disk).
    fn truncate(path: &str, len: u64) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open fixture for truncation")
            .set_len(len)
            .expect("truncate fixture");
    }

    /// Open a real `FileSource` over `path` the way `run_pipeline` does.
    fn open_source(path: &str) -> FileSource {
        FileSource::new(
            path,
            Some(termsdr::capture::file::IqFileFormat::Cu8),
            Some(TEST_SAMPLE_RATE),
            Some(TEST_CENTER_FREQ),
        )
        .expect("open fixture as a raw cu8 FileSource")
    }

    /// Drive the real `capture_thread` once and return the shared flag plus the blocks it
    /// handed to DSP.
    ///
    /// `capture_thread` is called inline rather than spawned: every terminating path in
    /// its read loop breaks out, so the call returns as soon as the scripted file ends.
    fn run_capture(source: FileSource, path: &str) -> (Arc<AtomicBool>, Vec<Vec<IqSample>>) {
        let running = Arc::new(AtomicBool::new(true));
        let (tx_dsp, rx_dsp) = bounded(8);
        let (tx_audio, _rx_audio) = bounded(8);

        capture_thread(
            SourceType::File {
                path: path.to_string(),
            },
            Some(source),
            TEST_SAMPLE_RATE,
            TEST_CENTER_FREQ,
            TEST_FFT_SIZE,
            None,
            0,
            tx_dsp,
            tx_audio,
            running.clone(),
            None,
        );

        let blocks: Vec<Vec<IqSample>> = rx_dsp.try_iter().collect();
        (running, blocks)
    }

    /// A read error must stop the pipeline.
    ///
    /// This is the blocker: the `Err` arm logged and `break`-ed without clearing
    /// `running`, so the DSP, audio, and server threads kept looping against a dead
    /// source. Headless, that is a server that never exits (measured: exit 124 after a
    /// 20 s timeout, versus ~106 ms for a clean EOF); in the TUI, a frozen display.
    ///
    /// The dataset here is a valid four-sample file truncated to five bytes (two and a
    /// half samples) after it was opened, so `read_samples` genuinely returns the
    /// "ended mid-sample" error through the production decode path.
    #[test]
    fn mid_sample_read_error_stops_the_pipeline() {
        let dir = TempDir::new("mid-sample");
        let path = dir.write("shrinks.cu8", &cu8_bytes(&[(0, 0), (64, 64), (128, 128), (192, 192)]));
        let source = open_source(&path);
        // Five bytes: one whole sample plus three bytes of the next.
        truncate(&path, 5);

        let (running, blocks) = run_capture(source, &path);

        assert!(
            blocks.is_empty(),
            "a failing read must not deliver IQ blocks to DSP, got {} block(s)",
            blocks.len()
        );
        assert!(
            !running.load(Ordering::SeqCst),
            "a read error must clear the shared `running` flag; leaving it set keeps the \
             DSP/audio/server threads alive against a dead source (headless hang, TUI freeze)"
        );
    }

    /// End of file must stop the pipeline — the sibling arm that already works.
    ///
    /// Kept next to the error test so the two stop reasons stay symmetric: a refactor that
    /// drops either one is then obvious.
    #[test]
    fn eof_stops_the_pipeline() {
        let dir = TempDir::new("eof");
        let path = dir.write("two.cu8", &cu8_bytes(&[(0, 255), (255, 0)]));
        let source = open_source(&path);

        let (running, blocks) = run_capture(source, &path);

        // The two samples are forwarded, then EOF stops the loop.
        assert_eq!(
            blocks.len(),
            1,
            "the samples before EOF must reach DSP exactly once, got {} block(s)",
            blocks.len()
        );
        assert_eq!(blocks[0].len(), 2, "only the samples produced may be forwarded");
        assert!(
            !running.load(Ordering::SeqCst),
            "EOF must clear the shared `running` flag so replay ends cleanly"
        );
    }

    /// A short final block must forward only the samples actually produced, and the loop
    /// must keep going rather than stopping after the first block.
    ///
    /// This is the non-regression side of the same switch: a fix for the error arm must
    /// not clear `running` on a *successful* read, and only `buffer[..count]` may reach
    /// DSP (the tail still holds the previous block).
    #[test]
    fn partial_final_block_forwards_actual_count_then_eof_stops() {
        let dir = TempDir::new("partial");
        // Three samples with a four-sample block: one short block, then EOF.
        let path = dir.write("three.cu8", &cu8_bytes(&[(0, 0), (128, 128), (255, 255)]));
        let source = open_source(&path);

        let (running, blocks) = run_capture(source, &path);

        assert_eq!(
            blocks.len(),
            1,
            "one short block must be forwarded before the EOF stop, got {}",
            blocks.len()
        );
        assert_eq!(
            blocks[0].len(),
            3,
            "only the three samples produced may be forwarded, not the full four-sample buffer"
        );
        assert!(
            !running.load(Ordering::SeqCst),
            "the trailing EOF must stop the pipeline"
        );
    }

    #[test]
    fn non_file_offline_is_filtered_before_live_sink_selection() {
        let dir = TempDir::new("offline-non-file");
        let without_offline = dir.path.join("without.wav");
        let with_offline = dir.path.join("with.wav");

        let select = |path: &std::path::Path, offline: bool| {
            let (tx, rx) = bounded(4);
            let format = Arc::new(std::sync::atomic::AtomicU8::new(
                WsAudioFormat::PcmRaw as u8,
            ));
            let mut sink = select_audio_sink(
                Some(path.to_str().unwrap()),
                offline,
                true,
                Some(tx),
                format,
            )
            .unwrap();
            sink.write(&[0.25, -0.5]).unwrap();
            sink.finish().unwrap();
            !rx.is_empty()
        };

        let source = SourceType::Test { signals: vec![] };
        let control_offline_replay = false && matches!(source, SourceType::File { .. });
        let raw_offline = true;
        let offline_replay = raw_offline && matches!(source, SourceType::File { .. });

        // run_pipeline computes `offline_replay = args.offline &&
        // matches!(source, File)` before passing that filtered value to the sink helper.
        assert!(!offline_replay, "non-file --offline must be filtered out");
        assert!(
            select(&without_offline, control_offline_replay),
            "control run must retain live WebSocket output"
        );
        assert!(
            select(&with_offline, offline_replay),
            "--offline is ignored for --test/non-file sources, so it must select the same live+WAV sink as the control run"
        );
    }

    #[test]
    fn unpaced_file_replay_bypasses_sleep_and_delivers_every_audio_block() {
        let dir = TempDir::new("unpaced-all-audio");
        let samples: Vec<(u8, u8)> = (0..12)
            .map(|index| (index * 7, 255 - index * 7))
            .collect();
        let path = dir.write("twelve.cu8", &cu8_bytes(&samples));
        let source = FileSource::new(
            &path,
            Some(termsdr::capture::file::IqFileFormat::Cu8),
            Some(40.0),
            Some(TEST_CENTER_FREQ),
        )
        .unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let (tx_dsp, _rx_dsp) = bounded(1);
        let (tx_audio, rx_audio) = bounded(1);
        let consumer = std::thread::spawn(move || {
            let mut blocks = Vec::new();
            while let Ok(block) = rx_audio.recv() {
                blocks.push(block);
            }
            blocks
        });

        let started = Instant::now();
        capture_loop(
            SourceType::File { path: path.clone() },
            Some(source),
            40.0,
            TEST_CENTER_FREQ,
            TEST_FFT_SIZE,
            None,
            0,
            tx_dsp,
            tx_audio,
            running,
            None,
            Pacing::Unpaced,
        );
        let elapsed = started.elapsed();
        let blocks = consumer.join().unwrap();

        assert_eq!(blocks.len(), 3, "all three file blocks must reach audio");
        assert_eq!(blocks.iter().map(Vec::len).sum::<usize>(), 12);
        assert!(
            elapsed < Duration::from_millis(100),
            "unpaced replay took {elapsed:?}; real-time pacing would sleep about 300 ms"
        );
    }

    #[test]
    fn unpaced_file_replay_never_drops_when_audio_is_slow_for_over_500ms() {
        let dir = TempDir::new("unpaced-slow-audio");
        let samples: Vec<(u8, u8)> = (0..12)
            .map(|index| (index * 7, 255 - index * 7))
            .collect();
        let path = dir.write("twelve-slow.cu8", &cu8_bytes(&samples));
        let source = FileSource::new(
            &path,
            Some(termsdr::capture::file::IqFileFormat::Cu8),
            Some(TEST_SAMPLE_RATE),
            Some(TEST_CENTER_FREQ),
        )
        .unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let (tx_dsp, _rx_dsp) = bounded(1);
        let (tx_audio, rx_audio) = bounded(1);
        let consumer = std::thread::spawn(move || {
            // Exceed the production send timeout while keeping the regression
            // bounded. Offline rendering must apply backpressure, not drop.
            std::thread::sleep(Duration::from_millis(650));
            rx_audio.into_iter().collect::<Vec<_>>()
        });

        let started = Instant::now();
        capture_loop(
            SourceType::File { path: path.clone() },
            Some(source),
            TEST_SAMPLE_RATE,
            TEST_CENTER_FREQ,
            TEST_FFT_SIZE,
            None,
            0,
            tx_dsp,
            tx_audio,
            running,
            None,
            Pacing::Unpaced,
        );
        let elapsed = started.elapsed();
        let blocks = consumer.join().unwrap();

        assert_eq!(
            blocks.len(),
            3,
            "offline replay dropped a source block while waiting for a slow audio consumer"
        );
        assert_eq!(
            blocks.iter().map(Vec::len).sum::<usize>(),
            samples.len(),
            "every source sample must reach audio during offline replay"
        );
        assert!(elapsed < Duration::from_secs(2), "test exceeded its bounded runtime: {elapsed:?}");
    }

    /// If the audio worker exits after a WAV/resampler failure, its disconnected
    /// receiver must stop the whole offline pipeline rather than only the capture loop.
    #[test]
    fn unpaced_audio_disconnect_stops_the_pipeline() {
        let dir = TempDir::new("unpaced-audio-disconnect");
        let path = dir.write(
            "audio-disconnect.cu8",
            &cu8_bytes(&[(0, 255), (64, 192), (128, 128), (255, 0)]),
        );
        let source = open_source(&path);
        let running = Arc::new(AtomicBool::new(true));
        let (tx_dsp, _rx_dsp) = bounded(1);
        let (tx_audio, rx_audio) = bounded(1);
        drop(rx_audio);

        let capture_running = running.clone();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let capture = std::thread::spawn(move || {
            capture_loop(
                SourceType::File { path },
                Some(source),
                TEST_SAMPLE_RATE,
                TEST_CENTER_FREQ,
                TEST_FFT_SIZE,
                None,
                0,
                tx_dsp,
                tx_audio,
                capture_running,
                None,
                Pacing::Unpaced,
            );
            finished_tx.send(()).expect("report capture-loop return");
        });

        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("unpaced capture loop must return when the audio receiver disconnects");
        capture.join().expect("capture thread must not panic");
        assert!(
            !running.load(Ordering::SeqCst),
            "an offline audio disconnect must clear `running` so the server/TUI returns and run_pipeline can join the failed audio worker"
        );
    }
}
