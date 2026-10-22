import type { PttKey } from './types';

const timeFmt = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });
const weekdayFmt = new Intl.DateTimeFormat('en-GB', { weekday: 'long' });
const dayMonthFmt = new Intl.DateTimeFormat('en-GB', { day: 'numeric', month: 'long' });
const dayMonthYearFmt = new Intl.DateTimeFormat('en-GB', { day: 'numeric', month: 'long', year: 'numeric' });

/** Clock time in the user's locale, e.g. "5:27 PM" or "17:27". */
export function formatTime(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? '' : timeFmt.format(d);
}

function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

/** "Today", "Yesterday", "Monday 19 October", or "Monday 19 October 2025" for other years. */
export function dayLabel(iso: string, now = new Date()): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '';
  const diff = Math.round((startOfDay(now) - startOfDay(d)) / 86_400_000);
  if (diff === 0) return 'Today';
  if (diff === 1) return 'Yesterday';
  const date = d.getFullYear() === now.getFullYear() ? dayMonthFmt.format(d) : dayMonthYearFmt.format(d);
  return `${weekdayFmt.format(d)} ${date}`;
}

/** Stable key for grouping records by local calendar day. */
export function dayKey(iso: string): string {
  const d = new Date(iso);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

export function formatNumber(n: number): string {
  return Math.round(n).toLocaleString('en-US');
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${formatNumber(n)} ${n === 1 ? one : many}`;
}

/** Decimal units, ≥1000 MB shown as GB ("1.5 GB"). */
export function formatSize(mb: number): string {
  if (mb >= 1000) return `${(mb / 1000).toFixed(1).replace(/\.0$/, '')} GB`;
  return `${Math.round(mb)} MB`;
}

/** "~/Library/…/models" style: replaces the home folder with "~". */
export function shortPath(path: string): string {
  return path.replace(/^\/Users\/[^/]+/, '~');
}

export const PTT_KEYS: { value: PttKey; label: string; cap: string }[] = [
  { value: 'fn', label: 'Fn (Globe)', cap: 'fn' },
  { value: 'right_option', label: 'Right Option', cap: 'Right ⌥' },
  { value: 'right_command', label: 'Right Command', cap: 'Right ⌘' },
  { value: 'right_control', label: 'Right Control', cap: 'Right ⌃' },
  { value: 'right_shift', label: 'Right Shift', cap: 'Right ⇧' },
  { value: 'none', label: 'No key', cap: '' },
];

/** The key cap text for a push-to-talk key ("fn", "Right ⌥"); empty when none is set. */
export function pttCap(key: PttKey | undefined | null): string {
  return PTT_KEYS.find((k) => k.value === key)?.cap ?? '';
}

const MOD_DISPLAY: Record<string, string> = {
  commandorcontrol: '⌘',
  cmdorctrl: '⌘',
  command: '⌘',
  cmd: '⌘',
  super: '⌘',
  meta: '⌘',
  control: '⌃',
  ctrl: '⌃',
  alt: '⌥',
  option: '⌥',
  shift: '⇧',
};

/** "CommandOrControl+Shift+Space" -> ["⌘", "⇧", "Space"] */
export function acceleratorParts(acc: string | null | undefined): string[] {
  if (!acc) return [];
  return acc.split('+').map((p) => MOD_DISPLAY[p.toLowerCase()] ?? p);
}

const CODE_KEYS: Record<string, string> = {
  Space: 'Space', Enter: 'Enter', Tab: 'Tab', Backspace: 'Backspace', Delete: 'Delete',
  ArrowUp: 'Up', ArrowDown: 'Down', ArrowLeft: 'Left', ArrowRight: 'Right',
  Home: 'Home', End: 'End', PageUp: 'PageUp', PageDown: 'PageDown',
  Minus: '-', Equal: '=', BracketLeft: '[', BracketRight: ']', Backslash: '\\',
  Semicolon: ';', Quote: "'", Comma: ',', Period: '.', Slash: '/', Backquote: '`',
};

/**
 * Build a Tauri accelerator string from a keydown event.
 * Returns null while only modifiers are held or the combo is not usable.
 */
export function acceleratorFromEvent(e: KeyboardEvent): string | null {
  const code = e.code;
  let key: string | undefined;
  if (/^Key[A-Z]$/.test(code)) key = code.slice(3);
  else if (/^Digit[0-9]$/.test(code)) key = code.slice(5);
  else if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) key = code;
  else key = CODE_KEYS[code];
  if (!key) return null;

  const mods: string[] = [];
  if (e.metaKey) mods.push('CommandOrControl');
  if (e.ctrlKey) mods.push('Control');
  if (e.altKey) mods.push('Alt');
  if (e.shiftKey) mods.push('Shift');

  const isFn = /^F\d+$/.test(key);
  if (mods.length === 0 && !isFn) return null;
  // Shift alone + a printable key would just type a capital letter.
  if (mods.length === 1 && mods[0] === 'Shift' && !isFn) return null;
  return [...mods, key].join('+');
}
