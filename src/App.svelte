<script lang="ts">
  import { onMount } from 'svelte';
  import { windowLabel } from './lib/api';
  import { PRODUCT } from './lib/brand';
  import { app } from './lib/state.svelte';
  import Dashboard from './views/Dashboard.svelte';
  import Widget from './views/Widget.svelte';

  let kind = $state<'dashboard' | 'widget' | null>(null);
  let failed = $state<string | null>(null);

  onMount(() => {
    document.title = PRODUCT;
    void (async () => {
      const label = await windowLabel();
      const k = label === 'widget' ? 'widget' : 'dashboard';
      if (k === 'widget') document.documentElement.classList.add('widget-transparent');
      try {
        await app.init(k);
        kind = k;
      } catch (e) {
        failed = e instanceof Error ? e.message : String(e);
      }
    })();
    return () => app.destroy();
  });
</script>

{#if kind === 'widget'}
  <Widget />
{:else if kind === 'dashboard'}
  <Dashboard />
{:else if failed}
  <div class="failed" role="alert">
    <p class="title">{PRODUCT} could not start its speech engine.</p>
    <p class="detail selectable">{failed}</p>
  </div>
{/if}

<style>
  .failed {
    display: grid;
    place-content: center;
    height: 100%;
    padding: 32px;
    text-align: center;
  }
  .title {
    font-size: 15px;
    font-weight: 700;
  }
  .detail {
    margin-top: 6px;
    color: var(--ink-2);
  }
</style>
