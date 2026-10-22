<script lang="ts">
  import { CheckIcon, RotateCwIcon } from '@lucide/svelte';
  import Keycaps from '../../lib/components/Keycaps.svelte';
  import Row from '../../lib/components/Row.svelte';
  import Section from '../../lib/components/Section.svelte';
  import Segmented from '../../lib/components/Segmented.svelte';
  import { PRODUCT } from '../../lib/brand';
  import { PTT_KEYS, acceleratorFromEvent, acceleratorParts, pttCap } from '../../lib/format';
  import { app } from '../../lib/state.svelte';
  import type { PermissionKind, PttKey } from '../../lib/types';

  const s = $derived(app.settings);
  const snap = $derived(app.snapshot);
  let capturing = $state(false);
  let captureBtn: HTMLButtonElement | undefined = $state();
  let spinning = $state(false);

  const activeDevice = $derived(s?.inputDevice ?? app.devices.find((d) => d.isDefault)?.name ?? 'the system default');

  function onCaptureKey(e: KeyboardEvent) {
    if (!capturing) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.key === 'Escape') {
      capturing = false;
      return;
    }
    const acc = acceleratorFromEvent(e);
    if (!acc) return; // still holding modifiers
    capturing = false;
    void app.updateSettings({ toggleShortcut: acc });
    captureBtn?.focus();
  }

  async function refreshDevices() {
    spinning = true;
    await app.loadDevices();
    setTimeout(() => (spinning = false), 400);
  }

  const PERMISSIONS: { kind: PermissionKind; label: string; help: string }[] = [
    { kind: 'microphone', label: 'Microphone', help: 'To hear you while the key is held.' },
    { kind: 'accessibility', label: 'Accessibility', help: 'To type the text into other apps.' },
    { kind: 'inputMonitoring', label: 'Input Monitoring', help: 'To notice when you press your dictation key.' },
  ];

  function granted(kind: PermissionKind) {
    const p = snap?.permissions;
    if (!p) return false;
    return kind === 'microphone' ? p.microphone === 'granted' : p[kind];
  }
</script>

<svelte:window onkeydowncapture={onCaptureKey} />

{#if s && snap}
  <Section id="recording" title="Recording">
    <Row
      id="rec-key"
      label="Dictation key"
      help="Hold it and talk, let go to paste. Double-tap it to keep going hands-free, then tap once more to finish."
    >
      <select
        class="field select"
        aria-labelledby="rec-key"
        value={s.pttKey}
        onchange={(e) => app.updateSettings({ pttKey: e.currentTarget.value as PttKey })}
      >
        {#each PTT_KEYS as k (k.value)}
          <option value={k.value}>{k.label}</option>
        {/each}
      </select>
      {#snippet below()}
        {#if s.pttKey !== 'none' && !snap.pttListening}
          <p class="note status-warn">
            {PRODUCT} can't listen for {pttCap(s.pttKey)} yet. It needs Input Monitoring, listed under Permissions below.
          </p>
        {/if}
        {#if s.pttKey === 'fn' && !snap.fnUsage.ok}
          <div class="note-row">
            <p class="note status-warn">macOS also opens {snap.fnUsage.label} when fn is pressed.</p>
            <button class="btn btn-sm" onclick={() => app.fixFnKey()}>Make fn do nothing else</button>
          </div>
        {/if}
      {/snippet}
    </Row>

    <Row id="rec-toggle" label="Hands-free shortcut" help="Press once to start and again to finish. Easier for long passages.">
      {#if capturing}
        <span class="capture" aria-live="polite">Type the new shortcut, or Esc</span>
        <button class="btn btn-quiet" onclick={() => (capturing = false)}>Cancel</button>
      {:else}
        <Keycaps keys={acceleratorParts(s.toggleShortcut)} />
        {#if s.toggleShortcut}
          <button class="btn btn-quiet" onclick={() => app.updateSettings({ toggleShortcut: null })}>Clear</button>
        {/if}
        <button class="btn" bind:this={captureBtn} onclick={() => (capturing = true)}>{s.toggleShortcut ? 'Change' : 'Set'}</button>
      {/if}
    </Row>

    <Row label="Bring back the pill" help="Shows the pill again if you hid it. This shortcut is fixed.">
      <Keycaps keys={acceleratorParts(s.showWidgetShortcut)} />
    </Row>

    <Row id="rec-mic" label="Microphone" help="Now using {activeDevice}.">
      <select
        class="field select"
        aria-labelledby="rec-mic"
        value={s.inputDevice ?? ''}
        onchange={(e) => app.updateSettings({ inputDevice: e.currentTarget.value || null })}
      >
        <option value="">Same as macOS</option>
        {#each app.devices as d (d.name)}
          <option value={d.name}>{d.name}</option>
        {/each}
      </select>
      <button class="icon-btn" class:spinning title="Look for microphones again" aria-label="Look for microphones again" onclick={refreshDevices}>
        <RotateCwIcon size={14} strokeWidth={1.5} />
      </button>
    </Row>

    <Row label="Input boost" help="Turn this up if a quiet microphone keeps losing words.">
      <Segmented
        label="Input boost"
        value={s.inputGain}
        onchange={(v) => app.updateSettings({ inputGain: v })}
        options={[
          { value: 1, label: 'Off' },
          { value: 1.5, label: '1.5×' },
          { value: 2, label: '2×' },
          { value: 3, label: '3×' },
        ]}
      />
    </Row>

    <h3 class="sub">Permissions</h3>
    {#each PERMISSIONS as p (p.kind)}
      <Row label={p.label} help={p.help}>
        {#if granted(p.kind)}
          <span class="allowed status-ok"><CheckIcon size={14} strokeWidth={2} aria-hidden="true" />Allowed</span>
        {:else}
          <button class="btn btn-sm" onclick={() => app.requestPermission(p.kind)}>Allow…</button>
        {/if}
      </Row>
    {/each}
  </Section>
{/if}

<style>
  .select {
    width: 200px;
  }
  .note {
    font-size: 12px;
    line-height: 17px;
  }
  .note-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-top: 4px;
  }
  .capture {
    display: inline-flex;
    align-items: center;
    height: 28px;
    padding: 0 10px;
    border-radius: var(--radius-control);
    font-size: 13px;
    color: var(--ink);
    box-shadow: inset 0 0 0 1px var(--ink-3);
  }
  .sub {
    margin-top: 24px;
    padding-bottom: 2px;
    font-size: 13px;
  }
  .allowed {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 13px;
    font-weight: 500;
  }
  .spinning :global(svg) {
    animation: turn 0.4s linear;
  }
  @keyframes turn {
    to {
      transform: rotate(360deg);
    }
  }
</style>
