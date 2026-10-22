<script lang="ts">
  import type { Snippet } from 'svelte';

  /** A settings section with an anchor (`settings-<id>`) for the sidebar and scroll spy. */
  let { id, title, intro, children }: { id: string; title: string; intro?: string; children: Snippet } = $props();
</script>

<section class="section settings-section" id="settings-{id}" data-section={id} aria-labelledby="settings-{id}-title">
  <h2 class="title" id="settings-{id}-title" tabindex="-1">{title}</h2>
  {#if intro}<p class="intro">{intro}</p>{/if}
  <div class="rows">
    {@render children()}
  </div>
</section>

<style>
  .section {
    scroll-margin-top: calc(var(--toolbar-height) + 8px);
  }
  .title {
    font-size: 17px;
    line-height: 24px;
  }
  .intro {
    max-width: 58ch;
    margin-top: 2px;
    font-size: 13px;
    line-height: 19px;
    color: var(--ink-2);
  }
  .rows {
    margin-top: 4px;
  }
</style>
