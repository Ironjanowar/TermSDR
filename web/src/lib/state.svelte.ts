// Reactive global SDR state — Svelte 5 $state runes

import type { StateMessage, AgcState, BandpassState, VfoState } from './protocol';

class SdrState {
  // Connection
  centerFreq = $state(100_000_000);
  sampleRate = $state(2_048_000);
  fftSize = $state(2048);
  gain = $state<number | null>(null);
  gainMode = $state('auto');

  // Tuning
  cursorFreq = $state(100_000_000);
  cursorActive = $state(true);
  demodMode = $state('AM');
  channelBw = $state(8000);

  // Audio
  volume = $state(80);
  squelch = $state(-60);
  muted = $state(false);
  audioFormat = $state('opus');

  // AGC
  agc = $state<AgcState>({
    enabled: true,
    attack_ms: 10,
    decay_ms: 300,
    target_level: 0.3,
    max_gain_db: 50,
  });

  // Bandpass
  bandpass = $state<BandpassState>({
    enabled: true,
    low_hz: 300,
    high_hz: 3400,
  });

  // VFOs
  vfos = $state<VfoState[]>([]);

  // Meta
  clientsConnected = $state(0);

  /** Apply a server state message to local state. */
  updateFromServer(msg: StateMessage) {
    this.centerFreq = msg.center_freq;
    this.sampleRate = msg.sample_rate;
    this.fftSize = msg.fft_size;
    this.gain = msg.gain;
    this.gainMode = msg.gain_mode;
    this.cursorFreq = msg.cursor_freq;
    this.cursorActive = msg.cursor_active;
    this.demodMode = msg.demod_mode;
    this.channelBw = msg.channel_bw;
    this.volume = msg.volume;
    this.squelch = msg.squelch;
    this.muted = msg.muted;
    this.agc = msg.agc;
    this.bandpass = msg.bandpass;
    this.vfos = msg.vfos;
    this.audioFormat = msg.audio_format;
    this.clientsConnected = msg.clients_connected;
  }
}

export const sdrState = new SdrState();
