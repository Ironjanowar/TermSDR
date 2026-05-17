//! `TermSDR` binary entry point.
//!
//! Parses CLI args, sets up capture/DSP/audio/UI threads, and starts the TUI
//! or WebSocket server in headless mode.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use crossbeam_channel::{bounded, Receiver, Sender};
use crossterm::event::{self, Event};

use termsdr::audio::cpal_sink::CpalSink;
use termsdr::audio::ws_sink::{WebSocketSink, WsAudioFormat};
use termsdr::audio::{audio_thread, AudioControl, AudioSink, NullSink};
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
fn run_pipeline(config: AppConfig, args: &CliArgs) -> Result<()> {
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

    // Thread 1: Capture
    let running_capture = running.clone();
    let source_type = config.source.clone();
    let shared_freq_capture = shared_center_freq.clone();
    let capture_handle = thread::Builder::new()
        .name("capture".into())
        .spawn(move || {
            capture_thread(source_type, sample_rate, center_freq, fft_size, gain, ppm, iq_tx_dsp, iq_tx_audio, running_capture, shared_freq_capture);
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

    let audio_handle = thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            let sink: Box<dyn AudioSink> = if is_headless {
                if let Some(tx) = ws_audio_tx {
                    Box::new(WebSocketSink::new(tx, format_atomic))
                } else {
                    Box::new(NullSink::new(AUDIO_OUTPUT_SAMPLE_RATE))
                }
            } else {
                match CpalSink::new(None) {
                    Ok(s) => Box::new(s),
                    Err(e) => {
                        tracing::warn!("Audio: no output device ({e}), audio disabled");
                        Box::new(NullSink::new(AUDIO_OUTPUT_SAMPLE_RATE))
                    }
                }
            };
            audio_thread(&iq_rx_audio, &audio_ctrl_rx, sink, center_freq, cursor_freq, channel_bw, sample_rate);
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

    // Signal shutdown
    running.store(false, Ordering::SeqCst);
    let _ = audio_ctrl_tx.send(AudioControl::Shutdown);

    // Wait for threads to join
    let _ = capture_handle.join();
    let _ = dsp_handle.join();
    let _ = audio_handle.join();

    result
}

#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)] // thread entry point takes ownership
fn capture_thread(
    source_type: SourceType,
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
    let mut source: Box<dyn Source> = match source_type {
        SourceType::Test { signals } => {
            let mut src = TestSource::new(sample_rate, center_freq, &signals, 0.02);
            if let Some(ref freq) = shared_center_freq {
                src = src.with_shared_freq(freq.clone());
            }
            Box::new(src)
        }
        SourceType::File { path: _ } => {
            tracing::warn!("File replay not yet implemented");
            return;
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
    let block_duration = Duration::from_secs_f64(block_size as f64 / sample_rate);
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
            Ok(_) => {
                // Send to DSP (non-blocking)
                let _ = tx_dsp.try_send(buffer.clone());
                // Send to Audio (non-blocking, tolerate drops)
                let _ = tx_audio.try_send(buffer.clone());
            }
            Err(e) => {
                tracing::error!("Capture error: {e}");
                break;
            }
        }

        // Pace based on sample rate to avoid flooding
        let elapsed = start.elapsed();
        if elapsed < block_duration {
            if let Some(remaining) = block_duration.checked_sub(elapsed) {
                thread::sleep(remaining);
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
