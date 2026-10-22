<script lang="ts">
  import type { Snippet } from 'svelte';

  /** The view's title band. Doubles as the window drag area under the transparent title bar. */
  let { title, lead, actions }: { title: string; lead?: Snippet; actions?: Snippet } = $props();
</script>

<header class="toolbar" data-tauri-drag-region>
  {#if lead}{@render lead()}{/if}
  <h1 class="title" data-tauri-drag-region>{title}</h1>
  <div class="spacer" data-tauri-drag-region></div>
  {#if actions}<div class="actions">{@render actions()}</div>{/if}
</header>

<style>
  .toolbar {
    position: sticky;
    top: 0;
    z-index: 20;
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--toolbar-height);
    padding: 0 20px 0 28px;
    background: var(--surface);
  }
  .title {
    font-size: 15px;
    line-height: 20px;
    font-weight: 700;
    white-space: nowrap;
  }
  .spacer {
    flex: 1;
    align-self: stretch;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 6px;
  }
</style>
