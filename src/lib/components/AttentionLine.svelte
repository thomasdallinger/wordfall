<script lang="ts">
  import { PRODUCT } from '../brand';
  import { pttCap } from '../format';
  import { SYSTEM_SETTINGS } from '../platform';
  import { app } from '../state.svelte';
  import type { PermissionKind } from '../types';

  /** One actionable line when something stops dictation from working. Renders nothing otherwise. */

  interface Attention {
    tone: 'warn' | 'bad' | 'info';
    text: string;
    action?: { label: string; run: () => void };
  }

  const PERMISSION_NAMES: Record<PermissionKind, string> = {
    microphone: 'microphone access',
    accessibility: 'Accessibility',
    inputMonitoring: 'Input Monitoring',
  };

  let waiting = $state<PermissionKind | null>(null);

  function allow(kind: PermissionKind) {
    waiting = kind;
    void app.requestPermission(kind);
    setTimeout(() => {
      if (waiting === kind) waiting = null;
    }, 5000);
  }

  const attention = $derived.by((): Attention | null => {
    const s = app.snapshot;
    if (!s) return null;
    const key = pttCap(s.settings.pttKey);
    const missing = app.missingPermissions.filter((k) => k !== 'inputMonitoring' || s.settings.pttKey !== 'none');
    if (missing.length > 0) {
      const kind = missing[0];
      const text = {
        microphone: `${PRODUCT} can't hear you until it has microphone access.`,
        accessibility: `Without Accessibility, ${PRODUCT} can copy your words but can't paste them.`,
        inputMonitoring: `Input Monitoring is off, so holding ${key || 'your key'} does nothing yet.`,
      }[kind];
      return {
        tone: kind === 'microphone' ? 'bad' : 'warn',
        text,
        action: {
          label: waiting === kind ? `Waiting for ${SYSTEM_SETTINGS}…` : `Allow ${kind === 'microphone' ? 'microphone' : PERMISSION_NAMES[kind]}`,
          run: () => allow(kind),
        },
      };
    }
    const chooseModel = { label: 'Choose a model', run: () => app.go('settings', 'model') };
    if (s.modelState === 'error')
      return { tone: 'bad', text: `The speech model didn't load. ${s.modelError ?? ''}`.trim(), action: chooseModel };
    if (s.modelState === 'unloaded') return { tone: 'warn', text: 'No speech model is loaded yet.', action: chooseModel };
    if (s.modelState === 'loading') {
      const name = app.activeModelInfo?.displayName ?? 'the speech model';
      return { tone: 'info', text: `Loading ${name}. Dictation works again in a few seconds.` };
    }
    if (s.settings.pttKey === 'fn' && !s.fnUsage.ok)
      return {
        tone: 'warn',
        text: `Pressing fn also opens ${s.fnUsage.label}, which gets in the way.`,
        action: { label: 'Make fn do nothing else', run: () => app.fixFnKey() },
      };
    if (s.settings.pttKey !== 'none' && !s.pttListening)
      return {
        tone: 'warn',
        text: `${PRODUCT} can't watch the ${key} key right now.`,
        action: { label: 'Pick another key', run: () => app.go('settings', 'recording') },
      };
    if (s.settings.pttKey === 'none' && !s.settings.toggleShortcut)
      return {
        tone: 'warn',
        text: 'No key starts dictation yet.',
        action: { label: 'Pick a key', run: () => app.go('settings', 'recording') },
      };
    return null;
  });
</script>

{#if attention}
  <div class="attention {attention.tone}" role="status">
    <span class="dot" aria-hidden="true"></span>
    <p class="text">
      {attention.text}
    </p>
    {#if attention.action}
      <button class="btn btn-sm action" onclick={attention.action.run}>{attention.action.label}</button>
    {/if}
  </div>
{/if}

<style>
  .attention {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 40px;
    margin-bottom: 20px;
    padding: 8px 8px 8px 14px;
    border-radius: var(--radius-sheet);
    background: var(--well);
  }
  .attention.warn {
    background: color-mix(in srgb, var(--warn) 11%, var(--surface));
  }
  .attention.bad {
    background: color-mix(in srgb, var(--bad) 10%, var(--surface));
  }
  .dot {
    width: 7px;
    height: 7px;
    flex-shrink: 0;
    border-radius: 999px;
    background: var(--ink-2);
  }
  .warn .dot {
    background: var(--warn);
  }
  .bad .dot {
    background: var(--bad);
  }
  .info .dot {
    animation: breathe 1.6s ease-in-out infinite;
  }
  .text {
    flex: 1;
    min-width: 0;
    font-size: 13px;
    line-height: 18px;
  }
  .action {
    background: var(--surface);
    box-shadow: 0 0 0 1px var(--line);
  }
  .action:hover {
    background: var(--surface);
    box-shadow: 0 0 0 1px var(--ink-3);
  }
</style>
