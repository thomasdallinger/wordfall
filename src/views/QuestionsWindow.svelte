<script lang="ts">
  import { ChevronDownIcon, ChevronUpIcon, XIcon } from '@lucide/svelte';
  import { onMount } from 'svelte';
  import QuestionStack from '../lib/components/QuestionStack.svelte';
  import { api, startWindowDrag } from '../lib/api';
  import { app } from '../lib/state.svelte';

  /** Transparent margin around the panel, for its shadow (the window is sized to panel + margin). */
  const MARGIN = 12;

  const active = $derived(app.activeMeeting);
  const data = $derived(active ? (app.meetingQuestions[active.id] ?? null) : null);
  const open = $derived(data?.questions.filter((q) => q.state === 'live').length ?? 0);
  const goal = $derived(data?.goal ?? '');
  let collapsed = $state(false);
  let panel: HTMLDivElement | undefined = $state();

  // Load the questions of the meeting being recorded whenever that changes; events keep them fresh.
  $effect(() => {
    if (active) void app.loadQuestions(active.id);
  });

  onMount(() => {
    // Keep the native window as small as the panel, so the transparent rest doesn't take clicks.
    let last = '';
    const ro = new ResizeObserver(() => {
      if (!panel) return;
      const r = panel.getBoundingClientRect();
      const key = `${Math.ceil(r.width)}x${Math.ceil(r.height)}`;
      if (key === last) return;
      last = key;
      api.resizeQuestionsWindow(Math.ceil(r.width) + MARGIN * 2, Math.ceil(r.height) + MARGIN * 2).catch(() => {});
    });
    if (panel) ro.observe(panel);
    return () => ro.disconnect();
  });

  function onHeaderDown(e: PointerEvent) {
    if (e.button !== 0 || (e.target as HTMLElement).closest('button')) return;
    void startWindowDrag();
  }
</script>

<div class="root" style:--margin="{MARGIN}px">
  <div class="panel" bind:this={panel}>
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="head" onpointerdown={onHeaderDown}>
      <span class="live-dot" aria-hidden="true"></span>
      <span class="title">Questions</span>
      {#if data?.status === 'thinking'}
        <span class="sub" role="status">Reading the talk…</span>
      {:else if collapsed && open > 0}
        <span class="sub">{open} open</span>
      {/if}
      <span class="spacer"></span>
      <button class="icon-btn small" aria-label={collapsed ? 'Show questions' : 'Fold questions away'} title={collapsed ? 'Show' : 'Fold away'} onclick={() => (collapsed = !collapsed)}>
        {#if collapsed}<ChevronDownIcon size={15} strokeWidth={1.5} />{:else}<ChevronUpIcon size={15} strokeWidth={1.5} />{/if}
      </button>
      <button class="icon-btn small" aria-label="Hide this window" title="Hide (show it again from the meeting)" onclick={() => api.showQuestionsWindow(false)}>
        <XIcon size={15} strokeWidth={1.5} />
      </button>
    </div>
    {#if !collapsed}
      <div class="body">
        {#if goal}
          <p class="goal"><span class="goal-label">Goal</span> {goal}</p>
        {/if}
        {#if active}
          <QuestionStack meetingId={active.id} startedAt={active.startedAt} questions={data?.questions ?? []} status={data?.status ?? 'listening'} />
        {:else}
          <p class="idle">No meeting is being recorded.</p>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .root {
    padding: var(--margin);
    width: max-content;
  }
  .panel {
    width: 316px;
    border-radius: var(--radius-sheet);
    background: var(--bg);
    box-shadow:
      0 0 0 1px var(--line),
      var(--shadow-float);
    overflow: hidden;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 36px;
    padding: 0 6px 0 12px;
    cursor: grab;
  }
  .live-dot {
    width: 7px;
    height: 7px;
    border-radius: 999px;
    background: var(--signal);
  }
  .title {
    font-size: 13px;
    font-weight: 700;
    color: var(--ink);
  }
  .sub {
    font-size: 12px;
    color: var(--ink-2);
  }
  .spacer {
    flex: 1;
  }
  .icon-btn.small {
    width: 24px;
    height: 24px;
  }
  .body {
    max-height: 560px;
    overflow-y: auto;
    padding: 0 12px 12px;
  }
  .goal {
    margin-bottom: 10px;
    font-size: 12px;
    line-height: 17px;
    color: var(--ink-2);
  }
  .goal-label {
    font-weight: 700;
  }
  .idle {
    font-size: 13px;
    color: var(--ink-2);
  }
</style>
