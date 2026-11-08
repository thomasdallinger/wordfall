<script lang="ts">
  import { onMount } from 'svelte';
  import Page from '../lib/components/Page.svelte';
  import { api } from '../lib/api';
  import { clock, dayKey, dayLabel, duration, formatTime } from '../lib/format';
  import { app } from '../lib/state.svelte';
  import type { MeetingItem } from '../lib/types';
  import MeetingDetail from './meetings/MeetingDetail.svelte';

  let items = $state<MeetingItem[]>([]);
  let loaded = $state(false);
  let starting = $state(false);
  let now = $state(Date.now());

  const active = $derived(app.activeMeeting);
  const groups = $derived.by(() => {
    const out: { key: string; label: string; items: MeetingItem[] }[] = [];
    for (const m of items) {
      const key = dayKey(m.startedAt);
      const last = out[out.length - 1];
      if (last?.key === key) last.items.push(m);
      else out.push({ key, label: dayLabel(m.startedAt), items: [m] });
    }
    return out;
  });

  async function load() {
    try {
      items = await api.listMeetings();
      loaded = true;
    } catch (e) {
      app.error('Meetings did not load', e);
    }
  }

  onMount(() => {
    void load();
    const t = setInterval(() => (now = Date.now()), 1000);
    const off = app.onMeetingEvent((e) => {
      if (e.kind !== 'segments') void load();
    });
    return () => {
      clearInterval(t);
      off();
    };
  });

  async function start() {
    starting = true;
    await app.startMeeting();
    starting = false;
  }

  function status(m: MeetingItem): string {
    switch (m.status) {
      case 'recording':
        return `Recording, ${clock(now - Date.parse(m.startedAt))}`;
      case 'processing':
        return 'Finishing the transcript…';
      case 'summarizing':
        return 'Writing notes…';
      default:
        return duration(m.durationMs);
    }
  }
</script>

{#if app.meetingId}
  {#key app.meetingId}
    <MeetingDetail id={app.meetingId} />
  {/key}
{:else}
  <Page title="Meetings">
    {#snippet actions()}
      {#if active}
        <button class="btn" onclick={() => (app.meetingId = active.id)}>Show current meeting</button>
      {:else}
        <button class="btn btn-signal" disabled={starting} onclick={start}>{starting ? 'Starting…' : 'Start meeting'}</button>
      {/if}
    {/snippet}

    {#if loaded && items.length === 0}
      <div class="empty">
        <p class="empty-words">No meetings yet.</p>
        <p class="empty-help">
          Start one when a call begins. Wordfall records you and the other participants, writes the transcript as you
          go, and turns it into notes when you stop. You can jot down your own notes along the way; they shape the
          result. Everything stays on this computer.
        </p>
      </div>
    {/if}

    {#each groups as g (g.key)}
      <section class="day" aria-labelledby="mday-{g.key}">
        <h2 class="day-label" id="mday-{g.key}">{g.label}</h2>
        <ul>
          {#each g.items as m (m.id)}
            <li>
              <button class="item" onclick={() => (app.meetingId = m.id)}>
                <span class="margin">
                  <time datetime={m.startedAt}>{formatTime(m.startedAt)}</time>
                </span>
                <span class="body">
                  <span class="title">
                    {#if m.status === 'recording'}<span class="live-dot" aria-hidden="true"></span>{/if}
                    {m.title || 'Untitled meeting'}
                  </span>
                  {#if m.preview}<span class="preview">{m.preview}</span>{/if}
                  <span class="meta" class:busy={m.status !== 'done'}>{status(m)}</span>
                </span>
              </button>
            </li>
          {/each}
        </ul>
      </section>
    {/each}
  </Page>
{/if}

<style>
  .empty {
    padding: 48px 0 0 88px;
  }
  .empty-words {
    font-family: var(--font-text);
    font-style: italic;
    font-size: 22px;
    line-height: 30px;
    color: var(--ink-2);
  }
  .empty-help {
    max-width: 52ch;
    margin-top: 10px;
    font-size: 13px;
    line-height: 20px;
    color: var(--ink-2);
  }
  .day {
    margin-top: 28px;
  }
  .day:first-of-type {
    margin-top: 4px;
  }
  .day-label {
    position: sticky;
    top: var(--toolbar-height);
    z-index: 5;
    margin: 0 -12px 4px;
    padding: 6px 12px;
    font-size: 13px;
    line-height: 18px;
    background: var(--surface);
  }
  .item {
    display: grid;
    grid-template-columns: 72px minmax(0, 1fr);
    column-gap: 16px;
    width: calc(100% + 24px);
    margin: 0 -12px;
    padding: 10px 12px;
    border-radius: var(--radius-sheet);
    text-align: left;
    transition: background-color var(--dur) var(--ease);
  }
  .item:hover {
    background: var(--bg);
  }
  .margin {
    padding-top: 2px;
    font-size: 12px;
    line-height: 16px;
    color: var(--ink-3);
    text-align: right;
    white-space: nowrap;
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .title {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 15px;
    line-height: 20px;
    font-weight: 700;
    color: var(--ink);
  }
  .preview {
    max-width: 62ch;
    font-family: var(--font-text);
    font-size: 15px;
    line-height: 24px;
    color: var(--ink-2);
    overflow: hidden;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }
  .meta {
    font-size: 12px;
    color: var(--ink-3);
    font-variant-numeric: tabular-nums;
  }
  .meta.busy {
    color: var(--ink-2);
  }
  .live-dot {
    width: 7px;
    height: 7px;
    border-radius: 999px;
    background: var(--signal);
  }
</style>
