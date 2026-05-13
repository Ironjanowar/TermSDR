<script lang="ts">
  interface Props {
    label: string;
    value: number;
    min: number;
    max: number;
    step?: number;
    unit?: string;
    onChange: (value: number) => void;
  }

  let { label, value, min, max, step = 1, unit = '', onChange }: Props = $props();

  let editing = $state(false);
  let editValue = $state('');

  function startEdit() {
    editing = true;
    editValue = String(value);
  }

  function commitEdit() {
    editing = false;
    const parsed = parseFloat(editValue);
    if (!isNaN(parsed)) {
      const clamped = Math.min(max, Math.max(min, parsed));
      onChange(clamped);
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') commitEdit();
    if (e.key === 'Escape') { editing = false; }
  }

  function handleSlider(e: Event) {
    const val = parseFloat((e.target as HTMLInputElement).value);
    onChange(val);
  }
  const fieldId = $derived(`field-${label.toLowerCase().replace(/\s+/g, '-')}`);
</script>

<div class="field">
  <label class="field-label" for={fieldId}>{label}</label>
  <div class="field-controls">
    <input
      type="range"
      class="field-slider"
      id={fieldId}
      {min}
      {max}
      {step}
      {value}
      oninput={handleSlider}
    />
    <span class="field-value-wrap">
      {#if editing}
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="field-input"
          type="number"
          bind:value={editValue}
          onblur={commitEdit}
          onkeydown={handleKeydown}
          autofocus
          {step}
        />
      {:else}
        <button class="field-value" onclick={startEdit} title="Click to edit">
          {Number.isInteger(step) || step >= 1 ? value : value.toFixed(2)}
        </button>
      {/if}
      {#if unit}
        <span class="field-unit">{unit}</span>
      {/if}
    </span>
  </div>
</div>

<style>
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  .field-label {
    color: #999;
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .field-controls {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  .field-slider {
    flex: 1;
    -webkit-appearance: none;
    appearance: none;
    height: 4px;
    background: #333;
    border-radius: 2px;
    outline: none;
  }

  .field-slider::-webkit-slider-thumb {
    -webkit-appearance: none;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: #00d4ff;
    cursor: pointer;
    border: none;
  }

  .field-slider::-moz-range-thumb {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: #00d4ff;
    cursor: pointer;
    border: none;
  }

  .field-value-wrap {
    display: flex;
    align-items: center;
    gap: 0.15rem;
    min-width: 5rem;
    justify-content: flex-end;
  }

  .field-value {
    background: none;
    border: 1px solid transparent;
    color: #e0e0e0;
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.85rem;
    padding: 0.15rem 0.3rem;
    border-radius: 3px;
    cursor: pointer;
    text-align: right;
    min-width: 3rem;
  }

  .field-value:hover {
    border-color: #555;
    background: #1a1a2e;
  }

  .field-input {
    width: 4rem;
    background: #1a1a2e;
    border: 1px solid #00d4ff;
    color: #e0e0e0;
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.85rem;
    padding: 0.15rem 0.3rem;
    border-radius: 3px;
    text-align: right;
    outline: none;
  }

  .field-unit {
    color: #666;
    font-size: 0.75rem;
    min-width: 1.5rem;
  }
</style>
