<script lang="ts">
  import { ChevronLeftIcon } from '@lucide/svelte';
  import { onMount, tick } from 'svelte';
  import Page from '../lib/components/Page.svelte';
  import { api } from '../lib/api';
  import { LabRecorder } from '../lib/labRecorder.svelte';
  import { app } from '../lib/state.svelte';
  import type { BenchInfo, LabPrompt } from '../lib/types';
  import LabProgress from './lab/LabProgress.svelte';
  import PromptCard from './lab/PromptCard.svelte';
  import PromptList from './lab/PromptList.svelte';

  let prompts = $state<LabPrompt[]>([]);
  let info = $state<BenchInfo | null>(null);
  let loaded = $state(false);
  let index = $state(0);
  /** Show the current prompt's last result rather than the record button. */
  let reviewing = $state(false);
  let lastMs = $state<number | null>(null);
  let card: PromptCard | undefined = $state();

  const current = $derived(prompts[index] ?? null);
  const recordedCount = $derived(prompts.filter((p) => p.recorded).length);

  const recorder = new LabRecorder({
    onresult(r) {
      const p = prompts.find((x) => x.id === r.promptId);
      if (p) Object.assign(p, { recorded: true, hypothesis: r.hypothesis, wer: r.wer });
      lastMs = r.ms;
      reviewing = true;
      void refreshInfo();
    },
    onfail(message, stage) {
      if (message) app.toast(stage === 'start' ? 'error' : 'warning', message);
      // A failed take returns to the record button; a cancelled one restores the last result.
      reviewing = !message && current?.hypothesis != null;
    },
  });

  async function refreshInfo() {
    try {
      info = await api.benchInfo();
    } catch (e) {
      app.error('Test data could not be read', e);
    }
  }

  onMount(() => {
    void recorder.attach();
    void (async () => {
      try {
        const [list, bench] = await Promise.all([api.labPrompts(), api.benchInfo()]);
        prompts = list;
        info = bench;
        const first = list.findIndex((p) => !p.recorded);
        select(first === -1 ? 0 : first, false);
        loaded = true;
        await tick();
        card?.focus();
      } catch (e) {
        app.error('The prompts did not load', e);
      }
    })();
    return () => recorder.dispose();
  });

  function select(i: number, focus = true) {
    if (i < 0 || i >= prompts.length) return;
    if (recorder.phase === 'recording') void recorder.cancel();
    index = i;
    lastMs = null;
    reviewing = prompts[i].hypothesis !== null;
    if (focus) void tick().then(() => card?.focus());
  }

  function selectFromList(id: string) {
    select(prompts.findIndex((p) => p.id === id));
    const el = document.getElementById('lab-current');
    const page = el?.closest('.page');
    if (el && page) {
      const offset = el.getBoundingClientRect().top - page.getBoundingClientRect().top;
      if (offset < 0) page.scrollTo({ top: page.scrollTop + offset - 68 });
    }
  }

  async function remove(id: string) {
    const p = prompts.find((x) => x.id === id);
    if (!p) return;
    const prev = { recorded: p.recorded, hypothesis: p.hypothesis, wer: p.wer };
    Object.assign(p, { recorded: false, hypothesis: null, wer: null });
    if (current?.id === id) reviewing = false;
    try {
      await api.labDelete(id);
      void refreshInfo();
    } catch (e) {
      Object.assign(p, prev);
      app.error('The clip is still there', e);
    }
  }
</script>

<Page title="Voice Lab">
  {#snippet lead()}
    <button class="icon-btn back" aria-label="Back to Accuracy settings" title="Back to Settings" onclick={() => app.go('settings', 'accuracy')}>
      <ChevronLeftIcon size={18} strokeWidth={1.5} />
    </button>
  {/snippet}

  {#if loaded}
    <LabProgress recorded={recordedCount} total={prompts.length} {info} />

    {#if current}
      <div id="lab-current" class="current">
        <PromptCard
          bind:this={card}
          prompt={current}
          {index}
          total={prompts.length}
          {recorder}
          {reviewing}
          {lastMs}
          onprev={() => select(index - 1)}
          onnext={() => select(index + 1)}
          onrerecord={() => {
            reviewing = false;
            card?.focus();
          }}
        />
      </div>
    {/if}

    <PromptList {prompts} currentId={current?.id ?? null} onselect={selectFromList} ondelete={remove} />
  {/if}
</Page>

<style>
  .back {
    margin-left: -10px;
  }
  .current {
    margin-top: 20px;
    scroll-margin-top: calc(var(--toolbar-height) + 16px);
  }
</style>
