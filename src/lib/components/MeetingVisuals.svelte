<script module lang="ts">
  /** Thumbnails by "meeting/visual", so switching tabs doesn't fetch them again. */
  const thumbs = new Map<string, string | null>();
</script>

<script lang="ts">
  import { Trash2Icon } from '@lucide/svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import { api } from '../api';
  import { clock } from '../format';
  import { app } from '../state.svelte';
  import type { MeetingVisual } from '../types';

  let {
    meetingId,
    visuals,
    durationMs,
    reading,
    onadd,
  }: {
    meetingId: string;
    visuals: MeetingVisual[];
    /** The meeting's length so far: photos can be placed anywhere up to it. */
    durationMs: number;
    /** Names of images being read right now. */
    reading: string[];
    /** "Add image…" was pressed. */
    onadd: () => void;
  } = $props();

  let images = $state<Record<number, string | null>>({});
  const expanded = new SvelteSet<number>();
  let editing = $state<number | null>(null);
  let timeDraft = $state('');

  const sorted = $derived([...visuals].sort((a, b) => a.atMs - b.atMs || a.id - b.id));

  /** Loads a visual's thumbnail once its row is on the page (cached across tabs). */
  function thumbnail(visual: number) {
    return () => {
      const key = `${meetingId}/${visual}`;
      if (thumbs.has(key)) {
        images[visual] = thumbs.get(key) ?? null;
        return;
      }
      thumbs.set(key, null);
      api
        .meetingVisualImage(meetingId, visual)
        .then((url) => {
          thumbs.set(key, url);
          images[visual] = url;
        })
        .catch(() => thumbs.delete(key));
    };
  }

  function rows(v: MeetingVisual): string[] {
    return v.text.split('\n').filter((r) => r.trim());
  }

  function toggle(id: number) {
    if (expanded.has(id)) expanded.delete(id);
    else expanded.add(id);
  }

  async function remove(v: MeetingVisual) {
    try {
      await api.deleteMeetingVisual(meetingId, v.id);
      thumbs.delete(`${meetingId}/${v.id}`);
    } catch (e) {
      app.error('The picture is still there', e);
    }
  }

  async function open(v: MeetingVisual) {
    try {
      await api.openMeetingVisual(meetingId, v.id);
    } catch (e) {
      app.error('The picture did not open', e);
    }
  }

  function editTime(v: MeetingVisual) {
    editing = v.id;
    timeDraft = clock(v.atMs);
  }

  /** "3:20", "03:20" or "1:02:03" in ms; null if it isn't a time. */
  function parseClock(text: string): number | null {
    const parts = text.trim().split(':').map((p) => Number(p));
    if (parts.length < 2 || parts.length > 3 || parts.some((p) => !Number.isInteger(p) || p < 0)) return null;
    const [h, m, s] = parts.length === 3 ? parts : [0, ...parts];
    if (s >= 60 || (parts.length === 3 && m >= 60)) return null;
    return ((h * 60 + m) * 60 + s) * 1000;
  }

  async function saveTime(v: MeetingVisual) {
    const id = editing;
    editing = null;
    if (id !== v.id) return;
    const at = parseClock(timeDraft);
    if (at === null) {
      if (timeDraft.trim() !== clock(v.atMs)) app.toast('error', 'That is not a time', 'Write it as minutes:seconds, for example 3:20.');
      return;
    }
    if (at === v.atMs) return;
    try {
      await api.moveMeetingVisual(meetingId, v.id, Math.min(at, durationMs));
    } catch (e) {
      app.error('The picture was not moved', e);
    }
  }

  function focusOnMount(node: HTMLInputElement) {
    node.focus();
    node.select();
  }
</script>

<div class="visuals">
  <div class="add">
    <button class="btn" onclick={onadd}>Add image…</button>
    <p class="hint">
      Or drop images here, or paste one with <span class="keycap">⌘</span> <span class="keycap">V</span>. Their text gives the notes
      the right names and numbers.
    </p>
  </div>

  {#each reading as name, i (`${name}-${i}`)}
    <p class="reading" role="status"><span class="busy-dot" aria-hidden="true"></span>Reading {name}…</p>
  {/each}

  {#if sorted.length === 0 && reading.length === 0}
    <p class="placeholder">
      Nothing shown yet. Add a photo of a printout or a whiteboard, or turn on reading shared screens in Settings › Meetings.
    </p>
  {/if}

  <ul class="list">
    {#each sorted as v (v.id)}
      {@const lines = rows(v)}
      {@const long = lines.length > 5}
      <li class="visual" {@attach thumbnail(v.id)}>
        <div class="pic">
          {#if images[v.id]}
            <button class="thumb" title="Open the picture" onclick={() => open(v)}>
              <img src={images[v.id]} alt="{v.source === 'screen' ? 'Screen' : 'Photo'} at {clock(v.atMs)}" />
            </button>
          {:else}
            <div class="thumb none" aria-hidden="true"></div>
          {/if}
        </div>
        <div class="body">
          <div class="meta">
            {#if editing === v.id}
              <input
                class="field time-field"
                bind:value={timeDraft}
                aria-label="Shown at (minutes:seconds)"
                {@attach focusOnMount}
                onblur={() => saveTime(v)}
                onkeydown={(e) => {
                  if (e.key === 'Enter') (e.currentTarget as HTMLInputElement).blur();
                  if (e.key === 'Escape') editing = null;
                }}
              />
            {:else if v.source === 'photo'}
              <button class="time" title="Move it to when it was talked about" onclick={() => editTime(v)}>{clock(v.atMs)}</button>
            {:else}
              <span class="time static">{clock(v.atMs)}{#if v.untilMs && v.untilMs - v.atMs >= 60_000}–{clock(v.untilMs)}{/if}</span>
            {/if}
            <span class="kind">{v.source === 'screen' ? 'Screen' : 'Photo'}</span>
            {#if v.label}<span class="label" title={v.label}>{v.label}</span>{/if}
            <span class="spacer"></span>
            <button class="icon-btn is-bad" title="Delete" aria-label="Delete this {v.source === 'screen' ? 'screen' : 'photo'}" onclick={() => remove(v)}>
              <Trash2Icon size={14} strokeWidth={1.5} />
            </button>
          </div>
          {#if lines.length === 0}
            <p class="empty">No text was found in it, so the notes can't use it.</p>
          {:else}
            <div class={['text', 'selectable', { clamped: long && !expanded.has(v.id) }]}>
              {#each lines as row, r (r)}<p>{row}</p>{/each}
            </div>
            {#if long}
              <button class="more" onclick={() => toggle(v.id)}>{expanded.has(v.id) ? 'Show less' : `Show all ${lines.length} lines`}</button>
            {/if}
          {/if}
        </div>
      </li>
    {/each}
  </ul>
</div>

<style>
  .add {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 16px;
  }
  .hint {
    max-width: 52ch;
    font-size: 12px;
    line-height: 20px;
    color: var(--ink-2);
  }
  .reading {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 0 12px;
    font-size: 13px;
    color: var(--ink-2);
  }
  .busy-dot {
    width: 7px;
    height: 7px;
    border-radius: 999px;
    background: var(--ink-2);
    animation: breathe 1.6s ease-in-out infinite;
  }
  .placeholder {
    max-width: 56ch;
    font-family: var(--font-text);
    font-style: italic;
    font-size: 16px;
    line-height: 26px;
    color: var(--ink-2);
  }
  .list {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .visual {
    display: grid;
    grid-template-columns: 144px minmax(0, 1fr);
    column-gap: 16px;
    padding: 12px 0;
  }
  .visual + .visual {
    border-top: 1px solid var(--line);
  }
  .thumb {
    display: block;
    width: 144px;
    padding: 0;
    border-radius: var(--radius-control);
    overflow: hidden;
    box-shadow: inset 0 0 0 1px var(--line);
    background: var(--surface);
  }
  .thumb img {
    display: block;
    width: 100%;
    max-height: 120px;
    object-fit: cover;
    object-position: top;
  }
  .thumb.none {
    height: 80px;
    background: var(--well);
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 28px;
    font-size: 12px;
    color: var(--ink-2);
  }
  .time {
    font-variant-numeric: tabular-nums;
    color: var(--ink-2);
    border-radius: 4px;
  }
  button.time:hover {
    color: var(--ink);
    text-decoration: underline;
  }
  .time-field {
    width: 72px;
    height: 24px;
    font-size: 12px;
  }
  .kind {
    font-weight: 700;
    color: var(--ink);
  }
  .label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .spacer {
    flex: 1;
  }
  .text {
    max-width: 66ch;
    font-size: 13px;
    line-height: 20px;
    color: var(--ink);
    overflow-wrap: anywhere;
  }
  .text.clamped {
    max-height: 100px;
    overflow: hidden;
  }
  .text p {
    margin: 0;
  }
  .more {
    margin-top: 4px;
    font-size: 12px;
    font-weight: 500;
    color: var(--ink-2);
  }
  .more:hover {
    color: var(--ink);
    text-decoration: underline;
  }
  .empty {
    font-size: 12px;
    color: var(--ink-2);
  }
  @media (prefers-reduced-motion: reduce) {
    .busy-dot {
      animation: none;
    }
  }
</style>
