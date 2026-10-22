<script lang="ts">
  import { onMount } from 'svelte';
  import Row from '../../lib/components/Row.svelte';
  import Section from '../../lib/components/Section.svelte';
  import Segmented from '../../lib/components/Segmented.svelte';
  import Switch from '../../lib/components/Switch.svelte';
  import { api } from '../../lib/api';
  import { formatNumber, plural } from '../../lib/format';
  import { app } from '../../lib/state.svelte';
  import type { BenchInfo, PromptMode } from '../../lib/types';

  const s = $derived(app.settings);
  let bench = $state<BenchInfo | null>(null);

  onMount(() => {
    api.benchInfo().then(
      (b) => (bench = b),
      () => {},
    );
  });

  const MODES: { value: PromptMode; label: string }[] = [
    { value: 'none', label: 'Off' },
    { value: 'dictionary', label: 'Dictionary' },
    { value: 'projects', label: '+ Projects' },
    { value: 'glossary', label: '+ Dev terms' },
  ];
  const MODE_HELP: Record<PromptMode, string> = {
    none: 'The model listens without any hints.',
    dictionary: 'Before it listens, the model is told the words in your dictionary.',
    projects: 'Your dictionary words, plus the names of your project folders.',
    glossary: 'Your words, your project names, and a built-in list of common developer terms.',
  };

  const keptLine = $derived.by(() => {
    if (!bench || !s?.keepAudio) return '';
    if (bench.historyWithAudio === 0) return 'Nothing kept yet.';
    return `${formatNumber(bench.historyWithAudio)} kept so far, ${formatNumber(bench.historyVerified)} of them verified.`;
  });
  const labLine = $derived(bench ? ` ${plural(bench.tiers.lab ?? 0, 'clip')} recorded so far.` : '');
</script>

{#if s}
  <Section id="accuracy" title="Accuracy" intro="Optional tools for getting more words right, especially names and jargon.">
    <Row label="Vocabulary hints" help={MODE_HELP[s.promptMode]}>
      <Segmented label="Vocabulary hints" value={s.promptMode} onchange={(v) => app.updateSettings({ promptMode: v })} options={MODES} />
    </Row>
    <Row
      id="acc-keep"
      label="Keep recordings"
      help="Saves the audio of each dictation on this Mac, so a corrected transcript can be used to measure accuracy. The newest 1,000 are kept."
    >
      <Switch labelledby="acc-keep" checked={s.keepAudio} onchange={(v) => app.updateSettings({ keepAudio: v })} />
      {#snippet below()}
        {#if keptLine}<p class="kept">{keptLine}</p>{/if}
      {/snippet}
    </Row>
    <Row label="Voice Lab" help="Read short prompts out loud and see exactly which words the model misses.{labLine}">
      <button class="btn" onclick={() => app.go('lab')}>Open Voice Lab</button>
    </Row>
  </Section>
{/if}

<style>
  .kept {
    font-size: 12px;
    color: var(--ink-2);
  }
</style>
