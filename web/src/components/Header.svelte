<script lang="ts">
  import { sdrState } from '../lib/state.svelte';
  import { wsClient } from '../lib/ws.svelte';
  import AudioControl from './AudioControl.svelte';

  interface Props {
    audioSuspended?: boolean;
    onAudioResume?: () => void;
    settingsOpen?: boolean;
    onToggleSettings?: () => void;
  }

  let { audioSuspended = false, onAudioResume, settingsOpen = false, onToggleSettings }: Props = $props();

  const statusIcon = $derived(
    wsClient.status === 'connected' ? '🟢' :
    wsClient.status === 'connecting' ? '🟡' : '🔴'
  );

  const statusText = $derived(
    wsClient.status.charAt(0).toUpperCase() + wsClient.status.slice(1)
  );

  function formatFreq(hz: number): string {
    const mhz = hz / 1_000_000;
    // Format as XXX.XXX.XXX MHz
    const str = Math.round(hz).toString().padStart(9, '0');
    return `${str.slice(0, -6)}.${str.slice(-6, -3)}.${str.slice(-3)} MHz`;
  }

  function formatBw(hz: number): string {
    if (hz >= 1_000_000) return `${(hz / 1_000_000).toFixed(1)} MHz`;
    if (hz >= 1_000) return `${(hz / 1_000).toFixed(1)} kHz`;
    return `${hz} Hz`;
  }
</script>

<header class="header">
  <div class="header-left">
    <span class="logo">📻 TermSDR</span>
  </div>

  <div class="header-center">
    <span class="freq">{formatFreq(sdrState.cursorFreq)}</span>
    <span class="sep">|</span>
    <span class="info">{sdrState.demodMode}</span>
    <span class="sep">|</span>
    <span class="info">{formatBw(sdrState.channelBw)}</span>
    <span class="sep">|</span>
    <span class="info">BW {formatBw(sdrState.sampleRate)}</span>
  </div>

  <div class="header-right">
    <span class="status" title="{statusText}">
      {statusIcon}
      {#if sdrState.clientsConnected > 0}
        <span class="clients">{sdrState.clientsConnected}</span>
      {/if}
    </span>
    <AudioControl suspended={audioSuspended} onResume={onAudioResume} />
    <button class="settings-btn" class:active={settingsOpen} title="Settings" onclick={() => onToggleSettings?.()}>⚙️</button>
  </div>
</header>

<style>
  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 1rem;
    background: #1a1a2e;
    border-bottom: 1px solid #333;
    font-family: 'JetBrains Mono', 'Fira Code', monospace;
    color: #e0e0e0;
    gap: 1rem;
    flex-shrink: 0;
  }

  .logo {
    font-weight: bold;
    font-size: 1.1rem;
    color: #00d4ff;
  }

  .header-center {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex-wrap: wrap;
    justify-content: center;
  }

  .freq {
    font-size: 1.3rem;
    font-weight: bold;
    color: #00ff88;
    letter-spacing: 0.05em;
  }

  .sep {
    color: #555;
  }

  .info {
    color: #aaa;
    font-size: 0.9rem;
  }

  .header-right {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }

  .status {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.85rem;
  }

  .clients {
    color: #888;
    font-size: 0.8rem;
  }

  .settings-btn {
    background: none;
    border: 1px solid #444;
    border-radius: 4px;
    padding: 0.25rem 0.5rem;
    cursor: pointer;
    font-size: 1rem;
    color: #ccc;
    transition: border-color 0.2s;
  }

  .settings-btn:hover {
    border-color: #00d4ff;
  }

  .settings-btn.active {
    border-color: #00d4ff;
    background: #00d4ff22;
  }
</style>
