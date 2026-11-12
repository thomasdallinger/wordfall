<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../api';
  import { app } from '../state.svelte';
  import type { MeetingScreen, ScreenWindow } from '../types';

  /** While a meeting records: which window's text is read (the shared screen), pause, and what's wrong. */
  let { meetingId }: { meetingId: string } = $props();

  let sources = $state<ScreenWindow[]>([]);
  const screen = $derived<MeetingScreen>(app.meetingScreens[meetingId] ?? { status: 'choose', window: null });
  const picked = $derived(screen.window?.windowId ?? null);
  /** The picked window stays in the list even when it's gone from the screen. */
  const options = $derived(
    screen.window && !sources.some((s) => s.windowId === screen.window!.windowId) ? [screen.window, ...sources] : sources,
  );

  function name(w: ScreenWindow): string {
    return w.title && w.title !== w.app ? `${w.app}: ${w.title}` : w.app;
  }

  async function refresh() {
    try {
      sources = await api.meetingScreenSources();
    } catch (e) {
      app.error('The windows could not be listed', e);
    }
  }

  async function pick(value: string) {
    try {
      app.meetingScreens[meetingId] = await api.setMeetingScreen(meetingId, value ? Number(value) : null, false);
    } catch (e) {
      app.error('That window can’t be read', e);
      void refresh();
    }
  }

  async function pause(paused: boolean) {
    try {
      app.meetingScreens[meetingId] = await api.setMeetingScreen(meetingId, picked, paused);
    } catch (e) {
      app.error('Screen reading did not change', e);
    }
  }

  onMount(() => {
    void refresh();
    void app.loadScreen(meetingId);
  });
</script>

<div class="screen">
  <label class="label" for="screen-source">Screen</label>
  <select
    id="screen-source"
    class="field select"
    value={picked === null ? '' : String(picked)}
    onfocus={refresh}
    onchange={(e) => pick(e.currentTarget.value)}
  >
    <option value="">Don’t read a screen</option>
    {#each options as w (w.windowId)}
      <option value={String(w.windowId)}>{name(w)}</option>
    {/each}
  </select>
  {#if picked !== null && (screen.status === 'reading' || screen.status === 'paused')}
    <button class="btn btn-quiet btn-sm" onclick={() => pause(screen.status !== 'paused')}>
      {screen.status === 'paused' ? 'Resume' : 'Pause'}
    </button>
  {/if}
  <span class="state" role="status">
    {#if screen.status === 'reading'}
      Reading it every 10 seconds; a picture is kept when its text changes.
    {:else if screen.status === 'paused'}
      Paused. Nothing is read until you resume.
    {:else if screen.status === 'choose'}
      Pick the window with the shared screen to read its text.
    {:else if screen.status === 'source-lost'}
      That window closed. Pick another one.
    {/if}
  </span>
</div>
{#if screen.status === 'needs-permission'}
  <p class="problem">
    <span class="status-warn">Wordfall can’t see other windows.</span>
    Allow it under Screen Recording, then quit and reopen Wordfall.
    <button class="btn btn-sm" onclick={() => app.requestPermission('screen')}>Open System Settings</button>
  </p>
{/if}

<style>
  .screen {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 8px;
    min-width: 0;
  }
  .label {
    font-size: 13px;
    font-weight: 700;
    color: var(--ink-2);
  }
  .select {
    width: 260px;
    height: 26px;
    font-size: 12px;
  }
  .state {
    min-width: 0;
    font-size: 12px;
    color: var(--ink-2);
  }
  .problem {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin-top: 6px;
    font-size: 12px;
    color: var(--ink-2);
  }
</style>
