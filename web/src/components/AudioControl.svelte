<script lang="ts">
  import { sdrState } from '../lib/state.svelte';
  import { wsClient } from '../lib/ws.svelte';

  interface Props {
    suspended?: boolean;
    onResume?: () => void;
  }

  let { suspended = false, onResume }: Props = $props();

  function toggleMute() {
    wsClient.sendCommand({ cmd: 'toggle_mute' });
  }

  function handleClick() {
    if (suspended) {
      onResume?.();
    } else {
      toggleMute();
    }
  }

  const icon = $derived(
    suspended ? '🔇' :
    sdrState.muted ? '🔇' : '🔊'
  );

  const title = $derived(
    suspended ? 'Click to enable audio' :
    sdrState.muted ? 'Unmute' : 'Mute'
  );
</script>

<button class="audio-control" onclick={handleClick} {title}>
  <span class="audio-icon">{icon}</span>
  {#if suspended}
    <span class="audio-label">Click to enable</span>
  {/if}
</button>

<style>
  .audio-control {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    background: none;
    border: 1px solid #444;
    border-radius: 4px;
    padding: 0.25rem 0.5rem;
    cursor: pointer;
    color: #ccc;
    font-family: inherit;
    font-size: 0.85rem;
    transition: border-color 0.2s;
  }

  .audio-control:hover {
    border-color: #00d4ff;
  }

  .audio-icon {
    font-size: 1rem;
  }

  .audio-label {
    font-size: 0.75rem;
    color: #ff8800;
    animation: pulse 1.5s ease-in-out infinite;
  }

  @keyframes pulse {
    0%, 100% { opacity: 0.6; }
    50% { opacity: 1; }
  }
</style>
