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
  meetingVoicePrint: boolean;    // learn the user's voice from dictations (voice.json: an average voice embedding, no audio) to find "Me" in meetings (default true)
  meetingTermCheck: boolean;     // after a meeting, fix words that sound like a vocabulary term (words to recognise, "PO = purchase order" entries, abbreviations/names shown) where the summary model agrees (default true)
  summaryModel: string;          // id from list_summary_models, e.g. "qwen3-4b-instruct-2507"
  meetingAudioDays: number;      // meeting WAVs are deleted after this many days; 0 = kept (default 14)
  meetingLiveHelp: boolean;      // live help while a meeting records; false turns questions and map off (no model runs; default true)
  meetingQuestions: boolean;     // propose questions to ask while a meeting records (needs the summary model; default true)
  meetingQuestionsWindow: boolean; // also show them in the floating `questions` window (default true)
  meetingMap: boolean;           // build a system map while a meeting records (same model process as the questions; default true)
  meetingScreen: boolean;        // while a meeting records, read the text of one window the user picks (macOS; default false)
  meetingPhotoWatch: boolean;    // offer images arriving in Downloads (AirDrop) while recording and 30 min after (default true)
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
// screen = macOS Screen Recording (only asked for when reading shared screens is on; a grant needs a relaunch); false on Windows.
interface Permissions { microphone: 'granted' | 'denied' | 'undetermined'; accessibility: boolean; inputMonitoring: boolean; screen: boolean }
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
interface MeetingSegment { startMs: number; endMs: number; channel: 'mic' | 'system'; speaker: number; text: string; language?: string;
  fixes?: { heard: string; written: string; at?: number }[] } // misheard terms the term check replaced in `text` (absent when none), in text order; `at` = where `written` starts (UTF-16 units; absent in meetings checked before it was kept)
interface MeetingSpeaker { id: number; name: string; renamed: boolean }
interface Meeting {
  id: string; title: string; titleFromUser: boolean; startedAt: string; endedAt: string | null; durationMs: number;
  status: MeetingStatus; notes: string /* the user's own */; goal: string /* steers the questions */;
  questions: MeetingQuestion[]; systemMap: SystemMap; segments: MeetingSegment[]; speakers: MeetingSpeaker[];
  visuals: MeetingVisual[] /* screens and photos; [] for meetings from before */;
  screenNames?: { atMs: number; names?: string[] }[] /* name-shaped text on each frame of the shared screen read, for naming speakers; absent when none */;
  summary: string | null /* Markdown: "# Title", summary, ## decisions / action items / notes */;
  summaryModel: string | null; language: string | null; error: string | null;
  systemAudio: boolean; stoppedByLimit: boolean; hasAudio: boolean;
}
// Live questions. At most 3 'live' at a time; answered ones close by themselves, unanswered live ones expire
// after 5 minutes; 'live' and 'later' ones left at the end are added to the notes as "Open questions".
type QuestionKind = 'understand' | 'challenge' | 'follow-up';
type QuestionState = 'live' | 'later' | 'asked' | 'answered' | 'expired' | 'deleted';
type QuestionStatus = 'off' | 'no-model' | 'starting' | 'listening' | 'thinking' | 'failed';
interface MeetingQuestion { number: number; text: string; kind: QuestionKind; state: QuestionState; atMs: number; changedMs: number;
  bigPicture?: true /* asked for as the question about the goal and the big picture (one every 10 minutes from the 5th) */ }
interface MeetingQuestions { questions: MeetingQuestion[]; status: QuestionStatus; goal: string }
// System map: built while a meeting records, a run per question run (talk since the last run + the user's notes);
// its open points steer the questions; added below the notes as "## System map" (see system_map.rs `section`).
type MapNodeKind = 'ours' | 'customer' | 'outside' | 'person';
interface MapNode { id: number; name: string; kind: MapNodeKind; known: string[]; atMs: number }
interface MapEdge { from: number; to: number; what: string; how: string } // how: "API", "CSV", "mail", "by hand", … or "unknown"
interface MapOpenPoint { number: number; node: number; text: string }
interface SystemMap { nodes: MapNode[]; edges: MapEdge[]; open: MapOpenPoint[] }
interface MeetingMap { map: SystemMap; status: QuestionStatus; alsoMentioned: string[] } // status of the live model shared with the questions; alsoMentioned: names said but not on the map (hidden candidates), latest first, at most 12
// What was shown: text read (OCR, macOS) from the shared screen or an image the user added. It enters the notes and
// the live questions as "[mm:ss] Screen: …" / "[mm:ss] Photo: …" lines, merged into the transcript by time for the
// notes, the latest after the transcript for the questions (capped).
// Pictures (visuals/<uuid>.jpg + -thumb.jpg) are deleted with the recordings; the text stays.
interface MeetingVisual { id: number; atMs: number; untilMs?: number /* screen last seen unchanged */; source: 'screen' | 'photo';
  label: string /* "zoom.us: Zoom Meeting" or the file name */; text: string /* a line per row, cells joined by " | " */; image?: string }
interface ScreenWindow { windowId: number; app: string; title: string /* empty without Screen Recording */ }
// off: setting off / not recording / Windows; choose: no window picked; needs-permission: macOS hands out wallpaper only.
type ScreenStatus = 'off' | 'choose' | 'reading' | 'paused' | 'needs-permission' | 'source-lost';
interface MeetingScreen { status: ScreenStatus; window: ScreenWindow | null }
interface MeetingItem { id: string; title: string; startedAt: string; durationMs: number; status: MeetingStatus; hasSummary: boolean; preview: string }
interface ActiveMeeting { id: string; startedAt: string; stopsAt: string | null }
interface SummaryModelInfo { id: string; displayName: string; sizeMb: number; downloaded: boolean; badge: string; description: string; recommended: boolean }
```

## Commands (`invoke`)

Meeting commands take the meeting's `id` (a UUID); an id with anything but letters, digits, `-` and `_`
(a path separator, `..`) is refused like a meeting that doesn't exist.

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
| `request_permission` | `{ kind: 'microphone' \| 'accessibility' \| 'inputMonitoring' \| 'screen' }` | – |
| `set_fn_do_nothing` | – | `FnUsage` |
| `copy_text` | `{ text }` | – |
| `show_dashboard` | – | – |
| `import_data` | `{ path }` (a folder the user picked) | `{ history: number, dictionary: number, suggestions: number, modelFolder: string \| null }` — detects JSON files by structure (records / entries / suggestions); if the folder (or its `models/` subfolder) contains ggml models it is added to `modelFolders` |
| `correct_history` | `{ id, text }` | – (sets text + reference) |
| `verify_history` | `{ id }` | – (reference = current text) |
| `list_vocabulary` | – | `string[]` (words to recognise; an entry may say what it stands for, "PO = purchase order": dictation hints use the term before "=", meetings the term check and the notes' glossary) |
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
| `meeting_map` | `{ id }` | `MeetingMap` (updates via `meeting-map`) |
| `rename_map_node` | `{ id, node: number, name }` | – (fails if another box has that name) |
| `delete_map_node` | `{ id, node: number }` | – (its flows and open points go too; the model won't add it again) |
| `act_on_question` | `{ id, number, act: 'later' \| 'asked' \| 'deleted' }` | – (only on 'live' or 'later' questions) |
| `show_questions_window` | `{ show: boolean }` | – (shows it only while a meeting records) |
| `resize_questions_window` | `{ width, height }` (logical px of the content) | – (top-left stays put) |
| `rename_speaker` | `{ id, speaker: number, name }` | `Meeting` |
| `delete_meeting` | `{ id }` | – (not while it's recording or being processed) |
| `summarize_meeting` | `{ id }` | – (writes the notes again in the background) |
| `meeting_folder` | `{ id }` | `string` (folder with `meeting.json`, `mic.wav`, `system.wav`; open with `open_path`) |
| `add_meeting_image` | `{ id, path, atMs?: number, offered?: boolean }` | `MeetingVisual` (reads it, ~0.5 s; at now while recording, else the end; fails on Windows). `path` must be an existing image file (jpg, jpeg, png, heic, heif, tif, tiff, gif, bmp, webp; at most 100 MB); `offered`: from a `meeting-photo-offer`, so it must be inside Downloads |
| `paste_meeting_image` | `{ id }` | `MeetingVisual[]` (image files copied in Finder, or a copied picture; fails if neither) |
| `delete_meeting_visual` | `{ id, visual: number }` | – (and its picture) |
| `move_meeting_visual` | `{ id, visual: number, atMs }` | – (clamped to the meeting's length) |
| `meeting_visual_image` | `{ id, visual: number, full?: boolean }` | `string \| null` (JPEG data URL, thumbnail unless `full`; null once deleted) |
| `open_meeting_visual` | `{ id, visual: number }` | – (opens the picture in the system viewer) |
| `meeting_screen` | `{ id }` | `MeetingScreen` (updates via `meeting-screen`) |
| `meeting_screen_sources` | – | `ScreenWindow[]` (on-screen windows of other apps, meeting apps first) |
| `set_meeting_screen` | `{ id, windowId: number \| null, paused: boolean }` | `MeetingScreen` (only while that meeting records) |
| `list_summary_models` | – | `SummaryModelInfo[]` |
| `download_summary_model` / `delete_summary_model` | `{ id }` | – (progress via `model-download-progress`) |
| `forget_voice_print` | – | – (deletes `voice.json`, the voice learnt from dictations; learnt again while `meetingVoicePrint` is on) |

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
| `meeting-progress` | `{ id, phase: 'transcribing' \| 'improving' \| 'speakers' \| 'loading' \| 'checking' \| 'reading' \| 'writing', fraction: number \| null, text: string \| null }` (`improving` = the final pass over the whole recording, with `fraction`; `checking` = the summary model checks words that sound like the user's terms, with `fraction`; `text` = notes so far while writing) |
| `meeting-changed` | `{ id }` (status, transcript or notes changed: reload it) |
| `meeting-questions` | `{ id } & MeetingQuestions` (questions, status or goal changed) |
| `meeting-map` | `{ id } & MeetingMap` (the system map or the live model's status changed) |
| `meeting-visuals` | `{ id, visuals: MeetingVisual[] }` (a screen or photo was added, moved or deleted) |
| `meeting-screen` | `{ id } & MeetingScreen` |
| `meeting-photo-offer` | `{ id, path, name }` (an image arrived in Downloads; the UI asks Add / Ignore) |
| `meetings-changed` | – |
| `summary-models-changed` | – |
