<script lang="ts">
  let {
    checked,
    onchange,
    disabled = false,
    label,
    labelledby,
  }: {
    checked: boolean;
    onchange: (value: boolean) => void;
    disabled?: boolean;
    /** Accessible name when no visible label is linked via `labelledby`. */
    label?: string;
    labelledby?: string;
  } = $props();
</script>

<button
  type="button"
  role="switch"
  aria-checked={checked}
  aria-label={labelledby ? undefined : label}
  aria-labelledby={labelledby}
  {disabled}
  class="switch"
  class:on={checked}
  onclick={() => onchange(!checked)}
>
  <span class="knob"></span>
</button>

<style>
  .switch {
    position: relative;
    flex-shrink: 0;
    width: 32px;
    height: 18px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--ink) 18%, transparent);
    transition: background-color var(--dur) var(--ease);
  }
  .switch:hover {
    background: color-mix(in srgb, var(--ink) 26%, transparent);
  }
  .switch.on {
    background: var(--ink);
  }
  .switch:disabled {
    opacity: 0.4;
  }
  .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 14px;
    height: 14px;
    border-radius: 999px;
    background: var(--surface);
    box-shadow: 0 1px 1.5px rgb(0 0 0 / 0.25);
    transition: transform var(--dur) var(--ease);
  }
  .on .knob {
    transform: translateX(14px);
  }
</style>
