<script lang="ts">
  import ConfirmDialog from '../../lib/components/ConfirmDialog.svelte';
  import Row from '../../lib/components/Row.svelte';
  import Section from '../../lib/components/Section.svelte';
  import { api, pickFolder } from '../../lib/api';
  import { plural, shortPath } from '../../lib/format';
  import { THIS_DEVICE } from '../../lib/platform';
  import { app } from '../../lib/state.svelte';

  let importing = $state(false);
  let confirm: ConfirmDialog | undefined = $state();

  async function runImport() {
    const path = await pickFolder('Choose a folder to import from');
    if (!path) return;
    importing = true;
    try {
      const r = await api.importData(path);
      const parts = [plural(r.history, 'transcript'), plural(r.dictionary, 'replacement'), plural(r.suggestions, 'suggestion')];
      app.toast(
        'success',
        `Imported ${parts[0]}, ${parts[1]} and ${parts[2]}`,
        r.modelFolder ? `Models in ${shortPath(r.modelFolder)} are now available.` : '',
      );
      app.historyChanged();
      await Promise.all([app.refreshStats(), app.loadDictionary(), app.loadModels()]);
    } catch (e) {
      app.error('Nothing was imported', e);
    } finally {
      importing = false;
    }
  }

  async function exportCsv() {
    try {
      const path = await api.exportHistoryCsv();
      app.toast('success', 'Saved a CSV of every transcript', shortPath(path));
    } catch (e) {
      app.error('The CSV was not saved', e);
    }
  }

  async function clearAll() {
    try {
      await api.clearHistory();
      app.historyChanged();
      app.toast('success', 'All transcripts deleted');
    } catch (e) {
      app.error('The transcripts are still there', e);
    }
  }
</script>

<Section id="data" title="Data" intro="Everything stays on {THIS_DEVICE} unless you export it.">
  <Row
    label="Import from a folder"
    help="Reads transcripts, dictionary entries and suggestions from JSON files in a folder you pick. Any Whisper models inside are used from there."
  >
    <button class="btn" disabled={importing} onclick={runImport}>{importing ? 'Importing…' : 'Choose folder…'}</button>
  </Row>
  <Row label="Export transcripts" help="Saves all of them as one CSV file in Downloads.">
    <button class="btn" onclick={exportCsv}>Export</button>
  </Row>
  <Row label="Delete all transcripts" help="Removes every transcript and its recording. The dictionary is not touched.">
    <button class="btn btn-bad" onclick={() => confirm?.show()}>Delete all…</button>
  </Row>
</Section>

<ConfirmDialog
  bind:this={confirm}
  title="Delete every transcript?"
  message="This removes all transcripts and kept recordings from {THIS_DEVICE}. It can't be undone."
  confirmLabel="Delete all"
  onconfirm={clearAll}
/>
