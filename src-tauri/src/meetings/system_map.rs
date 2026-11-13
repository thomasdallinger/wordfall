//! System map: while a meeting is recorded, the notes model keeps a small map of the systems,
//! people and roles that come up, how they connect (what flows, and how: API, CSV, mail, by hand)
//! and what is still unknown about them. Each run reads only the talk since the last run, plus the
//! map as it stands and the user's notes, and answers with small edits ("node", "alias", "known",
//! "flow", "open", "solved"), each citing the transcript line it comes from; the edits are checked
//! against that line before they change the map. A run costs about the same early and late in a
//! meeting.
//!
//! Precision first: the model may call a name a topic, data or a feature instead of a box (it is set
//! aside), a person needs a name, a title or a role word, a new box stays a hidden candidate until it
//! is said again, takes part in a flow or has a proper name, and every ~10 minutes a tidy-up run
//! looks at the whole map once (keep, merge, drop, re-kind).
//!
//! The user's notes steer it: what they say to focus on or find out becomes open points. Open points
//! feed the live questions (see `questions.rs`), and the map is added below the notes at the end in
//! a fixed Markdown shape another program reads (see `section`).
//!
//! This module is the pure part (data, prompt, answer format, rules); the meeting thread that runs
//! it lives in `mod.rs`.

use serde::{Deserialize, Serialize};
use super::{questions::similar, Segment, Speaker};
use crate::llm;

/// Node lines one answer may write (boxes, candidates or set-aside names), and new boxes one run may
/// draw at once.
const NODE_LINES: usize = 3;
const MAX_NEW: usize = 2;
/// Other lines one answer may write.
const MAX_LINES: usize = 7;
const ANSWER_TOKENS: u32 = 300;
/// Past this many boxes, a new one needs a flow or strong evidence (a name, the notes).
const SOFT_CAP: usize = 40;
/// Greedy: the map wants the plain facts, the same each time (see `wordfall-llm`).
const TEMPERATURE: f32 = 0.0;
/// Lines of earlier talk a run sees before the new ones.
pub const EARLIER_LINES: usize = 4;
/// New talk a run reads at most (when runs fell behind, the latest of it).
const NEW_CHARS: usize = 6_000;
/// The notes a run reads at most.
const NOTES_CHARS: usize = 1_200;
/// Facts kept per node, open points kept in all.
const MAX_KNOWN: usize = 8;
const MAX_OPEN: usize = 12;
const NAME_MAX: usize = 48;
const TEXT_MAX: usize = 160;
/// Names set aside as no box, kept per meeting.
const MAX_SET_ASIDE: usize = 120;
/// The line a box was first said in, kept for the tidy-up, at most this long.
const SAID_MAX: usize = 140;
/// Pairs of boxes that look alike the tidy-up judges at most.
const MAX_PAIRS: usize = 12;
/// Candidates the prompt shows at most (the ones the new talk names).
const MAX_SHOWN_CANDIDATES: usize = 6;
/// Names said but not on the map that the live view lists under it, at most.
const ALSO_MENTIONED: usize = 12;
/// Meeting time between two tidy-up runs.
pub const TIDY_EVERY_MS: u64 = 10 * 60_000;
/// A tidy-up is worth it from this many boxes on.
const TIDY_FROM: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// What the user's side builds or runs.
    Ours,
    /// Runs at the customer (the others in the meeting).
    Customer,
    /// A third party's service.
    Outside,
    /// A person, team or role.
    Person,
}

impl Kind {
    /// The words for it in the exported section.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Ours => "our system",
            Kind::Customer => "customer system",
            Kind::Outside => "outside service",
            Kind::Person => "person or role",
        }
    }

    /// The words for it in the prompt and the model's answer. Not "our system": the others in a
    /// meeting say "our ERP" all the time, and a small model then files their systems as the user's.
    fn word(self, room: Room) -> &'static str {
        match self {
            Kind::Ours => room.ours(),
            k => k.label(),
        }
    }

    const ALL: [Kind; 4] = [Kind::Ours, Kind::Customer, Kind::Outside, Kind::Person];

    /// Either word for "ours" is read, whatever the room.
    fn from_word(s: &str) -> Option<Self> {
        let s = s.trim();
        if s == Room::Call.ours() || s == Room::OneMic.ours() {
            return Some(Kind::Ours);
        }
        Self::ALL.into_iter().find(|k| k.label() == s && *k != Kind::Ours)
    }
}

/// Where the meeting happens, for what the model is told about "us". On a call, the microphone is
/// the user ("Me") and the others come in as system audio; in a room, everyone is on the one
/// microphone, so "Me" is whoever spoke first and the speaker labels don't tell the sides apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Room {
    Call,
    OneMic,
}

impl Room {
    fn ours(self) -> &'static str {
        match self {
            Room::Call => "Me's system",
            Room::OneMic => "my company's system",
        }
    }
}

/// What the model may call a name instead of a box: it is set aside, never drawn.
const NOT_BOX_WORDS: [&str; 3] = ["topic", "data or document", "feature or process"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: u32,
    pub name: String,
    pub kind: Kind,
    /// Facts said in the meeting, one short line each.
    pub known: Vec<String>,
    /// Names it had before the user renamed it (the model may still use them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aka: Vec<String>,
    /// Meeting time it was added at.
    pub at_ms: u64,
    /// The user named it: the tidy-up never drops it or merges it away.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub named: bool,
    /// The line it was first said in, for the tidy-up to judge it by.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub said: String,
}

/// Something flows from one node to another.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub from: u32,
    pub to: u32,
    /// What flows ("orders", "invoices as PDF").
    pub what: String,
    /// How ("API", "CSV", "mail", "by hand", …).
    pub how: String,
}

/// Something about a node that is still unknown.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenPoint {
    /// O1, O2, … within the meeting.
    pub number: u32,
    pub node: u32,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SystemMap {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub open: Vec<OpenPoint>,
    /// Open points handed out so far: numbers are never reused, so "solved: O3" can't hit a newer one.
    pub opened: u32,
    /// Names of nodes the user deleted: never added again.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<String>,
    /// The highest id a deleted node had: ids aren't reused, so the model's "N5" can't mean an old one.
    #[serde(skip_serializing_if = "is_zero")]
    pub dropped_ids: u32,
    /// Boxes said once and not confirmed yet: not drawn, not exported, not in the prompt. One becomes
    /// a box when it is said again in a later run, takes part in a flow, or is in the notes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<Node>,
    /// Names the model called a topic, data or a feature, or the tidy-up dropped: no box.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub set_aside: Vec<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Lower case, single spaces, no quotes: how names are compared.
fn norm(name: &str) -> String {
    name.trim_matches(|c: char| c.is_whitespace() || "\"'“”„`*".contains(c))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Letters and digits only, lower case.
fn compact(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn cut(text: &str, max: usize) -> String {
    let text = text.trim().trim_matches(['"', '“', '”', '„']).trim();
    let mut out: String = text.chars().take(max).collect();
    if out.len() < text.len() {
        out.push('…');
    }
    out
}

fn names(x: &Node) -> impl Iterator<Item = &String> {
    std::iter::once(&x.name).chain(&x.aka)
}

/// Word endings that don't make another word (plural, German cases, "-in" for a woman).
const ENDINGS: &[&str] = &["s", "es", "e", "en", "n", "er", "ern", "nen", "in", "innen"];

/// The same word but for an ending: "Lieferant" ~ "Lieferanten", "order" ~ "orders".
fn inflected(a: &str, b: &str) -> bool {
    a == b
        || [(a, b), (b, a)].iter().any(|(x, y)| x.chars().count() >= 3 && y.strip_prefix(x).is_some_and(|rest| ENDINGS.contains(&rest)))
}

/// Edits (insert, delete, change a letter) from one word to the other.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let here = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(here + 1);
            prev = here;
        }
    }
    row[b.len()]
}

/// Two names for the same thing heard twice: the same but for an ending, or (five letters or more)
/// at most one letter in five different ("Varnor" ~ "Varnog", "Lieferant" ~ "Lieferamt").
fn near(a: &str, b: &str) -> bool {
    let (a, b) = (compact(a), compact(b));
    if a.chars().count() < 3 || b.chars().count() < 3 {
        return false;
    }
    if inflected(&a, &b) {
        return true;
    }
    let (la, lb) = (a.chars().count(), b.chars().count());
    la.min(lb) >= 5 && edit_distance(&a, &b) as f32 <= 0.2 * la.max(lb) as f32
}

/// Words for a part of a system rather than which system it is: two names that differ only by them
/// are one box ("Navision" ~ "Navision-Modul" ~ "Navision Core"). The longer ones also as the last
/// part of a German compound ("Navisionmodul").
const PART_WORDS: &[&str] = &[
    "modul", "module", "core", "engine", "system", "tool", "software", "app", "plattform", "platform", "komponente", "component",
];

/// A name without its part words, letters and digits only ("Navi-Sion Modul" → "navision"); the
/// whole name when it is nothing but part words ("Core System").
fn head(name: &str) -> String {
    let kept: Vec<String> = words(name)
        .into_iter()
        .filter(|w| !PART_WORDS.contains(&w.as_str()))
        .map(|w| {
            let stem = PART_WORDS.iter().filter(|p| p.len() >= 5).find_map(|p| w.strip_suffix(p).filter(|rest| rest.chars().count() >= 3));
            stem.map(str::to_string).unwrap_or(w)
        })
        .collect();
    if kept.is_empty() { compact(name) } else { kept.concat() }
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Two names of one thing that speech recognition heard (or a screen showed) in different ways: the
/// same but for part words, or (four letters on) sounding alike with at most a third of the letters
/// different and the same digits ("Navision" ~ "Navisjon Modul"; not "Windows 10" ~ "Windows 11").
fn variant(a: &str, b: &str) -> bool {
    let (x, y) = (head(a), head(b));
    let (lx, ly) = (x.chars().count(), y.chars().count());
    lx.min(ly) >= 3
        && !different_people(a, b)
        && (x == y
            || (lx.min(ly) >= 4
                && digits(&x) == digits(&y)
                && super::terms::phonetic(&x) == super::terms::phonetic(&y)
                && edit_distance(&x, &y) * 3 <= lx.max(ly)))
}

/// Whether `heard` is the name `shown` (on a screen, in the notes) as speech recognition heard it:
/// a variant (see `variant`), or from four letters on one letter off with the same first letter
/// ("SKU" is too short; "EDIX" ~ "Edix", "ECAD" ~ "eCard").
fn heard_as(shown: &str, heard: &str) -> bool {
    if variant(shown, heard) {
        return true;
    }
    let (x, y) = (head(shown), head(heard));
    x.chars().count().min(y.chars().count()) >= 4 && x.chars().next() == y.chars().next() && digits(&x) == digits(&y) && edit_distance(&x, &y) <= 1
}

/// The spelling a screen or the notes showed for `name` when it was heard otherwise: that spelling
/// with the part words `name` has ("Ecard Modul" → "ECAD Modul" when "ECAD" was shown), if exactly
/// one shown spelling fits. `None` when `name` is spelled as shown already.
fn shown_spelling(name: &str, spellings: &[String]) -> Option<String> {
    if spellings.iter().any(|s| head(s) == head(name)) {
        return None;
    }
    let fits: Vec<&String> = spellings.iter().filter(|s| heard_as(s, name)).collect();
    let [shown] = fits.as_slice() else { return None };
    if head(shown) != compact(shown) {
        return Some(shown.to_string());
    }
    let parts = name.split(|c: char| c.is_whitespace() || c == '-').filter(|w| PART_WORDS.contains(&w.to_lowercase().as_str()));
    Some(std::iter::once(shown.as_str()).chain(parts).collect::<Vec<_>>().join(" "))
}

/// The node called `name` among `nodes`. A small model doesn't always repeat a name exactly, so
/// failing that: the same letters ("BusinessCentral"), the one node whose name holds it or is held
/// by it ("Business Central" for "Microsoft Dynamics Business Central"), the one node whose name
/// is the same but for an ending or a misheard letter ("Lieferanten", "Varnog" for "Varnor"), or
/// the one that is the same heard another way (see `variant`: "Navisjon Modul" for "Navision").
fn find_in(nodes: &[Node], name: &str) -> Option<u32> {
    let n = norm(name);
    if let Some(x) = nodes.iter().find(|x| names(x).any(|a| norm(a) == n)) {
        return Some(x.id);
    }
    let c = compact(name);
    if c.chars().count() < 3 {
        return None;
    }
    if let Some(x) = nodes.iter().find(|x| names(x).any(|a| compact(a) == c)) {
        return Some(x.id);
    }
    let unique = |hits: Vec<u32>| (hits.len() == 1).then(|| hits[0]);
    // Whole words, not letters: "ERP" is not in "LagerPro".
    let mine = words(name);
    let holds = |a: &[String], b: &[String]| !a.is_empty() && a.iter().all(|w| b.contains(w));
    let holding: Vec<u32> = nodes
        .iter()
        .filter(|x| names(x).any(|a| {
            let theirs = words(a);
            compact(a).chars().count() >= 3 && (holds(&mine, &theirs) || holds(&theirs, &mine))
        }))
        .map(|x| x.id)
        .collect();
    unique(holding)
        .or_else(|| unique(nodes.iter().filter(|x| names(x).any(|a| near(a, name))).map(|x| x.id).collect()))
        .or_else(|| unique(nodes.iter().filter(|x| names(x).any(|a| variant(a, name))).map(|x| x.id).collect()))
}

impl SystemMap {
    /// The box called `name` (or once called so, or nearly so; see `find_in`), or for "Me's system"
    /// the one box of that kind. Candidates aren't boxes yet: see `candidate`.
    pub fn find(&self, name: &str) -> Option<u32> {
        find_in(&self.nodes, name).or_else(|| {
            matches!(norm(name).as_str(), "me's system" | "my company's system" | "our system" | "me")
                .then(|| {
                    let ours: Vec<u32> = self.nodes.iter().filter(|x| x.kind == Kind::Ours).map(|x| x.id).collect();
                    (ours.len() == 1).then(|| ours[0])
                })
                .flatten()
        })
    }

    /// The hidden candidate called `name` (as loosely as `find`).
    fn candidate(&self, name: &str) -> Option<u32> {
        find_in(&self.candidates, name)
    }

    fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// A box or a candidate.
    fn any_node(&self, id: u32) -> Option<&Node> {
        self.node(id).or_else(|| self.candidates.iter().find(|n| n.id == id))
    }

    fn any_node_mut(&mut self, id: u32) -> Option<&mut Node> {
        self.nodes.iter_mut().chain(self.candidates.iter_mut()).find(|n| n.id == id)
    }

    fn name_of(&self, id: u32) -> &str {
        self.any_node(id).map(|n| n.name.as_str()).unwrap_or("?")
    }

    /// The number the next new box (or candidate) gets: never one a deleted box had.
    fn next_id(&self) -> u32 {
        self.nodes.iter().chain(&self.candidates).map(|x| x.id).max().unwrap_or(0).max(self.dropped_ids) + 1
    }

    fn is_dropped(&self, name: &str) -> bool {
        let n = norm(name);
        self.dropped.iter().any(|d| norm(d) == n)
    }

    /// Set aside as no box before (or nearly that name).
    fn is_set_aside(&self, name: &str) -> bool {
        let n = norm(name);
        self.set_aside.iter().any(|d| norm(d) == n || inflected(&compact(d), &compact(name)))
    }

    fn set_aside(&mut self, name: &str) {
        if !self.is_set_aside(name) {
            self.set_aside.push(name.to_string());
            let over = self.set_aside.len().saturating_sub(MAX_SET_ASIDE);
            self.set_aside.drain(..over);
        }
    }

    /// A candidate becomes a box.
    fn promote(&mut self, id: u32) -> bool {
        let Some(i) = self.candidates.iter().position(|n| n.id == id) else { return false };
        let node = self.candidates.remove(i);
        self.nodes.push(node);
        true
    }

    /// The hidden candidates' names, the latest first (at most `ALSO_MENTIONED`, each once): listed
    /// under the live map so nothing said is overlooked, without a box. Names set aside as topics,
    /// data or features aren't candidates.
    pub fn also_mentioned(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut latest: Vec<&Node> = self.candidates.iter().collect();
        latest.sort_by_key(|n| std::cmp::Reverse((n.at_ms, n.id)));
        for n in latest {
            if out.len() < ALSO_MENTIONED && !out.iter().any(|x| norm(x) == norm(&n.name)) {
                out.push(n.name.clone());
            }
        }
        out
    }

    /// Open points as "Name: what is unknown", for the questions.
    pub fn unknowns(&self) -> Vec<String> {
        self.open.iter().map(|o| format!("{}: {}", self.name_of(o.node), o.text)).collect()
    }

    /// The user renames a node. The old name stays known to the model's edits.
    pub fn rename(&mut self, id: u32, name: &str) -> anyhow::Result<()> {
        let name = cut(name, NAME_MAX);
        anyhow::ensure!(!name.is_empty(), "A name can't be empty.");
        let n = norm(&name);
        let taken = self.nodes.iter().any(|x| x.id != id && names(x).any(|a| norm(a) == n));
        anyhow::ensure!(!taken, "Another box already has that name.");
        let node = self.nodes.iter_mut().find(|n| n.id == id).ok_or_else(|| anyhow::anyhow!("That box is gone."))?;
        node.named = true;
        if norm(&node.name) != norm(&name) {
            let old = std::mem::replace(&mut node.name, name);
            node.aka.push(old);
        } else {
            node.name = name;
        }
        Ok(())
    }

    /// The user deletes a wrong node: its connections and open points go with it, and the model
    /// won't add it again.
    pub fn delete(&mut self, id: u32) -> anyhow::Result<()> {
        let node = self.remove(id).ok_or_else(|| anyhow::anyhow!("That box is gone."))?;
        self.dropped.push(node.name);
        self.dropped.extend(node.aka);
        Ok(())
    }

    /// Takes a box off the map with its flows and open points.
    fn remove(&mut self, id: u32) -> Option<Node> {
        let i = self.nodes.iter().position(|n| n.id == id)?;
        let node = self.nodes.remove(i);
        self.dropped_ids = self.dropped_ids.max(node.id);
        self.edges.retain(|e| e.from != id && e.to != id);
        self.open.retain(|o| o.node != id);
        Some(node)
    }

    /// `from` is the same box as `into`: its names, facts, flows and open points move over. A proper
    /// name takes over from a generic one.
    fn merge(&mut self, from: u32, into: u32) -> bool {
        let (Some(a), Some(b)) = (self.node(from), self.node(into)) else { return false };
        // A person and a system are never one box.
        if from == into || (a.kind == Kind::Person) != (b.kind == Kind::Person) {
            return false;
        }
        let edges: Vec<Edge> = self.edges.iter().filter(|e| e.from == from || e.to == from).cloned().collect();
        let open: Vec<OpenPoint> = self.open.iter().filter(|o| o.node == from).cloned().collect();
        let Some(gone) = self.remove(from) else { return false };
        let mut gone_names: Vec<String> = names(&gone).cloned().collect();
        let target = self.nodes.iter_mut().find(|n| n.id == into).expect("checked above");
        if is_generic(&target.name) && !is_generic(&gone.name) && !target.named {
            let old = std::mem::replace(&mut target.name, gone_names.remove(0));
            target.aka.push(old);
        }
        // Only names that fit the box that stays (no generic word on a proper name).
        let target = self.nodes.iter().find(|n| n.id == into).expect("checked above");
        let fitting: Vec<String> = gone_names.into_iter().filter(|a| alias_fits(self, target, a, false)).collect();
        let target = self.nodes.iter_mut().find(|n| n.id == into).expect("checked above");
        for a in fitting {
            if norm(&a) != norm(&target.name) && !target.aka.iter().any(|x| norm(x) == norm(&a)) {
                target.aka.push(a);
            }
        }
        for k in gone.known {
            add_known(self, into, k);
        }
        for mut e in edges {
            e.from = if e.from == from { into } else { e.from };
            e.to = if e.to == from { into } else { e.to };
            let twice = self.edges.iter().any(|x| (x.from, x.to) == (e.from, e.to) && similar(&x.what, &e.what));
            if e.from != e.to && !twice {
                self.edges.push(e);
            }
        }
        for mut o in open {
            o.node = into;
            self.open.push(o);
        }
        true
    }
}

// ---------- the model's edits ----------

/// Where an edit says it comes from: a numbered transcript line, or the user's notes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cite {
    Line(usize),
    Notes,
}

/// One edit the model proposes. Nodes are referred to by number ("N3"); a new node gets one of the
/// numbers it was offered, which `apply` maps to the node it really becomes (or already was).
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Node { n: u32, name: String, kind: Kind, aka: Option<String>, cite: Cite },
    /// A name the model called a topic, data or a feature: no box.
    NotBox { name: String, cite: Cite },
    Alias { n: u32, name: String, cite: Cite },
    Known { n: u32, fact: String, cite: Cite },
    Flow { from: u32, to: u32, what: String, how: String, cite: Cite },
    Open { n: u32, text: String, cite: Cite },
    Solved { number: u32, fact: String, cite: Cite },
}

/// What the edits are checked against: the transcript lines the model saw (numbered from 1), which
/// of them are new, the notes, the speakers the user named, and the meeting's language.
pub struct Evidence<'a> {
    pub lines: &'a [String],
    pub notes: &'a str,
    /// The first line that is new in this run (the ones before it were read before).
    pub new_from: usize,
    /// Names the user gave speakers.
    pub people: &'a [String],
    pub language: &'a str,
    /// Everything said in the meeting so far (a name said anywhere is backed by speech).
    pub spoken: &'a str,
    /// The text of every screen and photo so far.
    pub screens: &'a [String],
    /// Names as screens or the notes spelled them (see `shown_spelling`).
    pub spellings: &'a [String],
    /// New boxes one answer may draw.
    pub max_new: usize,
    /// The lines were picked from all over the meeting (see `closing`): the one next to a line may
    /// be from an hour later, so a flow's ends must both be in its own line.
    pub picked: bool,
}

impl<'a> Evidence<'a> {
    /// All lines new, nobody named, English, nothing else said or shown.
    #[cfg(test)]
    pub fn of(lines: &'a [String], notes: &'a str) -> Self {
        Evidence {
            lines,
            notes,
            new_from: 1,
            people: &[],
            language: "English",
            spoken: "",
            screens: &[],
            spellings: &[],
            max_new: MAX_NEW,
            picked: false,
        }
    }

    /// A name said in the meeting (in these lines or before), also as heard another way (see
    /// `heard_as`): not only read off a screen.
    fn said_anywhere(&self, name: &str) -> bool {
        let spoken_lines = self.lines.iter().filter(|l| !super::visuals::is_shown(l)).map(|l| line_text(l));
        if spoken_lines.chain(std::iter::once(self.spoken)).any(|t| name_in(name, t)) {
            return true;
        }
        let telling: Vec<String> = words(name).into_iter().filter(|w| w.chars().count() >= 4 && !PART_WORDS.contains(&w.as_str())).collect();
        let heard: std::collections::HashSet<String> = words(self.spoken).into_iter().filter(|w| w.chars().count() >= 3).collect();
        !telling.is_empty() && telling.iter().all(|t| heard.contains(t) || heard.iter().any(|h| heard_as(t, h)))
    }

    /// On how many screens (or photos) a name was shown.
    fn screens_with(&self, name: &str) -> usize {
        self.screens.iter().filter(|s| name_in(name, s)).count()
    }

    /// Whether line `l` is a shared screen's (not speech, nor a photo the user chose to add).
    fn is_screen(&self, l: usize) -> bool {
        l >= 1 && l <= self.lines.len() && self.lines[l - 1].split_once("] ").is_some_and(|(_, rest)| rest.starts_with("Screen: "))
    }

    /// The cited text, with `around` lines on either side.
    fn around(&self, cite: Cite, around: usize) -> String {
        match cite {
            Cite::Notes => self.notes.to_string(),
            Cite::Line(l) if l >= 1 && l <= self.lines.len() => {
                let from = l.saturating_sub(around + 1);
                let to = (l + around).min(self.lines.len());
                self.lines[from..to].iter().map(|x| line_text(x)).collect::<Vec<_>>().join("\n")
            }
            Cite::Line(_) => String::new(),
        }
    }

    /// Like `around`, without what was shown: a screen or a photo asks nothing and is unsure of nothing.
    fn spoken_around(&self, cite: Cite, around: usize) -> String {
        match cite {
            Cite::Line(l) if l >= 1 && l <= self.lines.len() => {
                let from = l.saturating_sub(around + 1);
                let to = (l + around).min(self.lines.len());
                self.lines[from..to].iter().filter(|x| !super::visuals::is_shown(x)).map(|x| line_text(x)).collect::<Vec<_>>().join("\n")
            }
            _ => self.around(cite, around),
        }
    }

    /// One line's text (no time, no speaker), empty when there's no such line.
    fn line(&self, l: usize) -> &str {
        if l >= 1 && l <= self.lines.len() { line_text(&self.lines[l - 1]) } else { "" }
    }

    /// The user said this line (on a call; in a room nobody is "Me", see `unlabel_me`).
    fn said_by_me(&self, l: usize) -> bool {
        l >= 1 && l <= self.lines.len() && self.lines[l - 1].split_once(": ").is_some_and(|(head, _)| head.ends_with("] Me"))
    }

    /// The cited line is one this run reads for the first time (or the notes).
    fn is_new(&self, cite: Cite) -> bool {
        match cite {
            Cite::Notes => true,
            Cite::Line(l) => l >= self.new_from && l <= self.lines.len(),
        }
    }
}

/// "[03:10] Speaker 1: text" → "text".
fn line_text(line: &str) -> &str {
    line.split_once(": ").map(|(_, t)| t).unwrap_or(line)
}

/// Words of a text, lower case, letters and digits only.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// Little words that carry no fact (English and German).
const STOP: &[&str] = &[
    "that", "this", "with", "from", "they", "their", "them", "there", "then", "than", "have", "been", "were", "will",
    "would", "into", "onto", "about", "which", "what", "when", "where", "does", "only", "also", "some", "each", "every",
    "über", "eine", "einen", "einem", "einer", "dass", "wird", "werden", "nicht", "auch", "oder", "aber", "noch", "schon",
    "sind", "haben", "eine", "dann", "wenn", "beim", "einmal",
];

/// Even littler words.
const LITTLE: &[&str] = &[
    "the", "and", "for", "was", "has", "are", "you", "our", "who", "how", "why", "not", "can", "its", "der", "die", "das",
    "und", "wer", "wie", "ist", "hat", "mit", "von", "den", "dem", "des", "zur", "zum", "vom", "per", "via", "aus", "bei",
    "ins", "ein", "to", "of", "in", "an", "on", "at", "by", "as", "zu", "im", "am", "um",
];

/// Words that name a kind of thing rather than one thing ("the system", "our team").
const GENERIC: &[&str] = &[
    "system", "systems", "service", "team", "tool", "software", "data", "server", "platform", "erp", "crm", "shop", "web",
    "webshop", "portal", "app", "application", "warehouse", "database", "inbox", "datenbank", "firma", "kunde", "kunden",
];

/// Things that move between systems, or where files are dropped, never a box of their own (file
/// formats, channels, folders): a name with one of these words is how something flows, not a node.
const NOT_NODES: &[&str] = &[
    "csv", "pdf", "xml", "json", "camt", "mt940", "excel", "xlsx", "email", "mail", "api", "file", "files", "datei",
    "dateien", "edi", "ftp", "sftp", "folder", "ordner", "export", "format", "access", "zugang", "schnittstelle", "link",
];

/// The last part of a name that makes it a piece of data, never a box: a number, a date, an amount
/// ("Bestellnummer", "Liefertermin", "Versanddatum", "order number", "delivery date"). In German the
/// last part of a compound says what it is.
const DATA_HEADS: &[&str] = &[
    "nummer", "datum", "termin", "termine", "preis", "preise", "menge", "betrag", "summe", "adresse", "frist",
    "number", "numbers", "date", "dates", "price", "prices", "amount", "quantity", "address", "deadline",
];

/// Words of a street address ("Industriestraße 12", "12 Main Street"): a letterhead on a photo
/// or a screen is no name. The German ones also as the end of a compound.
const STREETS: &[&str] = &["straße", "strasse", "gasse", "allee", "platz", "weg", "street", "road", "avenue", "lane", "boulevard"];

/// Whether a name is a format, a channel, a folder, a piece of data, an address or "Me" rather than
/// a system or person.
fn not_a_node(name: &str) -> bool {
    let w = words(name);
    // "200 orders per day": a number, not a box.
    let number = name.trim().starts_with(|c: char| c.is_ascii_digit());
    let data = w.last().is_some_and(|last| DATA_HEADS.iter().any(|h| last.ends_with(h)));
    let street = w.iter().any(|x| STREETS.iter().any(|s| x == s || (s.chars().count() >= 5 && x.ends_with(s))));
    number
        || data
        || street
        || matches!(compact(name).as_str(), "me" | "ich" | "we" | "wir")
        || w.iter().any(|x| NOT_NODES.contains(&x.as_str()))
        || NOT_NODES.contains(&compact(name).as_str())
}

/// Titles before a person's name.
const TITLES: &[&str] = &["herr", "herrn", "frau", "mr", "mrs", "ms", "miss", "dr", "doktor", "prof", "professor", "sir"];

/// Words for a team, a department or a job. Five letters or more also as the last part of a German
/// compound ("Projektleiter", "Haustechniker", "Fachabteilung").
const ROLES: &[&str] = &[
    // English
    "it", "hr", "manager", "managers", "lead", "head", "director", "boss", "owner", "ceo", "cto", "cfo", "coo", "founder",
    "admin", "administrator", "developer", "developers", "engineer", "engineers", "consultant", "advisor", "adviser",
    "accountant", "accountants", "bookkeeper", "clerk", "assistant", "secretary", "technician", "technicians", "staff",
    "team", "department", "accounting", "finance", "sales", "purchasing", "procurement", "buyer", "buyers", "support",
    "helpdesk", "legal", "marketing", "logistics", "dispatch", "controller", "controlling", "reviewer", "approver",
    "customer", "customers", "client", "clients", "supplier", "suppliers", "vendor", "vendors", "partner", "partners",
    "employee", "employees", "user", "users", "operator", "operators", "driver", "drivers", "courier", "agency",
    "carrier", "bank", "lawyer", "auditor", "planner", "dispatcher", "receptionist", "executive", "board",
    // German
    "edv", "einkauf", "einkäufer", "einkäuferin", "vertrieb", "verkauf", "verkäufer", "buchhaltung", "buchhalter",
    "buchhalterin", "geschäftsführung", "geschäftsführer", "geschäftsführerin", "vorstand", "chef", "chefin", "leiter",
    "leiterin", "leitung", "abteilung", "kundin", "lieferant", "lieferanten", "lieferantin", "techniker", "technikerin",
    "monteur", "monteure", "monteurin", "lagerist", "lageristen", "logistik", "empfang", "sekretariat", "sekretär",
    "sekretärin", "assistenz", "assistent", "assistentin", "kundendienst", "außendienst", "innendienst", "disposition",
    "disponent", "disponentin", "steuerberater", "steuerberaterin", "berater", "beraterin", "prüfer", "prüferin",
    "ersteller", "erstellerin", "anwender", "benutzer", "nutzer", "mitarbeiter", "mitarbeiterin", "sachbearbeiter",
    "sachbearbeiterin", "personalabteilung", "entwickler", "entwicklerin", "spediteur", "spedition", "bankberater",
    "inhaber", "inhaberin", "meister", "polier", "verantwortlicher", "verantwortliche", "management", "kanzlei",
];

/// Common first names (German, Austrian, English), lower case: a person box with one of these is a
/// person.
const FIRST_NAMES: &[&str] = &[
    "aaron", "adam", "adrian", "agnes", "alex", "alexa", "alexander", "alexandra", "alfred", "ali", "alice", "alina",
    "alois", "amelie", "amy", "andi", "andrea", "andreas", "andrew", "andy", "angela", "angelika", "anja", "anna",
    "anne", "annette", "anton", "antonia", "armin", "arne", "astrid", "barbara", "bastian", "beate", "ben", "benedikt",
    "benjamin", "bernd", "bernhard", "bettina", "bianca", "birgit", "björn", "bob", "brigitte", "bruno", "carina",
    "carl", "carla", "carmen", "carola", "caroline", "carsten", "charlotte", "chris", "christa", "christian",
    "christina", "christine", "christoph", "christopher", "clara", "claudia", "conny", "corinna", "cornelia", "dagmar",
    "daniel", "daniela", "dave", "david", "denise", "dennis", "dieter", "dirk", "dominik", "doris", "edith", "elena",
    "elias", "elisabeth", "elke", "ella", "emil", "emily", "emma", "eric", "erich", "erik", "erika", "ernst", "esther",
    "eva", "fabian", "felix", "ferdinand", "finn", "florian", "franz", "franziska", "frank", "fred", "friedrich",
    "gabi", "gabriele", "georg", "george", "gerald", "gerda", "gerhard", "gernot", "gertrud", "gisela", "gregor",
    "gudrun", "günter", "günther", "hannah", "hannes", "hans", "harald", "harry", "heidi", "heike", "heinz", "helena",
    "helga", "helmut", "henrik", "herbert", "hermann", "hilde", "horst", "hubert", "ines", "ingrid", "irene", "iris",
    "isabel", "isabella", "jack", "jakob", "james", "jan", "jana", "jane", "janine", "jasmin", "jennifer", "jens",
    "jessica", "jim", "joachim", "johann", "johanna", "johannes", "john", "jonas", "jonathan", "josef", "joseph",
    "julia", "julian", "juliane", "jürgen", "justin", "karin", "karl", "karla", "kate", "katharina", "kathrin",
    "katja", "katrin", "kerstin", "kevin", "kim", "klaus", "konrad", "kristina", "lara", "lars", "laura", "lea",
    "lena", "leo", "leon", "leonie", "lilly", "linda", "lisa", "lorenz", "lothar", "louis", "lucas", "lucy", "ludwig",
    "luis", "luisa", "lukas", "luca", "manfred", "manuel", "manuela", "marc", "marcel", "marco", "marcus", "margit",
    "maria", "marie", "marina", "mario", "marion", "markus", "marlene", "martin", "martina", "mary", "mathias",
    "matthew", "matthias", "max", "maximilian", "melanie", "mia", "michael", "michaela", "michelle", "mike", "mila",
    "miriam", "monika", "moritz", "nadine", "natalie", "nicole", "niklas", "nico", "nina", "noah", "norbert", "olaf",
    "oliver", "olivia", "oskar", "otto", "pascal", "patrick", "patricia", "paul", "paula", "peter", "petra", "philipp",
    "pia", "rainer", "ralf", "ralph", "regina", "reinhard", "renate", "rene", "richard", "rita", "robert", "roland",
    "rolf", "roman", "rosa", "rudolf", "ruth", "sabine", "sabrina", "sam", "samuel", "sandra", "sara", "sarah",
    "sascha", "sebastian", "silke", "silvia", "simon", "simone", "sofia", "sonja", "sophie", "stefan", "stefanie",
    "steffen", "stephan", "stephanie", "steve", "steven", "susanne", "susi", "susan", "sven", "svenja", "sylvia",
    "tanja", "theo", "thomas", "thorsten", "tim", "timo", "tobias", "tom", "toni", "torsten", "ulrich", "ulrike",
    "ursula", "uta", "ute", "uwe", "valentin", "vanessa", "vera", "verena", "viktor", "viktoria", "volker", "walter",
    "werner", "wilhelm", "william", "wolfgang", "yvonne", "zoe", "ben", "bill", "charles", "daisy", "edward", "emma",
    "grace", "henry", "jacob", "jessica", "josh", "kelly", "liam", "megan", "nancy", "nick", "olga", "oscar", "rachel",
    "rebecca", "ryan", "sean", "tina", "tony", "victor", "jeff", "jenny", "joe", "ken", "lucas", "mark", "matt", "phil",
    "rob", "sophia", "ted", "will", "anita", "elif", "mehmet", "ahmet", "fatma", "ivan", "milan", "jakub", "tomas",
];

/// Two names with different common first names ("Anna" and "Anne", "Jonas" and "Jana"): two people,
/// however alike they sound.
fn different_people(a: &str, b: &str) -> bool {
    let first = |n: &str| words(n).into_iter().find(|w| FIRST_NAMES.contains(&w.as_str()));
    first(a).zip(first(b)).is_some_and(|(x, y)| x != y)
}

fn role_word(w: &str) -> bool {
    ROLES.iter().any(|r| w == *r || (r.chars().count() >= 5 && w.ends_with(r)))
}

/// What says a person box is a person: a first name, a title (in the name or right before it in the
/// line: "Herrn Tobelmann"), or a name the user gave (in the notes, or a speaker they named).
fn person_named(name: &str, line: &str, ev: &Evidence) -> bool {
    let w = words(name);
    let first_name = |x: &String| FIRST_NAMES.contains(&x.as_str()) || x.strip_suffix('s').is_some_and(|y| FIRST_NAMES.contains(&y));
    let line = words(line);
    w.iter().any(|x| first_name(x) || TITLES.contains(&x.as_str()))
        || line.windows(2).any(|p| TITLES.contains(&p[0].as_str()) && w.contains(&p[1]))
        || ev.people.iter().any(|p| norm(p) == norm(name) || words(p).iter().any(|x| x.chars().count() >= 3 && w.contains(x)))
        || said_in(name, ev.notes)
}

/// A person box needs one of: what `person_named` looks for, or a team or job word ("Einkauf",
/// "Buchhaltung", "Projektleiter", "accounting", "sales team"). "Krankenstand" has none of them.
fn person_signal(name: &str, line: &str, ev: &Evidence) -> bool {
    person_named(name, line, ev) || words(name).iter().any(|x| role_word(x))
}

/// Words right before a name that say it is one ("die Firma Bitwerk", "called LagerPro").
const NAMING: &[&str] = &["firma", "fa", "namens", "called", "named", "company", "brand", "marke", "hersteller", "anbieter", "vendor", "provider"];
/// Company forms right after a name.
const COMPANY_FORMS: &[&str] = &["gmbh", "ag", "kg", "ohg", "se", "inc", "ltd", "llc", "corp", "plc", "co"];

/// A name written or said the way names are: "LagerPro", "die Firma Bitwerk", "Bitwerk GmbH", "a
/// company called X", or (outside German, where every noun is capitalised) "Business Central" with
/// capitals in the middle of a sentence.
fn proper_name(name: &str, line: &str, language: &str) -> bool {
    let camel = name.split_whitespace().any(|w| {
        let c: Vec<char> = w.chars().collect();
        c.windows(2).any(|p| p[0].is_lowercase() && p[1].is_uppercase())
    });
    if camel {
        return true;
    }
    // "die Firma Bitwerk", "a company called LagerPro", "Bitwerk GmbH": named as a name.
    let all = words(name);
    let (Some(first), Some(last)) = (all.first(), all.last()) else { return false };
    let said = words(line);
    let named = said.windows(2).any(|p| {
        (NAMING.contains(&p[0].as_str()) && inflected(first, &p[1])) || (inflected(last, &p[0]) && COMPANY_FORMS.contains(&p[1].as_str()))
    });
    if named {
        return true;
    }
    if language == "German" {
        return false;
    }
    let tokens: Vec<&str> = line.split_whitespace().collect();
    tokens.iter().enumerate().any(|(i, t)| {
        let bare: String = t.chars().filter(|c| c.is_alphanumeric()).collect();
        i > 0
            && bare.to_lowercase() == *first
            && bare.starts_with(char::is_uppercase)
            // A screen's rows and cells (" / ", " | ") start like sentences.
            && !tokens[i - 1].ends_with(['.', '?', '!', ':', '/', '|'])
    })
}

/// Two words match on their first five letters ("invoices" ~ "invoice", "exports" ~ "exported").
/// Loose on purpose: for facts, whose words are said in other forms.
fn same_word(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let n = 5.min(a.len()).min(b.len());
    if a.len() < 5 || b.len() < 5 {
        return a == b;
    }
    a[..n] == b[..n]
}

/// A name's word `w` is said as the word `h`: the same word but for an ending, or (five letters or
/// more) the last part of a German compound ("Dashboard" in "Einkaufsdashboard"). Not a first part:
/// "Liefertermin" is no "Lieferant".
fn said_as(w: &str, h: &str) -> bool {
    if inflected(w, h) {
        return true;
    }
    w.chars().count() >= 5
        && h.chars().count() > w.chars().count()
        && std::iter::once(h).chain(ENDINGS.iter().filter_map(|e| h.strip_suffix(e))).any(|x| x.ends_with(w))
}

/// The share of a claim's content words (four letters or more, not little words) that the
/// evidence has, also as the last part of a compound ("Bankdatei" for "die Bank schickt eine
/// Datei"); `None` when it has no content words.
fn support(claim: &str, evidence: &str) -> Option<f32> {
    let have = words(evidence);
    let content: Vec<String> = words(claim).into_iter().filter(|w| w.chars().count() >= 4 && !STOP.contains(&w.as_str())).collect();
    if content.is_empty() {
        return None;
    }
    let head = |w: &str, h: &str| h.chars().count() >= 4 && w.chars().count() > h.chars().count() && w.ends_with(h);
    let hit = content.iter().filter(|w| have.iter().any(|h| same_word(w, h) || head(w, h))).count();
    Some(hit as f32 / content.len() as f32)
}

/// Whether two texts share a word of three letters or more that says something ("API" counts).
fn shares_word(a: &str, b: &str) -> bool {
    let b = words(b);
    words(a).iter().any(|w| w.chars().count() >= 3 && !LITTLE.contains(&w.as_str()) && !STOP.contains(&w.as_str()) && b.iter().any(|x| same_word(w, x)))
}

/// Whether a name was said in `text`: as a whole (letters only), or every word of it, maybe with
/// another ending ("Herr Tobelmann" in "an Herrn Tobelmann").
fn name_in(name: &str, text: &str) -> bool {
    let c = compact(name);
    if c.chars().count() >= 3 && compact(text).contains(&c) {
        return true;
    }
    let have = words(text);
    let w = words(name);
    !w.is_empty() && w.iter().all(|x| x.chars().count() <= 1 || have.iter().any(|h| inflected(x, h)))
}

/// Whether a name is said in `text` as words: one of its telling words, as a whole word (or with an
/// ending, or as the last part of a compound), or for a name of generic words only ("the ERP") the
/// telling one of those.
fn said_in(name: &str, text: &str) -> bool {
    let have = words(text);
    let all = words(name);
    let weak = |w: &str| matches!(w, "system" | "systems" | "service" | "software" | "data" | "tool" | "app" | "application" | "platform" | "team");
    let usable = |w: &&String| w.chars().count() >= 2 && !STOP.contains(&w.as_str()) && !LITTLE.contains(&w.as_str()) && !TITLES.contains(&w.as_str());
    let mut telling: Vec<&String> = all.iter().filter(usable).filter(|w| !GENERIC.contains(&w.as_str())).collect();
    if telling.is_empty() {
        telling = all.iter().filter(usable).filter(|w| !weak(w)).collect();
    }
    if telling.is_empty() {
        return compact(name).chars().count() >= 3 && have.iter().any(|h| *h == compact(name));
    }
    telling.iter().any(|w| have.iter().any(|h| said_as(w, h)))
}

/// Whether a node is mentioned in `text`, by its name or another name it has (see `said_in`).
fn mentioned(node: &Node, text: &str) -> bool {
    names(node).any(|n| said_in(n, text))
}

/// A flow's "what" that is only one of its ends' names ("Liefertermin -> X: Liefertermin").
fn only_an_end(what: &str, ends: &[&Node]) -> bool {
    let w = compact(what);
    w.chars().count() >= 3 && ends.iter().any(|n| names(n).any(|a| {
        let a = compact(a);
        a.chars().count() >= 3 && (said_as(&w, &a) || said_as(&a, &w))
    }))
}

fn is_generic(name: &str) -> bool {
    let w = words(name);
    !w.is_empty() && w.iter().all(|x| GENERIC.contains(&x.as_str()) || matches!(x.as_str(), "our" | "the" | "unser" | "unsere" | "das" | "die" | "der"))
}

/// Whether the proper name `name` has a generic word of `generic` in it, alone or as the end of a
/// word ("Shopware shop" for "web shop", "Navision ERP" for "the ERP", "Onlineshop" for "shop").
fn keeps_word(generic: &str, name: &str) -> bool {
    let theirs: Vec<String> = words(generic).into_iter().filter(|w| GENERIC.contains(&w.as_str())).collect();
    words(name).iter().any(|h| theirs.iter().any(|w| h == w || (w.chars().count() >= 3 && h.ends_with(w.as_str())) || (h.chars().count() >= 4 && w.ends_with(h.as_str()))))
}

/// A name that is only a team, department or job ("Buchhaltung", "der Einkauf", "sales team").
fn is_role(name: &str) -> bool {
    let w = words(name);
    w.iter().any(|x| role_word(x))
        && w.iter().all(|x| role_word(x) || GENERIC.contains(&x.as_str()) || LITTLE.contains(&x.as_str()) || matches!(x.as_str(), "unser" | "unsere"))
}

/// Whether `alias` may become another name of the box `target` (an `alias` line, "also called", a
/// merge): never the name of another box or candidate; a generic word ("ERP", "the system") only for a
/// system that has no proper name yet (or gets both names in one line: `born`), a team or job word
/// only for a person or role. The model once called a supplier "ERP", and every later "ERP" line
/// went to that supplier.
fn alias_fits(map: &SystemMap, target: &Node, alias: &str, born: bool) -> bool {
    let elsewhere = map.find(alias).or_else(|| map.candidate(alias)).is_some_and(|other| other != target.id);
    let person = target.kind == Kind::Person;
    let role = is_role(alias);
    let generic = is_generic(alias) && !role;
    !elsewhere && !not_a_node(alias) && !(generic && (person || (!born && !is_generic(&target.name)))) && !(role && !person)
}

/// Whether the box (or candidate) `target`, which a new node line says is "also called" by it, may
/// be the box for `name` (of `kind`): a generic box of the same kind takes the proper name ("our ERP,
/// that's Business Central"); any other box only gets it as another name when that fits (see
/// `alias_fits`) and both are systems or both are people.
fn may_take(map: &SystemMap, target: u32, name: &str, kind: Kind) -> bool {
    let Some(t) = map.any_node(target) else { return false };
    if is_generic(&t.name) && !is_generic(name) && !t.named {
        return t.kind == kind || t.kind == Kind::Person;
    }
    (t.kind == Kind::Person) == (kind == Kind::Person) && alias_fits(map, t, name, false)
}

/// Something about it is uncertain in this line: a question, or someone doesn't know.
fn uncertain(text: &str) -> bool {
    const CUES: &[&str] = &[
        "?", "don't know", "do not know", "not sure", "no idea", "have to check", "would have to", "need to check", "never found",
        "look up", "find out", "unclear", "we'll check", "will check", "weiß ich nicht", "weiß nicht", "nachschauen",
        "nicht sicher", "keine ahnung", "müsste", "klären", "herausfinden", "prüfen",
    ];
    let t = text.to_lowercase();
    CUES.iter().any(|c| t.contains(c))
}

/// What a run changed, for the log and the replay.
#[derive(Debug, Default)]
pub struct Applied {
    pub lines: Vec<String>,
}

/// Applies a run's edits, each checked against the line it cites (or the notes):
/// - a new node's name was said there (and isn't a file format, a channel, a piece of data, "Me",
///   or a name set aside as no box); a person needs a first name, a title, a role word or a name
///   the user gave; at most `MAX_NEW` new ones per run;
/// - a new node is a hidden candidate unless it is strong (in the notes, a person's name, a proper
///   name); a candidate becomes a box when a later run cites a new line for it, or a flow goes to
///   it (past `SOFT_CAP` boxes only a flow, the notes or a strong name admit one);
/// - a fact's words are mostly there;
/// - a flow's two ends are both named within one line of it (or one there and the other the box
///   the lines just before were about), and what flows is said there and is more than an end's name;
/// - an open point cites a question, someone unsure, or the notes;
/// - an open point is solved only by a fact that is mostly in its line.
///
/// Edits on nodes the user deleted are skipped. Nodes come first, so a flow can go to a node the
/// same answer adds.
pub fn apply(map: &mut SystemMap, ops: Vec<Op>, ev: &Evidence, now_ms: u64) -> Applied {
    let mut out = Applied::default();
    // The model's node numbers -> real ids (existing nodes keep theirs).
    let mut ids: std::collections::HashMap<u32, u32> = map.nodes.iter().chain(&map.candidates).map(|n| (n.id, n.id)).collect();
    // Candidates this answer made: saying them twice in one answer doesn't count.
    let mut made: Vec<u32> = Vec::new();
    // New boxes this answer drew at once.
    let mut shown = 0;
    let (nodes, rest): (Vec<Op>, Vec<Op>) = ops.into_iter().partition(|o| matches!(o, Op::Node { .. } | Op::NotBox { .. }));
    for op in nodes.into_iter().chain(rest) {
        match op {
            Op::NotBox { name, cite } => {
                let name = cut(&name, NAME_MAX);
                // A box on the map stays (the tidy-up decides); a candidate goes.
                if name.is_empty() || map.find(&name).is_some() || !name_in(&name, &ev.around(cite, 2)) {
                    continue;
                }
                if let Some(id) = map.candidate(&name) {
                    map.candidates.retain(|c| c.id != id);
                }
                if !map.is_set_aside(&name) {
                    out.lines.push(format!("(set aside: {name})"));
                    map.set_aside(&name);
                }
            }
            Op::Node { n, name, kind, aka, cite } => {
                let name = cut(&name, NAME_MAX);
                if name.is_empty() || map.is_dropped(&name) || not_a_node(&name) {
                    continue;
                }
                if garbled(&name) {
                    out.lines.push(format!("(skipped node {name}: no name)"));
                    continue;
                }
                let line = ev.around(cite, 2);
                if !name_in(&name, &line) {
                    out.lines.push(format!("(skipped node {name}: not in its line)"));
                    continue;
                }
                let aka = aka.map(|a| cut(&a, NAME_MAX)).filter(|a| !a.is_empty() && name_in(a, &ev.around(cite, 2)));
                // Heard otherwise than a screen or the notes spell it: the shown spelling, the heard
                // one as another name.
                let heard = name.clone();
                let (name, aka) = match shown_spelling(&name, ev.spellings) {
                    Some(shown) if kind != Kind::Person => (shown, aka.or(Some(name))),
                    _ => (name, aka),
                };
                // The same node by another name, or the one generic box ("the web shop", "the warehouse
                // system") named in this line or the one before it: "which system is that?" "Shopware".
                let generic_here = || {
                    let before = match cite {
                        Cite::Line(l) if l >= 2 => format!("{}\n{}", ev.around(Cite::Line(l - 1), 0), ev.around(cite, 0)),
                        _ => ev.around(cite, 0),
                    };
                    let hits: Vec<u32> = map
                        .nodes
                        .iter()
                        .chain(&map.candidates)
                        .filter(|x| is_generic(&x.name) && !is_generic(&name) && (x.kind == kind || x.kind == Kind::Person) && name_in(&x.name, &before))
                        // The proper name keeps the generic box's word ("web shop" -> "Shopware shop"):
                        // without it, a product said next to "the ERP" (Outlook, a supplier) isn't the ERP.
                        .filter(|x| keeps_word(&x.name, &name))
                        .map(|x| x.id)
                        .collect();
                    (hits.len() == 1).then(|| hits[0])
                };
                // A name that is a candidate already is that candidate (the model may even give its
                // number), not the generic box said nearby: "Halvorsen" next to "ERP" doesn't make the ERP Halvorsen.
                let own = map.find(&name).or_else(|| map.find(&heard));
                let cand = if own.is_none() { map.candidate(&name).or_else(|| map.candidate(&heard)) } else { None };
                let by_aka = || aka.as_deref().and_then(|a| map.find(a)).filter(|&id| may_take(map, id, &name, kind));
                let known = own.or_else(by_aka).or_else(|| if cand.is_none() { generic_here() } else { None });
                if let Some(id) = known {
                    ids.insert(n, id);
                    // "also called" a generic box that takes this candidate's name: one box, not two.
                    if let Some(c) = cand.filter(|&c| c != id) {
                        if let Some(i) = map.candidates.iter().position(|x| x.id == c) {
                            let gone = map.candidates.remove(i);
                            ids.insert(c, id);
                            for k in gone.known {
                                add_known(map, id, k);
                            }
                        }
                    }
                    // A generic candidate ("the web shop") that now gets its proper name is a box.
                    map.promote(id);
                    let aka = aka.filter(|a| map.node(id).is_some_and(|node| alias_fits(map, node, a, false)));
                    let node = map.nodes.iter_mut().find(|x| x.id == id).expect("found above");
                    // "our ERP, that's Business Central": the proper name takes over from the generic one.
                    if is_generic(&node.name) && !is_generic(&name) && norm(&node.name) != norm(&name) && !node.named {
                        let old = std::mem::replace(&mut node.name, name.clone());
                        out.lines.push(format!("{old} is {name}"));
                        node.aka.push(old);
                    }
                    if let Some(a) = aka.filter(|a| norm(a) != norm(&node.name) && !node.aka.iter().any(|x| norm(x) == norm(a))) {
                        node.aka.push(a);
                    }
                    // Systems may be refiled (ours / customer / outside); a person never becomes a system,
                    // unless it was a generic name ("warehouse") that turned out to be one.
                    let was_generic_person = node.kind == Kind::Person && node.aka.iter().any(|a| is_generic(a));
                    if node.kind != kind && ((node.kind == Kind::Person) == (kind == Kind::Person) || was_generic_person) {
                        node.kind = kind;
                        out.lines.push(format!("{} is now: {}", node.name, kind.label()));
                    }
                    continue;
                }
                // Set aside before: it comes back only as a candidate (the model said otherwise once).
                let was_set_aside = map.is_set_aside(&name);
                let here = ev.around(cite, 1);
                if kind == Kind::Person && !person_signal(&name, &here, ev) {
                    out.lines.push(format!("(skipped person {name}: no name, title or role)"));
                    continue;
                }
                // Read off one screen and never said: OCR garbage, a menu, a field, an example
                // record. Such a name needs a second screen (or to be said) to be a box.
                let shown_once = matches!(cite, Cite::Line(l) if ev.is_screen(l)) && !ev.said_anywhere(&name) && ev.screens_with(&name) < 2;
                let strong = !was_set_aside
                    && !shown_once
                    && (cite == Cite::Notes
                        || said_in(&name, ev.notes)
                        || (kind == Kind::Person && person_named(&name, &here, ev))
                        || (kind == Kind::Ours && matches!(cite, Cite::Line(l) if ev.said_by_me(l)))
                        || matches!(cite, Cite::Line(l) if (l.saturating_sub(2)..=l + 2).any(|x| name_in(&name, ev.line(x)) && proper_name(&name, ev.line(x), ev.language))));
                if was_set_aside {
                    map.set_aside.retain(|d| !(norm(d) == norm(&name) || inflected(&compact(d), &compact(&name))));
                }
                // A candidate said again, in a line this run reads for the first time.
                if let Some(id) = cand.or_else(|| aka.as_deref().and_then(|a| map.candidate(a)).filter(|&c| may_take(map, c, &name, kind))) {
                    ids.insert(n, id);
                    // Said again: cited in a line read for the first time, or named in one of them (not the
                    // same answer).
                    let candidate = map.candidates.iter().find(|c| c.id == id).expect("found above");
                    let named_anew = (ev.new_from..=ev.lines.len()).any(|l| mentioned(candidate, ev.line(l)));
                    let again = (ev.is_new(cite) || named_anew) && !made.contains(&id) && map.nodes.len() < SOFT_CAP && !shown_once;
                    if (again || strong) && map.promote(id) {
                        let node = map.nodes.last_mut().expect("just promoted");
                        if node.kind != kind && (node.kind == Kind::Person) == (kind == Kind::Person) {
                            node.kind = kind;
                        }
                        out.lines.push(format!("+ {} ({}), said again", node.name, node.kind.label()));
                    }
                    continue;
                }
                // At most `MAX_NEW` new boxes drawn per run; hidden candidates only take node lines.
                if strong && shown >= ev.max_new {
                    out.lines.push(format!("(skipped node {name}: {} new ones in this run)", ev.max_new));
                    continue;
                }
                let id = map.next_id();
                ids.insert(n, id);
                made.push(id);
                let said = match cite {
                    Cite::Line(l) => cut(ev.line(l), SAID_MAX),
                    Cite::Notes => "(my notes)".to_string(),
                };
                let mut node = Node { id, name: name.clone(), kind, known: Vec::new(), aka: Vec::new(), at_ms: now_ms, named: false, said };
                // Both names in one line: a generic second name may go with a proper one.
                let aka = aka.filter(|a| alias_fits(map, &node, a, true));
                node.aka.extend(aka.clone());
                let also = aka.as_deref().map(|a| format!(", also {a}")).unwrap_or_default();
                if strong {
                    shown += 1;
                    out.lines.push(format!("+ {name} ({}){also}", kind.label()));
                    map.nodes.push(node);
                } else {
                    out.lines.push(format!("(candidate) {name} ({}){also}", kind.label()));
                    map.candidates.push(node);
                }
            }
            Op::Alias { n, name, cite } => {
                let Some(&id) = ids.get(&n) else { continue };
                let name = cut(&name, NAME_MAX);
                if !name_in(&name, &ev.around(cite, 1)) || !map.any_node(id).is_some_and(|node| alias_fits(map, node, &name, false)) {
                    out.lines.push(format!("(skipped alias {name} for {})", map.name_of(id)));
                    continue;
                }
                let Some(node) = map.any_node_mut(id) else { continue };
                if is_generic(&node.name) && !is_generic(&name) && !node.named {
                    let old = std::mem::replace(&mut node.name, name.clone());
                    out.lines.push(format!("{old} is {name}"));
                    node.aka.push(old);
                } else if norm(&node.name) != norm(&name) && !node.aka.iter().any(|x| norm(x) == norm(&name)) {
                    out.lines.push(format!("{} is also called {name}", node.name));
                    node.aka.push(name);
                }
            }
            Op::Known { n, fact, cite } => {
                let Some(&id) = ids.get(&n) else { continue };
                let fact = cut(&fact, TEXT_MAX);
                // Said there, about this node (it is named there or close by), and more than its name.
                let about = map.any_node(id).is_some_and(|node| mentioned(node, &ev.around(cite, 2)));
                let only_name = map.any_node(id).is_some_and(|node| words(&fact).iter().all(|w| words(&node.name).contains(w)));
                if !about || only_name || support(&fact, &ev.around(cite, 1)).is_none_or(|s| s < 0.5) {
                    out.lines.push(format!("(skipped fact on {}: not in its line) {fact}", map.name_of(id)));
                    continue;
                }
                if !in_language(&fact, ev.language) {
                    out.lines.push(format!("(skipped fact on {}: not in {}) {fact}", map.name_of(id), ev.language));
                    continue;
                }
                if add_known(map, id, fact.clone()) && map.node(id).is_some() {
                    out.lines.push(format!("{}: {fact}", map.name_of(id)));
                }
            }
            Op::Flow { from, to, what, how, cite } => {
                let (Some(&a), Some(&b)) = (ids.get(&from), ids.get(&to)) else { continue };
                let (Some(na), Some(nb)) = (map.any_node(a), map.any_node(b)) else { continue };
                if a == b {
                    continue;
                }
                let (what, how) = (cut(&what, TEXT_MAX), cut(&how, 40));
                // "API" when nobody said how: unknown.
                let how = if unknown_how(&how) || how_said(&how, &ev.around(cite, 2)) { how } else { hows(ev_language(&how)).last().copied().unwrap_or("unknown").to_string() };
                let label = format!("{} -> {}: {what}, {how}", na.name, nb.name);
                let ends = flow_ends(map, na, nb, ev, cite);
                let echo = only_an_end(&what, &[na, nb]);
                // A flow that brings a candidate onto the map needs something said to move, in the cited
                // line itself (a line is a whole turn: one line away can be another topic).
                let still = (map.node(a).is_none() || map.node(b).is_none()) && !moves(&ev.around(cite, 0));
                // A candidate read off one screen only comes in with a flow when it is backed too.
                let unbacked = [na, nb].iter().any(|n| map.node(n.id).is_none() && !ev.said_anywhere(&n.name) && ev.screens_with(&n.name) < 2);
                let denied = withheld(&what, &ev.spoken_around(cite, 0));
                let foreign = !in_language(&what, ev.language);
                if what.is_empty() || says_unknown(&what) || !ends || echo || still || unbacked || denied || foreign || support(&what, &ev.around(cite, 1)).is_some_and(|s| s == 0.0) {
                    let why = if echo {
                        "what flows is an end's name"
                    } else if denied {
                        "said not to flow"
                    } else if foreign {
                        "not in the meeting's language"
                    } else if unbacked {
                        "an end only shown once"
                    } else if still && ends {
                        "nothing moves there"
                    } else {
                        "not in its lines"
                    };
                    out.lines.push(format!("(skipped flow {label}: {why})"));
                    continue;
                }
                for id in [a, b] {
                    if map.promote(id) {
                        out.lines.push(format!("+ {} ({}), in a flow", map.name_of(id), map.node(id).map(|n| n.kind.label()).unwrap_or("")));
                    }
                }
                let same = map.edges.iter_mut().find(|e| ((e.from, e.to) == (a, b) || (e.from, e.to) == (b, a)) && similar(&e.what, &what));
                match same {
                    Some(e) => {
                        if unknown_how(&e.how) && !unknown_how(&how) {
                            e.how = how.clone();
                            out.lines.push(format!("{} -> {}: now {how}", map.name_of(a), map.name_of(b)));
                        }
                    }
                    None => {
                        out.lines.push(label);
                        map.edges.push(Edge { from: a, to: b, what, how });
                    }
                }
            }
            Op::Open { n, text, cite } => {
                let Some(&id) = ids.get(&n) else { continue };
                if map.node(id).is_none() {
                    continue;
                }
                let text = cut(&text, TEXT_MAX);
                // Someone asked or wasn't sure (in that line or next to it), or the notes want it.
                let wanted = cite == Cite::Notes || support(&text, ev.notes).is_some_and(|s| s >= 0.5);
                if !(wanted || uncertain(&ev.spoken_around(cite, 1))) {
                    out.lines.push(format!("(skipped open point on {}: nothing uncertain in its line) {text}", map.name_of(id)));
                    continue;
                }
                let repeat = map.open.iter().any(|o| similar(&o.text, &text))
                    || map.node(id).is_some_and(|node| node.known.iter().any(|k| similar(k, &text)));
                if text.is_empty() || repeat || map.open.len() >= MAX_OPEN {
                    continue;
                }
                if !in_language(&text, ev.language) {
                    out.lines.push(format!("(skipped open point on {}: not in {}) {text}", map.name_of(id), ev.language));
                    continue;
                }
                map.opened += 1;
                let number = map.opened;
                out.lines.push(format!("O{number} {}: {text}", map.name_of(id)));
                map.open.push(OpenPoint { number, node: id, text });
            }
            Op::Solved { number, fact, cite } => {
                let Some(i) = map.open.iter().position(|o| o.number == number) else { continue };
                let fact = cut(&fact, TEXT_MAX);
                // Said there, and an answer to this point (they share a word), not to another one.
                let answers = shares_word(&map.open[i].text, &fact);
                // "I'd have to look up whether the API is on" answers nothing.
                let unsure = matches!(cite, Cite::Line(_)) && uncertain(&ev.spoken_around(cite, 0));
                if says_unknown(&fact) || !answers || unsure || !in_language(&fact, ev.language) || support(&fact, &ev.around(cite, 1)).is_none_or(|s| s < 0.5) {
                    out.lines.push(format!("(O{number} not solved: no answer in its line) {fact}"));
                    continue;
                }
                let o = map.open.remove(i);
                out.lines.push(format!("O{number} solved: {fact}"));
                add_known(map, o.node, fact);
            }
        }
    }
    out.lines.extend(merge_variants(map, ev.spellings));
    out
}

/// Boxes that are one thing heard or shown in different ways become one ("Navision", "Navisjon
/// Modul", "Navision Core"; see `variant`), and a box heard otherwise than a screen or the notes
/// spell it takes that spelling (see `shown_spelling`), the heard name kept as another name. The
/// older box stays; a person and a system, two people with different first names, or two boxes
/// the user named are never joined. Boxes the user named keep their names.
fn merge_variants(map: &mut SystemMap, spellings: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..map.nodes.len() {
        let node = &map.nodes[i];
        let Some(shown) = shown_spelling(&node.name, spellings).filter(|_| !node.named && node.kind != Kind::Person) else { continue };
        // Another box with that name: one box, joined below.
        if map.nodes.iter().any(|x| x.id != node.id && names(x).any(|a| norm(a) == norm(&shown))) {
            continue;
        }
        let node = &mut map.nodes[i];
        let old = std::mem::replace(&mut node.name, shown.clone());
        out.push(format!("{old} is {shown} (as shown)"));
        node.aka.retain(|a| norm(a) != norm(&shown));
        node.aka.push(old);
    }
    let canon = |x: &str| shown_spelling(x, spellings).unwrap_or_else(|| x.to_string());
    let mut refused: Vec<(u32, u32)> = Vec::new();
    loop {
        let pair = map.nodes.iter().enumerate().find_map(|(i, a)| {
            map.nodes[i + 1..]
                .iter()
                .find(|b| {
                    (a.kind == Kind::Person) == (b.kind == Kind::Person)
                        && !(a.named && b.named)
                        && !refused.contains(&(a.id, b.id))
                        && names(a).any(|x| names(b).any(|y| variant(&canon(x), &canon(y))))
                })
                .map(|b| (a.id, b.id, b.named && !a.named))
        });
        let Some((older, newer, keep_newer)) = pair else { break };
        let (from, into) = if keep_newer { (older, newer) } else { (newer, older) };
        let (from_name, into_name) = (map.name_of(from).to_string(), map.name_of(into).to_string());
        if !map.merge(from, into) {
            refused.push((older, newer));
            continue;
        }
        // The name a screen or the notes showed wins over a heard one.
        let node = map.nodes.iter_mut().find(|n| n.id == into).expect("merged into it");
        let is_shown = |a: &str| spellings.iter().any(|s| head(s) == head(a));
        let shown = node.aka.iter().position(|a| is_shown(a));
        if let (Some(k), false) = (shown, node.named || is_shown(&node.name)) {
            let name = node.aka.remove(k);
            let old = std::mem::replace(&mut node.name, name);
            node.aka.push(old);
        }
        out.push(format!("{from_name} is {into_name} (heard alike)"));
    }
    out
}

/// A name OCR or speech recognition made up rather than one somebody uses: an underscore (a file or
/// field name, "IGE_X_Diag"), or a lone lower-case letter between words ("Diag r am").
fn garbled(name: &str) -> bool {
    name.contains('_') || name.split_whitespace().any(|w| w.chars().count() == 1 && w.chars().all(char::is_lowercase))
}

/// Whether a text is written in `language` as far as its little words tell (German and English; in
/// another language, or with too few little words, it passes). The model sometimes answers in the
/// language of an English screen or of the worked example.
fn in_language(text: &str, language: &str) -> bool {
    const DE: &[&str] = &[
        "der", "die", "das", "und", "ist", "nicht", "mit", "von", "für", "wird", "werden", "ein", "eine", "einen", "im", "auf",
        "zu", "den", "dem", "sind", "wie", "wer", "welche", "welcher", "welches", "wo", "aus", "bei", "nach", "auch", "kann",
        "hat", "es", "vom", "zum", "zur", "oder", "wenn", "noch",
    ];
    const EN: &[&str] = &[
        "the", "is", "are", "and", "of", "to", "for", "with", "from", "by", "on", "it", "this", "that", "which", "what", "how",
        "who", "can", "be", "has", "have", "does", "into", "used", "when", "where", "a", "or", "its", "their",
    ];
    let w = words(text);
    let de = w.iter().filter(|x| DE.contains(&x.as_str())).count();
    let en = w.iter().filter(|x| EN.contains(&x.as_str())).count();
    match language {
        "German" => !(en >= 2 && en > de),
        "English" => !(de >= 2 && de > en),
        _ => true,
    }
}

/// Whether the line says that what would flow is left out ("die Preise vorher löschen", "ohne
/// Preise", "without prices"): one of its words within four words of a word for without or removing.
fn withheld(what: &str, line: &str) -> bool {
    const WITHOUT: &[&str] = &["ohne", "kein", "keine", "keinen", "keiner", "without", "no", "none"];
    const REMOVE: &[&str] = &["lösch", "entfern", "streich", "gestrich", "weglass", "weggelass", "remov", "strip", "delet", "exclud"];
    let line = words(line);
    let content: Vec<String> = words(what).into_iter().filter(|w| w.chars().count() >= 4 && !STOP.contains(&w.as_str())).collect();
    line.iter().enumerate().any(|(i, w)| {
        content.iter().any(|c| same_word(c, w))
            && line[i.saturating_sub(4)..(i + 5).min(line.len())].iter().any(|x| WITHOUT.contains(&x.as_str()) || REMOVE.iter().any(|r| x.contains(r)))
    })
}

/// Words that say something moves (German and English): sending, going, importing, reading, typing
/// in, a format or a channel. Whole words: as word starts, "liefert" would match "Liefertermin" and
/// "import" "important".
const MOVES: &[&str] = &[
    "geht", "gehen", "ging", "kommt", "kommen", "kam", "gekommen", "importiert", "importieren", "exportiert",
    "exportieren", "lädt", "lade", "geladen", "holt", "hole", "holen", "geholt", "liest", "lesen", "gelesen",
    "schreibt", "geschrieben", "bekommt", "bekommen", "kriegt", "kriegen", "erhält", "erhalten", "landet", "landen",
    "trägt", "tippt", "tippen", "zieht", "ziehen", "gezogen", "zurück", "liefert", "liefern", "geliefert", "zeigt",
    "legt", "ablegen", "send", "sends", "sent", "go", "goes", "went", "come", "comes", "came", "import", "imports",
    "imported", "export", "exports", "exported", "load", "loads", "loaded", "pull", "pulls", "push", "pushes",
    "read", "reads", "write", "writes", "written", "get", "gets", "got", "passes", "passed", "feeds", "flows",
    "lands", "sync", "syncs", "type", "types", "typed", "enter", "enters", "entered", "stores", "saves", "shows",
    "copy", "copies", "copied", "email", "mail", "mails", "pdf", "csv", "xml", "datei", "file", "files", "api",
    "schnittstelle", "link", "ftp", "edi", "läuft", "laufen", "fließt", "rüber", "hoch",
];

/// Word starts that only verbs of moving have (German separable verbs and their forms).
const MOVE_STARTS: &[&str] = &[
    "schick", "geschickt", "versend", "gesendet", "send", "verschick", "einspiel", "eingespielt", "einles",
    "eingelesen", "einspeis", "importier", "exportier", "hochgelad", "übertr", "überspiel", "hochlad", "hochgelad", "abhol", "übernimm",
    "übernehm", "übernomm", "weitergeb", "weitergeg", "weiterleit", "weitergeleit", "auslesen", "ausgelesen",
    "empfang", "synchronis", "abgleich", "abgeglich", "eintrag", "eingetrag", "abtipp", "speicher", "abgelegt",
    "abruf", "abgeruf", "upload", "download", "receiv", "forward", "deliver", "fetch",
];

/// Whether the text says something moves (see `MOVES`, `MOVE_STARTS`).
fn moves(text: &str) -> bool {
    words(text).iter().any(|w| MOVES.contains(&w.as_str()) || MOVE_STARTS.iter().any(|m| w.starts_with(m)))
}

/// Both ends of a flow are named within one line of the line it cites, or one is named in that line
/// and the other is the clear subject: the only box (or candidate) named in the nearest line before
/// it that names one.
fn flow_ends(map: &SystemMap, a: &Node, b: &Node, ev: &Evidence, cite: Cite) -> bool {
    if ev.picked {
        let here = ev.around(cite, 0);
        return mentioned(a, &here) && mentioned(b, &here);
    }
    let near = ev.around(cite, 1);
    if mentioned(a, &near) && mentioned(b, &near) {
        return true;
    }
    let Cite::Line(l) = cite else { return false };
    let here = ev.around(cite, 0);
    let subject = |other: &Node| {
        (1..=3).filter_map(|back| l.checked_sub(back).filter(|x| *x >= 1)).find_map(|before| {
            let text = ev.line(before);
            let named: Vec<u32> = map.nodes.iter().chain(&map.candidates).filter(|x| mentioned(x, text)).map(|x| x.id).collect();
            (!named.is_empty()).then(|| named == [other.id])
        }) == Some(true)
    };
    (mentioned(a, &here) && subject(b)) || (mentioned(b, &here) && subject(a))
}

fn add_known(map: &mut SystemMap, id: u32, fact: String) -> bool {
    let Some(node) = map.any_node_mut(id) else { return false };
    if fact.is_empty() || node.known.iter().any(|k| similar(k, &fact)) || node.known.len() >= MAX_KNOWN {
        return false;
    }
    node.known.push(fact);
    true
}

/// "…not specified — remains open": a small model "solves" open points with that too.
fn says_unknown(fact: &str) -> bool {
    const NOT: &[&str] = &[
        "unknown", "not specified", "not said", "not mentioned", "no mention", "remains open", "still open", "unclear",
        "not clear", "not confirmed", "unconfirmed", "no flow", "unbekannt", "unklar", "offen", "nicht genannt", "nicht bekannt",
    ];
    let f = fact.to_lowercase();
    NOT.iter().any(|n| f.contains(n))
}

fn unknown_how(how: &str) -> bool {
    matches!(norm(how).as_str(), "" | "?" | "unknown" | "unbekannt" | "unklar" | "not said" | "nicht gesagt" | "inconnu" | "desconocido")
}

/// Whether the talk near a flow says how it moves (or words for it: "email" for mail, "CSV" or
/// "folder" for a file, "manually" for by hand).
fn how_said(how: &str, near: &str) -> bool {
    let t = near.to_lowercase();
    let cues: &[&str] = match norm(how).as_str() {
        "api" | "schnittstelle" => &["api", "schnittstelle", "interface", "service layer", "service-layer"],
        "csv" => &["csv"],
        "file" | "datei" => &["file", "csv", "folder", "export", "datei", "ordner", "upload", "hochlad", "lade"],
        "mail" => &["mail"],
        "by hand" | "von hand" => &["hand", "manual", "typ", "tipp", "abtipp"],
        "phone" | "telefon" => &["phone", "call", "telefon", "anruf"],
        "database" | "datenbank" => &["database", "datenbank", "sql"],
        _ => return true,
    };
    cues.iter().any(|c| t.contains(c))
}

/// The language whose list of hows a how comes from (for its word for "unknown").
fn ev_language(how: &str) -> &'static str {
    if hows("German").iter().any(|h| norm(h) == norm(how)) && !hows("English").iter().any(|h| norm(h) == norm(how)) { "German" } else { "English" }
}

/// The ways data moves, as the answer may name them (a fixed list, in the meeting's language).
fn hows(language: &str) -> &'static [&'static str] {
    match language {
        "German" => &["API", "CSV", "Datei", "Mail", "von Hand", "Telefon", "Datenbank", "unbekannt"],
        _ => &["API", "CSV", "file", "mail", "by hand", "phone", "database", "unknown"],
    }
}

/// Reads the model's answer (see `grammar`). Lenient about what the grammar can't rule out.
pub fn parse(answer: &str) -> Vec<Op> {
    let n = |s: &str| s.trim().trim_start_matches('N').parse::<u32>().ok();
    let mut ops = Vec::new();
    for line in answer.lines().map(str::trim).filter(|l| !l.is_empty()) {
        // "… [L4]" / "… [notes]"
        let Some((body, cite)) = line.strip_suffix(']').and_then(|l| l.rsplit_once(" [")) else { continue };
        let cite = match cite {
            "notes" => Cite::Notes,
            l => match l.strip_prefix('L').and_then(|x| x.parse().ok()) {
                Some(l) => Cite::Line(l),
                None => continue,
            },
        };
        let Some((head, rest)) = body.split_once(": ") else { continue };
        let mut head_words = head.split_whitespace();
        let (verb, arg) = (head_words.next().unwrap_or(""), head_words.collect::<Vec<_>>().join(" "));
        let op = match verb {
            "node" => {
                let (main, aka) = match rest.split_once(" also called ") {
                    Some((m, a)) => (m, Some(a.trim().to_string())),
                    None => (rest, None),
                };
                main.strip_suffix(')').and_then(|m| m.rsplit_once(" (")).and_then(|(name, kind)| {
                    let name = name.trim().to_string();
                    if NOT_BOX_WORDS.contains(&kind.trim()) {
                        return Some(Op::NotBox { name, cite });
                    }
                    Some(Op::Node { n: n(&arg)?, name, kind: Kind::from_word(kind)?, aka, cite })
                })
            }
            "alias" => n(&arg).map(|n| Op::Alias { n, name: rest.trim().to_string(), cite }),
            "known" => n(&arg).map(|n| Op::Known { n, fact: rest.trim().to_string(), cite }),
            "open" => n(&arg).map(|n| Op::Open { n, text: rest.trim().to_string(), cite }),
            "solved" => arg.trim_start_matches('O').parse().ok().map(|number| Op::Solved { number, fact: rest.trim().to_string(), cite }),
            "flow" => arg.split_once(" -> ").and_then(|(a, b)| {
                let (what, how) = rest.split_once(" | how: ")?;
                Some(Op::Flow { from: n(a)?, to: n(b)?, what: what.trim().to_string(), how: how.trim().to_string(), cite })
            }),
            _ => None,
        };
        ops.extend(op);
    }
    ops
}

fn alternatives(items: impl IntoIterator<Item = String>) -> String {
    let all: Vec<String> = items.into_iter().map(|i| format!("\"{}\"", i.replace('\\', "\\\\").replace('"', "\\\""))).collect();
    if all.is_empty() { "\"\"".into() } else { format!("({})", all.join(" | ")) }
}

/// The kinds a node line may give: the four kinds of box, and the kinds that are no box.
fn kinds(room: Room) -> String {
    alternatives(Kind::ALL.iter().map(|k| k.word(room).to_string()).chain(NOT_BOX_WORDS.iter().map(|w| w.to_string())))
}

/// Node lines and other lines a run's answer may have.
const ANSWER_SIZE: (usize, usize) = (NODE_LINES, MAX_LINES);

/// The answer's form: up to `size.0` "node" lines first, then up to `size.1` other lines, each
/// ending in the line it comes from ("[L4]", "[notes]"); with `CLOSING_SIZE`, the other lines are
/// flows only. Node numbers, line numbers, open point
/// numbers, kinds and the kinds of "how" are fixed lists, so the model can only point at what
/// exists; a kind may be "topic", "data or document" or "feature or process", for a name that is no
/// box. Each text has a word limit and words at most 20 letters (left free, a small model goes on
/// inside a field until it runs out of tokens, or runs words together at the limit), and no CJK
/// or full-width characters (it drifts into them). The answer may end right after its last line:
/// made to write a line break first, it pads the line with spaces instead.
fn grammar(existing: &[u32], new: &[u32], lines: usize, open: &[u32], language: &str, room: Room, size: (usize, usize)) -> String {
    let ids = alternatives(existing.iter().chain(new).map(|n| format!("N{n}")));
    let new_ids = alternatives(new.iter().map(|n| format!("N{n}")));
    let cites = alternatives((1..=lines).map(|l| format!("L{l}")).chain(["notes".to_string()]));
    let opens = alternatives(open.iter().map(|o| format!("O{o}")));
    let solved = if open.is_empty() { "" } else { " | solved" };
    let items = if size == CLOSING_SIZE { "flow".to_string() } else { format!("alias | known | flow | open{solved}") };
    let how = alternatives(hows(language).iter().map(|h| h.to_string()));
    format!(
        "root ::= (node \"\\n\"){{0,{nodes}}} (item (\"\\n\" item){{0,{more}}} \"\\n\"?)?\n\
         node ::= \"node \" {new_ids} \": \" name \" (\" kind \")\" (\" also called \" name)? cite\n\
         kind ::= {kinds}\n\
         item ::= {items}\n\
         alias ::= \"alias \" id \": \" name cite\n\
         known ::= \"known \" id \": \" text cite\n\
         flow ::= \"flow \" id \" -> \" id \": \" text \" | how: \" how cite\n\
         open ::= \"open \" id \": \" qtext \"?\" cite\n\
         solved ::= \"solved \" {opens} \": \" text cite\n\
         id ::= {ids}\n\
         how ::= {how}\n\
         cite ::= \" [\" {cites} \"]\"\n\
         name ::= nchar{{1,24}} (\" \" nchar{{1,20}}){{0,4}}\n\
         nchar ::= [^\\[\\]|\\n:()<>? \\u3000-\\u9fff\\uff00-\\uffef]\n\
         text ::= word (\" \" word){{0,13}}\n\
         word ::= [^\\[\\]|\\n<> \\u3000-\\u9fff\\uff00-\\uffef]{{1,20}}\n\
         qtext ::= qword (\" \" qword){{0,15}}\n\
         qword ::= [^\\[\\]|\\n<>? \\u3000-\\u9fff\\uff00-\\uffef]{{1,20}}\n",
        nodes = size.0,
        more = size.1 - 1,
        kinds = kinds(room),
    )
}

// ---------- prompt ----------

pub struct Input<'a> {
    pub notes: &'a str,
    pub language: Option<&'a str>,
    /// Talk since the last run.
    pub new: &'a [Segment],
    /// Talk the runs before read (the spoken backing of a name, see `Evidence::said_anywhere`).
    pub said_before: &'a [Segment],
    /// Screens and photos since the last run: "[mm:ss] Screen: …" lines among the new ones, with
    /// only the rows not shown before (see `fresh_rows`).
    pub visuals: &'a [super::Visual],
    /// Screens and photos the runs before read.
    pub shown_before: &'a [super::Visual],
    /// Names as the screens and the notes spell them (the meeting's vocabulary).
    pub spellings: &'a [String],
    /// The last lines before it (already read), for context.
    pub earlier: &'a [String],
    pub speakers: &'a [Speaker],
    pub map: &'a SystemMap,
    /// The notes changed since the last run (then it runs even without new talk).
    pub notes_changed: bool,
    /// On a call (the others on system audio) or all in one room on the microphone.
    pub room: Room,
}

/// What one run reads of a screen: at most this many characters of the rows it hasn't shown yet,
/// all screens of the run together at most `SCREEN_CHARS` (and half the talk's length).
const SCREEN_CAPS: super::visuals::Caps = super::visuals::Caps { per_visual: 700, total: SCREEN_CHARS, ..super::visuals::MAP };
const SCREEN_CHARS: usize = 1_000;
/// Rows this alike (bigrams of their letters, Dice) are one row read twice: OCR reads a slide a
/// little differently each time ("Roadmap und Meilenst", "Roadmap und Mellenst").
const SAME_ROW: f32 = 0.7;

/// The letter pairs of a row, lower case, letters and digits only.
fn bigrams(row: &str) -> std::collections::HashSet<(char, char)> {
    let c: Vec<char> = compact(row).chars().collect();
    c.windows(2).map(|p| (p[0], p[1])).collect()
}

/// The visuals of this run with only the rows no visual before showed (nor one earlier in this
/// run): a slide read again with OCR jitter, the same sidebar, a form with one field changed add
/// only what changed. Visuals with nothing new are left out; photos are kept whole (the user chose
/// them).
fn fresh_rows(visuals: &[super::Visual], before: &[super::Visual]) -> Vec<super::Visual> {
    let mut seen: Vec<std::collections::HashSet<(char, char)>> = Vec::new();
    let mut exact: std::collections::HashSet<String> = std::collections::HashSet::new();
    let remember = |row: &str, seen: &mut Vec<_>, exact: &mut std::collections::HashSet<String>| {
        if exact.insert(compact(row)) {
            seen.push(bigrams(row));
        }
    };
    for v in before {
        for row in v.text.lines() {
            remember(row, &mut seen, &mut exact);
        }
    }
    let mut out = Vec::new();
    for v in visuals {
        if v.source == super::visuals::Source::Photo {
            out.push(v.clone());
            continue;
        }
        let mut fresh: Vec<&str> = Vec::new();
        for row in v.text.lines().map(str::trim).filter(|r| compact(r).chars().count() >= 3) {
            let b = bigrams(row);
            let known = exact.contains(&compact(row))
                || seen.iter().any(|s| 2.0 * s.intersection(&b).count() as f32 >= SAME_ROW * (s.len() + b.len()) as f32);
            if !known {
                fresh.push(row);
            }
            remember(row, &mut seen, &mut exact);
        }
        if !fresh.is_empty() {
            out.push(super::Visual { text: fresh.join("\n"), ..v.clone() });
        }
    }
    out
}

/// In a room, the microphone's first voice isn't the user: no line says "Me".
pub fn unlabel_me(line: String, room: Room) -> String {
    match room {
        Room::OneMic => match line.split_once("] Me: ") {
            Some((time, text)) => format!("{time}] Speaker 0: {text}"),
            None => line,
        },
        Room::Call => line,
    }
}

/// The map's boxes as the model reads them: every box (number, name, kind, other names) in the order
/// they came, so a new box only adds to the end and the helper keeps what came before (it comes first
/// in the prompt; see `user_prompt`).
fn boxes_text(map: &SystemMap, room: Room) -> String {
    if map.nodes.is_empty() {
        return "(empty)\n".to_string();
    }
    let mut s = String::new();
    for n in &map.nodes {
        s.push_str(&format!("N{} {} ({})", n.id, n.name, n.kind.word(room)));
        if !n.aka.is_empty() {
            s.push_str(&format!(", also called {}", n.aka.iter().take(3).cloned().collect::<Vec<_>>().join(", ")));
        }
        s.push('\n');
    }
    s
}

/// What the map has on what a run reads, small however big the map grows: facts, flows and open
/// points only of the boxes `talk` (the new lines and the notes) names, and the open points it
/// touches; last the candidates `all_talk` (all the lines the run shows, and the notes) names.
fn news_text(map: &SystemMap, talk: &str, all_talk: &str, room: Room) -> String {
    let maybe = named_candidates(map, all_talk);
    let maybe_text = if maybe.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = maybe.iter().map(|n| format!("N{} {} ({})", n.id, n.name, n.kind.word(room))).collect();
        format!("Said before, not on the map (no box unless the new lines show it is a system, service or person): {}\n", list.join(", "))
    };
    let mut s = String::new();
    let here: Vec<&Node> = map.nodes.iter().filter(|n| mentioned(n, talk)).collect();
    let open: Vec<&OpenPoint> = map.open.iter().filter(|o| here.iter().any(|n| n.id == o.node) || shares_word(&o.text, talk)).collect();
    let mut details = String::new();
    for n in &here {
        for k in &n.known {
            details.push_str(&format!("N{} known: {k}\n", n.id));
        }
        for e in map.edges.iter().filter(|e| e.from == n.id || e.to == n.id) {
            details.push_str(&format!("flow N{} -> N{}: {} | how: {}\n", e.from, e.to, e.what, e.how));
        }
    }
    for o in open {
        details.push_str(&format!("open O{} on N{}: {}\n", o.number, o.node, o.text));
    }
    if !details.is_empty() {
        let mut seen = std::collections::HashSet::new();
        let details: String = details.lines().filter(|l| seen.insert(l.to_string())).map(|l| format!("{l}\n")).collect();
        s.push_str(&format!("What the map has on the boxes the new talk names:\n{details}"));
    }
    s.push_str(&maybe_text);
    s
}

/// The candidates the new lines (or the notes) name, the latest ones: the model sees them with their
/// numbers, so it can confirm one (a node line, a flow) instead of losing it.
fn named_candidates<'a>(map: &'a SystemMap, talk: &str) -> Vec<&'a Node> {
    let named: Vec<&Node> = map.candidates.iter().filter(|n| mentioned(n, talk)).collect();
    named[named.len().saturating_sub(MAX_SHOWN_CANDIDATES)..].to_vec()
}

/// The new lines' text and the notes: what a run's prompt shows facts and candidates for.
fn talk_of(new: &[String], notes: &str) -> String {
    format!("{}\n{notes}", new.iter().map(|l| line_text(l)).collect::<Vec<_>>().join("\n"))
}

/// Instructions and a worked example; the same in every run of a meeting (it comes first, so a
/// helper that keeps the start of the prompt doesn't read it again).
fn system_prompt(language: &str, room: Room) -> String {
    let hows = hows(language).join(", ");
    let (who, sides, asker) = match room {
        Room::Call => (
            "\"Me\", the person who recorded it",
            "\"customer system\" runs at the organisation Me is talking to (when they say \"our ERP\" or \"our shop\", \
that is a customer system); \"Me's system\" is only what Me's own company builds or runs (Me says \"we are building\")",
            "Me",
        ),
        Room::OneMic => (
            "me, the person who recorded it. Everyone sat in one room around one microphone, so the speaker labels \
don't tell who is on which side, and any speaker may be me",
            "\"my company's system\" is what my company builds, runs or delivers to the others (the tool or project the \
meeting is about: \"we built\", \"the new tool\", \"what you will get\"; my notes may name it); \"customer system\" is \
what the organisation we visit already uses (their ERP, their mail, their Excel lists), even when they say \"our\"",
            "Speaker 2",
        ),
    };
    format!(
        "You keep a map of a meeting for {who}: the software systems, outside services and people that come up, \
what data flows between them and how, and what is still unknown. The transcript comes from speech recognition and \
has mistakes; a word that makes no sense is misheard, not a box.

A box is one of these, and nothing else:
- a software system, tool, app, platform or online service, with a name (\"Business Central\", \"Shopware\", \
\"DATEV\", \"Outlook\") or a clear role (\"the web shop\", \"the ERP\", \"the mail inbox\");
- an organisation that provides a service (a carrier, a bank, an IT provider, a tax advisor);
- a real person, team or role who does something with the systems (\"Petra\", \"Frau Huber\", \"accounting\", \
\"der Einkauf\", \"the warehouse staff\").
Kinds of box: {sides}; \"outside service\" belongs to a third party (a carrier, a bank, a hosting agency, a tax \
advisor and their software); \"person or role\" is a person, team or role.
Not a box (if you started a node line for one, give it this kind and it is set aside):
- \"topic\": subjects, problems, qualities, events, absences, plans (Krankenstand, Urlaub, Weihnachten, \
Bestellqualität, Rückstand, sick leave, Christmas, delays, feedback);
- \"data or document\": fields, numbers, dates, business objects, documents (Liefertermin, Bestellnummer, \
Angebot, Auftragsbestätigung, Rechnung, Vertrag, order number, delivery date, invoice, contract); they are what flows; \
also the things the systems describe: machines, plants, vehicles, buildings and their parts, product types, and \
the example records a demo shows (Maschine, Anlage, Fahrzeug, Bauteil, a machine, a plant, a part, one customer's order);
- \"feature or process\": a function, screen, step or rule inside a system (Freigabe, Erinnerung, Texteditor, \
Import, approval, reminder, search, the check).
A file format (CSV, PDF, XML), a channel (mail, API), a shared folder or a file is never a node: it is how something \
flows between two nodes.

Each run you get the boxes so far (numbered N1, N2, …, open points O1, O2, …), my notes, and the talk as numbered \
lines; the lines before the new ones were read already. Write only what the new lines (and my notes) add, one \
change per line, each ending with the line it comes from in brackets ([L4], or [notes]):
node N7: Name (kind) [L2]   a new box; use the new numbers offered. Add \"also called X\" before the bracket when \
the talk gives it a second name (\"our ERP, that's Business Central\").
alias N2: other name [L5]   a box on the map is called something else too.
known N2: a short fact about it [L3]   only what the line says.
flow N2 -> N7: what flows | how: one of {hows} [L4]   from where the data leaves to where it arrives, only when \
the talk says something moves between those two. Two things named in one sentence are not a flow; what flows is \
data (orders, invoices), never one of the two names.
open N2: a short question that is still open? [L6]   only when someone asks it and it isn't answered, someone \
isn't sure, or my notes want it found out; give each thing my notes want found out its own open point.
solved O3: the fact that answers it [L8]   when a new line answers an open point.
Nodes first, then the other lines. Use the node numbers, never names, after \"node\". Never invent: if it isn't \
in the line you cite, leave it out. Most lines add nothing to the map; then write nothing. Write facts, flows and \
open points in {language}, also when a line or a screen is in another language; keep names as they were said, or \
as a screen spells them. A box a new line names by another spelling (heard wrong) is that box: use its number.

Example.
Map so far:
N1 Webshop (customer system)
Talk (L1 was read before, L2 on are new):
L1 [03:10] Speaker 1: Our webshop takes the orders.
L2 [03:14] Speaker 1: Every hour the orders go to Navision as an XML file.
L3 [03:20] {asker}: Who looks at failed imports?
L4 [03:23] Speaker 1: Nobody, I think. Petra from finance also works in Navision, on the invoices.
L5 [03:31] Speaker 1: The delivery date is often wrong since Christmas, half the team is on sick leave.
L6 [03:40] Speaker 1: Yes, exactly, we have to look at that again.
New node numbers: N2, N3, N4.
Answer:
node N2: Navision (customer system) [L2]
node N3: Petra from finance (person or role) [L4]
node N4: delivery date (data or document) [L5]
flow N1 -> N2: orders, every hour | how: file [L2]
known N3: works on the invoices in Navision [L4]
open N2: Who looks at failed imports? [L3]
(Petra works in Navision, but nothing was said to flow between them: no flow line. Christmas and sick leave are \
topics, and L6 adds nothing: no lines for them.)

{shown}",
        shown = llm::SHOWN.trim_start(),
    )
}

/// The run's question. What changes least comes first, so the helper keeps it from the run before
/// and reads only the rest: the boxes (new ones only add to the end), what I removed, my notes;
/// then what the map has on the boxes the new talk names, the talk, the new numbers and the
/// language to write in.
fn user_prompt(input: &Input, earlier: &[String], new: &[String], notes: &str, new_ids: &[u32], language: &str) -> String {
    let talk = talk_of(new, notes);
    let mut p = format!("Map so far:\n{}", boxes_text(input.map, input.room));
    if !input.map.dropped.is_empty() {
        p.push_str(&format!("Removed by me, never add again: {}\n", input.map.dropped.join(", ")));
    }
    if !notes.is_empty() {
        p.push_str(&format!("\nMy notes (what to focus on and what the map must find out):\n{notes}\n"));
    }
    let news = news_text(input.map, &talk, &talk, input.room);
    if !news.is_empty() {
        p.push_str(&format!("\n{news}"));
    }
    let numbered = |from: usize, lines: &[String]| -> String {
        lines.iter().enumerate().map(|(i, l)| format!("L{} {l}\n", from + i)).collect()
    };
    p.push_str(&match (earlier.len(), new.len()) {
        (0, 0) => "\nTalk: nothing new was said; my notes changed.\n".to_string(),
        (0, _) => format!("\nTalk (all lines are new):\n{}", numbered(1, new)),
        (e, 0) => format!("\nTalk (L1 to L{e} were read before; nothing new was said, my notes changed):\n{}", numbered(1, earlier)),
        (e, _) => format!("\nTalk (L1 to L{e} were read before, L{} on are new):\n{}{}", e + 1, numbered(1, earlier), numbered(e + 1, new)),
    });
    let ids: Vec<String> = new_ids.iter().map(|n| format!("N{n}")).collect();
    p.push_str(&format!("New node numbers: {}. Write in {language}.\nAnswer:", ids.join(", ")));
    p
}

/// What one run asked and answered, for measuring.
pub struct Run {
    pub ops: Vec<Op>,
    /// The numbered lines the run read, to check the edits against.
    pub lines: Vec<String>,
    /// The first of them that was new.
    pub new_from: usize,
    pub notes: String,
    /// Names the user gave speakers.
    pub people: Vec<String>,
    pub language: &'static str,
    /// Everything said so far, the text of every screen so far, and the shown spellings, to back
    /// names with (see `Evidence`).
    pub spoken: String,
    pub screens: Vec<String>,
    pub spellings: Vec<String>,
    /// The last look at the whole meeting (see `closing`), not a run on new talk.
    pub closing: bool,
    /// The answer as written.
    pub answer: String,
    /// What the model call cost.
    pub cost: llm::ChatStats,
}

impl Run {
    pub fn evidence(&self) -> Evidence<'_> {
        Evidence {
            lines: &self.lines,
            notes: &self.notes,
            new_from: self.new_from,
            people: &self.people,
            language: self.language,
            spoken: &self.spoken,
            screens: &self.screens,
            spellings: &self.spellings,
            max_new: if self.closing { CLOSING_SIZE.0 } else { MAX_NEW },
            picked: self.closing,
        }
    }
}

/// The earlier lines a run shows at most (they are context: a speaker's turn may run to 1,200
/// characters, and four of them were a quarter of a prompt).
const EARLIER_CHARS: usize = 900;

/// The last `max` characters of `lines`: the latest lines whole, the one before cut at its start
/// ("[12:00] Speaker 1: …the end of what they said"), older ones left out.
fn latest_words(lines: Vec<String>, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut left = max;
    for line in lines.into_iter().rev() {
        if left == 0 {
            break;
        }
        let n = line.chars().count();
        if n <= left {
            left -= n;
            out.push(line);
            continue;
        }
        let (head, text) = line.split_once(": ").unwrap_or(("", &line));
        let keep = left.saturating_sub(head.chars().count() + 3);
        if keep >= 60 {
            let skip = text.chars().count() - keep;
            let mut tail: String = text.chars().skip(skip).collect();
            // Start at a word.
            if let Some(space) = tail.find(' ').filter(|i| *i < tail.len() / 2) {
                tail = tail[space + 1..].to_string();
            }
            out.push(format!("{head}: …{tail}"));
        }
        break;
    }
    out.reverse();
    out
}

/// Everything said in `segments`, one segment a line.
fn spoken_text(segments: &[&[Segment]]) -> String {
    segments.iter().flat_map(|s| s.iter()).map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n")
}

/// One run: asks the model how the new talk (and changed notes) change the map. `None` when
/// there's nothing new to read.
pub fn run(worker: &mut llm::Worker, input: &Input) -> anyhow::Result<Option<Run>> {
    let fresh = fresh_rows(input.visuals, input.shown_before);
    let mut new: Vec<String> = super::pipeline::timeline_lines(input.new, input.speakers, &fresh, SCREEN_CAPS)
        .into_iter()
        .map(|l| unlabel_me(l, input.room))
        .collect();
    let notes: String = input.notes.trim().chars().take(NOTES_CHARS).collect();
    if new.is_empty() && !(input.notes_changed && !notes.is_empty()) {
        return Ok(None);
    }
    // Fell behind: the latest talk.
    while new.iter().map(String::len).sum::<usize>() > NEW_CHARS && new.len() > 1 {
        new.remove(0);
    }
    let language = llm::language_name(input.language);
    let first_new = input.map.next_id();
    let new_ids: Vec<u32> = (first_new..first_new + NODE_LINES as u32).collect();
    let existing: Vec<u32> = input.map.nodes.iter().map(|n| n.id).collect();
    let open: Vec<u32> = input.map.open.iter().map(|o| o.number).collect();
    let earlier = latest_words(input.earlier.iter().map(|l| unlabel_me(l.clone(), input.room)).collect(), EARLIER_CHARS);
    // Node lines may also use the number of a candidate the new lines name (see `named_candidates`).
    let node_ids: Vec<u32> = named_candidates(input.map, &talk_of(&new, &notes)).iter().map(|n| n.id).chain(new_ids.iter().copied()).collect();
    let lines: Vec<String> = earlier.iter().chain(&new).cloned().collect();
    let grammar = grammar(&existing, &node_ids, lines.len(), &open, language, input.room, ANSWER_SIZE);
    let prompt = user_prompt(input, &earlier, &new, &notes, &new_ids, language);
    let system = system_prompt(language, input.room);
    let mut answer = worker.chat_at(&system, &prompt, ANSWER_TOKENS, Some(&grammar), Some(TEMPERATURE), Some("map"), |_| {})?;
    let cost = worker.last.unwrap_or_default();
    // Cut off by the token limit: the last line is unfinished.
    if cost.tokens >= ANSWER_TOKENS as u64 {
        answer.truncate(answer.trim_end().rfind('\n').unwrap_or(0));
    }
    let people = input.speakers.iter().filter(|s| s.renamed).map(|s| s.name.clone()).collect();
    Ok(Some(Run {
        ops: parse(&answer),
        lines,
        new_from: earlier.len() + 1,
        notes,
        people,
        language,
        spoken: spoken_text(&[input.said_before, input.new]),
        screens: input.shown_before.iter().chain(input.visuals).map(|v| v.text.clone()).collect(),
        spellings: input.spellings.to_vec(),
        closing: false,
        answer,
        cost,
    }))
}

// ---------- the last look ----------

/// Node lines and flow lines the last look may write.
const CLOSING_SIZE: (usize, usize) = (5, 10);
const CLOSING_TOKENS: u32 = 520;
/// What the last look reads at most: spoken lines, and each overview screen.
const CLOSING_TALK: usize = 4_000;
const CLOSING_SCREEN: usize = 2_400;
/// Overview screens it reads at most, and the boxes a screen must name to be one.
const CLOSING_SCREENS: usize = 2;
const OVERVIEW_BOXES: usize = 3;

/// A row as `visuals::chrome` compares it: lower case, digits as '#'.
fn chrome_key(row: &str) -> String {
    row.chars().map(|c| if c.is_ascii_digit() { '#' } else { c.to_ascii_lowercase() }).collect::<String>().trim().to_string()
}

/// How alike two texts are (letter pairs, Dice).
fn alike(a: &str, b: &str) -> f32 {
    let (a, b) = (bigrams(a), bigrams(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    2.0 * a.intersection(&b).count() as f32 / (a.len() + b.len()) as f32
}

/// People whose name ("Klara Huber": a common first name and a capitalised word) is on at least
/// half the screens (three or more): the call's video tiles, so most likely the people in the call.
fn people_on_screen(screens: &[&super::Visual]) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for v in screens {
        let mut here: Vec<String> = Vec::new();
        for row in v.text.lines() {
            let tokens: Vec<&str> = row.split(|c: char| !c.is_alphabetic() && c != '-').filter(|t| !t.is_empty()).collect();
            for p in tokens.windows(2) {
                let capital = |w: &str| w.chars().next().is_some_and(char::is_uppercase) && w.chars().skip(1).all(char::is_lowercase) && w.chars().count() >= 2;
                if capital(p[0]) && capital(p[1]) && FIRST_NAMES.contains(&p[0].to_lowercase().as_str()) {
                    let name = format!("{} {}", p[0], p[1]);
                    if !here.contains(&name) {
                        here.push(name);
                    }
                }
            }
        }
        for name in here {
            match counts.iter_mut().find(|(n, _)| *n == name) {
                Some((_, c)) => *c += 1,
                None => counts.push((name, 1)),
            }
        }
    }
    counts.into_iter().filter(|(_, c)| *c >= 3 && 2 * c >= screens.len()).map(|(n, _)| n).collect()
}

/// The sentences of a spoken line that name a box, or the whole line when none does on its own.
fn naming_sentences(text: &str, known: &[&Node]) -> String {
    let sentences: Vec<&str> = text.split_inclusive(['.', '?', '!']).map(str::trim).filter(|s| !s.is_empty()).collect();
    let naming: Vec<&str> = sentences.iter().copied().filter(|s| known.iter().any(|n| mentioned(n, s))).collect();
    if naming.is_empty() { text.to_string() } else { naming.join(" ") }
}

/// What the last look reads: the screens that showed the most boxes at once (an overview or an
/// architecture slide; at most `CLOSING_SCREENS` that differ, without the window around them), the
/// people on most screens, and the spoken turns of the whole meeting that name two boxes (or
/// candidates) and say something moves, cut to the sentences that name one, the ones naming most
/// first within `CLOSING_TALK`, in the order they were said.
fn closing_lines(input: &Input) -> Vec<String> {
    let map = input.map;
    let known: Vec<&Node> = map.nodes.iter().chain(&map.candidates).collect();
    let named = |text: &str| known.iter().filter(|n| mentioned(n, text)).count();
    let all: Vec<super::Visual> = input.shown_before.iter().chain(input.visuals).cloned().collect();
    let chrome = super::visuals::chrome(&all);
    let screens: Vec<&super::Visual> = all.iter().filter(|v| v.source == super::visuals::Source::Screen).collect();
    let content = |v: &super::Visual| {
        v.text.lines().map(str::trim).filter(|r| !r.is_empty() && !chrome.contains(&chrome_key(r))).collect::<Vec<_>>().join(" / ")
    };
    let mut overviews: Vec<(usize, u64, String)> =
        screens.iter().map(|v| (v.at_ms, content(v))).map(|(at, t)| (named(&t), at, t)).filter(|(n, ..)| *n >= OVERVIEW_BOXES).collect();
    overviews.sort_by_key(|(n, at, t)| (std::cmp::Reverse(*n), std::cmp::Reverse(t.len()), *at));
    let mut shown: Vec<(u64, String)> = Vec::new();
    for (_, at, text) in overviews {
        if shown.len() < CLOSING_SCREENS && !shown.iter().any(|(_, t)| alike(t, &text) >= SAME_ROW) {
            shown.push((at, text));
        }
    }
    let mut lines: Vec<(u64, String)> =
        shown.into_iter().map(|(at, t)| (at, format!("[{}] Screen: {}", super::pipeline::clock(at), cut(&t, CLOSING_SCREEN)))).collect();
    let people = people_on_screen(&screens);
    if let (false, Some(first)) = (people.is_empty(), screens.first()) {
        lines.push((first.at_ms, format!("[{}] Screen: {} (a name on most screens)", super::pipeline::clock(first.at_ms), people.join(", "))));
    }
    let segments: Vec<Segment> = input.said_before.iter().chain(input.new).cloned().collect();
    let mut talk: Vec<(usize, u64, String)> = super::pipeline::transcript_lines(&segments, input.speakers)
        .into_iter()
        .map(|l| unlabel_me(l, input.room))
        .filter(|l| named(line_text(l)) >= 2 && moves(line_text(l)))
        .map(|l| {
            let (head, text) = l.split_once(": ").unwrap_or(("", &l));
            let line = format!("{head}: {}", naming_sentences(text, &known));
            (named(&line), super::visuals::line_ms(&line).unwrap_or(0), line)
        })
        .collect();
    talk.sort_by_key(|(n, at, _)| (std::cmp::Reverse(*n), *at));
    let mut budget = CLOSING_TALK;
    for (_, at, line) in talk {
        if line.len() <= budget {
            budget -= line.len();
            lines.push((at, line));
        }
    }
    lines.sort_by_key(|(at, _)| *at);
    lines.into_iter().map(|(_, l)| l).collect()
}

/// The last look at the whole meeting, once it is over: the boxes and flows the map has, and
/// `closing_lines` (the overview screens, the people on screen, the spoken lines naming two
/// boxes). The answer may add flows the map lacks and the people, teams or roles shown working with
/// a system (no facts, no open points), checked like a run's (a flow's two ends in its own line).
/// A live run sees a few lines at a time; an overview slide shown at the start and the end, and a
/// flow told in pieces, only show from here. `None` when nothing names two boxes.
pub fn closing(worker: &mut llm::Worker, input: &Input) -> anyhow::Result<Option<Run>> {
    let lines = closing_lines(input);
    if lines.is_empty() || input.map.nodes.len() < 2 {
        return Ok(None);
    }
    let notes: String = input.notes.trim().chars().take(NOTES_CHARS).collect();
    let language = llm::language_name(input.language);
    let first_new = input.map.next_id();
    let new_ids: Vec<u32> = (first_new..first_new + CLOSING_SIZE.0 as u32).collect();
    let existing: Vec<u32> = input.map.nodes.iter().map(|n| n.id).collect();
    let talk = talk_of(&lines, &notes);
    let node_ids: Vec<u32> = named_candidates(input.map, &talk).iter().map(|n| n.id).chain(new_ids.iter().copied()).collect();
    let grammar = grammar(&existing, &node_ids, lines.len(), &[], language, input.room, CLOSING_SIZE);
    let mut p = format!("Map so far:\n{}", boxes_text(input.map, input.room));
    let flows: Vec<String> = input.map.edges.iter().map(|e| format!("flow N{} -> N{}: {} | how: {}\n", e.from, e.to, e.what, e.how)).collect();
    if !flows.is_empty() {
        p.push_str(&format!("Flows on the map:\n{}", flows.concat()));
    }
    if !notes.is_empty() {
        p.push_str(&format!("\nMy notes (what to focus on and what the map must find out):\n{notes}\n"));
    }
    let maybe = news_text(&SystemMap { edges: Vec::new(), open: Vec::new(), ..input.map.clone() }, "", &talk, input.room);
    if !maybe.is_empty() {
        p.push_str(&format!("\n{maybe}"));
    }
    let numbered: String = lines.iter().enumerate().map(|(i, l)| format!("L{} {l}\n", i + 1)).collect();
    let ids: Vec<String> = new_ids.iter().map(|n| format!("N{n}")).collect();
    p.push_str(&format!(
        "\nThe meeting is over. Last, look at it as a whole: the screens that showed several boxes at once, and the \
spoken lines from the whole meeting that name two boxes. Add only what the map lacks: a flow between two boxes when a \
line shows something moving from one to the other (on a screen, columns and arrows can show it: \"A → B\", \"team | \
does this in B\"), and a person, team or role a line shows working with a system (with a flow to it). No facts, no \
open points; a flow the map has already is no new line.\n\
Talk (all lines are new):\n{numbered}New node numbers: {}. Write in {language}.\nAnswer:",
        ids.join(", ")
    ));
    let system = system_prompt(language, input.room);
    let mut answer = worker.chat_at(&system, &p, CLOSING_TOKENS, Some(&grammar), Some(TEMPERATURE), Some("map"), |_| {})?;
    let cost = worker.last.unwrap_or_default();
    if cost.tokens >= CLOSING_TOKENS as u64 {
        answer.truncate(answer.trim_end().rfind('\n').unwrap_or(0));
    }
    let ops = parse(&answer).into_iter().filter(|o| matches!(o, Op::Node { .. } | Op::NotBox { .. } | Op::Flow { .. })).collect();
    let people = input.speakers.iter().filter(|s| s.renamed).map(|s| s.name.clone()).collect();
    Ok(Some(Run {
        ops,
        lines,
        new_from: 1,
        notes,
        people,
        language,
        spoken: spoken_text(&[input.said_before, input.new]),
        screens: input.shown_before.iter().chain(input.visuals).map(|v| v.text.clone()).collect(),
        spellings: input.spellings.to_vec(),
        closing: true,
        answer,
        cost,
    }))
}

// ---------- tidy-up ----------

/// What the tidy-up says one box really is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// A real box (of a kind the map runs keep; see `apply_tidy`).
    Box,
    /// The same as another box.
    Same(u32),
    /// A topic, data, a feature, or a misheard word: no box.
    NotBox,
}

/// What a tidy-up run asked and answered.
pub struct Tidy {
    pub verdicts: Vec<(u32, Verdict)>,
    pub answer: String,
    pub cost: llm::ChatStats,
}

/// Boxes whose names look alike (another spelling, a misheard form, the same last part of a German
/// compound): the tidy-up says for each pair whether they are one box. The user's own boxes aren't
/// paired with each other; at most `MAX_PAIRS`, the closest first.
fn look_alike(map: &SystemMap) -> Vec<(u32, u32)> {
    let mut pairs: Vec<(f32, u32, u32)> = Vec::new();
    for (i, a) in map.nodes.iter().enumerate() {
        for b in &map.nodes[i + 1..] {
            let first_name = |n: &Node| words(&n.name).into_iter().find(|w| FIRST_NAMES.contains(&w.as_str()));
            if (a.named && b.named) || first_name(a).zip(first_name(b)).is_some_and(|(x, y)| x != y) {
                continue;
            }
            let best = names(a)
                .flat_map(|x| names(b).map(move |y| (compact(x), compact(y))))
                .filter_map(|(x, y)| {
                    let (lx, ly) = (x.chars().count(), y.chars().count());
                    if lx.min(ly) < 4 {
                        return None;
                    }
                    let ratio = edit_distance(&x, &y) as f32 / lx.max(ly) as f32;
                    let start = |n: usize| x.chars().take(n).eq(y.chars().take(n));
                    let tail = |n: usize| lx >= n && ly >= n && x.chars().rev().take(n).eq(y.chars().rev().take(n));
                    (ratio <= 0.4 || (lx.min(ly) >= 5 && start(4)) || tail(6)).then_some(ratio)
                })
                .fold(f32::MAX, f32::min);
            if best < f32::MAX {
                pairs.push((best, a.id, b.id));
            }
        }
    }
    pairs.sort_by(|x, y| x.0.total_cmp(&y.0));
    pairs.into_iter().take(MAX_PAIRS).map(|(_, a, b)| (a, b)).collect()
}

/// The tidy-up's answer: first "same" or "different" for each pair that looks alike, then one line
/// per box, in order, so none is left out and none comes twice. Merges come only from the pairs
/// (offered "same as N#" per box, the model merged people with other people).
fn tidy_grammar(ids: &[u32], pairs: &[(u32, u32)], room: Room) -> String {
    let pair_lines: Vec<String> = pairs.iter().map(|(a, b)| format!("\"N{a} = N{b}: \" (\"same\" | \"different\") \"\\n\"")).collect();
    let lines: Vec<String> = ids.iter().map(|id| format!("\"N{id}: \" verdict \"\\n\"")).collect();
    format!(
        "root ::= {}\n\
         verdict ::= {} | \"misheard\"\n",
        pair_lines.into_iter().chain(lines).collect::<Vec<_>>().join(" "),
        kinds(room),
    )
}

fn tidy_system_prompt(room: Room) -> String {
    let ours = room.ours();
    let sides = match room {
        Room::Call => "\"Me's system\" (what Me's own company builds or runs)",
        Room::OneMic => "\"my company's system\" (what my company builds, runs or delivers to the others; my notes may name it)",
    };
    format!(
        "You tidy up a map of a meeting. While the meeting went on, boxes for the software systems, outside services \
and people that came up were picked out of a speech recognition transcript, a few lines at a time, so some are no \
box at all, and some are the same box twice.

A box is a software system, tool, app or online service with a name or a clear role, an organisation that provides \
a service, or a real person, team or role who does something with the systems.
Not a box: topics, problems, events, absences, plans (Krankenstand, Urlaub, Weihnachten, sick leave, delays); data \
fields, numbers and documents (Liefertermin, Bestellnummer, Angebot, Auftragsbestätigung, order number, invoice); \
the things the systems describe (machines, plants, vehicles, their parts, product types, a demo's example records: \
\"data or document\"); features, functions, screens and steps of a system (Freigabe, Erinnerung, approval, reminder, \
import, a settings page).

First, for each pair of boxes whose names look alike, say whether they are the same box (\"N3 = N9: same\") or \
two different things (\"N3 = N9: different\"). Speech recognition often hears one name in several ways \
(\"Navision\", \"Navishon\", \"Navi Sion\"): the same box. Singular and plural, a short and a long name of one \
thing are the same box too.
Then, for each box, in order, write what it really is:
- its kind, when it is a real box: \"customer system\" (runs at the organisation we talk to), \"outside service\" \
(a third party's), \"person or role\", or {sides};
- \"topic\", \"data or document\" or \"feature or process\" when it is that and no box;
- \"misheard\" when its name is no real word or name and the line it was said in doesn't make it one.
Judge each box by the line it was first said in. A supplier or customer that only sells or buys goods is a \
\"person or role\" only when it does something with the systems; a single company named as an example is a topic.
Example: \"N7: customer system\", \"N8: topic\", \"N9: {ours}\"."
    )
}

fn tidy_prompt(map: &SystemMap, notes: &str) -> String {
    let mut p = String::new();
    if !notes.trim().is_empty() {
        p.push_str(&format!("My notes:\n{}\n\n", notes.trim().chars().take(NOTES_CHARS).collect::<String>()));
    }
    // Without the kind each box has now: shown it, the model only repeated it.
    p.push_str("Boxes:\n");
    for n in &map.nodes {
        p.push_str(&format!("N{} {}", n.id, n.name));
        if !n.aka.is_empty() {
            p.push_str(&format!(", also called {}", n.aka.iter().take(3).cloned().collect::<Vec<_>>().join(", ")));
        }
        for k in n.known.iter().take(2) {
            p.push_str(&format!("; {k}"));
        }
        if !n.said.is_empty() {
            p.push_str(&format!("; said: \"{}\"", n.said));
        }
        let flows: Vec<String> = map
            .edges
            .iter()
            .filter(|e| e.from == n.id)
            .take(2)
            .map(|e| format!("{} to {}", e.what, map.name_of(e.to)))
            .collect();
        if !flows.is_empty() {
            p.push_str(&format!("; sends {}", flows.join(", ")));
        }
        p.push('\n');
    }
    let pairs = look_alike(map);
    if !pairs.is_empty() {
        p.push_str("\nPairs that look alike:\n");
        for (a, b) in &pairs {
            p.push_str(&format!("N{a} {} = N{b} {}\n", map.name_of(*a), map.name_of(*b)));
        }
    }
    p.push_str("\nFirst the pairs, then one line per box, in this order.\nAnswer:\n");
    p
}

/// Reads the tidy-up's answer.
pub fn parse_tidy(answer: &str) -> Vec<(u32, Verdict)> {
    answer
        .lines()
        .filter_map(|l| {
            let (id, v) = l.trim().split_once(": ")?;
            let v = v.trim();
            // "N3 = N9: same"
            if let Some((a, b)) = id.split_once(" = ") {
                let (a, b) = (a.trim_start_matches('N').parse().ok()?, b.trim_start_matches('N').parse().ok()?);
                return (v == "same").then_some((a, Verdict::Same(b)));
            }
            let id = id.trim_start_matches('N').parse().ok()?;
            let verdict = if v == "misheard" || NOT_BOX_WORDS.contains(&v) {
                Verdict::NotBox
            } else {
                Kind::from_word(v).map(|_| Verdict::Box)?
            };
            Some((id, verdict))
        })
        .collect()
}

/// The tidy-up: the model looks at every box once (`None` for a map too small to bother).
pub fn tidy(worker: &mut llm::Worker, map: &SystemMap, notes: &str, room: Room) -> anyhow::Result<Option<Tidy>> {
    if map.nodes.len() < TIDY_FROM {
        return Ok(None);
    }
    let ids: Vec<u32> = map.nodes.iter().map(|n| n.id).collect();
    let pairs = look_alike(map);
    let tokens = 12 * (ids.len() + pairs.len()) as u32 + 16;
    let grammar = tidy_grammar(&ids, &pairs, room);
    let answer = worker.chat_at(&tidy_system_prompt(room), &tidy_prompt(map, notes), tokens, Some(&grammar), Some(TEMPERATURE), None, |_| {})?;
    let cost = worker.last.unwrap_or_default();
    Ok(Some(Tidy { verdicts: parse_tidy(&answer), answer, cost }))
}

/// Applies the tidy-up: drops what it calls no box, merges the pairs it calls the same, keeps the
/// rest as it is. A box the user named, or one named in the notes, is never dropped or merged away;
/// a box with a flow isn't dropped either (a flow passed the checks against its lines). Dropped
/// names are set aside, so the next runs don't add them again.
pub fn apply_tidy(map: &mut SystemMap, verdicts: &[(u32, Verdict)], notes: &str) -> Applied {
    let mut out = Applied::default();
    let kept = |map: &SystemMap, id: u32| map.node(id).is_some_and(|n| n.named || names(n).any(|a| said_in(a, notes)));
    for &(id, verdict) in verdicts {
        let Some(node) = map.node(id) else { continue };
        let name = node.name.clone();
        match verdict {
            // A box stays as it is: the kind comes from the map runs, which see the talk. (Shown the
            // kind, the tidy-up only repeated it; not shown it, it swapped ours and the customer's.)
            Verdict::Box => {}
            Verdict::Same(other) => {
                // The older box stays (unless the user named the newer one); chains end at a box that stays.
                let Some(target) = map.node(other) else { continue };
                let (from, into) = if (target.at_ms, target.id) <= (node.at_ms, node.id) && !node.named || target.named { (id, other) } else { (other, id) };
                if kept(map, from) {
                    continue;
                }
                let into_name = map.name_of(into).to_string();
                let from_name = map.name_of(from).to_string();
                if map.merge(from, into) {
                    out.lines.push(format!("{from_name} is {into_name} (tidy-up)"));
                }
            }
            Verdict::NotBox => {
                let has_flow = map.edges.iter().any(|e| e.from == id || e.to == id);
                if kept(map, id) || has_flow {
                    continue;
                }
                if let Some(gone) = map.remove(id) {
                    out.lines.push(format!("- {name} (tidy-up)"));
                    for a in names(&gone) {
                        map.set_aside(a);
                    }
                }
            }
        }
    }
    out
}

// ---------- export ----------

/// The map as a Markdown section, in the shape another program reads (don't change it):
///
/// ```text
/// ## System map
/// ### <name> (<kind>)
/// - Known: <fact>
/// - Connects to <other name>: <what flows>, <how>
/// - Open: <what is still unknown>
/// ```
///
/// Nodes in kind order (ours, customer, outside, people), each in the order it came up; a flow is
/// listed under the node it comes from. Candidates are left out. `None` for an empty map.
pub fn section(map: &SystemMap) -> Option<String> {
    if map.nodes.is_empty() {
        return None;
    }
    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s = String::from("## System map\n");
    for kind in Kind::ALL {
        for n in map.nodes.iter().filter(|n| n.kind == kind) {
            s.push_str(&format!("### {} ({})\n", one_line(&n.name), kind.label()));
            for k in &n.known {
                s.push_str(&format!("- Known: {}\n", one_line(k)));
            }
            for e in map.edges.iter().filter(|e| e.from == n.id) {
                s.push_str(&format!("- Connects to {}: {}, {}\n", one_line(map.name_of(e.to)), one_line(&e.what), one_line(&e.how)));
            }
            for o in map.open.iter().filter(|o| o.node == n.id) {
                s.push_str(&format!("- Open: {}\n", one_line(&o.text)));
            }
        }
    }
    Some(s.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TALK: &[&str] = &[
        "[00:40] Speaker 1: Everything ends up in our ERP, that's Microsoft Dynamics Business Central.",
        "[00:48] Speaker 1: The sales team types web shop orders into Business Central by hand, every morning.",
        "[01:10] Speaker 1: The shop sends an email for every order to the sales inbox.",
        "[01:30] Speaker 1: Business Central has an API, but we have never used it.",
        "[01:40] Me: Is the API switched on for your tenant?",
        "[01:44] Speaker 1: I'd have to look that up.",
        "[02:00] Speaker 1: Every night Business Central exports a CSV file to a shared folder, and the warehouse system picks it up.",
        "[02:05] Speaker 1: It's called LagerPro.",
    ];

    fn ev() -> (Vec<String>, String) {
        (TALK.iter().map(|s| s.to_string()).collect(), "focus on the ERP export".to_string())
    }

    fn lines(talk: &[&str]) -> Vec<String> {
        talk.iter().map(|s| s.to_string()).collect()
    }

    fn node(n: u32, name: &str, kind: Kind, line: usize) -> Op {
        Op::Node { n, name: name.into(), kind, aka: None, cite: Cite::Line(line) }
    }

    /// A box put straight on the map, for tests that aren't about admitting it.
    fn put(map: &mut SystemMap, id: u32, name: &str, kind: Kind) {
        map.nodes.push(Node { id, name: name.into(), kind, known: Vec::new(), aka: Vec::new(), at_ms: 0, named: false, said: String::new() });
    }

    fn visible(map: &SystemMap) -> Vec<&str> {
        map.nodes.iter().map(|n| n.name.as_str()).collect()
    }

    fn hidden(map: &SystemMap) -> Vec<&str> {
        map.candidates.iter().map(|n| n.name.as_str()).collect()
    }

    #[test]
    fn parses_the_answer() {
        let ops = parse(
            "node N4: Business Central (customer system) also called ERP [L1]\n\
             node N5: Thing (robot) [L1]\n\
             node N6: Krankenstand (topic) [L2]\n\
             node N7: Jonas (my company's system) [L2]\n\
             alias N4: Dynamics [L1]\n\
             known N4: sales team types orders in by hand [L2]\n\
             flow N4 -> N6: open deliveries | how: CSV [L7]\n\
             open N4: Is the API switched on? [L5]\n\
             solved O3: the API is on [notes]\n\
             known N4: no citation\n",
        );
        assert_eq!(
            ops,
            vec![
                Op::Node { n: 4, name: "Business Central".into(), kind: Kind::Customer, aka: Some("ERP".into()), cite: Cite::Line(1) },
                Op::NotBox { name: "Krankenstand".into(), cite: Cite::Line(2) },
                Op::Node { n: 7, name: "Jonas".into(), kind: Kind::Ours, aka: None, cite: Cite::Line(2) },
                Op::Alias { n: 4, name: "Dynamics".into(), cite: Cite::Line(1) },
                Op::Known { n: 4, fact: "sales team types orders in by hand".into(), cite: Cite::Line(2) },
                Op::Flow { from: 4, to: 6, what: "open deliveries".into(), how: "CSV".into(), cite: Cite::Line(7) },
                Op::Open { n: 4, text: "Is the API switched on?".into(), cite: Cite::Line(5) },
                Op::Solved { number: 3, fact: "the API is on".into(), cite: Cite::Notes },
            ]
        );
    }

    #[test]
    fn grammar_offers_only_what_exists() {
        let g = grammar(&[1, 2], &[3, 4, 5], 6, &[2], "English", Room::Call, ANSWER_SIZE);
        assert!(g.contains("id ::= (\"N1\" | \"N2\" | \"N3\" | \"N4\" | \"N5\")"), "{g}");
        assert!(g.contains("\"node \" (\"N3\" | \"N4\" | \"N5\")"));
        assert!(g.contains("\"L6\" | \"notes\""));
        assert!(g.contains("solved ::= \"solved \" (\"O2\")") && g.contains("| open | solved\n"));
        assert!(g.contains("\"Me's system\"") && g.contains("\"topic\" | \"data or document\" | \"feature or process\""));
        assert!(!grammar(&[], &[1, 2, 3], 2, &[], "German", Room::Call, ANSWER_SIZE).contains("| solved\n"));
        let room = grammar(&[], &[1, 2, 3], 2, &[], "German", Room::OneMic, ANSWER_SIZE);
        assert!(room.contains("\"von Hand\"") && room.contains("\"my company's system\"") && !room.contains("Me's system"));
    }

    #[test]
    fn edits_must_be_in_the_line_they_cite() {
        let (lines, notes) = ev();
        let ev = Evidence::of(&lines, &notes);
        let mut map = SystemMap::default();
        let a = apply(
            &mut map,
            vec![
                Op::Node { n: 1, name: "Business Central".into(), kind: Kind::Customer, aka: Some("ERP".into()), cite: Cite::Line(1) },
                node(2, "LagerPro", Kind::Customer, 8),
                node(3, "SAP", Kind::Customer, 1),          // not in that line
                node(4, "CSV", Kind::Outside, 7),           // a format, not a node
                Op::Known { n: 1, fact: "Sales team types orders in by hand".into(), cite: Cite::Line(2) },
                Op::Known { n: 1, fact: "Runs on servers in Frankfurt".into(), cite: Cite::Line(2) }, // not said
                Op::Flow { from: 1, to: 2, what: "open deliveries, nightly".into(), how: "CSV".into(), cite: Cite::Line(7) },
                Op::Open { n: 1, text: "Is the API switched on?".into(), cite: Cite::Line(5) },
                Op::Open { n: 2, text: "Who maintains LagerPro?".into(), cite: Cite::Line(3) }, // nothing uncertain near it
                Op::Open { n: 1, text: "How does the ERP export work?".into(), cite: Cite::Notes },
            ],
            &ev,
            30_000,
        );
        assert_eq!(visible(&map), ["Business Central", "LagerPro"], "{:?}", a.lines);
        assert_eq!(map.nodes[0].aka, ["ERP"]);
        assert_eq!(map.nodes[0].known, ["Sales team types orders in by hand"]);
        assert_eq!(map.edges.len(), 1, "{:?}", a.lines);
        assert_eq!((map.edges[0].from, map.edges[0].to, map.edges[0].how.as_str()), (1, 2, "CSV"));
        assert_eq!(map.open.iter().map(|o| o.number).collect::<Vec<_>>(), [1, 2]);

        // A later run: the existing node by another name, a solved point, a person by role.
        let b = apply(
            &mut map,
            vec![
                Op::Node { n: 9, name: "ERP".into(), kind: Kind::Customer, aka: None, cite: Cite::Line(1) },
                node(10, "Sales team", Kind::Person, 2),
                Op::Flow { from: 1, to: 10, what: "invoices".into(), how: "mail".into(), cite: Cite::Line(4) }, // ends not there
                Op::Solved { number: 1, fact: "nobody knows yet".into(), cite: Cite::Line(6) },
                Op::Solved { number: 1, fact: "Business Central exports a CSV file every night".into(), cite: Cite::Line(7) }, // no answer to O1
                Op::Solved { number: 1, fact: "Business Central has an API that was never used".into(), cite: Cite::Line(4) },
            ],
            &ev,
            60_000,
        );
        assert_eq!(map.nodes.len(), 2, "ERP is Business Central: {:?}", b.lines);
        assert_eq!(hidden(&map), ["Sales team"], "a role said once waits as a candidate");
        assert_eq!(map.edges.len(), 1, "{:?}", b.lines);
        assert_eq!(map.open.len(), 1, "{:?}", b.lines);
    }

    #[test]
    fn a_proper_name_takes_over_from_a_generic_one() {
        let lines = lines(&["[00:10] Speaker 1: Orders go into our ERP.", "[00:20] Speaker 1: The ERP is Business Central."]);
        let ev = Evidence::of(&lines, "");
        let mut map = SystemMap::default();
        apply(&mut map, vec![node(1, "ERP", Kind::Customer, 1)], &ev, 0);
        apply(&mut map, vec![Op::Alias { n: 1, name: "Business Central".into(), cite: Cite::Line(2) }], &ev, 0);
        assert_eq!(map.nodes[0].name, "Business Central");
        assert_eq!(map.nodes[0].aka, ["ERP"]);
    }

    #[test]
    fn a_generic_name_never_goes_to_another_box() {
        let talk = lines(&[
            "[33:05] Speaker 3: Von Halvorsen kommen die Paletten immer montags.",
            "[33:12] Speaker 1: Die Lagerplätze stehen im ERP.",
            "[33:40] Speaker 3: Halvorsen bucht die Paletten selbst ein, das ERP nicht. Das prüft der Einkauf.",
        ]);
        let ev = Evidence { language: "German", ..Evidence::of(&talk, "") };
        let mut map = SystemMap::default();
        put(&mut map, 1, "ERP", Kind::Customer);
        apply(&mut map, vec![node(2, "Halvorsen", Kind::Customer, 1)], &Evidence { lines: &talk[..1], ..ev }, 0);
        assert_eq!(hidden(&map), ["Halvorsen"]);
        // Said again next to "ERP", with the candidate's number: Halvorsen stays Halvorsen, the ERP the ERP.
        let ev = Evidence { new_from: 2, language: "German", ..Evidence::of(&talk, "") };
        let a = apply(&mut map, vec![node(2, "Halvorsen", Kind::Customer, 3)], &ev, 0);
        assert_eq!(visible(&map), ["ERP", "Halvorsen"], "{:?}", a.lines);
        assert!(map.nodes.iter().all(|n| n.aka.is_empty()));
        // No other box's name, no generic word on a proper name, no system word on a person, no team on a system.
        put(&mut map, 5, "Herr Lindqvist", Kind::Person);
        let alias = |n: u32, name: &str| Op::Alias { n, name: name.into(), cite: Cite::Line(3) };
        let a = apply(&mut map, vec![alias(2, "ERP"), alias(1, "Halvorsen"), alias(5, "ERP"), alias(1, "Einkauf")], &ev, 0);
        assert!(map.nodes.iter().all(|n| n.aka.is_empty()), "{:?}", a.lines);
        assert_eq!((map.find("ERP"), map.find("Halvorsen")), (Some(1), Some(2)));
        // A product said for the first time next to "the ERP" isn't the ERP either.
        let talk = lines(&["[54:00] Speaker 1: Die Bestätigung kommt per Mail ins Outlook und dann ins ERP."]);
        let a = apply(&mut map, vec![node(7, "Outlook", Kind::Customer, 1)], &Evidence { language: "German", ..Evidence::of(&talk, "") }, 0);
        assert_eq!(map.node(1).map(|n| n.name.as_str()), Some("ERP"), "{:?}", a.lines);
        // "Halvorsen, also called ERP" for an outside service: the customer's ERP doesn't become Halvorsen.
        let mut map = SystemMap::default();
        put(&mut map, 1, "ERP", Kind::Customer);
        let a = apply(&mut map, vec![Op::Node { n: 2, name: "Halvorsen".into(), kind: Kind::Outside, aka: Some("ERP".into()), cite: Cite::Line(3) }], &ev, 0);
        assert_eq!(visible(&map), ["ERP"], "{:?}", a.lines);
        assert_eq!(hidden(&map), ["Halvorsen"]);
        assert!(map.candidates[0].aka.is_empty(), "ERP is another box's name");
        // The same kind: the generic box gets its proper name, as before.
        let talk = lines(&["[20:00] Speaker 1: Unser ERP, das ist Navision."]);
        let ev = Evidence { language: "German", ..Evidence::of(&talk, "") };
        let a = apply(&mut map, vec![Op::Node { n: 3, name: "Navision".into(), kind: Kind::Customer, aka: Some("ERP".into()), cite: Cite::Line(1) }], &ev, 0);
        assert_eq!(visible(&map), ["Navision"], "{:?}", a.lines);
        assert_eq!(map.nodes[0].aka, ["ERP"]);
    }

    #[test]
    fn a_screen_line_can_be_cited() {
        let talk = lines(&[
            "[03:10] Speaker 1: Die kommen dann als PDF rein.",
            "[03:20] Screen: Lieferschein / Halvorsen AG | Tour 12 / Rampe | 3 / Fragen?",
            "[03:25] Speaker 2: Genau, so sieht die aus.",
        ]);
        // Read off one screen and never said: a candidate, not a box.
        let one = ["Lieferschein / Halvorsen AG | Tour 12 / Rampe | 3 / Fragen?".to_string()];
        let mut map = SystemMap::default();
        apply(&mut map, vec![node(1, "Halvorsen", Kind::Outside, 2)], &Evidence { language: "German", screens: &one, ..Evidence::of(&talk, "") }, 0);
        assert_eq!((visible(&map), hidden(&map)), (vec![], vec!["Halvorsen"]));
        // On a second screen it is one.
        let two = [one[0].clone(), "Halvorsen AG / Tour 13".to_string()];
        let ev = Evidence { language: "German", screens: &two, ..Evidence::of(&talk, "") };
        let mut map = SystemMap::default();
        let a = apply(
            &mut map,
            vec![
                node(1, "Halvorsen", Kind::Outside, 2),
                node(2, "Screen", Kind::Person, 2),
                Op::Known { n: 1, fact: "Lieferschein mit Rampe".into(), cite: Cite::Line(2) },
                Op::Open { n: 1, text: "Wer prüft die Tour?".into(), cite: Cite::Line(2) },
            ],
            &ev,
            0,
        );
        assert_eq!(visible(&map), ["Halvorsen"], "a name on a screen is spelled out: {:?}", a.lines);
        assert!(map.candidates.is_empty(), "the line's head is no person");
        assert_eq!(map.nodes[0].known, ["Lieferschein mit Rampe"]);
        assert!(map.open.is_empty(), "a screen asks nothing: {:?}", a.lines);
        assert!(!proper_name("Supplier", "Delivery note / Supplier | Halvorsen", "English"), "a row starts like a sentence");
        // A letterhead's address is no other name for the company.
        let talk = lines(&["[03:20] Photo: Meier Hydraulik GmbH • Industriestraße 12 • 4600 Wels / Lieferschein"]);
        let a = apply(
            &mut map,
            vec![node(3, "Meier Hydraulik GmbH", Kind::Outside, 1), Op::Alias { n: 3, name: "Industriestraße 12, 4600 Wels".into(), cite: Cite::Line(1) }],
            &Evidence { language: "German", ..Evidence::of(&talk, "") },
            0,
        );
        assert_eq!(visible(&map), ["Halvorsen", "Meier Hydraulik GmbH"], "{:?}", a.lines);
        assert!(map.nodes[1].aka.is_empty(), "{:?}", a.lines);
        assert!(not_a_node("Industriestraße 12") && not_a_node("12 Main Street") && !not_a_node("Postfach"));
    }

    #[test]
    fn candidates_are_listed_as_also_mentioned() {
        let mut map = SystemMap::default();
        put(&mut map, 1, "Varnor", Kind::Customer);
        for (i, name) in ["Ostwind", "Pallas", "ostwind", "Quarzlicht"].iter().enumerate() {
            let id = 10 + i as u32;
            map.candidates.push(Node { id, name: name.to_string(), kind: Kind::Outside, known: Vec::new(), aka: Vec::new(), at_ms: id as u64 * 1000, named: false, said: String::new() });
        }
        map.set_aside("Liefertermin");
        assert_eq!(map.also_mentioned(), ["Quarzlicht", "ostwind", "Pallas"], "the latest first, each once, no topics");
        for i in 0..20 {
            map.candidates.push(Node { id: 100 + i, name: format!("Name{i}"), kind: Kind::Outside, known: Vec::new(), aka: Vec::new(), at_ms: 100_000, named: false, said: String::new() });
        }
        assert_eq!(map.also_mentioned().len(), ALSO_MENTIONED);
        assert_eq!(map.also_mentioned()[0], "Name19");
    }

    #[test]
    fn a_tidy_merge_keeps_people_and_systems_apart() {
        let mut map = SystemMap::default();
        put(&mut map, 1, "Varnor", Kind::Customer);
        put(&mut map, 2, "Varno", Kind::Person);
        put(&mut map, 3, "ERP-System", Kind::Customer);
        put(&mut map, 4, "Varnors", Kind::Customer);
        map.nodes[3].aka.push("ERP".into());
        assert!(!map.merge(2, 1), "a person and a system are never one box");
        assert!(map.merge(4, 1));
        assert_eq!(map.nodes[0].aka, ["Varnors"], "a generic name doesn't go to a proper one");
    }

    #[test]
    fn rename_and_delete_stick() {
        let lines = lines(&["[00:10] Speaker 1: BC gets the orders from Shopware by mail."]);
        let ev = Evidence::of(&lines, "");
        let mut map = SystemMap::default();
        apply(
            &mut map,
            vec![
                node(1, "BC", Kind::Customer, 1),
                node(2, "Shopware", Kind::Customer, 1),
                Op::Flow { from: 2, to: 1, what: "orders".into(), how: "mail".into(), cite: Cite::Line(1) },
            ],
            &ev,
            0,
        );
        assert_eq!(map.edges.len(), 1);
        assert_eq!(map.nodes.len(), 2, "the flow made the candidate BC a box");
        map.rename(1, "Business Central").unwrap();
        assert!(map.nodes[1].named);
        assert!(map.rename(2, "business central").is_err());
        assert_eq!(map.find("BC"), Some(1), "the old name still finds it");
        map.delete(2).unwrap();
        assert!(map.edges.is_empty());
        apply(&mut map, vec![node(3, "Shopware", Kind::Customer, 1), node(4, "Me", Kind::Ours, 1)], &ev, 0);
        assert_eq!(map.nodes.len(), 1, "a deleted node isn't added again, and Me is no box");
        assert_eq!(map.next_id(), 3, "the deleted node's number isn't handed out again");
    }

    #[test]
    fn a_generic_box_gets_its_proper_name() {
        let lines = lines(&[
            "[00:10] Speaker 1: The web shop sends an email for every order.",
            "[00:20] Me: And the web shop itself, which system is that?",
            "[00:25] Speaker 1: It's a Shopware shop, hosted by an agency.",
            "[00:30] Speaker 1: Business Central exports to a shared folder, as a bank statement file.",
        ]);
        let ev = Evidence::of(&lines, "");
        let mut map = SystemMap::default();
        apply(&mut map, vec![node(1, "web shop", Kind::Customer, 1)], &ev, 0);
        assert_eq!(hidden(&map), ["web shop"]);
        apply(&mut map, vec![node(2, "Shopware shop", Kind::Customer, 3), node(3, "shared folder", Kind::Outside, 4), node(4, "bank statement file", Kind::Customer, 4)], &ev, 0);
        assert_eq!(map.nodes.len(), 1, "{:?}", map.nodes);
        assert_eq!(map.nodes[0].name, "Shopware shop");
        assert_eq!(map.nodes[0].aka, ["web shop"]);
        assert!(map.candidates.is_empty());
    }

    #[test]
    fn an_unsure_line_solves_nothing() {
        let lines1 = lines(&["[00:10] Me: Is the API switched on?", "[00:12] Speaker 1: I'd have to look up whether the API is switched on."]);
        let mut map = SystemMap::default();
        apply(&mut map, vec![node(1, "API", Kind::Customer, 1)], &Evidence::of(&lines1, ""), 0);
        assert!(map.nodes.is_empty() && map.candidates.is_empty(), "an API is no node");
        let lines2 = lines(&[
            "[00:05] Speaker 1: We use Business Central, it has an API.",
            "[00:10] Me: Is the API in Business Central switched on?",
            "[00:12] Speaker 1: I'd have to look up whether the API is switched on.",
        ]);
        let ev = Evidence::of(&lines2, "");
        apply(&mut map, vec![node(1, "Business Central", Kind::Customer, 1), Op::Open { n: 1, text: "Is the API switched on?".into(), cite: Cite::Line(2) }], &ev, 0);
        apply(&mut map, vec![Op::Solved { number: 1, fact: "The API is switched on".into(), cite: Cite::Line(3) }], &ev, 0);
        assert_eq!(map.open.len(), 1, "still open");
    }

    #[test]
    fn a_how_nobody_said_is_unknown() {
        let lines = lines(&["[00:10] Speaker 1: The tax advisor imports the invoices into DATEV."]);
        let ev = Evidence::of(&lines, "");
        let mut map = SystemMap::default();
        apply(
            &mut map,
            vec![
                node(1, "tax advisor", Kind::Outside, 1),
                node(2, "DATEV", Kind::Outside, 1),
                node(3, "200 orders per day", Kind::Customer, 1),
                Op::Flow { from: 1, to: 2, what: "invoices".into(), how: "API".into(), cite: Cite::Line(1) },
            ],
            &ev,
            0,
        );
        assert_eq!(visible(&map), ["DATEV", "tax advisor"], "the flow made the tax advisor a box");
        assert_eq!(map.edges[0].how, "unknown");
    }

    #[test]
    fn a_person_stays_a_person() {
        let lines = lines(&["[00:10] Speaker 1: Tom does the invoices."]);
        let ev = Evidence::of(&lines, "");
        let mut map = SystemMap::default();
        apply(&mut map, vec![node(1, "Tom", Kind::Person, 1)], &ev, 0);
        apply(&mut map, vec![node(2, "Tom", Kind::Customer, 1)], &ev, 0);
        assert_eq!(map.nodes[0].kind, Kind::Person);
    }

    #[test]
    fn a_person_needs_a_name_a_title_or_a_role() {
        let talk = lines(&[
            "[10:00] Speaker 1: Die Wochenpläne schreibt Frau Martina, außer sie ist im Krankenstand.",
            "[10:10] Speaker 1: Unterschreiben tut das am Ende Frau Ostrowski vom Werk Nord.",
            "[10:20] Speaker 2: Der Projektleiter und die Buchhaltung kriegen das auch, die Kinder nicht.",
            "[10:30] Speaker 2: Laura from accounting signs it off.",
        ]);
        let mut ev = Evidence::of(&talk, "");
        ev.language = "German";
        let mut map = SystemMap::default();
        let a = apply(&mut map, vec![node(1, "Krankenstand", Kind::Person, 1), node(2, "Frau Martina", Kind::Person, 1)], &ev, 0);
        assert_eq!(visible(&map), ["Frau Martina"], "{:?}", a.lines);
        apply(&mut map, vec![node(3, "Ostrowski", Kind::Person, 2), node(4, "Kinder", Kind::Person, 3)], &ev, 0);
        apply(&mut map, vec![node(5, "Projektleiter", Kind::Person, 3), node(6, "Buchhaltung", Kind::Person, 3)], &ev, 0);
        apply(&mut map, vec![node(7, "Laura", Kind::Person, 4)], &ev, 0);
        assert_eq!(visible(&map), ["Frau Martina", "Ostrowski", "Laura"], "a title in the line, a first name");
        assert_eq!(hidden(&map), ["Projektleiter", "Buchhaltung"], "a role word: a candidate");
        // A name the user gave a speaker, or wrote in the notes.
        let more = lines(&["[11:00] Speaker 3: Pichlmayr checks it, then Moser."]);
        let people = vec!["Moser".to_string()];
        let ev = Evidence { people: &people, ..Evidence::of(&more, "ask Pichlmayr about the approval") };
        apply(&mut map, vec![node(8, "Pichlmayr", Kind::Person, 1), node(9, "Moser", Kind::Person, 1)], &ev, 0);
        assert!(map.find("Pichlmayr").is_some() && map.find("Moser").is_some());
    }

    #[test]
    fn topics_data_and_features_are_set_aside() {
        let lines = lines(&[
            "[05:00] Speaker 1: Der Liefertermin passt oft nicht, und die Bestellnummer fehlt.",
            "[05:10] Speaker 1: Die Freigabe macht der Varnor dann automatisch.",
            "[05:20] Speaker 1: Der Liefertermin ist schon wieder falsch, sagt der Varnor.",
        ]);
        let ev = Evidence { new_from: 1, language: "German", ..Evidence::of(&lines, "") };
        let mut map = SystemMap::default();
        apply(
            &mut map,
            vec![
                node(1, "Liefertermin", Kind::Ours, 1),
                node(2, "Bestellnummer", Kind::Ours, 1),
                node(3, "order number", Kind::Customer, 1),
                Op::NotBox { name: "Freigabe".into(), cite: Cite::Line(2) },
                node(4, "Varnor", Kind::Customer, 2),
            ],
            &ev,
            0,
        );
        assert!(map.nodes.is_empty(), "{:?}", map.nodes);
        assert_eq!(hidden(&map), ["Varnor"], "data heads are no boxes");
        assert_eq!(map.set_aside, ["Freigabe"]);
        // Called a topic now, the candidate goes; called a box again later, it is only a candidate.
        apply(&mut map, vec![Op::NotBox { name: "Varnor".into(), cite: Cite::Line(2) }], &ev, 0);
        assert!(map.candidates.is_empty());
        let notes = "Was macht der Varnor mit der Freigabe?";
        apply(&mut map, vec![node(5, "Freigabe", Kind::Ours, 2), node(6, "Varnor", Kind::Customer, 3)], &Evidence { language: "German", ..Evidence::of(&lines, notes) }, 0);
        assert!(map.nodes.is_empty(), "not even the notes make a set-aside name a box at once: {:?}", map.nodes);
        assert_eq!(hidden(&map), ["Freigabe", "Varnor"]);
        assert!(map.set_aside.is_empty());
    }

    #[test]
    fn names_with_endings_and_company_words() {
        let talk = lines(&[
            "[37:52] Speaker 3: Die Prüfprotokolle schicke ich immer an Herrn Tobelmann.",
            "[24:20] Speaker 1: Das betreut die Firma Bitwerk aus Graz, und die Hansen GmbH macht den Druck.",
        ]);
        let ev = Evidence { language: "German", ..Evidence::of(&talk, "") };
        let mut map = SystemMap::default();
        apply(&mut map, vec![node(1, "Herr Tobelmann", Kind::Person, 1)], &ev, 0);
        apply(&mut map, vec![node(2, "Bitwerk", Kind::Outside, 2), node(3, "Hansen", Kind::Outside, 2)], &ev, 0);
        assert_eq!(visible(&map), ["Herr Tobelmann", "Bitwerk", "Hansen"]);
    }

    #[test]
    fn a_candidate_named_again_in_a_new_line_is_said_again() {
        let talk = lines(&["[01:00] Speaker 1: Das liegt im Postfach.", "[01:30] Speaker 1: Und das Postfach ist voll.", "[02:00] Speaker 1: Ja."]);
        let mut map = SystemMap::default();
        apply(&mut map, vec![node(1, "Postfach", Kind::Customer, 1)], &Evidence { lines: &talk[..1], language: "German", ..Evidence::of(&talk, "") }, 0);
        // The next run reads L1, L2 as earlier lines and L3 as new: citing earlier lines is no second mention.
        let ev = Evidence { new_from: 3, language: "German", ..Evidence::of(&talk, "") };
        apply(&mut map, vec![node(5, "Postfach", Kind::Customer, 1), node(6, "Postfach", Kind::Customer, 2)], &ev, 0);
        assert!(map.nodes.is_empty());
        // Proposed again citing its first line, while a new line names it: said again.
        let talk = lines(&["[01:00] Speaker 1: Wir schreiben in SAP.", "[01:30] Speaker 1: SAP macht ein PDF daraus."]);
        apply(&mut map, vec![node(6, "SAP", Kind::Customer, 1)], &Evidence { lines: &talk[..1], language: "German", ..Evidence::of(&talk, "") }, 0);
        apply(&mut map, vec![node(7, "SAP", Kind::Customer, 1)], &Evidence { new_from: 2, language: "German", ..Evidence::of(&talk, "") }, 0);
        assert_eq!(visible(&map), ["SAP"]);
    }

    #[test]
    fn on_a_call_my_own_system_is_a_box_at_once() {
        let talk = lines(&["[00:05] Me: Wir bauen für euch das Kundenportal.", "[00:10] Speaker 1: Wir haben ein Lagerportal."]);
        let ev = Evidence { language: "German", ..Evidence::of(&talk, "") };
        let mut map = SystemMap::default();
        apply(&mut map, vec![node(1, "Kundenportal", Kind::Ours, 1), node(2, "Lagerportal", Kind::Ours, 2)], &ev, 0);
        assert_eq!(visible(&map), ["Kundenportal"]);
        assert_eq!(hidden(&map), ["Lagerportal"], "only Me names our own systems");
    }

    #[test]
    fn a_flow_brings_a_candidate_in_only_when_something_moves() {
        let talk = lines(&[
            "[06:41] Speaker 1: In der Tourenplanung gibt es jetzt einen Filter pro Fahrer.",
            "[06:55] Speaker 2: Der Lieferant schickt die Bestätigung an das Postfach.",
        ]);
        let ev = Evidence { language: "German", ..Evidence::of(&talk, "") };
        let mut map = SystemMap::default();
        let flow = |from, to, what: &str, line| Op::Flow { from, to, what: what.into(), how: "Mail".into(), cite: Cite::Line(line) };
        let a = apply(&mut map, vec![node(1, "Tourenplanung", Kind::Ours, 1), flow(1, 2, "Touren pro Fahrer", 1), node(2, "Fahrer", Kind::Outside, 1)], &ev, 0);
        assert!(map.nodes.is_empty() && map.edges.is_empty(), "{:?}", a.lines);
        let talk2 = lines(&["[07:30] Speaker 2: Der Lieferant schickt die Bestätigung an das Postfach."]);
        let ev2 = Evidence { language: "German", ..Evidence::of(&talk2, "") };
        apply(&mut map, vec![node(3, "Postfach", Kind::Customer, 1), node(4, "Lieferant", Kind::Outside, 1), flow(4, 3, "Bestätigung", 1)], &ev2, 0);
        assert_eq!(visible(&map), ["Lieferant", "Postfach"]);
        assert!(moves("Das geht dann per E-Mail raus") && moves("It imports the invoices") && moves("Die schicken wir weiter"));
        assert!(!moves("Das Postfach hat Ordner") && !moves("Der Liefertermin ist wichtig") && !moves("an important feedback"));
    }

    #[test]
    fn a_candidate_becomes_a_box_when_said_again_later() {
        let talk = lines(&[
            "[01:00] Speaker 1: Die Rechnungen landen im Postfach.",
            "[01:30] Speaker 1: Das Postfach liest Jonas jede Stunde.",
            "[02:00] Speaker 1: Und das Dashboard zeigt es an.",
        ]);
        let mut map = SystemMap::default();
        // Run 1 reads L1 and L2.
        let ev1 = Evidence { lines: &talk[..2], language: "German", ..Evidence::of(&talk, "") };
        apply(&mut map, vec![node(1, "Postfach", Kind::Customer, 1), node(2, "Postfach", Kind::Customer, 2)], &ev1, 0);
        assert_eq!(hidden(&map), ["Postfach"], "twice in one run is once");
        // Run 2: L1, L2 were read before; citing the line it was first said in again doesn't count.
        let ev2 = Evidence { new_from: 3, language: "German", ..Evidence::of(&talk, "") };
        apply(&mut map, vec![node(5, "Postfach", Kind::Customer, 1)], &ev2, 0);
        assert!(map.nodes.is_empty());
        let talk3 = lines(&["[03:00] Speaker 2: Alles aus dem Postfach geht dann weiter."]);
        let ev3 = Evidence { language: "German", ..Evidence::of(&talk3, "") };
        let a = apply(&mut map, vec![node(6, "Postfach", Kind::Customer, 1)], &ev3, 0);
        assert_eq!(visible(&map), ["Postfach"], "{:?}", a.lines);
        // In the notes: a box at once.
        let ev4 = Evidence { language: "German", ..Evidence::of(&talk, "Wie kommt das Dashboard zu den Daten?") };
        apply(&mut map, vec![node(7, "Dashboard", Kind::Ours, 3)], &ev4, 0);
        assert_eq!(visible(&map), ["Postfach", "Dashboard"]);
    }

    #[test]
    fn at_most_two_new_boxes_a_run_and_fewer_on_a_full_map() {
        let talk = lines(&["[01:00] Speaker 1: We use Shopware, Navision, LagerPro and DATEV."]);
        let ev = Evidence::of(&talk, "");
        let mut map = SystemMap::default();
        apply(&mut map, vec![node(1, "Shopware", Kind::Customer, 1), node(2, "Navision", Kind::Customer, 1), node(3, "LagerPro", Kind::Customer, 1)], &ev, 0);
        assert_eq!(visible(&map), ["Shopware", "Navision"]);
        let talk2 = lines(&["[02:00] Speaker 1: Then the warehouse, the inbox and the old server."]);
        apply(&mut map, vec![node(4, "warehouse", Kind::Customer, 1), node(5, "inbox", Kind::Customer, 1), node(6, "old server", Kind::Customer, 1)], &Evidence::of(&talk2, ""), 0);
        assert_eq!(hidden(&map), ["warehouse", "inbox", "old server"], "candidates take only node lines");
        // Past the soft cap, saying a candidate again isn't enough.
        let mut full = SystemMap::default();
        for i in 1..=SOFT_CAP as u32 {
            put(&mut full, i, &format!("Box{i}x"), Kind::Customer);
        }
        let talk = lines(&["[01:00] Speaker 1: the bank sends a file", "[02:00] Speaker 1: the bank again"]);
        apply(&mut full, vec![node(50, "bank", Kind::Outside, 1)], &Evidence::of(&talk[..1], ""), 0);
        apply(&mut full, vec![node(51, "bank", Kind::Outside, 2)], &Evidence { new_from: 2, ..Evidence::of(&talk, "") }, 0);
        assert_eq!(full.nodes.len(), SOFT_CAP);
        assert_eq!(hidden(&full), ["bank"]);
    }

    #[test]
    fn flow_ends_are_whole_words_near_the_line() {
        let talk = lines(&[
            "[20:00] Speaker 1: Der Lieferant schickt die Rechnung als PDF ins Postfach.",
            "[20:20] Speaker 1: Das ist einfach so.",
            "[20:30] Speaker 1: Ganz anderes Thema.",
            "[20:40] Speaker 1: Der Liefertermin und der Wunschliefertermin stehen im Varnor.",
            "[20:40] Speaker 1: Und das Postfach hat auch noch Ordner.",
            "[20:50] Speaker 2: Jonas liest die Rechnung dann jede Stunde aus.",
        ]);
        let ev = Evidence { language: "German", ..Evidence::of(&talk, "") };
        let mut map = SystemMap::default();
        for (id, name) in [(1, "Lieferant"), (2, "Postfach"), (3, "Varnor"), (4, "Jonas"), (5, "Wunschliefertermin")] {
            put(&mut map, id, name, Kind::Customer);
        }
        let flow = |from, to, what: &str, line| Op::Flow { from, to, what: what.into(), how: "Mail".into(), cite: Cite::Line(line) };
        let a = apply(
            &mut map,
            vec![
                flow(1, 2, "Rechnung als PDF", 1),  // real
                flow(1, 3, "Termine", 4),           // "Liefertermin" is no "Lieferant"
                flow(5, 3, "Liefertermin", 4),      // what flows is an end's name
                flow(1, 4, "Bestätigungen", 6),     // Lieferant 5 lines away
                flow(2, 4, "die Rechnung", 6),      // Jonas here, Postfach the line before: the subject
            ],
            &ev,
            0,
        );
        let got: Vec<(u32, u32)> = map.edges.iter().map(|e| (e.from, e.to)).collect();
        assert_eq!(got, [(1, 2), (2, 4)], "{:?}", a.lines);
    }

    #[test]
    fn finds_nodes_by_loose_names() {
        let mut map = SystemMap::default();
        put(&mut map, 1, "Microsoft Dynamics Business Central", Kind::Customer);
        put(&mut map, 2, "Order portal", Kind::Ours);
        put(&mut map, 3, "DHL", Kind::Outside);
        put(&mut map, 4, "Varnor", Kind::Customer);
        put(&mut map, 5, "Lieferant", Kind::Outside);
        assert_eq!(map.find("business central"), Some(1));
        assert_eq!(map.find("Microsoft Dynamics BusinessCentral"), Some(1));
        assert_eq!(map.find("Me's system"), Some(2));
        assert_eq!(map.find("my company's system"), Some(2));
        assert_eq!(map.find("Varnog"), Some(4), "a misheard letter");
        assert_eq!(map.find("Lieferanten"), Some(5), "a plural");
        assert_eq!(map.find("Lieferamt"), Some(5));
        assert_eq!(map.find("SAP"), None);
        assert_eq!(map.find("Liefertermin"), None);
        assert_eq!(map.find("DHX"), None, "short names only exactly");
    }

    #[test]
    fn words_match_as_words() {
        assert!(said_as("lieferant", "lieferanten") && said_as("dashboard", "einkaufsdashboard") && said_as("order", "orders"));
        assert!(!said_as("lieferant", "liefertermin") && !said_as("liefer", "lieferanten") && !said_as("erp", "lagerpro"));
        assert!(not_a_node("Bestellnummer") && not_a_node("Wunschliefertermin") && not_a_node("delivery date") && !not_a_node("Terminplaner"));
        assert!(near("Varnor", "Varnog") && near("Lieferant", "Lieferanten") && !near("Shopify", "Shopware") && !near("Prüfer", "Prüfung"));
    }

    #[test]
    fn support_matches_word_stems() {
        assert_eq!(support("exports invoices monthly", "Once a month I export all invoices"), Some(1.0));
        assert_eq!(support("exports invoices weekly", "Once a month I export all invoices"), Some(2.0 / 3.0));
        assert_eq!(support("it is", "anything"), None);
        assert_eq!(support("Bankdatei", "Die Bank schickt jeden Morgen eine Datei"), Some(1.0));
        assert!(uncertain("I think it's CAMT, but I would have to check.") && uncertain("Welches Format hat die Datei?"));
        assert!(!uncertain("It's called LagerPro."));
    }

    #[test]
    fn exports_in_the_fixed_shape() {
        let lines = lines(&["[00:10] Speaker 1: The order portal sends orders to Business Central by API; Laura decides on API access; orders are typed in by hand.",
                         "[00:20] Me: Is the API switched on?"]);
        let ev = Evidence::of(&lines, "");
        let mut map = SystemMap::default();
        put(&mut map, 1, "Laura", Kind::Person);
        put(&mut map, 2, "Business Central", Kind::Customer);
        put(&mut map, 3, "Order portal", Kind::Ours);
        map.candidates.push(Node { id: 4, name: "Hidden".into(), kind: Kind::Customer, known: Vec::new(), aka: Vec::new(), at_ms: 0, named: false, said: String::new() });
        apply(
            &mut map,
            vec![
                Op::Known { n: 2, fact: "Orders are typed in by hand".into(), cite: Cite::Line(1) },
                Op::Flow { from: 3, to: 2, what: "orders".into(), how: "API".into(), cite: Cite::Line(1) },
                Op::Open { n: 2, text: "Is the API\nswitched on?".into(), cite: Cite::Line(2) },
                Op::Known { n: 1, fact: "Decides on API access".into(), cite: Cite::Line(1) },
            ],
            &ev,
            0,
        );
        assert_eq!(
            section(&map).unwrap(),
            "## System map\n\
             ### Order portal (our system)\n\
             - Connects to Business Central: orders, API\n\
             ### Business Central (customer system)\n\
             - Known: Orders are typed in by hand\n\
             - Open: Is the API switched on?\n\
             ### Laura (person or role)\n\
             - Known: Decides on API access"
        );
        assert_eq!(section(&SystemMap::default()), None);
    }

    #[test]
    fn prompt_numbers_earlier_and_new_lines() {
        let mut map = SystemMap::default();
        put(&mut map, 1, "LagerPro", Kind::Customer);
        put(&mut map, 2, "DATEV", Kind::Outside);
        map.nodes[0].known.push("the warehouse system".into());
        map.nodes[1].known.push("the tax advisor's".into());
        let earlier = vec!["[00:58] Me: And then?".to_string()];
        let new = vec!["[01:01] Speaker 1: LagerPro exports a CSV.".to_string()];
        let input = Input {
            notes: "",
            language: Some("en"),
            new: &[],
            said_before: &[],
            visuals: &[],
            shown_before: &[],
            spellings: &[],
            earlier: &earlier,
            speakers: &[],
            map: &map,
            notes_changed: false,
            room: Room::Call,
        };
        let p = user_prompt(&input, &earlier, &new, "focus on the ERP export", &[3, 4, 5], "English");
        assert!(p.contains("N1 LagerPro (customer system)\nN2 DATEV (outside service)\n"), "{p}");
        assert!(p.contains("N1 known: the warehouse system") && !p.contains("tax advisor's"), "facts only of boxes the new talk names: {p}");
        assert!(p.contains("L1 [00:58] Me: And then?\nL2 [01:01] Speaker 1: LagerPro exports a CSV.\n"));
        assert!(p.contains("L1 to L1 were read before, L2 on are new") && p.contains("New node numbers: N3, N4, N5. Write in English."));
        // What changes least comes first: the boxes, then the notes, then what changes every run.
        let (boxes, notes, facts) = (p.find("N2 DATEV").unwrap(), p.find("focus on the ERP export").unwrap(), p.find("N1 known").unwrap());
        assert!(boxes < notes && notes < facts && facts < p.find("L1 [00:58]").unwrap(), "{p}");
    }

    #[test]
    fn names_heard_in_different_ways_are_one() {
        assert!(variant("Navision", "Navision-Modul") && variant("Navision Core", "navision"));
        assert!(variant("Shopware", "Shopwear") && variant("Teamviewer", "Teamviewer Engine"));
        assert!(variant("Lagerprofi", "Lagerprofie") && variant("Smartfoo", "Smartfo"));
        assert!(!variant("Windows 10", "Windows 11") && !variant("Anna", "Anne") && !variant("SAP", "SAPS"));
        assert!(!variant("Navision", "Navision Builder") && !variant("Outlook", "Lookout"));
        // One letter off a shown spelling, from four letters on.
        assert!(heard_as("ECAD", "eCard") && heard_as("ECAD", "E-Card Modul") && !heard_as("ECAD", "IKAD") && !heard_as("SKU", "SKI"));
        let shown = vec!["ECAD".to_string(), "Navision".to_string()];
        assert_eq!(shown_spelling("E-Card Modul", &shown).as_deref(), Some("ECAD Modul"));
        assert_eq!(shown_spelling("Navisjon", &shown).as_deref(), Some("Navision"));
        assert_eq!(shown_spelling("ECAD", &shown), None, "spelled as shown already");
        assert_eq!(shown_spelling("Outlook", &shown), None);
    }

    #[test]
    fn boxes_heard_alike_merge_under_the_shown_spelling() {
        let talk = lines(&[
            "[10:00] Speaker 1: Das Navision Modul holt die Aufträge.",
            "[10:20] Speaker 1: Und das eCard exportiert dann pro Station ein XML.",
        ]);
        let mut map = SystemMap::default();
        put(&mut map, 1, "Navision", Kind::Customer);
        put(&mut map, 2, "Navisjon Core", Kind::Customer);
        put(&mut map, 3, "eCard", Kind::Ours);
        put(&mut map, 4, "Anna", Kind::Person);
        put(&mut map, 5, "Anne", Kind::Person);
        map.edges.push(Edge { from: 2, to: 3, what: "Aufträge".into(), how: "unbekannt".into() });
        let shown = vec!["ECAD".to_string()];
        let ev = Evidence { language: "German", spellings: &shown, ..Evidence::of(&talk, "") };
        let a = apply(&mut map, vec![node(6, "E-Card", Kind::Ours, 2)], &ev, 0);
        assert_eq!(visible(&map), ["Navision", "ECAD", "Anna", "Anne"], "{:?}", a.lines);
        assert!(map.nodes[0].aka.contains(&"Navisjon Core".to_string()) && map.nodes[1].aka.contains(&"eCard".to_string()));
        assert_eq!((map.edges[0].from, map.edges[0].to), (1, 3), "the flow moves with the box");
        assert!(map.candidates.is_empty(), "the heard spelling of a box is that box: {:?}", a.lines);
    }

    #[test]
    fn garbled_names_foreign_facts_and_withheld_flows_are_left_out() {
        assert!(garbled("IGE-XAO_SeeAutoDiag") && garbled("Diag r amGeneration") && !garbled("Plan B") && !garbled("eCard"));
        assert!(in_language("wird im Admin-Bereich gepflegt", "German") && !in_language("is used for the export of the data", "German"));
        assert!(in_language("Preisdaten", "German") && in_language("exports it to the ERP", "English") && !in_language("wird von der IT gepflegt", "English"));
        assert!(withheld("Preise", "Nur dann die Preise vorher auslöschen, bevor sie es einspielen."));
        assert!(withheld("prices", "They import it without the prices.") && !withheld("Preise", "Da stehen die Preise drinnen, die gehen ins ERP."));
        let talk = lines(&["[50:17] Speaker 1: Nur dann die Preise vorher auslöschen, bevor sie es in Navision einspielen, im Shopware stehen sie."]);
        let mut map = SystemMap::default();
        put(&mut map, 1, "Shopware", Kind::Customer);
        put(&mut map, 2, "Navision", Kind::Customer);
        map.nodes[0].known.push("x".into());
        let ev = Evidence { language: "German", ..Evidence::of(&talk, "") };
        let a = apply(
            &mut map,
            vec![
                Op::Flow { from: 1, to: 2, what: "Preise".into(), how: "Datei".into(), cite: Cite::Line(1) },
                Op::Known { n: 1, fact: "the prices are in the Shopware".into(), cite: Cite::Line(1) },
            ],
            &ev,
            0,
        );
        assert!(map.edges.is_empty() && map.nodes[0].known == ["x"], "{:?}", a.lines);
    }

    #[test]
    fn earlier_lines_keep_their_latest_words() {
        let long = format!("[01:00] Speaker 1: {}", "und dann geht das weiter ".repeat(20));
        let lines = vec!["[00:50] Me: Old.".to_string(), long, "[01:40] Me: And then?".to_string()];
        let kept = latest_words(lines.clone(), 140);
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert!(kept[0].starts_with("[01:00] Speaker 1: …") && kept[0].ends_with("weiter ") && kept[0].chars().count() <= 140, "{kept:?}");
        assert_eq!(kept[1], "[01:40] Me: And then?");
        assert_eq!(latest_words(lines.clone(), 10_000), lines);
    }

    #[test]
    fn the_instructions_stay_the_same_with_or_without_screens() {
        // The helper keeps the instructions between runs only when they don't change.
        let p = system_prompt("German", Room::Call);
        assert!(p.contains("Screen:") && p.ends_with(llm::SHOWN.trim()), "{p}");
    }

    #[test]
    fn a_screen_shows_only_its_new_rows() {
        use super::super::visuals::{Source, Visual};
        let screen = |id: u32, text: &str| Visual { id, at_ms: id as u64 * 10_000, until_ms: None, source: Source::Screen, label: String::new(), text: text.into(), image: None };
        let before = [screen(1, "Roadmap und Meilenst | Weekly Update\nOne plant configuration as the single source of truth")];
        let now = [
            // The same slide read again with OCR jitter, and one new row.
            screen(2, "Roadmap und Mellenst | Weekly Update\nOne plant configuraton as the single source of truth\nXML export per station"),
            // Nothing new at all: left out.
            screen(3, "Roadmap und Meilenst | Weekly Update"),
        ];
        let fresh = fresh_rows(&now, &before);
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].text, "XML export per station");
        // A photo stays whole.
        let photo = Visual { source: Source::Photo, ..screen(4, "Roadmap und Meilenst | Weekly Update") };
        assert_eq!(fresh_rows(&[photo], &before).len(), 1);
    }

    #[test]
    fn a_candidate_the_new_talk_names_is_shown_with_its_number() {
        let mut map = SystemMap::default();
        put(&mut map, 1, "Business Central", Kind::Customer);
        map.candidates.push(Node { id: 7, name: "tax advisor".into(), kind: Kind::Outside, known: Vec::new(), aka: Vec::new(), at_ms: 0, named: false, said: "We send it to our tax advisor.".into() });
        map.candidates.push(Node { id: 8, name: "Pömmel".into(), kind: Kind::Customer, known: Vec::new(), aka: Vec::new(), at_ms: 0, named: false, said: String::new() });
        let text = news_text(&map, "", "So the tax advisor imports that into DATEV?", Room::Call);
        assert!(text.contains("not on the map (no box unless the new lines show it is a system, service or person): N7 tax advisor (outside service)\n") && !text.contains("Pömmel"), "{text}");
        // A flow to it (with something moving) brings it in.
        let talk = lines(&["[02:00] Speaker 1: Tom sends the invoices to the tax advisor by email."]);
        put(&mut map, 2, "Tom", Kind::Person);
        apply(&mut map, vec![Op::Flow { from: 2, to: 7, what: "invoices".into(), how: "mail".into(), cite: Cite::Line(1) }], &Evidence::of(&talk, ""), 0);
        assert_eq!(visible(&map), ["Business Central", "Tom", "tax advisor"]);
    }

    #[test]
    fn in_one_room_nobody_is_me() {
        assert_eq!(unlabel_me("[00:10] Me: Unser ERP ist Varnor.".into(), Room::OneMic), "[00:10] Speaker 0: Unser ERP ist Varnor.");
        assert_eq!(unlabel_me("[00:10] Me: Hello.".into(), Room::Call), "[00:10] Me: Hello.");
        let p = system_prompt("German", Room::OneMic);
        assert!(p.contains("\"my company's system\"") && !p.contains("Me's") && !p.contains("] Me:"), "{p}");
        assert!(system_prompt("English", Room::Call).contains("L3 [03:20] Me: Who looks"));
    }

    #[test]
    fn the_tidy_up_merges_drops_and_rekinds() {
        let mut map = SystemMap::default();
        put(&mut map, 1, "Varnor", Kind::Customer);
        put(&mut map, 2, "Lieferant", Kind::Outside);
        put(&mut map, 3, "Varnocker", Kind::Ours);
        put(&mut map, 4, "Pömmel", Kind::Person);
        put(&mut map, 5, "Prüfprozess", Kind::Ours);
        put(&mut map, 6, "Kollegin Huber", Kind::Customer);
        put(&mut map, 7, "Excel-Liste", Kind::Customer);
        map.nodes[5].named = true;
        map.nodes[2].known.push("liefert Daten".into());
        map.edges.push(Edge { from: 3, to: 2, what: "Bestellungen".into(), how: "unbekannt".into() });
        map.edges.push(Edge { from: 4, to: 2, what: "Termine".into(), how: "unbekannt".into() });
        assert_eq!(look_alike(&map), [(1, 3)], "Varnor and Varnocker look alike");
        let g = tidy_grammar(&[1, 2, 3], &[(1, 3)], Room::OneMic);
        assert!(g.starts_with("root ::= \"N1 = N3: \" (\"same\" | \"different\") \"\\n\" \"N1: \" verdict \"\\n\" \"N2: \" verdict \"\\n\" \"N3: \" verdict \"\\n\"\n"), "{g}");
        assert!(!g.contains("same as"));
        let verdicts = parse_tidy("N1 = N3: same\nN2 = N4: different\nN1: customer system\nN2: person or role\nN3: customer system\nN4: misheard\nN5: feature or process\nN6: person or role\nN7: customer system\n");
        assert_eq!(verdicts[0], (1, Verdict::Same(3)));
        assert_eq!(verdicts[1], (1, Verdict::Box));
        assert_eq!(verdicts[4], (4, Verdict::NotBox));
        assert!(parse_tidy("N2: same as N1\nN3: no idea\n").is_empty(), "only the pairs merge; unknown words are no verdict");
        assert_eq!(verdicts.len(), 8, "a pair said to be different is no verdict");
        let a = apply_tidy(&mut map, &verdicts, "");
        assert_eq!(visible(&map), ["Varnor", "Lieferant", "Pömmel", "Kollegin Huber", "Excel-Liste"], "{:?}", a.lines);
        let mut people = SystemMap::default();
        put(&mut people, 1, "Frau Martina", Kind::Person);
        put(&mut people, 2, "Martina", Kind::Person);
        put(&mut people, 3, "Frau Sabine", Kind::Person);
        assert_eq!(look_alike(&people), [(1, 2)], "two first names are two people");
        assert_eq!(map.nodes[0].aka, ["Varnocker"]);
        assert_eq!(map.nodes[0].known, ["liefert Daten"]);
        assert!(map.edges.iter().any(|e| (e.from, e.to) == (1, 2)), "the flow moved to the box that stays");
        assert!(!map.edges.iter().any(|e| e.from == 3 || e.to == 3));
        assert_eq!(map.nodes[1].kind, Kind::Outside, "the tidy-up doesn't re-kind");
        assert!(map.is_set_aside("Prüfprozess"), "dropped names stay away");
        assert!(map.find("Pömmel").is_some(), "a box with a flow isn't dropped as misheard");
    }

    #[test]
    fn an_old_map_still_loads() {
        let old = r#"{"nodes":[{"id":1,"name":"SAP","kind":"customer","known":[],"atMs":0}],"edges":[],"open":[],"opened":0}"#;
        let map: SystemMap = serde_json::from_str(old).unwrap();
        assert!(map.candidates.is_empty() && !map.nodes[0].named);
        let back = serde_json::to_string(&map).unwrap();
        assert!(!back.contains("candidates") && !back.contains("named") && !back.contains("setAside"), "{back}");
    }
}
