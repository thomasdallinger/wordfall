<script lang="ts">
  import { PencilIcon, PlusIcon, Trash2Icon, XIcon } from '@lucide/svelte';
  import Page from '../lib/components/Page.svelte';
  import Switch from '../lib/components/Switch.svelte';
  import { api } from '../lib/api';
  import { plural } from '../lib/format';
  import { app } from '../lib/state.svelte';
  import type { DictEntry, Suggestion } from '../lib/types';

  const enabled = $derived(app.settings?.dictionaryEnabled ?? false);
  const hintsOff = $derived(app.settings?.promptMode === 'none');

  function reasonText(s: Suggestion): string {
    const r = s.reason.toLowerCase();
    if (r.includes('knownfailure') || r.includes('known_failure') || r.includes('misrecogn')) return 'A common mishearing';
    return `Seen ${plural(s.occurrenceCount, 'time')}`;
  }

  // ---- replacements
  let heard = $state('');
  let written = $state('');
  let editingId = $state<string | null>(null);
  let draft = $state({ heard: '', written: '' });

  async function add(e: SubmitEvent) {
    e.preventDefault();
    const original = heard.trim();
    const replacement = written.trim();
    if (!original || !replacement) return;
    try {
      await api.upsertDictionary(original, replacement);
      heard = '';
      written = '';
      (e.currentTarget as HTMLFormElement | null)?.querySelector('input')?.focus();
      await app.loadDictionary();
    } catch (err) {
      app.error('The replacement was not added', err);
    }
  }

  function startEdit(d: DictEntry) {
    editingId = d.id;
    draft = { heard: d.original, written: d.replacement };
  }

  async function saveEdit(d: DictEntry) {
    const original = draft.heard.trim();
    const replacement = draft.written.trim();
    if (!original || !replacement) return;
    editingId = null;
    try {
      await api.upsertDictionary(original, replacement, d.id);
      await app.loadDictionary();
    } catch (err) {
      app.error('The replacement was not saved', err);
    }
  }

  async function removeEntry(id: string) {
    app.dictionary = app.dictionary.filter((d) => d.id !== id);
    try {
      await api.deleteDictionary(id);
    } catch (err) {
      app.error('The replacement is still there', err);
      void app.loadDictionary();
    }
  }

  // ---- suggestions
  let editingSuggestion = $state<string | null>(null);
  let suggestionDraft = $state('');

  async function accept(s: Suggestion, replacement?: string) {
    app.suggestions = app.suggestions.filter((x) => x.id !== s.id);
    editingSuggestion = null;
    try {
      await api.acceptSuggestion(s.id, replacement);
    } catch (e) {
      app.error('The suggestion was not added', e);
      void app.loadDictionary();
    }
  }

  async function dismiss(id: string) {
    app.suggestions = app.suggestions.filter((x) => x.id !== id);
    try {
      await api.dismissSuggestion(id);
    } catch (e) {
      app.error('The suggestion came back', e);
      void app.loadDictionary();
    }
  }

  async function dismissAll() {
    app.suggestions = [];
    try {
      await api.dismissAllSuggestions();
    } catch (e) {
      app.error('The suggestions came back', e);
      void app.loadDictionary();
    }
  }

  // ---- words to recognise
  let word = $state('');

  function addWord(term: string) {
    const t = term.trim();
    if (!t) return;
    if (app.vocabulary.some((v) => v.toLowerCase() === t.toLowerCase())) {
      app.toast('info', `“${t}” is already on the list`);
      return;
    }
    void app.setVocabulary([...app.vocabulary, t]);
  }

  function removeWord(term: string) {
    void app.setVocabulary(app.vocabulary.filter((v) => v !== term));
  }

  function focusSelect(node: HTMLInputElement) {
    node.focus();
    node.select();
  }
</script>

<Page title="Dictionary">
  <section class="part" aria-labelledby="replacements-title">
    <div class="block-head">
      <h2 id="replacements-title">Replacements</h2>
      <Switch checked={enabled} label="Use replacements" onchange={(v) => app.updateSettings({ dictionaryEnabled: v })} />
    </div>
    <p class="block-help">
      {#if enabled}
        Whenever the left side shows up in a transcript, it's swapped for the right side before anything is pasted.
      {:else}
        Paused. Transcripts are pasted exactly as heard until you switch this back on.
      {/if}
    </p>

    <form class="add" onsubmit={add}>
      <input class="field" placeholder="Heard as, like “post gress”" bind:value={heard} aria-label="Heard as" />
      <input class="field" placeholder="Write as, like “PostgreSQL”" bind:value={written} aria-label="Write as" />
      <button class="btn" type="submit" disabled={!heard.trim() || !written.trim()}>Add</button>
    </form>

    {#if app.suggestions.length > 0}
      <div class="suggested" role="group" aria-labelledby="suggested-title">
        <div class="suggested-head">
          <h3 id="suggested-title">Suggested</h3>
          <span class="muted">from your transcripts</span>
          <button class="btn btn-quiet btn-sm push" onclick={dismissAll}>Dismiss all</button>
        </div>
        <ul>
          {#each app.suggestions as s (s.id)}
            <li class="pair">
              <span class="heard">{s.detectedWord}</span>
              {#if editingSuggestion === s.id}
                <input
                  class="field compact"
                  bind:value={suggestionDraft}
                  {@attach focusSelect}
                  aria-label="Write “{s.detectedWord}” as"
                  onkeydown={(e) => {
                    if (e.key === 'Enter' && suggestionDraft.trim()) accept(s, suggestionDraft.trim());
                    if (e.key === 'Escape') editingSuggestion = null;
                  }}
                />
              {:else}
                <span class="written">{s.suggestedReplacement}</span>
              {/if}
              <span class="why">{reasonText(s)}</span>
              <span class="pair-actions always">
                {#if editingSuggestion === s.id}
                  <button class="btn btn-sm" disabled={!suggestionDraft.trim()} onclick={() => accept(s, suggestionDraft.trim())}>Add</button>
                  <button class="btn btn-quiet btn-sm" onclick={() => (editingSuggestion = null)}>Cancel</button>
                {:else}
                  <button class="btn btn-sm" onclick={() => accept(s)} aria-label="Add {s.detectedWord} as {s.suggestedReplacement}">Add</button>
                  <button
                    class="icon-btn"
                    title="Change before adding"
                    aria-label="Change the spelling for {s.detectedWord}"
                    onclick={() => {
                      editingSuggestion = s.id;
                      suggestionDraft = s.suggestedReplacement;
                    }}><PencilIcon size={14} strokeWidth={1.5} /></button
                  >
                  <button class="icon-btn" title="Dismiss" aria-label="Dismiss {s.detectedWord}" onclick={() => dismiss(s.id)}>
                    <XIcon size={15} strokeWidth={1.5} />
                  </button>
                {/if}
              </span>
            </li>
          {/each}
        </ul>
      </div>
    {/if}

    {#if app.dictionary.length > 0}
      <div class="entries" class:paused={!enabled}>
        <div class="pair head" aria-hidden="true">
          <span>Heard as</span>
          <span>Written as</span>
        </div>
        <ul aria-label="Replacements">
          {#each app.dictionary as d (d.id)}
            <li class="pair row">
              {#if editingId === d.id}
                <form class="edit" onsubmit={(e) => (e.preventDefault(), saveEdit(d))}>
                  <input class="field compact" bind:value={draft.heard} {@attach focusSelect} aria-label="Heard as" />
                  <input
                    class="field compact"
                    bind:value={draft.written}
                    aria-label="Written as"
                    onkeydown={(e) => e.key === 'Escape' && (editingId = null)}
                  />
                  <span class="why"></span>
                  <span class="pair-actions always">
                    <button class="btn btn-sm" type="submit">Save</button>
                    <button class="btn btn-quiet btn-sm" type="button" onclick={() => (editingId = null)}>Cancel</button>
                  </span>
                </form>
              {:else}
                <span class="heard">{d.original}</span>
                <span class="written">{d.replacement}</span>
                <span class="why"></span>
                <span class="pair-actions">
                  <button class="icon-btn" title="Edit" aria-label="Edit {d.original}" onclick={() => startEdit(d)}>
                    <PencilIcon size={14} strokeWidth={1.5} />
                  </button>
                  <button class="icon-btn is-bad" title="Delete" aria-label="Delete {d.original}" onclick={() => removeEntry(d.id)}>
                    <Trash2Icon size={14} strokeWidth={1.5} />
                  </button>
                </span>
              {/if}
            </li>
          {/each}
        </ul>
      </div>
    {:else}
      <p class="none">No replacements yet. Add one above whenever a word keeps coming out wrong.</p>
    {/if}
  </section>

  <section class="part" aria-labelledby="words-title">
    <div class="block-head">
      <h2 id="words-title">Words to recognise</h2>
    </div>
    <p class="block-help">
      Names and terms the model should expect. Nothing gets replaced; it just leans toward these spellings when it's
      unsure.
    </p>
    {#if hintsOff}
      <p class="off status-warn">
        Hints are switched off, so this list isn't used right now.
        <button class="link" onclick={() => app.go('settings', 'accuracy')}>Turn them on</button>
      </p>
    {/if}

    <form
      class="add single"
      onsubmit={(e) => {
        e.preventDefault();
        addWord(word);
        word = '';
      }}
    >
      <input class="field" placeholder="A name or term, like “Kubernetes”" bind:value={word} aria-label="Word to recognise" />
      <button class="btn" type="submit" disabled={!word.trim()}>Add</button>
    </form>

    {#if app.vocabulary.length > 0}
      <ul class="words" aria-label="Words to recognise">
        {#each app.vocabulary as term (term)}
          <li class="word">
            <span>{term}</span>
            <button class="word-x" aria-label="Remove {term}" title="Remove" onclick={() => removeWord(term)}>
              <XIcon size={12} strokeWidth={1.75} />
            </button>
          </li>
        {/each}
      </ul>
    {:else}
      <p class="none">The list is empty. Replacements above are always used as hints too.</p>
    {/if}

    {#if app.suggestedVocabulary.length > 0}
      <div class="from-projects">
        <span class="muted">From your project folders</span>
        {#each app.suggestedVocabulary as term (term)}
          <button class="btn btn-quiet btn-sm project" aria-label="Add {term}" onclick={() => addWord(term)}>
            <PlusIcon size={12} strokeWidth={1.75} />{term}
          </button>
        {/each}
      </div>
    {/if}
  </section>
</Page>

<style>
  .part + .part {
    margin-top: 48px;
  }
  .block-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    min-height: 28px;
  }
  h2 {
    font-size: 17px;
    line-height: 24px;
  }
  .block-help {
    max-width: 58ch;
    margin-top: 4px;
    font-size: 13px;
    line-height: 19px;
    color: var(--ink-2);
  }
  .off {
    margin-top: 8px;
    font-size: 13px;
  }
  .muted {
    font-size: 12px;
    color: var(--ink-2);
  }
  .add {
    display: grid;
    grid-template-columns: 1fr 1fr auto;
    gap: 8px;
    margin-top: 16px;
  }
  .add.single {
    grid-template-columns: minmax(0, 360px) auto;
    justify-content: start;
  }

  /* Heard → written pairs share one grid so columns line up across groups. */
  .pair {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) 150px 140px;
    align-items: center;
    column-gap: 12px;
    min-height: 40px;
    font-size: 13px;
  }
  .head {
    min-height: 28px;
    font-size: 12px;
    font-weight: 500;
    color: var(--ink-2);
    border-bottom: 1px solid var(--line);
  }
  .row {
    margin: 0 -10px;
    padding: 0 10px;
    border-radius: var(--radius-control);
  }
  .row:hover,
  .row:focus-within {
    background: var(--bg);
  }
  .heard {
    color: var(--ink-2);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .written {
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .paused .written {
    color: var(--ink-2);
    font-weight: 400;
  }
  .why {
    font-size: 12px;
    color: var(--ink-2);
  }
  .pair-actions {
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: 2px;
    opacity: 0;
    transition: opacity var(--dur) var(--ease);
  }
  .pair-actions.always,
  .row:hover .pair-actions,
  .row:focus-within .pair-actions {
    opacity: 1;
  }
  .edit {
    display: contents;
  }
  .compact {
    height: 26px;
  }
  .entries {
    margin-top: 24px;
  }

  .suggested {
    margin-top: 20px;
    padding: 10px 14px 6px;
    border-radius: var(--radius-sheet);
    background: var(--bg);
  }
  .suggested-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .suggested-head h3 {
    font-size: 13px;
  }
  .push {
    margin-left: auto;
  }
  .suggested .pair {
    min-height: 36px;
  }

  .none {
    margin-top: 16px;
    font-size: 13px;
    color: var(--ink-2);
  }
  .words {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 16px;
  }
  .word {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    height: 28px;
    padding: 0 4px 0 10px;
    border-radius: var(--radius-control);
    font-size: 13px;
    background: var(--well);
  }
  .word-x {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: 4px;
    color: var(--ink-3);
  }
  .word-x:hover {
    color: var(--ink);
    background: var(--press);
  }
  .from-projects {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px;
    margin-top: 16px;
  }
  .from-projects .muted {
    margin-right: 6px;
  }
  .project {
    gap: 4px;
    padding: 0 8px 0 6px;
    color: var(--ink);
    box-shadow: inset 0 0 0 1px var(--line);
  }
</style>
