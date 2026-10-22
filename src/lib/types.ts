// Mirrors docs/ipc.md — keep in sync with the Rust core.

export type RecState = 'idle' | 'listening' | 'processing';
export type RecMode = 'ptt' | 'toggle' | 'latched' | 'click' | null;
export type PttKey = 'fn' | 'right_option' | 'right_command' | 'right_control' | 'right_shift' | 'none';
export type WidgetVisibility = 'hidden' | 'recording' | 'always';
export type PillStyle = 'logo-and-text' | 'logo-only' | 'logo-only-monochrome';
/** Vocabulary hints fed to Whisper: none, dictionary words, + project names, + dev glossary. */
export type PromptMode = 'none' | 'dictionary' | 'projects' | 'glossary';

export interface Settings {
  model: string;
  language: string | null;
  removeFillers: boolean;
  pttKey: PttKey;
  toggleShortcut: string | null;
  showWidgetShortcut: string;
  inputDevice: string | null;
  inputGain: number;
  soundsEnabled: boolean;
  muteSystemAudio: boolean;
  copyOnly: boolean;
  launchAtLogin: boolean;
  widgetVisibility: WidgetVisibility;
  pillStyle: PillStyle;
  followCursor: boolean;
  widgetLocked: boolean;
  dictionaryEnabled: boolean;
  onboardingDone: boolean;
  /** Extra folders searched for ggml Whisper models. */
  modelFolders: string[];
  /** Keep a WAV of each dictation for correction/benchmarking. */
  keepAudio: boolean;
  promptMode: PromptMode;
  /** Advanced decoder knobs (tuned with the bench tool); opaque to the UI. */
  decode: Record<string, unknown>;
}

export interface HistoryRecord {
  id: string;
  text: string;
  raw_text: string | null;
  model: string | null;
  word_count: number;
  duration_seconds: number;
  transcription_ms: number | null;
  timestamp: string;
  /** Benchmark ground truth, set once the user corrected or verified it. Omitted when unset. */
  reference?: string | null;
  /** A WAV of this dictation is kept. */
  has_audio: boolean;
}

export interface HistoryPage {
  total: number;
  records: HistoryRecord[];
}

export interface Stats {
  totalWords: number;
  totalSeconds: number;
  totalSessions: number;
  firstUsed: string | null;
  lastUsed: string | null;
  streak: number;
  wpm: number;
}

export interface DictEntry {
  id: string;
  original: string;
  replacement: string;
  createdAt: string;
}

export interface Suggestion {
  id: string;
  detectedWord: string;
  suggestedReplacement: string;
  reason: string;
  confidence: number;
  occurrenceCount: number;
}

export interface ModelInfo {
  id: string;
  displayName: string;
  sizeMb: number;
  downloaded: boolean;
  path: string | null;
  /** 'folder' = found in one of `settings.modelFolders`. */
  source: 'app' | 'folder' | null;
  badge: string;
  description: string;
  englishOnly: boolean;
  recommended: boolean;
}

export type ModelState = 'unloaded' | 'loading' | 'ready' | 'error';
export type MicPermission = 'granted' | 'denied' | 'undetermined';
export type PermissionKind = 'microphone' | 'accessibility' | 'inputMonitoring';

export interface Permissions {
  microphone: MicPermission;
  accessibility: boolean;
  inputMonitoring: boolean;
}

export interface AudioDevice {
  name: string;
  isDefault: boolean;
}

export interface FnUsage {
  value: number | null;
  label: string;
  ok: boolean;
}

export interface Snapshot {
  settings: Settings;
  stats: Stats;
  recState: RecState;
  recMode: RecMode;
  modelState: ModelState;
  modelError: string | null;
  activeModel: string | null;
  permissions: Permissions;
  fnUsage: FnUsage;
  pttListening: boolean;
  version: string;
}

export type LabCategory = 'short' | 'jargon' | 'numbers' | 'prose' | 'long' | 'personal';

export interface LabPrompt {
  id: string;
  category: LabCategory;
  text: string;
  /** Words that must appear verbatim (jargon recall). */
  terms: string[];
  recorded: boolean;
  /** Last transcription of the user's recording. */
  hypothesis: string | null;
  /** Word error rate as a fraction (0.12 = 12%); can exceed 1. */
  wer: number | null;
}

export interface LabStopResult {
  promptId: string;
  hypothesis: string;
  wer: number;
  ms: number;
  seconds: number;
}

export interface BenchInfo {
  root: string;
  /** Clip count per corpus tier (`lab`, `smoke`, `noise`, …). */
  tiers: Record<string, number>;
  historyWithAudio: number;
  historyVerified: number;
  latestReport: string | null;
}

export interface ImportResult {
  history: number;
  dictionary: number;
  suggestions: number;
  /** Set when the picked folder (or its `models/` subfolder) held models and was added to `modelFolders`. */
  modelFolder: string | null;
}

export type NoticeKind = 'info' | 'success' | 'warning' | 'error';

export interface Notice {
  kind: NoticeKind;
  title: string;
  message: string;
}

/** Event name -> payload. */
export interface EventMap {
  'recording-state': { state: RecState; mode: RecMode };
  'audio-levels': { levels: number[] };
  'transcription-complete': { record: HistoryRecord };
  notice: Notice;
  'settings-changed': Settings;
  'history-changed': null;
  'dictionary-changed': null;
  'models-changed': null;
  'model-state': { state: ModelState; model: string | null; error: string | null };
  'model-download-progress': { id: string; downloaded: number; total: number };
  'permissions-changed': Permissions;
  'lab-levels': { levels: number[] };
}

export type EventName = keyof EventMap;
