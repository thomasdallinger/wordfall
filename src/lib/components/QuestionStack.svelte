<script lang="ts">
  import { onMount } from 'svelte';
  import { app } from '../state.svelte';
  import type { MeetingQuestion, QuestionKind, QuestionStatus } from '../types';

  let {
    meetingId,
    startedAt,
    questions,
    status,
  }: {
    meetingId: string;
    /** When the meeting started (question times count from there). */
    startedAt: string;
    questions: MeetingQuestion[];
    status: QuestionStatus;
  } = $props();

  /** How long a question the talk answered stays, saying so, before it leaves. */
  const ANSWERED_FOR_MS = 4000;

  const KIND: Record<QuestionKind, string> = {
    understand: 'Understand',
    challenge: 'Challenge',
    'follow-up': 'Follow-up',
  };

  // Meeting time, so a question the talk just answered can stay a moment, saying so.
  let now = $state(Date.now());
  onMount(() => {
    const t = setInterval(() => (now = Date.now()), 500);
    return () => clearInterval(t);
  });
  const meetingMs = $derived(now - Date.parse(startedAt));

  const live = $derived(
    questions.filter((q) => q.state === 'live' || (q.state === 'answered' && meetingMs - q.changedMs < ANSWERED_FOR_MS)),
  );
  const later = $derived(questions.filter((q) => q.state === 'later'));
  let showLater = $state(false);

  const empty = $derived.by(() => {
    switch (status) {
      case 'no-model':
        return 'Download a summary model in Settings › Meetings to get questions while you talk.';
      case 'starting':
        return 'Starting the model…';
      case 'failed':
        return 'Questions stopped because the model failed. The transcript goes on.';
      case 'off':
        return 'No questions are proposed for this meeting.';
      default:
        return 'Questions to ask appear here as the talk goes on.';
    }
  });

  function act(number: number, what: 'later' | 'asked' | 'deleted') {
    void app.actOnQuestion(meetingId, number, what);
  }
</script>

<div class="stack">
  {#if live.length === 0}
    <p class="empty">{empty}</p>
  {/if}
  <ol class="cards" aria-label="Questions to ask">
    {#each live as q (q.number)}
      <li class={['card', { answered: q.state === 'answered' }]}>
        <p class="kind">{KIND[q.kind]}</p>
        <p class="text selectable">{q.text}</p>
        {#if q.state === 'answered'}
          <p class="note" role="status">Answered in the talk</p>
        {:else}
          <div class="acts">
            <button class="act" title="You asked it" onclick={() => act(q.number, 'asked')}>Asked</button>
            <button class="act" title="Keep it for later" onclick={() => act(q.number, 'later')}>Later</button>
            <button class="act" title="Remove it" onclick={() => act(q.number, 'deleted')}>Delete</button>
          </div>
        {/if}
      </li>
    {/each}
  </ol>

  {#if later.length > 0}
    <button class="later-toggle" aria-expanded={showLater} onclick={() => (showLater = !showLater)}>
      Later ({later.length})
    </button>
    {#if showLater}
      <ol class="cards later" aria-label="Questions kept for later">
        {#each later as q (q.number)}
          <li class="card">
            <p class="kind">{KIND[q.kind]}</p>
            <p class="text selectable">{q.text}</p>
            <div class="acts">
              <button class="act" title="You asked it" onclick={() => act(q.number, 'asked')}>Asked</button>
              <button class="act" title="Remove it" onclick={() => act(q.number, 'deleted')}>Delete</button>
            </div>
          </li>
        {/each}
      </ol>
    {/if}
  {/if}
</div>

<style>
  .stack {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .empty {
    font-family: var(--font-text);
    font-style: italic;
    font-size: 14px;
    line-height: 21px;
    color: var(--ink-2);
  }
  .cards {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .card {
    padding: 10px 12px 6px;
    border-radius: var(--radius-sheet);
    background: var(--surface);
    border: 1px solid var(--line);
    transition: opacity var(--dur) var(--ease);
  }
  .card.answered {
    opacity: 0.6;
  }
  .kind {
    font-size: 12px;
    line-height: 16px;
    font-weight: 700;
    color: var(--ink-2);
  }
  .text {
    margin-top: 2px;
    font-family: var(--font-text);
    font-size: 15px;
    line-height: 22px;
    color: var(--ink);
    overflow-wrap: anywhere;
  }
  .acts {
    display: flex;
    gap: 2px;
    margin: 4px 0 0 -6px;
  }
  .act {
    height: 24px;
    padding: 0 6px;
    border-radius: var(--radius-control);
    font-size: 12px;
    font-weight: 500;
    color: var(--ink-2);
  }
  .act:hover {
    background: var(--hover);
    color: var(--ink);
  }
  .note {
    margin: 4px 0 4px;
    font-size: 12px;
    font-weight: 500;
    color: var(--ok);
  }
  .later-toggle {
    align-self: flex-start;
    height: 24px;
    padding: 0 6px;
    margin-left: -6px;
    border-radius: var(--radius-control);
    font-size: 12px;
    font-weight: 700;
    color: var(--ink-2);
  }
  .later-toggle:hover {
    background: var(--hover);
    color: var(--ink);
  }
  @media (prefers-reduced-motion: reduce) {
    .card {
      transition: none;
    }
  }
</style>
