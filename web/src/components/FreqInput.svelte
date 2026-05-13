<script lang="ts">
  interface Props {
    value: number;
    onSubmit: (hz: number) => void;
  }

  let { value, onSubmit }: Props = $props();

  let editing = $state(false);
  let editText = $state('');

  function formatFreq(hz: number): string {
    const s = Math.round(hz).toString().padStart(9, '0');
    return `${s.slice(0, -6)}.${s.slice(-6, -3)}.${s.slice(-3)}`;
  }

  function parseFreq(text: string): number | null {
    const t = text.trim().toUpperCase();
    const m = t.match(/^([\d.]+)\s*([MKG]?)(?:HZ)?$/);
    if (!m) return null;
    const num = parseFloat(m[1]);
    if (isNaN(num)) return null;
    const suffix = m[2];
    const mult = suffix === 'G' ? 1e9 : suffix === 'M' ? 1e6 : suffix === 'K' ? 1e3 : 1;
    return Math.round(num * mult);
  }

  function startEdit() {
    editing = true;
    const mhz = value / 1e6;
    editText = mhz.toFixed(3) + 'M';
  }

  function commitEdit() {
    editing = false;
    const hz = parseFreq(editText);
    if (hz !== null && hz > 0) {
      onSubmit(hz);
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') commitEdit();
    if (e.key === 'Escape') { editing = false; }
  }
</script>

<div class="freq-input">
  {#if editing}
    <!-- svelte-ignore a11y_autofocus -->
    <input
      class="freq-edit"
      type="text"
      bind:value={editText}
      onblur={commitEdit}
      onkeydown={handleKeydown}
      autofocus
    />
  {:else}
    <button class="freq-display" onclick={startEdit} title="Click to edit frequency">
      {formatFreq(value)} <span class="freq-unit">Hz</span>
    </button>
  {/if}
</div>

<style>
  .freq-input {
    display: flex;
    align-items: center;
  }

  .freq-display {
    background: #12122a;
    border: 1px solid #333;
    color: #00ff88;
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.95rem;
    font-weight: bold;
    padding: 0.3rem 0.5rem;
    border-radius: 4px;
    cursor: pointer;
    letter-spacing: 0.03em;
    white-space: nowrap;
  }

  .freq-display:hover {
    border-color: #00d4ff;
  }

  .freq-unit {
    color: #666;
    font-weight: normal;
    font-size: 0.75rem;
  }

  .freq-edit {
    background: #12122a;
    border: 1px solid #00d4ff;
    color: #00ff88;
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.95rem;
    padding: 0.3rem 0.5rem;
    border-radius: 4px;
    outline: none;
    width: 10rem;
  }
</style>
