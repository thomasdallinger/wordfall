// Typed wrapper around the Tauri IPC surface described in docs/ipc.md.
// Falls back to an in-memory mock backend when running in a plain browser.
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import type {
  AudioDevice,
  BenchInfo,
  DictEntry,
  EventMap,
  EventName,
  FnUsage,
  HistoryPage,
  ImportResult,
  LabPrompt,
  LabStopResult,
  ModelInfo,
  PermissionKind,
  Permissions,
  Settings,
  Snapshot,
  Stats,
  Suggestion,
} from './types';

export const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

export type Unlisten = () => void;

type MockModule = typeof import('./mock');
let mockPromise: Promise<MockModule> | null = null;
/** The mock backend is loaded on demand so it never ships in the Tauri bundle's startup path. */
function mock(): Promise<MockModule> {
  mockPromise ??= import('./mock');
  return mockPromise;
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri) return invoke<T>(cmd, args);
  const m = await mock();
  return m.mockInvoke(cmd, args ?? {}) as Promise<T>;
}

/** Subscribe to a backend event. Resolves to an unlisten function. */
export async function on<E extends EventName>(
  event: E,
  handler: (payload: EventMap[E]) => void,
): Promise<Unlisten> {
  if (isTauri) return listen<EventMap[E]>(event, (e) => handler(e.payload));
  const m = await mock();
  return m.mockListen(event, handler);
}

/** Which window this webview belongs to: `dashboard` or `widget`. */
export async function windowLabel(): Promise<string> {
  if (isTauri) return getCurrentWindow().label;
  return new URLSearchParams(location.search).get('window') ?? 'dashboard';
}

/** Begin a native window drag (widget pill). No-op in the browser. */
export async function startWindowDrag(): Promise<void> {
  if (!isTauri) return;
  await getCurrentWindow().startDragging();
}

/** Ask the user for a folder. Resolves null when they cancel. The browser mock picks a sample path. */
export async function pickFolder(title: string): Promise<string | null> {
  if (!isTauri) return (await mock()).mockPickFolder();
  const picked = await openDialog({ directory: true, multiple: false, title });
  return typeof picked === 'string' ? picked : null;
}

export const api = {
  getSnapshot: () => call<Snapshot>('get_snapshot'),
  updateSettings: (patch: Partial<Settings>) => call<Settings>('update_settings', { patch }),

  getHistory: (offset: number, limit: number, query?: string) =>
    call<HistoryPage>('get_history', { query: query || undefined, offset, limit }),
  deleteHistory: (id: string) => call<void>('delete_history', { id }),
  clearHistory: () => call<void>('clear_history'),
  exportHistoryCsv: () => call<string>('export_history_csv'),
  getStats: () => call<Stats>('get_stats'),
  /** Replace a transcript with the user's correction; it becomes benchmark ground truth. */
  correctHistory: (id: string, text: string) => call<void>('correct_history', { id, text }),
  /** Mark a transcript as exactly right (ground truth = current text). */
  verifyHistory: (id: string) => call<void>('verify_history', { id }),

  listDictionary: () => call<DictEntry[]>('list_dictionary'),
  upsertDictionary: (original: string, replacement: string, id?: string) =>
    call<DictEntry>('upsert_dictionary', { id, original, replacement }),
  deleteDictionary: (id: string) => call<void>('delete_dictionary', { id }),
  listSuggestions: () => call<Suggestion[]>('list_suggestions'),
  acceptSuggestion: (id: string, replacement?: string) =>
    call<void>('accept_suggestion', { id, replacement }),
  dismissSuggestion: (id: string) => call<void>('dismiss_suggestion', { id }),
  dismissAllSuggestions: () => call<void>('dismiss_all_suggestions'),
  /** Hint-only words (bias recognition, never replaced). */
  listVocabulary: () => call<string[]>('list_vocabulary'),
  setVocabulary: (terms: string[]) => call<string[]>('set_vocabulary', { terms }),
  /** Project folder names not yet in the vocabulary. */
  suggestedVocabulary: () => call<string[]>('suggested_vocabulary'),

  labPrompts: () => call<LabPrompt[]>('lab_prompts'),
  /** Starts a Voice Lab clip; levels arrive via `lab-levels`. */
  labStart: (promptId: string) => call<void>('lab_start', { promptId }),
  /** Saves the clip and transcribes it for feedback (may take ~1 s). */
  labStop: () => call<LabStopResult>('lab_stop'),
  labCancel: () => call<void>('lab_cancel'),
  labDelete: (promptId: string) => call<void>('lab_delete', { promptId }),
  benchInfo: () => call<BenchInfo>('bench_info'),
  /** Opens a file or folder inside the app's data folder. */
  openPath: (path: string) => call<void>('open_path', { path }),

  listModels: () => call<ModelInfo[]>('list_models'),
  downloadModel: (id: string) => call<void>('download_model', { id }),
  deleteModel: (id: string) => call<void>('delete_model', { id }),
  setModel: (id: string) => call<void>('set_model', { id }),

  startRecording: () => call<void>('start_recording'),
  stopRecording: () => call<void>('stop_recording'),
  cancelRecording: () => call<void>('cancel_recording'),
  toggleRecording: () => call<void>('toggle_recording'),

  listInputDevices: () => call<AudioDevice[]>('list_input_devices'),
  checkPermissions: () => call<Permissions>('check_permissions'),
  requestPermission: (kind: PermissionKind) => call<void>('request_permission', { kind }),
  setFnDoNothing: () => call<FnUsage>('set_fn_do_nothing'),

  copyText: (text: string) => call<void>('copy_text', { text }),
  showDashboard: () => call<void>('show_dashboard'),
  /** Imports transcripts, dictionary and suggestions found in a folder; adds it as a model folder if it holds models. */
  importData: (path: string) => call<ImportResult>('import_data', { path }),
  /** Fit the widget window to its content (logical px); Rust keeps it anchored bottom-center. */
  resizeWidget: (width: number, height: number) => call<void>('resize_widget', { width, height }),
};
