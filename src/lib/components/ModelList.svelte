<script lang="ts">
  import { Trash2Icon } from '@lucide/svelte';
  import { api } from '../api';
  import { formatSize, shortPath } from '../format';
  import { app } from '../state.svelte';
  import type { ModelInfo } from '../types';

  /** Radio list of speech models with download, progress and remove. Used by Settings and setup. */

  const s = $derived(app.settings);

  function progress(m: ModelInfo) {
    const d = app.downloads[m.id];
    if (!d || m.downloaded) return null;
    return d.total > 0 ? Math.min(1, d.downloaded / d.total) : 0;
  }

  async function download(m: ModelInfo) {
    app.downloads[m.id] = { downloaded: 0, total: m.sizeMb * 1_000_000 };
    try {
      await api.downloadModel(m.id);
    } catch (e) {
      delete app.downloads[m.id];
      app.error(`${m.displayName} did not download`, e);
    }
  }

  async function select(m: ModelInfo) {
    if (!m.downloaded || m.id === s?.model) return;
    try {
      await api.setModel(m.id);
      // The core persists `model` and emits settings-changed; update locally for snappiness.
      if (app.snapshot) app.snapshot.settings.model = m.id;
    } catch (e) {
      app.error(`Could not switch to ${m.displayName}`, e);
    }
  }

  async function remove(m: ModelInfo) {
    try {
      await api.deleteModel(m.id);
      await app.loadModels();
    } catch (e) {
      app.error(`${m.displayName} was not removed`, e);
    }
  }

</script>

{#if s}
<fieldset class="models">
  <legend class="sr-only">Speech model</legend>
  {#each app.models as m (m.id)}
    {@const pct = progress(m)}
    {@const active = m.id === s.model}
    <div class="model" class:active class:unavailable={!m.downloaded}>
      <input
        type="radio"
        name="model"
        id="model-{m.id}"
        value={m.id}
        checked={active}
        disabled={!m.downloaded}
        onchange={() => select(m)}
      />
      <label for="model-{m.id}" class="model-text">
        <span class="model-name">
          {m.displayName}
          {#if m.recommended}<span class="tag">Recommended</span>{/if}
        </span>
        <span class="model-desc">
          {m.description}
          {m.englishOnly ? 'English only.' : ''}
          {#if m.source === 'folder' && m.path}<span class="from">Found in {shortPath(m.path.replace(/\/[^/]+$/, ''))}.</span>{/if}
        </span>
      </label>
      <span class="model-size">{formatSize(m.sizeMb)}</span>
      <span class="model-action">
        {#if pct !== null}
          <span class="progress" role="progressbar" aria-label="Downloading {m.displayName}" aria-valuenow={Math.round(pct * 100)} aria-valuemin={0} aria-valuemax={100}>
            <span class="bar"><span style:width="{pct * 100}%"></span></span>
            <span class="pct">{Math.round(pct * 100)}%</span>
          </span>
        {:else if !m.downloaded}
          <button class="btn btn-sm" onclick={() => download(m)}>Download</button>
        {:else if active}
          <span class="in-use">{app.snapshot?.modelState === 'loading' ? 'Loading…' : 'In use'}</span>
        {:else if m.source === 'app'}
          <button class="icon-btn is-bad" title="Remove from this Mac" aria-label="Remove {m.displayName} from this Mac" onclick={() => remove(m)}>
            <Trash2Icon size={14} strokeWidth={1.5} />
          </button>
        {/if}
      </span>
    </div>
  {/each}
</fieldset>
{/if}

<style>
  .models {
    margin: 12px 0 0;
    padding: 0;
    border: 0;
  }
  .model {
    display: grid;
    grid-template-columns: 18px minmax(0, 1fr) 64px 96px;
    align-items: center;
    column-gap: 12px;
    padding: 10px 10px;
    margin: 0 -10px;
    border-radius: var(--radius-control);
  }
  .model:hover:not(.unavailable) {
    background: var(--bg);
  }
  .model.active {
    background: var(--bg);
  }
  input[type='radio'] {
    appearance: none;
    width: 16px;
    height: 16px;
    margin: 0;
    align-self: start;
    margin-top: 1px;
    border-radius: 999px;
    box-shadow: inset 0 0 0 1.5px var(--ink-3);
    transition: box-shadow var(--dur) var(--ease);
  }
  input[type='radio']:checked {
    box-shadow: inset 0 0 0 5px var(--ink);
  }
  input[type='radio']:disabled {
    opacity: 0.35;
  }
  .model-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .unavailable .model-text {
    cursor: default;
  }
  .model-name {
    font-size: 13px;
    font-weight: 500;
  }
  .tag {
    margin-left: 6px;
    font-size: 12px;
    font-weight: 400;
    color: var(--ink-2);
  }
  .model-desc {
    font-size: 12px;
    line-height: 17px;
    color: var(--ink-2);
  }
  .from {
    display: block;
  }
  .model-size {
    font-size: 12px;
    color: var(--ink-2);
    text-align: right;
  }
  .model-action {
    display: flex;
    justify-content: flex-end;
  }
  .in-use {
    font-size: 12px;
    font-weight: 500;
    color: var(--ink);
  }
  .progress {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 96px;
  }
  .bar {
    flex: 1;
    height: 3px;
    border-radius: 999px;
    background: var(--press);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--ink);
    transition: width var(--dur) linear;
  }
  .pct {
    width: 30px;
    font-size: 12px;
    color: var(--ink-2);
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
</style>
