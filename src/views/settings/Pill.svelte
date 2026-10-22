<script lang="ts">
  import Logo from '../../lib/components/Logo.svelte';
  import Row from '../../lib/components/Row.svelte';
  import Section from '../../lib/components/Section.svelte';
  import Segmented from '../../lib/components/Segmented.svelte';
  import Switch from '../../lib/components/Switch.svelte';
  import { PRODUCT } from '../../lib/brand';
  import { app } from '../../lib/state.svelte';
  import type { PillStyle, WidgetVisibility } from '../../lib/types';

  const s = $derived(app.settings);

  const LOOKS: { value: PillStyle; label: string }[] = [
    { value: 'logo-and-text', label: 'Mark and name' },
    { value: 'logo-only', label: 'Mark only' },
    { value: 'logo-only-monochrome', label: 'Just a dot' },
  ];

  const VISIBILITY_HELP: Record<WidgetVisibility, string> = {
    always: 'Stays on screen. Click it to start or stop, drag it to move it.',
    recording: 'Appears while you dictate and hides again afterwards.',
    hidden: 'Never shown. Your dictation key still works.',
  };

  let picker: HTMLDivElement | undefined = $state();

  function onPickerKey(e: KeyboardEvent) {
    if (!s) return;
    const step = e.key === 'ArrowRight' || e.key === 'ArrowDown' ? 1 : e.key === 'ArrowLeft' || e.key === 'ArrowUp' ? -1 : 0;
    if (!step) return;
    e.preventDefault();
    const i = LOOKS.findIndex((l) => l.value === s.pillStyle);
    const next = (i + step + LOOKS.length) % LOOKS.length;
    void app.updateSettings({ pillStyle: LOOKS[next].value });
    picker?.querySelectorAll<HTMLButtonElement>('button')[next]?.focus();
  }
</script>

{#if s}
  <Section id="pill" title="Pill" intro="The small floating control at the bottom of the screen. It shows a live waveform while you talk.">
    <div class="looks" role="radiogroup" aria-label="How the pill looks when idle" bind:this={picker} onkeydown={onPickerKey} tabindex="-1">
      {#each LOOKS as look (look.value)}
        {@const on = s.pillStyle === look.value}
        <button
          class="look"
          class:on
          role="radio"
          aria-checked={on}
          tabindex={on ? 0 : -1}
          onclick={() => app.updateSettings({ pillStyle: look.value })}
        >
          <span class="stage" aria-hidden="true">
            <span class="pill" class:mark-only={look.value === 'logo-only'} class:dot-only={look.value === 'logo-only-monochrome'}>
              {#if look.value === 'logo-only-monochrome'}
                <span class="pill-dot"></span>
              {:else}
                <Logo size={16} />
                {#if look.value === 'logo-and-text'}<span class="pill-name">{PRODUCT}</span>{/if}
              {/if}
            </span>
          </span>
          <span class="look-label">{look.label}</span>
        </button>
      {/each}
    </div>

    <Row label="Show the pill" help={VISIBILITY_HELP[s.widgetVisibility]}>
      <Segmented
        label="Show the pill"
        value={s.widgetVisibility}
        onchange={(v) => app.updateSettings({ widgetVisibility: v })}
        options={[
          { value: 'always', label: 'Always' },
          { value: 'recording', label: 'While dictating' },
          { value: 'hidden', label: 'Never' },
        ]}
      />
    </Row>
    <Row id="pill-follow" label="Follow the pointer" help="Moves to whichever display your pointer is on.">
      <Switch labelledby="pill-follow" checked={s.followCursor} onchange={(v) => app.updateSettings({ followCursor: v })} />
    </Row>
    <Row id="pill-lock" label="Lock in place" help="Stops it from being dragged by accident.">
      <Switch labelledby="pill-lock" checked={s.widgetLocked} onchange={(v) => app.updateSettings({ widgetLocked: v })} />
    </Row>
  </Section>
{/if}

<style>
  .looks {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 12px;
    margin: 14px 0 6px;
    outline: none;
  }
  .look {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    border-radius: var(--radius-sheet);
  }
  .stage {
    display: grid;
    place-items: center;
    width: 100%;
    height: 72px;
    border-radius: var(--radius-sheet);
    background: var(--bg);
    transition: box-shadow var(--dur) var(--ease);
  }
  .look:hover .stage {
    box-shadow: inset 0 0 0 1px var(--line);
  }
  .look.on .stage {
    box-shadow: inset 0 0 0 2px var(--ink);
  }
  .look-label {
    font-size: 12px;
    color: var(--ink-2);
  }
  .look.on .look-label {
    color: var(--ink);
    font-weight: 500;
  }
  .stage .pill {
    pointer-events: none;
  }
</style>
