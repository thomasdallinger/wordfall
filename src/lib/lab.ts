// Voice Lab helpers shared by the lab view and its components.
import type { LabCategory } from './types';

export const LAB_CATEGORIES: { id: LabCategory; label: string }[] = [
  { id: 'short', label: 'Quick replies' },
  { id: 'jargon', label: 'Technical terms' },
  { id: 'numbers', label: 'Numbers and dates' },
  { id: 'prose', label: 'Sentences' },
  { id: 'long', label: 'Long passages' },
  { id: 'personal', label: 'Your own words' },
];

export function categoryLabel(c: LabCategory): string {
  return LAB_CATEGORIES.find((x) => x.id === c)?.label ?? c;
}

export type WerTone = 'good' | 'fair' | 'poor';

/** Good ≤ 5 %, fair ≤ 15 %, poor otherwise. Always shown with words, never colour alone. */
export function werGrade(wer: number): { tone: WerTone; label: string } {
  if (wer <= 0.05) return { tone: 'good', label: 'Spot on' };
  if (wer <= 0.15) return { tone: 'fair', label: 'Close' };
  return { tone: 'poor', label: 'Off' };
}

/** The status class for a grade tone. */
export function werClass(tone: WerTone): string {
  return tone === 'good' ? 'status-ok' : tone === 'fair' ? 'status-warn' : 'status-bad';
}

export function formatWer(wer: number): string {
  return `${Math.round(wer * 100)}%`;
}

function escapeRe(s: string) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** Same rule as the Rust metrics: exact, case-sensitive match at word boundaries. */
export function containsTerm(hypothesis: string, term: string): boolean {
  return new RegExp(`(^|[^\\p{L}\\p{N}])${escapeRe(term)}($|[^\\p{L}\\p{N}])`, 'u').test(hypothesis);
}

export function missedTerms(terms: string[], hypothesis: string): string[] {
  return terms.filter((t) => !containsTerm(hypothesis, t));
}

/** Split `text` into plain and term segments so terms can be emphasised in place. */
export function termSegments(text: string, terms: string[]): { text: string; term: string | null }[] {
  if (terms.length === 0) return [{ text, term: null }];
  const sorted = [...terms].sort((a, b) => b.length - a.length).map(escapeRe);
  const re = new RegExp(`(?<![\\p{L}\\p{N}])(${sorted.join('|')})(?![\\p{L}\\p{N}])`, 'gu');
  return text
    .split(re)
    .filter(Boolean)
    .map((part) => ({ text: part, term: terms.includes(part) ? part : null }));
}

/** "0:07" */
export function formatClock(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}
