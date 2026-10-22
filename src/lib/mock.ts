// In-memory mock of the Rust core, used when the UI runs in a plain browser
// (`npm run dev`). It follows docs/ipc.md and simulates the recording,
// download and permission flows so every screen can be exercised.
//
// URL flags (dev only): ?perm=missing  ?mic=denied  ?fn=bad  ?onboarding=1  ?model=loading  ?ptt=unavailable  ?empty=1
import { PRODUCT } from './brand';
import type {
  AudioDevice,
  BenchInfo,
  DictEntry,
  EventMap,
  EventName,
  HistoryRecord,
  LabPrompt,
  LabStopResult,
  ModelInfo,
  Permissions,
  Settings,
  Snapshot,
  Stats,
  Suggestion,
} from './types';

type Handler = (payload: never) => void;
const listeners = new Map<string, Set<Handler>>();

function emit<E extends EventName>(event: E, payload: EventMap[E]) {
  // Deliver asynchronously, like real IPC events.
  queueMicrotask(() => {
    for (const h of listeners.get(event) ?? []) (h as (p: EventMap[E]) => void)(payload);
  });
}

export function mockListen<E extends EventName>(event: E, handler: (p: EventMap[E]) => void) {
  let set = listeners.get(event);
  if (!set) listeners.set(event, (set = new Set()));
  set.add(handler as Handler);
  return () => set.delete(handler as Handler);
}

const params = new URLSearchParams(typeof location !== 'undefined' ? location.search : '');
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const uid = () => Math.random().toString(36).slice(2, 10);
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v));

// ---------------------------------------------------------------- data

const empty = params.get('empty') === '1';

const settings: Settings = {
  model: 'distil-large-v3.5',
  language: null,
  removeFillers: true,
  pttKey: 'fn',
  toggleShortcut: 'Alt+Space',
  showWidgetShortcut: 'CommandOrControl+Shift+Space',
  inputDevice: null,
  inputGain: 1,
  soundsEnabled: true,
  muteSystemAudio: false,
  copyOnly: false,
  launchAtLogin: true,
  widgetVisibility: 'always',
  pillStyle: 'logo-and-text',
  followCursor: true,
  widgetLocked: false,
  dictionaryEnabled: true,
  onboardingDone: params.get('onboarding') !== '1',
  modelFolders: ['/Users/you/Models/whisper'],
  keepAudio: true,
  promptMode: 'dictionary',
  decode: { beamSize: 5, temperature: 0, noSpeechThreshold: 0.6 },
};

const missing = params.get('perm') === 'missing';
const permissions: Permissions = {
  microphone: params.get('mic') === 'denied' ? 'denied' : 'granted',
  accessibility: !missing,
  inputMonitoring: !missing,
};

let fnUsage = params.get('fn') === 'bad'
  ? { value: 2, label: 'the emoji picker', ok: false }
  : { value: 0, label: 'Do Nothing', ok: true };

let modelState: Snapshot['modelState'] = params.get('model') === 'loading' ? 'loading' : 'ready';
let recState: Snapshot['recState'] = 'idle';
let recMode: Snapshot['recMode'] = null;

const APP_MODELS = `/Users/you/Library/Application Support/${PRODUCT}/models`;

const models: ModelInfo[] = [
  {
    id: 'base.en', displayName: 'Base', sizeMb: 142, downloaded: false, path: null,
    source: null, badge: 'English only', englishOnly: true, recommended: false,
    description: 'Very small and quick. Good enough for short notes on an older Mac.',
  },
  {
    id: 'distil-large-v3.5', displayName: 'Distil Large v3.5', sizeMb: 1520, downloaded: true,
    path: `${APP_MODELS}/ggml-distil-large-v3.5.bin`, source: 'app',
    badge: 'English only', englishOnly: true, recommended: true,
    description: 'Nearly as accurate as Large in English, at half the size.',
  },
  {
    id: 'large-v3-turbo', displayName: 'Large v3 Turbo', sizeMb: 1620, downloaded: true,
    path: '/Users/you/Models/whisper/ggml-large-v3-turbo.bin', source: 'folder',
    badge: 'Multilingual', englishOnly: false, recommended: false,
    description: 'Fast and accurate in around a hundred languages.',
  },
  {
    id: 'large-v3', displayName: 'Large v3', sizeMb: 3100, downloaded: false, path: null,
    source: null, badge: 'Multilingual', englishOnly: false, recommended: false,
    description: 'The most accurate option, and the slowest.',
  },
];

const SAMPLE_TEXTS = [
  "Let's move the retry logic into its own module so the upload worker and the sync worker can share it, and add a test for the backoff.",
  'Thanks, that works.',
  'Can you check why the build takes four minutes now? It was under ninety seconds last week, so something in the dependency cache must have changed.',
  'Rename the setting to keep recordings and make the help text say how many are stored.',
  'Ship it.',
  'Write a short summary of the three options we discussed for search: a local index, a hosted service, or plain SQL with trigram matching. Keep it under a page and end with a recommendation.',
  'The date picker jumps to January when you clear the field. Reproduce it first, then fix it without touching the calendar styles.',
  'Remind me to update the changelog before the release on Friday.',
  'Okay, plan for today. Fix the flaky login test, review the two open pull requests, and if there is time, start on the CSV export.',
  "Use a debounce of about two hundred milliseconds on the search input so we don't hit the server on every keystroke.",
  'Revert the last commit.',
  "Draft a reply saying the meeting works for me, but I'd like to move it to the afternoon so I can finish the review first.",
  'Explain what this regular expression does, step by step, and suggest a version that is easier to read.',
  "Let's keep the API as it is for now and add a version header, so older clients keep working while we migrate.",
];

function words(text: string) {
  return text.trim().split(/\s+/).filter(Boolean).length;
}

function makeHistory(): HistoryRecord[] {
  const now = Date.now();
  // Minutes ago, clamped so "today" entries never cross midnight.
  const sinceMidnight = Math.floor((now - new Date(new Date(now).setHours(0, 0, 0, 0)).getTime()) / 60_000);
  const today = (m: number) => Math.min(m, Math.max(1, sinceMidnight - 5));
  const day = 60 * 24;
  const offsetsMin = [
    today(4), today(26), today(95), today(140), sinceMidnight + 60 * 6, sinceMidnight + 60 * 7,
    sinceMidnight + 60 * 9, day * 2 + 30, day * 2 + 75, day * 2 + 190, day * 4 + 20, day * 4 + 21,
    day * 6, day * 9,
  ];
  const out: HistoryRecord[] = [];
  // Expand to a realistic count so paging can be exercised (~140 records).
  for (let i = 0; i < (empty ? 0 : 140); i++) {
    const text = SAMPLE_TEXTS[i % SAMPLE_TEXTS.length];
    const base = offsetsMin[i] ?? offsetsMin[offsetsMin.length - 1] + (i - offsetsMin.length) * 180;
    const wc = words(text);
    out.push({
      id: `h${i}`,
      text,
      raw_text: text,
      model: 'distil-large-v3.5',
      word_count: wc,
      duration_seconds: Math.max(1, Math.round(wc / 2.2)),
      transcription_ms: 300 + ((i * 97) % 900),
      timestamp: new Date(now - base * 60_000).toISOString(),
      // Audio is kept for the newest dictations; a few of those are already verified.
      has_audio: i < 48,
      reference: i < 48 && i % 6 === 1 ? text : undefined,
    });
  }
  return out;
}

let history = makeHistory();

const stats: Stats = empty
  ? { totalWords: 0, totalSeconds: 0, totalSessions: 0, firstUsed: null, lastUsed: null, streak: 0, wpm: 0 }
  : {
      totalWords: 24033,
      totalSeconds: 11712,
      totalSessions: 653,
      firstUsed: '2026-06-02T09:12:00Z',
      lastUsed: new Date().toISOString(),
      streak: 6,
      wpm: 123,
    };

let dictionary: DictEntry[] = empty ? [] : [
  { id: 'd1', original: 'post gress', replacement: 'PostgreSQL', createdAt: '2026-09-02T10:00:00Z' },
  { id: 'd2', original: 'svelt', replacement: 'Svelte', createdAt: '2026-09-04T10:00:00Z' },
  { id: 'd3', original: 'type script', replacement: 'TypeScript', createdAt: '2026-09-10T10:00:00Z' },
  { id: 'd4', original: 'whisper cpp', replacement: 'whisper.cpp', createdAt: '2026-10-01T10:00:00Z' },
  { id: 'd5', original: 'next JS', replacement: 'Next.js', createdAt: '2026-10-03T10:00:00Z' },
];

let suggestions: Suggestion[] = empty ? [] : [
  { id: 's1', detectedWord: 'graph QL', suggestedReplacement: 'GraphQL', reason: 'whisperKnownFailure', confidence: 0.95, occurrenceCount: 4 },
  { id: 's2', detectedWord: 'cube control', suggestedReplacement: 'kubectl', reason: 'whisperKnownFailure', confidence: 0.9, occurrenceCount: 3 },
  { id: 's3', detectedWord: 'sequel light', suggestedReplacement: 'SQLite', reason: 'whisperKnownFailure', confidence: 0.85, occurrenceCount: 2 },
  { id: 's4', detectedWord: 'standup', suggestedReplacement: 'stand-up', reason: 'frequentUnknown', confidence: 0.5, occurrenceCount: 9 },
];

// Hint-only words and project folder names (vocabulary hints).
let vocabulary: string[] = empty ? [] : ['Kubernetes', 'WebSocket', 'OAuth'];
const projects = ['notes-app', 'design-tokens', 'sync-worker', 'photo-tagger', 'invoice-cli'];

const BUILTIN_PROMPTS: [string, LabPrompt['category'], string, string[]?][] = [
  ['cmd-1', 'short', 'Go ahead.'],
  ['cmd-2', 'short', 'Undo that.'],
  ['cmd-3', 'short', 'Run the tests again.'],
  ['cmd-4', 'short', 'Looks good, merge it.'],
  ['cmd-5', 'short', 'Thanks, that works.'],
  ['cmd-6', 'short', 'Open a new branch.'],
  ['cmd-7', 'short', 'What does this function return?'],
  ['cmd-8', 'short', 'Keep going.'],
  ['tech-1', 'jargon', 'Move the sessions table to PostgreSQL and keep the row level security policies.', ['PostgreSQL']],
  ['tech-2', 'jargon', 'Expose the search endpoint over GraphQL and cache the results in Redis.', ['GraphQL', 'Redis']],
  ['tech-3', 'jargon', 'Rewrite the settings page in Svelte with Tailwind and keep the TypeScript types.', ['Svelte', 'Tailwind', 'TypeScript']],
  ['tech-4', 'jargon', 'Build a CLI that exports the API keys as JSON.', ['CLI', 'API', 'JSON']],
  ['tech-5', 'jargon', 'Deploy the worker to Kubernetes and check the pods with kubectl.', ['Kubernetes', 'kubectl']],
  ['tech-6', 'jargon', 'The CI pipeline fails because the end to end tests time out.', ['CI']],
  ['tech-7', 'jargon', 'Store the user settings in SQLite and sync them over a WebSocket.', ['SQLite', 'WebSocket']],
  ['tech-8', 'jargon', 'Add OAuth login and keep the refresh token in the keychain on macOS.', ['OAuth', 'macOS']],
  ['tech-9', 'jargon', 'Compile whisper.cpp with Metal and compare it against the CPU build.', ['whisper.cpp', 'Metal', 'CPU']],
  ['tech-10', 'jargon', 'Store the IDs in local storage and render the list in the UI.', ['IDs', 'UI']],
  ['num-1', 'numbers', 'Set the timeout to 30 seconds and retry 3 times.'],
  ['num-2', 'numbers', 'The review is on March 4th at 2:30 PM.'],
  ['num-3', 'numbers', 'We have 250 users, and about 12 percent are on the free plan.'],
  ['prose-1', 'prose', 'Find out why the setup screen skips the permission step for some new users, then write a short note about what you found.'],
  ['prose-2', 'prose', "Please keep the change small, leave unrelated files alone, and check the result in a browser before calling it done."],
  ['prose-3', 'prose', 'When a long job stops halfway, it should pick up from the last finished step instead of starting over from the beginning.'],
  ['prose-4', 'prose', 'Before writing any code, look around for existing solutions, since someone has probably solved this before.'],
  ['long-1', 'long', "Here is the plan for today. First, fix the login bug, because people get locked out after a password reset. Then look at the database migration and make sure the access rules still work for the admin role. After that, review the pipeline, since the browser tests have been flaky all week. If there is time left, start on the export, but do not ship anything without running the full test suite first."],
  ['long-2', 'long', 'The idea is that the heavy work runs on a bigger machine. We hand the job over, and it reports back when it is finished. There should be three speeds: a quick one, a balanced one, and a careful one that is slower but more thorough. All three write down what they learned, so the next run can start from there.'],
];
const PROJECT_TEMPLATES = ['Open the {term} repo and fix the failing tests.', 'Deploy {term} to staging after lunch.'];
const WORD_TEMPLATE = 'Please double check the {term} part before we ship.';
const MAX_PERSONAL = 8;

/** Typical Whisper misses, used to make mock lab feedback plausible. */
const MISHEARD: Record<string, string> = {
  PostgreSQL: 'post gress QL', GraphQL: 'graph QL', Redis: 'red is', Svelte: 'svelt', Tailwind: 'tail wind',
  TypeScript: 'type script', Kubernetes: 'cooper netties', kubectl: 'cube control', SQLite: 'sequel light',
  WebSocket: 'web socket', OAuth: 'oh auth', 'whisper.cpp': 'whisper CPP', Metal: 'metal', IDs: 'ideas',
  'notes-app': 'notes app', 'design-tokens': 'design tokens', 'sync-worker': 'sink worker',
  'photo-tagger': 'photo tagger', 'invoice-cli': 'invoice CLI',
};

const slug = (s: string) => s.toLowerCase().replace(/[^a-z0-9]/g, '-');

/** Last feedback per recorded prompt (the clip "on disk"). */
const labClips = new Map<string, { hypothesis: string; wer: number }>(empty ? [] : [
  ['cmd-1', { hypothesis: 'Go ahead.', wer: 0 }],
  ['cmd-2', { hypothesis: 'Undo that.', wer: 0 }],
  ['cmd-3', { hypothesis: 'Run the test again.', wer: 0.25 }],
  ['cmd-5', { hypothesis: 'Thanks, that works.', wer: 0 }],
  ['tech-1', { hypothesis: 'Move the sessions table to post gress QL and keep the row level security policies.', wer: 0.154 }],
  ['tech-4', { hypothesis: 'Build a CLI that exports the API keys as JSON.', wer: 0 }],
  ['tech-5', { hypothesis: 'Deploy the worker to Kubernetes and check the pods with cube control.', wer: 0.167 }],
  ['num-1', { hypothesis: 'Set the timeout to thirty seconds and retry three times.', wer: 0 }],
  ['prose-2', { hypothesis: 'Please keep the change small, leave unrelated files alone, and check the result in a browser before calling it done.', wer: 0 }],
  ['project-notes-app', { hypothesis: 'Open the notes app repo and fix the failing tests.', wer: 0.1 }],
]);

function labPrompts(): LabPrompt[] {
  const list: Omit<LabPrompt, 'recorded' | 'hypothesis' | 'wer'>[] = BUILTIN_PROMPTS.map(
    ([id, category, text, terms]) => ({ id, category, text, terms: terms ?? [] }),
  );
  projects.slice(0, MAX_PERSONAL).forEach((p, i) =>
    list.push({ id: `project-${slug(p)}`, category: 'personal', text: PROJECT_TEMPLATES[i % 2].replace('{term}', p), terms: [p] }),
  );
  [...dictionary.map((d) => d.replacement), ...vocabulary].slice(0, MAX_PERSONAL).forEach((w) =>
    list.push({ id: `word-${slug(w)}`, category: 'personal', text: WORD_TEMPLATE.replace('{term}', w), terms: [w] }),
  );
  return list.map((p) => {
    const clip = labClips.get(p.id);
    return { ...p, recorded: !!clip, hypothesis: clip?.hypothesis ?? null, wer: clip?.wer ?? null };
  });
}

// Port of the Rust metrics normalisation (subset) so mock WER numbers are realistic.
const NUMBER_WORDS: Record<string, string> = {
  one: '1', two: '2', three: '3', four: '4', five: '5', ten: '10', twelve: '12', thirty: '30', first: '1st', third: '3rd',
};
function normalizeWords(text: string): string[] {
  return text
    .toLowerCase()
    .replace(/[-.:/_]/g, ' ')
    .replace(/[^\p{L}\p{N}'\s]/gu, ' ')
    .split(/\s+/)
    .map((t) => t.replace(/^'+|'+$/g, ''))
    .filter(Boolean)
    .map((t) => NUMBER_WORDS[t] ?? t);
}
function wordErrorRate(reference: string, hypothesis: string): number {
  const r = normalizeWords(reference);
  const h = normalizeWords(hypothesis);
  let prev = Array.from({ length: h.length + 1 }, (_, j) => j);
  for (let i = 0; i < r.length; i++) {
    const cur = [i + 1];
    for (let j = 0; j < h.length; j++)
      cur[j + 1] = r[i] === h[j] ? prev[j] : 1 + Math.min(prev[j], prev[j + 1], cur[j]);
    prev = cur;
  }
  return r.length === 0 ? (h.length ? 1 : 0) : prev[h.length] / r.length;
}

/** Mishear some terms and, occasionally, a filler word, like a real model would. */
function mockHypothesis(p: LabPrompt): string {
  let text = p.text;
  for (const t of p.terms) if (MISHEARD[t] && Math.random() < 0.45) text = text.replace(t, MISHEARD[t]);
  if (Math.random() < 0.3) text = text.replace(/\b(the|a|and|to) /i, '');
  if (Math.random() < 0.15) text = text.replace(/\bthree\b|\b3\b/, 'free');
  return text;
}

const BENCH_ROOT = `/Users/you/Library/Application Support/${PRODUCT}/bench`;
let latestReport: string | null = empty ? null : `${BENCH_ROOT}/results/2026-10-19T15-32-08/report.html`;

function benchInfo(): BenchInfo {
  const withAudio = history.filter((r) => r.has_audio);
  return {
    root: BENCH_ROOT,
    tiers: { lab: labClips.size, noise: 6, smoke: 48 },
    historyWithAudio: withAudio.length,
    historyVerified: withAudio.filter((r) => r.reference != null).length,
    latestReport,
  };
}

let lab: { promptId: string; startedAt: number; timer: ReturnType<typeof setInterval> } | null = null;

function labStart(promptId: string) {
  if (recState !== 'idle') throw new Error('Finish the current dictation first.');
  if (permissions.microphone !== 'granted') throw new Error('Microphone access is needed.');
  if (lab) clearInterval(lab.timer);
  let t = 0;
  const timer = setInterval(() => {
    t += 1 / 30;
    const env = Math.max(0, Math.sin(t * 6.1) * 0.45 + Math.sin(t * 1.7) * 0.35 + 0.3);
    const levels = Array.from({ length: 7 }, (_, i) =>
      Math.min(1, env * (0.45 + Math.random() * 0.7) * (0.7 + 0.3 * Math.sin(t * 4 + i))),
    );
    emit('lab-levels', { levels });
  }, 1000 / 30);
  lab = { promptId, startedAt: Date.now(), timer };
}

function labEnd() {
  const cur = lab;
  if (cur) clearInterval(cur.timer);
  lab = null;
  return cur;
}

async function labStop(): Promise<LabStopResult> {
  const cur = labEnd();
  if (!cur) throw new Error('Not recording.');
  const seconds = (Date.now() - cur.startedAt) / 1000;
  if (seconds < 0.4) throw new Error('That was too short. Hold the button while you read.');
  await sleep(700);
  const prompt = labPrompts().find((p) => p.id === cur.promptId);
  if (!prompt) throw new Error('Unknown prompt.');
  const hypothesis = mockHypothesis(prompt);
  const wer = wordErrorRate(prompt.text, hypothesis);
  labClips.set(prompt.id, { hypothesis, wer });
  return { promptId: prompt.id, hypothesis, wer, ms: 380 + Math.round(Math.random() * 500), seconds };
}

const devices: AudioDevice[] = [
  { name: 'MacBook Pro Microphone', isDefault: true },
  { name: 'USB Microphone', isDefault: false },
  { name: 'Bluetooth Headset', isDefault: false },
];

// ---------------------------------------------------------------- recording simulation

let levelTimer: ReturnType<typeof setInterval> | null = null;
let widgetSize: { width: number; height: number } | null = null;
let recStartedAt = 0;

function setRec(state: Snapshot['recState'], mode: Snapshot['recMode']) {
  recState = state;
  recMode = mode;
  emit('recording-state', { state, mode });
}

function startLevels() {
  let t = 0;
  levelTimer = setInterval(() => {
    t += 1 / 30;
    // Syllable-like envelope with per-band jitter.
    const env = Math.max(0, Math.sin(t * 7.3) * 0.5 + Math.sin(t * 2.1) * 0.35 + 0.25);
    const levels = Array.from({ length: 7 }, (_, i) =>
      Math.min(1, env * (0.45 + Math.random() * 0.75) * (0.7 + 0.3 * Math.sin(t * 5 + i))),
    );
    emit('audio-levels', { levels });
  }, 1000 / 30);
}

function stopLevels() {
  if (levelTimer) clearInterval(levelTimer);
  levelTimer = null;
  emit('audio-levels', { levels: Array(7).fill(0) });
}

function start(mode: Snapshot['recMode']) {
  if (recState !== 'idle') return;
  recStartedAt = Date.now();
  setRec('listening', mode);
  startLevels();
}

function stop() {
  if (recState !== 'listening') return;
  stopLevels();
  const secs = (Date.now() - recStartedAt) / 1000;
  setRec('processing', recMode);
  setTimeout(() => {
    setRec('idle', null);
    if (secs < 0.6) {
      emit('notice', { kind: 'warning', title: 'That was too short to hear', message: 'Keep holding while you talk.' });
      return;
    }
    const text = SAMPLE_TEXTS[Math.floor(Math.random() * SAMPLE_TEXTS.length)];
    const record: HistoryRecord = {
      id: uid(), text, raw_text: text, model: settings.model, word_count: words(text),
      duration_seconds: Math.max(1, Math.round(secs)), transcription_ms: 640,
      timestamp: new Date().toISOString(),
      has_audio: settings.keepAudio,
    };
    history = [record, ...history];
    stats.totalWords += record.word_count;
    stats.totalSeconds += record.duration_seconds;
    stats.totalSessions += 1;
    stats.lastUsed = record.timestamp;
    emit('transcription-complete', { record });
    emit('history-changed', null);
  }, 800);
}

function cancel() {
  if (recState === 'idle') return;
  stopLevels();
  setRec('idle', null);
}

// ---------------------------------------------------------------- command router

function snapshot(): Snapshot {
  return clone({
    settings, stats, recState, recMode, modelState,
    modelError: null, activeModel: modelState === 'ready' ? settings.model : null,
    permissions, fnUsage,
    pttListening: params.get('ptt') !== 'unavailable',
    version: '0.1.0',
  });
}

function updateSettings(patch: Partial<Settings>) {
  Object.assign(settings, patch);
  emit('settings-changed', clone(settings));
  return clone(settings);
}

async function download(id: string) {
  const m = models.find((x) => x.id === id);
  if (!m || m.downloaded) return;
  const total = m.sizeMb * 1_000_000;
  const steps = 40;
  for (let i = 1; i <= steps; i++) {
    await sleep(90);
    emit('model-download-progress', { id, downloaded: Math.round((total * i) / steps), total });
  }
  m.downloaded = true;
  m.source = 'app';
  m.path = `${APP_MODELS}/ggml-${id}.bin`;
  emit('models-changed', null);
  emit('notice', { kind: 'success', title: `${m.displayName} is downloaded`, message: '' });
}

async function setModel(id: string) {
  updateSettings({ model: id });
  modelState = 'loading';
  emit('model-state', { state: 'loading', model: id, error: null });
  await sleep(900);
  modelState = 'ready';
  emit('model-state', { state: 'ready', model: id, error: null });
}

type Args = Record<string, any>;

export async function mockInvoke(cmd: string, a: Args): Promise<unknown> {
  await sleep(15);
  switch (cmd) {
    case 'get_snapshot': return snapshot();
    case 'update_settings': return updateSettings(a.patch ?? {});

    case 'get_history': {
      const q = (a.query ?? '').toLowerCase().trim();
      const all = q ? history.filter((r) => r.text.toLowerCase().includes(q)) : history;
      return { total: all.length, records: clone(all.slice(a.offset, a.offset + a.limit)) };
    }
    case 'delete_history':
      history = history.filter((r) => r.id !== a.id);
      emit('history-changed', null);
      return;
    case 'clear_history':
      history = [];
      emit('history-changed', null);
      return;
    case 'export_history_csv': return `/Users/you/Downloads/${PRODUCT.toLowerCase()}-transcripts.csv`;
    case 'correct_history': {
      const text = String(a.text ?? '').trim();
      if (!text) throw new Error("The corrected text can't be empty.");
      const r = history.find((x) => x.id === a.id);
      if (!r) throw new Error('Transcript not found.');
      Object.assign(r, { text, reference: text, word_count: words(text) });
      emit('history-changed', null);
      return;
    }
    case 'verify_history': {
      const r = history.find((x) => x.id === a.id);
      if (!r) throw new Error('Transcript not found.');
      r.reference = r.text;
      emit('history-changed', null);
      return;
    }
    case 'get_stats': return clone(stats);

    case 'list_dictionary': return clone(dictionary);
    case 'upsert_dictionary': {
      let entry = dictionary.find((d) => d.id === a.id);
      if (entry) Object.assign(entry, { original: a.original, replacement: a.replacement });
      else {
        entry = { id: uid(), original: a.original, replacement: a.replacement, createdAt: new Date().toISOString() };
        dictionary = [entry, ...dictionary];
      }
      emit('dictionary-changed', null);
      return clone(entry);
    }
    case 'delete_dictionary':
      dictionary = dictionary.filter((d) => d.id !== a.id);
      emit('dictionary-changed', null);
      return;
    case 'list_suggestions': return clone(suggestions);
    case 'accept_suggestion': {
      const s = suggestions.find((x) => x.id === a.id);
      if (s) {
        dictionary = [{ id: uid(), original: s.detectedWord, replacement: a.replacement ?? s.suggestedReplacement, createdAt: new Date().toISOString() }, ...dictionary];
        suggestions = suggestions.filter((x) => x.id !== a.id);
      }
      emit('dictionary-changed', null);
      return;
    }
    case 'dismiss_suggestion':
      suggestions = suggestions.filter((x) => x.id !== a.id);
      emit('dictionary-changed', null);
      return;
    case 'dismiss_all_suggestions':
      suggestions = [];
      emit('dictionary-changed', null);
      return;

    case 'list_vocabulary': return clone(vocabulary);
    case 'set_vocabulary': {
      const seen = new Set<string>();
      vocabulary = (a.terms as string[])
        .map((t) => t.trim())
        .filter((t) => t && !seen.has(t.toLowerCase()) && seen.add(t.toLowerCase()));
      emit('dictionary-changed', null);
      return clone(vocabulary);
    }
    case 'suggested_vocabulary':
      return projects.filter((p) => !vocabulary.some((v) => v.toLowerCase() === p.toLowerCase()));

    case 'lab_prompts': return labPrompts();
    case 'lab_start': labStart(a.promptId); return;
    case 'lab_stop': return labStop();
    case 'lab_cancel': labEnd(); return;
    case 'lab_delete': labClips.delete(a.promptId); return;
    case 'bench_info': return benchInfo();
    case 'open_path':
      if (!String(a.path).startsWith(BENCH_ROOT)) throw new Error('Only files in the app folder can be opened.');
      return;

    case 'list_models': return clone(models);
    case 'download_model': void download(a.id); return;
    case 'delete_model': {
      const m = models.find((x) => x.id === a.id);
      if (m) Object.assign(m, { downloaded: false, path: null, source: null });
      emit('models-changed', null);
      return;
    }
    case 'set_model': void setModel(a.id); return;

    case 'start_recording': start('click'); return;
    case 'stop_recording': stop(); return;
    case 'cancel_recording': cancel(); return;
    case 'toggle_recording': recState === 'idle' ? start('toggle') : stop(); return;

    case 'list_input_devices': return clone(devices);
    case 'check_permissions': return clone(permissions);
    case 'request_permission':
      setTimeout(() => {
        if (a.kind === 'microphone') permissions.microphone = 'granted';
        else permissions[a.kind as 'accessibility' | 'inputMonitoring'] = true;
        emit('permissions-changed', clone(permissions));
      }, 1200);
      return;
    case 'set_fn_do_nothing':
      fnUsage = { value: 0, label: 'Do Nothing', ok: true };
      return clone(fnUsage);

    case 'copy_text':
      try { await navigator.clipboard.writeText(a.text); } catch { /* headless / no focus */ }
      return;
    case 'show_dashboard': return;
    case 'resize_widget': widgetSize = { width: a.width, height: a.height }; return;
    case 'import_data': {
      await sleep(600);
      const modelFolder = `${a.path}/models`;
      if (!settings.modelFolders.includes(modelFolder)) updateSettings({ modelFolders: [...settings.modelFolders, modelFolder] });
      emit('history-changed', null);
      emit('dictionary-changed', null);
      emit('models-changed', null);
      return { history: 214, dictionary: 9, suggestions: 6, modelFolder };
    }
  }
  throw new Error(`mock: unknown command ${cmd}`);
}

/** Browser stand-in for the native folder picker. */
export async function mockPickFolder(): Promise<string | null> {
  await sleep(150);
  return '/Users/you/Documents/dictation-backup';
}

// Handy for driving the UI from devtools / Playwright.
if (typeof window !== 'undefined') {
  (window as unknown as Record<string, unknown>).__mock = {
    emit,
    settings,
    permissions,
    labClips,
    clearLatestReport: () => (latestReport = null),
    get widgetSize() {
      return widgetSize;
    },
  };
}
