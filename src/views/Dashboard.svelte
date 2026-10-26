<script lang="ts">
  import Sidebar from '../lib/components/Sidebar.svelte';
  import Toaster from '../lib/components/Toaster.svelte';
  import { IS_WINDOWS, primaryMod } from '../lib/platform';
  import { app, type View } from '../lib/state.svelte';
  import Dictionary from './Dictionary.svelte';
  import Onboarding from './Onboarding.svelte';
  import Settings from './Settings.svelte';
  import Transcripts from './Transcripts.svelte';
  import VoiceLab from './VoiceLab.svelte';

  const KEYS: Record<string, View> = { '1': 'transcripts', '2': 'dictionary', '3': 'settings', ',': 'settings' };

  function onkeydown(e: KeyboardEvent) {
    if (!primaryMod(e) || e.shiftKey || e.altKey || (IS_WINDOWS ? e.metaKey : e.ctrlKey)) return;
    const view = KEYS[e.key];
    if (!view) return;
    e.preventDefault();
    app.go(view);
  }
</script>

<svelte:window {onkeydown} />

{#if app.settings && !app.settings.onboardingDone}
  <Onboarding />
{:else}
  <div class="window">
    <Sidebar />
    <main class="main">
      {#if app.view === 'transcripts'}
        <Transcripts />
      {:else if app.view === 'dictionary'}
        <Dictionary />
      {:else if app.view === 'lab'}
        <VoiceLab />
      {:else}
        <Settings />
      {/if}
    </main>
  </div>
{/if}

<Toaster full={!!app.settings && !app.settings.onboardingDone} />

<style>
  .window {
    display: flex;
    height: 100%;
  }
  .main {
    flex: 1;
    min-width: 0;
    height: 100%;
  }
</style>
