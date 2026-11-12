//! Misheard terms in a finished meeting's transcript, fixed from the meeting's own vocabulary.
//!
//! The vocabulary is what the user listed under Dictionary › Words to recognise ("PO = purchase
//! order" for an abbreviation and what it stands for), plus abbreviations and product-like names
//! typed into the meeting or read from its screens and photos. Speech recognition hears such
//! words as something else ("ZB" as "ZP" or "Seby's", a product name in three spellings). Code
//! finds the places that sound or look like a term (`find`: German phonetic keys, edit distance,
//! spelled-out letters); the summary model then says, for each place on its own, whether the term
//! was said (`check`). It can only accept what code proposed, never write text of its own. What
//! changed is kept on the segment (`Fix`), what it turned down on the meeting (`Rejected`), so
//! nothing is invented silently and nothing is asked twice.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;

/// A word or name the meeting may use, with what it stands for when the user said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub text: String,
    pub meaning: Option<String>,
}

/// A word the summary model accepted as a misheard term: what the transcript said before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fix {
    pub heard: String,
    pub written: String,
    /// Where `written` starts in the segment's text, in UTF-16 code units (as the UI indexes
    /// strings). Missing in meetings checked before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<usize>,
}

/// A place the summary model said was not the term: not asked about again when the notes are
/// written again. `start_ms` is the segment's start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rejected {
    pub start_ms: u64,
    pub heard: String,
    pub written: String,
}

/// A place in a text that may be a misheard term.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Bytes of the text that would be replaced.
    pub range: Range<usize>,
    pub heard: String,
    /// The term, with a plural "s" when the heard word had one.
    pub written: String,
    /// Index into the terms.
    pub term: usize,
    /// How close the heard words are to the term (0..1); the closest are asked about first.
    pub score: f32,
}

/// Abbreviations up to this many letters are matched by how their letters are spoken.
const ABBREVIATION_MAX: usize = 5;
/// A word must be at least this similar to a term (1 − edit distance / length) to be a candidate.
const MIN_SIMILARITY: f32 = 0.6;
/// Places asked about in one check at most, the most similar first: each takes the summary
/// model a moment, and a long list mostly adds places that only look alike.
pub const CANDIDATES_MAX: usize = 200;
/// The helper's kept context for the questions; dropped after the check (`Worker::forget`), as it
/// is as large as the one the notes need next.
const SLOT: &str = "terms";

/// "PO = purchase order" → the term and its meaning; a plain entry has none.
pub fn parse_entry(entry: &str) -> Option<Term> {
    let text = crate::vocab::term_of(entry);
    let meaning = entry.split_once('=').map(|(_, m)| m.trim()).filter(|m| !m.is_empty());
    (!text.is_empty()).then(|| Term { text: text.to_string(), meaning: meaning.map(str::to_string) })
}

/// All capitals (2 to `ABBREVIATION_MAX` letters): matched letter by letter as spoken.
fn is_abbreviation(term: &str) -> bool {
    let n = term.chars().count();
    (2..=ABBREVIATION_MAX).contains(&n) && term.chars().all(|c| c.is_uppercase())
}

/// Kölner Phonetik: a key that German words sounding alike share ("Drewolin", "Trevolin" → 27356).
pub fn phonetic(word: &str) -> String {
    let letters: Vec<char> = word
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'ä' => 'a',
            'ö' => 'o',
            'ü' => 'u',
            'é' | 'è' | 'ê' => 'e',
            c => c,
        })
        .filter(|c| c.is_ascii_alphabetic() || *c == 'ß')
        .collect();
    let mut codes: Vec<char> = Vec::new();
    for (i, &c) in letters.iter().enumerate() {
        let prev = i.checked_sub(1).map(|p| letters[p]);
        let next = letters.get(i + 1).copied();
        let code: &str = match c {
            'a' | 'e' | 'i' | 'j' | 'o' | 'u' | 'y' => "0",
            'h' => "",
            'b' => "1",
            'p' => {
                if next == Some('h') {
                    "3"
                } else {
                    "1"
                }
            }
            'd' | 't' => {
                if matches!(next, Some('c' | 's' | 'z')) {
                    "8"
                } else {
                    "2"
                }
            }
            'f' | 'v' | 'w' => "3",
            'g' | 'k' | 'q' => "4",
            'c' => {
                let hard = if i == 0 {
                    matches!(next, Some('a' | 'h' | 'k' | 'l' | 'o' | 'q' | 'r' | 'u' | 'x'))
                } else {
                    matches!(next, Some('a' | 'h' | 'k' | 'o' | 'q' | 'u' | 'x')) && !matches!(prev, Some('s' | 'z'))
                };
                if hard {
                    "4"
                } else {
                    "8"
                }
            }
            'x' => {
                if matches!(prev, Some('c' | 'k' | 'q')) {
                    "8"
                } else {
                    "48"
                }
            }
            'l' => "5",
            'm' | 'n' => "6",
            'r' => "7",
            's' | 'z' | 'ß' => "8",
            _ => "",
        };
        for d in code.chars() {
            if codes.last() != Some(&d) {
                codes.push(d);
            }
        }
    }
    codes.iter().enumerate().filter(|(i, d)| *i == 0 || **d != '0').map(|(_, d)| *d).collect()
}

/// How the letters of an abbreviation are said in German ("TD" → "tede", "HB" → "habe"). Letters
/// it doesn't know (not A to Z) are left out, so an abbreviation of those has no key.
fn spoken(letters: &str) -> String {
    letters
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            'a' => "a",
            'b' => "be",
            'c' => "ze",
            'd' => "de",
            'e' => "e",
            'f' => "ef",
            'g' => "ge",
            'h' => "ha",
            'i' => "i",
            'j' => "jot",
            'k' => "ka",
            'l' => "el",
            'm' => "em",
            'n' => "en",
            'o' => "o",
            'p' => "pe",
            'q' => "ku",
            'r' => "er",
            's' => "es",
            't' => "te",
            'u' => "u",
            'v' => "fau",
            'w' => "we",
            'x' => "ix",
            'y' => "ypsilon",
            'z' => "zet",
            _ => "",
        })
        .collect()
}

fn edit_distance<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let next = (row[j + 1] + 1).min(row[j] + 1).min(diag + usize::from(x != y));
            diag = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// The same word with another ending: equal but for at most three letters at the end of each
/// ("Trevolins" for "Trevolin", "Lieferanten" for a name "Lieferanto"). Either it is the term
/// already, or an ordinary word the term only sounds like; a mishearing changes more than the
/// ending. Both in lowercase.
fn same_stem(heard: &[char], term: &[char]) -> bool {
    let common = heard.iter().zip(term).take_while(|(x, y)| x == y).count();
    common >= 4 && heard.len() - common <= 3 && term.len() - common <= 3
}

/// How similar a heard form is to a term (see `MIN_SIMILARITY`), if it is similar enough and
/// sounds alike: phonetic keys equal, or one digit apart when both have at least three. Cheap
/// checks first: this runs for every term at every word.
fn closeness(lower: &[char], key: &[char], term_lower: &[char], term_key: &[char]) -> Option<f32> {
    if lower.len() < 3 || key.is_empty() || term_key.is_empty() {
        return None;
    }
    let longest = lower.len().max(term_lower.len()).max(1) as f32;
    if 1.0 - lower.len().abs_diff(term_lower.len()) as f32 / longest < MIN_SIMILARITY {
        return None;
    }
    if key != term_key && (key.len().min(term_key.len()) < 3 || key.len().abs_diff(term_key.len()) > 1 || edit_distance(key, term_key) > 1) {
        return None;
    }
    let sim = 1.0 - edit_distance(lower, term_lower) as f32 / longest;
    (sim >= MIN_SIMILARITY).then_some(sim)
}

/// A word of a text: letters and digits, with an apostrophe inside ("Teddy's"). Hyphens part words.
struct Token {
    range: Range<usize>,
    /// Only spaces between this token and the one before (no punctuation, no hyphen).
    spaced: bool,
}

fn tokens(text: &str) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    let mut start: Option<usize> = None;
    let mut gap_start = 0;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (k, &(i, c)) in chars.iter().enumerate() {
        let apostrophe = matches!(c, '\'' | '’')
            && start.is_some()
            && chars.get(k + 1).is_some_and(|(_, n)| n.is_alphabetic());
        if c.is_alphanumeric() || apostrophe {
            if start.is_none() {
                start = Some(i);
                let gap = &text[gap_start..i];
                let spaced = !out.is_empty() && !gap.is_empty() && gap.chars().all(char::is_whitespace);
                out.push(Token { range: i..i, spaced });
            }
        } else if let Some(s) = start.take() {
            out.last_mut().expect("pushed at its start").range = s..i;
            gap_start = i;
        }
    }
    if let Some(s) = start {
        out.last_mut().expect("pushed at its start").range = s..text.len();
    }
    out
}

/// A term as `find` compares it, worked out once per check.
enum Pattern {
    Abbreviation {
        text: String,
        letters: usize,
        /// Phonetic key of how its letters are said; empty for letters `spoken` doesn't know.
        key: String,
    },
    Name {
        /// The term without spaces, lowercase.
        lower: Vec<char>,
        key: Vec<char>,
        words: usize,
        capital: bool,
        ends_with_s: bool,
    },
}

/// A word of the text as `find` compares it, worked out once per text.
struct Word<'a> {
    text: &'a str,
    range: Range<usize>,
    spaced: bool,
    is_term: bool,
    /// Only spaces and full stops between this word and the next ("A. B.").
    close_to_next: bool,
    /// Letters only, at least two: how many, how many capitals, and the phonetic keys of the
    /// letters as spoken, of all of them and without the last one (a plural "s").
    caps: Option<(usize, usize, String, String)>,
    /// "Teddy's": the first letter and the phonetic key of the name.
    possessive: Option<(char, String)>,
}

/// One to three words read as one ("Dreh Wolin"), as `find` compares it with names.
struct Run {
    range: Range<usize>,
    capital: bool,
    lower: Vec<char>,
    key: Vec<char>,
    /// Without a final "s": lowercase and key.
    plural: Option<(Vec<char>, Vec<char>)>,
}

/// The vocabulary, ready to be looked for in many texts (`find` builds one per text).
pub struct Matcher<'a> {
    terms: &'a [Term],
    patterns: Vec<Pattern>,
    lowercase: HashSet<String>,
}

impl<'a> Matcher<'a> {
    pub fn new(terms: &'a [Term]) -> Self {
        let patterns = terms
            .iter()
            .map(|t| {
                let text = t.text.as_str();
                if is_abbreviation(text) {
                    Pattern::Abbreviation { text: text.to_string(), letters: text.chars().count(), key: phonetic(&spoken(text)) }
                } else {
                    let compact: String = text.split_whitespace().collect();
                    Pattern::Name {
                        lower: compact.to_lowercase().chars().collect(),
                        key: phonetic(&compact).chars().collect(),
                        words: text.split_whitespace().count(),
                        capital: text.chars().any(char::is_uppercase),
                        ends_with_s: compact.ends_with('s'),
                    }
                }
            })
            .collect();
        let lowercase = terms.iter().map(|t| t.text.to_lowercase()).collect();
        Matcher { terms, patterns, lowercase }
    }

    fn words<'t>(&self, text: &'t str) -> Vec<Word<'t>> {
        let toks = tokens(text);
        toks.iter()
            .enumerate()
            .map(|(i, t)| {
                let w = &text[t.range.clone()];
                let n = w.chars().count();
                let caps = (n >= 2 && w.chars().all(char::is_alphabetic)).then(|| {
                    let upper = w.chars().filter(|c| c.is_uppercase()).count();
                    let but_last = &w[..w.len() - w.chars().last().map_or(0, char::len_utf8)];
                    (n, upper, phonetic(&spoken(w)), phonetic(&spoken(but_last)))
                });
                let possessive = w
                    .strip_suffix("'s")
                    .or_else(|| w.strip_suffix("’s"))
                    .and_then(|stem| stem.chars().next().map(|first| (first, phonetic(stem))));
                let close_to_next = toks.get(i + 1).is_some_and(|next| text[t.range.end..next.range.start].trim_matches([' ', '.']).is_empty());
                Word { text: w, range: t.range.clone(), spaced: t.spaced, is_term: self.lowercase.contains(&w.to_lowercase()), close_to_next, caps, possessive }
            })
            .collect()
    }

    /// Places in `text` that may be one of the terms misheard, best first where they overlap
    /// (only the best of overlapping ones is kept). A word that is a term already is left alone.
    pub fn find(&self, text: &str) -> Vec<Candidate> {
        let words = self.words(text);
        let mut found: Vec<Candidate> = Vec::new();
        let mut runs: [Vec<Option<Run>>; 3] = Default::default();
        if self.patterns.iter().any(|p| matches!(p, Pattern::Name { .. })) {
            for (len, runs) in (1..=3).zip(runs.iter_mut()) {
                *runs = (0..words.len().saturating_sub(len - 1)).map(|i| run(&words[i..i + len])).collect();
            }
        }
        for (ti, pattern) in self.patterns.iter().enumerate() {
            match pattern {
                Pattern::Abbreviation { text: t, letters: n, key } => {
                    if !key.is_empty() {
                        abbreviation_candidates(text, &words, ti, t, *n, key, &mut found);
                    }
                }
                Pattern::Name { lower, key, words: count, capital, ends_with_s } => {
                    let t = &self.terms[ti].text;
                    for len in count.saturating_sub(1).max(1)..=(count + 1).min(3) {
                        for (i, r) in runs[len - 1].iter().enumerate() {
                            let Some(r) = r else { continue };
                            // A name is written with a capital ("macOS" too); a word in small letters is an ordinary word.
                            if (*capital && !r.capital) || (len == 1 && words[i].is_term) {
                                continue;
                            }
                            // A plural "s" on a misheard term: the stem first ("Drewolins" → "Trevolins").
                            let plural = r.plural.as_ref().filter(|_| !ends_with_s).and_then(|(l, k)| closeness(l, k, lower, key).map(|s| (s, "s")));
                            let Some((score, suffix)) = plural.or_else(|| closeness(&r.lower, &r.key, lower, key).map(|s| (s, ""))) else { continue };
                            if same_stem(&r.lower, lower) {
                                continue;
                            }
                            found.push(Candidate { heard: text[r.range.clone()].into(), range: r.range.clone(), written: format!("{t}{suffix}"), term: ti, score });
                        }
                    }
                }
            }
        }
        // The best first; of overlapping places only the best one (the kept ones never overlap,
        // so only the one starting last before a place's end can overlap it).
        found.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.range.start.cmp(&b.range.start)));
        let mut kept: BTreeMap<usize, Candidate> = BTreeMap::new();
        for c in found {
            let overlaps = kept.range(..c.range.end).next_back().is_some_and(|(_, k)| k.range.end > c.range.start);
            if !overlaps {
                kept.insert(c.range.start, c);
            }
        }
        kept.into_values().collect()
    }
}

/// `words` read as one name, if they may be: joined only across spaces, and when more than one,
/// none of them a term already or a single letter.
fn run(words: &[Word]) -> Option<Run> {
    if words.iter().skip(1).any(|w| !w.spaced) || (words.len() > 1 && words.iter().any(|w| w.is_term || w.text.chars().count() < 2)) {
        return None;
    }
    let heard: String = words.iter().map(|w| w.text).collect();
    let plural = heard.strip_suffix('s').map(|stem| (stem.to_lowercase().chars().collect(), phonetic(stem).chars().collect()));
    Some(Run {
        range: words[0].range.start..words[words.len() - 1].range.end,
        capital: words[0].text.starts_with(char::is_uppercase),
        lower: heard.to_lowercase().chars().collect(),
        key: phonetic(&heard).chars().collect(),
        plural,
    })
}

/// Places where the abbreviation `t` (`n` letters, said as `key`) may have been heard as other
/// capitals, as a name with a possessive, or spelled out.
fn abbreviation_candidates(text: &str, words: &[Word], ti: usize, t: &str, n: usize, key: &str, found: &mut Vec<Candidate>) {
    for (i, word) in words.iter().enumerate() {
        let w = word.text;
        if word.is_term {
            continue;
        }
        // Heard as other capitals: "ZP", plural "ZPs" / "ZPS".
        if let Some((count, upper, all, but_last)) = &word.caps {
            let plural_caps = upper == count && *upper == n + 1 && w.ends_with('S');
            let plural_small = *upper == n && *count == n + 1 && w.ends_with('s');
            let (base, plural, base_key) = if upper == count && *upper == n {
                (w, false, all)
            } else if plural_caps || plural_small {
                (&w[..w.len() - 1], true, but_last)
            } else {
                ("", false, all)
            };
            if !base.is_empty() && base != t && base_key == key {
                let shared = base.chars().zip(t.chars()).filter(|(a, b)| a == b).count();
                if 2 * shared >= n {
                    let written = if plural { format!("{t}s") } else { t.to_string() };
                    found.push(Candidate { range: word.range.clone(), heard: w.into(), written, term: ti, score: shared as f32 / n as f32 });
                }
            }
        }
        // Heard as an English name with a possessive: "Teddy's" for "TDs".
        if let Some((first, stem_key)) = &word.possessive {
            let same_start = t.chars().next().is_some_and(|b| first.eq_ignore_ascii_case(&b));
            if same_start && stem_key == key {
                found.push(Candidate { range: word.range.clone(), heard: w.into(), written: format!("{t}s"), term: ti, score: 0.5 });
            }
        }
        // Spelled out: "T D", "T. D.".
        if i + n <= words.len() {
            let run = &words[i..i + n];
            let single = |w: &Word| w.text.chars().count() == 1 && w.text.chars().all(char::is_uppercase);
            if run.iter().all(single) && run[..n - 1].iter().all(|w| w.close_to_next) {
                let letters: String = run.iter().map(|w| w.text).collect();
                if phonetic(&spoken(&letters)) == key {
                    let mut range = run[0].range.start..run[n - 1].range.end;
                    // "T. D. kam" → "TD kam"; a full stop that may end the sentence stays.
                    let rest = &text[range.end..];
                    if rest.starts_with('.') && rest[1..].trim_start().starts_with(char::is_lowercase) {
                        range.end += 1;
                    }
                    found.push(Candidate { heard: text[range.clone()].into(), range, written: t.to_string(), term: ti, score: 1.0 });
                }
            }
        }
    }
}

/// Places in `text` that may be one of `terms` misheard (see `Matcher::find`).
#[cfg(test)]
pub fn find(text: &str, terms: &[Term]) -> Vec<Candidate> {
    Matcher::new(terms).find(text)
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `text` with the accepted candidates replaced, and what each replaced (in text order, with
/// where it is in the new text).
pub fn apply(text: &str, accepted: &[&Candidate]) -> (String, Vec<Fix>) {
    let mut sorted: Vec<&&Candidate> = accepted.iter().collect();
    sorted.sort_by_key(|c| c.range.start);
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut at) = (0, 0);
    let mut fixes = Vec::new();
    for c in sorted {
        let between = &text[copied..c.range.start];
        out.push_str(between);
        at += utf16_len(between);
        fixes.push(Fix { heard: c.heard.clone(), written: c.written.clone(), at: Some(at) });
        out.push_str(&c.written);
        at += utf16_len(&c.written);
        copied = c.range.end;
    }
    out.push_str(&text[copied..]);
    (out, fixes)
}

/// Adds the `new` fixes of a later check (positions in the text it wrote) to a segment's
/// `fixes` (positions in the text before it): those after a new fix move by the length it
/// changed. Kept in text order; fixes without a position last.
pub fn add_fixes(fixes: &mut Vec<Fix>, mut new: Vec<Fix>) {
    new.sort_by_key(|f| f.at.unwrap_or(usize::MAX));
    // Where each new fix was in the text before, and how much longer it made it.
    let mut moved = 0isize;
    let changes: Vec<(usize, isize)> = new
        .iter()
        .filter_map(|f| {
            let delta = utf16_len(&f.written) as isize - utf16_len(&f.heard) as isize;
            let before = f.at?.checked_add_signed(-moved)?;
            moved += delta;
            Some((before, delta))
        })
        .collect();
    for f in fixes.iter_mut() {
        if let Some(p) = f.at {
            let shift: isize = changes.iter().filter(|(before, _)| *before < p).map(|(_, d)| d).sum();
            f.at = p.checked_add_signed(shift);
        }
    }
    fixes.extend(new);
    fixes.sort_by_key(|f| f.at.unwrap_or(usize::MAX));
}

/// Terms taken from what was typed into or shown in a meeting, at most.
const SHOWN_MAX: usize = 40;

/// Words of `text` that look like names of things rather than ordinary words: abbreviations
/// ("ZB", also in "ZB-2026-0412") and names with a capital inside ("TreVolin"), each once. An
/// abbreviation takes as its meaning a longer word on the same line that starts with its first
/// letter and has the others in order ("Zollbeleg ZB-2026-0412" → ZB = Zollbeleg).
pub fn distinctive(text: &str) -> Vec<Term> {
    let mut out: Vec<Term> = Vec::new();
    for line in text.lines() {
        let words: Vec<&str> = tokens(line).into_iter().map(|t| &line[t.range]).collect();
        for &w in &words {
            if !w.chars().all(char::is_alphabetic) || out.iter().any(|o| o.text == w) {
                continue;
            }
            let inner_capital = w.chars().skip(1).any(char::is_uppercase) && w.chars().any(char::is_lowercase);
            if is_abbreviation(w) {
                let meaning = words.iter().find(|long| spells(long, w)).map(|m| m.to_string());
                out.push(Term { text: w.to_string(), meaning });
            } else if inner_capital {
                out.push(Term { text: w.to_string(), meaning: None });
            }
        }
    }
    out
}

/// Whether `long` could be what `abbreviation` stands for: a word of six letters or more, not in
/// capitals, starting with its first letter and with the others in order.
fn spells(long: &str, abbreviation: &str) -> bool {
    let lower = long.to_lowercase();
    let mut letters = abbreviation.to_lowercase().chars().collect::<Vec<_>>().into_iter();
    if long.chars().count() < 6 || long.chars().all(char::is_uppercase) || !long.chars().all(char::is_alphabetic) {
        return false;
    }
    let mut rest = lower.chars();
    if rest.next() != letters.next() {
        return false;
    }
    letters.all(|l| rest.by_ref().any(|c| c == l))
}

/// Whether a dictionary replacement reads as a term ("Claude Code", "macOS") rather than a
/// phrase or a snippet ("Mit freundlichen Grüßen", "Best regards, Thomas"): at most three words,
/// no punctuation between them, each word of several with a capital or a digit.
fn term_like(s: &str) -> bool {
    let words: Vec<&str> = s.split_whitespace().collect();
    let inside = |c: char| c.is_alphanumeric() || c.is_whitespace() || matches!(c, '-' | '.' | '\'' | '’' | '&' | '+' | '/');
    (1..=3).contains(&words.len())
        && s.chars().all(inside)
        && !s.ends_with('.')
        && (words.len() == 1 || words.iter().all(|w| w.chars().any(|c| c.is_uppercase() || c.is_numeric())))
}

/// The meeting's vocabulary: the user's entries ("ZB = Zollbeleg" or a plain term), the words
/// dictionary replacements write when they read as terms (not phrases), then distinctive words
/// of what was typed into the meeting or shown in it (`typed_and_shown`, up to `SHOWN_MAX`). Each
/// term once; a meaning given anywhere stays with it.
pub fn vocabulary(entries: &[String], replacements: &[String], typed_and_shown: &[&str]) -> Vec<Term> {
    let mut out: Vec<Term> = Vec::new();
    let add = |term: Term, out: &mut Vec<Term>| match out.iter_mut().find(|t| t.text.to_lowercase() == term.text.to_lowercase()) {
        Some(t) if t.meaning.is_none() => t.meaning = term.meaning,
        Some(_) => {}
        None => out.push(term),
    };
    for e in entries {
        if let Some(t) = parse_entry(e) {
            add(t, &mut out);
        }
    }
    for r in replacements {
        if term_like(r.trim()) {
            add(Term { text: r.trim().to_string(), meaning: None }, &mut out);
        }
    }
    let shown: Vec<Term> = typed_and_shown.iter().flat_map(|t| distinctive(t)).collect();
    for t in shown.into_iter().take(SHOWN_MAX) {
        add(t, &mut out);
    }
    out
}

/// A term's words as `uses` compares them.
struct TermWords {
    words: Vec<(String, String)>,
    abbreviation: Vec<bool>,
}

impl TermWords {
    fn of(term: &str) -> Self {
        let words: Vec<(String, String)> = tokens(term).iter().map(|t| (term[t.range.clone()].to_string(), term[t.range.clone()].to_lowercase())).collect();
        let abbreviation = words.iter().map(|(w, _)| is_abbreviation(w)).collect();
        TermWords { words, abbreviation }
    }
}

/// The words of a text, as written and in lowercase.
fn text_words(text: &str) -> Vec<(&str, String)> {
    tokens(text).into_iter().map(|t| (&text[t.range.clone()], text[t.range].to_lowercase())).collect()
}

/// How often `words` have the whole term, its words in a row: an abbreviation in capitals (with a
/// plural "s" when last), other words in any case, the last one also with an ending of up to
/// three letters.
fn uses(words: &[(&str, String)], term: &TermWords) -> usize {
    let m = term.words.len();
    if m == 0 || words.len() < m {
        return 0;
    }
    let matches = |j: usize, (word, lower): &(&str, String)| {
        let (t, t_lower) = &term.words[j];
        let last = j + 1 == m;
        match (term.abbreviation[j], last) {
            (true, true) => word == t || word.strip_suffix('s') == Some(t.as_str()),
            (true, false) => word == t,
            (false, true) => lower.strip_prefix(t_lower.as_str()).is_some_and(|rest| rest.chars().count() <= 3),
            (false, false) => lower == t_lower,
        }
    };
    (0..=words.len() - m).filter(|&k| (0..m).all(|j| matches(j, &words[k + j]))).count()
}

/// The terms `text` uses, as "ZB = what it stands for" when the user said, else the term alone
/// (abbreviations count in capitals only), for the notes: their spelling, and that each is a thing
/// of its own.
pub fn glossary(terms: &[Term], text: &str) -> Vec<String> {
    let words = text_words(text);
    terms
        .iter()
        .filter(|t| uses(&words, &TermWords::of(&t.text)) > 0)
        .map(|t| match &t.meaning {
            Some(m) => format!("{} = {m}", t.text),
            None => t.text.clone(),
        })
        .collect()
}

/// Characters of transcript shown on each side of a candidate.
const CONTEXT: usize = 160;

/// A candidate in its place: the transcript before and after it (a little of the neighbouring
/// segments too), for the model to judge by.
pub struct Place<'a> {
    pub candidate: &'a Candidate,
    pub term: &'a Term,
    pub before: String,
    pub after: String,
    /// How often the meeting has the heard words, and the term itself.
    pub heard_count: usize,
    pub term_count: usize,
}

/// How often `texts` have `term`.
fn term_uses(texts: &[Vec<(&str, String)>], term: &str) -> usize {
    let t = TermWords::of(term);
    texts.iter().map(|words| uses(words, &t)).sum()
}

/// How often `texts` have `heard` as whole words.
fn heard_uses(texts: &[Vec<(&str, String)>], heard: &str) -> usize {
    let words: Vec<&str> = heard.split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '’').filter(|w| !w.is_empty()).collect();
    texts
        .iter()
        .map(|t| t.windows(words.len().max(1)).filter(|w| w.iter().map(|(x, _)| *x).eq(words.iter().copied())).count())
        .sum()
}

fn tail(text: &str, max: usize) -> String {
    let n = text.chars().count();
    let cut: String = text.chars().skip(n.saturating_sub(max)).collect();
    if n > max {
        format!("…{cut}")
    } else {
        cut
    }
}

fn head(text: &str, max: usize) -> String {
    let cut: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        format!("{cut}…")
    } else {
        cut
    }
}

/// The candidates of segment `i` of `texts` in their places.
#[cfg(test)]
pub fn places<'a>(texts: &[&str], i: usize, found: &'a [Candidate], terms: &'a [Term]) -> Vec<Place<'a>> {
    let words: Vec<Vec<(&str, String)>> = texts.iter().map(|t| text_words(t)).collect();
    places_in(texts, &words, i, found, terms)
}

fn places_in<'a>(texts: &[&str], words: &[Vec<(&str, String)>], i: usize, found: &'a [Candidate], terms: &'a [Term]) -> Vec<Place<'a>> {
    let text = texts[i];
    let prev = i.checked_sub(1).map_or("", |p| texts[p]);
    let next = texts.get(i + 1).copied().unwrap_or("");
    found
        .iter()
        .map(|c| Place {
            candidate: c,
            term: &terms[c.term],
            before: tail(format!("{prev} {}", &text[..c.range.start]).trim_start(), CONTEXT),
            after: head(format!("{} {next}", &text[c.range.end..]).trim_end(), CONTEXT),
            heard_count: heard_uses(words, &c.heard),
            term_count: term_uses(words, &terms[c.term].text),
        })
        .collect()
}

const SYSTEM: &str = "You correct a speech-recognition transcript of a meeting. The user listed terms that come up \
    in their meetings. Speech recognition doesn't know such terms, so where one was said it writes something that sounds \
    alike instead: other letters for an abbreviation (\"ZB\" as \"ZP\" or \"Seby's\"), an ordinary word or a garbled \
    spelling for a name; it never writes the term itself there. You get one place where the transcript has words that \
    sound like a term, marked [[like this]]. Say whether the speaker most likely said the term there. Answer yes when the \
    term makes sense there and the marked words make little sense (a strange abbreviation, a non-word, a word that \
    doesn't fit). Answer no when the marked words are an ordinary word, a well-known abbreviation or a name that makes \
    good sense there.";

/// The terms of `places`, each once with its meaning: the start of every question, the same for
/// all of them (the helper keeps it read).
pub fn terms_list(places: &[&Place]) -> String {
    let mut p = String::from("Terms:\n");
    let mut listed: Vec<&str> = Vec::new();
    for pl in places {
        if !listed.contains(&pl.term.text.as_str()) {
            listed.push(&pl.term.text);
            match &pl.term.meaning {
                Some(m) => p.push_str(&format!("- {}: {m}\n", pl.term.text)),
                None => p.push_str(&format!("- {}\n", pl.term.text)),
            }
        }
    }
    p
}

/// The question about one place, after `terms` (`terms_list`). The model says first what the
/// heard words mean there, then whether the term was said: asked straight away, or about many
/// places in one answer, it leans on how it answered before rather than on the sentence.
pub fn question(terms: &str, pl: &Place) -> String {
    let times = |n: usize| if n == 1 { "once".to_string() } else { format!("{n} times") };
    let (c, t) = (pl.candidate, &pl.term.text);
    let meaning = pl.term.meaning.as_ref().map(|m| format!(" ({m})")).unwrap_or_default();
    format!(
        "{terms}\nTerm: {t}{meaning}. The transcript has \"{}\" {} and \"{t}\" {}.\nTranscript: \"{}[[{}]]{}\"\n\n\
         First say in one short sentence what [[{}]] means there, if anything. Then answer whether the speaker said {t} \
         there: yes or no.",
        c.heard,
        times(pl.heard_count),
        times(pl.term_count),
        pl.before,
        c.heard,
        pl.after,
        c.heard,
    )
}

/// The answer: a reason on one line, then the verdict.
const GRAMMAR: &str = "root ::= [^\\n]{5,200} \"\\nAnswer: \" (\"yes\" | \"no\")\n";

/// The verdict in an answer (see `GRAMMAR`), and the reason before it.
fn verdict(answer: &str) -> (bool, &str) {
    let (reason, verdict) = answer.rsplit_once("Answer:").unwrap_or((answer, ""));
    (verdict.trim() == "yes", reason.trim())
}

/// The model's verdict on one candidate of text `segment`, with its reason.
#[derive(Debug, Clone)]
pub struct Decision {
    pub segment: usize,
    pub candidate: Candidate,
    pub yes: bool,
    pub reason: String,
}

/// The candidates of each of `texts` worth asking about: not `skip`ped (turned down before), at
/// most `CANDIDATES_MAX` of them, the most similar first; each text's in text order.
pub fn candidates(texts: &[&str], terms: &[Term], skip: impl Fn(usize, &Candidate) -> bool) -> Vec<Vec<Candidate>> {
    let matcher = Matcher::new(terms);
    let mut all: Vec<(usize, Candidate)> =
        texts.iter().enumerate().flat_map(|(i, t)| matcher.find(t).into_iter().map(move |c| (i, c))).filter(|(i, c)| !skip(*i, c)).collect();
    if all.len() > CANDIDATES_MAX {
        // Stable: of equally close ones, the earlier first.
        all.sort_by(|a, b| b.1.score.total_cmp(&a.1.score));
        all.truncate(CANDIDATES_MAX);
    }
    let mut out = vec![Vec::new(); texts.len()];
    for (i, c) in all {
        out[i].push(c);
    }
    for found in &mut out {
        found.sort_by_key(|c| c.range.start);
    }
    out
}

/// Finds candidates in `texts` (see `candidates`) and asks the model about each on its own
/// (`question`). `progress` gets the share done (0..1). The helper's context for these questions
/// is dropped afterwards, also when one fails.
pub fn check(
    worker: &mut crate::llm::Worker,
    texts: &[&str],
    terms: &[Term],
    skip: impl Fn(usize, &Candidate) -> bool,
    mut progress: impl FnMut(f32),
) -> anyhow::Result<Vec<Decision>> {
    let found = candidates(texts, terms, skip);
    let words: Vec<Vec<(&str, String)>> = texts.iter().map(|t| text_words(t)).collect();
    let all: Vec<(usize, Place)> =
        found.iter().enumerate().flat_map(|(i, f)| places_in(texts, &words, i, f, terms).into_iter().map(move |p| (i, p))).collect();
    if all.is_empty() {
        return Ok(Vec::new());
    }
    let listed = terms_list(&all.iter().map(|(_, p)| p).collect::<Vec<_>>());
    let mut ask = || -> anyhow::Result<Vec<Decision>> {
        let mut out = Vec::with_capacity(all.len());
        for (k, (i, p)) in all.iter().enumerate() {
            let answer = worker.chat_at(SYSTEM, &question(&listed, p), 90, Some(GRAMMAR), Some(0.0), Some(SLOT), |_| {})?;
            let (yes, reason) = verdict(&answer);
            out.push(Decision { segment: *i, candidate: p.candidate.clone(), yes, reason: reason.to_string() });
            progress((k + 1) as f32 / all.len() as f32);
        }
        Ok(out)
    };
    let asked = ask();
    if let Err(e) = worker.forget(SLOT) {
        log::warn!("could not drop the term check's context: {e:#}");
    }
    asked
}

/// The texts the accepted decisions change: index, new text and what changed.
pub fn fixed(texts: &[&str], decisions: &[Decision]) -> Vec<(usize, String, Vec<Fix>)> {
    let mut out = Vec::new();
    for (i, text) in texts.iter().enumerate() {
        let accepted: Vec<&Candidate> = decisions.iter().filter(|d| d.yes && d.segment == i).map(|d| &d.candidate).collect();
        if !accepted.is_empty() {
            let (new, fixes) = apply(text, &accepted);
            out.push((i, new, fixes));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(entries: &[&str]) -> Vec<Term> {
        entries.iter().filter_map(|e| parse_entry(e)).collect()
    }

    fn corrected(text: &str, entries: &[&str]) -> String {
        let t = terms(entries);
        let found = find(text, &t);
        apply(text, &found.iter().collect::<Vec<_>>()).0
    }

    fn fix(heard: &str, written: &str, at: usize) -> Fix {
        Fix { heard: heard.into(), written: written.into(), at: Some(at) }
    }

    #[test]
    fn an_entry_may_say_what_it_stands_for() {
        assert_eq!(parse_entry("ZK = Zollkennung"), Some(Term { text: "ZK".into(), meaning: Some("Zollkennung".into()) }));
        assert_eq!(parse_entry(" Brontix "), Some(Term { text: "Brontix".into(), meaning: None }));
        assert_eq!(parse_entry("Brontix ="), Some(Term { text: "Brontix".into(), meaning: None }));
        assert_eq!(parse_entry(" = nothing"), None);
    }

    #[test]
    fn koelner_phonetik() {
        assert_eq!(phonetic("Müller-Lüdenscheidt"), "65752682");
        assert_eq!(phonetic("Wikipedia"), "3412");
        assert_eq!(phonetic("Breschnew"), "17863");
        assert_eq!(phonetic("Drewolin"), phonetic("Trevolin"));
        assert_eq!(phonetic(&spoken("TD")), phonetic(&spoken("DT")));
        assert_eq!(phonetic(&spoken("TD")), phonetic("Teddy"));
    }

    #[test]
    fn capitals_heard_for_an_abbreviation() {
        let t = ["ZB = Zollbeleg"];
        assert_eq!(corrected("Wir haben die ZP geprüft.", &t), "Wir haben die ZB geprüft.");
        assert_eq!(corrected("Drei ZPs sind gekommen, und zwei ZPS fehlen.", &t), "Drei ZBs sind gekommen, und zwei ZBs fehlen.");
        assert_eq!(corrected("Eine Änderungs-ZP kam.", &t), "Eine Änderungs-ZB kam.");
    }

    #[test]
    fn spelled_and_named_forms_of_an_abbreviation() {
        assert_eq!(corrected("Die Teddy's liegen schon im Fahrerhaus.", &["TD"]), "Die TDs liegen schon im Fahrerhaus.");
        assert_eq!(corrected("Das steht auf dem T. D. oben.", &["TD"]), "Das steht auf dem TD oben.");
        assert_eq!(corrected("Das steht auf dem T D oben.", &["TD"]), "Das steht auf dem TD oben.");
    }

    #[test]
    fn ordinary_words_and_other_abbreviations_stay() {
        let t = ["TD"];
        for text in ["Tee oder Kaffee, egal.", "Ted hat Tee gekocht.", "Aber die TDB liefert.", "Im DT-System.", "Das ist ein PDF."] {
            assert!(find(text, &terms(&t)).is_empty(), "{text}: {:?}", find(text, &terms(&t)));
        }
        // An abbreviation the user listed too is a term in its own right.
        let found = find("Die TDB liefert, die TT auch.", &terms(&["TD", "TDB"]));
        assert_eq!(found.iter().map(|c| c.heard.as_str()).collect::<Vec<_>>(), ["TT"]);
    }

    #[test]
    fn abbreviations_of_letters_without_a_spoken_form_are_not_matched() {
        // "ÄÖ" has no key: without the check, every other such word matched it.
        assert!(spoken("ÄÖ").is_empty());
        for text in ["Die ÖÜ kam.", "Ä Ö", "Ein Öö's."] {
            assert!(find(text, &terms(&["ÄÖ"])).is_empty(), "{text}");
        }
    }

    #[test]
    fn names_heard_in_other_spellings() {
        let t = ["Trevolin"];
        assert_eq!(corrected("Das läuft über Drewolin, also über Trewolin.", &t), "Das läuft über Trevolin, also über Trevolin.");
        assert_eq!(corrected("Die Daten gehen nach Dreh Wolin zurück.", &t), "Die Daten gehen nach Trevolin zurück.");
        assert_eq!(corrected("Die Drewolin-Daten.", &t), "Die Trevolin-Daten.");
        // Already right, or a different word; nor is a word glued onto the right one.
        assert!(find("Im Trevolin a und es ist drollig, der Trevolins.", &terms(&t)).is_empty());
        // The same word with another ending is a word of its own ("Trevolinern" for "Trevolina").
        assert!(find("Die Trevolinern und die Trevolinas.", &terms(&["Trevolina"])).is_empty());
        // A name is written with a capital: "drewolin" in small letters is some other word.
        assert!(find("Das ist drewolin.", &terms(&t)).is_empty());
        assert!(find("Das macht nichts.", &terms(&["macOS"])).is_empty());
    }

    #[test]
    fn a_plural_s_stays_on_the_term() {
        assert_eq!(corrected("Alle Drewolins sind da.", &["Trevolin"]), "Alle Trevolins sind da.");
    }

    #[test]
    fn multi_word_terms() {
        assert_eq!(corrected("Wir nutzen Klaude Kode dafür.", &["Claude Code"]), "Wir nutzen Claude Code dafür.");
    }

    #[test]
    fn overlapping_candidates_keep_the_closest() {
        let found = find("Das Drewolin ist da.", &terms(&["Trevolin", "Trevolan"]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].written, "Trevolin");
    }

    #[test]
    fn applying_keeps_what_was_heard_and_where() {
        let text = "Die ZPs und das Drewolin.";
        let t = terms(&["ZB", "Trevolin"]);
        let found = find(text, &t);
        let (out, fixes) = apply(text, &found.iter().collect::<Vec<_>>());
        assert_eq!(out, "Die ZBs und das Trevolin.");
        assert_eq!(fixes, [fix("ZPs", "ZBs", 4), fix("Drewolin", "Trevolin", 16)]);
        // Only what was accepted changes.
        let (out, fixes) = apply(text, &[&found[1]]);
        assert_eq!(out, "Die ZPs und das Trevolin.");
        assert_eq!(fixes, [fix("Drewolin", "Trevolin", 16)]);
        // Positions count as the UI does (UTF-16), after replacements of another length.
        let text = "Größe: Dreh Wolin und die ZP.";
        let found = find(text, &t);
        let (out, fixes) = apply(text, &found.iter().collect::<Vec<_>>());
        assert_eq!(out, "Größe: Trevolin und die ZB.");
        assert_eq!(fixes, [fix("Dreh Wolin", "Trevolin", 7), fix("ZP", "ZB", 24)]);
    }

    #[test]
    fn a_later_check_moves_the_fixes_after_its_own() {
        let t = terms(&["ZB", "Trevolin"]);
        let text = "Im Dreh Wolin liegt der ZP.";
        let found = find(text, &t);
        // First run: only the ZB.
        let zb = found.iter().find(|c| c.written == "ZB").unwrap();
        let (text, first) = apply(text, &[zb]);
        let mut fixes = Vec::new();
        add_fixes(&mut fixes, first);
        assert_eq!(fixes, [fix("ZP", "ZB", 24)]);
        // The notes written again: the name, before it and two shorter.
        let found = find(&text, &t);
        assert_eq!(found.len(), 1);
        let (text, second) = apply(&text, &[&found[0]]);
        add_fixes(&mut fixes, second);
        assert_eq!(text, "Im Trevolin liegt der ZB.");
        assert_eq!(fixes, [fix("Dreh Wolin", "Trevolin", 3), fix("ZP", "ZB", 22)]);
        let utf16: Vec<u16> = text.encode_utf16().collect();
        for f in &fixes {
            let at = f.at.unwrap();
            assert_eq!(String::from_utf16(&utf16[at..at + f.written.encode_utf16().count()]).unwrap(), f.written);
        }
        // Fixes kept before positions were stored stay, last.
        let mut old = vec![Fix { heard: "ZPs".into(), written: "ZBs".into(), at: None }];
        add_fixes(&mut old, vec![fix("ZP", "ZB", 5)]);
        assert_eq!(old[0], fix("ZP", "ZB", 5));
        assert_eq!(old[1].at, None);
    }

    #[test]
    fn fixes_load_without_a_position() {
        let f: Fix = serde_json::from_str(r#"{"heard":"ZP","written":"ZB"}"#).unwrap();
        assert_eq!(f.at, None);
        assert_eq!(serde_json::to_string(&fix("ZP", "ZB", 3)).unwrap(), r#"{"heard":"ZP","written":"ZB","at":3}"#);
    }

    #[test]
    fn the_model_sees_each_place_in_its_sentence() {
        let t = terms(&["ZB = Zollbeleg", "Trevolin"]);
        let texts = ["Der Spediteur schickt eine neue.", "Wir haben die ZP geprüft.", "Das passt."];
        let found = find(texts[1], &t);
        let pl = places(&texts, 1, &found, &t);
        let listed = terms_list(&pl.iter().collect::<Vec<_>>());
        assert_eq!(listed, "Terms:\n- ZB: Zollbeleg\n");
        assert_eq!(
            question(&listed, &pl[0]),
            "Terms:\n- ZB: Zollbeleg\n\nTerm: ZB (Zollbeleg). The transcript has \"ZP\" once and \"ZB\" 0 times.\n\
             Transcript: \"Der Spediteur schickt eine neue. Wir haben die [[ZP]] geprüft. Das passt.\"\n\n\
             First say in one short sentence what [[ZP]] means there, if anything. Then answer whether the speaker said ZB \
             there: yes or no."
        );
    }

    #[test]
    fn the_answer_is_a_reason_then_a_verdict() {
        assert_eq!(GRAMMAR, "root ::= [^\\n]{5,200} \"\\nAnswer: \" (\"yes\" | \"no\")\n");
        assert_eq!(verdict("A customs document.\nAnswer: yes"), (true, "A customs document."));
        assert_eq!(verdict("Means to deliver.\nAnswer: no"), (false, "Means to deliver."));
        // Anything else keeps the transcript as it is.
        assert!(!verdict("cut off").0);
    }

    #[test]
    fn long_neighbours_are_cut() {
        let t = terms(&["ZB"]);
        let long = "wort ".repeat(100);
        let texts = [long.as_str(), "Die ZP kam.", long.as_str()];
        let found = find(texts[1], &t);
        let pl = places(&texts, 1, &found, &t);
        assert!(pl[0].before.starts_with('…') && pl[0].before.chars().count() <= CONTEXT + 1);
        assert!(pl[0].after.ends_with('…') && pl[0].after.chars().count() <= CONTEXT + 1);
    }

    #[test]
    fn distinctive_words_of_a_screen() {
        let found = distinctive("Zollbeleg ZB-2026-0412 von TreVolin GmbH, Pos 1 | KW 47 | DN12 Stk\nKunde: Kaltenbrunner KG");
        let shown: Vec<(&str, Option<&str>)> = found.iter().map(|t| (t.text.as_str(), t.meaning.as_deref())).collect();
        // ZB stands for the word before it; KW for nothing on its line; KG not for "Kaltenbrunner" (no g).
        assert_eq!(shown, [("ZB", Some("Zollbeleg")), ("TreVolin", None), ("GmbH", None), ("KW", None), ("KG", None)]);
    }

    #[test]
    fn the_vocabulary_merges_its_sources() {
        let v = vocabulary(
            &["ZB = Zollbeleg".into(), "Trevolin".into()],
            &["Wortfeld".into(), "trevolin".into()],
            &["Siehe ZB-17 und KW 3.", "Der ZB kam.", "Quittung QT-4"],
        );
        let shown: Vec<(&str, Option<&str>)> = v.iter().map(|t| (t.text.as_str(), t.meaning.as_deref())).collect();
        assert_eq!(shown, [("ZB", Some("Zollbeleg")), ("Trevolin", None), ("Wortfeld", None), ("KW", None), ("QT", Some("Quittung"))]);
    }

    #[test]
    fn phrases_a_replacement_writes_are_no_terms() {
        let replacements: Vec<String> =
            ["Mit freundlichen Grüßen", "Best regards, Thomas", "Danke.", "Claude Code", "macOS", "Next.js", "Wortfeld 2"].map(String::from).to_vec();
        let v = vocabulary(&[], &replacements, &[]);
        assert_eq!(v.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["Claude Code", "macOS", "Next.js", "Wortfeld 2"]);
        // The user's own words to recognise stay, whatever they look like.
        assert_eq!(vocabulary(&["Mit freundlichen Grüßen".into()], &[], &[]).len(), 1);
    }

    #[test]
    fn the_glossary_lists_terms_said_in_the_meeting() {
        let t = terms(&["ZB = Zollbeleg", "QX = Quarzuhr", "Trevolin = das Lagersystem", "Zollinka", "Brontex"]);
        assert_eq!(glossary(&t, "Die ZBs kamen ins trevolin, dann zur Zollinka."), ["ZB = Zollbeleg", "Trevolin = das Lagersystem", "Zollinka"]);
        // An abbreviation counts in capitals only.
        assert!(glossary(&t, "zb und qx").is_empty());
    }

    #[test]
    fn the_glossary_needs_the_whole_term() {
        let t = terms(&["Claude Code", "Next.js", "Mit freundlichen Grüßen"]);
        assert!(glossary(&t, "Claude hat den Code geschrieben, next week.").is_empty());
        assert_eq!(glossary(&t, "Wir nutzen Claude Code mit Next.js."), ["Claude Code", "Next.js"]);
        assert_eq!(glossary(&t, "Mit freundlichen Grüßen, Anna"), ["Mit freundlichen Grüßen"]);
        assert!(glossary(&t, "Mit dem Kunden.").is_empty());
    }

    #[test]
    fn only_accepted_decisions_change_a_text() {
        let t = terms(&["ZB", "Trevolin"]);
        let texts = ["Die ZP und das Drewolin.", "Nichts."];
        let found = find(texts[0], &t);
        let decisions: Vec<Decision> = found
            .iter()
            .map(|c| Decision { segment: 0, candidate: c.clone(), yes: c.written == "Trevolin", reason: String::new() })
            .collect();
        let out = fixed(&texts, &decisions);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, "Die ZP und das Trevolin.");
        assert_eq!(out[0].2, [fix("Drewolin", "Trevolin", 15)]);
    }

    #[test]
    fn places_count_the_heard_words_and_the_term() {
        let t = terms(&["ZB", "Trevolin"]);
        let texts = ["Die ZPs und die ZP.", "Der ZB, die ZBs, ab heute.", "Im Dreh Wolin und im Trevolins."];
        let count = |i: usize| {
            let found = find(texts[i], &t);
            places(&texts, i, &found, &t).iter().map(|p| (p.candidate.heard.clone(), p.heard_count, p.term_count)).collect::<Vec<_>>()
        };
        assert_eq!(count(0), [("ZPs".to_string(), 1, 2), ("ZP".to_string(), 1, 2)]);
        assert_eq!(count(2), [("Dreh Wolin".to_string(), 1, 1)]);
    }

    #[test]
    fn candidates_are_capped_closest_first_and_skip_what_was_turned_down() {
        let t = terms(&["ZB", "Trevolin"]);
        // Each segment has a close one (spelled out: 1.0) and a farther one (ZP for ZB: 0.5).
        let texts: Vec<String> = (0..150).map(|i| format!("Satz {i}: Z B und ZP.")).collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let found = candidates(&texts, &t, |_, _| false);
        let all: Vec<&Candidate> = found.iter().flatten().collect();
        assert_eq!(all.len(), CANDIDATES_MAX);
        assert_eq!(all.iter().filter(|c| c.heard == "Z B").count(), 150, "the closest ones all stay");
        assert_eq!(all.iter().filter(|c| c.heard == "ZP").count(), 50, "the first of the rest");
        assert!(found[0].len() == 2 && found[149].len() == 1);
        // A place turned down before is not asked again.
        let found = candidates(&texts[..2], &t, |i, c| i == 0 && c.heard == "ZP");
        assert_eq!(found[0].iter().map(|c| c.heard.as_str()).collect::<Vec<_>>(), ["Z B"]);
        assert_eq!(found[1].len(), 2);
    }

    /// 500 terms over 10 000 words: before, every term looked at every word with a fresh
    /// lowercase copy of all the terms (200 terms took 46 s). Release builds take well under a
    /// second (`cargo test --release terms::tests::many`); debug builds are much slower.
    #[test]
    fn many_terms_over_a_long_meeting() {
        let syllables = ["ka", "lo", "mi", "te", "ru", "sen", "bar", "dor", "fin", "gel", "hu", "wen"];
        let word = |n: usize| -> String {
            let mut w: String = (0..2 + n % 3).map(|k| syllables[(n / (k + 1) + k * 7) % syllables.len()]).collect();
            if n % 4 == 0 {
                w = w[..1].to_uppercase() + &w[1..];
            }
            w
        };
        let entries: Vec<String> = (0..500)
            .map(|i| match i % 5 {
                0 => format!("{}{}", (b'A' + (i % 26) as u8) as char, (b'A' + (i / 26 % 26) as u8) as char),
                1 => format!("{} {}", word(i * 31 + 4), word(i * 17 + 8)),
                _ => {
                    let w = word(i * 13 + 4);
                    w[..1].to_uppercase() + &w[1..]
                }
            })
            .collect();
        let t: Vec<Term> = entries.iter().filter_map(|e| parse_entry(e)).collect();
        // Every third word misheard (a sound swapped for one alike), every 50th two capitals.
        let heard = |n: usize| -> String {
            match n % 50 {
                0 => format!("{}{}", (b'A' + (n % 26) as u8) as char, (b'A' + (n / 7 % 26) as u8) as char),
                k if k % 3 == 0 => word(n).replace('k', "g").replace('t', "d").replace('w', "f"),
                _ => word(n),
            }
        };
        let segments: Vec<String> = (0..1000).map(|s| (0..10).map(|k| heard(s * 10 + k)).collect::<Vec<_>>().join(" ") + ".").collect();
        let texts: Vec<&str> = segments.iter().map(String::as_str).collect();
        let started = std::time::Instant::now();
        let found = candidates(&texts, &t, |_, _| false);
        let glossary = glossary(&t, &segments.join("\n"));
        let took = started.elapsed();
        assert_eq!(found.iter().flatten().count(), CANDIDATES_MAX);
        assert!(!glossary.is_empty());
        if !cfg!(debug_assertions) {
            assert!(took.as_secs_f32() < 1.0, "{took:?}");
        }
        eprintln!("500 terms over 10 000 words: {took:?}, {} candidates kept, {} terms said", found.iter().flatten().count(), glossary.len());
    }
}
