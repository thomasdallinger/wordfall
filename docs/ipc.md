# Wordfall IPC contract (Rust core <-> Svelte UI)

Windows (labels): `dashboard` (main UI), `widget` (floating pill, transparent, always on top) and `questions`
(live questions of the meeting being recorded: transparent, always on top, never takes focus; open while one records).
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
  languages: string[];           // ISO 639-1 codes the user speaks; [] = any, one = forced,
                                 // several = detected per dictation among them (English-only models ignore it).
                                 // Default: ["en"] + the system language. Replaces `language` (migrated on load).
  removeFillers: boolean;        // um/uh/ähm…; "um" is kept when the text is German or Portuguese
  formatLists: boolean;          // "first … second …" / "bullet point …" become "1. …" / "- …" lines
  pttKey: PttKey;                // default "fn" (macOS), "right_control" (Windows; no "fn" there)
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
  meetingMaxMinutes: number;     // meetings stop by themselves after this long; 0 = only when stopped (default 180)
  meetingSystemAudio: boolean;   // also record what the computer plays (Windows loopback, macOS 14.2+ process tap)
  meetingSpeakers: boolean;      // tell speakers apart (30 MB CAM++ voice model, fetched on first meeting)
  summaryModel: string;          // id from list_summary_models, e.g. "qwen3-4b-instruct-2507"
  meetingAudioDays: number;      // meeting WAVs are deleted after this many days; 0 = kept (default 14)
  meetingQuestions: boolean;     // propose questions to ask while a meeting records (needs the summary model; default true)
  meetingQuestionsWindow: boolean; // also show them in the floating `questions` window (default true)
}

interface HistoryRecord {
  id: string; text: string; raw_text: string | null; model: string | null;
  word_count: number; duration_seconds: number; transcription_ms: number | null;
  language?: string | null; // ISO 639-1 the dictation was decoded as; omitted for older records
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

// On Windows, accessibility and inputMonitoring are always true; microphone reflects the privacy setting.
interface Permissions { microphone: 'granted' | 'denied' | 'undetermined'; accessibility: boolean; inputMonitoring: boolean }
interface AudioDevice { name: string; isDefault: boolean }
interface FnUsage { value: number | null; label: string; ok: boolean } // ok = Fn/Globe set to "Do Nothing"; always ok on Windows

interface Snapshot {
  settings: Settings; stats: Stats; recState: RecState; recMode: RecMode;
  modelState: ModelState; modelError: string | null; activeModel: string | null;
  permissions: Permissions; fnUsage: FnUsage; pttListening: boolean; version: string;
  meeting: ActiveMeeting | null; // the meeting being recorded
}

// Meetings. Times in ms from the start of the meeting; speaker 0 is "Me".
type MeetingStatus = 'recording' | 'processing' | 'summarizing' | 'done';
interface MeetingSegment { startMs: number; endMs: number; channel: 'mic' | 'system'; speaker: number; text: string; language?: string }
interface MeetingSpeaker { id: number; name: string; renamed: boolean }
interface Meeting {
  id: string; title: string; titleFromUser: boolean; startedAt: string; endedAt: string | null; durationMs: number;
  status: MeetingStatus; notes: string /* the user's own */; goal: string /* steers the questions */;
  questions: MeetingQuestion[]; segments: MeetingSegment[]; speakers: MeetingSpeaker[];
  summary: string | null /* Markdown: "# Title", summary, ## decisions / action items / notes */;
  summaryModel: string | null; language: string | null; error: string | null;
  systemAudio: boolean; stoppedByLimit: boolean; hasAudio: boolean;
}
// Live questions. At most 3 'live' at a time; answered ones close by themselves, unanswered live ones expire
// after 5 minutes; 'live' and 'later' ones left at the end are added to the notes as "Open questions".
type QuestionKind = 'understand' | 'challenge' | 'follow-up';
type QuestionState = 'live' | 'later' | 'asked' | 'answered' | 'expired' | 'deleted';
type QuestionStatus = 'off' | 'no-model' | 'starting' | 'listening' | 'thinking' | 'failed';
interface MeetingQuestion { number: number; text: string; kind: QuestionKind; state: QuestionState; atMs: number; changedMs: number }
interface MeetingQuestions { questions: MeetingQuestion[]; status: QuestionStatus; goal: string }
interface MeetingItem { id: string; title: string; startedAt: string; durationMs: number; status: MeetingStatus; hasSummary: boolean; preview: string }
interface ActiveMeeting { id: string; startedAt: string; stopsAt: string | null }
interface SummaryModelInfo { id: string; displayName: string; sizeMb: number; downloaded: boolean; badge: string; description: string; recommended: boolean }
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
| `list_meetings` | – | `MeetingItem[]` (newest first) |
| `get_meeting` | `{ id }` | `Meeting` |
| `start_meeting` | – | `string` (id; fails if one is running or the microphone is unavailable) |
| `stop_meeting` | – | – (the rest is transcribed, speakers labelled, then notes written; follow `meeting-progress`) |
| `update_meeting` | `{ id, patch: { title?: string, notes?: string, goal?: string } }` | – (an empty title lets the notes name it) |
| `meeting_questions` | `{ id }` | `MeetingQuestions` (updates via `meeting-questions`) |
| `act_on_question` | `{ id, number, act: 'later' \| 'asked' \| 'deleted' }` | – (only on 'live' or 'later' questions) |
| `show_questions_window` | `{ show: boolean }` | – (shows it only while a meeting records) |
| `resize_questions_window` | `{ width, height }` (logical px of the content) | – (top-left stays put) |
| `rename_speaker` | `{ id, speaker: number, name }` | `Meeting` |
| `delete_meeting` | `{ id }` | – (not while it's recording or being processed) |
| `summarize_meeting` | `{ id }` | – (writes the notes again in the background) |
| `meeting_folder` | `{ id }` | `string` (folder with `meeting.json`, `mic.wav`, `system.wav`; open with `open_path`) |
| `list_summary_models` | – | `SummaryModelInfo[]` |
| `download_summary_model` / `delete_summary_model` | `{ id }` | – (progress via `model-download-progress`) |

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
| `meeting-state` | `{ active: ActiveMeeting \| null }` |
| `meeting-levels` | `{ mic?: number, system?: number }` (0..1, 5 per second while recording) |
| `meeting-segments` | `{ id, segments: MeetingSegment[], speakers: MeetingSpeaker[] }` (new transcript; speakers are provisional until the meeting ends) |
| `meeting-progress` | `{ id, phase: 'transcribing' \| 'speakers' \| 'loading' \| 'reading' \| 'writing', fraction: number \| null, text: string \| null }` (`text` = notes so far while writing) |
| `meeting-changed` | `{ id }` (status, transcript or notes changed: reload it) |
| `meeting-questions` | `{ id } & MeetingQuestions` (questions, status or goal changed) |
| `meetings-changed` | – |
| `summary-models-changed` | – |
