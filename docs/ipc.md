# Wordfall IPC contract (Rust core <-> Svelte UI)

Windows (labels): `dashboard` (main UI) and `widget` (floating pill, transparent, always on top).
The UI picks its view from `getCurrentWindow().label`.

All payload keys are camelCase.

## Types

```ts
type RecState = 'idle' | 'listening' | 'processing';
type RecMode = 'ptt' | 'toggle' | 'latched' | 'click' | null;
type PttKey = 'fn' | 'right_option' | 'right_command' | 'right_control' | 'right_shift' | 'none';
type WidgetVisibility = 'hidden' | 'recording' | 'always';
type PillStyle = 'logo-and-text' | 'logo-only' | 'logo-only-monochrome';

interface Settings {
  model: string;                 // e.g. "distil-large-v3.5"
  language: string | null;       // null = auto (English-only models ignore it)
  removeFillers: boolean;
  pttKey: PttKey;                // default "fn"
  toggleShortcut: string | null; // Tauri accelerator, e.g. "Alt+Space"
  showWidgetShortcut: string;    // fixed: "CommandOrControl+Shift+Space"
  inputDevice: string | null;    // null = system default
  inputGain: number;             // 1 | 1.5 | 2 | 3
  soundsEnabled: boolean;
  muteSystemAudio: boolean;
  copyOnly: boolean;             // copy to clipboard instead of pasting
  launchAtLogin: boolean;
  widgetVisibility: WidgetVisibility;
  pillStyle: PillStyle;
  followCursor: boolean;         // move widget to the monitor with the cursor
  widgetLocked: boolean;
  dictionaryEnabled: boolean;
  onboardingDone: boolean;
  modelFolders: string[];       // extra folders searched for ggml Whisper models
  keepAudio: boolean;            // keep WAV of each dictation for correction/benchmarking
  promptMode: 'none' | 'dictionary' | 'projects' | 'glossary'; // vocabulary hints fed to Whisper
  decode: Record<string, unknown>; // advanced decoder knobs (tuned via `wordfall bench`); UI treats as opaque
}

interface HistoryRecord {
  id: string; text: string; raw_text: string | null; model: string | null;
  word_count: number; duration_seconds: number; transcription_ms: number | null;
  timestamp: string; // RFC3339
  reference?: string | null; // set once the user corrected/verified it (benchmark ground truth)
  has_audio: boolean;        // a WAV of this dictation is kept
}

interface LabPrompt { id: string; category: 'short' | 'jargon' | 'numbers' | 'prose' | 'long' | 'personal';
  text: string; terms: string[]; recorded: boolean; hypothesis: string | null; wer: number | null }
interface LabStopResult { promptId: string; hypothesis: string; wer: number; ms: number; seconds: number }
interface BenchInfo { root: string; tiers: Record<string, number>; historyWithAudio: number;
  historyVerified: number; latestReport: string | null }

interface Stats {
  totalWords: number; totalSeconds: number; totalSessions: number;
  firstUsed: string | null; lastUsed: string | null; streak: number; wpm: number;
}

interface DictEntry { id: string; original: string; replacement: string; createdAt: string }
interface Suggestion { id: string; detectedWord: string; suggestedReplacement: string;
  reason: string; confidence: number; occurrenceCount: number }

interface ModelInfo {
  id: string; displayName: string; sizeMb: number; downloaded: boolean;
  path: string | null; source: 'app' | 'folder' | null; // 'folder' = found in one of settings.modelFolders
  badge: string; description: string; englishOnly: boolean; recommended: boolean;
}
type ModelState = 'unloaded' | 'loading' | 'ready' | 'error';

interface Permissions { microphone: 'granted' | 'denied' | 'undetermined'; accessibility: boolean; inputMonitoring: boolean }
interface AudioDevice { name: string; isDefault: boolean }
interface FnUsage { value: number | null; label: string; ok: boolean } // ok = Fn/Globe set to "Do Nothing"

interface Snapshot {
  settings: Settings; stats: Stats; recState: RecState; recMode: RecMode;
  modelState: ModelState; modelError: string | null; activeModel: string | null;
  permissions: Permissions; fnUsage: FnUsage; pttListening: boolean; version: string;
}
```

## Commands (`invoke`)

| command | args | returns |
|---|---|---|
| `get_snapshot` | – | `Snapshot` |
| `update_settings` | `{ patch: Partial<Settings> }` | `Settings` |
| `get_history` | `{ query?: string, offset: number, limit: number }` | `{ total: number, records: HistoryRecord[] }` |
| `delete_history` | `{ id }` | – |
| `clear_history` | – | – |
| `export_history_csv` | – | `string` (path written, in ~/Downloads) |
| `get_stats` | – | `Stats` |
| `list_dictionary` | – | `DictEntry[]` |
| `upsert_dictionary` | `{ id?: string, original, replacement }` | `DictEntry` |
| `delete_dictionary` | `{ id }` | – |
| `list_suggestions` | – | `Suggestion[]` |
| `accept_suggestion` | `{ id, replacement?: string }` | – |
| `dismiss_suggestion` | `{ id }` | – |
| `dismiss_all_suggestions` | – | – |
| `list_models` | – | `ModelInfo[]` |
| `download_model` | `{ id }` | – (progress via events) |
| `delete_model` | `{ id }` | – |
| `set_model` | `{ id }` | – (state via `model-state`) |
| `start_recording` / `stop_recording` / `cancel_recording` / `toggle_recording` | – | – |
| `list_input_devices` | – | `AudioDevice[]` |
| `check_permissions` | – | `Permissions` |
| `request_permission` | `{ kind: 'microphone' \| 'accessibility' \| 'inputMonitoring' }` | – |
| `set_fn_do_nothing` | – | `FnUsage` |
| `copy_text` | `{ text }` | – |
| `show_dashboard` | – | – |
| `import_data` | `{ path }` (a folder the user picked) | `{ history: number, dictionary: number, suggestions: number, modelFolder: string \| null }` — detects JSON files by structure (records / entries / suggestions); if the folder (or its `models/` subfolder) contains ggml models it is added to `modelFolders` |
| `correct_history` | `{ id, text }` | – (sets text + reference) |
| `verify_history` | `{ id }` | – (reference = current text) |
| `list_vocabulary` | – | `string[]` (hint-only words) |
| `set_vocabulary` | `{ terms: string[] }` | `string[]` |
| `suggested_vocabulary` | – | `string[]` (project folder names not yet added) |
| `lab_prompts` | – | `LabPrompt[]` |
| `lab_start` | `{ promptId }` | – (emits `lab-levels`) |
| `lab_stop` | – | `LabStopResult` (saves clip, transcribes for feedback; may take ~1 s) |
| `lab_cancel` | – | – |
| `lab_delete` | `{ promptId }` | – |
| `bench_info` | – | `BenchInfo` |
| `open_path` | `{ path }` | – (opens a file/folder inside Wordfall's data dir) |
| `resize_widget` | `{ width: number, height: number }` (logical px of the widget content) | – |

## Events (`listen`)

| event | payload |
|---|---|
| `recording-state` | `{ state: RecState, mode: RecMode }` |
| `audio-levels` | `{ levels: number[] }` (7 values 0..1, ~30 fps while listening) |
| `transcription-complete` | `{ record: HistoryRecord }` |
| `notice` | `{ kind: 'info'\|'success'\|'warning'\|'error', title: string, message: string }` |
| `settings-changed` | `Settings` |
| `history-changed` | – |
| `dictionary-changed` | – |
| `models-changed` | – |
| `model-state` | `{ state: ModelState, model: string \| null, error: string \| null }` |
| `model-download-progress` | `{ id: string, downloaded: number, total: number }` |
| `permissions-changed` | `Permissions` |
| `lab-levels` | `{ levels: number[] }` (7 values, ~30 fps while a Voice Lab clip records) |
