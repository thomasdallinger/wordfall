//! Live questions: while a meeting is recorded, the notes model reads the last minutes of the
//! transcript and proposes questions "Me" could ask next, steered by the meeting's goal if one was
//! given. At most `MAX_LIVE` are open at a time; each run also says which open ones the talk has
//! answered since. A question nobody acts on expires after `LIVE_FOR_MS`; one kept for Later stays
//! until the meeting ends, and the notes list what was left open.
//!
//! Left to itself, the model asks about whatever is on screen right now, asks it again a few
//! minutes later in other words, and asks what a misheard word means. So every
//! `BIG_PICTURE_EVERY_MS` one run asks for a question about the goal and the big picture instead;
//! the meeting's names as written on screen come with the prompt; a new question that asks much the
//! same as an earlier one, or what a word nobody showed means, is left out (`apply`, `misheard`).
//!
//! A run reads the transcript before anything that changes from run to run, from a start that
//! stays put for several runs (`window_start`), so the helper reads only what was said since.
//!
//! This module is the pure part (prompt, answer format, rules); the meeting thread that runs it
//! lives in `mod.rs`.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use super::{terms::Term, visuals, Segment, Speaker, Visual};
use crate::llm;

/// Open questions shown at a time.
pub const MAX_LIVE: usize = 3;
/// New questions one run may add.
const MAX_NEW_PER_RUN: usize = 2;
/// A live question nobody pressed anything on expires after this much meeting time.
pub const LIVE_FOR_MS: u64 = 5 * 60_000;
/// The transcript a run reads: the last minutes, at most this many characters…
const RECENT_MS: u64 = 8 * 60_000;
const RECENT_CHARS: usize = 10_000;
/// …from a start on this grid, so it stays the same for several runs (see `window_start`).
const WINDOW_STEP_MS: u64 = 4 * 60_000;
/// What was on screen: the latest screens of the transcript's minutes, this many characters…
const SCREEN_CHARS: usize = visuals::QUESTIONS.total;
/// …each cut as `visuals::QUESTIONS` says, none left out before the latest are taken
/// (`visuals::lines` takes out the window around them, from all the screens of those minutes).
const SCREEN_CAPS: visuals::Caps = visuals::Caps { total: 12_000, share: 1.0, ..visuals::QUESTIONS };
/// Names of the meeting the prompt lists at most.
const NAMES_MAX: usize = 30;
/// Earlier questions the prompt lists: the open ones, the big-picture ones, and those suggested
/// in the last minutes, at most this many (older repeats are caught by `similar`).
const EARLIER_MS: u64 = 20 * 60_000;
const EARLIER_MAX: usize = 12;
/// A question about the goal and the big picture is asked for once the meeting has gone on this
/// long, and again when the last one is this old.
const BIG_PICTURE_FROM_MS: u64 = 5 * 60_000;
const BIG_PICTURE_EVERY_MS: u64 = 10 * 60_000;
/// Questions longer than this are cut (the grammar keeps answers to one line each).
const TEXT_MAX: usize = 240;
/// Context the questions worker asks for: the recent transcript and a short answer fit easily.
pub const CTX: u32 = 8192;
const ANSWER_TOKENS: u32 = 220;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Something said is vague, ambiguous or unexplained.
    Understand,
    /// A claim, assumption, number or plan that should be tested.
    Challenge,
    /// Something left open (owner, date, number, next step), or worth going deeper on.
    FollowUp,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Understand => "understand",
            Kind::Challenge => "challenge",
            Kind::FollowUp => "follow-up",
        }
    }

    fn from_word(w: &str) -> Option<Self> {
        match w.trim() {
            "understand" => Some(Kind::Understand),
            "challenge" => Some(Kind::Challenge),
            "follow-up" => Some(Kind::FollowUp),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    /// In the stack, waiting for the user.
    Live,
    /// Kept for later by the user.
    Later,
    /// The user asked it.
    Asked,
    /// The talk answered it.
    Answered,
    /// Nobody acted on it in time.
    Expired,
    /// The user deleted it.
    Deleted,
}

impl State {
    pub fn is_open(self) -> bool {
        matches!(self, State::Live | State::Later)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    /// 1, 2, … within the meeting, in the order proposed (shown and referred to as Q1, Q2, …).
    pub number: u32,
    pub text: String,
    pub kind: Kind,
    pub state: State,
    /// Meeting time it was proposed at.
    pub at_ms: u64,
    /// Meeting time of its last change of state.
    pub changed_ms: u64,
    /// Asked for as the question about the goal and the big picture (see `big_picture_due`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub big_picture: bool,
}

/// What the user does to a question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Act {
    Later,
    Asked,
    Deleted,
}

/// Applies the user's `act` to question `number`. Errors explain why it can't be done.
pub fn act(questions: &mut [Question], number: u32, act: Act, now_ms: u64) -> anyhow::Result<()> {
    let q = questions.iter_mut().find(|q| q.number == number).ok_or_else(|| anyhow::anyhow!("That question is gone."))?;
    anyhow::ensure!(q.state.is_open(), "That question is no longer open.");
    q.state = match act {
        Act::Later => State::Later,
        Act::Asked => State::Asked,
        Act::Deleted => State::Deleted,
    };
    q.changed_ms = now_ms;
    Ok(())
}

/// Expires live questions that have waited too long. Returns whether any did.
pub fn expire(questions: &mut [Question], now_ms: u64) -> bool {
    let mut changed = false;
    for q in questions.iter_mut().filter(|q| q.state == State::Live && now_ms.saturating_sub(q.at_ms) >= LIVE_FOR_MS) {
        q.state = State::Expired;
        q.changed_ms = now_ms;
        changed = true;
    }
    changed
}

pub fn live_count(questions: &[Question]) -> usize {
    questions.iter().filter(|q| q.state == State::Live).count()
}

/// A question one run proposes.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub kind: Kind,
    pub text: String,
    /// Written as the question about the goal and the big picture.
    pub big_picture: bool,
}

/// What one run of the model said.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// Questions the talk answered (checked against the transcript).
    pub answered: Vec<u32>,
    pub new: Vec<Proposal>,
    /// Proposed, but left out: they ask what a word means that was likely misheard (`misheard`).
    pub misheard: Vec<String>,
}

/// The model's answer as written: answered questions with the words it says answer them.
#[derive(Debug, Default, PartialEq)]
struct Parsed {
    answered: Vec<(u32, String)>,
    new: Vec<Proposal>,
}

/// What `apply` did.
#[derive(Debug, Default, PartialEq)]
pub struct Applied {
    pub changed: bool,
    /// New questions left out as asking much the same as an earlier one: (its number, the text).
    pub repeats: Vec<(u32, String)>,
}

/// Applies a run: marks answered questions, adds new ones that don't ask what an earlier one asked
/// (whatever became of it) while there's room.
pub fn apply(questions: &mut Vec<Question>, outcome: Outcome, now_ms: u64) -> Applied {
    let mut applied = Applied::default();
    for q in questions.iter_mut().filter(|q| q.state.is_open() && outcome.answered.contains(&q.number)) {
        q.state = State::Answered;
        q.changed_ms = now_ms;
        applied.changed = true;
    }
    for p in outcome.new {
        if live_count(questions) >= MAX_LIVE {
            break;
        }
        if let Some(earlier) = questions.iter().find(|q| same_question(&q.text, &p.text)) {
            applied.repeats.push((earlier.number, p.text));
            continue;
        }
        let number = questions.iter().map(|q| q.number).max().unwrap_or(0) + 1;
        questions.push(Question {
            number,
            text: p.text,
            kind: p.kind,
            state: State::Live,
            at_ms: now_ms,
            changed_ms: now_ms,
            big_picture: p.big_picture,
        });
        applied.changed = true;
    }
    applied
}

/// The words that carry a question's meaning (lower case, short words left out).
fn content_words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 3)
        .map(str::to_lowercase)
        .collect()
}

/// Whether two short texts say much the same (most of their content words are shared). The
/// system map compares its facts and flows with it; questions use `same_question`.
pub(super) fn similar(a: &str, b: &str) -> bool {
    let (a, b) = (content_words(a), content_words(b));
    if a.is_empty() || b.is_empty() {
        return a == b;
    }
    let shared = a.intersection(&b).count() as f32;
    shared / a.len().min(b.len()) as f32 >= 0.7
}

/// Words of a question that say how it asks, not what about ("what exactly does … mean in the
/// context of"), in the languages questions are mostly written in; with the words of three
/// letters or fewer, they are left out when questions are compared.
const FILLER: &[&str] = &[
    // German
    "aber", "alle", "also", "auch", "bedeuten", "bedeutet", "beim", "dabei", "dafür", "damit", "dann", "dass", "denn", "diese",
    "diesem", "diesen", "dieser", "dieses", "doch", "durch", "eigentlich", "eine", "einem", "einen", "einer", "eines", "etwa",
    "genau", "gibt", "haben", "heißt", "hier", "jetzt", "jeweils", "kann", "kein", "keine", "kontext", "konkret", "können",
    "könnte", "mehr", "muss", "müssen", "nach", "noch", "nicht", "oder", "ohne", "praxis", "rolle", "schon", "sein", "sich",
    "sind", "soll", "sollen", "sollte", "spielt", "tatsächlich", "über", "unter", "warum", "welche", "welchem", "welchen",
    "welcher", "welches", "wenn", "werden", "wieder", "wird", "wirklich", "wurde", "zwischen", "detail", "details",
    // English
    "about", "actually", "been", "does", "each", "exactly", "from", "have", "into", "just", "mean", "means", "more", "most",
    "much", "only", "other", "play", "plays", "really", "role", "same", "should", "some", "than", "that", "their", "them",
    "then", "there", "these", "they", "this", "what", "when", "where", "which", "while", "will", "with", "would", "context",
];

/// The words a question is about: lower case, each once, without short words and `FILLER`; a
/// number of two digits or more counts ("Was bedeutet 29 %?" is about the 29).
fn topic_words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in text.split(|c: char| !c.is_alphanumeric()).map(str::to_lowercase) {
        let n = w.chars().count();
        let number = w.chars().all(|c| c.is_ascii_digit());
        let topic = if number { n >= 2 } else { n > 3 && !FILLER.contains(&w.as_str()) };
        if topic && !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

/// Whether two words are the same word in another form: one starts the other ("Feld",
/// "Feldern"), ends it as a compound does ("System", "Testsystem"), or they share most of a long
/// start ("aktualisiert", "Aktualisierung").
fn same_word(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (short, long) = if a.len() <= b.len() { (&a, &b) } else { (&b, &a) };
    let common = short.iter().zip(long.iter()).take_while(|(x, y)| x == y).count();
    a == b
        || (short.len() >= 4 && common == short.len())
        || (short.len() >= 5 && long.ends_with(short))
        || (short.len() >= 5 && common >= 5 && common * 5 >= short.len() * 4)
}

/// Whether a new question asks much the same as an earlier one: at least two of the shorter one's
/// topic words, and half of them, are in the other (in some form, see `same_word`). The model asks
/// a topic again in other words while it is still being talked about, often the same start with
/// another ending ("Warum wird X so erstellt, und wie …?" / "Warum wird X so erstellt, wenn …?").
pub fn same_question(a: &str, b: &str) -> bool {
    let (a, b) = (topic_words(a), topic_words(b));
    let (short, long) = if a.len() <= b.len() { (&a, &b) } else { (&b, &a) };
    if short.is_empty() {
        return false;
    }
    let shared = short.iter().filter(|w| long.iter().any(|l| same_word(w, l))).count();
    shared >= 2.min(short.len()) && shared * 2 >= short.len()
}

/// The term a question asks the meaning of: what an "understand" question puts in quotes
/// ("Was bedeutet „Togel“ …?").
fn quoted_term(p: &Proposal) -> Option<&str> {
    if p.kind != Kind::Understand {
        return None;
    }
    let open = p.text.find(['"', '„', '“', '”', '«', '»', '‚', '‘'])?;
    let rest = &p.text[open..];
    let first = rest.chars().next()?;
    let rest = &rest[first.len_utf8()..];
    let close = rest.find(['"', '“', '”', '«', '»', '‘', '’'])?;
    let term = rest[..close].trim();
    (!term.is_empty() && term.split_whitespace().count() <= 4).then_some(term)
}

/// Lowercase words of `text`.
fn word_set(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// Whether a proposed question asks what a word means that is likely a mishearing: when something
/// was shown in the meeting, a word on none of the screens or photos (nor in the goal or the
/// notes); without anything shown, one said only once. Speech recognition hears a name or an
/// uncommon word as some other word, and the same way each time it's said; what was on screen
/// is spelled right. A quoted fragment of one or two letters ("S" read off a screen) isn't worth
/// asking about either.
fn misheard(p: &Proposal, input: &Input) -> bool {
    let Some(term) = quoted_term(p) else { return false };
    if term.chars().filter(|c| c.is_alphanumeric()).count() < 3 {
        return true;
    }
    let words = word_set(term);
    if words.is_empty() {
        return false;
    }
    // In some form: "Wing" is on a screen that says "Wings".
    let known = |text: &str| {
        let set = word_set(text);
        words.iter().all(|w| set.contains(w) || set.iter().any(|x| same_word(w, x)))
    };
    if known(input.goal) || known(input.notes) {
        return false;
    }
    if !input.visuals.is_empty() {
        return !input.visuals.iter().any(|v| known(&v.text));
    }
    input.segments.iter().filter(|s| known(&s.text)).count() < 2
}

pub struct Input<'a> {
    pub goal: &'a str,
    pub notes: &'a str,
    pub language: Option<&'a str>,
    pub segments: &'a [Segment],
    pub speakers: &'a [Speaker],
    /// What was shown (screens, photos): the latest come after the transcript.
    pub visuals: &'a [Visual],
    pub questions: &'a [Question],
    /// What the system map still doesn't know ("Name: what"); questions go after it first.
    pub unknowns: &'a [String],
    /// The meeting's names and terms as written (`meeting_vocabulary`): those shown, typed or said
    /// are listed, so questions spell them so and see through their mishearings.
    pub vocabulary: &'a [Term],
    pub now_ms: u64,
}

/// Where the transcript a run reads starts: `RECENT_MS` back, moved back onto the
/// `WINDOW_STEP_MS` grid. It then stays put for several runs, and as the helper keeps what it
/// read of a prompt that starts the same, a run reads only what was said since the last one
/// (and what comes after the transcript), not all the minutes again.
fn window_start(now_ms: u64) -> u64 {
    now_ms.saturating_sub(RECENT_MS) / WINDOW_STEP_MS * WINDOW_STEP_MS
}

/// "[mm:ss] Speaker: text" lines of the last minutes of the talk (at most `RECENT_CHARS`), and the
/// meeting time they start at. Too long, they start a step later; only if that's still too long
/// are they cut at a line.
fn talk_lines(input: &Input) -> (u64, String) {
    let mut from = window_start(input.now_ms);
    loop {
        let recent: Vec<Segment> = input.segments.iter().filter(|s| s.end_ms >= from).cloned().collect();
        let lines = super::pipeline::transcript_lines(&recent, input.speakers).join("\n");
        let Some((cut, _)) = lines.char_indices().rev().nth(RECENT_CHARS) else { return (from, lines) };
        if from + WINDOW_STEP_MS <= input.now_ms.saturating_sub(WINDOW_STEP_MS) {
            from += WINDOW_STEP_MS;
            continue;
        }
        // Keep the end, from the start of a line.
        return (from, lines[cut..].split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_default());
    }
}

/// "[mm:ss] Screen: …" / "[mm:ss] Photo: …" lines of the latest of what was shown since `from_ms`,
/// `SCREEN_CHARS` at most (the latest at least), in the order shown.
fn screen_lines(input: &Input, from_ms: u64) -> Vec<String> {
    let shown = visuals::since(input.visuals, from_ms);
    let all = visuals::lines(&shown, SCREEN_CAPS, SCREEN_CAPS.total);
    let mut kept: Vec<String> = Vec::new();
    let mut chars = 0;
    for (_, line) in all.into_iter().rev() {
        chars += line.chars().count();
        if !kept.is_empty() && chars > SCREEN_CHARS {
            break;
        }
        kept.push(line);
    }
    kept.reverse();
    kept
}

/// The meeting's names to list: those of the vocabulary that were shown, typed or said (a word
/// the user listed for other meetings isn't), "Term (= meaning)", at most `NAMES_MAX`.
fn names(input: &Input, talk: &str) -> Vec<String> {
    let mut seen = word_set(talk);
    seen.extend(word_set(input.goal));
    seen.extend(word_set(input.notes));
    for v in input.visuals {
        seen.extend(word_set(&v.text));
    }
    input
        .vocabulary
        .iter()
        .filter(|t| {
            let words = word_set(&t.text);
            !words.is_empty() && words.iter().all(|w| seen.contains(w))
        })
        .take(NAMES_MAX)
        .map(|t| match &t.meaning {
            Some(m) => format!("{} (= {m})", t.text),
            None => t.text.clone(),
        })
        .collect()
}

/// How many new questions a run may propose now.
pub fn room(questions: &[Question]) -> usize {
    MAX_LIVE.saturating_sub(live_count(questions)).min(MAX_NEW_PER_RUN)
}

/// Whether a run should ask for a question about the goal and the big picture: the meeting has
/// gone on `BIG_PICTURE_FROM_MS` and none was suggested in the last `BIG_PICTURE_EVERY_MS`.
/// Without it, nearly every question is about the detail on screen at the moment.
pub fn big_picture_due(questions: &[Question], now_ms: u64) -> bool {
    now_ms >= BIG_PICTURE_FROM_MS && !questions.iter().any(|q| q.big_picture && now_ms.saturating_sub(q.at_ms) < BIG_PICTURE_EVERY_MS)
}

/// The instructions: the same in every run of a meeting (once something was shown), so the helper
/// keeps them with the transcript.
fn system_prompt(language: &str, goal: bool, shown: bool) -> String {
    let aim = if goal { "moves the meeting towards its goal" } else { "moves the meeting forward" };
    let shown = if shown { llm::SHOWN } else { "" };
    format!(
        "You listen to a meeting together with \"Me\", the person who recorded it, and suggest questions Me could \
         ask next. A good question is short (at most 20 words), specific to what was just said, and {aim}: it asks \
         what Me needs to understand (what something is for, who uses it or decides, why it is so, what happens \
         before and after it, what is still open) rather than details Me can see on screen (where to click, how \
         exactly a view works). Never suggest a question the transcript already answers, one Me already asked, or \
         one about the same thing as a question suggested before, even in other words. Ask from a new angle each \
         time: when questions before already asked who is responsible for something or how something is ensured, \
         ask what something is for, why it is so, or what happens before and after it instead. The transcript comes from \
         speech recognition and has mistakes, in names most of all: a word that makes no sense where it stands, or \
         sounds like one of the meeting's names, is that name or a common word misheard. Write names as the \
         meeting's names are written, and never ask what a word means unless it was shown or written there.{shown} \
         Translate words from the screen in the sense of the meeting's subject (in engineering, a \"plant\" is an \
         installation, not a flower). Write the questions in {language}, addressed to the others in the meeting."
    )
}

/// The request: what stays the same from run to run first (the goal, the notes, the transcript,
/// which only grows until its start moves on), then what changes every time (the latest screens,
/// the names, the questions so far and what to do).
fn user_prompt(input: &Input, transcript: &str, screens: &[String], room: usize, big_picture: bool) -> String {
    let mut p = String::new();
    let goal = input.goal.trim();
    if !goal.is_empty() {
        p.push_str(&format!("Goal of the meeting: {goal}\n\n"));
    }
    let notes: String = input.notes.trim().chars().take(1200).collect();
    if !notes.is_empty() {
        p.push_str(&format!("My own notes so far:\n{notes}\n\n"));
    }
    p.push_str(&format!("The last minutes of the meeting:\n<transcript>\n{transcript}\n</transcript>\n\n"));
    if !screens.is_empty() {
        p.push_str(&format!("Shown lately:\n{}\n\n", screens.join("\n")));
    }
    let names = names(input, transcript);
    if !names.is_empty() {
        p.push_str(&format!("The meeting's names, as written: {}\n\n", names.join(", ")));
    }
    if !input.unknowns.is_empty() {
        p.push_str("Still unknown about the systems and people involved (good questions find these out):\n");
        for u in input.unknowns.iter().take(8) {
            p.push_str(&format!("- {u}\n"));
        }
        p.push('\n');
    }
    let earlier: Vec<&Question> = input
        .questions
        .iter()
        .filter(|q| !q.state.is_open() && (q.big_picture || input.now_ms.saturating_sub(q.at_ms) < EARLIER_MS))
        .collect();
    if !earlier.is_empty() {
        p.push_str("Questions suggested before that are closed (never suggest these again, nor the same in other words):\n");
        for q in &earlier[earlier.len().saturating_sub(EARLIER_MAX)..] {
            p.push_str(&format!("- {}\n", q.text));
        }
        p.push('\n');
    }
    let open: Vec<&Question> = input.questions.iter().filter(|q| q.state.is_open()).collect();
    if !open.is_empty() {
        p.push_str("Questions suggested before that are still open (don't suggest these again either):\n");
        for q in &open {
            p.push_str(&format!("Q{} ({}, suggested at {}): {}\n", q.number, q.kind.word(), super::pipeline::clock(q.at_ms), q.text));
        }
        p.push('\n');
        p.push_str(
            "First, for each open question, one line saying whether someone in the transcript answered it after it \
             was suggested: \"Q2: yes\" followed by the words from the transcript that answer it, copied exactly, \
             for example Q2: yes \"we launch on the first of March\"; otherwise \"Q2: no\".\n",
        );
    }
    let then = if open.is_empty() { "Suggest" } else { "Then suggest" };
    let kinds = "understand: something said is vague, ambiguous or unexplained.\n\
                 challenge: a claim, assumption, number or plan that should be tested.\n\
                 follow-up: something left open (no owner, date, number or next step), or a topic worth going deeper on.\n";
    if room == 0 {
        p.push_str("Then stop: suggest no new question.");
    } else if big_picture {
        let purpose = if goal.is_empty() { "purpose" } else { "goal" };
        p.push_str(&format!(
            "{then} one question about the {purpose} of the meeting and the big picture, not about what is on screen \
             now: who works with what is discussed and in which roles, what it replaced and why, how it came about, \
             what happens before and after it, or what comes next. Write it on a line that starts with \"big \
             picture, \" and its kind, for example \"big picture, understand: …?\"."
        ));
        if room > 1 {
            p.push_str(&format!(
                " After it, at most {} more new question{s} Me could ask about what was just said, one per line, each \
                 starting with its kind (only when there is a good one).",
                room - 1,
                s = if room == 2 { "" } else { "s" }
            ));
        }
        p.push_str(&format!(" The kinds:\n{kinds}"));
    } else {
        p.push_str(&format!(
            "{then} at most {room} new question{s} Me could ask now, one per line, each starting with its kind:\n{kinds}\
             Only suggest a question when there is a good one. None is fine, especially when little was said.",
            s = if room == 1 { "" } else { "s" }
        ));
    }
    p
}

/// The answer's form: a verdict line per open question ("Q2: no", or "Q2: yes" with the words that
/// answer it), then up to `room` "kind: question?" lines, the first of them a "big picture, kind:
/// question?" line when one is due. A verdict for each question makes a small model weigh each one
/// (offered a single "none", it took that every time); each question ends at its question mark, so
/// it can't ramble on after it.
fn grammar(open: &[u32], room: usize, big_picture: bool) -> String {
    let verdicts: Vec<String> = open
        .iter()
        .map(|n| format!("\"Q{n}: \" (\"no\" | \"yes \\\"\" [^\"\\n]+ \"\\\"\") \"\\n\""))
        .collect();
    let verdicts = if verdicts.is_empty() { String::new() } else { format!("{} ", verdicts.join(" ")) };
    let questions = if big_picture && room > 0 { format!("big question{{0,{}}}", room - 1) } else { format!("question{{0,{room}}}") };
    format!(
        "root ::= {verdicts}{questions}\n\
         big ::= \"big picture, \" question\n\
         question ::= kind \": \" [^\\n?]+ \"?\\n\"\n\
         kind ::= \"understand\" | \"challenge\" | \"follow-up\"\n"
    )
}

/// Reads the model's answer (see `grammar`). Lenient about what the grammar can't rule out.
fn parse(answer: &str) -> Parsed {
    let mut out = Parsed::default();
    for line in answer.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(rest) = line.strip_prefix('Q') {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let verdict = rest[digits.len()..].trim_start_matches(':').trim();
            if let (Ok(n), Some(quote)) = (digits.parse(), verdict.strip_prefix("yes")) {
                out.answered.push((n, quote.trim().trim_matches('"').trim().to_string()));
            }
            continue;
        }
        let (line, big_picture) = match line.strip_prefix("big picture,") {
            Some(rest) => (rest.trim_start(), true),
            None => (line, false),
        };
        let Some((kind, text)) = line.split_once(':') else { continue };
        let Some(kind) = Kind::from_word(kind) else { continue };
        let text = text.trim().trim_matches(['"', '“', '”', '„']).trim();
        if text.split_whitespace().count() < 3 {
            continue;
        }
        let mut cut: String = text.chars().take(TEXT_MAX).collect();
        if cut.len() < text.len() {
            cut.push('…');
        }
        out.new.push(Proposal { kind, text: cut, big_picture });
    }
    out
}

/// Words for comparing a quote with the transcript: lower case, letters and digits only.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// Keeps the answered questions whose quote (at least three words) was said and ends in talk from
/// after the question was proposed: a small model otherwise calls questions answered that weren't,
/// or quotes what the question itself was about.
fn verified(parsed: &[(u32, String)], input: &Input) -> Vec<u32> {
    // Every word each speaker said, with when its segment started. Per speaker, because a quote
    // often runs on over someone else's "right" in between.
    let mut sorted: Vec<&Segment> = input.segments.iter().collect();
    sorted.sort_by_key(|s| s.start_ms);
    let mut speakers: Vec<u32> = sorted.iter().map(|s| s.speaker).collect();
    speakers.sort_unstable();
    speakers.dedup();
    let streams: Vec<Vec<(String, u64)>> = speakers
        .iter()
        .map(|sp| {
            sorted.iter().filter(|s| s.speaker == *sp).flat_map(|s| words(&s.text).into_iter().map(|w| (w, s.start_ms))).collect()
        })
        .collect();
    parsed
        .iter()
        .filter(|(n, quote)| {
            let Some(q) = input.questions.iter().find(|q| q.number == *n && q.state.is_open()) else { return false };
            let quote = words(quote);
            quote.len() >= 3
                && streams.iter().filter(|said| said.len() >= quote.len()).any(|said| {
                    said.windows(quote.len()).any(|w| w.iter().zip(&quote).all(|((a, _), b)| a == b) && w[w.len() - 1].1 >= q.at_ms)
                })
        })
        .map(|(n, _)| *n)
        .collect()
}

/// The names of the meeting's vocabulary that a screen or photo shows.
fn shown_names(input: &Input) -> Vec<Term> {
    let screens: Vec<HashSet<String>> = input.visuals.iter().map(|v| word_set(&v.text)).collect();
    input
        .vocabulary
        .iter()
        .filter(|t| {
            let words = word_set(&t.text);
            !words.is_empty() && screens.iter().any(|s| words.iter().all(|w| s.contains(w)))
        })
        .cloned()
        .collect()
}

/// How often `words` (lower case) come in a row in `text`, as whole words.
fn uses_of(text: &str, words: &[String]) -> usize {
    let all: Vec<String> = text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
    if words.is_empty() || all.len() < words.len() {
        return 0;
    }
    all.windows(words.len()).filter(|w| w == &words).count()
}

/// A proposed question with the names the screens show (`shown`) in their shown spelling where
/// it has what speech recognition heard for them ("das eCard-Modul" → "das ECAD-Modul"): the live
/// transcript is corrected only after the meeting (the term check), and the model copies what it
/// reads. Only heard words the transcript has at least twice (a misheard name comes back the same
/// way) and no screen shows itself; the sound-alikes are the term check's (`terms::Matcher`).
fn respell(text: &str, input: &Input, shown: &[Term]) -> String {
    if shown.is_empty() {
        return text.to_string();
    }
    let found = super::terms::Matcher::new(shown).find(text);
    let heard_words = |c: &super::terms::Candidate| -> Vec<String> { c.heard.split(|ch: char| !ch.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect() };
    let accepted: Vec<&super::terms::Candidate> = found
        .iter()
        .filter(|c| {
            let words = heard_words(c);
            let said: usize = input.segments.iter().map(|s| uses_of(&s.text, &words)).sum();
            said >= 2 && !input.visuals.iter().any(|v| uses_of(&v.text, &words) > 0)
        })
        .collect();
    super::terms::apply(text, &accepted).0
}

/// One run: asks the model about the recent transcript. `None` when there's nothing to read.
pub fn run(worker: &mut llm::Worker, input: &Input) -> anyhow::Result<Option<Outcome>> {
    let (from, transcript) = talk_lines(input);
    if transcript.trim().is_empty() {
        return Ok(None);
    }
    let screens = screen_lines(input, from);
    let room = room(input.questions);
    let big_picture = room > 0 && big_picture_due(input.questions, input.now_ms);
    let open: Vec<u32> = input.questions.iter().filter(|q| q.state.is_open()).map(|q| q.number).collect();
    let language = llm::language_name(input.language);
    // Told about screens once anything was shown, not only while some are listed: the instructions
    // stay the same, so the helper keeps them.
    let system = system_prompt(language, !input.goal.trim().is_empty(), !input.visuals.is_empty());
    let grammar = grammar(&open, room, big_picture);
    let user = user_prompt(input, &transcript, &screens, room, big_picture);
    let answer = worker.chat_at(&system, &user, ANSWER_TOKENS, Some(&grammar), None, Some("questions"), |_| {})?;
    let parsed = parse(&answer);
    let answered = verified(&parsed.answered, input);
    for (n, quote) in parsed.answered.iter().filter(|(n, _)| !answered.contains(n)) {
        log::info!("meeting questions: Q{n} \"{quote}\" was not said after the question; still open");
    }
    let shown = shown_names(input);
    let respelled = parsed.new.into_iter().map(|p| Proposal { text: respell(&p.text, input, &shown), ..p });
    let (misheard, new): (Vec<Proposal>, Vec<Proposal>) = respelled.partition(|p| misheard(p, input));
    Ok(Some(Outcome { answered, new, misheard: misheard.into_iter().map(|p| p.text).collect() }))
}

/// The questions left open at the end, for the notes: "## Open questions" and a line each.
pub fn open_section(questions: &[Question], heading: &str) -> Option<String> {
    let open: Vec<&Question> = questions.iter().filter(|q| q.state.is_open()).collect();
    if open.is_empty() {
        return None;
    }
    let mut s = format!("## {heading}\n");
    for q in open {
        s.push_str(&format!("- {}\n", q.text));
    }
    Some(s.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::visuals::Source;

    fn q(number: u32, text: &str, state: State, at_ms: u64) -> Question {
        Question { number, text: text.into(), kind: Kind::Understand, state, at_ms, changed_ms: at_ms, big_picture: false }
    }

    fn seg(start_ms: u64, text: &str) -> Segment {
        Segment { start_ms, end_ms: start_ms + 3000, channel: super::super::Channel::System, speaker: 1, text: text.into(), language: None, fixes: Vec::new() }
    }

    fn screen(id: u32, at_s: u64, until_s: Option<u64>, text: &str) -> Visual {
        Visual { id, at_ms: at_s * 1000, until_ms: until_s.map(|u| u * 1000), source: Source::Screen, label: String::new(), text: text.into(), image: None }
    }

    fn input<'a>(segments: &'a [Segment], visuals: &'a [Visual], questions: &'a [Question], vocabulary: &'a [Term], now_ms: u64) -> Input<'a> {
        Input { goal: "", notes: "", language: None, segments, speakers: &[], visuals, questions, unknowns: &[], vocabulary, now_ms }
    }

    fn proposal(kind: Kind, text: &str) -> Proposal {
        Proposal { kind, text: text.into(), big_picture: false }
    }

    #[test]
    fn names_heard_otherwise_take_the_screens_spelling() {
        let segments = vec![seg(10_000, "Das eCard exportiert ein XML."), seg(20_000, "Im eCard sind Makros."), seg(30_000, "Er trägt ein Dress.")];
        let visuals = vec![screen(1, 5, Some(40), "ECAD Modul | Makros | Doresa")];
        let vocabulary = vec![Term { text: "ECAD".into(), meaning: None }, Term { text: "Doresa".into(), meaning: None }];
        let i = input(&segments, &visuals, &[], &vocabulary, 60_000);
        let shown = shown_names(&i);
        assert_eq!(respell("Was macht das eCard-Modul mit den Makros?", &i, &shown), "Was macht das ECAD-Modul mit den Makros?");
        // Heard once: maybe just a word.
        assert_eq!(respell("Warum das Dress?", &i, &shown), "Warum das Dress?");
    }

    #[test]
    fn parses_the_answer() {
        let out = parse(
            "Q2: yes \"we launch in March\"\nQ3: no\nQ5: yes \"Anna does it\"\nbig picture, understand: Who will work with the new tool every day?\nchallenge: Why twelve euros and not ten?\nfollow-up: Wer übernimmt das Onboarding bis Freitag?\n",
        );
        assert_eq!(out.answered, vec![(2, "we launch in March".into()), (5, "Anna does it".into())]);
        assert_eq!(
            out.new,
            vec![
                Proposal { kind: Kind::Understand, text: "Who will work with the new tool every day?".into(), big_picture: true },
                proposal(Kind::Challenge, "Why twelve euros and not ten?"),
                proposal(Kind::FollowUp, "Wer übernimmt das Onboarding bis Freitag?"),
            ]
        );
        assert_eq!(parse("Q1: no\n"), Parsed::default());
        // Too short to be a question, or not a kind.
        assert!(parse("understand: Why?\nnote: something here to say\n").new.is_empty());
    }

    #[test]
    fn grammar_lists_as_many_questions_as_there_is_room_for() {
        let g = grammar(&[2, 5], 1, false);
        assert!(g.contains("question{0,1}") && g.contains("\"Q2: \"") && g.contains("\"Q5: \""));
        assert!(grammar(&[], 2, false).starts_with("root ::= question{0,2}\n"));
        // The big-picture question first, and it is not optional.
        assert!(grammar(&[], 2, true).starts_with("root ::= big question{0,1}\n"));
        assert!(grammar(&[3], 0, true).contains("\"Q3: \"") && grammar(&[3], 0, true).contains("question{0,0}"));
    }

    #[test]
    fn an_answer_counts_only_if_its_words_were_said_after_the_question() {
        let mut right = seg(68_000, "Right.");
        right.speaker = 0;
        let segments = vec![seg(10_000, "The launch is on the first of March."), right, seg(70_000, "Anna will do the onboarding, yes.")];
        let questions = vec![q(1, "When is the launch?", State::Live, 60_000), q(2, "Who does the onboarding?", State::Live, 60_000)];
        let input = input(&segments, &[], &questions, &[], 80_000);
        let claimed = vec![
            (1, "the first of March".to_string()),                   // said, but before the question
            (2, "Anna will do the onboarding".to_string()),          // said after it
            (2, "yes".to_string()),                                  // too short to tell
            (2, "Anna will do the marketing".to_string()),           // never said
            (1, "first of March. Anna will do the".to_string()),     // runs into talk after it, over "Right." in between
        ];
        assert_eq!(verified(&claimed, &input), vec![2, 1]);
    }

    #[test]
    fn the_talk_comes_first_and_what_was_shown_lately_after_it() {
        let segments = vec![seg(100_000, "Ganz am Anfang."), seg(690_000, "Die Peckl Isten fehlen noch.")];
        // An old slide, and one shown since before the last minutes and still on screen.
        let visuals = vec![screen(1, 50, Some(90), "Agenda"), screen(2, 230, Some(710), "Packlisten PL-0388")];
        let input = input(&segments, &visuals, &[], &[], 720_000);
        let (from, talk) = talk_lines(&input);
        assert_eq!(talk, "[11:30] Speaker 1: Die Peckl Isten fehlen noch.");
        assert_eq!(screen_lines(&input, from), ["[03:50] Screen: Packlisten PL-0388"]);
        let prompt = user_prompt(&input, &talk, &screen_lines(&input, from), 2, false);
        assert!(prompt.find("</transcript>").unwrap() < prompt.find("Screen: Packlisten").unwrap());
        assert!(system_prompt("German", false, true).contains("\"Screen:\""));
        assert!(!system_prompt("German", false, false).contains("\"Screen:\""));
    }

    #[test]
    fn the_transcript_starts_at_the_same_place_for_several_runs() {
        assert_eq!(window_start(60_000), 0);
        assert_eq!(window_start(720_000), 240_000);
        assert_eq!(window_start(959_000), 240_000);
        assert_eq!(window_start(960_000), 480_000);
        // Half a minute later, with more said, a new screen and a question: the request starts as the
        // last one did up to the end of what was said then.
        let mut segments: Vec<Segment> = (0..40).map(|i| seg(200_000 + i * 15_000, &format!("Satz Nummer {i} über die Anlage."))).collect();
        let earlier_visuals = vec![screen(1, 600, Some(700), "Anlagenübersicht")];
        let earlier = input(&segments[..34], &earlier_visuals, &[], &[], 720_000);
        let (from, talk) = talk_lines(&earlier);
        let before = user_prompt(&earlier, &talk, &screen_lines(&earlier, from), 2, false);
        segments.truncate(36);
        let visuals = vec![screen(1, 600, Some(700), "Anlagenübersicht"), screen(2, 740, None, "Versionen vergleichen")];
        let questions = vec![q(1, "Wer gibt eine Version frei?", State::Live, 720_000)];
        let later = input(&segments, &visuals, &questions, &[], 750_000);
        let (from, talk) = talk_lines(&later);
        let after = user_prompt(&later, &talk, &screen_lines(&later, from), 2, false);
        let kept = &before[..before.find("\n</transcript>").unwrap()];
        assert!(after.starts_with(kept));
    }

    #[test]
    fn a_long_transcript_starts_a_step_later_before_it_is_cut() {
        let long = "Wort ".repeat(100);
        let segments: Vec<Segment> = (0..120).map(|i| seg(i * 6_000, &long)).collect();
        let (from, talk) = talk_lines(&input(&segments, &[], &[], &[], 720_000));
        assert!(talk.chars().count() <= RECENT_CHARS);
        assert!(from >= 240_000 && talk.starts_with('['));
    }

    #[test]
    fn rephrased_questions_are_repeats() {
        let pairs = [
            (
                "Wie genau wird das Statushandling umgesetzt, wenn ein Feld rot ist, aber die Eltern-Elemente gelb sind?",
                "Wie wird das Statushandling bei roten Feldern mit gelben Eltern-Elementen umgesetzt?",
            ),
            ("Was bedeutet „Wartungsmodus“ im Export-Modul?", "Wie wird der Wartungsmodus im Export-Modul genutzt?"),
            ("Wie werden die Makros aus dem Dateisystem synchronisiert?", "Wie genau wird der Inhalt von Makros aus dem Dateisystem synchronisiert?"),
            ("Who owns the migration to the new billing system?", "Who owns the billing system migration?"),
            (
                "Warum wird die Konfiguration auf einer abstrakten Ebene erstellt, und wie stellen wir sicher, dass sie korrekt umgesetzt wird?",
                "Warum wird die Konfiguration auf einer abstrakten Ebene erstellt, wenn die konkreten Bauteile bereits im Excel enthalten sind?",
            ),
            (
                "Was bedeutet der Wert „29 %“ im Kontext der Anlagenkonfiguration?",
                "Was bedeutet „29 %“ im Kontext der Anlagenkonfiguration genau – ist das der Fortschritt bei der Datenpflege oder etwas anderes?",
            ),
        ];
        for (a, b) in pairs {
            assert!(same_question(a, b), "{a} / {b}");
        }
        let different = [
            ("Welche Rolle spielt die Versionierung bei Änderungen im Testsystem?", "Wer hat Zugriff auf das Testsystem?"),
            ("Wer pflegt die technischen Eigenschaften?", "Wie viele Werte werden automatisch erzeugt?"),
            ("Which customers are in the pilot group?", "What does 'soon' mean for the launch date?"),
        ];
        for (a, b) in different {
            assert!(!same_question(a, b), "{a} / {b}");
        }
    }

    #[test]
    fn asking_what_an_unseen_word_means_is_left_out() {
        let segments = vec![seg(10_000, "Da gibt es einen Togel."), seg(20_000, "Der Togel schaltet das um."), seg(30_000, "Und dann der Sied.")];
        let visuals = vec![screen(1, 5, Some(40), "Stationen / Wings / Funktionen")];
        let shown = input(&segments, &visuals, &[], &[], 60_000);
        // Not on any screen: a mishearing, even when said twice.
        assert!(misheard(&proposal(Kind::Understand, "Was bedeutet „Togel“ in den Regeln?"), &shown));
        // On screen, or not asking what a word means.
        assert!(!misheard(&proposal(Kind::Understand, "Was bedeutet \"Wing\" bei den Stationen?"), &shown));
        assert!(!misheard(&proposal(Kind::Understand, "Was bedeutet \"Wings\" bei den Stationen?"), &shown));
        assert!(!misheard(&proposal(Kind::Challenge, "Ist der „Togel“ wirklich nötig?"), &shown));
        assert!(!misheard(&proposal(Kind::Understand, "Wer schaltet den Togel um?"), &shown));
        // A letter read off a screen is no term to ask about.
        assert!(misheard(&proposal(Kind::Understand, "Was bedeutet „S“ in „Version S“?"), &shown));
        // Nothing shown: a word said once is likely misheard, one said again is not.
        let heard = input(&segments, &[], &[], &[], 60_000);
        assert!(misheard(&proposal(Kind::Understand, "Was bedeutet „Sied“ hier?"), &heard));
        assert!(!misheard(&proposal(Kind::Understand, "Was bedeutet „Togel“ hier?"), &heard));
    }

    #[test]
    fn names_listed_are_those_of_this_meeting() {
        let vocabulary = vec![
            Term { text: "AB".into(), meaning: Some("Auftragsbestätigung".into()) },
            Term { text: "ECAD".into(), meaning: None },
            Term { text: "Claude Code".into(), meaning: None },
        ];
        let segments = vec![seg(10_000, "Das geht dann an Claude Code.")];
        let visuals = vec![screen(1, 5, Some(40), "ECAD Export / Makros")];
        let input = input(&segments, &visuals, &[], &vocabulary, 60_000);
        let (_, talk) = talk_lines(&input);
        assert_eq!(names(&input, &talk), ["ECAD", "Claude Code"]);
        assert!(user_prompt(&input, &talk, &[], 2, false).contains("The meeting's names, as written: ECAD, Claude Code\n"));
    }

    #[test]
    fn a_big_picture_question_is_due_every_ten_minutes_from_the_fifth() {
        assert!(!big_picture_due(&[], 4 * 60_000));
        assert!(big_picture_due(&[], 5 * 60_000));
        let mut qs = vec![q(1, "Wer arbeitet heute mit dem Werkzeug?", State::Expired, 6 * 60_000)];
        assert!(big_picture_due(&qs, 7 * 60_000), "an ordinary question doesn't count");
        qs[0].big_picture = true;
        assert!(!big_picture_due(&qs, 15 * 60_000));
        assert!(big_picture_due(&qs, 16 * 60_000));
        let input = input(&[], &[], &qs, &[], 16 * 60_000);
        let prompt = user_prompt(&input, "[15:50] Speaker 1: Und so weiter.", &[], 2, true);
        assert!(prompt.contains("big picture, ") && prompt.contains("at most 1 more new question "));
        // Big-picture questions stay listed after the last minutes, others drop out.
        let old = vec![q(1, "Was zeigt die erste Folie?", State::Expired, 0), qs[0].clone()];
        let later = Input { questions: &old, now_ms: 40 * 60_000, ..input };
        let prompt = user_prompt(&later, "[39:50] Speaker 1: Gut.", &[], 2, false);
        assert!(prompt.contains("- Wer arbeitet heute mit dem Werkzeug?") && !prompt.contains("erste Folie"));
    }

    #[test]
    fn at_most_three_live_and_no_repeats() {
        let mut qs = vec![q(1, "Who owns the migration to the new billing system?", State::Live, 0), q(2, "Is the budget approved?", State::Deleted, 0)];
        let applied = apply(
            &mut qs,
            Outcome {
                new: vec![
                    proposal(Kind::FollowUp, "Who owns the billing system migration?"), // repeats Q1
                    proposal(Kind::Challenge, "Is the budget approved already?"),        // repeats deleted Q2
                    proposal(Kind::Understand, "What does 'soon' mean for the launch date?"),
                    Proposal { kind: Kind::Understand, text: "Which customers are in the pilot group?".into(), big_picture: true },
                    proposal(Kind::Understand, "How many support tickets came in last month?"),
                ],
                ..Default::default()
            },
            60_000,
        );
        assert!(applied.changed);
        assert_eq!(applied.repeats.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(live_count(&qs), MAX_LIVE);
        let texts: Vec<&str> = qs.iter().map(|q| q.text.as_str()).collect();
        assert_eq!(texts[2..], ["What does 'soon' mean for the launch date?", "Which customers are in the pilot group?"]);
        assert_eq!(qs.last().unwrap().number, 4);
        assert!(qs.last().unwrap().big_picture && !qs[2].big_picture);
    }

    #[test]
    fn answered_questions_close_and_unanswered_ones_expire() {
        let mut qs = vec![q(1, "Who owns it?", State::Live, 0), q(2, "When is the launch?", State::Later, 0), q(3, "Why now?", State::Live, 200_000)];
        apply(&mut qs, Outcome { answered: vec![2, 7], ..Default::default() }, 250_000);
        assert_eq!(qs[1].state, State::Answered);
        assert!(expire(&mut qs, LIVE_FOR_MS));
        assert_eq!([qs[0].state, qs[2].state], [State::Expired, State::Live]);
        assert!(!expire(&mut qs, LIVE_FOR_MS + 1));
    }

    #[test]
    fn user_acts_only_on_open_questions() {
        let mut qs = vec![q(1, "Who owns it?", State::Live, 0), q(2, "Why now?", State::Answered, 0)];
        act(&mut qs, 1, Act::Later, 5_000).unwrap();
        assert_eq!(qs[0].state, State::Later);
        act(&mut qs, 1, Act::Asked, 6_000).unwrap();
        assert_eq!(qs[0].state, State::Asked);
        assert!(act(&mut qs, 2, Act::Deleted, 7_000).is_err());
        assert!(act(&mut qs, 9, Act::Deleted, 7_000).is_err());
    }

    #[test]
    fn room_counts_live_questions_only() {
        let qs = vec![q(1, "a b c d", State::Live, 0), q(2, "e f g h", State::Later, 0)];
        assert_eq!(room(&qs), 2);
        let full = vec![q(1, "a", State::Live, 0), q(2, "b", State::Live, 0), q(3, "c", State::Live, 0)];
        assert_eq!(room(&full), 0);
    }

    #[test]
    fn open_questions_section_lists_live_and_later() {
        let qs = vec![q(1, "Who owns it?", State::Live, 0), q(2, "When?", State::Later, 0), q(3, "Why?", State::Asked, 0)];
        assert_eq!(open_section(&qs, "Open questions").unwrap(), "## Open questions\n- Who owns it?\n- When?");
        assert_eq!(open_section(&qs[2..], "Open questions"), None);
    }
}
