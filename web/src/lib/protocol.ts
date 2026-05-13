// TermSDR WebSocket Protocol — TypeScript types matching server's protocol.rs

// ============================================================================
// Binary frame constants
// ============================================================================

export const TAG_SPECTRUM = 0x01;
export const TAG_AUDIO = 0x02;

// ============================================================================
// Binary frame types
// ============================================================================

export interface SpectrumFrame {
  timestamp: number;
  numBins: number;
  data: Float32Array;
}

export interface AudioFrame {
  format: 'opus' | 'pcm';
  data: Uint8Array;
}

// ============================================================================
// JSON message types (server → client)
// ============================================================================

export interface AgcState {
  enabled: boolean;
  attack_ms: number;
  decay_ms: number;
  target_level: number;
  max_gain_db: number;
}

export interface BandpassState {
  enabled: boolean;
  low_hz: number;
  high_hz: number;
}

export interface VfoState {
  id: number;
  freq: number;
  channel_bw: number;
  mode: string;
  color: string;
}

export interface StateMessage {
  type: 'state';
  center_freq: number;
  sample_rate: number;
  fft_size: number;
  gain: number | null;
  gain_mode: string;
  cursor_freq: number;
  cursor_active: boolean;
  demod_mode: string;
  channel_bw: number;
  volume: number;
  squelch: number;
  muted: boolean;
  agc: AgcState;
  bandpass: BandpassState;
  vfos: VfoState[];
  audio_format: string;
  clients_connected: number;
}

export interface ErrorMessage {
  type: 'error';
  cmd: string;
  message: string;
}

export type ServerMessage = StateMessage | ErrorMessage;

// ============================================================================
// JSON command types (client → server)
// ============================================================================

export type Command =
  | { cmd: 'set_frequency'; value: number }
  | { cmd: 'tune'; delta: number }
  | { cmd: 'set_bandwidth'; value: number }
  | { cmd: 'zoom'; direction: 'in' | 'out' }
  | { cmd: 'set_volume'; value: number }
  | { cmd: 'set_squelch'; value: number }
  | { cmd: 'set_demod'; mode: string }
  | { cmd: 'set_channel_bw'; value: number }
  | { cmd: 'toggle_mute' }
  | { cmd: 'vfo_add'; freq: number; channel_bw: number; mode: string }
  | { cmd: 'vfo_remove'; id: number }
  | { cmd: 'vfo_clear' }
  | { cmd: 'set_agc'; enabled: boolean; attack_ms?: number; decay_ms?: number; target_level?: number; max_gain_db?: number }
  | { cmd: 'set_bandpass'; enabled: boolean; low_hz?: number; high_hz?: number }
  | { cmd: 'set_gain'; value: number }
  | { cmd: 'get_state' }
  | { cmd: 'set_audio_format'; format: 'opus' | 'pcm' };

// ============================================================================
// Binary frame parsers
// ============================================================================

export type BinaryFrame =
  | { type: 'spectrum'; frame: SpectrumFrame }
  | { type: 'audio'; frame: AudioFrame };

/**
 * Parse a binary WebSocket frame.
 * Format:
 *   Spectrum: [0x01][timestamp_ms: u64 LE][num_bins: u16 LE][f32 LE × N]
 *   Audio:    [0x02][opus or pcm bytes]
 */
export function parseBinaryFrame(data: ArrayBuffer): BinaryFrame | null {
  const view = new DataView(data);
  if (data.byteLength < 1) return null;

  const tag = view.getUint8(0);

  if (tag === TAG_SPECTRUM) {
    if (data.byteLength < 11) return null;
    const timestamp = Number(view.getBigUint64(1, true));
    const numBins = view.getUint16(9, true);
    const expectedLen = 11 + numBins * 4;
    if (data.byteLength < expectedLen) return null;

    // Direct typed array view — avoids per-element DataView reads (~60x faster)
    // Note: f32 LE matches platform endianness on all modern browsers (x86/ARM LE)
    const bins = new Float32Array(data.slice(11, 11 + numBins * 4));

    return {
      type: 'spectrum',
      frame: { timestamp, numBins, data: bins },
    };
  }

  if (tag === TAG_AUDIO) {
    if (data.byteLength < 2) return null;
    const formatByte = view.getUint8(1);
    return {
      type: 'audio',
      frame: {
        format: formatByte === 0x01 ? 'pcm' : 'opus',
        data: new Uint8Array(data.slice(2)), // copy to avoid shared backing buffer
      },
    };
  }

  return null;
}

/**
 * Encode a command as a JSON string for sending over WebSocket.
 */
export function encodeCommand(cmd: Command): string {
  return JSON.stringify(cmd);
}

/**
 * Parse a JSON text frame from the server.
 */
export function parseServerMessage(text: string): ServerMessage | null {
  try {
    const msg = JSON.parse(text);
    if (msg.type === 'state' || msg.type === 'error') {
      return msg as ServerMessage;
    }
    return null;
  } catch {
    return null;
  }
}
