<script lang="ts">
  import { onMount } from 'svelte';
  import Logo from '../lib/components/Logo.svelte';
  import { api, on, startWindowDrag } from '../lib/api';
  import { PRODUCT } from '../lib/brand';
  import { waveHeights } from '../lib/levels';
  import { app } from '../lib/state.svelte';
  import type { Notice } from '../lib/types';

  const DRAG_THRESHOLD = 4;
  /** Transparent margin around the content when sizing the window (matches --pill-margin). */
  const WINDOW_MARGIN = 10;
  const BARS = 15;

  const rec = $derived(app.snapshot?.recState ?? 'idle');
  const look = $derived(app.settings?.pillStyle ?? 'logo-and-text');
  const locked = $derived(app.settings?.widgetLocked ?? false);
  const live = $derived(rec !== 'idle');
  const heights = $derived(rec === 'listening' ? waveHeights(app.levels, BARS) : null);

  let stack: HTMLDivElement | undefined = $state();
  let notice = $state<Notice | null>(null);
  let noticeTimer: ReturnType<typeof setTimeout> | undefined;

  onMount(() => {
    // Keep the native window as small as its content so transparent areas don't swallow clicks.
    let last = '';
    let frame = 0;
    const ro = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        if (!stack) return;
        const r = stack.getBoundingClientRect();
        const width = Math.ceil(r.width) + WINDOW_MARGIN * 2;
        const height = Math.ceil(r.height) + WINDOW_MARGIN * 2;
        const key = `${width}x${height}`;
        if (key === last) return;
        last = key;
        api.resizeWidget(width, height).catch(() => {});
      });
    });
    if (stack) ro.observe(stack);

    const un = on('notice', (n) => {
      if (n.kind !== 'warning' && n.kind !== 'error') return;
      notice = n;
      clearTimeout(noticeTimer);
      noticeTimer = setTimeout(() => (notice = null), 4000);
    });
    return () => {
      ro.disconnect();
      cancelAnimationFrame(frame);
      clearTimeout(noticeTimer);
      void un.then((u) => u());
    };
  });

  function activate() {
    if (rec === 'idle') void api.startRecording();
    else if (rec === 'listening') void api.stopRecording();
  }

  // ---- click vs. drag
  let down: { x: number; y: number; dragging: boolean } | null = null;

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0 || (e.target as HTMLElement).closest('button')) return;
    down = { x: e.screenX, y: e.screenY, dragging: false };
  }

  function onPointerMove(e: PointerEvent) {
    if (!down || down.dragging || locked) return;
    if (Math.hypot(e.screenX - down.x, e.screenY - down.y) > DRAG_THRESHOLD) {
      down.dragging = true;
      void startWindowDrag();
    }
  }

  function onPointerUp() {
    if (!down) return;
    const wasDrag = down.dragging;
    down = null;
    if (!wasDrag) activate();
  }

  const label = $derived(
    rec === 'idle' ? `Start dictating with ${PRODUCT}` : rec === 'listening' ? 'Stop and paste' : 'Writing out what you said',
  );
</script>

<svelte:window onpointermove={onPointerMove} onpointerup={onPointerUp} onblur={() => (down = null)} />

<div class="pill-root">
  <div class="pill-stack" bind:this={stack}>
    {#if notice}
      <button class="pill-note {notice.kind}" title={notice.message || undefined} onclick={() => (notice = null)}>
        <span class="pill-note-dot" aria-hidden="true"></span>
        <span class="pill-note-text">{notice.title}</span>
      </button>
    {/if}

    <div
      class="pill"
      class:live
      class:locked
      class:mark-only={!live && look === 'logo-only'}
      class:dot-only={!live && look === 'logo-only-monochrome'}
      role="button"
      tabindex="0"
      aria-label={label}
      title={live ? undefined : label}
      onpointerdown={onPointerDown}
      onkeydown={(e) => {
        if (e.key !== 'Enter' && e.key !== ' ') return;
        e.preventDefault();
        activate();
      }}
    >
      {#if live}
        <span class="pill-slot">
          {#if rec === 'listening'}
            <button class="pill-btn pill-cancel" aria-label="Cancel, paste nothing" title="Cancel" onclick={() => api.cancelRecording()}>
              <svg width="8" height="8" viewBox="0 0 8 8" aria-hidden="true">
                <path d="M1.5 1.5l5 5M6.5 1.5l-5 5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
              </svg>
            </button>
          {/if}
        </span>
        <span class="pill-wave" class:settled={rec === 'processing'} aria-hidden="true">
          {#each { length: BARS } as _, i (i)}
            <span
              style:transform={heights ? `scaleY(${heights[i]})` : undefined}
              style:animation-delay={heights ? undefined : `${i * 70}ms`}
            ></span>
          {/each}
        </span>
        <span class="pill-slot">
          {#if rec === 'listening'}
            <button class="pill-btn pill-stop" aria-label="Stop and paste" title="Stop and paste" onclick={() => api.stopRecording()}>
              <svg width="7" height="7" viewBox="0 0 7 7" aria-hidden="true"><rect width="7" height="7" rx="1.5" fill="currentColor" /></svg>
            </button>
          {/if}
        </span>
      {:else if look === 'logo-only-monochrome'}
        <span class="pill-dot" aria-hidden="true"></span>
      {:else}
        <Logo size={16} />
        {#if look === 'logo-and-text'}<span class="pill-name">{PRODUCT}</span>{/if}
      {/if}
    </div>
  </div>
</div>
