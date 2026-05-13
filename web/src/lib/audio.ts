// TermSDR Web Audio player — PCM raw playback via ScriptProcessorNode
// TODO: Add Opus decoding support (raw Opus frames need a JS decoder or AudioWorklet)

const SAMPLE_RATE = 48000;
const BUFFER_SIZE = 4096; // ScriptProcessorNode buffer size
const JITTER_BUFFER_SIZE = 48000; // ~1s ring buffer capacity (samples)

export class AudioPlayer {
  private ctx: AudioContext | null = null;
  private gainNode: GainNode | null = null;
  private processor: ScriptProcessorNode | null = null;

  // Ring buffer for PCM samples
  private ringBuffer = new Float32Array(JITTER_BUFFER_SIZE);
  private writePos = 0;
  private readPos = 0;
  private bufferedSamples = 0;

  private _muted = false;
  private _volume = 0.8; // 0.0–1.0

  get suspended(): boolean {
    return this.ctx?.state === 'suspended';
  }

  get started(): boolean {
    return this.ctx !== null;
  }

  /** Initialize AudioContext and processing chain. */
  start() {
    if (this.ctx) return;

    this.ctx = new AudioContext({ sampleRate: SAMPLE_RATE });
    this.gainNode = this.ctx.createGain();
    this.gainNode.gain.value = this._muted ? 0 : this._volume;
    this.gainNode.connect(this.ctx.destination);

    // ScriptProcessorNode pulls samples from ring buffer
    this.processor = this.ctx.createScriptProcessor(BUFFER_SIZE, 0, 1);
    this.processor.onaudioprocess = (e) => {
      const output = e.outputBuffer.getChannelData(0);
      const len = output.length;

      for (let i = 0; i < len; i++) {
        if (this.bufferedSamples > 0) {
          output[i] = this.ringBuffer[this.readPos];
          this.readPos = (this.readPos + 1) % JITTER_BUFFER_SIZE;
          this.bufferedSamples--;
        } else {
          output[i] = 0; // underrun — silence
        }
      }
    };

    this.processor.connect(this.gainNode);
  }

  /** Resume AudioContext (needed for autoplay policy). */
  async resume() {
    if (this.ctx?.state === 'suspended') {
      await this.ctx.resume();
    }
  }

  /** Stop and clean up. */
  stop() {
    this.processor?.disconnect();
    this.gainNode?.disconnect();
    this.ctx?.close();
    this.processor = null;
    this.gainNode = null;
    this.ctx = null;
    this.writePos = 0;
    this.readPos = 0;
    this.bufferedSamples = 0;
  }

  /** Feed PCM f32 LE audio data into the jitter buffer. */
  feedPcm(data: Uint8Array) {
    // data is raw f32 LE bytes
    const numSamples = Math.floor(data.byteLength / 4);
    if (numSamples === 0) return;

    // Create a DataView to read f32 LE values
    const view = new DataView(data.buffer, data.byteOffset, data.byteLength);

    for (let i = 0; i < numSamples; i++) {
      if (this.bufferedSamples >= JITTER_BUFFER_SIZE) {
        // Buffer full — drop oldest samples (advance read pointer)
        this.readPos = (this.readPos + 1) % JITTER_BUFFER_SIZE;
        this.bufferedSamples--;
      }
      this.ringBuffer[this.writePos] = view.getFloat32(i * 4, true);
      this.writePos = (this.writePos + 1) % JITTER_BUFFER_SIZE;
      this.bufferedSamples++;
    }
  }

  /** Set volume (0–100 scale). */
  setVolume(v: number) {
    this._volume = Math.max(0, Math.min(1, v / 100));
    if (this.gainNode && !this._muted) {
      this.gainNode.gain.value = this._volume;
    }
  }

  /** Set muted state. */
  setMuted(m: boolean) {
    this._muted = m;
    if (this.gainNode) {
      this.gainNode.gain.value = m ? 0 : this._volume;
    }
  }
}
