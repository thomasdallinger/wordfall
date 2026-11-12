/**
 * Where the term check replaced misheard words in a transcript segment, for marking them.
 * No imports: `npm test` runs this file in Node as it is.
 */

/** A piece of transcript text; `heard` is set where the term check replaced what was heard. */
export interface TextPart {
  text: string;
  heard?: string;
}

/** A fix as stored on a segment (`TermFix`): `at` is where `written` starts, in UTF-16 units. */
interface Fix {
  heard: string;
  written: string;
  at?: number;
}

const WORD_CHAR = /[\p{L}\p{N}]/u;

/** Whether `text` has a whole word (or words) at `at`..`end`: no letter or digit right outside. */
function wholeWord(text: string, at: number, end: number): boolean {
  return !WORD_CHAR.test(text.charAt(at - 1)) && !WORD_CHAR.test(text.charAt(end));
}

/**
 * `text` cut into parts at the words the term check wrote, so they can be marked. A fix is
 * marked where it says it is (`at`); one kept without a position, or whose position no longer
 * has its word, is marked at the first whole-word occurrence not marked already.
 */
export function fixedParts(text: string, fixes: Fix[] | undefined): TextPart[] {
  const spans: { at: number; end: number; heard: string }[] = [];
  const free = (at: number, end: number) => spans.every((s) => end <= s.at || at >= s.end);
  const place = (f: Fix, at: number) => spans.push({ at, end: at + f.written.length, heard: f.heard });
  const unplaced: Fix[] = [];
  for (const f of fixes ?? []) {
    const at = f.at;
    if (at !== undefined && text.startsWith(f.written, at) && free(at, at + f.written.length)) place(f, at);
    else unplaced.push(f);
  }
  for (const f of unplaced) {
    if (!f.written) continue;
    for (let at = text.indexOf(f.written); at >= 0; at = text.indexOf(f.written, at + 1)) {
      const end = at + f.written.length;
      if (wholeWord(text, at, end) && free(at, end)) {
        place(f, at);
        break;
      }
    }
  }
  spans.sort((a, b) => a.at - b.at);
  const out: TextPart[] = [];
  let pos = 0;
  for (const s of spans) {
    if (s.at > pos) out.push({ text: text.slice(pos, s.at) });
    out.push({ text: text.slice(s.at, s.end), heard: s.heard });
    pos = s.end;
  }
  if (pos < text.length || out.length === 0) out.push({ text: text.slice(pos) });
  return out;
}
