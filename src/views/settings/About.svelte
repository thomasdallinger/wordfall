<script lang="ts">
  import Logo from '../../lib/components/Logo.svelte';
  import Row from '../../lib/components/Row.svelte';
  import Section from '../../lib/components/Section.svelte';
  import { PRODUCT } from '../../lib/brand';
  import { app } from '../../lib/state.svelte';
</script>

<Section id="about" title="About">
  <div class="about">
    <span class="mark"><Logo size={28} dot="var(--signal)" /></span>
    <div>
      <p class="name">{PRODUCT}</p>
      <p class="version">Version {app.snapshot?.version ?? '–'}</p>
    </div>
  </div>
  <p class="blurb">
    Push-to-talk dictation for macOS. Your voice is turned into text on this Mac and is never uploaded.
  </p>
  <Row label="Setup" help="Go through permissions, the model and a practice run again.">
    <button class="btn" onclick={() => app.updateSettings({ onboardingDone: false })}>Run setup again</button>
  </Row>
</Section>

<style>
  .about {
    display: flex;
    align-items: center;
    gap: 14px;
    margin-top: 14px;
  }
  .mark {
    display: flex;
    color: var(--ink);
  }
  .name {
    font-size: 15px;
    font-weight: 700;
  }
  .version {
    font-size: 12px;
    color: var(--ink-2);
  }
  .blurb {
    max-width: 58ch;
    margin: 14px 0 6px;
    font-size: 13px;
    line-height: 19px;
    color: var(--ink-2);
  }
</style>
