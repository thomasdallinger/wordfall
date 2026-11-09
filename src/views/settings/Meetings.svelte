<script lang="ts">
  import { Trash2Icon } from '@lucide/svelte';
  import Row from '../../lib/components/Row.svelte';
  import Section from '../../lib/components/Section.svelte';
  import Switch from '../../lib/components/Switch.svelte';
  import { api } from '../../lib/api';
  import { formatSize } from '../../lib/format';
  import { IS_WINDOWS, THIS_DEVICE } from '../../lib/platform';
  import { app } from '../../lib/state.svelte';
  import type { SummaryModelInfo } from '../../lib/types';

  const s = $derived(app.settings);

  const LIMITS: [number, string][] = [
    [30, '30 minutes'],
    [60, '1 hour'],
    [90, '1.5 hours'],
    [120, '2 hours'],
    [180, '3 hours'],
    [240, '4 hours'],
    [360, '6 hours'],
    [480, '8 hours'],
    [0, 'Never'],
  ];
  const KEEP: [number, string][] = [
    [1, '1 day'],
    [7, '1 week'],
    [14, '2 weeks'],
    [30, '30 days'],
    [90, '90 days'],
    [0, 'Forever'],
  ];

  const systemHelp = IS_WINDOWS
    ? 'Records what this PC plays, so voices from Zoom, Teams or Meet are captured cleanly, even with headphones.'
    : 'Records what this Mac plays, so voices from Zoom, Teams or Meet are captured cleanly, even with headphones. Needs macOS 14.2 or later; macOS asks once for permission.';

  function progress(m: SummaryModelInfo) {
    const d = app.downloads[m.id];
    if (!d || m.downloaded) return null;
    return d.total > 0 ? Math.min(1, d.downloaded / d.total) : 0;
  }

  async function download(m: SummaryModelInfo) {
    app.downloads[m.id] = { downloaded: 0, total: m.sizeMb * 1_000_000 };
    try {
      await api.downloadSummaryModel(m.id);
    } catch (e) {
      delete app.downloads[m.id];
      app.error(`${m.displayName} did not download`, e);
    }
  }

  async function remove(m: SummaryModelInfo) {
    try {
      await api.deleteSummaryModel(m.id);
      await app.loadSummaryModels();
    } catch (e) {
      app.error(`${m.displayName} was not removed`, e);
    }
  }
</script>

{#if s}
  <Section
    id="meetings"
    title="Meetings"
    intro="Meetings are transcribed while they run and turned into notes when they end, all on {THIS_DEVICE}."
  >
    <Row id="meet-limit" label="Stop automatically after" help="A meeting you forget to stop ends by itself. You can always stop it sooner.">
      <select
        class="field select"
        aria-labelledby="meet-limit"
        value={s.meetingMaxMinutes}
        onchange={(e) => app.updateSettings({ meetingMaxMinutes: Number(e.currentTarget.value) })}
      >
        {#each LIMITS as [v, label] (v)}
          <option value={v}>{label}</option>
        {/each}
      </select>
    </Row>

    <Row id="meet-system" label="Record other participants" help={systemHelp}>
      <Switch labelledby="meet-system" checked={s.meetingSystemAudio} onchange={(v) => app.updateSettings({ meetingSystemAudio: v })} />
    </Row>

    <Row
      id="meet-speakers"
      label="Tell speakers apart"
      help="Labels who said what (Me, Speaker 1, Speaker 2…). Click a name in a transcript to rename that person. Uses a 30 MB voice model, downloaded with your first meeting."
    >
      <Switch labelledby="meet-speakers" checked={s.meetingSpeakers} onchange={(v) => app.updateSettings({ meetingSpeakers: v })} />
    </Row>

    <Row
      id="meet-questions"
      label="Suggest questions to ask"
      help="While you talk, the summary model reads the last few minutes and proposes up to three questions: what was vague, what to challenge, what was left open. Give the meeting a goal to steer them. Questions you keep for later end up in the notes."
    >
      <Switch labelledby="meet-questions" checked={s.meetingQuestions} onchange={(v) => app.updateSettings({ meetingQuestions: v })} />
    </Row>

    {#if s.meetingQuestions}
      <Row
        id="meet-questions-window"
        label="Show questions above other apps"
        help="A small window at the top right that stays above the call. It doesn’t take the keyboard from the call; drag it anywhere."
      >
        <Switch
          labelledby="meet-questions-window"
          checked={s.meetingQuestionsWindow}
          onchange={(v) => app.updateSettings({ meetingQuestionsWindow: v })}
        />
      </Row>
    {/if}

    <Row id="meet-keep" label="Keep recordings for" help="The audio of each meeting, for re-checking the transcript. Notes and transcripts are kept until you delete them.">
      <select
        class="field select"
        aria-labelledby="meet-keep"
        value={s.meetingAudioDays}
        onchange={(e) => app.updateSettings({ meetingAudioDays: Number(e.currentTarget.value) })}
      >
        {#each KEEP as [v, label] (v)}
          <option value={v}>{label}</option>
        {/each}
      </select>
    </Row>

    <div class="row-head">
      <div class="label">Notes model</div>
      <p class="help">
        A language model that runs on {THIS_DEVICE} and writes the notes. Bigger models write better notes and need more
        memory. Notes for a long meeting take a few minutes.
      </p>
    </div>
    <fieldset class="models">
      <legend class="sr-only">Notes model</legend>
      {#each app.summaryModels as m (m.id)}
        {@const pct = progress(m)}
        {@const active = m.id === s.summaryModel}
        <div class="model" class:active class:unavailable={!m.downloaded}>
          <input
            type="radio"
            name="summary-model"
            id="sm-{m.id}"
            checked={active}
            disabled={!m.downloaded}
            onchange={() => app.updateSettings({ summaryModel: m.id })}
          />
          <label for="sm-{m.id}" class="model-text">
            <span class="model-name">
              {m.displayName}
              <span class="tag">{m.recommended ? 'Recommended' : m.badge}</span>
            </span>
            <span class="model-desc">{m.description}</span>
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
              <span class="in-use">In use</span>
            {:else}
              <button class="icon-btn is-bad" title="Remove from {THIS_DEVICE}" aria-label="Remove {m.displayName} from {THIS_DEVICE}" onclick={() => remove(m)}>
                <Trash2Icon size={14} strokeWidth={1.5} />
              </button>
            {/if}
          </span>
        </div>
      {/each}
    </fieldset>
    {#if !app.summaryModels.some((m) => m.downloaded)}
      <p class="note">Download a model to get notes. Until then, meetings are transcribed but not summarised.</p>
    {/if}
  </Section>
{/if}

<style>
  .select {
    width: 160px;
  }
  .row-head {
    padding: 14px 0 0;
  }
  .label {
    font-size: 13px;
    line-height: 18px;
    font-weight: 500;
  }
  .help {
    margin-top: 2px;
    max-width: 58ch;
    font-size: 12px;
    line-height: 17px;
    color: var(--ink-2);
  }
  .note {
    margin: 6px 0 8px;
    font-size: 12px;
    color: var(--ink-2);
  }
  .models {
    margin: 8px 0 0;
    padding: 0;
    border: 0;
  }
  .model {
    display: grid;
    grid-template-columns: 18px minmax(0, 1fr) 64px 96px;
    align-items: center;
    column-gap: 12px;
    padding: 10px;
    margin: 0 -10px;
    border-radius: var(--radius-control);
  }
  .model:hover:not(.unavailable),
  .model.active {
    background: var(--bg);
  }
  input[type='radio'] {
    appearance: none;
    width: 16px;
    height: 16px;
    margin: 1px 0 0;
    align-self: start;
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
