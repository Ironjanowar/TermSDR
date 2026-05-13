<script lang="ts">
  import { sdrState } from '../lib/state.svelte';
  import { wsClient } from '../lib/ws.svelte';
  import SettingsField from './SettingsField.svelte';
  import Toggle from './Toggle.svelte';

  interface Props {
    open: boolean;
    onClose: () => void;
  }

  let { open, onClose }: Props = $props();

  function handleOverlayClick(e: MouseEvent) {
    if ((e.target as HTMLElement).classList.contains('modal-overlay')) {
      onClose();
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') onClose();
  }

  // Command senders
  function setVolume(v: number) { wsClient.sendCommand({ cmd: 'set_volume', value: Math.round(v) }); }
  function setSquelch(v: number) { wsClient.sendCommand({ cmd: 'set_squelch', value: Math.round(v) }); }
  function setChannelBw(v: number) { wsClient.sendCommand({ cmd: 'set_channel_bw', value: Math.round(v) }); }
  function setDemod(mode: string) { wsClient.sendCommand({ cmd: 'set_demod', mode }); }
  function setGain(v: number) { wsClient.sendCommand({ cmd: 'set_gain', value: Math.round(v) }); }

  function setAgc(patch: Partial<{ enabled: boolean; attack_ms: number; decay_ms: number; target_level: number; max_gain_db: number }>) {
    wsClient.sendCommand({
      cmd: 'set_agc',
      enabled: patch.enabled ?? sdrState.agc.enabled,
      attack_ms: patch.attack_ms ?? sdrState.agc.attack_ms,
      decay_ms: patch.decay_ms ?? sdrState.agc.decay_ms,
      target_level: patch.target_level ?? sdrState.agc.target_level,
      max_gain_db: patch.max_gain_db ?? sdrState.agc.max_gain_db,
    });
  }

  function setBandpass(patch: Partial<{ enabled: boolean; low_hz: number; high_hz: number }>) {
    wsClient.sendCommand({
      cmd: 'set_bandpass',
      enabled: patch.enabled ?? sdrState.bandpass.enabled,
      low_hz: patch.low_hz ?? sdrState.bandpass.low_hz,
      high_hz: patch.high_hz ?? sdrState.bandpass.high_hz,
    });
  }

  function setAudioFormat(fmt: 'opus' | 'pcm') {
    wsClient.sendCommand({ cmd: 'set_audio_format', format: fmt });
  }

  const demodModes = ['AM', 'FM', 'NFM'];
</script>

<svelte:window onkeydown={handleKeydown} />

{#if open}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="modal-overlay" onclick={handleOverlayClick}>
    <div class="modal-panel">
      <div class="modal-header">
        <h2>⚙️ Settings</h2>
        <button class="close-btn" onclick={onClose}>✕</button>
      </div>

      <div class="modal-body">
        <!-- General -->
        <section class="section">
          <h3>General</h3>
          <SettingsField label="Volume" value={sdrState.volume} min={0} max={100} step={1} unit="%" onChange={setVolume} />
          <SettingsField label="Squelch" value={sdrState.squelch} min={-120} max={0} step={1} unit="dB" onChange={setSquelch} />
          <SettingsField label="Channel BW" value={sdrState.channelBw} min={1000} max={200000} step={1000} unit="Hz" onChange={setChannelBw} />
          {#if sdrState.gain !== null}
            <SettingsField label="Gain" value={sdrState.gain} min={0} max={50} step={1} unit="dB" onChange={setGain} />
          {/if}
          <div class="demod-row">
            <span class="field-label-inline">Demod</span>
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
        </section>

        <!-- AGC -->
        <section class="section">
          <h3>AGC</h3>
          <Toggle label="Enabled" checked={sdrState.agc.enabled} onChange={(v) => setAgc({ enabled: v })} />
          {#if sdrState.agc.enabled}
            <SettingsField label="Attack" value={sdrState.agc.attack_ms} min={1} max={100} step={1} unit="ms" onChange={(v) => setAgc({ attack_ms: v })} />
            <SettingsField label="Decay" value={sdrState.agc.decay_ms} min={50} max={2000} step={10} unit="ms" onChange={(v) => setAgc({ decay_ms: v })} />
            <SettingsField label="Target Level" value={sdrState.agc.target_level} min={0.05} max={1} step={0.05} unit="" onChange={(v) => setAgc({ target_level: v })} />
            <SettingsField label="Max Gain" value={sdrState.agc.max_gain_db} min={1} max={100} step={1} unit="dB" onChange={(v) => setAgc({ max_gain_db: v })} />
          {/if}
        </section>

        <!-- Audio Filter -->
        <section class="section">
          <h3>Audio Filter</h3>
          <Toggle label="Bandpass" checked={sdrState.bandpass.enabled} onChange={(v) => setBandpass({ enabled: v })} />
          {#if sdrState.bandpass.enabled}
            <SettingsField label="Low Cut" value={sdrState.bandpass.low_hz} min={50} max={1000} step={10} unit="Hz" onChange={(v) => setBandpass({ low_hz: v })} />
            <SettingsField label="High Cut" value={sdrState.bandpass.high_hz} min={1000} max={8000} step={100} unit="Hz" onChange={(v) => setBandpass({ high_hz: v })} />
          {/if}
        </section>

        <!-- Audio Format -->
        <section class="section">
          <h3>Audio Format</h3>
          <div class="segmented">
            <button class="seg-btn" class:active={sdrState.audioFormat === 'opus'} onclick={() => setAudioFormat('opus')}>Opus</button>
            <button class="seg-btn" class:active={sdrState.audioFormat === 'pcm'} onclick={() => setAudioFormat('pcm')}>PCM Raw</button>
          </div>
        </section>
      </div>
    </div>
  </div>
{/if}

<style>
  .modal-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    z-index: 1000;
    display: flex;
    justify-content: flex-end;
  }

  .modal-panel {
    width: 340px;
    max-width: 90vw;
    height: 100%;
    background: #1a1a2e;
    border-left: 1px solid #333;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    animation: slideIn 0.2s ease-out;
  }

  @keyframes slideIn {
    from { transform: translateX(100%); }
    to { transform: translateX(0); }
  }

  .modal-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 1rem;
    border-bottom: 1px solid #333;
  }

  .modal-header h2 {
    font-size: 1.1rem;
    color: #e0e0e0;
    margin: 0;
  }

  .close-btn {
    background: none;
    border: none;
    color: #888;
    font-size: 1.2rem;
    cursor: pointer;
    padding: 0.25rem;
  }

  .close-btn:hover {
    color: #fff;
  }

  .modal-body {
    padding: 1rem;
    display: flex;
    flex-direction: column;
    gap: 1.5rem;
  }

  .section {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .section h3 {
    color: #00d4ff;
    font-size: 0.85rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    margin: 0;
    padding-bottom: 0.25rem;
    border-bottom: 1px solid #2a2a3e;
  }

  .demod-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }

  .field-label-inline {
    color: #999;
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .segmented {
    display: flex;
    gap: 0;
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
    font-size: 0.8rem;
    padding: 0.35rem 0.75rem;
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }

  .seg-btn:last-child {
    border-right: none;
  }

  .seg-btn.active {
    background: #00d4ff22;
    color: #00d4ff;
  }

  .seg-btn:hover:not(.active) {
    background: #1a1a3e;
    color: #ccc;
  }
</style>
