<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import Header from './components/Header.svelte';
  import Spectrum from './components/Spectrum.svelte';
  import Waterfall from './components/Waterfall.svelte';
  import Controls from './components/Controls.svelte';
  import SettingsModal from './components/SettingsModal.svelte';
  import { wsClient } from './lib/ws.svelte';
  import { sdrState } from './lib/state.svelte';
  import { AudioPlayer } from './lib/audio';
  import type { SpectrumFrame, AudioFrame } from './lib/protocol';

  let waterfall: Waterfall;
  const audioPlayer = new AudioPlayer();

  let audioSuspended = $state(true);
  let settingsOpen = $state(false);

  function handleSpectrumFrame(frame: SpectrumFrame) {
    waterfall?.addSpectrumRow(frame);
  }

  function handleAudioFrame(frame: AudioFrame) {
    if (frame.format === 'pcm') {
      audioPlayer.feedPcm(frame.data);
    }
    // TODO: handle Opus frames when decoder is added
  }

  async function handleAudioResume() {
    if (!audioPlayer.started) {
      audioPlayer.start();
    }
    await audioPlayer.resume();
    audioSuspended = audioPlayer.suspended;
  }

  $effect(() => {
    audioPlayer.setVolume(sdrState.volume);
  });

  $effect(() => {
    audioPlayer.setMuted(sdrState.muted);
  });

  onMount(() => {
    wsClient.onAudio = handleAudioFrame;
    wsClient.onReconnect = () => {
      waterfall?.clear();
    };
    audioPlayer.start();
    audioSuspended = audioPlayer.suspended;
    wsClient.connect();
  });

  onDestroy(() => {
    wsClient.onAudio = null;
    wsClient.onReconnect = null;
    wsClient.disconnect();
    audioPlayer.stop();
  });
</script>

<div class="app">
  <Header {audioSuspended} onAudioResume={handleAudioResume} {settingsOpen} onToggleSettings={() => settingsOpen = !settingsOpen} />

  <main class="content">
    <div class="panel spectrum-panel">
      <Spectrum onFrame={handleSpectrumFrame} />
    </div>

    <div class="panel waterfall-panel">
      <Waterfall bind:this={waterfall} />
    </div>

    <div class="panel controls-panel">
      <Controls />
    </div>
  </main>

  <SettingsModal open={settingsOpen} onClose={() => settingsOpen = false} />
</div>

<style>
  :global(*, *::before, *::after) {
    box-sizing: border-box;
    margin: 0;
    padding: 0;
  }

  :global(html, body) {
    height: 100%;
    background: #0d0d1a;
    color: #e0e0e0;
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    overflow: hidden;
  }

  :global(#app) {
    height: 100%;
  }

  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: #0d0d1a;
  }

  .content {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 2px;
    min-height: 0;
  }

  .panel {
    border: 1px solid #2a2a3e;
    border-radius: 4px;
    background: #12122a;
    overflow: hidden;
  }

  .spectrum-panel {
    flex: 3;
    min-height: 0;
  }

  .waterfall-panel {
    flex: 4;
    min-height: 0;
  }

  .controls-panel {
    flex-shrink: 0;
    height: 60px;
  }

</style>
