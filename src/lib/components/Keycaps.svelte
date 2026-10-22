<script lang="ts">
  /** Key caps for a shortcut, e.g. ["⌘", "⇧", "Space"]. */
  let { keys, empty = 'None' }: { keys: string[]; empty?: string } = $props();
</script>

{#if keys.length === 0}
  <span class="none">{empty}</span>
{:else}
  <span class="caps" aria-label={keys.join(' ')}>
    {#each keys as key, i (i)}<kbd class="keycap">{key}</kbd>{/each}
  </span>
{/if}

<style>
  .caps {
    display: inline-flex;
    gap: 3px;
  }
  .none {
    font-size: 13px;
    color: var(--ink-3);
  }
</style>
