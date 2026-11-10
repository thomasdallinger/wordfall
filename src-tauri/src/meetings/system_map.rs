//! System map: while a meeting is recorded, the notes model keeps a small map of the systems,
//! people and roles that come up, how they connect (what flows, and how: API, CSV, mail, by hand)
//! and what is still unknown about them. Each run reads only the talk since the last run, plus the
//! map as it stands and the user's notes, and answers with small edits ("node", "alias", "known",
//! "flow", "open", "solved"), each citing the transcript line it comes from; the edits are checked
//! against that line before they change the map. A run costs about the same early and late in a
//! meeting.
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

/// Edits one run may make, small steps: new nodes, and other lines.
const MAX_NEW: usize = 3;
const MAX_LINES: usize = 7;
const ANSWER_TOKENS: u32 = 300;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// What Me's side builds or runs.
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
    /// meeting say "our ERP" all the time, and a small model then files their systems as Me's.
    fn word(self) -> &'static str {
        match self {
            Kind::Ours => "Me's system",
            k => k.label(),
        }
    }

    const ALL: [Kind; 4] = [Kind::Ours, Kind::Customer, Kind::Outside, Kind::Person];

    fn from_word(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.word() == s.trim())
    }
}

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

impl SystemMap {
    /// The node called `name` (or once called so). A small model doesn't always repeat a name
    /// exactly, so failing that: the same letters ("BusinessCentral"), the one node whose name holds
    /// it or is held by it ("Business Central" for "Microsoft Dynamics Business Central"), or for
    /// "Me's system" the one node of that kind.
    pub fn find(&self, name: &str) -> Option<u32> {
        let names = |x: &'_ Node| -> Vec<String> { std::iter::once(&x.name).chain(&x.aka).cloned().collect() };
        let n = norm(name);
        if let Some(x) = self.nodes.iter().find(|x| names(x).iter().any(|a| norm(a) == n)) {
            return Some(x.id);
        }
        let c = compact(name);
        if c.chars().count() < 3 {
            return None;
        }
        if let Some(x) = self.nodes.iter().find(|x| names(x).iter().any(|a| compact(a) == c)) {
            return Some(x.id);
        }
        let unique = |hits: Vec<u32>| (hits.len() == 1).then(|| hits[0]);
        // Whole words, not letters: "ERP" is not in "LagerPro".
        let mine = words(name);
        let holds = |a: &[String], b: &[String]| !a.is_empty() && a.iter().all(|w| b.contains(w));
        let holding: Vec<u32> = self
            .nodes
            .iter()
            .filter(|x| names(x).iter().any(|a| {
                let theirs = words(a);
                compact(a).chars().count() >= 3 && (holds(&mine, &theirs) || holds(&theirs, &mine))
            }))
            .map(|x| x.id)
            .collect();
        unique(holding).or_else(|| {
            matches!(n.as_str(), "me's system" | "our system" | "me")
                .then(|| unique(self.nodes.iter().filter(|x| x.kind == Kind::Ours).map(|x| x.id).collect()))
                .flatten()
        })
    }

    fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    fn name_of(&self, id: u32) -> &str {
        self.node(id).map(|n| n.name.as_str()).unwrap_or("?")
    }

    fn is_dropped(&self, name: &str) -> bool {
        let n = norm(name);
        self.dropped.iter().any(|d| norm(d) == n)
    }

    /// Open points as "Name: what is unknown", for the questions.
    pub fn unknowns(&self) -> Vec<String> {
        self.open.iter().map(|o| format!("{}: {}", self.name_of(o.node), o.text)).collect()
    }

    /// The user renames a node. The old name stays known to the model's edits.
    pub fn rename(&mut self, id: u32, name: &str) -> anyhow::Result<()> {
        let name = cut(name, NAME_MAX);
        anyhow::ensure!(!name.is_empty(), "A name can't be empty.");
        anyhow::ensure!(self.find(&name).is_none_or(|other| other == id), "Another box already has that name.");
        let node = self.nodes.iter_mut().find(|n| n.id == id).ok_or_else(|| anyhow::anyhow!("That box is gone."))?;
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
        let i = self.nodes.iter().position(|n| n.id == id).ok_or_else(|| anyhow::anyhow!("That box is gone."))?;
        let node = self.nodes.remove(i);
        self.dropped_ids = self.dropped_ids.max(node.id);
        self.dropped.push(node.name);
        self.dropped.extend(node.aka);
        self.edges.retain(|e| e.from != id && e.to != id);
        self.open.retain(|o| o.node != id);
        Ok(())
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
    Alias { n: u32, name: String, cite: Cite },
    Known { n: u32, fact: String, cite: Cite },
    Flow { from: u32, to: u32, what: String, how: String, cite: Cite },
    Open { n: u32, text: String, cite: Cite },
    Solved { number: u32, fact: String, cite: Cite },
}

/// What the edits are checked against: the transcript lines the model saw (numbered from 1) and
/// the notes.
pub struct Evidence<'a> {
    pub lines: &'a [String],
    pub notes: &'a str,
}

impl Evidence<'_> {
    /// The cited text, with `around` lines on either side.
    fn around(&self, cite: Cite, around: usize) -> String {
        match cite {
            Cite::Notes => self.notes.to_string(),
            Cite::Line(l) if l >= 1 && l <= self.lines.len() => {
                let from = l.saturating_sub(around + 1);
                let to = (l + around).min(self.lines.len());
                self.lines[from..to].iter().map(|x| x.split_once(": ").map(|(_, t)| t).unwrap_or(x)).collect::<Vec<_>>().join("\n")
            }
            Cite::Line(_) => String::new(),
        }
    }
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

/// Words that name a kind of thing rather than one thing ("the system", "our team").
const GENERIC: &[&str] = &[
    "system", "systems", "service", "team", "tool", "software", "data", "server", "platform", "erp", "crm", "shop",
    "webshop", "portal", "app", "application", "warehouse", "database", "inbox", "datenbank", "firma", "kunde", "kunden",
];

/// Things that move between systems, or where files are dropped, never a box of their own (file
/// formats, channels, folders): a name with one of these words is how something flows, not a node.
const NOT_NODES: &[&str] = &[
    "csv", "pdf", "xml", "json", "camt", "mt940", "excel", "xlsx", "email", "mail", "api", "file", "files", "datei",
    "dateien", "edi", "ftp", "sftp", "folder", "ordner", "export", "format", "access", "zugang", "schnittstelle",
];

/// Whether a name is a format, a channel, a folder or "Me" rather than a system or person.
fn not_a_node(name: &str) -> bool {
    let w = words(name);
    // "200 orders per day": a number, not a box.
    let number = name.trim().starts_with(|c: char| c.is_ascii_digit());
    number || matches!(compact(name).as_str(), "me" | "ich" | "we" | "wir") || w.iter().any(|x| NOT_NODES.contains(&x.as_str())) || NOT_NODES.contains(&compact(name).as_str())
}

/// Two words match on their first five letters ("invoices" ~ "invoice", "exports" ~ "exported").
fn same_word(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let n = 5.min(a.len()).min(b.len());
    if a.len() < 5 || b.len() < 5 {
        return a == b;
    }
    a[..n] == b[..n]
}

/// The share of a claim's content words (four letters or more, not little words) that the
/// evidence has; `None` when it has no content words.
fn support(claim: &str, evidence: &str) -> Option<f32> {
    let have = words(evidence);
    let content: Vec<String> = words(claim).into_iter().filter(|w| w.chars().count() >= 4 && !STOP.contains(&w.as_str())).collect();
    if content.is_empty() {
        return None;
    }
    let hit = content.iter().filter(|w| have.iter().any(|h| same_word(w, h))).count();
    Some(hit as f32 / content.len() as f32)
}

/// Whether two texts share a word of three letters or more that says something ("API" counts).
fn shares_word(a: &str, b: &str) -> bool {
    const LITTLE: &[&str] = &["the", "and", "for", "was", "has", "are", "you", "our", "who", "how", "why", "not", "can", "its", "der", "die", "das", "und", "wer", "wie", "ist", "hat", "mit", "von", "den"];
    let b = words(b);
    words(a).iter().any(|w| w.chars().count() >= 3 && !LITTLE.contains(&w.as_str()) && !STOP.contains(&w.as_str()) && b.iter().any(|x| same_word(w, x)))
}

/// Whether a name was said in `text`: as a whole (letters only), or every word of it.
fn name_in(name: &str, text: &str) -> bool {
    let c = compact(name);
    if c.chars().count() >= 3 && compact(text).contains(&c) {
        return true;
    }
    let have = words(text);
    let w = words(name);
    !w.is_empty() && w.iter().all(|x| x.chars().count() <= 1 || have.contains(x))
}

/// Whether a node is mentioned in `text`: a distinctive word of its name or another name it has.
fn mentioned(node: &Node, text: &str) -> bool {
    let have = words(text);
    std::iter::once(&node.name).chain(&node.aka).any(|n| {
        name_in(n, text)
            || words(n).iter().any(|w| w.chars().count() >= 3 && !GENERIC.contains(&w.as_str()) && !STOP.contains(&w.as_str()) && have.iter().any(|h| same_word(w, h)))
    })
}

fn is_generic(name: &str) -> bool {
    let w = words(name);
    !w.is_empty() && w.iter().all(|x| GENERIC.contains(&x.as_str()) || matches!(x.as_str(), "our" | "the" | "unser" | "unsere" | "das" | "die" | "der"))
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
/// - a new node's name was said there (and isn't a file format, a channel or "Me");
/// - a fact's words are mostly there;
/// - a flow's two ends are both mentioned there or close by, and what flows too;
/// - an open point cites a question, someone unsure, or the notes;
/// - an open point is solved only by a fact that is mostly in its line.
///
/// Edits on nodes the user deleted are skipped. Nodes come first, so a flow can go to a node the
/// same answer adds.
pub fn apply(map: &mut SystemMap, ops: Vec<Op>, ev: &Evidence, now_ms: u64) -> Applied {
    let mut out = Applied::default();
    // The model's node numbers -> real ids (existing nodes keep theirs).
    let mut ids: std::collections::HashMap<u32, u32> = map.nodes.iter().map(|n| (n.id, n.id)).collect();
    let (nodes, rest): (Vec<Op>, Vec<Op>) = ops.into_iter().partition(|o| matches!(o, Op::Node { .. }));
    for op in nodes.into_iter().chain(rest) {
        match op {
            Op::Node { n, name, kind, aka, cite } => {
                let name = cut(&name, NAME_MAX);
                if name.is_empty() || map.is_dropped(&name) || not_a_node(&name) {
                    continue;
                }
                let line = ev.around(cite, 2);
                if !name_in(&name, &line) {
                    out.lines.push(format!("(skipped node {name}: not in its line)"));
                    continue;
                }
                let aka = aka.map(|a| cut(&a, NAME_MAX)).filter(|a| !a.is_empty() && name_in(a, &ev.around(cite, 2)));
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
                        .filter(|x| is_generic(&x.name) && !is_generic(&name) && (x.kind == kind || x.kind == Kind::Person) && name_in(&x.name, &before))
                        .map(|x| x.id)
                        .collect();
                    (hits.len() == 1).then(|| hits[0])
                };
                let known = map.find(&name).or_else(|| aka.as_deref().and_then(|a| map.find(a))).or_else(generic_here);
                if let Some(id) = known {
                    ids.insert(n, id);
                    let node = map.nodes.iter_mut().find(|x| x.id == id).expect("found above");
                    // "our ERP, that's Business Central": the proper name takes over from the generic one.
                    if is_generic(&node.name) && !is_generic(&name) && norm(&node.name) != norm(&name) {
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
                let id = map.nodes.iter().map(|x| x.id).max().unwrap_or(0).max(map.dropped_ids) + 1;
                ids.insert(n, id);
                out.lines.push(format!("+ {name} ({}){}", kind.label(), aka.as_deref().map(|a| format!(", also {a}")).unwrap_or_default()));
                map.nodes.push(Node { id, name, kind, known: Vec::new(), aka: aka.into_iter().collect(), at_ms: now_ms });
            }
            Op::Alias { n, name, cite } => {
                let Some(&id) = ids.get(&n) else { continue };
                let name = cut(&name, NAME_MAX);
                if !name_in(&name, &ev.around(cite, 1)) || map.find(&name).is_some_and(|other| other != id) {
                    continue;
                }
                let node = map.nodes.iter_mut().find(|x| x.id == id).expect("mapped");
                if is_generic(&node.name) && !is_generic(&name) {
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
                let about = map.node(id).is_some_and(|node| mentioned(node, &ev.around(cite, 2)));
                let only_name = map.node(id).is_some_and(|node| words(&fact).iter().all(|w| words(&node.name).contains(w)));
                if !about || only_name || support(&fact, &ev.around(cite, 1)).is_none_or(|s| s < 0.5) {
                    out.lines.push(format!("(skipped fact on {}: not in its line) {fact}", map.name_of(id)));
                    continue;
                }
                if add_known(map, id, fact.clone()) {
                    out.lines.push(format!("{}: {fact}", map.name_of(id)));
                }
            }
            Op::Flow { from, to, what, how, cite } => {
                let (Some(&a), Some(&b)) = (ids.get(&from), ids.get(&to)) else { continue };
                if a == b {
                    continue;
                }
                let (what, how) = (cut(&what, TEXT_MAX), cut(&how, 40));
                let near = ev.around(cite, 2);
                // "API" when nobody said how: unknown.
                let how = if unknown_how(&how) || how_said(&how, &near) { how } else { hows(ev_language(&how)).last().copied().unwrap_or("unknown").to_string() };
                let label = format!("{} -> {}: {what}, {how}", map.name_of(a), map.name_of(b));
                let ends = [a, b].iter().all(|id| map.node(*id).is_some_and(|n| mentioned(n, &near)));
                if what.is_empty() || says_unknown(&what) || !ends || support(&what, &ev.around(cite, 1)).is_some_and(|s| s == 0.0) {
                    out.lines.push(format!("(skipped flow {label}: not in its lines)"));
                    continue;
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
                let text = cut(&text, TEXT_MAX);
                // Someone asked or wasn't sure (in that line or next to it), or the notes want it.
                let wanted = cite == Cite::Notes || support(&text, ev.notes).is_some_and(|s| s >= 0.5);
                if !(wanted || uncertain(&ev.around(cite, 1))) {
                    out.lines.push(format!("(skipped open point on {}: nothing uncertain in its line) {text}", map.name_of(id)));
                    continue;
                }
                let repeat = map.open.iter().any(|o| similar(&o.text, &text))
                    || map.node(id).is_some_and(|node| node.known.iter().any(|k| similar(k, &text)));
                if text.is_empty() || repeat || map.open.len() >= MAX_OPEN {
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
                let unsure = matches!(cite, Cite::Line(_)) && uncertain(&ev.around(cite, 0));
                if says_unknown(&fact) || !answers || unsure || support(&fact, &ev.around(cite, 1)).is_none_or(|s| s < 0.5) {
                    out.lines.push(format!("(O{number} not solved: no answer in its line) {fact}"));
                    continue;
                }
                let o = map.open.remove(i);
                out.lines.push(format!("O{number} solved: {fact}"));
                add_known(map, o.node, fact);
            }
        }
    }
    out
}

fn add_known(map: &mut SystemMap, id: u32, fact: String) -> bool {
    let Some(node) = map.nodes.iter_mut().find(|n| n.id == id) else { return false };
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
                    Some(Op::Node { n: n(&arg)?, name: name.trim().to_string(), kind: Kind::from_word(kind)?, aka, cite })
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

/// The answer's form: up to `MAX_NEW` "node" lines first, then up to `MAX_LINES` other lines, each
/// ending in the line it comes from ("[L4]", "[notes]"). Node numbers, line numbers, open point
/// numbers and the kinds of "how" are fixed lists, so the model can only point at what exists.
/// Each text has a word limit and words at most 20 letters (left free, a small model goes on
/// inside a field until it runs out of tokens, or runs words together at the limit), and no CJK
/// or full-width characters (it drifts into them). The answer may end right after its last line:
/// made to write a line break first, it pads the line with spaces instead.
fn grammar(existing: &[u32], new: &[u32], lines: usize, open: &[u32], language: &str) -> String {
    let ids = alternatives(existing.iter().chain(new).map(|n| format!("N{n}")));
    let new_ids = alternatives(new.iter().map(|n| format!("N{n}")));
    let cites = alternatives((1..=lines).map(|l| format!("L{l}")).chain(["notes".to_string()]));
    let opens = alternatives(open.iter().map(|o| format!("O{o}")));
    let solved = if open.is_empty() { "" } else { " | solved" };
    let how = alternatives(hows(language).iter().map(|h| h.to_string()));
    format!(
        "root ::= (node \"\\n\"){{0,{MAX_NEW}}} (item (\"\\n\" item){{0,{more}}} \"\\n\"?)?\n\
         node ::= \"node \" {new_ids} \": \" name \" (\" kind \")\" (\" also called \" name)? cite\n\
         kind ::= \"customer system\" | \"outside service\" | \"person or role\" | \"Me's system\"\n\
         item ::= alias | known | flow | open{solved}\n\
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
        more = MAX_LINES - 1
    )
}

// ---------- prompt ----------

pub struct Input<'a> {
    pub notes: &'a str,
    pub language: Option<&'a str>,
    /// Talk since the last run.
    pub new: &'a [Segment],
    /// The last lines before it (already read), for context.
    pub earlier: &'a [String],
    pub speakers: &'a [Speaker],
    pub map: &'a SystemMap,
    /// The notes changed since the last run (then it runs even without new talk).
    pub notes_changed: bool,
}

/// The map as the model reads it: numbered nodes, their facts, flows and open points.
fn map_text(map: &SystemMap) -> String {
    if map.nodes.is_empty() {
        return "(empty)\n".into();
    }
    let mut s = String::new();
    for n in &map.nodes {
        s.push_str(&format!("N{} {} ({})", n.id, n.name, n.kind.word()));
        if !n.aka.is_empty() {
            s.push_str(&format!(", also called {}", n.aka.join(", ")));
        }
        s.push('\n');
        for k in &n.known {
            s.push_str(&format!("  known: {k}\n"));
        }
        for e in map.edges.iter().filter(|e| e.from == n.id) {
            s.push_str(&format!("  flow N{} -> N{}: {} | how: {}\n", e.from, e.to, e.what, e.how));
        }
        for o in map.open.iter().filter(|o| o.node == n.id) {
            s.push_str(&format!("  open O{}: {}\n", o.number, o.text));
        }
    }
    s
}

/// Instructions and a worked example; the same in every run of a meeting (it comes first, so a
/// helper that keeps the start of the prompt doesn't read it again).
fn system_prompt(language: &str) -> String {
    let hows = hows(language).join(", ");
    format!(
        "You keep a map of a meeting for \"Me\", the person who recorded it: the systems, outside services, people \
and roles that come up, what flows between them and how, and what is still unknown. The transcript comes from \
speech recognition and has mistakes.

Kinds: \"customer system\" runs at the organisation Me is talking to (when they say \"our ERP\" or \"our shop\", \
that is a customer system); \"outside service\" belongs to a third party (a carrier, a bank, a hosting agency, a \
tax advisor and their software); \"person or role\" is a person, team or role; \"Me's system\" is only what Me's \
own company builds or runs (Me says \"we are building\"). A file format (CSV, PDF, XML), a channel (mail, API), a \
shared folder or a file is never a node: it is how something flows between two nodes.

Each run you get the map so far (nodes numbered N1, N2, …, open points O1, O2, …), my notes, and the talk as \
numbered lines; the lines before the new ones were read already. Write only what the new lines (and my notes) \
add, one change per line, each ending with the line it comes from in brackets ([L4], or [notes]):
node N7: Name (kind) [L2]   a system, service, person or role that data or work passes through; use the new \
numbers offered. Add \"also called X\" before the bracket when the talk gives it a second name (\"our ERP, that's \
Business Central\").
alias N2: other name [L5]   a node on the map is called something else too.
known N2: a short fact about it [L3]   only what the line says.
flow N2 -> N7: what flows | how: one of {hows} [L4]   from where the data leaves to where it arrives, only when \
the talk says something moves between those two. Two things named in one sentence are not a flow.
open N2: a short question that is still open? [L6]   only when someone asks it and it isn't answered, someone \
isn't sure, or my notes want it found out; give each thing my notes want found out its own open point.
solved O3: the fact that answers it [L8]   when a new line answers an open point.
Nodes first, then the other lines. Use the node numbers, never names, after \"node\". Never invent: if it isn't \
in the line you cite, leave it out. Write nothing when the new lines add nothing. Write facts, flows and open \
points in {language}; keep names as they were said.

Example.
Map so far:
N1 Webshop (customer system)
Talk (L1 was read before, L2 on are new):
L1 [03:10] Speaker 1: Our webshop takes the orders.
L2 [03:14] Speaker 1: Every hour the orders go to Navision as an XML file.
L3 [03:20] Me: Who looks at failed imports?
L4 [03:23] Speaker 1: Nobody, I think. Petra from finance also works in Navision, on the invoices.
New node numbers: N2, N3, N4.
Answer:
node N2: Navision (customer system) [L2]
node N3: Petra from finance (person or role) [L4]
flow N1 -> N2: orders, every hour | how: file [L2]
known N3: works on the invoices in Navision [L4]
open N2: Who looks at failed imports? [L3]
(Petra works in Navision, but nothing was said to flow between them: no flow line.)"
    )
}

fn user_prompt(input: &Input, earlier: &[String], new: &[String], notes: &str, new_ids: &[u32]) -> String {
    let mut p = format!("Map so far:\n{}", map_text(input.map));
    if !input.map.dropped.is_empty() {
        p.push_str(&format!("Removed by me, never add again: {}\n", input.map.dropped.join(", ")));
    }
    if !notes.is_empty() {
        p.push_str(&format!("\nMy notes (what to focus on and what the map must find out):\n{notes}\n"));
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
    p.push_str(&format!("New node numbers: {}.\nAnswer:", ids.join(", ")));
    p
}

/// What one run asked and answered, for measuring.
pub struct Run {
    pub ops: Vec<Op>,
    /// The numbered lines the run read, to check the edits against.
    pub lines: Vec<String>,
    pub notes: String,
    /// The answer as written.
    pub answer: String,
    /// What the model call cost.
    pub cost: llm::ChatStats,
}

impl Run {
    pub fn evidence(&self) -> Evidence<'_> {
        Evidence { lines: &self.lines, notes: &self.notes }
    }
}

/// One run: asks the model how the new talk (and changed notes) change the map. `None` when
/// there's nothing new to read.
pub fn run(worker: &mut llm::Worker, input: &Input) -> anyhow::Result<Option<Run>> {
    let mut new = super::pipeline::transcript_lines(input.new, input.speakers);
    let notes: String = input.notes.trim().chars().take(NOTES_CHARS).collect();
    if new.is_empty() && !(input.notes_changed && !notes.is_empty()) {
        return Ok(None);
    }
    // Fell behind: the latest talk.
    while new.iter().map(String::len).sum::<usize>() > NEW_CHARS && new.len() > 1 {
        new.remove(0);
    }
    let language = llm::language_name(input.language);
    let first_new = input.map.nodes.iter().map(|n| n.id).max().unwrap_or(0).max(input.map.dropped_ids) + 1;
    let new_ids: Vec<u32> = (first_new..first_new + MAX_NEW as u32).collect();
    let existing: Vec<u32> = input.map.nodes.iter().map(|n| n.id).collect();
    let open: Vec<u32> = input.map.open.iter().map(|o| o.number).collect();
    let lines: Vec<String> = input.earlier.iter().chain(&new).cloned().collect();
    let grammar = grammar(&existing, &new_ids, lines.len(), &open, language);
    let prompt = user_prompt(input, input.earlier, &new, &notes, &new_ids);
    let mut answer = worker.chat_at(&system_prompt(language), &prompt, ANSWER_TOKENS, Some(&grammar), Some(TEMPERATURE), Some("map"), |_| {})?;
    let cost = worker.last.unwrap_or_default();
    // Cut off by the token limit: the last line is unfinished.
    if cost.tokens >= ANSWER_TOKENS as u64 {
        answer.truncate(answer.trim_end().rfind('\n').unwrap_or(0));
    }
    Ok(Some(Run { ops: parse(&answer), lines, notes, answer, cost }))
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
/// listed under the node it comes from. `None` for an empty map.
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

    fn node(n: u32, name: &str, kind: Kind, line: usize) -> Op {
        Op::Node { n, name: name.into(), kind, aka: None, cite: Cite::Line(line) }
    }

    #[test]
    fn parses_the_answer() {
        let ops = parse(
            "node N4: Business Central (customer system) also called ERP [L1]\n\
             node N5: Thing (robot) [L1]\n\
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
        let g = grammar(&[1, 2], &[3, 4, 5], 6, &[2], "English");
        assert!(g.contains("id ::= (\"N1\" | \"N2\" | \"N3\" | \"N4\" | \"N5\")"), "{g}");
        assert!(g.contains("\"node \" (\"N3\" | \"N4\" | \"N5\")"));
        assert!(g.contains("\"L6\" | \"notes\""));
        assert!(g.contains("solved ::= \"solved \" (\"O2\")") && g.contains("| open | solved\n"));
        assert!(!grammar(&[], &[1, 2, 3], 2, &[], "German").contains("| solved\n"));
        assert!(grammar(&[], &[1, 2, 3], 2, &[], "German").contains("\"von Hand\""));
    }

    #[test]
    fn edits_must_be_in_the_line_they_cite() {
        let (lines, notes) = ev();
        let ev = Evidence { lines: &lines, notes: &notes };
        let mut map = SystemMap::default();
        let a = apply(
            &mut map,
            vec![
                Op::Node { n: 1, name: "Business Central".into(), kind: Kind::Customer, aka: Some("ERP".into()), cite: Cite::Line(1) },
                node(2, "LagerPro", Kind::Customer, 8),
                node(3, "SAP", Kind::Customer, 1),          // not in that line
                node(4, "CSV", Kind::Outside, 7),           // a format, not a node
                node(5, "Sales team", Kind::Person, 2),
                Op::Known { n: 1, fact: "Sales team types orders in by hand".into(), cite: Cite::Line(2) },
                Op::Known { n: 1, fact: "Runs on servers in Frankfurt".into(), cite: Cite::Line(2) }, // not said
                Op::Flow { from: 1, to: 2, what: "open deliveries, nightly".into(), how: "CSV".into(), cite: Cite::Line(7) },
                Op::Flow { from: 1, to: 5, what: "invoices".into(), how: "mail".into(), cite: Cite::Line(4) }, // ends not there
                Op::Open { n: 1, text: "Is the API switched on?".into(), cite: Cite::Line(5) },
                Op::Open { n: 2, text: "Who maintains LagerPro?".into(), cite: Cite::Line(3) }, // nothing uncertain near it
                Op::Open { n: 1, text: "How does the ERP export work?".into(), cite: Cite::Notes },
            ],
            &ev,
            30_000,
        );
        let names: Vec<&str> = map.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["Business Central", "LagerPro", "Sales team"], "{:?}", a.lines);
        assert_eq!(map.nodes[0].aka, ["ERP"]);
        assert_eq!(map.nodes[0].known, ["Sales team types orders in by hand"]);
        assert_eq!(map.edges.len(), 1, "{:?}", a.lines);
        assert_eq!((map.edges[0].from, map.edges[0].to, map.edges[0].how.as_str()), (1, 2, "CSV"));
        assert_eq!(map.open.iter().map(|o| o.number).collect::<Vec<_>>(), [1, 2]);

        // A later run: the existing node by another name, a solved point.
        let b = apply(
            &mut map,
            vec![
                Op::Node { n: 9, name: "ERP".into(), kind: Kind::Customer, aka: None, cite: Cite::Line(1) },
                Op::Solved { number: 1, fact: "nobody knows yet".into(), cite: Cite::Line(6) },
                Op::Solved { number: 1, fact: "Business Central exports a CSV file every night".into(), cite: Cite::Line(7) }, // no answer to O1
                Op::Solved { number: 1, fact: "Business Central has an API that was never used".into(), cite: Cite::Line(4) },
            ],
            &ev,
            60_000,
        );
        assert_eq!(map.nodes.len(), 3, "ERP is Business Central: {:?}", b.lines);
        assert_eq!(map.open.len(), 1, "{:?}", b.lines);
    }

    #[test]
    fn a_proper_name_takes_over_from_a_generic_one() {
        let lines = vec!["[00:10] Speaker 1: Orders go into our ERP.".to_string(), "[00:20] Speaker 1: The ERP is Business Central.".to_string()];
        let ev = Evidence { lines: &lines, notes: "" };
        let mut map = SystemMap::default();
        apply(&mut map, vec![node(1, "ERP", Kind::Customer, 1)], &ev, 0);
        apply(&mut map, vec![Op::Alias { n: 1, name: "Business Central".into(), cite: Cite::Line(2) }], &ev, 0);
        assert_eq!(map.nodes[0].name, "Business Central");
        assert_eq!(map.nodes[0].aka, ["ERP"]);
    }

    #[test]
    fn rename_and_delete_stick() {
        let lines = vec!["[00:10] Speaker 1: BC gets the orders from Shopware by mail.".to_string()];
        let ev = Evidence { lines: &lines, notes: "" };
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
        map.rename(1, "Business Central").unwrap();
        assert!(map.rename(2, "business central").is_err());
        assert_eq!(map.find("BC"), Some(1), "the old name still finds it");
        map.delete(2).unwrap();
        assert!(map.edges.is_empty());
        apply(&mut map, vec![node(3, "Shopware", Kind::Customer, 1), node(4, "Me", Kind::Ours, 1)], &ev, 0);
        assert_eq!(map.nodes.len(), 1, "a deleted node isn't added again, and Me is no box");
        let next = map.nodes.iter().map(|n| n.id).max().unwrap().max(map.dropped_ids) + 1;
        assert_eq!(next, 3, "the deleted node's number isn't handed out again");
    }

    #[test]
    fn a_generic_box_gets_its_proper_name() {
        let lines = vec![
            "[00:10] Speaker 1: The web shop sends an email for every order.".to_string(),
            "[00:20] Me: And the web shop itself, which system is that?".to_string(),
            "[00:25] Speaker 1: It's a Shopware shop, hosted by an agency.".to_string(),
            "[00:30] Speaker 1: Business Central exports to a shared folder, as a bank statement file.".to_string(),
        ];
        let ev = Evidence { lines: &lines, notes: "" };
        let mut map = SystemMap::default();
        apply(&mut map, vec![node(1, "Webshop", Kind::Customer, 1)], &ev, 0);
        apply(&mut map, vec![node(2, "Shopware shop", Kind::Customer, 3), node(3, "shared folder", Kind::Outside, 4), node(4, "bank statement file", Kind::Customer, 4)], &ev, 0);
        assert_eq!(map.nodes.len(), 1, "{:?}", map.nodes);
        assert_eq!(map.nodes[0].name, "Shopware shop");
        assert_eq!(map.nodes[0].aka, ["Webshop"]);
    }

    #[test]
    fn an_unsure_line_solves_nothing() {
        let lines = vec!["[00:10] Me: Is the API switched on?".to_string(), "[00:12] Speaker 1: I'd have to look up whether the API is switched on.".to_string()];
        let ev = Evidence { lines: &lines, notes: "" };
        let mut map = SystemMap::default();
        apply(&mut map, vec![node(1, "API", Kind::Customer, 1)], &ev, 0);
        assert!(map.nodes.is_empty(), "an API is no node");
        let lines = vec![
            "[00:05] Speaker 1: Business Central has an API.".to_string(),
            "[00:10] Me: Is the API in Business Central switched on?".to_string(),
            "[00:12] Speaker 1: I'd have to look up whether the API is switched on.".to_string(),
        ];
        let ev = Evidence { lines: &lines, notes: "" };
        apply(&mut map, vec![node(1, "Business Central", Kind::Customer, 1), Op::Open { n: 1, text: "Is the API switched on?".into(), cite: Cite::Line(2) }], &ev, 0);
        apply(&mut map, vec![Op::Solved { number: 1, fact: "The API is switched on".into(), cite: Cite::Line(3) }], &ev, 0);
        assert_eq!(map.open.len(), 1, "still open");
    }

    #[test]
    fn a_how_nobody_said_is_unknown() {
        let lines = vec!["[00:10] Speaker 1: The tax advisor imports the invoices into DATEV.".to_string()];
        let ev = Evidence { lines: &lines, notes: "" };
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
        assert_eq!(map.nodes.len(), 2);
        assert_eq!(map.edges[0].how, "unknown");
    }

    #[test]
    fn a_person_stays_a_person() {
        let lines = vec!["[00:10] Speaker 1: Tom does the invoices.".to_string()];
        let ev = Evidence { lines: &lines, notes: "" };
        let mut map = SystemMap::default();
        apply(&mut map, vec![node(1, "Tom", Kind::Person, 1)], &ev, 0);
        apply(&mut map, vec![node(2, "Tom", Kind::Customer, 1)], &ev, 0);
        assert_eq!(map.nodes[0].kind, Kind::Person);
    }

    #[test]
    fn finds_nodes_by_loose_names() {
        let lines = vec!["[00:10] Speaker 1: Microsoft Dynamics Business Central, the order portal and DHL.".to_string()];
        let ev = Evidence { lines: &lines, notes: "" };
        let mut map = SystemMap::default();
        apply(
            &mut map,
            vec![node(1, "Microsoft Dynamics Business Central", Kind::Customer, 1), node(2, "Order portal", Kind::Ours, 1), node(3, "DHL", Kind::Outside, 1)],
            &ev,
            0,
        );
        assert_eq!(map.find("business central"), Some(1));
        assert_eq!(map.find("Microsoft Dynamics BusinessCentral"), Some(1));
        assert_eq!(map.find("Me's system"), Some(2));
        assert_eq!(map.find("SAP"), None);
    }

    #[test]
    fn support_matches_word_stems() {
        assert_eq!(support("exports invoices monthly", "Once a month I export all invoices"), Some(1.0));
        assert_eq!(support("exports invoices weekly", "Once a month I export all invoices"), Some(2.0 / 3.0));
        assert_eq!(support("it is", "anything"), None);
        assert!(uncertain("I think it's CAMT, but I would have to check.") && uncertain("Welches Format hat die Datei?"));
        assert!(!uncertain("It's called LagerPro."));
    }

    #[test]
    fn exports_in_the_fixed_shape() {
        let lines = vec!["[00:10] Speaker 1: The order portal sends orders to Business Central by API; Laura decides on API access; orders are typed in by hand.".to_string(),
                         "[00:20] Me: Is the API switched on?".to_string()];
        let ev = Evidence { lines: &lines, notes: "" };
        let mut map = SystemMap::default();
        apply(
            &mut map,
            vec![
                node(1, "Laura", Kind::Person, 1),
                node(2, "Business Central", Kind::Customer, 1),
                node(3, "Order portal", Kind::Ours, 1),
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
        let lines = vec!["[00:10] Speaker 1: LagerPro is the warehouse system.".to_string()];
        let ev = Evidence { lines: &lines, notes: "" };
        let mut map = SystemMap::default();
        apply(&mut map, vec![node(1, "LagerPro", Kind::Customer, 1)], &ev, 0);
        let earlier = vec!["[00:58] Me: And then?".to_string()];
        let new = vec!["[01:01] Speaker 1: It exports a CSV.".to_string()];
        let input = Input { notes: "", language: Some("en"), new: &[], earlier: &earlier, speakers: &[], map: &map, notes_changed: false };
        let p = user_prompt(&input, &earlier, &new, "focus on the ERP export", &[2, 3, 4]);
        assert!(p.contains("N1 LagerPro (customer system)\n"), "{p}");
        assert!(p.contains("L1 [00:58] Me: And then?\nL2 [01:01] Speaker 1: It exports a CSV.\n"));
        assert!(p.contains("L1 to L1 were read before, L2 on are new") && p.contains("New node numbers: N2, N3, N4."));
        assert!(p.contains("focus on the ERP export"));
    }
}
