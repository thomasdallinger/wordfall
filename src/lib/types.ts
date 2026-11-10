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
  /** Languages the user dictates in (ISO 639-1). Empty = any; several = detected per dictation among them. */
  languages: string[];
  removeFillers: boolean;
  /** Turn spoken enumerations ("first … second …", "bullet point …") into lists. */
  formatLists: boolean;
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
  /** Meetings stop by themselves after this many minutes (0 = only when stopped). */
  meetingMaxMinutes: number;
  /** Record what the computer plays (the other participants), not just the microphone. */
  meetingSystemAudio: boolean;
  /** Tell speakers apart in meeting transcripts. */
  meetingSpeakers: boolean;
  /** Language model that writes meeting notes. */
  summaryModel: string;
  /** Meeting recordings are deleted after this many days (0 = kept). */
  meetingAudioDays: number;
  /** Live help while a meeting records (questions and system map); off turns both off. */
  meetingLiveHelp: boolean;
  /** Propose questions to ask while a meeting is recorded (needs the summary model). */
  meetingQuestions: boolean;
  /** Also show them in a floating window above other apps. */
  meetingQuestionsWindow: boolean;
  /** Build a map of the systems and people that come up while a meeting is recorded. */
  meetingMap: boolean;
}

export type MeetingStatus = 'recording' | 'processing' | 'summarizing' | 'done';
export type Channel = 'mic' | 'system';

export interface MeetingSegment {
  startMs: number;
  endMs: number;
  channel: Channel;
  speaker: number;
  text: string;
  language?: string;
}

export interface MeetingSpeaker {
  id: number;
  name: string;
  renamed: boolean;
}

export interface Meeting {
  id: string;
  /** Empty until the user or the notes name it. */
  title: string;
  titleFromUser: boolean;
  startedAt: string;
  endedAt: string | null;
  durationMs: number;
  status: MeetingStatus;
  /** The user's own notes. */
  notes: string;
  /** What the user wants out of the meeting; live questions steer towards it. */
  goal: string;
  /** Questions proposed while it was recorded. */
  questions: MeetingQuestion[];
  /** Systems, people and how they connect, built while it was recorded. */
  systemMap: SystemMap;
  segments: MeetingSegment[];
  speakers: MeetingSpeaker[];
  /** Markdown notes written by the language model. */
  summary: string | null;
  summaryModel: string | null;
  language: string | null;
  /** Why the notes are missing, or what went wrong. */
  error: string | null;
  systemAudio: boolean;
  stoppedByLimit: boolean;
  hasAudio: boolean;
}

export interface MeetingItem {
  id: string;
  title: string;
  startedAt: string;
  durationMs: number;
  status: MeetingStatus;
  hasSummary: boolean;
  preview: string;
}

export interface ActiveMeeting {
  id: string;
  startedAt: string;
  /** When the time limit stops it, if there is one. */
  stopsAt: string | null;
}

/** understand: something vague; challenge: a claim to test; follow-up: something left open. */
export type QuestionKind = 'understand' | 'challenge' | 'follow-up';
export type QuestionState = 'live' | 'later' | 'asked' | 'answered' | 'expired' | 'deleted';
/** What the user does to an open question. */
export type QuestionAct = 'later' | 'asked' | 'deleted';
/** What the live questions are doing: off, no summary model, loading it, waiting for talk, reading, failed. */
export type QuestionStatus = 'off' | 'no-model' | 'starting' | 'listening' | 'thinking' | 'failed';

export interface MeetingQuestion {
  /** 1, 2, … in the order proposed. */
  number: number;
  text: string;
  kind: QuestionKind;
  state: QuestionState;
  /** Meeting time it was proposed at, and of its last change. */
  atMs: number;
  changedMs: number;
}

export interface MeetingQuestions {
  questions: MeetingQuestion[];
  status: QuestionStatus;
  /** The meeting's goal, which the questions steer towards. */
  goal: string;
}

/** What a box on the system map is. */
export type MapNodeKind = 'ours' | 'customer' | 'outside' | 'person';

export interface MapNode {
  id: number;
  name: string;
  kind: MapNodeKind;
  /** Facts said in the meeting. */
  known: string[];
  /** Meeting time it was added at. */
  atMs: number;
}

/** Something flows from one box to another: what, and how (API, CSV, mail, by hand, …). */
export interface MapEdge {
  from: number;
  to: number;
  what: string;
  how: string;
}

/** Something about a box that is still unknown. */
export interface MapOpenPoint {
  number: number;
  node: number;
  text: string;
}

export interface SystemMap {
  nodes: MapNode[];
  edges: MapEdge[];
  open: MapOpenPoint[];
}

/** The map and what the live model (shared with the questions) is doing. */
export interface MeetingMap {
  map: SystemMap;
  status: QuestionStatus;
}

export type MeetingPhase = 'transcribing' | 'speakers' | 'loading' | 'reading' | 'writing';

export interface MeetingProgress {
  id: string;
  phase: MeetingPhase;
  /** 0..1 while reading the transcript. */
  fraction: number | null;
  /** The notes so far while writing. */
  text: string | null;
}

export interface SummaryModelInfo {
  id: string;
  displayName: string;
  sizeMb: number;
  downloaded: boolean;
  badge: string;
  description: string;
  recommended: boolean;
}

export interface HistoryRecord {
  id: string;
  text: string;
  raw_text: string | null;
  model: string | null;
  word_count: number;
  duration_seconds: number;
  transcription_ms: number | null;
  /** Language the dictation was recognised as (ISO 639-1). Omitted for older records. */
  language?: string | null;
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
  /** The meeting being recorded, if any. */
  meeting: ActiveMeeting | null;
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
  'meeting-state': { active: ActiveMeeting | null };
  'meeting-segments': { id: string; segments: MeetingSegment[]; speakers: MeetingSpeaker[] };
  'meeting-changed': { id: string };
  'meeting-progress': MeetingProgress;
  'meeting-questions': { id: string } & MeetingQuestions;
  'meeting-map': { id: string } & MeetingMap;
  'meeting-levels': { mic?: number; system?: number };
  'meetings-changed': null;
  'summary-models-changed': null;
}

export type EventName = keyof EventMap;
