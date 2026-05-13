//! WebSocket audio sink — streams PCM/Opus audio to connected WS clients.

use super::AudioSink;
use anyhow::Result;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// Audio format for WebSocket streaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WsAudioFormat {
    Opus = 0x00,
    PcmRaw = 0x01,
}

impl WsAudioFormat {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x00 => Some(Self::Opus),
            0x01 => Some(Self::PcmRaw),
            _ => None,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "opus" => Some(Self::Opus),
            "pcm" | "pcm_raw" | "raw" => Some(Self::PcmRaw),
            _ => None,
        }
    }
}

/// Tag byte for audio binary frames.
const TAG_AUDIO: u8 = 0x02;

/// Opus frame size: 20ms at 48kHz = 960 samples.
#[allow(dead_code)]
const OPUS_FRAME_SAMPLES: usize = 960;

/// WebSocket audio sink that sends audio frames to connected clients
/// via a crossbeam channel (bridging sync audio thread → async WS broadcast).
pub struct WebSocketSink {
    /// Channel to send encoded audio frames to the WS broadcast task.
    tx: crossbeam_channel::Sender<Vec<u8>>,
    /// Current audio format (switchable at runtime).
    format: Arc<AtomicU8>,
    /// Sample rate (always 48000 for WS streaming).
    sample_rate: u32,
    /// Buffer for accumulating samples (used for Opus framing).
    buffer: Vec<f32>,
    /// Opus encoder (when feature enabled).
    #[cfg(feature = "opus")]
    encoder: Option<opus::Encoder>,
}

impl WebSocketSink {
    /// Create a new WebSocketSink.
    ///
    /// `tx` — sender side of the channel; the WS broadcast task reads from the receiver.
    /// `format` — shared atomic for runtime format switching.
    pub fn new(
        tx: crossbeam_channel::Sender<Vec<u8>>,
        format: Arc<AtomicU8>,
    ) -> Self {
        #[cfg(feature = "opus")]
        let encoder = {
            match opus::Encoder::new(48000, opus::Channels::Mono, opus::Application::Voip) {
                Ok(mut enc) => {
                    let _ = enc.set_bitrate(opus::Bitrate::Bits(32000));
                    Some(enc)
                }
                Err(e) => {
                    eprintln!("WS Audio: Failed to create Opus encoder: {}, falling back to PCM", e);
                    // Force PCM format
                    format.store(WsAudioFormat::PcmRaw as u8, Ordering::Relaxed);
                    None
                }
            }
        };

        Self {
            tx,
            format,
            sample_rate: 48000,
            buffer: Vec::with_capacity(OPUS_FRAME_SAMPLES * 2),
            #[cfg(feature = "opus")]
            encoder,
        }
    }

    /// Get the current format.
    pub fn current_format(&self) -> WsAudioFormat {
        WsAudioFormat::from_byte(self.format.load(Ordering::Relaxed))
            .unwrap_or(WsAudioFormat::PcmRaw)
    }

    /// Encode and send a PCM raw frame.
    fn send_pcm_frame(&self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        // Frame: [TAG_AUDIO][format: 0x01][f32 LE bytes...]
        let mut frame = Vec::with_capacity(2 + samples.len() * 4);
        frame.push(TAG_AUDIO);
        frame.push(WsAudioFormat::PcmRaw as u8);
        for &s in samples {
            frame.extend_from_slice(&s.to_le_bytes());
        }
        let _ = self.tx.try_send(frame);
    }

    /// Encode and send an Opus frame.
    #[cfg(feature = "opus")]
    fn send_opus_frame(&mut self, samples: &[f32]) {
        if let Some(ref mut encoder) = self.encoder {
            let mut output = vec![0u8; 4000]; // max opus frame
            match encoder.encode_float(samples, &mut output) {
                Ok(len) => {
                    let mut frame = Vec::with_capacity(2 + len);
                    frame.push(TAG_AUDIO);
                    frame.push(WsAudioFormat::Opus as u8);
                    frame.extend_from_slice(&output[..len]);
                    let _ = self.tx.try_send(frame);
                }
                Err(e) => {
                    eprintln!("WS Audio: Opus encode error: {}", e);
                }
            }
        }
    }

    /// Process buffered samples — flush complete Opus frames or send PCM.
    fn flush_buffer(&mut self) {
        let fmt = self.current_format();
        match fmt {
            WsAudioFormat::PcmRaw => {
                if !self.buffer.is_empty() {
                    let samples: Vec<f32> = self.buffer.drain(..).collect();
                    self.send_pcm_frame(&samples);
                }
            }
            WsAudioFormat::Opus => {
                #[cfg(feature = "opus")]
                {
                    while self.buffer.len() >= OPUS_FRAME_SAMPLES {
                        let frame_samples: Vec<f32> =
                            self.buffer.drain(..OPUS_FRAME_SAMPLES).collect();
                        self.send_opus_frame(&frame_samples);
                    }
                }
                #[cfg(not(feature = "opus"))]
                {
                    // Opus not available, fall back to PCM
                    if !self.buffer.is_empty() {
                        let samples: Vec<f32> = self.buffer.drain(..).collect();
                        self.send_pcm_frame(&samples);
                    }
                }
            }
        }
    }
}

impl AudioSink for WebSocketSink {
    fn write(&mut self, samples: &[f32]) -> Result<()> {
        self.buffer.extend_from_slice(samples);
        self.flush_buffer();
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

// ============================================================================
// Audio frame encoding/decoding helpers (for protocol use)
// ============================================================================

/// Decode an audio binary frame.
/// Returns `(format_byte, audio_data)` or None if malformed.
pub fn decode_audio_frame(data: &[u8]) -> Option<(u8, &[u8])> {
    if data.len() < 2 {
        return None;
    }
    if data[0] != TAG_AUDIO {
        return None;
    }
    Some((data[1], &data[2..]))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sink() -> (WebSocketSink, crossbeam_channel::Receiver<Vec<u8>>, Arc<AtomicU8>) {
        let (tx, rx) = crossbeam_channel::bounded(100);
        let format = Arc::new(AtomicU8::new(WsAudioFormat::PcmRaw as u8));
        let sink = WebSocketSink::new(tx, format.clone());
        (sink, rx, format)
    }

    #[test]
    fn test_ws_sink_creation() {
        let (sink, _rx, _fmt) = make_sink();
        assert_eq!(sink.sample_rate(), 48000);
        assert_eq!(sink.current_format(), WsAudioFormat::PcmRaw);
    }

    #[test]
    fn test_ws_sink_sample_rate() {
        let (sink, _rx, _fmt) = make_sink();
        assert_eq!(sink.sample_rate(), 48000);
    }

    #[test]
    fn test_ws_sink_implements_audio_sink() {
        let (mut sink, _rx, _fmt) = make_sink();
        // Should compile and work via the trait
        let result = AudioSink::write(&mut sink, &[0.1, 0.2, 0.3]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_pcm_raw_frame_format() {
        let (mut sink, rx, _fmt) = make_sink();
        let samples = vec![1.0f32, -1.0, 0.5];
        sink.write(&samples).unwrap();

        let frame = rx.try_recv().unwrap();
        // Tag byte
        assert_eq!(frame[0], TAG_AUDIO);
        // Format byte: PCM raw = 0x01
        assert_eq!(frame[1], WsAudioFormat::PcmRaw as u8);
        assert_eq!(frame[1], 0x01);
        // Audio data: 3 f32s = 12 bytes
        assert_eq!(frame.len(), 2 + 3 * 4);

        // Verify actual float values
        let v0 = f32::from_le_bytes(frame[2..6].try_into().unwrap());
        let v1 = f32::from_le_bytes(frame[6..10].try_into().unwrap());
        let v2 = f32::from_le_bytes(frame[10..14].try_into().unwrap());
        assert!((v0 - 1.0).abs() < f32::EPSILON);
        assert!((v1 - (-1.0)).abs() < f32::EPSILON);
        assert!((v2 - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_pcm_raw_empty_write_no_frame() {
        let (mut sink, rx, _fmt) = make_sink();
        sink.write(&[]).unwrap();
        assert!(rx.try_recv().is_err(), "Empty write should not produce a frame");
    }

    #[test]
    fn test_pcm_raw_multiple_writes() {
        let (mut sink, rx, _fmt) = make_sink();
        sink.write(&[0.1, 0.2]).unwrap();
        sink.write(&[0.3]).unwrap();

        let frame1 = rx.try_recv().unwrap();
        let frame2 = rx.try_recv().unwrap();
        assert_eq!(frame1.len(), 2 + 2 * 4);
        assert_eq!(frame2.len(), 2 + 1 * 4);
    }

    #[test]
    fn test_format_switching_at_runtime() {
        let (mut sink, rx, fmt) = make_sink();

        // Start in PCM mode
        sink.write(&[1.0]).unwrap();
        let frame = rx.try_recv().unwrap();
        assert_eq!(frame[1], 0x01); // PCM

        // Switch to Opus (without opus feature, it falls back to PCM)
        fmt.store(WsAudioFormat::Opus as u8, Ordering::Relaxed);
        sink.write(&[1.0]).unwrap();
        let frame = rx.try_recv().unwrap();

        #[cfg(feature = "opus")]
        {
            // With opus, small writes get buffered until 960 samples
            // So this might not produce output yet
        }
        #[cfg(not(feature = "opus"))]
        {
            // Without opus, falls back to PCM
            assert_eq!(frame[1], 0x01);
        }

        // Switch back to PCM
        fmt.store(WsAudioFormat::PcmRaw as u8, Ordering::Relaxed);
        sink.write(&[0.5]).unwrap();
        let frame = rx.try_recv().unwrap();
        assert_eq!(frame[1], 0x01);
    }

    #[test]
    fn test_ws_audio_format_from_byte() {
        assert_eq!(WsAudioFormat::from_byte(0x00), Some(WsAudioFormat::Opus));
        assert_eq!(WsAudioFormat::from_byte(0x01), Some(WsAudioFormat::PcmRaw));
        assert_eq!(WsAudioFormat::from_byte(0x02), None);
        assert_eq!(WsAudioFormat::from_byte(0xFF), None);
    }

    #[test]
    fn test_ws_audio_format_from_str() {
        assert_eq!(WsAudioFormat::parse("opus"), Some(WsAudioFormat::Opus));
        assert_eq!(WsAudioFormat::parse("OPUS"), Some(WsAudioFormat::Opus));
        assert_eq!(WsAudioFormat::parse("pcm"), Some(WsAudioFormat::PcmRaw));
        assert_eq!(WsAudioFormat::parse("pcm_raw"), Some(WsAudioFormat::PcmRaw));
        assert_eq!(WsAudioFormat::parse("raw"), Some(WsAudioFormat::PcmRaw));
        assert_eq!(WsAudioFormat::parse("invalid"), None);
    }

    #[test]
    fn test_decode_audio_frame() {
        let frame = vec![0x02, 0x01, 0xAA, 0xBB];
        let (fmt, data) = decode_audio_frame(&frame).unwrap();
        assert_eq!(fmt, 0x01);
        assert_eq!(data, &[0xAA, 0xBB]);
    }

    #[test]
    fn test_decode_audio_frame_wrong_tag() {
        let frame = vec![0x01, 0x01, 0xAA];
        assert!(decode_audio_frame(&frame).is_none());
    }

    #[test]
    fn test_decode_audio_frame_too_short() {
        assert!(decode_audio_frame(&[]).is_none());
        assert!(decode_audio_frame(&[0x02]).is_none());
    }

    #[test]
    fn test_decode_audio_frame_empty_payload() {
        let frame = vec![0x02, 0x00];
        let (fmt, data) = decode_audio_frame(&frame).unwrap();
        assert_eq!(fmt, 0x00);
        assert!(data.is_empty());
    }

    #[cfg(feature = "opus")]
    #[test]
    fn test_opus_buffering_under_frame_size() {
        let (tx, rx) = crossbeam_channel::bounded(100);
        let format = Arc::new(AtomicU8::new(WsAudioFormat::Opus as u8));
        let mut sink = WebSocketSink::new(tx, format);

        // Send fewer than 960 samples — should NOT produce output
        let samples = vec![0.0f32; 500];
        sink.write(&samples).unwrap();
        assert!(rx.try_recv().is_err(), "Under 960 samples should not produce opus frame");
    }

    #[cfg(feature = "opus")]
    #[test]
    fn test_opus_frame_produced_at_960_samples() {
        let (tx, rx) = crossbeam_channel::bounded(100);
        let format = Arc::new(AtomicU8::new(WsAudioFormat::Opus as u8));
        let mut sink = WebSocketSink::new(tx, format);

        // Send exactly 960 samples
        let samples = vec![0.1f32; 960];
        sink.write(&samples).unwrap();

        let frame = rx.try_recv().unwrap();
        assert_eq!(frame[0], TAG_AUDIO);
        assert_eq!(frame[1], 0x00); // Opus format
        // Opus compressed output should be smaller than raw PCM (960 * 4 = 3840 bytes)
        assert!(frame.len() < 3840, "Opus frame should be smaller than raw PCM");
        assert!(frame.len() > 2, "Opus frame should have some data");
    }

    #[cfg(feature = "opus")]
    #[test]
    fn test_opus_multiple_frames() {
        let (tx, rx) = crossbeam_channel::bounded(100);
        let format = Arc::new(AtomicU8::new(WsAudioFormat::Opus as u8));
        let mut sink = WebSocketSink::new(tx, format);

        // Send 2000 samples — should produce 2 opus frames (960 each), 80 remain buffered
        let samples = vec![0.1f32; 2000];
        sink.write(&samples).unwrap();

        let frame1 = rx.try_recv().unwrap();
        let frame2 = rx.try_recv().unwrap();
        assert_eq!(frame1[0], TAG_AUDIO);
        assert_eq!(frame2[0], TAG_AUDIO);
        assert!(rx.try_recv().is_err(), "Should only produce 2 frames from 2000 samples");
    }

    #[test]
    fn test_large_pcm_write() {
        let (mut sink, rx, _fmt) = make_sink();
        let samples = vec![0.5f32; 4800]; // 100ms of audio at 48kHz
        sink.write(&samples).unwrap();

        let frame = rx.try_recv().unwrap();
        assert_eq!(frame[0], TAG_AUDIO);
        assert_eq!(frame[1], 0x01);
        assert_eq!(frame.len(), 2 + 4800 * 4);
    }

    #[test]
    fn test_sink_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<WebSocketSink>();
    }

    #[test]
    fn test_channel_full_drops_frames() {
        // Bounded channel of size 2 — third write should silently drop
        let (tx, rx) = crossbeam_channel::bounded(2);
        let format = Arc::new(AtomicU8::new(WsAudioFormat::PcmRaw as u8));
        let mut sink = WebSocketSink::new(tx, format);

        // Fill the channel
        sink.write(&[1.0]).unwrap();
        sink.write(&[2.0]).unwrap();
        // This should NOT block or error — just drop
        sink.write(&[3.0]).unwrap();

        // Only 2 frames in channel
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_format_switch_flushes_buffer_as_pcm() {
        // Start in Opus mode (without feature), accumulate samples,
        // then switch to PCM — buffered samples should flush
        let (tx, rx) = crossbeam_channel::bounded(100);
        let format = Arc::new(AtomicU8::new(WsAudioFormat::Opus as u8));
        let mut sink = WebSocketSink::new(tx, format.clone());

        // Write some samples in "Opus" mode
        // Without opus feature, this falls back to PCM immediately
        sink.write(&[0.5; 100]).unwrap();

        #[cfg(not(feature = "opus"))]
        {
            // Without opus, samples are sent as PCM fallback immediately
            let frame = rx.try_recv().unwrap();
            assert_eq!(frame[0], TAG_AUDIO);
            assert_eq!(frame[1], 0x01); // PCM fallback
        }

        // Switch to PCM explicitly
        format.store(WsAudioFormat::PcmRaw as u8, Ordering::Relaxed);
        sink.write(&[0.3; 50]).unwrap();
        let frame = rx.try_recv().unwrap();
        assert_eq!(frame[1], 0x01); // PCM
    }

    #[test]
    fn test_decode_audio_frame_opus_format() {
        let frame = vec![0x02, 0x00, 0xDE, 0xAD, 0xBE, 0xEF];
        let (fmt, data) = decode_audio_frame(&frame).unwrap();
        assert_eq!(fmt, 0x00); // Opus
        assert_eq!(data, &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[cfg(feature = "opus")]
    #[test]
    fn test_opus_format_switch_mid_buffer() {
        // Accumulate 500 samples in Opus mode, then switch to PCM
        // The 500 buffered samples should flush as PCM
        let (tx, rx) = crossbeam_channel::bounded(100);
        let format = Arc::new(AtomicU8::new(WsAudioFormat::Opus as u8));
        let mut sink = WebSocketSink::new(tx, format.clone());

        // Write 500 samples — not enough for an Opus frame (960)
        sink.write(&[0.1f32; 500]).unwrap();
        assert!(rx.try_recv().is_err(), "500 samples should be buffered, not sent");

        // Switch to PCM
        format.store(WsAudioFormat::PcmRaw as u8, Ordering::Relaxed);
        // Next write should flush buffer + new samples as PCM
        sink.write(&[0.2f32; 10]).unwrap();
        let frame = rx.try_recv().unwrap();
        assert_eq!(frame[0], TAG_AUDIO);
        assert_eq!(frame[1], 0x01); // PCM
        // Should contain 510 samples (500 buffered + 10 new)
        assert_eq!(frame.len(), 2 + 510 * 4);
    }
}
