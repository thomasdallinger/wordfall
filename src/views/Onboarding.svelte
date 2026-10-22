<script lang="ts">
  import { CheckIcon } from '@lucide/svelte';
  import { onMount, tick } from 'svelte';
  import Keycaps from '../lib/components/Keycaps.svelte';
  import Logo from '../lib/components/Logo.svelte';
  import ModelList from '../lib/components/ModelList.svelte';
  import Waveform from '../lib/components/Waveform.svelte';
  import { api, on } from '../lib/api';
  import { PRODUCT } from '../lib/brand';
  import { acceleratorParts, pttCap } from '../lib/format';
  import { app } from '../lib/state.svelte';
  import type { HistoryRecord, PermissionKind } from '../lib/types';

  const STEPS = ['Permissions', 'Model', 'Try it'] as const;
  let step = $state(0);
  let heading: HTMLHeadingElement | undefined = $state();

  const PERMISSIONS: { kind: PermissionKind; label: string; help: string }[] = [
    { kind: 'microphone', label: 'Microphone', help: 'Hears you while you hold the key.' },
    { kind: 'accessibility', label: 'Accessibility', help: 'Types the text where your cursor is.' },
    { kind: 'inputMonitoring', label: 'Input Monitoring', help: 'Notices the dictation key from any app.' },
  ];

  function granted(kind: PermissionKind) {
    const p = app.snapshot?.permissions;
    if (!p) return false;
    return kind === 'microphone' ? p.microphone === 'granted' : p[kind];
  }

  const allGranted = $derived(app.missingPermissions.length === 0);
  const model = $derived(app.activeModelInfo);
  const modelReady = $derived(!!model?.downloaded && app.snapshot?.modelState === 'ready');
  const rec = $derived(app.snapshot?.recState ?? 'idle');
  const cap = $derived(pttCap(app.settings?.pttKey));
  const toggleKeys = $derived(acceleratorParts(app.settings?.toggleShortcut));

  /** Dictations that arrived while the practice step was open. */
  let practice = $state<HistoryRecord[]>([]);

  onMount(() => {
    // System Settings changes don't always emit an event, so check while the permissions step is open.
    const poll = setInterval(() => {
      if (step === 0) void app.refreshPermissions();
    }, 2000);
    const un = on('transcription-complete', ({ record }) => {
      if (step === 2) practice = [...practice, record];
    });
    return () => {
      clearInterval(poll);
      void un.then((u) => u());
    };
  });

  async function goTo(i: number) {
    step = i;
    await tick();
    heading?.focus();
  }

  function finish() {
    void app.updateSettings({ onboardingDone: true });
    app.go('transcripts');
  }

  // Hold-to-talk fallback for people whose key isn't working yet.
  let holding = false;
  function holdStart() {
    if (holding || rec !== 'idle') return;
    holding = true;
    void api.startRecording();
  }
  function holdEnd() {
    if (!holding) return;
    holding = false;
    void api.stopRecording();
  }
</script>

<div class="onboarding">
  <div class="titlebar" data-tauri-drag-region></div>

  <div class="column">
    <div class="brand"><Logo size={20} dot="var(--signal)" /> {PRODUCT}</div>

    <ol class="steps" aria-label="Setup steps">
      {#each STEPS as label, i (label)}
        {@const done = i < step}
        <li class="step" class:current={i === step} class:done aria-current={i === step ? 'step' : undefined}>
          {#if done}<CheckIcon size={13} strokeWidth={2} aria-hidden="true" />{/if}
          {label}{#if done}<span class="sr-only"> (done)</span>{/if}
        </li>
      {/each}
    </ol>

    <section class="body" aria-labelledby="setup-heading">
      {#if step === 0}
        <h1 id="setup-heading" tabindex="-1" bind:this={heading}>Three switches in System Settings</h1>
        <p class="lead">
          Each button opens the right page in System Settings. Turn {PRODUCT} on there and come back; this page notices
          by itself.
        </p>
        <ul class="list">
          {#each PERMISSIONS as p (p.kind)}
            {@const ok = granted(p.kind)}
            <li class="item">
              <div>
                <p class="item-label">{p.label}</p>
                <p class="item-help">{p.help}</p>
              </div>
              {#if ok}
                <span class="allowed status-ok"><CheckIcon size={14} strokeWidth={2} aria-hidden="true" />Allowed</span>
              {:else}
                <button class="btn btn-sm" onclick={() => app.requestPermission(p.kind)}>Allow…</button>
              {/if}
            </li>
          {/each}
        </ul>
      {:else if step === 1}
        <h1 id="setup-heading" tabindex="-1" bind:this={heading}>Pick a speech model</h1>
        <p class="lead">It runs on this Mac, so it needs a one-time download. You can switch any time in Settings.</p>
        <div class="models"><ModelList /></div>
      {:else}
        <h1 id="setup-heading" tabindex="-1" bind:this={heading}>Try it</h1>
        <p class="lead">
          {#if cap}
            Hold <kbd class="keycap">{cap}</kbd>, say a sentence, and let go.
          {:else if toggleKeys.length}
            Press <Keycaps keys={toggleKeys} />, say a sentence, and press it again.
          {:else}
            Hold the button below, say a sentence, and let go.
          {/if}
        </p>
        <div class="practice" aria-live="polite">
          {#if rec === 'listening'}
            <div class="live"><Waveform levels={app.levels} bars={25} height={32} /><span>Listening</span></div>
          {:else if rec === 'processing'}
            <p class="pending">Writing it out…</p>
          {:else if practice.length === 0}
            <p class="placeholder">Your words will appear here.</p>
          {/if}
          {#each practice as r (r.id)}
            <p class="said selectable">{r.text}</p>
          {/each}
        </div>
        <div class="practice-foot">
          {#if practice.length > 0}
            <p class="status-ok worked">That worked. In other apps the text is typed where your cursor is.</p>
          {:else}
            <p class="hint">Key not doing anything?</p>
          {/if}
          <button
            class="btn btn-sm hold"
            onpointerdown={(e) => e.button === 0 && holdStart()}
            onpointerup={holdEnd}
            onpointerleave={holdEnd}
            onkeydown={(e) => {
              if ((e.key === ' ' || e.key === 'Enter') && !e.repeat) {
                e.preventDefault();
                holdStart();
              }
            }}
            onkeyup={(e) => (e.key === ' ' || e.key === 'Enter') && holdEnd()}
          >
            Hold here to talk
          </button>
        </div>
      {/if}
    </section>

    <footer class="foot">
      <button class="btn btn-quiet" onclick={finish}>Set up later</button>
      <span class="spacer"></span>
      {#if step === 1 && !modelReady}
        <span class="wait">{model?.downloaded ? 'Loading the model…' : 'Download a model to continue.'}</span>
      {/if}
      {#if step > 0}
        <button class="btn btn-quiet" onclick={() => goTo(step - 1)}>Back</button>
      {/if}
      {#if step < STEPS.length - 1}
        <button class="btn btn-signal" disabled={step === 1 && !modelReady} onclick={() => goTo(step + 1)}>
          {step === 0 && !allGranted ? 'Continue anyway' : 'Continue'}
        </button>
      {:else}
        <button class="btn btn-signal" onclick={finish}>Done</button>
      {/if}
    </footer>
  </div>
</div>

<style>
  .onboarding {
    height: 100%;
    overflow-y: auto;
    background: var(--surface);
  }
  .titlebar {
    position: sticky;
    top: 0;
    height: var(--toolbar-height);
  }
  .column {
    display: flex;
    flex-direction: column;
    max-width: 560px;
    min-height: calc(100% - var(--toolbar-height));
    margin: 0 auto;
    padding: 8px 32px 40px;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    font-weight: 700;
  }
  .steps {
    display: flex;
    gap: 20px;
    margin-top: 40px;
    font-size: 13px;
    color: var(--ink-3);
  }
  .step {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .step.current {
    color: var(--ink);
    font-weight: 700;
  }
  .step.done {
    color: var(--ink-2);
  }
  .body {
    margin-top: 16px;
  }
  h1 {
    font-size: 32px;
    line-height: 38px;
    letter-spacing: -0.01em;
    outline: none;
  }
  .lead {
    max-width: 46ch;
    margin-top: 10px;
    font-size: 15px;
    line-height: 22px;
    color: var(--ink-2);
  }
  .list {
    margin-top: 28px;
  }
  .item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 14px 0;
  }
  .item + .item {
    border-top: 1px solid var(--line);
  }
  .item-label {
    font-size: 13px;
    font-weight: 500;
  }
  .item-help {
    margin-top: 2px;
    font-size: 12px;
    color: var(--ink-2);
  }
  .allowed {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 13px;
    font-weight: 500;
  }
  .models {
    margin-top: 16px;
  }
  .practice {
    min-height: 168px;
    margin-top: 28px;
    padding: 22px 24px;
    border-radius: var(--radius-sheet);
    background: var(--bg);
  }
  .placeholder,
  .pending {
    font-family: var(--font-text);
    font-style: italic;
    font-size: 17px;
    line-height: 28px;
    color: var(--ink-3);
  }
  .pending {
    color: var(--ink-2);
  }
  .live {
    display: flex;
    align-items: center;
    gap: 14px;
    font-size: 13px;
    color: var(--ink-2);
  }
  .said {
    font-family: var(--font-text);
    font-size: 17px;
    line-height: 28px;
    color: var(--ink);
  }
  .said + .said {
    margin-top: 10px;
  }
  .live + .said,
  .pending + .said {
    margin-top: 14px;
  }
  .practice-foot {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 10px;
    margin-top: 10px;
  }
  .hint,
  .worked {
    flex: 1;
    font-size: 12px;
    color: var(--ink-2);
  }
  .worked {
    color: var(--ok);
  }
  .hold {
    touch-action: none;
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: auto;
    padding-top: 40px;
  }
  .spacer {
    flex: 1;
  }
  .wait {
    margin-right: 6px;
    font-size: 12px;
    color: var(--ink-2);
  }
</style>
