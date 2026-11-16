//! Digital signal processing pipeline: FFT computation and spectral analysis.

pub mod fft;
pub mod fir;
pub mod resampler;

// TODO: DSP pipeline orchestration.
// Will chain: Source -> windowing -> FFT -> magnitude -> dB conversion -> output
