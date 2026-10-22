<script lang="ts">
  import { onMount, tick } from 'svelte';
  import Page from '../lib/components/Page.svelte';
  import { app, SETTINGS_SECTIONS, type SettingsSection } from '../lib/state.svelte';
  import About from './settings/About.svelte';
  import Accuracy from './settings/Accuracy.svelte';
  import Data from './settings/Data.svelte';
  import General from './settings/General.svelte';
  import Model from './settings/Model.svelte';
  import Pill from './settings/Pill.svelte';
  import Recording from './settings/Recording.svelte';

  let scroller: HTMLElement | undefined = $state();
  /** While a programmatic scroll runs, the spy must not overwrite the requested section. */
  let jumping = false;
  let spy = () => {};

  async function jump(id: SettingsSection) {
    await tick();
    const el = document.getElementById(`settings-${id}`);
    if (!el || !scroller) return;
    jumping = true;
    app.settingsSection = id;
    const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
    // Scroll only the page, never the window (scrollIntoView would move every ancestor).
    const offset = el.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
    scroller.scrollTo({ top: scroller.scrollTop + offset - 60, behavior: reduce ? 'auto' : 'smooth' });
    el.querySelector<HTMLElement>('h2')?.focus({ preventScroll: true });
    setTimeout(() => {
      jumping = false;
      spy();
    }, 600);
  }

  onMount(() => {
    // Sidebar sub-items and links elsewhere request a section through `app.go('settings', id)`.
    const stopListening = app.onSectionRequest((id) => void jump(id));
    if (app.pendingSection) {
      void jump(app.pendingSection);
      app.pendingSection = null;
    }
    if (!scroller) return stopListening;
    const root = scroller;
    const sections = Array.from(root.querySelectorAll<HTMLElement>('[data-section]'));
    const update = () => {
      if (jumping) return;
      const top = root.getBoundingClientRect().top + 80;
      let current = sections[0];
      for (const s of sections) if (s.getBoundingClientRect().top <= top) current = s;
      // At the very bottom the last section wins even if it's short.
      if (root.scrollTop + root.clientHeight >= root.scrollHeight - 4) current = sections[sections.length - 1];
      const id = current?.dataset.section as SettingsSection | undefined;
      if (id && app.settingsSection !== id) app.settingsSection = id;
    };
    spy = update;
    if (!jumping) app.settingsSection = SETTINGS_SECTIONS[0].id;
    root.addEventListener('scroll', update, { passive: true });
    return () => {
      stopListening();
      root.removeEventListener('scroll', update);
    };
  });
</script>

<Page title="Settings" bind:scroller>
  <div class="settings">
    <General />
    <Recording />
    <Model />
    <Pill />
    <Accuracy />
    <Data />
    <About />
  </div>
</Page>

<style>
  .settings {
    max-width: 640px;
    padding-top: 8px;
  }
  .settings :global(h2[id$='-title']:focus) {
    outline: none;
  }
</style>
