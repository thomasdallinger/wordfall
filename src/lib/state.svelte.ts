// Shared reactive store for both windows. Holds the backend snapshot plus
// lazily-loaded collections, and keeps them fresh by listening to IPC events.
import { api, on, type Unlisten } from './api';
import { SILENT_LEVELS } from './levels';
import type {
  AudioDevice,
  DictEntry,
  HistoryRecord,
  ModelInfo,
  Notice,
  NoticeKind,
  PermissionKind,
  Settings,
  Snapshot,
  Suggestion,
} from './types';

export type View = 'transcripts' | 'dictionary' | 'settings' | 'lab';
export type SettingsSection = 'general' | 'recording' | 'model' | 'pill' | 'accuracy' | 'data' | 'about';

export const SETTINGS_SECTIONS: { id: SettingsSection; label: string }[] = [
  { id: 'general', label: 'General' },
  { id: 'recording', label: 'Recording' },
  { id: 'model', label: 'Model' },
  { id: 'pill', label: 'Pill' },
  { id: 'accuracy', label: 'Accuracy' },
  { id: 'data', label: 'Data' },
  { id: 'about', label: 'About' },
];

export interface Toast extends Notice {
  id: number;
}

class AppState {
  snapshot = $state<Snapshot | null>(null);
  dictionary = $state<DictEntry[]>([]);
  suggestions = $state<Suggestion[]>([]);
  /** Hint-only words, and project names that could be added as hints. */
  vocabulary = $state<string[]>([]);
  suggestedVocabulary = $state<string[]>([]);
  models = $state<ModelInfo[]>([]);
  devices = $state<AudioDevice[]>([]);
  downloads = $state<Record<string, { downloaded: number; total: number }>>({});
  levels = $state<number[]>(SILENT_LEVELS);
  #historyListeners = new Set<() => void>();
  #sectionListeners = new Set<(id: SettingsSection) => void>();
  /** The most recent finished dictation (used by the onboarding practice area). */
  lastTranscript = $state<HistoryRecord | null>(null);
  toasts = $state<Toast[]>([]);

  // Dashboard navigation
  view = $state<View>('transcripts');
  /** Settings section currently in view (scroll spy) or requested via `go`. */
  settingsSection = $state<SettingsSection>('general');
  /** Set by `go('settings', section)` before the settings view exists; it scrolls there on mount. */
  pendingSection = $state<SettingsSection | null>(null);

  settings = $derived(this.snapshot?.settings ?? null);
  activeModelInfo = $derived(
    this.models.find((m) => m.id === (this.snapshot?.settings.model ?? '')) ?? null,
  );
  missingPermissions = $derived.by(() => {
    const p = this.snapshot?.permissions;
    if (!p) return [] as PermissionKind[];
    const out: PermissionKind[] = [];
    if (p.microphone !== 'granted') out.push('microphone');
    if (!p.accessibility) out.push('accessibility');
    if (!p.inputMonitoring) out.push('inputMonitoring');
    return out;
  });

  #unlisten: Unlisten[] = [];
  #toastSeq = 0;

  async init(kind: 'dashboard' | 'widget') {
    this.snapshot = await api.getSnapshot();
    await this.#subscribe(kind);
    if (kind === 'dashboard') {
      await Promise.all([this.loadDictionary(), this.loadModels(), this.loadDevices()]);
    }
  }

  destroy() {
    this.#unlisten.forEach((u) => u());
    this.#unlisten = [];
  }

  async #subscribe(kind: 'dashboard' | 'widget') {
    const subs: Promise<Unlisten>[] = [
      on('recording-state', ({ state, mode }) => {
        if (!this.snapshot) return;
        this.snapshot.recState = state;
        this.snapshot.recMode = mode;
        if (state !== 'listening') this.levels = SILENT_LEVELS;
      }),
      on('audio-levels', ({ levels }) => (this.levels = levels)),
      on('settings-changed', (s) => {
        if (this.snapshot) this.snapshot.settings = s;
      }),
      on('model-state', ({ state, model, error }) => {
        if (!this.snapshot) return;
        this.snapshot.modelState = state;
        this.snapshot.activeModel = state === 'ready' ? model : this.snapshot.activeModel;
        this.snapshot.modelError = error;
      }),
      on('permissions-changed', (p) => {
        if (this.snapshot) this.snapshot.permissions = p;
      }),
    ];
    if (kind === 'dashboard') {
      subs.push(
        on('notice', (n) => this.toast(n.kind, n.title, n.message)),
        on('history-changed', () => this.#onHistoryChanged()),
        on('transcription-complete', ({ record }) => {
          this.lastTranscript = record;
          this.#onHistoryChanged();
        }),
        on('dictionary-changed', () => this.loadDictionary()),
        on('models-changed', () => this.loadModels()),
        on('model-download-progress', (p) => {
          this.downloads[p.id] = { downloaded: p.downloaded, total: p.total };
        }),
      );
    }
    this.#unlisten = await Promise.all(subs);
  }

  #historyTimer: ReturnType<typeof setTimeout> | null = null;
  #onHistoryChanged() {
    // transcription-complete and history-changed usually arrive together.
    if (this.#historyTimer) clearTimeout(this.#historyTimer);
    this.#historyTimer = setTimeout(() => {
      this.historyChanged();
      void this.refreshStats();
    }, 30);
  }

  /** Tells open lists that history changed (new dictation, import, clear). */
  historyChanged() {
    this.#historyListeners.forEach((fn) => fn());
  }

  /** Runs `fn` whenever history changes. Returns an unsubscribe function. */
  onHistoryChanged(fn: () => void): () => void {
    this.#historyListeners.add(fn);
    return () => this.#historyListeners.delete(fn);
  }

  /** Runs `fn` when something asks to show a settings section. Returns an unsubscribe function. */
  onSectionRequest(fn: (id: SettingsSection) => void): () => void {
    this.#sectionListeners.add(fn);
    return () => this.#sectionListeners.delete(fn);
  }

  async refreshSnapshot() {
    this.snapshot = await api.getSnapshot();
  }

  async refreshStats() {
    const stats = await api.getStats();
    if (this.snapshot) this.snapshot.stats = stats;
  }

  async loadDictionary() {
    const [d, s, v, sv] = await Promise.all([
      api.listDictionary(),
      api.listSuggestions(),
      api.listVocabulary(),
      api.suggestedVocabulary(),
    ]);
    this.dictionary = d;
    this.suggestions = s;
    this.vocabulary = v;
    this.suggestedVocabulary = sv;
  }

  async setVocabulary(terms: string[]) {
    const prev = { vocabulary: this.vocabulary, suggested: this.suggestedVocabulary };
    const lower = new Set(terms.map((t) => t.toLowerCase()));
    this.vocabulary = terms; // optimistic
    this.suggestedVocabulary = this.suggestedVocabulary.filter((t) => !lower.has(t.toLowerCase()));
    try {
      this.vocabulary = await api.setVocabulary(terms);
      this.suggestedVocabulary = await api.suggestedVocabulary();
    } catch (e) {
      this.vocabulary = prev.vocabulary;
      this.suggestedVocabulary = prev.suggested;
      this.error('The word list was not saved', e);
    }
  }

  async loadModels() {
    this.models = await api.listModels();
    for (const m of this.models) if (m.downloaded) delete this.downloads[m.id];
  }

  async loadDevices() {
    this.devices = await api.listInputDevices();
  }

  async updateSettings(patch: Partial<Settings>) {
    if (!this.snapshot) return;
    const prev = { ...this.snapshot.settings };
    Object.assign(this.snapshot.settings, patch); // optimistic
    try {
      this.snapshot.settings = await api.updateSettings(patch);
    } catch (e) {
      this.snapshot.settings = prev;
      this.error('That change was not saved', e);
    }
  }

  async deleteHistory(id: string): Promise<boolean> {
    try {
      await api.deleteHistory(id);
      return true;
    } catch (e) {
      this.error('The transcript is still there', e);
      return false;
    }
  }

  /** Saves a corrected transcript as benchmark ground truth. Resolves false on failure. */
  async correctHistory(id: string, text: string): Promise<boolean> {
    try {
      await api.correctHistory(id, text.trim());
      this.toast('success', 'Correction saved');
      return true;
    } catch (e) {
      this.error('The correction was not saved', e);
      return false;
    }
  }

  /** Marks a transcript as exactly right (ground truth = current text). */
  async verifyHistory(id: string): Promise<boolean> {
    try {
      await api.verifyHistory(id);
      return true;
    } catch (e) {
      this.error('Could not mark it as right', e);
      return false;
    }
  }

  async copy(text: string, label = 'Copied') {
    try {
      await api.copyText(text);
      this.toast('success', label);
    } catch (e) {
      this.error('Nothing was copied', e);
    }
  }

  async requestPermission(kind: PermissionKind) {
    try {
      await api.requestPermission(kind);
    } catch (e) {
      this.error('System Settings did not open', e);
    }
  }

  async refreshPermissions() {
    const p = await api.checkPermissions();
    if (this.snapshot) this.snapshot.permissions = p;
  }

  async fixFnKey() {
    try {
      const fn = await api.setFnDoNothing();
      if (this.snapshot) this.snapshot.fnUsage = fn;
      if (fn.ok) this.toast('success', 'The Globe key is free now');
    } catch (e) {
      this.error('The Globe key setting did not change', e);
    }
  }

  go(view: View, section?: SettingsSection) {
    this.view = view;
    if (view !== 'settings' || !section) return;
    // An open settings view scrolls right away; otherwise it picks this up when it mounts.
    if (this.#sectionListeners.size > 0) this.#sectionListeners.forEach((fn) => fn(section));
    else this.pendingSection = section;
  }

  toast(kind: NoticeKind, title: string, message = '') {
    const id = ++this.#toastSeq;
    this.toasts = [...this.toasts.slice(-2), { id, kind, title, message }];
    setTimeout(() => this.dismissToast(id), kind === 'error' ? 6000 : 3500);
  }

  dismissToast(id: number) {
    this.toasts = this.toasts.filter((t) => t.id !== id);
  }

  error(title: string, e: unknown) {
    this.toast('error', title, e instanceof Error ? e.message : String(e ?? ''));
  }
}

export const app = new AppState();
