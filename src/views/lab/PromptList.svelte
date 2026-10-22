<script lang="ts">
  import { Trash2Icon } from '@lucide/svelte';
  import Segmented from '../../lib/components/Segmented.svelte';
  import { LAB_CATEGORIES, formatWer, werClass, werGrade } from '../../lib/lab';
  import type { LabPrompt } from '../../lib/types';

  type Filter = 'all' | 'todo' | 'done';

  let {
    prompts,
    currentId,
    onselect,
    ondelete,
  }: {
    prompts: LabPrompt[];
    currentId: string | null;
    onselect: (id: string) => void;
    ondelete: (id: string) => void;
  } = $props();

  let filter = $state<Filter>('all');

  const groups = $derived(
    LAB_CATEGORIES.map((c) => {
      const all = prompts.filter((p) => p.category === c.id);
      const items = all.filter((p) => filter === 'all' || (filter === 'done') === p.recorded);
      return { ...c, items, done: all.filter((p) => p.recorded).length, count: all.length };
    }).filter((g) => g.items.length > 0),
  );
</script>

<div class="head">
  <h2>All prompts</h2>
  <Segmented
    label="Which prompts to list"
    value={filter}
    onchange={(v) => (filter = v)}
    options={[
      { value: 'all', label: 'All' },
      { value: 'todo', label: 'To record' },
      { value: 'done', label: 'Recorded' },
    ]}
  />
</div>

{#if groups.length === 0}
  <p class="none">
    {filter === 'todo' ? 'Every prompt has a clip. Record any of them again to replace it.' : 'No clips yet. Read the prompt above to make the first one.'}
  </p>
{/if}

{#each groups as g (g.id)}
  <section class="group" aria-labelledby="lab-{g.id}">
    <h3 id="lab-{g.id}">{g.label} <span class="tally">{g.done} of {g.count}</span></h3>
    <ul>
      {#each g.items as p (p.id)}
        {@const selected = p.id === currentId}
        {@const grade = p.wer !== null ? werGrade(p.wer) : null}
        <li class="row" class:selected>
          <button class="pick" aria-current={selected ? 'true' : undefined} onclick={() => onselect(p.id)}>
            <span class="text" class:todo={!p.recorded}>{p.text}</span>
          </button>
          <span class="score">
            {#if grade && p.wer !== null}
              <span class={werClass(grade.tone)}>{formatWer(p.wer)} wrong</span>
            {:else}
              <span class="todo-label">Not recorded</span>
            {/if}
          </span>
          <span class="del">
            {#if p.recorded}
              <button class="icon-btn is-bad" title="Delete clip" aria-label="Delete the clip for “{p.text}”" onclick={() => ondelete(p.id)}>
                <Trash2Icon size={14} strokeWidth={1.5} />
              </button>
            {/if}
          </span>
        </li>
      {/each}
    </ul>
  </section>
{/each}

<style>
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 44px;
  }
  h2 {
    font-size: 17px;
    line-height: 24px;
  }
  .none {
    margin-top: 16px;
    font-size: 13px;
    color: var(--ink-2);
  }
  .group {
    margin-top: 22px;
  }
  h3 {
    margin-bottom: 4px;
    font-size: 13px;
  }
  .tally {
    margin-left: 6px;
    font-weight: 400;
    color: var(--ink-2);
    font-variant-numeric: tabular-nums;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 96px 28px;
    align-items: center;
    column-gap: 10px;
    margin: 0 -10px;
    padding: 0 10px;
    border-radius: var(--radius-control);
  }
  .row:hover {
    background: var(--bg);
  }
  .row.selected {
    background: var(--press);
  }
  .pick {
    min-width: 0;
    min-height: 34px;
    text-align: left;
  }
  .pick:focus-visible {
    outline-offset: 0;
  }
  .text {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
  }
  .text.todo {
    color: var(--ink-2);
  }
  .score {
    font-size: 12px;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .todo-label {
    color: var(--ink-3);
  }
  .del {
    display: flex;
    opacity: 0;
    transition: opacity var(--dur) var(--ease);
  }
  .row:hover .del,
  .row:focus-within .del {
    opacity: 1;
  }
</style>
