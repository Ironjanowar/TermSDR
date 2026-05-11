use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use crossbeam_channel::{bounded, Receiver, Sender};
use crossterm::event::{self, Event};

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
        SourceSelection::RtlSdr(_idx) => {
            config.source = SourceType::RtlSdr;
        }
    }

    // Run the 3-thread pipeline
    run_pipeline(config)
}

fn run_pipeline(config: AppConfig) -> Result<()> {
    // Create channels
    // Capture -> DSP: IQ blocks
    let (iq_tx, iq_rx): (Sender<Vec<IqSample>>, Receiver<Vec<IqSample>>) = bounded(4);
    // DSP -> TUI: FFT results
    let (fft_tx, fft_rx): (Sender<FftResult>, Receiver<FftResult>) = bounded(2);

    // Shutdown signal
    let running = Arc::new(AtomicBool::new(true));

    let sample_rate = config.sample_rate;
    let center_freq = config.center_freq;
    let fft_size = config.fft_size;

    // Thread 1: Capture
    let running_capture = running.clone();
    let source_type = config.source.clone();
    let capture_handle = thread::Builder::new()
        .name("capture".into())
        .spawn(move || {
            capture_thread(source_type, sample_rate, center_freq, fft_size, iq_tx, running_capture)
        })?;

    // Thread 2: DSP
    let running_dsp = running.clone();
    let dsp_handle = thread::Builder::new()
        .name("dsp".into())
        .spawn(move || {
            dsp_thread(fft_size, sample_rate, center_freq, iq_rx, fft_tx, running_dsp)
        })?;

    // Thread 3: TUI (main thread)
    let result = tui_thread(config, fft_rx, running.clone());

    // Signal shutdown
    running.store(false, Ordering::SeqCst);

    // Wait for threads to join
    let _ = capture_handle.join();
    let _ = dsp_handle.join();

    result
}

fn capture_thread(
    source_type: SourceType,
    sample_rate: f64,
    center_freq: f64,
    fft_size: usize,
    tx: Sender<Vec<IqSample>>,
    running: Arc<AtomicBool>,
) {
    let mut source: Box<dyn Source> = match source_type {
        SourceType::Test { signals } => {
            Box::new(TestSource::new(sample_rate, center_freq, signals, 0.02))
        }
        SourceType::File { path: _ } => {
            eprintln!("File replay not yet implemented");
            return;
        }
        SourceType::RtlSdr => {
            eprintln!("RTL-SDR not yet implemented");
            return;
        }
    };

    let block_size = fft_size;
    let block_duration = Duration::from_secs_f64(block_size as f64 / sample_rate);
    let mut buffer = vec![IqSample::new(0.0, 0.0); block_size];

    while running.load(Ordering::Relaxed) {
        let start = Instant::now();

        match source.read_samples(&mut buffer) {
            Ok(_) => {
                // Try to send; if full, drop the block (non-blocking)
                let _ = tx.try_send(buffer.clone());
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
) -> Result<()> {
    let mut terminal = init_terminal()?;
    let mut app = App::new(config);

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
