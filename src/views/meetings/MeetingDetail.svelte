<script lang="ts">
  import { ChevronLeftIcon, CopyIcon, EllipsisIcon } from '@lucide/svelte';
  import { onMount, tick } from 'svelte';
  import ConfirmDialog from '../../lib/components/ConfirmDialog.svelte';
  import MeetingVisuals from '../../lib/components/MeetingVisuals.svelte';
  import Menu from '../../lib/components/Menu.svelte';
  import Page from '../../lib/components/Page.svelte';
  import QuestionStack from '../../lib/components/QuestionStack.svelte';
  import ScreenPicker from '../../lib/components/ScreenPicker.svelte';
  import Segmented from '../../lib/components/Segmented.svelte';
  import SystemMap from '../../lib/components/SystemMap.svelte';
  import { api, isTauri, onFileDrop, pickImages } from '../../lib/api';
  import { clock, dayLabel, duration, formatTime } from '../../lib/format';
  import { fixedParts, type TextPart } from '../../lib/fixes';
  import { notesAsText, renderNotes } from '../../lib/markdown';
  import { IS_WINDOWS, primaryMod } from '../../lib/platform';
  import { app } from '../../lib/state.svelte';
  import type { Meeting, MeetingSegment } from '../../lib/types';

  let { id }: { id: string } = $props();

  type Tab = 'summary' | 'notes' | 'transcript' | 'shown' | 'map';

  let meeting = $state<Meeting | null>(null);
  let tab = $state<Tab>('notes');
  let now = $state(Date.now());
  let notesDraft = $state('');
  let notesDirty = false;
  let titleDraft = $state('');
  let goalDraft = $state('');
  let editingSpeaker = $state<number | null>(null);
  let speakerDraft = $state('');
  let confirm: ConfirmDialog | undefined = $state();
  let scroller: HTMLElement | undefined = $state();
  let followTranscript = true;
  /** Files are dragged over the window. */
  let dragging = $state(false);
  /** Names of images being read. */
  let reading = $state<string[]>([]);

  const recording = $derived(meeting?.status === 'recording');
  const questions = $derived(app.meetingQuestions[id] ?? null);
  const showQuestions = $derived(recording && !!app.settings?.meetingLiveHelp && !!app.settings?.meetingQuestions);
  const liveMap = $derived(app.meetingMaps[id] ?? null);
  const showMap = $derived(recording && !!app.settings?.meetingLiveHelp && !!app.settings?.meetingMap);
  const busy = $derived(meeting?.status === 'processing' || meeting?.status === 'summarizing');
  const progress = $derived(app.meetingProgress[id] ?? null);
  const live = $derived(app.activeMeeting?.id === id ? app.activeMeeting : null);
  const rendered = $derived(meeting?.summary ? renderNotes(meeting.summary) : null);
  const draftNotes = $derived(progress?.phase === 'writing' && progress.text ? renderNotes(progress.text) : null);
  /** Screens and photos are read on macOS; elsewhere the tab shows only what a Mac read. */
  const canShow = $derived(!IS_WINDOWS || !!meeting?.visuals?.length);
  const showScreen = $derived(recording && !IS_WINDOWS && !!app.settings?.meetingScreen);
  const offers = $derived(app.photoOffers.filter((o) => o.id === id));
  const percent = $derived(progress?.fraction != null ? Math.round(progress.fraction * 100) : null);
  const tabs = $derived.by(() => {
    const t: { value: Tab; label: string }[] = [];
    if (!recording) t.push({ value: 'summary', label: 'Notes' });
    t.push({ value: 'notes', label: 'My notes' });
    t.push({ value: 'transcript', label: 'Transcript' });
    if (canShow) t.push({ value: 'shown', label: 'Screens & photos' });
    if (!recording && meeting?.systemMap?.nodes.length) t.push({ value: 'map', label: 'System map' });
    return t;
  });

  /** Consecutive segments of one speaker shown as one turn. */
  const turns = $derived.by(() => {
    if (!meeting) return [];
    const sorted = [...meeting.segments].sort((a, b) => a.startMs - b.startMs);
    const out: { speaker: number; startMs: number; text: string; parts: TextPart[] }[] = [];
    for (const s of sorted) {
      const last = out[out.length - 1];
      const parts = fixedParts(s.text, s.fixes);
      if (last && last.speaker === s.speaker && s.startMs - last.startMs < 120_000) {
        last.text += ` ${s.text}`;
        last.parts.push({ text: ' ' }, ...parts);
      } else out.push({ speaker: s.speaker, startMs: s.startMs, text: s.text, parts });
    }
    return out;
  });

  function speakerName(sid: number): string {
    return meeting?.speakers.find((s) => s.id === sid)?.name ?? (sid === 0 ? 'Me' : `Speaker ${sid}`);
  }

  async function load(initial = false) {
    try {
      const m = await api.getMeeting(id);
      const wasRecording = meeting?.status === 'recording';
      meeting = m;
      // When the recording ends, show the notes being written.
      if (wasRecording && m.status !== 'recording' && tab === 'notes' && !notesDirty) tab = 'summary';
      if (!notesDirty) notesDraft = m.notes;
      if (document.activeElement?.id !== 'meeting-title') titleDraft = m.title;
      if (document.activeElement?.id !== 'meeting-goal') goalDraft = m.goal;
      if (m.status === 'recording') {
        void app.loadQuestions(id);
        void app.loadMap(id);
      }
      if (initial) tab = m.status === 'recording' ? 'notes' : m.summary || m.status !== 'done' ? 'summary' : 'transcript';
      if (tab === 'summary' && m.status === 'recording') tab = 'notes';
    } catch (e) {
      app.error('The meeting did not load', e);
      app.meetingId = null;
    }
  }

  onMount(() => {
    void load(true);
    const t = setInterval(() => (now = Date.now()), 1000);
    const off = app.onMeetingEvent((e) => {
      if (e.kind === 'segments' && e.id === id && meeting) {
        meeting.segments = [...meeting.segments, ...e.segments];
        meeting.speakers = e.speakers;
        if (tab === 'transcript' && followTranscript) void tick().then(scrollToEnd);
      } else if (e.kind === 'visuals' && e.id === id && meeting) {
        meeting.visuals = e.visuals;
      } else if (e.kind === 'changed' && e.id === id) {
        void load();
      }
    });
    // Images dropped on the window (Tauri hands over paths; the browser preview uses DOM events).
    const dropped = onFileDrop(
      (over) => (dragging = over && canShow),
      (paths) => canShow && paths.length > 0 && void addPaths(paths),
    );
    return () => {
      clearInterval(t);
      off();
      void dropped.then((un) => un());
      if (notesDirty) void saveNotes();
    };
  });

  async function addPaths(paths: string[]) {
    const names = paths.map((p) => p.split(/[\\/]/).pop() ?? p);
    reading = [...reading, ...names];
    tab = 'shown';
    try {
      await app.addImages(id, paths);
    } finally {
      reading = reading.filter((n) => !names.includes(n));
    }
  }

  async function pickAndAdd() {
    try {
      const paths = await pickImages('Add images to this meeting');
      if (paths.length) await addPaths(paths);
    } catch (e) {
      app.error('The images were not added', e);
    }
  }

  async function paste() {
    reading = [...reading, 'the pasted image'];
    tab = 'shown';
    try {
      await api.pasteMeetingImage(id);
    } catch (e) {
      app.error('Nothing was added', e);
    } finally {
      reading = reading.filter((n) => n !== 'the pasted image');
    }
  }

  function editable(t: EventTarget | null): boolean {
    return t instanceof HTMLElement && (t.isContentEditable || t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT');
  }

  /** ⌘V outside a text field adds the image on the clipboard. */
  function onKeydown(e: KeyboardEvent) {
    if (!canShow || !primaryMod(e) || e.key.toLowerCase() !== 'v' || e.shiftKey || e.altKey || editable(e.target)) return;
    e.preventDefault();
    void paste();
  }

  // Browser preview only: Tauri delivers drops through `onFileDrop`.
  function onDragOver(e: DragEvent) {
    if (isTauri || !canShow || !e.dataTransfer?.types.includes('Files')) return;
    e.preventDefault();
    dragging = true;
  }

  function onDrop(e: DragEvent) {
    if (isTauri || !canShow) return;
    e.preventDefault();
    dragging = false;
    const files = Array.from(e.dataTransfer?.files ?? []).filter((f) => f.type.startsWith('image/') || /\.(heic|heif)$/i.test(f.name));
    if (files.length) void addPaths(files.map((f) => f.name));
  }

  function scrollToEnd() {
    const page = scroller?.closest('.page');
    page?.scrollTo({ top: page.scrollHeight });
  }

  function onPageScroll(node: HTMLElement) {
    const page = node.closest('.page');
    if (!page) return;
    const handler = () => (followTranscript = page.scrollTop + page.clientHeight >= page.scrollHeight - 40);
    page.addEventListener('scroll', handler, { passive: true });
    return () => page.removeEventListener('scroll', handler);
  }

  let notesTimer: ReturnType<typeof setTimeout> | undefined;
  function onNotesInput() {
    notesDirty = true;
    clearTimeout(notesTimer);
    notesTimer = setTimeout(() => void saveNotes(), 600);
  }

  async function saveNotes() {
    clearTimeout(notesTimer);
    const notes = notesDraft;
    try {
      await api.updateMeeting(id, { notes });
      if (notesDraft === notes) notesDirty = false;
    } catch (e) {
      app.error('Your notes were not saved', e);
    }
  }

  async function saveTitle() {
    if (!meeting || titleDraft.trim() === meeting.title) return;
    try {
      await api.updateMeeting(id, { title: titleDraft });
      meeting.title = titleDraft.trim();
      meeting.titleFromUser = titleDraft.trim() !== '';
    } catch (e) {
      app.error('The title was not saved', e);
    }
  }

  async function saveGoal() {
    if (!meeting || goalDraft.trim() === meeting.goal) return;
    try {
      await api.updateMeeting(id, { goal: goalDraft });
      meeting.goal = goalDraft.trim();
    } catch (e) {
      app.error('The goal was not saved', e);
    }
  }

  function editSpeaker(sid: number) {
    editingSpeaker = sid;
    speakerDraft = speakerName(sid);
  }

  async function saveSpeaker() {
    const sid = editingSpeaker;
    editingSpeaker = null;
    if (sid === null || !speakerDraft.trim() || speakerDraft.trim() === speakerName(sid)) return;
    try {
      meeting = await api.renameSpeaker(id, sid, speakerDraft);
    } catch (e) {
      app.error('The name was not saved', e);
    }
  }

  async function summarize() {
    if (notesDirty) await saveNotes();
    try {
      await api.summarizeMeeting(id);
      tab = 'summary';
      if (meeting) meeting.status = 'summarizing';
    } catch (e) {
      app.error('The notes can’t be written', e);
    }
  }

  function transcriptText(): string {
    return turns.map((t) => `[${clock(t.startMs)}] ${speakerName(t.speaker)}: ${t.text}`).join('\n\n');
  }

  async function showFolder() {
    try {
      await api.openPath(await api.meetingFolder(id));
    } catch (e) {
      app.error('The folder did not open', e);
    }
  }

  async function remove() {
    try {
      await api.deleteMeeting(id);
      app.meetingId = null;
      app.toast('success', 'Meeting deleted');
    } catch (e) {
      app.error('The meeting is still there', e);
    }
  }

  function focusOnMount(node: HTMLElement) {
    node.focus();
    if (node instanceof HTMLInputElement) node.select();
  }

  function autosize(node: HTMLTextAreaElement) {
    const fit = () => {
      node.style.height = 'auto';
      node.style.height = `${Math.max(node.scrollHeight, 220)}px`;
    };
    fit();
    node.addEventListener('input', fit);
    return () => node.removeEventListener('input', fit);
  }

  const when = $derived(meeting ? `${dayLabel(meeting.startedAt)} at ${formatTime(meeting.startedAt)}` : '');
  const phaseLabel = $derived.by(() => {
    if (!meeting) return '';
    if (meeting.status === 'processing') {
      if (progress?.phase === 'speakers') return 'Telling the speakers apart…';
      if (progress?.phase === 'improving') return 'Improving the transcript…';
      return 'Transcribing the last few seconds…';
    }
    if (meeting.status === 'summarizing') {
      if (!progress || progress.phase === 'loading') return 'Starting the summary model…';
      if (progress.phase === 'checking') return 'Checking your terms in the transcript…';
      if (progress.phase === 'reading') return 'Reading the transcript…';
      return 'Writing the notes…';
    }
    return '';
  });
</script>

<Page title={meeting?.title || (recording ? 'Meeting in progress' : 'Untitled meeting')} wide={showQuestions || showMap}>
  {#snippet lead()}
    <button class="icon-btn back" aria-label="Back to all meetings" title="All meetings" onclick={() => (app.meetingId = null)}>
      <ChevronLeftIcon size={18} strokeWidth={1.5} />
    </button>
  {/snippet}
  {#snippet actions()}
    {#if recording}
      <button class="btn btn-signal" onclick={() => app.stopMeeting()}>Stop</button>
    {:else if meeting}
      {#if meeting.summary}
        <button class="icon-btn" title="Copy notes" aria-label="Copy notes" onclick={() => app.copy(notesAsText(meeting!.summary!), 'Notes copied')}>
          <CopyIcon size={15} strokeWidth={1.5} />
        </button>
      {/if}
      <Menu label="Meeting actions">
        {#snippet trigger({ toggle, open, onkeydown })}
          <button class="icon-btn" aria-haspopup="menu" aria-expanded={open} aria-label="More" title="More" onclick={toggle} {onkeydown}>
            <EllipsisIcon size={16} strokeWidth={1.5} />
          </button>
        {/snippet}
        {#snippet children({ close })}
          {#if meeting!.summary}
            <button role="menuitem" onclick={() => { close(); void app.copy(meeting!.summary!, 'Copied as Markdown'); }}>
              <span class="menu-check"></span>Copy as Markdown
            </button>
          {/if}
          <button role="menuitem" disabled={meeting!.segments.length === 0} onclick={() => { close(); void app.copy(transcriptText(), 'Transcript copied'); }}>
            <span class="menu-check"></span>Copy transcript
          </button>
          <button role="menuitem" disabled={busy || meeting!.segments.length === 0} onclick={() => { close(); void summarize(); }}>
            <span class="menu-check"></span>{meeting!.summary ? 'Write the notes again' : 'Write notes'}
          </button>
          {#if meeting!.hasAudio}
            <button role="menuitem" onclick={() => { close(); void showFolder(); }}>
              <span class="menu-check"></span>Show recordings
            </button>
          {/if}
          <div class="menu-sep" role="separator"></div>
          <button role="menuitem" class="danger" disabled={busy} onclick={() => { close(); confirm?.show(); }}>
            <span class="menu-check"></span>Delete meeting…
          </button>
        {/snippet}
      </Menu>
    {/if}
  {/snippet}

  {#if meeting}
    <header class="head">
      <input
        id="meeting-title"
        class="title-field"
        bind:value={titleDraft}
        placeholder={recording ? 'Name this meeting' : busy ? 'The notes will name it' : 'Untitled meeting'}
        aria-label="Meeting title"
        spellcheck="false"
        onblur={saveTitle}
        onkeydown={(e) => e.key === 'Enter' && (e.currentTarget as HTMLInputElement).blur()}
      />
      {#if recording || meeting.goal}
        <label class="goal">
          <span class="goal-label">Goal</span>
          <input
            id="meeting-goal"
            class="goal-field"
            bind:value={goalDraft}
            placeholder={recording ? 'What should this meeting achieve? The questions steer towards it.' : ''}
            spellcheck="false"
            onblur={saveGoal}
            onkeydown={(e) => e.key === 'Enter' && (e.currentTarget as HTMLInputElement).blur()}
          />
        </label>
      {/if}
      {#if recording}
        <p class="status live" aria-live="off">
          <span class="live-dot" aria-hidden="true"></span>
          <span class="clock">{clock(now - Date.parse(meeting.startedAt))}</span>
          <span class="levels" aria-hidden="true">
            <span class="level" title="You" style:--l={app.meetingLevels.mic}></span>
            {#if meeting.systemAudio}<span class="level" title="Others" style:--l={app.meetingLevels.system}></span>{/if}
          </span>
          {#if live?.stopsAt}
            <span class="sub">Stops by itself at {formatTime(live.stopsAt)}.</span>
          {/if}
        </p>
        {#if !meeting.systemAudio}
          <p class="hint">Only your microphone is recorded, so other participants are heard only through your speakers.</p>
        {/if}
        {#if showScreen}
          <ScreenPicker meetingId={id} />
        {/if}
      {:else}
        <p class="status">
          {when}, {duration(meeting.durationMs)}.
          {#if meeting.stoppedByLimit}Stopped by the time limit.{/if}
        </p>
      {/if}
    </header>

    {#if busy}
      <div class="progress" role="status">
        <span class="busy-dot" aria-hidden="true"></span>
        <span>{phaseLabel}</span>
        {#if (progress?.phase === 'reading' || progress?.phase === 'improving' || progress?.phase === 'checking') && percent !== null}
          <span class="bar" aria-hidden="true"><span style:width="{percent}%"></span></span>
          <span class="pct">{percent} %</span>
        {/if}
      </div>
      {#if progress?.phase === 'improving'}
        <p class="hint">The whole recording is read once more, more carefully than live. This takes a few minutes for an hour.</p>
      {/if}
    {/if}

    {#each offers as offer (offer.path)}
      <div class="offer" role="alert">
        <span><strong>{offer.name}</strong> arrived in Downloads. Add it to this meeting?</span>
        <button class="btn btn-sm" onclick={() => { tab = 'shown'; void app.answerOffer(offer, true); }}>Add</button>
        <button class="btn btn-quiet btn-sm" onclick={() => app.answerOffer(offer, false)}>Ignore</button>
      </div>
    {/each}

    <div class={['columns', { 'with-questions': showQuestions }]}>
      <div class="main">
        {#if showMap}
          <section class="map-panel" aria-labelledby="map-title">
            <div class="panel-head">
              <h2 id="map-title">System map</h2>
              {#if liveMap?.status === 'thinking'}<span class="sub" role="status">Reading the talk…</span>{/if}
            </div>
            <SystemMap
              meetingId={id}
              map={liveMap?.map ?? meeting.systemMap}
              status={liveMap?.status ?? 'listening'}
              alsoMentioned={liveMap?.alsoMentioned ?? []}
              editable
            />
          </section>
        {/if}
        <div class="tabs">
          <Segmented label="Show" options={tabs} value={tab} onchange={(v) => (tab = v)} />
        </div>

        {#if tab === 'summary'}
          <div class="notes-view">
            {#if draftNotes}
              <div class="md drafting selectable" aria-busy="true">{@html draftNotes.html}</div>
            {:else if rendered}
              <div class="md selectable">{@html rendered.html}</div>
              {#if meeting.error}<p class="error">{meeting.error}</p>{/if}
            {:else if busy}
              <p class="placeholder">The notes appear here in a moment. Long meetings take a few minutes.</p>
            {:else}
              <p class="placeholder">{meeting.error ?? 'No notes yet.'}</p>
              {#if meeting.segments.length > 0}
                <button class="btn" onclick={summarize}>Write notes</button>
              {/if}
            {/if}
          </div>
        {:else if tab === 'notes'}
          <textarea
            class="my-notes selectable"
            bind:value={notesDraft}
            oninput={onNotesInput}
            onblur={() => notesDirty && saveNotes()}
            {@attach autosize}
            aria-label="Your notes"
            placeholder={recording
              ? 'Jot down what matters to you, in a few words. When the meeting ends, the notes are built around these points and filled in from the transcript.'
              : 'Your own notes. Write the notes again to work in changes.'}
          ></textarea>
        {:else if tab === 'map'}
          <SystemMap meetingId={id} map={meeting.systemMap} />
        {:else if tab === 'shown'}
          <MeetingVisuals meetingId={id} visuals={meeting.visuals ?? []} durationMs={meeting.durationMs} {reading} onadd={pickAndAdd} />
        {:else}
          <div class="transcript" bind:this={scroller} {@attach onPageScroll}>
            {#if turns.length === 0}
              <p class="placeholder">
                {recording ? 'The transcript appears here a few seconds after people speak.' : 'Nothing was transcribed.'}
              </p>
            {/if}
            {#each turns as t, i (i)}
              <div class="turn">
                <div class="margin">
                  <span class="time">{clock(t.startMs)}</span>
                </div>
                <div class="said">
                  {#if editingSpeaker === t.speaker && turns.findIndex((x) => x.speaker === t.speaker) === i}
                    <input
                      class="field speaker-field"
                      bind:value={speakerDraft}
                      aria-label="Speaker name"
                      {@attach focusOnMount}
                      onblur={saveSpeaker}
                      onkeydown={(e) => {
                        if (e.key === 'Enter') (e.currentTarget as HTMLInputElement).blur();
                        if (e.key === 'Escape') editingSpeaker = null;
                      }}
                    />
                  {:else}
                    <button class="speaker" title="Rename {speakerName(t.speaker)}" onclick={() => editSpeaker(t.speaker)}>
                      {speakerName(t.speaker)}
                    </button>
                  {/if}
                  <!-- prettier-ignore -->
                  <p class="words selectable">{#each t.parts as part, k (k)}{#if part.heard}<span class="term-fix" title="Corrected from “{part.heard}”">{part.text}</span>{:else}{part.text}{/if}{/each}</p>
                </div>
              </div>
            {/each}
            {#if recording && turns.length > 0}
              <p class="listening">Listening…</p>
            {/if}
          </div>
        {/if}
      </div>
      {#if showQuestions}
        <aside class="questions" aria-labelledby="questions-title">
          <div class="questions-head">
            <h2 id="questions-title">Questions</h2>
            {#if questions?.status === 'thinking'}<span class="sub" role="status">Reading the talk…</span>{/if}
            <span class="spacer"></span>
            {#if app.settings?.meetingQuestionsWindow}
              <button class="btn btn-quiet small" title="Show the questions above other apps" onclick={() => api.showQuestionsWindow(true)}>
                Float
              </button>
            {/if}
          </div>
          <QuestionStack meetingId={id} startedAt={meeting.startedAt} questions={questions?.questions ?? []} status={questions?.status ?? 'listening'} />
        </aside>
      {/if}
    </div>
  {/if}
</Page>

<svelte:window onkeydown={onKeydown} ondragover={onDragOver} ondragleave={() => (dragging = false)} ondrop={onDrop} />

{#if dragging}
  <div class="drop" aria-hidden="true"><p>Drop images to add them to this meeting</p></div>
{/if}

<ConfirmDialog
  bind:this={confirm}
  title="Delete this meeting?"
  message="Its notes, transcript and recordings are removed from this computer. This can't be undone."
  confirmLabel="Delete"
  onconfirm={remove}
/>

<style>
  .back {
    margin-left: -10px;
  }
  .head {
    padding-top: 8px;
  }
  .title-field {
    width: 100%;
    padding: 0;
    border: 0;
    background: transparent;
    font-size: 22px;
    line-height: 30px;
    font-weight: 700;
    color: var(--ink);
    outline: none;
  }
  .title-field::placeholder {
    color: var(--ink-3);
  }
  .status {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 4px;
    font-size: 13px;
    color: var(--ink-2);
  }
  .status .clock {
    color: var(--ink);
    font-weight: 500;
    font-variant-numeric: tabular-nums;
  }
  .sub {
    color: var(--ink-3);
  }
  .hint {
    margin-top: 6px;
    max-width: 60ch;
    font-size: 12px;
    color: var(--ink-2);
  }
  .levels {
    display: inline-flex;
    gap: 3px;
    align-items: flex-end;
    height: 12px;
  }
  .level {
    width: 3px;
    height: calc(3px + var(--l, 0) * 9px);
    border-radius: 2px;
    background: var(--ink-2);
    transition: height 120ms ease-out;
  }
  .live-dot,
  .busy-dot {
    width: 7px;
    height: 7px;
    border-radius: 999px;
    background: var(--signal);
  }
  .busy-dot {
    background: var(--ink-2);
    animation: breathe 1.6s ease-in-out infinite;
  }
  .progress {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 16px;
    font-size: 13px;
    color: var(--ink-2);
  }
  .bar {
    width: 120px;
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
    font-size: 12px;
    font-variant-numeric: tabular-nums;
  }
  .tabs {
    margin: 20px 0 16px;
  }
  .goal {
    display: flex;
    align-items: baseline;
    gap: 8px;
    margin-top: 6px;
  }
  .goal-label {
    font-size: 13px;
    font-weight: 700;
    color: var(--ink-2);
  }
  .goal-field {
    flex: 1;
    min-width: 0;
    padding: 0;
    border: 0;
    background: transparent;
    font-size: 13px;
    line-height: 20px;
    color: var(--ink);
    outline: none;
  }
  .goal-field::placeholder {
    color: var(--ink-3);
  }
  .columns.with-questions {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
  }
  .questions {
    order: -1;
    margin-top: 20px;
  }
  @media (min-width: 1040px) {
    .columns.with-questions {
      grid-template-columns: minmax(0, 1fr) 280px;
      column-gap: 32px;
      align-items: start;
    }
    .questions {
      order: 0;
      position: sticky;
      top: 16px;
    }
  }
  .questions-head {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 28px;
    margin-bottom: 8px;
  }
  .map-panel {
    margin-top: 20px;
    padding-bottom: 20px;
    border-bottom: 1px solid var(--line);
  }
  .panel-head {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 28px;
    margin-bottom: 8px;
  }
  .panel-head h2,
  .questions-head h2 {
    font-size: 13px;
    font-weight: 700;
    color: var(--ink);
  }
  .questions-head .spacer {
    flex: 1;
  }
  .btn.small {
    height: 24px;
    padding: 0 8px;
    font-size: 12px;
  }
  .placeholder {
    max-width: 56ch;
    margin-bottom: 12px;
    font-family: var(--font-text);
    font-style: italic;
    font-size: 16px;
    line-height: 26px;
    color: var(--ink-2);
  }
  .error {
    margin-top: 16px;
    font-size: 13px;
    color: var(--ink-2);
  }
  .md {
    max-width: 66ch;
    font-family: var(--font-text);
    font-size: 16px;
    line-height: 26px;
    color: var(--ink);
  }
  .md.drafting {
    font-style: italic;
    color: var(--ink-2);
  }
  .md :global(h3) {
    margin: 24px 0 6px;
    font-family: var(--font-ui, inherit);
    font-size: 15px;
    line-height: 20px;
    font-weight: 700;
    font-style: normal;
    color: var(--ink);
  }
  .md :global(h4) {
    margin: 16px 0 4px;
    font-size: 16px;
    font-weight: 700;
  }
  .md :global(p) {
    margin: 0 0 10px;
  }
  .md :global(ul),
  .md :global(ol) {
    margin: 0 0 10px;
    padding-left: 22px;
  }
  .md :global(ul) {
    list-style: disc;
  }
  .md :global(ol) {
    list-style: decimal;
  }
  .md :global(li) {
    margin: 2px 0;
  }
  .md :global(li.task) {
    display: flex;
    gap: 10px;
    margin-left: -22px;
    list-style: none;
  }
  .md :global(.box) {
    flex-shrink: 0;
    width: 14px;
    height: 14px;
    margin-top: 6px;
    border-radius: 3px;
    box-shadow: inset 0 0 0 1.5px var(--ink-3);
  }
  .md :global(li.task.done .box) {
    background: var(--ink-2);
  }
  .my-notes {
    display: block;
    width: 100%;
    max-width: 66ch;
    min-height: 220px;
    padding: 12px 14px;
    resize: none;
    overflow: hidden;
    border-radius: var(--radius-sheet);
    border: 1px solid var(--line);
    background: var(--surface);
    font-family: var(--font-text);
    font-size: 16px;
    line-height: 26px;
    color: var(--ink);
    outline: none;
  }
  .my-notes:focus {
    border-color: var(--ink-3);
  }
  .my-notes::placeholder {
    font-style: italic;
    color: var(--ink-3);
  }
  .turn {
    display: grid;
    grid-template-columns: 72px minmax(0, 1fr);
    column-gap: 16px;
    padding: 8px 0;
  }
  .margin {
    padding-top: 2px;
    text-align: right;
    font-size: 12px;
    line-height: 16px;
    color: var(--ink-3);
    font-variant-numeric: tabular-nums;
  }
  .speaker {
    font-size: 12px;
    font-weight: 700;
    line-height: 16px;
    color: var(--ink-2);
    border-radius: 4px;
  }
  .speaker:hover {
    color: var(--ink);
    text-decoration: underline;
  }
  .speaker-field {
    width: 200px;
    height: 24px;
    font-size: 12px;
  }
  .term-fix {
    text-decoration: underline dotted;
    text-decoration-color: var(--ink-3);
    text-underline-offset: 3px;
    cursor: help;
  }

  .words {
    max-width: 62ch;
    margin-top: 2px;
    font-family: var(--font-text);
    font-size: 16px;
    line-height: 26px;
    color: var(--ink);
    overflow-wrap: anywhere;
  }
  .listening {
    padding: 8px 0 0 88px;
    font-family: var(--font-text);
    font-style: italic;
    font-size: 15px;
    color: var(--ink-3);
  }
  :global(.danger) {
    color: var(--bad);
  }
  .offer {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 16px;
    padding: 8px 10px;
    border-radius: var(--radius-control);
    background: var(--surface);
    box-shadow: inset 0 0 0 1px var(--line);
    font-size: 13px;
  }
  .offer span {
    flex: 1;
    min-width: 0;
  }
  .drop {
    position: fixed;
    inset: 12px;
    z-index: 50;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sheet);
    border: 2px dashed var(--ink-3);
    background: color-mix(in srgb, var(--bg) 88%, transparent);
    pointer-events: none;
  }
  .drop p {
    font-size: 15px;
    font-weight: 700;
    color: var(--ink);
  }
  @media (prefers-reduced-motion: reduce) {
    .level,
    .busy-dot {
      transition: none;
      animation: none;
    }
  }
</style>
