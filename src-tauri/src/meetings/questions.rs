//! Live questions: while a meeting is recorded, the notes model reads the last minutes of the
//! transcript and proposes questions "Me" could ask next, steered by the meeting's goal if one was
//! given. At most `MAX_LIVE` are open at a time; each run also says which open ones the talk has
//! answered since. A question nobody acts on expires after `LIVE_FOR_MS`; one kept for Later stays
//! until the meeting ends, and the notes list what was left open.
//!
//! This module is the pure part (prompt, answer format, rules); the meeting thread that runs it
//! lives in `mod.rs`.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use super::{visuals, Segment, Speaker, Visual};
use crate::llm;

/// Open questions shown at a time.
pub const MAX_LIVE: usize = 3;
/// New questions one run may add.
const MAX_NEW_PER_RUN: usize = 2;
/// A live question nobody pressed anything on expires after this much meeting time.
pub const LIVE_FOR_MS: u64 = 5 * 60_000;
/// The transcript a run reads: the last minutes, at most this many characters.
const RECENT_MS: u64 = 8 * 60_000;
const RECENT_CHARS: usize = 10_000;
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

/// What one run of the model said.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// Questions the talk answered (checked against the transcript).
    pub answered: Vec<u32>,
    pub new: Vec<(Kind, String)>,
}

/// The model's answer as written: answered questions with the words it says answer them.
#[derive(Debug, Default, PartialEq)]
struct Parsed {
    answered: Vec<(u32, String)>,
    new: Vec<(Kind, String)>,
}

/// Applies a run: marks answered questions, adds new ones that aren't repeats while there's room.
/// Returns whether anything changed.
pub fn apply(questions: &mut Vec<Question>, outcome: Outcome, now_ms: u64) -> bool {
    let mut changed = false;
    for q in questions.iter_mut().filter(|q| q.state.is_open() && outcome.answered.contains(&q.number)) {
        q.state = State::Answered;
        q.changed_ms = now_ms;
        changed = true;
    }
    for (kind, text) in outcome.new {
        if live_count(questions) >= MAX_LIVE {
            break;
        }
        if questions.iter().any(|q| similar(&q.text, &text)) {
            continue;
        }
        let number = questions.iter().map(|q| q.number).max().unwrap_or(0) + 1;
        questions.push(Question { number, text, kind, state: State::Live, at_ms: now_ms, changed_ms: now_ms });
        changed = true;
    }
    changed
}

/// The words that carry a question's meaning (lower case, short words left out).
fn content_words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 3)
        .map(str::to_lowercase)
        .collect()
}

/// Whether two questions ask much the same (most of their content words are shared).
pub(super) fn similar(a: &str, b: &str) -> bool {
    let (a, b) = (content_words(a), content_words(b));
    if a.is_empty() || b.is_empty() {
        return a == b;
    }
    let shared = a.intersection(&b).count() as f32;
    shared / a.len().min(b.len()) as f32 >= 0.7
}

pub struct Input<'a> {
    pub goal: &'a str,
    pub notes: &'a str,
    pub language: Option<&'a str>,
    pub segments: &'a [Segment],
    pub speakers: &'a [Speaker],
    /// What was shown (screens, photos); merged into the transcript by time.
    pub visuals: &'a [Visual],
    pub questions: &'a [Question],
    /// What the system map still doesn't know ("Name: what"); questions go after it first.
    pub unknowns: &'a [String],
    pub now_ms: u64,
}

/// "[mm:ss] Speaker: text" lines of the last minutes of the talk, with what was on screen then
/// ("[mm:ss] Screen: …", "[mm:ss] Photo: …").
fn recent_lines(input: &Input) -> String {
    let from = input.now_ms.saturating_sub(RECENT_MS);
    let recent: Vec<Segment> = input.segments.iter().filter(|s| s.end_ms >= from).cloned().collect();
    let shown = visuals::since(input.visuals, from);
    let lines = super::pipeline::timeline_lines(&recent, input.speakers, &shown, visuals::QUESTIONS).join("\n");
    match lines.char_indices().rev().nth(RECENT_CHARS) {
        // Too long: keep the end, from the start of a line.
        Some((cut, _)) => lines[cut..].split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_default(),
        None => lines,
    }
}

/// How many new questions a run may propose now.
pub fn room(questions: &[Question]) -> usize {
    MAX_LIVE.saturating_sub(live_count(questions)).min(MAX_NEW_PER_RUN)
}

fn system_prompt(language: &str, goal: bool, shown: bool) -> String {
    let aim = if goal { "and move the meeting towards its goal" } else { "and move the meeting forward" };
    let shown = if shown { llm::SHOWN } else { "" };
    format!(
        "You listen to a meeting together with \"Me\", the person who recorded it, and suggest questions Me could \
         ask next. A good question is short (at most 20 words), specific to what was just said, {aim}. Never \
         suggest a question the transcript already answers, or one Me already asked. The transcript comes from \
         speech recognition and has mistakes.{shown} Write the questions in {language}, addressed to the others in \
         the meeting."
    )
}

fn user_prompt(input: &Input, transcript: &str, room: usize) -> String {
    let mut p = String::new();
    if !input.goal.trim().is_empty() {
        p.push_str(&format!("Goal of the meeting: {}\n\n", input.goal.trim()));
    }
    let notes: String = input.notes.trim().chars().take(1200).collect();
    if !notes.is_empty() {
        p.push_str(&format!("My own notes so far:\n{notes}\n\n"));
    }
    if !input.unknowns.is_empty() {
        p.push_str("Still unknown about the systems and people involved (good questions find these out):\n");
        for u in input.unknowns.iter().take(8) {
            p.push_str(&format!("- {u}\n"));
        }
        p.push('\n');
    }
    let open: Vec<&Question> = input.questions.iter().filter(|q| q.state.is_open()).collect();
    if !open.is_empty() {
        p.push_str("Questions suggested before that are still open:\n");
        for q in &open {
            p.push_str(&format!("Q{} ({}, suggested at {}): {}\n", q.number, q.kind.word(), super::pipeline::clock(q.at_ms), q.text));
        }
        p.push('\n');
    }
    let earlier: Vec<&Question> = input.questions.iter().filter(|q| !q.state.is_open()).collect();
    if !earlier.is_empty() {
        p.push_str("Questions suggested before that are closed (never suggest these again, nor the same in other words):\n");
        for q in &earlier {
            p.push_str(&format!("- {}\n", q.text));
        }
        p.push('\n');
    }
    p.push_str(&format!("The last minutes of the meeting:\n<transcript>\n{transcript}\n</transcript>\n\n"));
    if !open.is_empty() {
        p.push_str(
            "First, for each open question, one line saying whether someone in the transcript answered it after it \
             was suggested: \"Q2: yes\" followed by the words from the transcript that answer it, copied exactly, \
             for example Q2: yes \"we launch on the first of March\"; otherwise \"Q2: no\".\n",
        );
    }
    let then = if open.is_empty() { "Suggest" } else { "Then suggest" };
    if room == 0 {
        p.push_str("Then stop: suggest no new question.");
    } else {
        p.push_str(&format!(
            "{then} at most {room} new question{s} Me could ask now, one per line, each starting with its kind:\n\
             understand: something said is vague, ambiguous or unexplained.\n\
             challenge: a claim, assumption, number or plan that should be tested.\n\
             follow-up: something left open (no owner, date, number or next step), or a topic worth going deeper on.\n\
             Only suggest a question when there is a good one. None is fine, especially when little was said.",
            s = if room == 1 { "" } else { "s" }
        ));
    }
    p
}

/// The answer's form: a verdict line per open question ("Q2: no", or "Q2: yes" with the words that
/// answer it), then up to `room` "kind: question?" lines. A verdict for each question makes a small
/// model weigh each one (offered a single "none", it took that every time); each question ends at
/// its question mark, so it can't ramble on after it.
fn grammar(open: &[u32], room: usize) -> String {
    let verdicts: Vec<String> = open
        .iter()
        .map(|n| format!("\"Q{n}: \" (\"no\" | \"yes \\\"\" [^\"\\n]+ \"\\\"\") \"\\n\""))
        .collect();
    let verdicts = if verdicts.is_empty() { String::new() } else { format!("{} ", verdicts.join(" ")) };
    format!(
        "root ::= {verdicts}question{{0,{room}}}\n\
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
        } else if let Some((kind, text)) = line.split_once(':') {
            let Some(kind) = Kind::from_word(kind) else { continue };
            let text = text.trim().trim_matches(['"', '“', '”', '„']).trim();
            if text.split_whitespace().count() < 3 {
                continue;
            }
            let mut cut: String = text.chars().take(TEXT_MAX).collect();
            if cut.len() < text.len() {
                cut.push('…');
            }
            let text = cut;
            out.new.push((kind, text));
        }
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

/// One run: asks the model about the recent transcript. `None` when there's nothing to read.
pub fn run(worker: &mut llm::Worker, input: &Input) -> anyhow::Result<Option<Outcome>> {
    let transcript = recent_lines(input);
    if transcript.trim().is_empty() {
        return Ok(None);
    }
    let room = room(input.questions);
    let open: Vec<u32> = input.questions.iter().filter(|q| q.state.is_open()).map(|q| q.number).collect();
    let language = llm::language_name(input.language);
    let system = system_prompt(language, !input.goal.trim().is_empty(), visuals::any_shown(&transcript.lines().map(str::to_string).collect::<Vec<_>>()));
    let grammar = grammar(&open, room);
    let answer = worker.chat_at(&system, &user_prompt(input, &transcript, room), ANSWER_TOKENS, Some(&grammar), None, Some("questions"), |_| {})?;
    let parsed = parse(&answer);
    let answered = verified(&parsed.answered, input);
    for (n, quote) in parsed.answered.iter().filter(|(n, _)| !answered.contains(n)) {
        log::info!("meeting questions: Q{n} \"{quote}\" was not said after the question; still open");
    }
    Ok(Some(Outcome { answered, new: parsed.new }))
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

    fn q(number: u32, text: &str, state: State, at_ms: u64) -> Question {
        Question { number, text: text.into(), kind: Kind::Understand, state, at_ms, changed_ms: at_ms }
    }

    #[test]
    fn parses_the_answer() {
        let out = parse(
            "Q2: yes \"we launch in March\"\nQ3: no\nQ5: yes \"Anna does it\"\nchallenge: Why twelve euros and not ten?\nfollow-up: Wer übernimmt das Onboarding bis Freitag?\n",
        );
        assert_eq!(out.answered, vec![(2, "we launch in March".into()), (5, "Anna does it".into())]);
        assert_eq!(
            out.new,
            vec![
                (Kind::Challenge, "Why twelve euros and not ten?".into()),
                (Kind::FollowUp, "Wer übernimmt das Onboarding bis Freitag?".into())
            ]
        );
        assert_eq!(parse("Q1: no\n"), Parsed::default());
        // Too short to be a question, or not a kind.
        assert!(parse("understand: Why?\nnote: something here to say\n").new.is_empty());
    }

    #[test]
    fn grammar_lists_as_many_questions_as_there_is_room_for() {
        let g = grammar(&[2, 5], 1);
        assert!(g.contains("question{0,1}") && g.contains("\"Q2: \"") && g.contains("\"Q5: \""));
        assert!(grammar(&[], 2).starts_with("root ::= question{0,2}"));
    }

    #[test]
    fn an_answer_counts_only_if_its_words_were_said_after_the_question() {
        let seg = |start_ms: u64, text: &str| Segment {
            start_ms,
            end_ms: start_ms + 3000,
            channel: super::super::Channel::System,
            speaker: 1,
            text: text.into(),
            language: None,
            fixes: Vec::new(),
        };
        let mut right = seg(68_000, "Right.");
        right.speaker = 0;
        let segments = vec![seg(10_000, "The launch is on the first of March."), right, seg(70_000, "Anna will do the onboarding, yes.")];
        let questions = vec![q(1, "When is the launch?", State::Live, 60_000), q(2, "Who does the onboarding?", State::Live, 60_000)];
        let input = Input { goal: "", notes: "", language: None, segments: &segments, speakers: &[], visuals: &[], questions: &questions, unknowns: &[], now_ms: 80_000 };
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
    fn recent_lines_show_what_is_on_screen() {
        use super::super::visuals::Source;
        let seg = |start_ms: u64, text: &str| Segment { start_ms, end_ms: start_ms + 3000, channel: super::super::Channel::System, speaker: 1, text: text.into(), language: None, fixes: Vec::new() };
        let segments = vec![seg(100_000, "Ganz am Anfang."), seg(690_000, "Die Peckl Isten fehlen noch.")];
        let shown = |id: u32, at_s: u64, until_s: Option<u64>, text: &str| Visual {
            id,
            at_ms: at_s * 1000,
            until_ms: until_s.map(|u| u * 1000),
            source: Source::Screen,
            label: String::new(),
            text: text.into(),
            image: None,
        };
        // An old slide, and one shown since before the last minutes and still on screen.
        let visuals = vec![shown(1, 50, Some(90), "Agenda"), shown(2, 230, Some(710), "Packlisten PL-0388")];
        let input = Input { goal: "", notes: "", language: None, segments: &segments, speakers: &[], visuals: &visuals, questions: &[], unknowns: &[], now_ms: 720_000 };
        assert_eq!(recent_lines(&input), "[03:50] Screen: Packlisten PL-0388\n[11:30] Speaker 1: Die Peckl Isten fehlen noch.");
        assert!(system_prompt("German", false, true).contains("\"Screen:\""));
        assert!(!system_prompt("German", false, false).contains("\"Screen:\""));
    }

    #[test]
    fn at_most_three_live_and_no_repeats() {
        let mut qs = vec![q(1, "Who owns the migration to the new billing system?", State::Live, 0), q(2, "Is the budget approved?", State::Deleted, 0)];
        let changed = apply(
            &mut qs,
            Outcome {
                answered: vec![],
                new: vec![
                    (Kind::FollowUp, "Who owns the billing system migration?".into()), // repeats Q1
                    (Kind::Challenge, "Is the budget approved already?".into()),        // repeats deleted Q2
                    (Kind::Understand, "What does 'soon' mean for the launch date?".into()),
                    (Kind::Understand, "Which customers are in the pilot group?".into()),
                    (Kind::Understand, "How many support tickets came in last month?".into()),
                ],
            },
            60_000,
        );
        assert!(changed);
        assert_eq!(live_count(&qs), MAX_LIVE);
        let texts: Vec<&str> = qs.iter().map(|q| q.text.as_str()).collect();
        assert_eq!(texts[2..], ["What does 'soon' mean for the launch date?", "Which customers are in the pilot group?"]);
        assert_eq!(qs.last().unwrap().number, 4);
    }

    #[test]
    fn answered_questions_close_and_unanswered_ones_expire() {
        let mut qs = vec![q(1, "Who owns it?", State::Live, 0), q(2, "When is the launch?", State::Later, 0), q(3, "Why now?", State::Live, 200_000)];
        apply(&mut qs, Outcome { answered: vec![2, 7], new: vec![] }, 250_000);
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
