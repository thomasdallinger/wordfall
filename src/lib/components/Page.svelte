<script lang="ts">
  import type { Snippet } from 'svelte';
  import Toolbar from './Toolbar.svelte';

  /**
   * One view: its own scroll container (so switching views starts at the top),
   * a sticky toolbar, and a centred content column.
   */
  let {
    title,
    lead,
    actions,
    children,
    scroller = $bindable(),
  }: {
    title: string;
    lead?: Snippet;
    actions?: Snippet;
    children: Snippet;
    scroller?: HTMLElement;
  } = $props();

  let scrolled = $state(false);
</script>

<div class="page" class:scrolled bind:this={scroller} onscroll={(e) => (scrolled = e.currentTarget.scrollTop > 0)}>
  <Toolbar {title} {lead} {actions} />
  <div class="content">
    {@render children()}
  </div>
</div>

<style>
  .page {
    height: 100%;
    overflow-y: auto;
    background: var(--surface);
  }
  .page.scrolled :global(.toolbar) {
    box-shadow: 0 1px 0 var(--line);
  }
  .content {
    max-width: 760px;
    margin: 0 auto;
    padding: 8px 40px 64px;
  }
</style>
