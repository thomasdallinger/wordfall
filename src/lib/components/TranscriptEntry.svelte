<script lang="ts">
  import { CheckIcon, CopyIcon, PencilIcon, Trash2Icon } from '@lucide/svelte';
  import { formatTime, languageName } from '../format';
  import { IS_WINDOWS, primaryMod } from '../platform';
  import { app } from '../state.svelte';
  import type { HistoryRecord } from '../types';

  let {
    record,
    query = '',
    oncopy,
    ondelete,
    oncorrect,
    onverify,
  }: {
    record: HistoryRecord;
    /** Search text to highlight. */
    query?: string;
    oncopy: () => void;
    ondelete: () => void;
    /** Save corrected text as ground truth; resolve true to close the editor. */
    oncorrect: (text: string) => Promise<boolean>;
    /** Mark the current text as exactly right. */
    onverify: () => void;
  } = $props();

  const verified = $derived(record.reference != null);
  const time = $derived(formatTime(record.timestamp));
  /** Only worth showing when the user dictates in more than one language. */
  const language = $derived(
    record.language && (app.settings?.languages.length ?? 0) !== 1 ? languageName(record.language) : null,
  );

  /** Plain and matching segments, for highlighting search hits in place. */
  const segments = $derived.by(() => {
    const q = query.trim();
    if (!q) return [{ text: record.text, hit: false }];
    const lower = record.text.toLowerCase();
    const needle = q.toLowerCase();
    const out: { text: string; hit: boolean }[] = [];
    let i = 0;
    for (let at = lower.indexOf(needle); at !== -1; at = lower.indexOf(needle, i)) {
      if (at > i) out.push({ text: record.text.slice(i, at), hit: false });
      out.push({ text: record.text.slice(at, at + needle.length), hit: true });
      i = at + needle.length;
    }
    if (i < record.text.length) out.push({ text: record.text.slice(i), hit: false });
    return out;
  });

  let entry: HTMLElement | undefined = $state();
  let editing = $state(false);
  let saving = $state(false);
  let draft = $state('');

  function startEdit() {
    draft = record.text;
    editing = true;
  }

  function stopEdit() {
    editing = false;
    entry?.focus();
  }

  async function save() {
    const text = draft.trim();
    if (!text || saving) return;
    saving = true;
    const ok = await oncorrect(text);
    saving = false;
    if (ok) stopEdit();
  }

  /** Grow the textarea with its content and put the caret at the end. */
  function autosize(node: HTMLTextAreaElement) {
    const fit = () => {
      node.style.height = 'auto';
      node.style.height = `${node.scrollHeight}px`;
    };
    fit();
    node.focus();
    node.setSelectionRange(node.value.length, node.value.length);
    node.addEventListener('input', fit);
    return () => node.removeEventListener('input', fit);
  }

  /** ↑/↓ walk the list; Enter corrects; ⌘C (Ctrl+C) copies the whole transcript. */
  function onkeydown(e: KeyboardEvent) {
    if (e.target !== entry) return;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const all = Array.from(document.querySelectorAll<HTMLElement>('[data-entry]'));
      all[all.indexOf(entry) + (e.key === 'ArrowDown' ? 1 : -1)]?.focus();
    } else if (e.key === 'Enter') {
      e.preventDefault();
      startEdit();
    } else if (e.key === 'c' && primaryMod(e) && !window.getSelection()?.toString()) {
      e.preventDefault();
      oncopy();
    }
  }
</script>

<!-- Each entry is a keyboard stop for list navigation (↑/↓, Enter, ⌘C); its buttons stay the real controls. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<article
  class={['entry', { editing }]}
  tabindex="0"
  data-entry
  aria-label="Transcript from {time}{verified ? ', verified' : ''}"
  aria-keyshortcuts={IS_WINDOWS ? 'Enter Control+C' : 'Enter Meta+C'}
  bind:this={entry}
  {onkeydown}
>
  <div class="margin">
    <time datetime={record.timestamp}>{time}</time>
    {#if language}<span>{language}</span>{/if}
    {#if verified}<span class="verified status-ok">Verified</span>{/if}
  </div>

  {#if editing}
    <div class="editor">
      <textarea
        class="words selectable"
        bind:value={draft}
        {@attach autosize}
        aria-label="Corrected transcript"
        onkeydown={(e) => {
          if (e.key === 'Escape') {
            e.preventDefault();
            e.stopPropagation();
            stopEdit();
          } else if (e.key === 'Enter' && primaryMod(e)) {
            e.preventDefault();
            void save();
          }
        }}
      ></textarea>
      <div class="edit-bar">
        <p class="edit-help">
          {#if record.has_audio}
            Write exactly what you said. The recording and this text are kept together as a test case.
          {:else}
            Write exactly what you said. No recording was kept, so this only fixes the text.
          {/if}
        </p>
        <button class="btn btn-quiet" onclick={stopEdit}>Cancel</button>
        <button class="btn btn-signal" disabled={!draft.trim() || saving} onclick={save} title={IS_WINDOWS ? 'Save (Ctrl+Enter)' : 'Save (⌘ Return)'}>Save</button>
      </div>
    </div>
  {:else}
    <p class="words selectable">
      {#each segments as seg, i (i)}{#if seg.hit}<mark>{seg.text}</mark>{:else}{seg.text}{/if}{/each}
    </p>
    <div class="actions">
      <button class="icon-btn" title="Copy" aria-label="Copy transcript" onclick={oncopy}>
        <CopyIcon size={15} strokeWidth={1.5} />
      </button>
      <button class="icon-btn" title="Correct" aria-label="Correct transcript" onclick={startEdit}>
        <PencilIcon size={15} strokeWidth={1.5} />
      </button>
      {#if !verified}
        <button class="icon-btn" title="Mark as right" aria-label="Mark transcript as right" onclick={onverify}>
          <CheckIcon size={16} strokeWidth={1.5} />
        </button>
      {/if}
      <button class="icon-btn is-bad" title="Delete" aria-label="Delete transcript" onclick={ondelete}>
        <Trash2Icon size={15} strokeWidth={1.5} />
      </button>
    </div>
  {/if}
</article>

<style>
  .entry {
    position: relative;
    display: grid;
    grid-template-columns: 72px minmax(0, 1fr) 120px;
    column-gap: 16px;
    margin: 0 -12px;
    padding: 10px 12px;
    border-radius: var(--radius-sheet);
    transition: background-color var(--dur) var(--ease);
  }
  .entry:hover,
  .entry:focus-within,
  .entry.editing {
    background: var(--bg);
  }
  .entry:focus-visible {
    outline-offset: 0;
  }
  .margin {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 2px;
    padding-top: 4px;
    font-size: 12px;
    line-height: 16px;
    color: var(--ink-3);
    white-space: nowrap;
  }
  .verified {
    font-weight: 500;
  }
  .words {
    max-width: 62ch;
    font-family: var(--font-text);
    font-size: 16px;
    line-height: 26px;
    color: var(--ink);
    overflow-wrap: anywhere;
    text-wrap: pretty;
    /* Dictated lists arrive as one item per line. */
    white-space: pre-line;
    cursor: text;
  }
  mark {
    color: inherit;
    background: color-mix(in srgb, var(--warn) 24%, transparent);
    border-radius: 2px;
  }
  .actions {
    display: flex;
    align-items: flex-start;
    justify-content: flex-end;
    gap: 2px;
    opacity: 0;
    transition: opacity var(--dur) var(--ease);
  }
  .entry:hover .actions,
  .entry:focus-within .actions {
    opacity: 1;
  }
  .editor {
    grid-column: 2 / 4;
  }
  textarea.words {
    display: block;
    width: 100%;
    max-width: none;
    padding: 6px 10px;
    margin: -6px -10px 0;
    width: calc(100% + 20px);
    resize: none;
    overflow: hidden;
    white-space: pre-wrap;
    border-radius: var(--radius-control);
    background: var(--surface);
    border: 1px solid var(--ink-3);
    outline: none;
  }
  .edit-bar {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 10px;
  }
  .edit-help {
    flex: 1;
    min-width: 0;
    margin-right: 8px;
    font-size: 12px;
    line-height: 17px;
    color: var(--ink-2);
  }
</style>
