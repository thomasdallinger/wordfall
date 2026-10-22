<script lang="ts">
  import type { Snippet } from 'svelte';
  import { tick } from 'svelte';

  /**
   * A small popover menu. Items are buttons with role menuitem / menuitemradio inside `children`.
   * Opens with click or ↓, moves with ↑/↓, closes with Esc and returns focus to the trigger.
   */
  let {
    open = $bindable(false),
    align = 'end',
    label,
    trigger,
    children,
  }: {
    open?: boolean;
    align?: 'start' | 'end';
    label: string;
    trigger: Snippet<[{ toggle: () => void; open: boolean; onkeydown: (e: KeyboardEvent) => void }]>;
    children: Snippet<[{ close: () => void }]>;
  } = $props();

  let root: HTMLDivElement | undefined = $state();
  let menu: HTMLDivElement | undefined = $state();

  function items(): HTMLElement[] {
    return Array.from(menu?.querySelectorAll<HTMLElement>('[role^="menuitem"]:not(:disabled)') ?? []);
  }

  async function show() {
    open = true;
    await tick();
    const list = items();
    (list.find((el) => el.getAttribute('aria-checked') === 'true') ?? list[0])?.focus();
  }

  function close(refocus = true) {
    open = false;
    if (refocus) root?.querySelector<HTMLElement>('[aria-haspopup]')?.focus();
  }

  function toggle() {
    if (open) close(false);
    else void show();
  }

  function onTriggerKey(e: KeyboardEvent) {
    if (e.key === 'ArrowDown' && !open) {
      e.preventDefault();
      void show();
    }
  }

  function onMenuKey(e: KeyboardEvent) {
    const list = items();
    const i = list.indexOf(document.activeElement as HTMLElement);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const next = list[(i + (e.key === 'ArrowDown' ? 1 : -1) + list.length) % list.length];
      next?.focus();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      close();
    } else if (e.key === 'Tab') {
      close(false);
    }
  }

  function onWindowPointer(e: PointerEvent) {
    if (open && root && !root.contains(e.target as Node)) close(false);
  }
</script>

<svelte:window onpointerdown={onWindowPointer} />

<div class="menu-root" bind:this={root}>
  {@render trigger({ toggle, open, onkeydown: onTriggerKey })}
  {#if open}
    <div class="menu" class:start={align === 'start'} role="menu" aria-label={label} tabindex="-1" bind:this={menu} onkeydown={onMenuKey}>
      {@render children({ close: () => close() })}
    </div>
  {/if}
</div>

<style>
  .menu-root {
    position: relative;
    display: inline-flex;
  }
  .menu {
    position: absolute;
    top: calc(100% + 4px);
    right: 0;
    z-index: 40;
    min-width: 200px;
    padding: 4px;
    border-radius: var(--radius-sheet);
    background: var(--surface);
    box-shadow:
      0 0 0 1px var(--line),
      var(--shadow-float);
    animation: appear var(--dur) var(--ease);
  }
  .menu.start {
    right: auto;
    left: 0;
  }
  .menu :global([role^='menuitem']) {
    display: flex;
    width: 100%;
    align-items: center;
    gap: 8px;
    height: 28px;
    padding: 0 8px;
    border-radius: 5px;
    font-size: 13px;
    color: var(--ink);
    text-align: left;
    outline: none;
  }
  .menu :global([role^='menuitem']:hover),
  .menu :global([role^='menuitem']:focus-visible) {
    background: var(--press);
  }
  .menu :global(.menu-sep) {
    height: 1px;
    margin: 4px 6px;
    background: var(--line);
  }
  .menu :global(.menu-check) {
    width: 14px;
    flex-shrink: 0;
    color: var(--ink);
  }
</style>
