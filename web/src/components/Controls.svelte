<script lang="ts">
  import { sdrState } from '../lib/state.svelte';
  import { wsClient } from '../lib/ws.svelte';
  import FreqInput from './FreqInput.svelte';

  const demodModes = ['AM', 'FM', 'NFM'];

  function setFreq(hz: number) { wsClient.sendCommand({ cmd: 'set_frequency', value: hz }); }
  function setGain(e: Event) { wsClient.sendCommand({ cmd: 'set_gain', value: parseInt((e.target as HTMLInputElement).value) }); }
  function setVolume(e: Event) { wsClient.sendCommand({ cmd: 'set_volume', value: parseInt((e.target as HTMLInputElement).value) }); }
  function setDemod(mode: string) { wsClient.sendCommand({ cmd: 'set_demod', mode }); }

  function addVfo() {
    wsClient.sendCommand({
      cmd: 'vfo_add',
      freq: sdrState.cursorFreq,
      channel_bw: sdrState.channelBw,
      mode: sdrState.demodMode,
    });
  }

  function removeVfo(id: number) {
    wsClient.sendCommand({ cmd: 'vfo_remove', id });
  }
</script>

<div class="controls">
  <!-- Frequency -->
  <div class="control-group">
    <span class="control-label">FREQ</span>
    <FreqInput value={sdrState.cursorFreq} onSubmit={setFreq} />
  </div>

  <!-- Gain -->
  {#if sdrState.gain !== null}
    <div class="control-group">
      <span class="control-label">GAIN</span>
      <input type="range" class="slider" min="0" max="50" value={sdrState.gain} oninput={setGain} />
      <span class="control-value">{sdrState.gain} dB</span>
    </div>
  {/if}

  <!-- Volume -->
  <div class="control-group">
    <span class="control-label">VOL</span>
    <input type="range" class="slider" min="0" max="100" value={sdrState.volume} oninput={setVolume} />
    <span class="control-value">{sdrState.volume}%</span>
  </div>

  <!-- Demod -->
  <div class="control-group">
    <span class="control-label">MODE</span>
    <div class="segmented">
      {#each demodModes as mode}
        <button
          class="seg-btn"
          class:active={sdrState.demodMode === mode}
          onclick={() => setDemod(mode)}
        >{mode}</button>
      {/each}
    </div>
  </div>

  <!-- VFOs -->
  <div class="control-group vfo-group">
    <span class="control-label">VFO</span>
    <div class="vfo-chips">
      {#each sdrState.vfos as vfo}
        <span class="vfo-chip" style="border-color: {vfo.color}">
          <span class="vfo-dot" style="background: {vfo.color}"></span>
          <span class="vfo-id">{vfo.id}</span>
          <button class="vfo-remove" onclick={() => removeVfo(vfo.id)}>×</button>
        </span>
      {/each}
      <button class="vfo-add" onclick={addVfo} title="Add VFO at cursor">+</button>
    </div>
  </div>
</div>

<style>
  .controls {
    display: flex;
    align-items: center;
    gap: 1rem;
    padding: 0.5rem 1rem;
    height: 100%;
    font-family: 'JetBrains Mono', monospace;
    flex-wrap: wrap;
    overflow: hidden;
  }

  .control-group {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    flex-shrink: 0;
  }

  .control-label {
    color: #666;
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    min-width: 2.5rem;
  }

  .control-value {
    color: #aaa;
    font-size: 0.8rem;
    min-width: 3rem;
    text-align: right;
  }

  .slider {
    width: 80px;
    -webkit-appearance: none;
    appearance: none;
    height: 4px;
    background: #333;
    border-radius: 2px;
    outline: none;
  }

  .slider::-webkit-slider-thumb {
    -webkit-appearance: none;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: #00d4ff;
    cursor: pointer;
    border: none;
  }

  .slider::-moz-range-thumb {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: #00d4ff;
    cursor: pointer;
    border: none;
  }

  .segmented {
    display: flex;
    border: 1px solid #333;
    border-radius: 4px;
    overflow: hidden;
  }

  .seg-btn {
    background: #12122a;
    border: none;
    border-right: 1px solid #333;
    color: #888;
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.75rem;
    padding: 0.2rem 0.5rem;
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }

  .seg-btn:last-child { border-right: none; }

  .seg-btn.active {
    background: #00d4ff22;
    color: #00d4ff;
  }

  .seg-btn:hover:not(.active) {
    background: #1a1a3e;
    color: #ccc;
  }

  .vfo-group {
    flex-shrink: 1;
    min-width: 0;
  }

  .vfo-chips {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    flex-wrap: wrap;
  }

  .vfo-chip {
    display: flex;
    align-items: center;
    gap: 0.2rem;
    border: 1px solid;
    border-radius: 12px;
    padding: 0.1rem 0.4rem;
    font-size: 0.7rem;
    color: #ccc;
  }

  .vfo-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  .vfo-id {
    font-size: 0.7rem;
  }

  .vfo-remove {
    background: none;
    border: none;
    color: #888;
    cursor: pointer;
    font-size: 0.85rem;
    padding: 0;
    line-height: 1;
  }

  .vfo-remove:hover { color: #ff6b6b; }

  .vfo-add {
    background: #12122a;
    border: 1px solid #444;
    border-radius: 50%;
    width: 22px;
    height: 22px;
    color: #888;
    font-size: 0.9rem;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
  }

  .vfo-add:hover {
    border-color: #00d4ff;
    color: #00d4ff;
  }
</style>
