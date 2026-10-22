<script lang="ts">
  import { ChevronLeftIcon, ChevronRightIcon } from '@lucide/svelte';
  import Waveform from '../../lib/components/Waveform.svelte';
  import { categoryLabel, formatClock, formatWer, missedTerms, termSegments, werClass, werGrade } from '../../lib/lab';
  import type { LabRecorder } from '../../lib/labRecorder.svelte';
  import type { LabPrompt } from '../../lib/types';

  let {
    prompt,
    index,
    total,
    recorder,
    reviewing,
    lastMs = null,
    onprev,
    onnext,
    onrerecord,
  }: {
    prompt: LabPrompt;
    index: number;
    total: number;
    recorder: LabRecorder;
    /** Show the last result instead of the record button. */
    reviewing: boolean;
    lastMs?: number | null;
    onprev: () => void;
    onnext: () => void;
    onrerecord: () => void;
  } = $props();

  let card: HTMLElement | undefined = $state();

  export function focus() {
    card?.focus({ preventScroll: true });
  }

  const phase = $derived(recorder.phase);
  const busy = $derived(phase !== 'idle');
  const showResult = $derived(!busy && reviewing && prompt.hypothesis !== null);
  const missed = $derived(showResult ? missedTerms(prompt.terms, prompt.hypothesis ?? '') : []);
  const segments = $derived(termSegments(prompt.text, prompt.terms));
  const size = $derived(prompt.text.length < 90 ? 'lg' : prompt.text.length < 220 ? 'md' : 'sm');
  const grade = $derived(prompt.wer !== null ? werGrade(prompt.wer) : null);

  const hint = $derived(
    phase === 'processing'
      ? 'Checking what was heard…'
      : phase === 'recording'
        ? recorder.latched
          ? 'Click again when you’re done. Esc throws it away.'
          : 'Let go when you’re done. Esc throws it away.'
        : 'Hold the button while you read, or click once to start and again to stop.',
  );

  /** Space records only from the card itself or the record button, so other buttons still activate. */
  function spaceRecords(e: KeyboardEvent) {
    const t = e.target as HTMLElement;
    return t === card || t.dataset.record !== undefined;
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === ' ' && spaceRecords(e)) {
      e.preventDefault();
      if (!e.repeat) recorder.press(prompt.id);
    } else if (e.key === 'Escape' && phase === 'recording') {
      e.preventDefault();
      void recorder.cancel();
    } else if (e.key === 'ArrowLeft' && !busy) {
      e.preventDefault();
      onprev();
    } else if (e.key === 'ArrowRight' && !busy) {
      e.preventDefault();
      onnext();
    }
  }

  function onkeyup(e: KeyboardEvent) {
    if (e.key !== ' ' || !spaceRecords(e)) return;
    e.preventDefault(); // keeps the record button from firing a click
    recorder.release();
  }
</script>

<svelte:window onblur={() => recorder.release()} />

<!-- The stage is a keyboard target (hold Space, ←/→, Esc); the record button stays the interactive control. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div
  class="stage"
  role="group"
  class:live={phase === 'recording'}
  tabindex="0"
  aria-label="Prompt {index + 1} of {total}. Hold Space to record, arrow keys to switch."
  aria-keyshortcuts="Space ArrowLeft ArrowRight Escape"
  bind:this={card}
  {onkeydown}
  {onkeyup}
  onfocusout={(e) => {
    if (!card?.contains(e.relatedTarget as Node | null)) recorder.release();
  }}
>
  <header class="head">
    <span>{categoryLabel(prompt.category)}</span>
    <span class="count">{index + 1} of {total}</span>
    <span class="nav">
      <button class="icon-btn" title="Previous (←)" aria-label="Previous prompt" disabled={busy || index === 0} onclick={onprev}>
        <ChevronLeftIcon size={16} strokeWidth={1.5} />
      </button>
      <button class="icon-btn" title="Next (→)" aria-label="Next prompt" disabled={busy || index >= total - 1} onclick={onnext}>
        <ChevronRightIcon size={16} strokeWidth={1.5} />
      </button>
    </span>
  </header>

  <p class="prompt {size} selectable">
    {#each segments as seg, i (i)}{#if seg.term}<span class="term" class:missed={missed.includes(seg.term)}>{seg.text}</span>{:else}{seg.text}{/if}{/each}
  </p>

  {#if showResult}
    <div class="result" aria-live="polite">
      <p class="heard-label">What the model heard</p>
      <p class="heard selectable">{prompt.hypothesis}</p>
      <p class="verdict">
        {#if grade && prompt.wer !== null}
          <span class={werClass(grade.tone)}><strong>{grade.label}.</strong></span>
          {formatWer(prompt.wer)} of words wrong{#if lastMs !== null}, checked in {lastMs} ms{/if}.
        {/if}
        {#if prompt.terms.length > 0}
          {#if missed.length > 0}
            <span class="status-bad">Missed {missed.join(', ')}.</span>
          {:else}
            Every key term came through.
          {/if}
        {/if}
      </p>
      <div class="result-actions">
        <button class="btn btn-quiet" onclick={onrerecord}>Record again</button>
        <button class="btn" disabled={index >= total - 1} onclick={onnext}>Next prompt</button>
      </div>
    </div>
  {:else}
    <div class="controls">
      <span class="side left">
        {#if phase === 'recording'}<Waveform levels={recorder.levels} bars={17} height={28} />{/if}
      </span>
      <button
        class="rec"
        class:recording={phase === 'recording'}
        data-record
        aria-label={phase === 'recording' ? 'Stop recording' : 'Record this prompt'}
        disabled={phase === 'processing'}
        onpointerdown={(e) => {
          if (e.button === 0) recorder.press(prompt.id);
        }}
        onpointerup={() => recorder.release()}
        onpointerleave={() => recorder.release()}
        onpointercancel={() => recorder.release()}
        onclick={(e) => {
          // Pointer clicks are handled above; detail === 0 means keyboard activation (Enter).
          if (e.detail === 0) recorder.toggle(prompt.id);
        }}
        oncontextmenu={(e) => e.preventDefault()}
      >
        <span class="rec-mark" class:stop={phase === 'recording'} class:wait={phase === 'processing'}></span>
      </button>
      <span class="side right">
        {#if phase === 'recording'}
          <span class="clock" aria-label="Recording time">{formatClock(recorder.elapsed)}</span>
        {/if}
      </span>
    </div>
    <p class="hint" aria-live="polite">
      {hint}
      {#if phase === 'idle'}<kbd class="keycap">Space</kbd> works too.{/if}
    </p>
  {/if}
</div>

<style>
  .stage {
    padding: 18px 24px 22px;
    border-radius: var(--radius-sheet);
    background: var(--bg);
    outline: none;
    transition: box-shadow var(--dur) var(--ease);
  }
  .stage:focus-visible {
    box-shadow: 0 0 0 2px var(--focus);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
    color: var(--ink-2);
  }
  .count {
    font-variant-numeric: tabular-nums;
  }
  .nav {
    display: flex;
    margin-left: auto;
    margin-right: -8px;
  }
  .prompt {
    margin: 18px 0 26px;
    max-width: 34em;
    font-family: var(--font-text);
    color: var(--ink);
    text-wrap: pretty;
  }
  .prompt.lg {
    font-size: 28px;
    line-height: 40px;
  }
  .prompt.md {
    font-size: 22px;
    line-height: 34px;
  }
  .prompt.sm {
    font-size: 18px;
    line-height: 29px;
  }
  .term {
    text-decoration: underline;
    text-decoration-style: dotted;
    text-decoration-color: var(--ink-3);
    text-decoration-thickness: 1.5px;
    text-underline-offset: 6px;
  }
  .term.missed {
    color: var(--bad);
    text-decoration-style: wavy;
    text-decoration-color: var(--bad);
    text-decoration-thickness: 1px;
  }
  .controls {
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    align-items: center;
    column-gap: 20px;
  }
  .side {
    display: flex;
  }
  .left {
    justify-content: flex-end;
  }
  .rec {
    display: grid;
    place-items: center;
    width: 56px;
    height: 56px;
    border-radius: 999px;
    background: var(--signal);
    touch-action: none;
    transition:
      transform var(--dur) var(--ease),
      background-color var(--dur) var(--ease);
  }
  .rec:hover {
    background: color-mix(in srgb, var(--signal) 88%, var(--ink));
  }
  .rec:active {
    transform: scale(0.96);
  }
  .rec:disabled {
    background: var(--press);
  }
  .rec-mark {
    width: 18px;
    height: 18px;
    border-radius: 999px;
    background: var(--signal-ink);
    transition:
      border-radius var(--dur) var(--ease),
      width var(--dur) var(--ease),
      height var(--dur) var(--ease);
  }
  .rec-mark.stop {
    width: 16px;
    height: 16px;
    border-radius: 3px;
  }
  .rec-mark.wait {
    background: var(--ink-2);
    animation: breathe 1.6s ease-in-out infinite;
  }
  .clock {
    font-size: 13px;
    font-weight: 500;
    color: var(--ink-2);
  }
  .hint {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    min-height: 20px;
    margin-top: 14px;
    font-size: 12px;
    color: var(--ink-2);
    text-align: center;
  }
  .result {
    padding-top: 18px;
    border-top: 1px solid var(--line);
  }
  .heard-label {
    font-size: 12px;
    color: var(--ink-2);
  }
  .heard {
    margin-top: 4px;
    font-family: var(--font-text);
    font-size: 17px;
    line-height: 28px;
  }
  .verdict {
    margin-top: 10px;
    font-size: 13px;
    line-height: 19px;
    color: var(--ink-2);
  }
  .verdict strong {
    font-weight: 700;
  }
  .result-actions {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
    margin-top: 16px;
  }
</style>
