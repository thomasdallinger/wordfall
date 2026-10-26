<script lang="ts">
  import { XIcon } from '@lucide/svelte';
  import ModelList from '../../lib/components/ModelList.svelte';
  import Row from '../../lib/components/Row.svelte';
  import Section from '../../lib/components/Section.svelte';
  import { pickFolder } from '../../lib/api';
  import { PRODUCT } from '../../lib/brand';
  import { LANGUAGES, formatSize, languageName, plural, shortPath } from '../../lib/format';
  import { THIS_DEVICE } from '../../lib/platform';
  import { app } from '../../lib/state.svelte';

  const s = $derived(app.settings);
  const englishOnly = $derived(app.activeModelInfo?.englishOnly ?? false);
  const addable = $derived(LANGUAGES.filter(([code]) => !s?.languages.includes(code)));
  const languageHelp = $derived.by(() => {
    const langs = s?.languages ?? [];
    if (englishOnly) return 'The current model only understands English. Pick a multilingual model to dictate in other languages.';
    if (langs.length === 0) return 'Any language. Add the ones you speak so short phrases aren’t mistaken for another language.';
    if (langs.length === 1) return `Everything is written in ${languageName(langs[0])}. Add another and ${PRODUCT} follows whichever you speak.`;
    return `Speak any of these and ${PRODUCT} writes in the one you used. Nothing to switch.`;
  });

  function addLanguage(code: string) {
    if (!s || !code || s.languages.includes(code)) return;
    void app.updateSettings({ languages: [...s.languages, code] });
  }

  function removeLanguage(code: string) {
    if (!s) return;
    void app.updateSettings({ languages: s.languages.filter((l) => l !== code) });
  }
  const onDisk = $derived(app.models.filter((m) => m.downloaded && m.source === 'app'));
  const diskMb = $derived(onDisk.reduce((n, m) => n + m.sizeMb, 0));

  async function addFolder() {
    const path = await pickFolder('Choose a folder with Whisper models');
    if (!path || !s) return;
    if (s.modelFolders.includes(path)) return;
    await app.updateSettings({ modelFolders: [...s.modelFolders, path] });
    await app.loadModels();
  }

  async function removeFolder(path: string) {
    if (!s) return;
    await app.updateSettings({ modelFolders: s.modelFolders.filter((f) => f !== path) });
    await app.loadModels();
  }
</script>

{#if s}
  <Section id="model" title="Model" intro="Speech is turned into text on {THIS_DEVICE}. Bigger models make fewer mistakes and take a little longer.">
    <ModelList />
    <p class="disk">{plural(onDisk.length, 'model')} downloaded, using {formatSize(diskMb)}.</p>

    <Row id="model-lang" label="Languages" help={languageHelp}>
      <select
        class="field lang"
        aria-labelledby="model-lang"
        disabled={englishOnly}
        value=""
        onchange={(e) => {
          addLanguage(e.currentTarget.value);
          e.currentTarget.value = '';
        }}
      >
        <option value="">Add a language…</option>
        {#each addable as [code, name] (code)}
          <option value={code}>{name}</option>
        {/each}
      </select>
      {#snippet below()}
        {#if !englishOnly && s.languages.length > 0}
          <ul class="folders" aria-label="Languages you dictate in">
            {#each s.languages as code (code)}
              <li>
                <span class="path">{languageName(code)}</span>
                <button class="icon-btn" title="Remove" aria-label="Remove {languageName(code)}" onclick={() => removeLanguage(code)}>
                  <XIcon size={14} strokeWidth={1.5} />
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      {/snippet}
    </Row>

    <Row label="Model folders" help="Other places to look for Whisper models in ggml format. Models there are used where they are, not copied.">
      <button class="btn" onclick={addFolder}>Add folder…</button>
      {#snippet below()}
        {#if s.modelFolders.length > 0}
          <ul class="folders">
            {#each s.modelFolders as f (f)}
              <li>
                <span class="path selectable" title={f}>{shortPath(f)}</span>
                <button class="icon-btn" title="Stop looking here" aria-label="Stop looking in {shortPath(f)}" onclick={() => removeFolder(f)}>
                  <XIcon size={14} strokeWidth={1.5} />
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      {/snippet}
    </Row>
  </Section>
{/if}

<style>
  .disk {
    margin: 8px 0 8px;
    font-size: 12px;
    color: var(--ink-2);
  }
  .lang {
    width: 200px;
  }
  .folders li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    height: 32px;
    padding-left: 10px;
    border-radius: var(--radius-control);
    background: var(--bg);
  }
  .folders li + li {
    margin-top: 4px;
  }
  .path {
    min-width: 0;
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
