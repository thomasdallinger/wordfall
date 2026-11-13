//! What was shown in a meeting: text read from a shared screen or from a photo (a printout, a
//! whiteboard). Speech recognition mishears names, numbers and abbreviations ("TD" became
//! "Teddy's"); the screen spells them. Visuals enter the notes, questions and map as their own
//! lines ("[03:20] Screen: …", "[05:10] Photo: …") merged into the transcript by time. They are
//! not speakers.
//!
//! This module is the pure part: text rows from OCR boxes (so tables stay readable), UI chrome
//! that repeats on every slide, change detection for screen reading, and the prompt lines with
//! their caps. Capturing, OCR and storage live in `capture.rs` and the platform code.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::platform::OcrLine;

/// Where a visual came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Read from the window the user picked while recording.
    Screen,
    /// An image the user added (dropped, picked, pasted or offered from Downloads).
    Photo,
}

impl Source {
    /// How its lines start in the prompts.
    fn word(self) -> &'static str {
        match self {
            Source::Screen => "Screen",
            Source::Photo => "Photo",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Visual {
    /// 1, 2, … within the meeting.
    pub id: u32,
    /// Meeting time it was first seen (or where the user placed the photo).
    pub at_ms: u64,
    /// Meeting time a screen was last seen unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<u64>,
    pub source: Source,
    /// The window ("Microsoft Teams: Screen sharing") or the file name.
    #[serde(default)]
    pub label: String,
    /// What was read, a line per row; cells of a table row are joined with " | ".
    pub text: String,
    /// The picture, relative to the meeting's folder ("visuals/3.jpg"); deleted with the recordings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

impl Visual {
    /// The last meeting time it was on screen.
    pub fn last_ms(&self) -> u64 {
        self.until_ms.unwrap_or(self.at_ms).max(self.at_ms)
    }
}

// ---------- rows ----------

/// OCR boxes as text rows, top to bottom: boxes that overlap vertically by at least half the
/// smaller height are one row, read left to right and joined with " | " (Vision returns a box per
/// table cell, so a table row stays one line instead of a column of loose values).
pub fn rows(lines: &[OcrLine]) -> Vec<String> {
    rows_of(lines.iter().filter(|l| !l.text.trim().is_empty()).collect())
}

/// A shared screen's text rows (see `rows`) without the text at the window's sides that isn't
/// what was shown: a sidebar of other pages and a lone label such as a video tile's name (see
/// `side_boxes`). Photos keep everything.
pub fn screen_rows(lines: &[OcrLine]) -> Vec<String> {
    let boxes: Vec<&OcrLine> = lines.iter().filter(|l| !l.text.trim().is_empty()).collect();
    let side = side_boxes(&boxes);
    rows_of(boxes.into_iter().enumerate().filter(|(i, _)| !side.contains(i)).map(|(_, b)| b).collect())
}

fn rows_of(mut boxes: Vec<&OcrLine>) -> Vec<String> {
    boxes.sort_by(|a, b| (a.y + a.h / 2.0).total_cmp(&(b.y + b.h / 2.0)));
    // (top, bottom, boxes)
    let mut grouped: Vec<(f32, f32, Vec<&OcrLine>)> = Vec::new();
    for b in boxes {
        let joins = grouped.last().is_some_and(|(top, bottom, _)| {
            let overlap = bottom.min(b.y + b.h) - top.max(b.y);
            overlap >= 0.5 * (bottom - top).min(b.h)
        });
        if joins {
            let (top, bottom, members) = grouped.last_mut().expect("checked above");
            *top = top.min(b.y);
            *bottom = bottom.max(b.y + b.h);
            members.push(b);
        } else {
            grouped.push((b.y, b.y + b.h, vec![b]));
        }
    }
    grouped
        .into_iter()
        .map(|(_, _, mut members)| {
            members.sort_by(|a, b| a.x.total_cmp(&b.x));
            members.iter().map(|m| squash(&m.text)).collect::<Vec<_>>().join(" | ")
        })
        .collect()
}

/// Whitespace collapsed to single spaces.
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------- the window's sides ----------

/// Text at a side of the window is apart from the rest when no box reaches across a gap of at
/// least this share of the width…
const SIDE_GAP: f32 = 0.005;
/// …and is left out when it is a lone label (at most this many boxes of at most `LABEL_WORDS`
/// words: the name under a video tile, a page number)…
const LABEL_BOXES: usize = 2;
const LABEL_WORDS: usize = 4;
/// …or a sidebar: a list of at least this many entries…
const SIDEBAR_MIN: usize = 3;
/// …at most this share of the width wide, starting at most this far from the window's edge…
const SIDEBAR_WIDTH: f32 = 0.25;
const SIDEBAR_EDGE: f32 = 0.05;
/// …with fewer than this share of its entries level with text beside it (a table's first column
/// is level with its other cells; a sidebar's entries only meet a line of the page by chance).
const SIDEBAR_LEVEL: f32 = 2.0 / 3.0;

/// Which of `boxes` sit at the left or right side of the window, apart from what was shown: a
/// lone label beyond a gap, or a sidebar listing other pages, projects or channels. OCR reads a
/// row across the whole width, so a sidebar entry would otherwise be glued to a line of the page
/// ("Projects | Q3 revenue grew"). Only the outermost group on each side is looked at, so a
/// page's own columns stay.
fn side_boxes(boxes: &[&OcrLine]) -> HashSet<usize> {
    let mut spans: Vec<(f32, f32)> = boxes.iter().map(|b| (b.x, b.x + b.w)).collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Gaps no box reaches across, left to right.
    let mut gaps: Vec<(f32, f32)> = Vec::new();
    let mut reach = f32::MIN;
    for (start, end) in spans {
        if reach > f32::MIN && start - reach >= SIDE_GAP {
            gaps.push((reach, start));
        }
        reach = reach.max(end);
    }
    let mut out = HashSet::new();
    let (Some(first), Some(last)) = (gaps.first(), gaps.last()) else { return out };
    for left in [true, false] {
        let group: Vec<usize> = (0..boxes.len())
            .filter(|&i| if left { boxes[i].x + boxes[i].w <= first.0 } else { boxes[i].x >= last.1 })
            .collect();
        let rest: Vec<&OcrLine> = (0..boxes.len()).filter(|i| !group.contains(i)).map(|i| boxes[i]).collect();
        let side: Vec<&OcrLine> = group.iter().map(|&i| boxes[i]).collect();
        if rest.len() > side.len() && (is_label(&side) || is_sidebar(&side, &rest, left)) {
            out.extend(group);
        }
    }
    out
}

/// A few short boxes on their own.
fn is_label(side: &[&OcrLine]) -> bool {
    side.len() <= LABEL_BOXES && side.iter().all(|b| b.text.split_whitespace().count() <= LABEL_WORDS)
}

/// A narrow list at the window's edge whose entries mostly aren't level with the text beside it.
fn is_sidebar(side: &[&OcrLine], rest: &[&OcrLine], left: bool) -> bool {
    let from = side.iter().map(|b| b.x).fold(f32::MAX, f32::min);
    let to = side.iter().map(|b| b.x + b.w).fold(f32::MIN, f32::max);
    let at_edge = if left { from <= SIDEBAR_EDGE } else { to >= 1.0 - SIDEBAR_EDGE };
    let centre = |b: &OcrLine| b.y + b.h / 2.0;
    let level = side.iter().filter(|b| rest.iter().any(|r| (centre(b) - centre(r)).abs() < 0.25 * b.h.min(r.h))).count();
    side.len() >= SIDEBAR_MIN && to - from <= SIDEBAR_WIDTH && at_edge && (level as f32) < SIDEBAR_LEVEL * side.len() as f32
}

// ---------- chrome ----------

/// A screen visual's cells are only compared once there are this many…
const CHROME_MIN: usize = 3;
/// …and a cell in at least this share of them (in fifths) is window chrome (toolbar, tabs, the
/// app's own buttons, the name under a video tile), not what was shown.
const CHROME_SHARE: usize = 4;

/// The cells of a row: what OCR read as one box each, joined with " | " by `rows`.
fn cells(row: &str) -> impl Iterator<Item = &str> {
    row.split(" | ").map(str::trim).filter(|c| !c.is_empty())
}

/// A cell as compared for chrome and change: lower case, digits as '#' (the clock in a toolbar
/// changes, and so do the values in a form while it is the same form), spaces collapsed.
fn cell_key(cell: &str) -> String {
    let key: String = cell.chars().map(|c| if c.is_ascii_digit() { '#' } else { c.to_ascii_lowercase() }).collect();
    squash(&key)
}

/// Cells that repeat on most screen visuals (in at least four in five of them, from three on):
/// the window around what was shown. Cells, not rows: OCR joins a toolbar button with whatever
/// stands level with it, so a whole row rarely repeats. Photos don't count.
pub fn chrome(visuals: &[Visual]) -> HashSet<String> {
    let screens: Vec<&Visual> = visuals.iter().filter(|v| v.source == Source::Screen).collect();
    if screens.len() < CHROME_MIN {
        return HashSet::new();
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for v in &screens {
        let unique: HashSet<String> = v.text.lines().flat_map(cells).map(cell_key).collect();
        for k in unique {
            *counts.entry(k).or_default() += 1;
        }
    }
    counts.into_iter().filter(|(_, n)| n * 5 >= screens.len() * CHROME_SHARE).map(|(k, _)| k).collect()
}

/// The rows of `text` without their chrome cells; rows of chrome only are left out.
fn content_rows<'a>(text: &'a str, chrome: &'a HashSet<String>) -> impl Iterator<Item = String> + 'a {
    text.lines()
        .map(|row| cells(row).filter(|c| !chrome.contains(&cell_key(c))).collect::<Vec<_>>().join(" | "))
        .filter(|row| !row.is_empty())
}

// ---------- change detection ----------

/// A frame needs this many meaningful words to be worth keeping (a blank share or a transition
/// has fewer).
const MIN_WORDS: usize = 8;
/// A frame shows something new when more than this share of its cells are on none of the
/// screens shown lately…
const NEW_SHARE: f32 = 0.1;
/// …and more than this many (but at most a quarter of them, so a short slide after another still
/// counts). Fewer are OCR misreadings, a tooltip or a changed value; a new slide, page, dialog or
/// a scrolled-to part of a page brings more.
const NEW_CELLS: usize = 3;
/// Screens on screen within this long before a frame count as shown lately: going back to a page
/// or closing a dialog shows nothing new.
pub const RECENT_MS: u64 = 5 * 60_000;

/// Words that carry content: lower case; with a letter and at least three characters, or a
/// number of at least two digits.
fn words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '-')
        .map(|w| w.trim_matches('-').to_lowercase())
        .filter(|w| {
            let n = w.chars().count();
            let letters = w.chars().any(char::is_alphabetic);
            (letters && n >= 3) || (!letters && w.chars().filter(char::is_ascii_digit).count() >= 2)
        })
        .collect()
}

/// The keys of the cells of `rows` that say something (a word of three letters or more; not a
/// lone number, icon or toggle), without `chrome`.
fn content_cells<'a>(rows: impl Iterator<Item = &'a str>, chrome: &HashSet<String>) -> HashSet<String> {
    rows.flat_map(cells)
        .map(cell_key)
        .filter(|k| !chrome.contains(k) && k.split(|c: char| !c.is_alphabetic()).any(|w| w.chars().count() >= 3))
        .collect()
}

/// Whether `a` is `b` with at most `max` letters changed, added or dropped (OCR misreads small
/// print differently from frame to frame: "Meilenstein", "Mellenstein").
fn near(a: &[char], b: &[char], max: usize) -> bool {
    if a.len().abs_diff(b.len()) > max {
        return false;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + usize::from(ca != cb));
        }
        if cur.iter().min().is_some_and(|m| *m > max) {
            return false;
        }
        prev = cur;
    }
    prev[b.len()] <= max
}

/// Cells already shown, by length in characters, to find a cell or a misreading of it.
struct Shown(HashMap<usize, Vec<Vec<char>>>);

impl Shown {
    fn new<'a>(keys: impl Iterator<Item = &'a String>) -> Self {
        let mut by_len: HashMap<usize, Vec<Vec<char>>> = HashMap::new();
        for k in keys {
            let chars: Vec<char> = k.chars().collect();
            by_len.entry(chars.len()).or_default().push(chars);
        }
        Shown(by_len)
    }

    /// Whether `cell` was shown, allowing a misread character in five.
    fn has(&self, cell: &str) -> bool {
        let chars: Vec<char> = cell.chars().collect();
        let max = (chars.len() / 5).max(1);
        (chars.len().saturating_sub(max)..=chars.len() + max)
            .filter_map(|n| self.0.get(&n))
            .flatten()
            .any(|shown| shown == &chars || near(&chars, shown, max))
    }
}

/// What a new frame of the shared screen shows, against the screens shown lately.
#[derive(Debug, PartialEq, Eq)]
pub enum Seen {
    /// Nothing new: the screen it shows stays on screen longer.
    Same,
    /// Something new worth keeping.
    New,
    /// Too little text to keep (blank, a transition, a video).
    Nothing,
}

/// Compares a frame's rows with the texts of the screens `shown` lately, all without `chrome`.
#[cfg(test)]
fn judge(shown: &[&str], rows: &[String], chrome: &HashSet<String>) -> Seen {
    compare(shown, rows, chrome).0
}

/// `judge`, and for `Same` which of `shown` the frame has most cells in common with.
fn compare(shown: &[&str], rows: &[String], chrome: &HashSet<String>) -> (Seen, Option<usize>) {
    let frame = content_cells(rows.iter().map(String::as_str), chrome);
    let before: Vec<HashSet<String>> = shown.iter().map(|t| content_cells(t.lines(), chrome)).collect();
    if !frame.is_empty() && !before.is_empty() {
        let known = Shown::new(before.iter().flatten());
        let new = frame.iter().filter(|c| !known.has(c)).count();
        if new <= NEW_CELLS.min(frame.len() / 4) || (new as f32) <= NEW_SHARE * frame.len() as f32 {
            // The latest screen (first) wins a tie.
            let closest = (0..before.len()).rev().max_by_key(|&i| before[i].intersection(&frame).count());
            return (Seen::Same, closest);
        }
    }
    let text = rows.iter().flat_map(|r| cells(r)).filter(|c| !chrome.contains(&cell_key(c))).collect::<Vec<_>>().join("\n");
    if words(&text).len() >= MIN_WORDS {
        (Seen::New, None)
    } else {
        (Seen::Nothing, None)
    }
}

/// At most this many distinct screens are kept per meeting (four hours of a slide a minute).
pub const MAX_SCREENS: usize = 240;

/// What a frame of the shared screen, read at meeting time `now_ms`, does to `visuals` (see
/// `judge`, against the latest screen visual and those on screen within `RECENT_MS`, without
/// chrome): `Same` has already made the screen it shows last until `now_ms`; `New` asks the
/// caller to add a screen visual with `rows` (only while fewer than `MAX_SCREENS` are kept);
/// `Nothing` leaves them as they are. The app's screen reading and the command line's
/// `--screen-frames` both go through here.
pub fn observe(visuals: &mut [Visual], rows: &[String], now_ms: u64) -> Seen {
    let chrome = chrome(visuals);
    let screens: Vec<usize> = (0..visuals.len()).filter(|&i| visuals[i].source == Source::Screen).collect();
    // The latest first: the one on screen last (a later one in the list wins a tie).
    let latest = screens.iter().copied().max_by_key(|&i| (visuals[i].last_ms(), i));
    let recent: Vec<usize> = latest
        .into_iter()
        .chain(screens.iter().copied().rev().filter(|&i| Some(i) != latest && visuals[i].last_ms() + RECENT_MS >= now_ms))
        .collect();
    let shown: Vec<&str> = recent.iter().map(|&i| visuals[i].text.as_str()).collect();
    match compare(&shown, rows, &chrome) {
        (Seen::Same, Some(k)) => {
            let v = &mut visuals[recent[k]];
            v.until_ms = Some(now_ms.max(v.last_ms()));
            Seen::Same
        }
        (Seen::New, _) if screens.len() >= MAX_SCREENS => Seen::Nothing,
        (seen, _) => seen,
    }
}

// ---------- prompt lines ----------

/// How much visual text a prompt takes.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    /// Characters per visual at most.
    pub per_visual: usize,
    /// Characters of all visuals together at most…
    pub total: usize,
    /// …and at most this share of the talk's characters (but always room for one visual).
    pub share: f32,
}

/// The notes read the whole meeting: a photo of a printout keeps its order number and positions.
pub const NOTES: Caps = Caps { per_visual: 800, total: 12_000, share: 0.25 };
/// The live questions read the last minutes.
pub const QUESTIONS: Caps = Caps { per_visual: 500, total: 1_500, share: 0.5 };
/// The live map reads what was shown since its last run.
pub const MAP: Caps = Caps { per_visual: 400, total: 1_200, share: 0.5 };
/// Below this, a visual's text says too little: fewer visuals are kept instead.
const MIN_PER_VISUAL: usize = 200;

/// The visual's text for a prompt: rows without chrome joined with " / ", cut at `max` characters.
fn prompt_text(v: &Visual, chrome: &HashSet<String>, max: usize) -> String {
    let text = content_rows(&v.text, chrome).map(|r| squash(&r)).collect::<Vec<_>>().join(" / ");
    if text.chars().count() <= max {
        return text;
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    // End at a word, unless that throws away most of it.
    if let Some(space) = cut.rfind(' ').filter(|i| *i > cut.len() / 2) {
        cut.truncate(space);
    }
    cut.push('…');
    cut
}

/// Prompt lines for `visuals` ("[mm:ss] Screen: …" / "[mm:ss] Photo: …") with their meeting
/// time, within `caps` for a talk of `talk_chars` characters. When they don't all fit, each gets
/// less text (down to `MIN_PER_VISUAL`); below that, photos are kept (the user chose them) and
/// screens are thinned out evenly over the meeting.
pub fn lines(visuals: &[Visual], caps: Caps, talk_chars: usize) -> Vec<(u64, String)> {
    let chrome = chrome(visuals);
    let mut kept: Vec<&Visual> = visuals.iter().filter(|v| !prompt_text(v, &chrome, usize::MAX).is_empty()).collect();
    if kept.is_empty() {
        return Vec::new();
    }
    kept.sort_by_key(|v| (v.at_ms, v.id));
    let allowed = ((talk_chars as f32 * caps.share) as usize).min(caps.total).max(caps.per_visual);
    let fits = (allowed / MIN_PER_VISUAL).max(1);
    if kept.len() > fits {
        let photos = kept.iter().filter(|v| v.source == Source::Photo).count();
        let screens: Vec<&Visual> = kept.iter().copied().filter(|v| v.source == Source::Screen).collect();
        let room = fits.saturating_sub(photos);
        let chosen: HashSet<u32> = (0..room).map(|k| screens[k * screens.len() / room.max(1)].id).collect();
        kept.retain(|v| v.source == Source::Photo || chosen.contains(&v.id));
        kept.truncate(fits);
    }
    let per = (allowed / kept.len()).clamp(MIN_PER_VISUAL.min(caps.per_visual), caps.per_visual);
    kept.iter()
        .map(|v| (v.at_ms, format!("[{}] {}: {}", super::pipeline::clock(v.at_ms), v.source.word(), prompt_text(v, &chrome, per))))
        .collect()
}

/// The talk's lines and the visuals' lines in one list by meeting time; a visual comes before
/// talk at the same time (it was on screen while it was talked about).
pub fn merge(talk: Vec<(u64, String)>, shown: Vec<(u64, String)>) -> Vec<String> {
    let mut all: Vec<(u64, bool, usize, String)> = Vec::with_capacity(talk.len() + shown.len());
    all.extend(shown.into_iter().enumerate().map(|(i, (ms, l))| (ms, false, i, l)));
    all.extend(talk.into_iter().enumerate().map(|(i, (ms, l))| (ms, true, i, l)));
    all.sort_by_key(|(ms, is_talk, i, _)| (*ms, *is_talk, *i));
    all.into_iter().map(|(.., l)| l).collect()
}

/// Whether prompt lines include what was shown ("[03:20] Screen: …" or "[03:20] Photo: …").
pub fn any_shown(lines: &[String]) -> bool {
    lines.iter().any(|l| is_shown(l))
}

/// Whether a prompt line is what was shown, not what was said.
pub fn is_shown(line: &str) -> bool {
    line.split_once("] ").is_some_and(|(_, rest)| rest.starts_with("Screen: ") || rest.starts_with("Photo: "))
}

/// The time of a transcript line as `--meeting` prints it ("[mm:ss] …" or "[h:mm:ss] …").
pub fn line_ms(line: &str) -> Option<u64> {
    let inside = line.strip_prefix('[')?.split_once(']')?.0;
    let parts: Vec<u64> = inside.split(':').map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
    let secs = match parts[..] {
        [m, s] => m * 60 + s,
        [h, m, s] => h * 3600 + m * 60 + s,
        _ => return None,
    };
    Some(secs * 1000)
}

/// Visuals whose time on screen reaches into the last part of the meeting (from `from_ms` on).
pub fn since(visuals: &[Visual], from_ms: u64) -> Vec<Visual> {
    visuals.iter().filter(|v| v.last_ms() >= from_ms).cloned().collect()
}

/// The next visual id: above those of `visuals` and above `last_given`, the highest handed out
/// before (a deleted visual's id isn't reused).
pub fn next_id(visuals: &[Visual], last_given: u32) -> u32 {
    visuals.iter().map(|v| v.id).fold(last_given, u32::max) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ocr(text: &str, x: f32, y: f32, w: f32, h: f32) -> OcrLine {
        OcrLine { text: text.into(), x, y, w, h }
    }

    fn visual(id: u32, at_s: u64, source: Source, text: &str) -> Visual {
        Visual { id, at_ms: at_s * 1000, until_ms: None, source, label: String::new(), text: text.into(), image: None }
    }

    #[test]
    fn table_cells_on_one_line_become_one_row() {
        // A heading, then a table whose cells Vision returns as separate boxes, out of order and
        // slightly skewed.
        let boxes = vec![
            ocr("Gewicht", 0.70, 0.300, 0.15, 0.030),
            ocr("Packliste   PL-0388", 0.10, 0.100, 0.50, 0.040),
            ocr("Pos", 0.10, 0.302, 0.05, 0.028),
            ocr("Artikel", 0.25, 0.298, 0.10, 0.030),
            ocr("1", 0.10, 0.352, 0.02, 0.028),
            ocr("Hydraulikschlauch DN12", 0.25, 0.350, 0.30, 0.030),
            ocr("12 kg", 0.70, 0.355, 0.08, 0.030),
            ocr("   ", 0.5, 0.5, 0.1, 0.03),
        ];
        assert_eq!(
            rows(&boxes),
            ["Packliste PL-0388", "Pos | Artikel | Gewicht", "1 | Hydraulikschlauch DN12 | 12 kg"]
        );
    }

    #[test]
    fn lines_close_together_but_not_overlapping_stay_apart() {
        let boxes = vec![ocr("First line of a paragraph", 0.1, 0.10, 0.6, 0.03), ocr("second line of it", 0.1, 0.132, 0.5, 0.03)];
        assert_eq!(rows(&boxes).len(), 2);
    }

    #[test]
    fn chrome_is_what_repeats_on_most_screens() {
        let mut vs = vec![
            visual(1, 10, Source::Screen, "Teams | Chat | 10:41\nQuarterly numbers\nRevenue up 12 percent"),
            visual(2, 20, Source::Screen, "Teams | Chat | 10:42\nNext steps\nHire two people"),
        ];
        assert!(chrome(&vs).is_empty(), "too few screens to tell");
        vs.push(visual(3, 30, Source::Screen, "Teams | Chat | 10:45\nRisks\nSupplier delays"));
        vs.push(visual(4, 40, Source::Photo, "Teams | Chat | 10:45\nA photo"));
        let c = chrome(&vs);
        assert_eq!(c.len(), 3, "{c:?}");
        assert!(c.contains(&cell_key("Teams")) && c.contains(&cell_key("11:00")));
        let lines = lines(&vs, NOTES, 100_000);
        assert_eq!(lines[0].1, "[00:10] Screen: Quarterly numbers / Revenue up 12 percent");
    }

    #[test]
    fn a_new_slide_is_new_and_the_same_slide_is_same() {
        let slide = "Packlisten prüfen\nGewicht je Karton eintragen\nFehlmengen an das Lager melden";
        let rows_of = |t: &str| t.lines().map(str::to_string).collect::<Vec<_>>();
        let none = HashSet::new();
        assert_eq!(judge(&[], &rows_of(slide), &none), Seen::New);
        assert_eq!(judge(&[slide], &rows_of(slide), &none), Seen::Same);
        // A menu covers the last row: still the same slide.
        assert_eq!(judge(&[slide], &rows_of("Packlisten prüfen\nGewicht je Karton eintragen"), &none), Seen::Same);
        let next = "Tourenplan für Montag\nFahrer früh einteilen\nRampen im System buchen";
        assert_eq!(judge(&[slide], &rows_of(next), &none), Seen::New);
        // A blank share or a transition: nothing to keep.
        assert_eq!(judge(&[slide], &rows_of("Loading …"), &none), Seen::Nothing);
    }

    #[test]
    fn chrome_alone_is_nothing() {
        let chrome: HashSet<String> = [cell_key("File Edit View Window Help Share Record Participants Chat Reactions")].into();
        let rows = vec!["File Edit View Window Help Share Record Participants Chat Reactions".to_string()];
        assert_eq!(judge(&[], &rows, &chrome), Seen::Nothing);
    }

    #[test]
    fn prompt_lines_are_capped_per_visual() {
        let long = (0..200).map(|i| format!("Position {i} Hydraulikschlauch")).collect::<Vec<_>>().join("\n");
        let v = vec![visual(1, 200, Source::Photo, &long)];
        let l = lines(&v, NOTES, 100_000);
        assert_eq!(l.len(), 1);
        assert!(l[0].1.starts_with("[03:20] Photo: Position 0 Hydraulikschlauch / Position 1 "));
        let text = l[0].1.split_once(": ").unwrap().1;
        assert!(text.chars().count() <= NOTES.per_visual && text.ends_with('…'), "{}", text.chars().count());
    }

    #[test]
    fn many_visuals_share_the_total_and_photos_are_kept() {
        let text = "Ein Satz mit genug Wörtern, damit er etwas zählt. ".repeat(20);
        let mut vs: Vec<Visual> = (1..=40).map(|i| visual(i, i as u64 * 60, Source::Screen, &format!("Folie {i}: {text}"))).collect();
        vs.push(visual(41, 1, Source::Photo, &text));
        // A short talk: a quarter of 8 000 characters is room for 10 visuals of 200.
        let l = lines(&vs, NOTES, 8_000);
        assert_eq!(l.len(), 10);
        assert!(l[0].1.starts_with("[00:01] Photo: "));
        let total: usize = l.iter().map(|(_, s)| s.split_once(": ").unwrap().1.chars().count()).sum();
        assert!(total <= 2_000 + 10, "{total}");
        // Screens are spread over the meeting, not just the first ones.
        assert!(l.last().unwrap().0 >= 30 * 60_000);
        // A long talk: all of them, each with less text than alone.
        let all = lines(&vs, NOTES, 1_000_000);
        assert_eq!(all.len(), 41);
        assert!(all.iter().all(|(_, s)| s.chars().count() < 320));
    }

    #[test]
    fn one_visual_always_fits() {
        let v = vec![visual(1, 5, Source::Photo, "Packliste PL-0388")];
        assert_eq!(lines(&v, QUESTIONS, 0), [(5_000, "[00:05] Photo: Packliste PL-0388".to_string())]);
        assert!(lines(&[visual(2, 5, Source::Photo, "  \n ")], NOTES, 1000).is_empty());
    }

    #[test]
    fn merge_puts_a_visual_before_talk_at_the_same_time() {
        let talk = vec![(0, "[00:00] Me: Hallo.".to_string()), (200_000, "[03:20] Anna: Die PLs.".to_string()), (260_000, "[04:20] Me: Gut.".into())];
        let shown = vec![(200_000, "[03:20] Photo: Packliste".to_string())];
        assert_eq!(merge(talk, shown), ["[00:00] Me: Hallo.", "[03:20] Photo: Packliste", "[03:20] Anna: Die PLs.", "[04:20] Me: Gut."]);
    }

    #[test]
    fn tells_lines_with_visuals() {
        assert!(any_shown(&["[00:01] Me: hi".into(), "[03:20] Photo: PL-0388".into()]));
        assert!(!any_shown(&["[00:01] Me: the Screen: is blank".into()]));
    }

    #[test]
    fn reads_line_times() {
        assert_eq!(line_ms("[03:20] Me: x"), Some(200_000));
        assert_eq!(line_ms("[1:02:03] Me: x"), Some(3_723_000));
        assert_eq!(line_ms("3 [00:01] Me"), None);
    }

    #[test]
    fn recent_visuals_include_the_one_still_on_screen() {
        let mut old = visual(1, 10, Source::Screen, "a");
        old.until_ms = Some(500_000);
        let vs = vec![old, visual(2, 20, Source::Photo, "b"), visual(3, 480, Source::Screen, "c")];
        assert_eq!(since(&vs, 400_000).iter().map(|v| v.id).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(next_id(&vs, 0), 4);
        // The newest was deleted: its id isn't handed out again.
        assert_eq!(next_id(&vs[..2], 3), 4);
    }

    #[test]
    fn frames_extend_the_screen_or_add_one() {
        let slide: Vec<String> = vec!["Brontix Overview single source of truth for plant configuration".into()];
        let mut vs = vec![visual(1, 0, Source::Screen, &slide[0]), visual(2, 5_000, Source::Photo, "a printout")];
        assert_eq!(observe(&mut vs, &slide, 10_000), Seen::Same);
        assert_eq!(vs[0].until_ms, Some(10_000));
        let other: Vec<String> = vec!["Admin area schemas templates prefills rules macros sheets folders".into()];
        assert_eq!(observe(&mut vs, &other, 20_000), Seen::New);
        assert_eq!(observe(&mut vs, &["ok".to_string()], 30_000), Seen::Nothing);
        // Once MAX_SCREENS are kept, a new screen is not added.
        let mut full: Vec<Visual> = (0..MAX_SCREENS as u32).map(|i| visual(i + 1, i as u64, Source::Screen, &slide[0])).collect();
        assert_eq!(observe(&mut full, &other, 1_000_000), Seen::Nothing);
    }

    #[test]
    fn old_meetings_without_visuals_load() {
        let v: Visual = serde_json::from_str(r#"{"id":1,"atMs":5,"source":"photo","text":"x"}"#).unwrap();
        assert_eq!((v.label.as_str(), v.image, v.until_ms), ("", None, None));
    }
}
