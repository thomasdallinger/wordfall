<script lang="ts">
  import Row from '../../lib/components/Row.svelte';
  import Section from '../../lib/components/Section.svelte';
  import Segmented from '../../lib/components/Segmented.svelte';
  import Switch from '../../lib/components/Switch.svelte';
  import { IS_WINDOWS, MOD } from '../../lib/platform';
  import { app } from '../../lib/state.svelte';
  import type { Settings } from '../../lib/types';

  type BoolKey = 'launchAtLogin' | 'removeFillers' | 'formatLists' | 'soundsEnabled' | 'muteSystemAudio';

  const s = $derived(app.settings);
  const switches: { key: BoolKey; label: string; help: string }[] = [
    {
      key: 'launchAtLogin',
      label: 'Open at login',
      help: `Starts quietly in the ${IS_WINDOWS ? 'notification area' : 'menu bar'} when you log in.`,
    },
    { key: 'removeFillers', label: 'Leave out filler words', help: 'Drops um, uh, äh, ähm and similar hesitations from the text.' },
    {
      key: 'formatLists',
      label: 'Make lists',
      help: 'Say “first, … second, …” or “bullet point …” (“erstens …”, “Stichpunkt …”) to get a numbered or bulleted list.',
    },
    { key: 'soundsEnabled', label: 'Sounds', help: 'A soft tone when recording starts and when it stops.' },
    { key: 'muteSystemAudio', label: 'Mute other audio', help: 'Silences music and videos while you talk, then turns them back on.' },
  ];

  function set<K extends keyof Settings>(key: K, value: Settings[K]) {
    void app.updateSettings({ [key]: value } as Partial<Settings>);
  }
</script>

{#if s}
  <Section id="general" title="General">
    <Row
      label="After you speak"
      help={s.copyOnly
        ? `The text goes to the clipboard and nothing is typed. Paste it yourself with ${MOD}V.`
        : 'The text is typed into whatever app you were using.'}
    >
      <Segmented
        label="After you speak"
        value={s.copyOnly ? 'copy' : 'paste'}
        onchange={(v) => set('copyOnly', v === 'copy')}
        options={[
          { value: 'paste', label: 'Paste it' },
          { value: 'copy', label: 'Copy only' },
        ]}
      />
    </Row>
    {#each switches as row (row.key)}
      <Row id="general-{row.key}" label={row.label} help={row.help}>
        <Switch labelledby="general-{row.key}" checked={s[row.key]} onchange={(v) => set(row.key, v)} />
      </Row>
    {/each}
  </Section>
{/if}
