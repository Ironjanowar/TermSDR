//! Integration test: TestSource -> FFT pipeline -> verify signal detection.

use termsdr::capture::test_signal::TestSource;
use termsdr::capture::Source;
use termsdr::dsp::fft::FftProcessor;
use termsdr::types::IqSample;

#[test]
fn test_pipeline_detects_signal_at_correct_frequency() {
    let fft_size = 2048;
    let sample_rate = 2_048_000.0;
    let center_freq = 100_000_000.0;
    let signal_offset = 100_000.0; // 100 kHz above center

    // Create a test source with a single signal
    let mut source = TestSource::new(
        sample_rate,
        center_freq,
        &[(signal_offset, 0.8)],
        0.01, // low noise
    );

    // Read samples
    let mut buffer = vec![IqSample::new(0.0, 0.0); fft_size];
    let n = source.read_samples(&mut buffer).unwrap();
    assert_eq!(n, fft_size);

    // Process through FFT
    let mut processor = FftProcessor::new(fft_size, 1);
    let result = processor
        .process(&buffer, center_freq, sample_rate)
        .expect("Should produce FFT result");

    // Verify FFT output size
    assert_eq!(result.bins.len(), fft_size);

    // Find the peak
    let (peak_bin, peak_db) = result.peak();

    // The signal should appear near the expected bin
    let expected_bin = result.frequency_to_bin(center_freq + signal_offset);
    assert!(
        (peak_bin as i32 - expected_bin as i32).unsigned_abs() <= 3,
        "Signal peak at bin {} but expected near bin {} for {}Hz offset",
        peak_bin,
        expected_bin,
        signal_offset
    );

    // Peak should be significantly above the noise floor
    let noise_bins: Vec<f32> = result
        .bins
        .iter()
        .enumerate()
        .filter(|(i, _)| (*i as i32 - peak_bin as i32).unsigned_abs() > 20)
        .map(|(_, &v)| v)
        .collect();
    let avg_noise: f32 = noise_bins.iter().sum::<f32>() / noise_bins.len() as f32;

    let snr = peak_db - avg_noise;
    assert!(
        snr > 20.0,
        "Signal-to-noise ratio too low: {} dB (peak={} dB, noise={} dB)",
        snr,
        peak_db,
        avg_noise
    );
}

#[test]
fn test_pipeline_detects_multiple_signals() {
    let fft_size = 4096;
    let sample_rate = 2_048_000.0;
    let center_freq = 100_000_000.0;

    let signals = vec![
        (200_000.0, 0.8),  // 200 kHz above center
        (-300_000.0, 0.6), // 300 kHz below center
    ];

    let mut source = TestSource::new(sample_rate, center_freq, &signals, 0.005);

    let mut buffer = vec![IqSample::new(0.0, 0.0); fft_size];
    source.read_samples(&mut buffer).unwrap();

    let mut processor = FftProcessor::new(fft_size, 1);
    let result = processor
        .process(&buffer, center_freq, sample_rate)
        .unwrap();

    // Check both signals are detectable
    for (offset, _amplitude) in &signals {
        let expected_bin = result.frequency_to_bin(center_freq + offset);
        // Check that there's a local peak near the expected bin
        let search_range = 5;
        let local_peak = (expected_bin.saturating_sub(search_range)
            ..=(expected_bin + search_range).min(result.bins.len() - 1))
            .max_by(|&a, &b| result.bins[a].partial_cmp(&result.bins[b]).unwrap())
            .unwrap();

        // The local peak should be well above the average
        let avg_db: f32 = result.bins.iter().sum::<f32>() / result.bins.len() as f32;
        assert!(
            result.bins[local_peak] > avg_db + 10.0,
            "Signal at offset {}Hz not detected (bin {} = {} dB, avg = {} dB)",
            offset,
            local_peak,
            result.bins[local_peak],
            avg_db
        );
    }
}

#[test]
fn test_pipeline_threads_start_stop() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;
    use crossbeam_channel::bounded;

    let running = Arc::new(AtomicBool::new(true));
    let (iq_tx, iq_rx) = bounded::<Vec<IqSample>>(4);
    let (fft_tx, fft_rx) = bounded::<termsdr::types::FftResult>(2);

    let fft_size = 1024;
    let sample_rate = 1_024_000.0;
    let center_freq = 100e6;

    // Capture thread
    let r1 = running.clone();
    let t1 = thread::spawn(move || {
        let mut source = TestSource::new(sample_rate, center_freq, &[(50_000.0, 0.5)], 0.01);
        let mut buf = vec![IqSample::new(0.0, 0.0); fft_size];
        while r1.load(Ordering::Relaxed) {
            source.read_samples(&mut buf).unwrap();
            if iq_tx.try_send(buf.clone()).is_err() {
                // Channel full or disconnected
            }
            thread::sleep(Duration::from_millis(10));
        }
    });

    // DSP thread
    let r2 = running.clone();
    let t2 = thread::spawn(move || {
        let mut processor = FftProcessor::new(fft_size, 1);
        while r2.load(Ordering::Relaxed) {
            match iq_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(samples) => {
                    if let Some(result) = processor.process(&samples, center_freq, sample_rate) {
                        let _ = fft_tx.try_send(result);
                    }
                }
                Err(_) => {}
            }
        }
    });

    // Let it run briefly
    thread::sleep(Duration::from_millis(200));

    // Should have some FFT results
    let mut count = 0;
    while let Ok(_result) = fft_rx.try_recv() {
        count += 1;
    }
    assert!(count > 0, "Pipeline should have produced FFT results, got {}", count);

    // Clean shutdown
    running.store(false, Ordering::SeqCst);
    t1.join().unwrap();
    t2.join().unwrap();
}

#[test]
fn test_source_trait_used_generically() {
    // Verify we can use TestSource through the Source trait
    fn process_source(source: &mut dyn Source, fft_size: usize) -> termsdr::types::FftResult {
        let mut buffer = vec![IqSample::new(0.0, 0.0); fft_size];
        source.read_samples(&mut buffer).unwrap();

        let mut processor = FftProcessor::new(fft_size, 1);
        processor
            .process(&buffer, source.center_freq(), source.sample_rate())
            .unwrap()
    }

    let mut source = TestSource::new(1_024_000.0, 100e6, &[(50_000.0, 0.7)], 0.01);
    let result = process_source(&mut source, 1024);
    assert_eq!(result.bins.len(), 1024);
    assert_eq!(result.sample_rate, 1_024_000.0);
    assert_eq!(result.center_freq, 100e6);
}
