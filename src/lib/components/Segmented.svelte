<script lang="ts" generics="T extends string | number">
  interface Option {
    value: T;
    label: string;
    disabled?: boolean;
  }

  let {
    options,
    value,
    onchange,
    label,
  }: {
    options: Option[];
    value: T;
    onchange: (value: T) => void;
    /** Accessible name for the group. */
    label: string;
  } = $props();

  let root: HTMLDivElement | undefined = $state();

  /** Arrow keys move the selection, like native radio groups. */
  function onkeydown(e: KeyboardEvent) {
    const step = e.key === 'ArrowRight' || e.key === 'ArrowDown' ? 1 : e.key === 'ArrowLeft' || e.key === 'ArrowUp' ? -1 : 0;
    if (!step) return;
    e.preventDefault();
    const enabled = options.filter((o) => !o.disabled);
    const i = enabled.findIndex((o) => o.value === value);
    const next = enabled[(i + step + enabled.length) % enabled.length];
    if (!next) return;
    onchange(next.value);
    const idx = options.indexOf(next);
    root?.querySelectorAll<HTMLButtonElement>('button')[idx]?.focus();
  }
</script>

<div class="seg" role="radiogroup" aria-label={label} tabindex="-1" bind:this={root} {onkeydown}>
  {#each options as opt (opt.value)}
    {@const on = opt.value === value}
    <button
      type="button"
      role="radio"
      aria-checked={on}
      tabindex={on ? 0 : -1}
      class="seg-item"
      class:on
      disabled={opt.disabled}
      onclick={() => !on && onchange(opt.value)}
    >
      {opt.label}
    </button>
  {/each}
</div>

<style>
  .seg {
    display: inline-flex;
    padding: 2px;
    gap: 2px;
    border-radius: 7px;
    background: var(--well);
  }
  .seg-item {
    height: 24px;
    padding: 0 10px;
    border-radius: 5px;
    font-size: 12px;
    font-weight: 500;
    color: var(--ink-2);
    white-space: nowrap;
    transition:
      background-color var(--dur) var(--ease),
      color var(--dur) var(--ease);
  }
  .seg-item:hover:not(.on):not(:disabled) {
    color: var(--ink);
  }
  .seg-item.on {
    color: var(--ink);
    background: var(--surface);
    box-shadow: 0 0 0 1px var(--line);
  }
  .seg-item:disabled {
    opacity: 0.4;
  }
  .seg-item:focus-visible {
    outline-offset: 0;
  }
</style>
