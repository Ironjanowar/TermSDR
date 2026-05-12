use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use crossbeam_channel::{bounded, Receiver, Sender};
use crossterm::event::{self, Event};

use termsdr::audio::cpal_sink::CpalSink;
use termsdr::audio::{audio_thread, AudioControl, AudioSink, NullSink};
use termsdr::capture::rtlsdr::RtlSdrSource;
use termsdr::capture::test_signal::TestSource;
use termsdr::capture::Source;
use termsdr::config::CliArgs;
use termsdr::dsp::fft::FftProcessor;
use termsdr::types::{AppConfig, FftResult, IqSample, SourceType};
use termsdr::ui::{
    draw_ui, init_terminal, restore_terminal, show_source_selection, App, SourceSelection,
};

fn main() -> Result<()> {
    let args = CliArgs::parse();
    let mut config = args.to_config()?;

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
    run_pipeline(config)
}

fn run_pipeline(config: AppConfig) -> Result<()> {
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
        _ => None,
    };

    // Thread 1: Capture
    let running_capture = running.clone();
    let source_type = config.source.clone();
    let shared_freq_capture = shared_center_freq.clone();
    let capture_handle = thread::Builder::new()
        .name("capture".into())
        .spawn(move || {
            capture_thread(source_type, sample_rate, center_freq, fft_size, gain, ppm, iq_tx_dsp, iq_tx_audio, running_capture, shared_freq_capture)
        })?;

    // Thread 2: DSP
    let running_dsp = running.clone();
    let dsp_handle = thread::Builder::new()
        .name("dsp".into())
        .spawn(move || {
            dsp_thread(fft_size, sample_rate, center_freq, iq_rx_dsp, fft_tx, running_dsp)
        })?;

    // Thread 4: Audio
    let cursor_freq = config.center_freq; // cursor starts at center
    let channel_bw = 25_000.0;
    let audio_handle = thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            let sink: Box<dyn AudioSink> = match CpalSink::new(None) {
                Ok(s) => Box::new(s),
                Err(e) => {
                    eprintln!("Audio: no output device ({}), audio disabled", e);
                    Box::new(NullSink::new(48000))
                }
            };
            audio_thread(iq_rx_audio, audio_ctrl_rx, sink, center_freq, cursor_freq, channel_bw, sample_rate);
        })?;

    // Thread 3: TUI (main thread)
    let result = tui_thread(config, fft_rx, running.clone(), audio_ctrl_tx.clone(), shared_center_freq);

    // Signal shutdown
    running.store(false, Ordering::SeqCst);
    let _ = audio_ctrl_tx.send(AudioControl::Shutdown);

    // Wait for threads to join
    let _ = capture_handle.join();
    let _ = dsp_handle.join();
    let _ = audio_handle.join();

    result
}

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
            let mut src = TestSource::new(sample_rate, center_freq, signals, 0.02);
            if let Some(ref freq) = shared_center_freq {
                src = src.with_shared_freq(freq.clone());
            }
            Box::new(src)
        }
        SourceType::File { path: _ } => {
            eprintln!("File replay not yet implemented");
            return;
        }
        SourceType::RtlSdr { device_index } => {
            match RtlSdrSource::new(device_index, center_freq, sample_rate, gain, ppm) {
                Ok(src) => Box::new(src),
                Err(e) => {
                    eprintln!("RTL-SDR error: {}", e);
                    return;
                }
            }
        }
    };

    let block_size = fft_size;
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
                    eprintln!("Retune error: {}", e);
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
                eprintln!("Capture error: {}", e);
                break;
            }
        }

        // Pace based on sample rate to avoid flooding
        let elapsed = start.elapsed();
        if elapsed < block_duration {
            thread::sleep(block_duration - elapsed);
        }
    }
}

fn dsp_thread(
    fft_size: usize,
    sample_rate: f64,
    center_freq: f64,
    rx: Receiver<Vec<IqSample>>,
    tx: Sender<FftResult>,
    running: Arc<AtomicBool>,
) {
    let mut processor = FftProcessor::new(fft_size, 1);

    while running.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(samples) => {
                if let Some(result) = processor.process(&samples, center_freq, sample_rate) {
                    let _ = tx.try_send(result);
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn tui_thread(
    config: AppConfig,
    fft_rx: Receiver<FftResult>,
    running: Arc<AtomicBool>,
    audio_ctrl_tx: crossbeam_channel::Sender<AudioControl>,
    shared_center_freq: Option<Arc<AtomicU64>>,
) -> Result<()> {
    let mut terminal = init_terminal()?;
    let mut app = App::new(config, Some(audio_ctrl_tx));
    app.shared_center_freq = shared_center_freq;

    let target_frame_time = Duration::from_millis(33); // ~30 FPS

    while !app.should_quit && running.load(Ordering::Relaxed) {
        let frame_start = Instant::now();

        // Drain all available FFT results, keep only the latest
        while let Ok(fft) = fft_rx.try_recv() {
            app.push_fft(fft);
        }

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
