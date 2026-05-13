<script lang="ts">
  interface Props {
    open: boolean;
    onClose: () => void;
  }

  let { open, onClose }: Props = $props();
</script>

{#if open}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="overlay" onclick={onClose}>
    <div class="help-panel" onclick={(e) => e.stopPropagation()}>
      <div class="help-header">
        <h2>⌨️ Keyboard Shortcuts</h2>
        <button class="close-btn" onclick={onClose}>✕</button>
      </div>

      <div class="help-body">
        <section>
          <h3>Navigation</h3>
          <div class="shortcut"><kbd>← →</kbd><span>Tune ±25 kHz</span></div>
          <div class="shortcut"><kbd>PgUp PgDn</kbd><span>Tune ±1 MHz</span></div>
          <div class="shortcut"><kbd>c</kbd><span>Center cursor on screen</span></div>
          <div class="shortcut"><kbd>Click</kbd><span>Tune to clicked frequency</span></div>
          <div class="shortcut"><kbd>Drag</kbd><span>Pan spectrum (waterfall)</span></div>
        </section>

        <section>
          <h3>Display</h3>
          <div class="shortcut"><kbd>+ −</kbd><span>Zoom bandwidth in/out</span></div>
          <div class="shortcut"><kbd>↑ ↓</kbd><span>Adjust gain ±1 dB</span></div>
          <div class="shortcut"><kbd>Scroll</kbd><span>Zoom (on spectrum)</span></div>
        </section>

        <section>
          <h3>Audio</h3>
          <div class="shortcut"><kbd>Space</kbd><span>Toggle mute</span></div>
          <div class="shortcut"><kbd>v / V</kbd><span>Volume up / down</span></div>
          <div class="shortcut"><kbd>d</kbd><span>Cycle demod (AM → FM → NFM)</span></div>
          <div class="shortcut"><kbd>w / W</kbd><span>Widen / narrow channel BW</span></div>
          <div class="shortcut"><kbd>m</kbd><span>Pin VFO at cursor</span></div>
        </section>

        <section>
          <h3>General</h3>
          <div class="shortcut"><kbd>?</kbd><span>Show this help</span></div>
          <div class="shortcut"><kbd>s</kbd><span>Toggle settings panel</span></div>
          <div class="shortcut"><kbd>Esc</kbd><span>Close panel / overlay</span></div>
        </section>
      </div>

      <div class="help-footer">Press any key to close</div>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.7);
    z-index: 2000;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .help-panel {
    background: #1a1a2e;
    border: 1px solid #333;
    border-radius: 8px;
    width: 460px;
    max-width: 90vw;
    max-height: 80vh;
    overflow-y: auto;
    animation: fadeIn 0.15s ease-out;
  }

  @keyframes fadeIn {
    from { opacity: 0; transform: scale(0.95); }
    to { opacity: 1; transform: scale(1); }
  }

  .help-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 1rem 1.25rem;
    border-bottom: 1px solid #333;
  }

  .help-header h2 {
    margin: 0;
    font-size: 1.1rem;
    color: #00d4ff;
  }

  .close-btn {
    background: none;
    border: none;
    color: #888;
    font-size: 1.2rem;
    cursor: pointer;
  }

  .close-btn:hover { color: #fff; }

  .help-body {
    padding: 1rem 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
  }

  section h3 {
    color: #ffdd00;
    font-size: 0.8rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    margin: 0 0 0.5rem 0;
  }

  .shortcut {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.2rem 0;
    font-size: 0.85rem;
  }

  kbd {
    display: inline-block;
    min-width: 5.5rem;
    background: #12122a;
    border: 1px solid #444;
    border-radius: 3px;
    padding: 0.15rem 0.4rem;
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.8rem;
    color: #00d4ff;
    text-align: center;
  }

  .shortcut span {
    color: #ccc;
  }

  .help-footer {
    text-align: center;
    padding: 0.75rem;
    color: #666;
    font-size: 0.75rem;
    border-top: 1px solid #2a2a3e;
  }
</style>
