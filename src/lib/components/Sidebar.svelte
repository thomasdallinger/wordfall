<script lang="ts">
  import { PRODUCT } from '../brand';
  import { acceleratorParts, pttCap } from '../format';
  import { app, SETTINGS_SECTIONS, type SettingsSection, type View } from '../state.svelte';
  import Keycaps from './Keycaps.svelte';
  import Logo from './Logo.svelte';

  const items: { id: View; label: string; key: string }[] = [
    { id: 'transcripts', label: 'Transcripts', key: '1' },
    { id: 'dictionary', label: 'Dictionary', key: '2' },
    { id: 'settings', label: 'Settings', key: '3' },
  ];

  const inSettings = $derived(app.view === 'settings' || app.view === 'lab');
  const rec = $derived(app.snapshot?.recState ?? 'idle');
  const cap = $derived(pttCap(app.settings?.pttKey));
  const toggleKeys = $derived(acceleratorParts(app.settings?.toggleShortcut));

  function isActive(id: View) {
    return id === 'settings' ? inSettings : app.view === id;
  }

  function goSection(id: SettingsSection) {
    app.go('settings', id);
  }
</script>

<nav class="sidebar" aria-label="Main">
  <div class="titlebar" data-tauri-drag-region></div>

  <ul class="nav">
    {#each items as item (item.id)}
      {@const active = isActive(item.id)}
      <li>
        <button
          class="nav-item"
          class:active
          aria-current={active && app.view !== 'lab' ? 'page' : undefined}
          aria-keyshortcuts="Meta+{item.key}"
          onclick={() => app.go(item.id)}
        >
          <span>{item.label}</span>
          {#if item.id === 'dictionary' && app.suggestions.length > 0}
            <span class="count" aria-label="{app.suggestions.length} suggested">{app.suggestions.length}</span>
          {/if}
        </button>
        {#if item.id === 'settings' && inSettings}
          <ul class="sub" aria-label="Settings sections">
            {#each SETTINGS_SECTIONS as s (s.id)}
              {@const here = app.view === 'settings' && app.settingsSection === s.id}
              <li>
                <button class="sub-item" class:here aria-current={here ? 'location' : undefined} onclick={() => goSection(s.id)}>
                  {s.label}
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      </li>
    {/each}
  </ul>

  <div class="foot" aria-live="polite">
    {#if rec === 'listening'}
      <span class="live-dot" aria-hidden="true"></span>
      <span>Listening</span>
    {:else if rec === 'processing'}
      <span class="busy-dot" aria-hidden="true"></span>
      <span>Writing it out</span>
    {:else if cap}
      <span class="hint">Hold <kbd class="keycap">{cap}</kbd> to dictate</span>
    {:else if toggleKeys.length}
      <span class="hint">Press <Keycaps keys={toggleKeys} /> to dictate</span>
    {:else}
      <span class="mark"><Logo size={14} /> {PRODUCT}</span>
    {/if}
  </div>
</nav>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    width: var(--sidebar-width);
    height: 100%;
    flex-shrink: 0;
    background: var(--bg);
    border-right: 1px solid var(--line);
  }
  .titlebar {
    height: var(--toolbar-height);
    flex-shrink: 0;
  }
  .nav {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 4px 10px;
  }
  .nav-item {
    display: flex;
    width: 100%;
    height: 30px;
    align-items: center;
    justify-content: space-between;
    padding: 0 10px;
    border-radius: var(--radius-control);
    font-size: 13px;
    font-weight: 500;
    color: var(--ink-2);
    transition:
      background-color var(--dur) var(--ease),
      color var(--dur) var(--ease);
  }
  .nav-item:hover {
    color: var(--ink);
    background: var(--hover);
  }
  .nav-item.active {
    color: var(--ink);
    font-weight: 700;
    background: var(--press);
  }
  .count {
    font-size: 12px;
    font-weight: 500;
    color: var(--ink-2);
    font-variant-numeric: tabular-nums;
  }
  .sub {
    display: flex;
    flex-direction: column;
    margin: 4px 0 6px;
  }
  .sub-item {
    display: flex;
    width: 100%;
    height: 26px;
    align-items: center;
    padding: 0 10px 0 22px;
    border-radius: var(--radius-control);
    font-size: 13px;
    color: var(--ink-2);
    transition: color var(--dur) var(--ease);
  }
  .sub-item:hover {
    color: var(--ink);
  }
  .sub-item.here {
    color: var(--ink);
    box-shadow: inset 2px 0 0 var(--ink);
    border-radius: 0 var(--radius-control) var(--radius-control) 0;
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: auto;
    padding: 16px 20px 18px;
    font-size: 12px;
    color: var(--ink-2);
  }
  .hint {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    white-space: nowrap;
  }
  .mark {
    display: inline-flex;
    align-items: center;
    gap: 6px;
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
</style>
