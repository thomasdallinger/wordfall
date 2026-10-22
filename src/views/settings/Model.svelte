<script lang="ts">
  import { XIcon } from '@lucide/svelte';
  import ModelList from '../../lib/components/ModelList.svelte';
  import Row from '../../lib/components/Row.svelte';
  import Section from '../../lib/components/Section.svelte';
  import { pickFolder } from '../../lib/api';
  import { formatSize, plural, shortPath } from '../../lib/format';
  import { app } from '../../lib/state.svelte';

  const LANGUAGES: [string, string][] = [
    ['en', 'English'], ['de', 'German'], ['fr', 'French'], ['es', 'Spanish'], ['it', 'Italian'],
    ['pt', 'Portuguese'], ['nl', 'Dutch'], ['pl', 'Polish'], ['sv', 'Swedish'], ['da', 'Danish'],
    ['no', 'Norwegian'], ['fi', 'Finnish'], ['cs', 'Czech'], ['uk', 'Ukrainian'], ['ru', 'Russian'],
    ['tr', 'Turkish'], ['el', 'Greek'], ['ja', 'Japanese'], ['zh', 'Chinese'], ['ko', 'Korean'],
    ['hi', 'Hindi'], ['ar', 'Arabic'],
  ];

  const s = $derived(app.settings);
  const englishOnly = $derived(app.activeModelInfo?.englishOnly ?? false);
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
  <Section id="model" title="Model" intro="Speech is turned into text on this Mac. Bigger models make fewer mistakes and take a little longer.">
    <ModelList />
    <p class="disk">{plural(onDisk.length, 'model')} downloaded, using {formatSize(diskMb)}.</p>

    <Row id="model-lang" label="Language" help={englishOnly ? 'The current model only understands English.' : 'Automatic works for most people. Pick one if it keeps guessing wrong.'}>
      <select
        class="field lang"
        aria-labelledby="model-lang"
        disabled={englishOnly}
        value={englishOnly ? 'en' : (s.language ?? '')}
        onchange={(e) => app.updateSettings({ language: e.currentTarget.value || null })}
      >
        <option value="">Automatic</option>
        {#each LANGUAGES as [code, name] (code)}
          <option value={code}>{name}</option>
        {/each}
      </select>
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
