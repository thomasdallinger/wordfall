<script lang="ts">
  import { CheckIcon, ListFilterIcon, SearchIcon, XIcon } from '@lucide/svelte';
  import { onMount } from 'svelte';
  import AttentionLine from '../lib/components/AttentionLine.svelte';
  import Menu from '../lib/components/Menu.svelte';
  import Page from '../lib/components/Page.svelte';
  import TranscriptEntry from '../lib/components/TranscriptEntry.svelte';
  import { api } from '../lib/api';
  import { dayKey, dayLabel, formatNumber, plural, pttCap } from '../lib/format';
  import { app } from '../lib/state.svelte';
  import type { HistoryRecord } from '../lib/types';

  const PAGE = 50;
  /** Bigger pages while a filter hides most rows, so matches show up quickly. */
  const FILTERED_PAGE = 200;
  type Filter = 'all' | 'audio' | 'verified';
  const FILTERS: { value: Filter; label: string }[] = [
    { value: 'all', label: 'All transcripts' },
    { value: 'audio', label: 'With a recording' },
    { value: 'verified', label: 'Verified' },
  ];

  let search: HTMLInputElement | undefined = $state();
  let query = $state('');
  let records = $state<HistoryRecord[]>([]);
  let total = $state(0);
  let loading = $state(false);
  let loaded = $state(false);
  let filter = $state<Filter>('all');
  let sentinelVisible = false;
  let reqId = 0;

  const hasMore = $derived(records.length < total);
  const visible = $derived(
    filter === 'all' ? records : records.filter((r) => (filter === 'audio' ? r.has_audio : r.reference != null)),
  );
  const groups = $derived.by(() => {
    const out: { key: string; label: string; items: HistoryRecord[] }[] = [];
    for (const r of visible) {
      const key = dayKey(r.timestamp);
      const last = out[out.length - 1];
      if (last?.key === key) last.items.push(r);
      else out.push({ key, label: dayLabel(r.timestamp), items: [r] });
    }
    return out;
  });
  const searching = $derived(query.trim() !== '');
  const cap = $derived(pttCap(app.settings?.pttKey));

  const statsLine = $derived.by(() => {
    const s = app.snapshot?.stats;
    if (!s || s.totalWords === 0) return '';
    const since = s.firstUsed
      ? new Date(s.firstUsed).toLocaleString('en-GB', {
          month: 'long',
          year: new Date(s.firstUsed).getFullYear() === new Date().getFullYear() ? undefined : 'numeric',
        })
      : null;
    const pace = s.wpm > 0 ? `, at about ${Math.round(s.wpm)} a minute` : '';
    return `${formatNumber(s.totalWords)} words${since ? ` since ${since}` : ''}${pace}.`;
  });

  async function load(reset: boolean) {
    const id = ++reqId;
    loading = true;
    try {
      // On reset keep the amount already shown so live updates don't collapse the list.
      const limit = reset ? Math.max(PAGE, records.length) : filter === 'all' ? PAGE : FILTERED_PAGE;
      const offset = reset ? 0 : records.length;
      const page = await api.getHistory(offset, limit, query.trim());
      if (id !== reqId) return;
      records = reset ? page.records : [...records, ...page.records];
      total = page.total;
      loaded = true;
    } catch (e) {
      app.error('Transcripts did not load', e);
    } finally {
      if (id === reqId) loading = false;
    }
    // Keep paging while the end of the list is on screen (also covers a filter that hides everything).
    if (id === reqId && sentinelVisible && records.length < total) void load(false);
  }

  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  function onSearchInput() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      records = [];
      void load(true);
    }, 180);
  }

  function clearSearch() {
    query = '';
    records = [];
    void load(true);
    search?.focus();
  }

  /** Watches the end of the list and loads the next page when it comes into view. */
  function watchEnd(node: HTMLElement) {
    const io = new IntersectionObserver(
      (entries) => {
        sentinelVisible = entries.some((e) => e.isIntersecting);
        if (sentinelVisible && loaded && !loading && records.length < total) void load(false);
      },
      { root: node.closest('.page'), rootMargin: '600px' },
    );
    io.observe(node);
    return () => io.disconnect();
  }

  onMount(() => {
    void load(true);
    // Reload when history changes elsewhere (new dictation, import, clear).
    return app.onHistoryChanged(() => void load(true));
  });

  function patch(id: string, p: Partial<HistoryRecord>) {
    records = records.map((r) => (r.id === id ? { ...r, ...p } : r));
  }

  async function correct(r: HistoryRecord, text: string) {
    const ok = await app.correctHistory(r.id, text);
    if (ok) {
      const clean = text.trim();
      patch(r.id, { text: clean, reference: clean, word_count: clean.split(/\s+/).filter(Boolean).length });
    }
    return ok;
  }

  async function verify(r: HistoryRecord) {
    if (await app.verifyHistory(r.id)) patch(r.id, { reference: r.text });
  }

  async function remove(r: HistoryRecord) {
    // Move focus to a neighbour so keyboard users don't lose their place.
    const all = Array.from(document.querySelectorAll<HTMLElement>('[data-entry]'));
    const i = all.findIndex((el) => el.contains(document.activeElement));
    records = records.filter((x) => x.id !== r.id);
    total = Math.max(0, total - 1);
    if (i !== -1) requestAnimationFrame(() => document.querySelectorAll<HTMLElement>('[data-entry]')[Math.min(i, all.length - 2)]?.focus());
    if (!(await app.deleteHistory(r.id))) void load(true);
  }

  async function exportCsv() {
    try {
      const path = await api.exportHistoryCsv();
      app.toast('success', 'Saved a CSV of every transcript', path);
    } catch (e) {
      app.error('The CSV was not saved', e);
    }
  }

  function setFilter(f: Filter) {
    filter = f;
    if (sentinelVisible && records.length < total && !loading) void load(false);
  }
</script>

<svelte:window
  onkeydown={(e) => {
    if (e.metaKey && e.key === 'f') {
      e.preventDefault();
      search?.focus();
      search?.select();
    }
  }}
/>

<Page title="Transcripts">
  {#snippet actions()}
    <div class="search">
      <SearchIcon size={14} strokeWidth={1.5} class="search-icon" aria-hidden="true" />
      <input
        bind:this={search}
        bind:value={query}
        oninput={onSearchInput}
        onkeydown={(e) => e.key === 'Escape' && query && (e.preventDefault(), clearSearch())}
        class="field search-field"
        type="text"
        placeholder="Search"
        aria-label="Search transcripts"
        aria-keyshortcuts="Meta+F"
        spellcheck="false"
      />
      {#if query}
        <button class="icon-btn clear" aria-label="Clear search" onclick={clearSearch}><XIcon size={12} strokeWidth={1.75} /></button>
      {/if}
    </div>
    <Menu label="Show and export">
      {#snippet trigger({ toggle, open, onkeydown })}
        <button
          class={['icon-btn', { filtered: filter !== 'all' }]}
          aria-haspopup="menu"
          aria-expanded={open}
          aria-label="Show and export"
          title="Show and export"
          onclick={toggle}
          {onkeydown}
        >
          <ListFilterIcon size={16} strokeWidth={1.5} />
        </button>
      {/snippet}
      {#snippet children({ close })}
        {#each FILTERS as f (f.value)}
          <button
            role="menuitemradio"
            aria-checked={filter === f.value}
            onclick={() => {
              setFilter(f.value);
              close();
            }}
          >
            <span class="menu-check">{#if filter === f.value}<CheckIcon size={14} strokeWidth={1.75} />{/if}</span>
            {f.label}
          </button>
        {/each}
        <div class="menu-sep" role="separator"></div>
        <button
          role="menuitem"
          onclick={() => {
            close();
            void exportCsv();
          }}
        >
          <span class="menu-check"></span>Export as CSV
        </button>
      {/snippet}
    </Menu>
  {/snippet}

  <AttentionLine />

  {#if filter !== 'all'}
    <p class="filter-note">
      Showing only {filter === 'audio' ? 'transcripts with a recording' : 'verified transcripts'}.
      <button class="link" onclick={() => setFilter('all')}>Show all</button>
    </p>
  {/if}

  {#if loaded && records.length === 0 && !searching}
    <div class="empty">
      <p class="empty-words">Nothing here yet.</p>
      <p class="empty-help">
        {#if cap}
          Hold <kbd class="keycap">{cap}</kbd> in any app, say something, and let go. The text is typed where your cursor
          is, and a copy lands here.
        {:else}
          Pick a dictation key in Settings, then speak into any app. A copy of everything you say lands here.
        {/if}
      </p>
    </div>
  {:else if loaded && records.length === 0}
    <p class="none">Nothing you said contains “{query.trim()}”.</p>
  {:else if loaded && visible.length === 0 && !hasMore}
    <p class="none">
      {#if filter === 'audio'}
        No transcripts with a recording. Turn on “Keep recordings” in Settings › Accuracy to start collecting them.
      {:else}
        Nothing verified yet. Correct a transcript, or mark one as right, and it shows up here.
      {/if}
    </p>
  {/if}

  {#each groups as g (g.key)}
    <section class="day" aria-labelledby="day-{g.key}">
      <h2 class="day-label" id="day-{g.key}">{g.label}</h2>
      {#each g.items as r (r.id)}
        <TranscriptEntry
          record={r}
          query={query}
          oncopy={() => app.copy(r.text)}
          ondelete={() => remove(r)}
          oncorrect={(text) => correct(r, text)}
          onverify={() => verify(r)}
        />
      {/each}
    </section>
  {/each}

  <div class="sentinel" {@attach watchEnd}></div>

  {#if loaded && visible.length > 0}
    <p class="foot">
      {#if hasMore}
        Loading earlier transcripts…
      {:else if searching}
        {plural(visible.length, 'transcript')} mention “{query.trim()}”.
      {:else if filter === 'all'}
        {statsLine}
      {/if}
    </p>
  {/if}
</Page>

<style>
  .search {
    position: relative;
    width: 220px;
  }
  .search :global(.search-icon) {
    position: absolute;
    top: 50%;
    left: 9px;
    transform: translateY(-50%);
    color: var(--ink-3);
    pointer-events: none;
  }
  .search-field {
    height: 28px;
    padding-left: 28px;
    padding-right: 26px;
  }
  .clear {
    position: absolute;
    top: 3px;
    right: 3px;
    width: 22px;
    height: 22px;
  }
  .filtered {
    color: var(--ink);
    background: var(--press);
  }
  .filter-note {
    margin-bottom: 16px;
    font-size: 13px;
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
    max-width: 46ch;
    margin-top: 10px;
    font-size: 13px;
    line-height: 20px;
    color: var(--ink-2);
  }
  .none {
    padding: 32px 0 0 88px;
    max-width: 60ch;
    font-size: 13px;
    line-height: 20px;
    color: var(--ink-2);
  }
  .sentinel {
    height: 1px;
  }
  .foot {
    margin-top: 32px;
    padding-left: 88px;
    font-size: 13px;
    color: var(--ink-2);
  }
</style>
