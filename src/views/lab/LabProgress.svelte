<script lang="ts">
  import { api } from '../../lib/api';
  import { formatNumber } from '../../lib/format';
  import { app } from '../../lib/state.svelte';
  import type { BenchInfo } from '../../lib/types';

  let { recorded, total, info }: { recorded: number; total: number; info: BenchInfo | null } = $props();

  const pct = $derived(total > 0 ? Math.min(1, recorded / total) : 0);

  async function open(path: string) {
    try {
      await api.openPath(path);
    } catch (e) {
      app.error('That did not open', e);
    }
  }
</script>

<section class="progress" aria-label="Progress">
  <div class="line">
    <p>
      <strong>{recorded} of {total}</strong> prompts recorded.
      {#if info}
        <span class="aside">
          Together with {formatNumber(info.historyWithAudio)} kept dictations, {formatNumber(info.historyVerified)} of them verified.
        </span>
      {/if}
    </p>
    {#if info}
      <div class="links">
        <button class="btn btn-quiet btn-sm" onclick={() => open(info.root)}>Show clips</button>
        {#if info.latestReport}
          {@const report = info.latestReport}
          <button class="btn btn-quiet btn-sm" onclick={() => open(report)}>Latest report</button>
        {/if}
      </div>
    {/if}
  </div>
  <div class="bar" role="progressbar" aria-label="Prompts recorded" aria-valuemin={0} aria-valuemax={total} aria-valuenow={recorded}>
    <span style:width="{pct * 100}%"></span>
  </div>
</section>

<style>
  .line {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    font-size: 13px;
    line-height: 19px;
  }
  strong {
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }
  .aside {
    color: var(--ink-2);
  }
  .links {
    display: flex;
    flex-shrink: 0;
    gap: 2px;
    margin-right: -9px;
  }
  .bar {
    height: 3px;
    margin-top: 10px;
    border-radius: 999px;
    background: var(--press);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--ink);
    transition: width 300ms var(--ease);
  }
</style>
