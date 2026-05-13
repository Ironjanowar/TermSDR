<script lang="ts">
  interface Props {
    label: string;
    checked: boolean;
    onChange: (value: boolean) => void;
  }

  let { label, checked, onChange }: Props = $props();
</script>

<label class="toggle-row">
  <span class="toggle-label">{label}</span>
  <button
    class="toggle-switch"
    class:on={checked}
    role="switch"
    aria-checked={checked}
    aria-label={label}
    onclick={() => onChange(!checked)}
  >
    <span class="toggle-knob"></span>
  </button>
</label>

<style>
  .toggle-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    cursor: pointer;
  }

  .toggle-label {
    color: #ccc;
    font-size: 0.85rem;
  }

  .toggle-switch {
    position: relative;
    width: 40px;
    height: 22px;
    border-radius: 11px;
    border: none;
    background: #444;
    cursor: pointer;
    transition: background 0.2s;
    padding: 0;
    flex-shrink: 0;
  }

  .toggle-switch.on {
    background: #00c853;
  }

  .toggle-knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: #fff;
    transition: transform 0.2s;
  }

  .toggle-switch.on .toggle-knob {
    transform: translateX(18px);
  }
</style>
