//! Meeting summaries with a local language model: the model catalog, the `wordfall-llm` helper
//! process (see `src/bin/wordfall-llm.rs` for why it is a separate process) and the prompts.

use serde::Serialize;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

pub struct LlmEntry {
    pub id: &'static str,
    pub display_name: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    pub badge: &'static str,
    pub description: &'static str,
}

pub const CATALOG: &[LlmEntry] = &[
    LlmEntry {
        id: "qwen3-4b-instruct-2507",
        display_name: "Qwen3 4B",
        file: "Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/Qwen3-4B-Instruct-2507-GGUF/resolve/main/Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        bytes: 2_497_281_120,
        badge: "Fast",
        description: "Good notes within a minute or two, even for long meetings. Runs well with 16 GB of memory.",
    },
    LlmEntry {
        id: "gemma-3-12b-it-qat",
        display_name: "Gemma 3 12B",
        file: "gemma-3-12b-it-qat-Q4_0.gguf",
        url: "https://huggingface.co/unsloth/gemma-3-12b-it-qat-GGUF/resolve/main/gemma-3-12b-it-qat-Q4_0.gguf",
        bytes: 6_909_282_688,
        badge: "Better writing",
        description: "More natural notes, especially in German. Wants a 12 GB graphics card, or a Mac with 24 GB.",
    },
    LlmEntry {
        id: "qwen3-30b-a3b-instruct-2507",
        display_name: "Qwen3 30B",
        file: "Qwen3-30B-A3B-Instruct-2507-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/Qwen3-30B-A3B-Instruct-2507-GGUF/resolve/main/Qwen3-30B-A3B-Instruct-2507-Q4_K_M.gguf",
        bytes: 18_556_686_752,
        badge: "Most thorough",
        description: "The most careful notes. Wants a Mac with 32 GB, or a 16 GB graphics card and 32 GB of memory.",
    },
];

pub fn entry(id: &str) -> Option<&'static LlmEntry> {
    CATALOG.iter().find(|e| e.id == id)
}

pub fn locate(dir: &Path, entry: &LlmEntry) -> Option<PathBuf> {
    let p = dir.join(entry.file);
    crate::transcribe::is_complete(&p, entry.bytes).then_some(p)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmInfo {
    pub id: String,
    pub display_name: String,
    pub size_mb: u64,
    pub downloaded: bool,
    pub badge: String,
    pub description: String,
    pub recommended: bool,
}

pub fn list(dir: &Path) -> Vec<LlmInfo> {
    CATALOG
        .iter()
        .enumerate()
        .map(|(i, e)| LlmInfo {
            id: e.id.into(),
            display_name: e.display_name.into(),
            size_mb: e.bytes / 1_000_000,
            downloaded: locate(dir, e).is_some(),
            badge: e.badge.into(),
            description: e.description.into(),
            recommended: i == 0,
        })
        .collect()
}

// ---------- helper process ----------

/// Context the helper is asked for when writing notes; it shrinks it if memory is tight.
pub const NOTES_CTX: u32 = 32_768;

fn helper_path() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let name = if cfg!(windows) { "wordfall-llm.exe" } else { "wordfall-llm" };
    let path = exe.with_file_name(name);
    anyhow::ensure!(path.exists(), "the summary helper ({name}) is missing next to {}", exe.display());
    Ok(path)
}

pub struct Worker {
    child: Child,
    stdin: ChildStdin,
    events: mpsc::Receiver<Value>,
    pub ctx: u32,
    next_id: u64,
    /// What the last chat cost.
    pub last: Option<ChatStats>,
}

/// Tokens read and written by one chat, and how long it took.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChatStats {
    pub prompt_tokens: u64,
    /// Of those, already held by the slot's kept context (not read again).
    pub kept_tokens: u64,
    pub tokens: u64,
    pub ms: u64,
}

pub enum Event<'a> {
    /// Reading the prompt: tokens done of total.
    Reading(usize, usize),
    /// A piece of the answer.
    Token(&'a str),
}

impl Worker {
    /// Starts the helper and loads `model` (takes a few seconds; the GPU warms up meanwhile).
    /// llama.cpp's log is appended to `log`. `ctx` is the context wanted, in tokens.
    pub fn spawn(model: &Path, log: &Path, ctx: u32) -> anyhow::Result<Self> {
        let stderr = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
        let mut cmd = Command::new(helper_path()?);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(stderr);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout"))?;
        let (tx, events) = mpsc::channel();
        std::thread::Builder::new().name("wordfall-llm-reader".into()).spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
            }
        })?;
        let mut w = Self { child, stdin, events, ctx: 0, next_id: 1, last: None };
        w.send(json!({ "op": "load", "model": model.display().to_string(), "ctx": ctx }))?;
        let ready = w.next_event(Duration::from_secs(300))?;
        match ready["event"].as_str() {
            Some("ready") => {
                w.ctx = ready["ctx"].as_u64().unwrap_or(8192) as u32;
                log::info!("summary model ready on {} with a {}-token context", ready["device"].as_str().unwrap_or("?"), w.ctx);
                Ok(w)
            }
            _ => anyhow::bail!("{}", ready["message"].as_str().unwrap_or("the summary model did not load")),
        }
    }

    fn send(&mut self, v: Value) -> anyhow::Result<()> {
        writeln!(self.stdin, "{v}")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn next_event(&self, timeout: Duration) -> anyhow::Result<Value> {
        self.events.recv_timeout(timeout).map_err(|e| match e {
            mpsc::RecvTimeoutError::Timeout => anyhow::anyhow!("the summary model stopped responding"),
            mpsc::RecvTimeoutError::Disconnected => anyhow::anyhow!("the summary model quit unexpectedly (see llm.log)"),
        })
    }

    pub fn count(&mut self, text: &str) -> anyhow::Result<usize> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "op": "count", "id": id, "text": text }))?;
        loop {
            let ev = self.next_event(Duration::from_secs(60))?;
            if ev["id"].as_u64() != Some(id) {
                continue;
            }
            return match ev["event"].as_str() {
                Some("count") => Ok(ev["tokens"].as_u64().unwrap_or(0) as usize),
                _ => anyhow::bail!("{}", ev["message"].as_str().unwrap_or("counting failed")),
            };
        }
    }

    /// Runs one chat completion, reporting progress through `on_event`.
    pub fn chat(&mut self, system: &str, user: &str, max_tokens: u32, on_event: impl FnMut(Event)) -> anyhow::Result<String> {
        self.chat_shaped(system, user, max_tokens, None, on_event)
    }

    /// Like `chat`, with the answer held to a GBNF `grammar` (root rule `root`).
    pub fn chat_shaped(
        &mut self,
        system: &str,
        user: &str,
        max_tokens: u32,
        grammar: Option<&str>,
        on_event: impl FnMut(Event),
    ) -> anyhow::Result<String> {
        self.chat_at(system, user, max_tokens, grammar, None, None, on_event)
    }

    /// Drops the context the helper keeps for `slot` (see `chat_at`), so its memory is free for
    /// the next request. The helper answers nothing; requests are handled in order.
    pub fn forget(&mut self, slot: &str) -> anyhow::Result<()> {
        self.send(json!({ "op": "forget", "slot": slot }))
    }

    /// Like `chat_shaped`, at a sampling `temperature` (the helper's default when `None`; 0 =
    /// greedy). With a `slot`, the helper keeps the context for the next request of that slot and
    /// reads only where its prompt differs from this one's (keep what doesn't change at the start).
    #[allow(clippy::too_many_arguments)]
    pub fn chat_at(
        &mut self,
        system: &str,
        user: &str,
        max_tokens: u32,
        grammar: Option<&str>,
        temperature: Option<f32>,
        slot: Option<&str>,
        mut on_event: impl FnMut(Event),
    ) -> anyhow::Result<String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = json!({
            "op": "chat", "id": id, "maxTokens": max_tokens,
            "messages": [{ "role": "system", "content": system }, { "role": "user", "content": user }],
        });
        if let Some(g) = grammar {
            request["grammar"] = json!(g);
        }
        if let Some(t) = temperature {
            request["temperature"] = json!(t);
        }
        if let Some(s) = slot {
            request["slot"] = json!(s);
        }
        self.send(request)?;
        loop {
            let ev = self.next_event(Duration::from_secs(600))?;
            if ev["id"].as_u64() != Some(id) {
                continue;
            }
            match ev["event"].as_str() {
                Some("progress") => {
                    on_event(Event::Reading(ev["done"].as_u64().unwrap_or(0) as usize, ev["total"].as_u64().unwrap_or(1) as usize))
                }
                Some("token") => on_event(Event::Token(ev["text"].as_str().unwrap_or(""))),
                Some("done") => {
                    self.last = Some(ChatStats {
                        prompt_tokens: ev["promptTokens"].as_u64().unwrap_or(0),
                        kept_tokens: ev["keptTokens"].as_u64().unwrap_or(0),
                        tokens: ev["tokens"].as_u64().unwrap_or(0),
                        ms: ev["ms"].as_u64().unwrap_or(0),
                    });
                    log::info!(
                        "summary step: {} prompt tokens, {} written, {} ms",
                        ev["promptTokens"], ev["tokens"], ev["ms"]
                    );
                    return Ok(ev["text"].as_str().unwrap_or("").to_string());
                }
                _ => anyhow::bail!("{}", ev["message"].as_str().unwrap_or("the summary model failed")),
            }
        }
    }
}

/// Identifies the helper build (its GPU code is compiled per build).
fn helper_stamp() -> Option<String> {
    let meta = std::fs::metadata(helper_path().ok()?).ok()?;
    let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(format!("{}-{modified}", meta.len()))
}

/// Runs the helper once on a tiny prompt, so the first real notes don't wait for its GPU code to
/// be compiled (on a Mac, Metal compiles llama.cpp's kernels on first use: about a minute and a
/// half, cached afterwards). Does nothing if this helper build has been warmed up before.
pub fn warm_up(model: &Path, data: &Path) {
    let marker = data.join("llm-warm");
    let Some(stamp) = helper_stamp() else { return };
    if std::fs::read_to_string(&marker).is_ok_and(|s| s == stamp) {
        return;
    }
    let started = std::time::Instant::now();
    let result = Worker::spawn(model, &data.join("llm.log"), NOTES_CTX).and_then(|mut w| w.chat("Answer with one word.", "Ready?", 1, |_| {}));
    match result {
        Ok(_) => {
            log::info!("summary model warmed up in {:.1}s", started.elapsed().as_secs_f32());
            if let Err(e) = std::fs::write(&marker, stamp) {
                log::warn!("could not note the warm-up: {e}");
            }
        }
        Err(e) => log::warn!("warming up the summary model failed: {e:#}"),
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Closing stdin ends the helper; make sure it is gone so its memory is freed.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------- prompts ----------

pub struct SummaryInput<'a> {
    /// A title the user gave the meeting, if any.
    pub title: Option<&'a str>,
    /// What the user wanted out of the meeting, if they said.
    pub goal: Option<&'a str>,
    pub notes: &'a str,
    /// "[mm:ss] Speaker: text" lines, with "[mm:ss] Screen: …" / "[mm:ss] Photo: …" lines of what
    /// was shown merged in (see `meetings::visuals`).
    pub transcript: &'a [String],
    /// Some transcript lines are text read from a screen or a photo.
    pub shown: bool,
    /// The terms of the meeting's vocabulary (the user's, and names from its screens) that were
    /// said, in their spelling ("PO = what it stands for" where the user said).
    pub glossary: &'a [String],
    pub speakers: &'a [String],
    pub language: Option<&'a str>,
    /// e.g. "Monday 9 November 2026, 14:00"
    pub when: &'a str,
    pub minutes: u64,
}

pub enum Progress<'a> {
    /// Reading the transcript; overall fraction 0..1.
    Reading(f32),
    /// Writing; the text so far.
    Writing(&'a str),
}

/// English name of a language code, for the prompt.
pub fn language_name(code: Option<&str>) -> &'static str {
    match code.unwrap_or("") {
        "en" => "English",
        "de" => "German",
        "fr" => "French",
        "es" => "Spanish",
        "it" => "Italian",
        "pt" => "Portuguese",
        "nl" => "Dutch",
        "pl" => "Polish",
        "cs" => "Czech",
        "sv" => "Swedish",
        "da" => "Danish",
        "no" | "nb" => "Norwegian",
        "fi" => "Finnish",
        "tr" => "Turkish",
        "ru" => "Russian",
        "uk" => "Ukrainian",
        "ja" => "Japanese",
        "zh" => "Chinese",
        "ko" => "Korean",
        _ => "the language of the transcript",
    }
}

fn system_prompt(language: &str, shown: bool) -> String {
    let mut p = format!(
        "You are a meticulous note-taker. You turn meeting transcripts into clear, well-organised meeting notes \
         in Markdown. Write in {language}. Use only what the transcript and the user's own notes say: never invent \
         names, numbers, dates, decisions or tasks. The transcript comes from speech recognition, so it has mistakes; \
         quietly correct obvious ones. \"Me\" is the person who recorded the meeting and reads these notes."
    );
    if shown {
        p.push_str(SHOWN);
    }
    p
}

/// Told when visuals are in the transcript: their text is spelled right where speech recognition
/// mishears (a document's abbreviation heard as a name is spelled on the printout).
pub const SHOWN: &str = " Lines marked \"Screen:\" or \"Photo:\" are not speech: they are text read from a shared screen or \
     a photo shown during the meeting, at that point in time. Treat them as the source for the exact names, numbers \
     and kinds of document being discussed, and use them to name topics correctly where the speech recognition \
     misheard them.";

/// Section headings in the notes' language (others are translated by the model).
fn headings(language: &str) -> [&'static str; 3] {
    match language {
        "German" => ["Entscheidungen", "Aufgaben", "Notizen"],
        "French" => ["Décisions", "Actions", "Notes"],
        "Spanish" => ["Decisiones", "Tareas", "Notas"],
        "Italian" => ["Decisioni", "Compiti", "Note"],
        "Dutch" => ["Besluiten", "Actiepunten", "Notities"],
        "Portuguese" => ["Decisões", "Tarefas", "Notas"],
        _ => ["Decisions", "Action items", "Notes"],
    }
}

/// Heading of the questions the meeting left open (added below the notes).
pub fn open_questions_heading(language: &str) -> &'static str {
    match language {
        "German" => "Offene Fragen",
        "French" => "Questions ouvertes",
        "Spanish" => "Preguntas abiertas",
        "Italian" => "Domande aperte",
        "Dutch" => "Open vragen",
        "Portuguese" => "Perguntas em aberto",
        _ => "Open questions",
    }
}

fn format_instructions(language: &str) -> String {
    let [decisions, actions, notes] = headings(language);
    format!(
        "Write the meeting notes in {language}, in exactly this structure, and nothing else:

# TITLE

Two or three sentences: what the meeting was about and what came out of it.

## {decisions}
- One line per decision that was actually agreed. Leave this section out if nothing was decided.

## {actions}
- [ ] **Who**: what, and by when only if a date was said. Only what someone said they would do: questions and advice aren't tasks. Leave this section out if there were no tasks.

## {notes}
### A heading per topic
- The key points, facts, numbers and open questions, as short bullet points."
    )
}

fn final_prompt(input: &SummaryInput, language: &str, body_label: &str, body: &str) -> String {
    let mut p = String::new();
    p.push_str(&format!("Meeting on {}, {} minutes long.
", input.when, input.minutes.max(1)));
    if !input.speakers.is_empty() {
        p.push_str(&format!("Speakers as labelled below: {}.
", input.speakers.join(", ")));
    }
    if !input.glossary.is_empty() {
        p.push_str(&format!(
            "Names and terms used in this meeting (from my vocabulary and the shared screen), each a thing of its own. \
             Write them in exactly this spelling, also where the transcript has a word that sounds like one of them and \
             clearly means it: {}.\n",
            input.glossary.join("; ")
        ));
    }
    if let Some(goal) = input.goal {
        p.push_str(&format!("What I wanted out of the meeting: {}
", goal.trim()));
    }
    p.push_str(&format!("
<{body_label}>
{body}
</{body_label}>

"));
    if !input.notes.trim().is_empty() {
        p.push_str(
            "My own notes, written during the meeting. They show what mattered to me. Every point in them must appear in your notes, in the right place, filled in with details from the transcript:
<my-notes>
",
        );
        p.push_str(input.notes.trim());
        p.push_str("
</my-notes>

");
    }
    p.push_str(&format_instructions(language));
    match input.title {
        Some(t) => p.push_str(&format!("

Use \"{t}\" as the title.")),
        None => p.push_str("

For TITLE, write a short, specific title (at most eight words)."),
    }
    p
}

fn part_prompt(part: usize, parts: usize, lines: &str) -> String {
    format!(
        "This is part {part} of {parts} of a meeting transcript.\n\n<transcript>\n{lines}\n</transcript>\n\n\
         Write detailed notes on this part only: every topic with its key points, facts and numbers, every decision, \
         every task with who takes it on and by when, and open questions. Short bullet points grouped under topic \
         headings. No introduction and no conclusion."
    )
}

/// Answer length for the final notes and for each part's notes.
const FINAL_TOKENS: u32 = 2048;
const PART_TOKENS: u32 = 1200;
/// Room for the chat template, instructions and notes beyond the transcript itself.
const OVERHEAD: usize = 1024;

/// Writes meeting notes for `input`. Transcripts too long for the model's context are read in
/// parts first (notes per part), then the part notes are combined.
pub fn summarize(worker: &mut Worker, input: &SummaryInput, mut progress: impl FnMut(Progress)) -> anyhow::Result<String> {
    let language = language_name(input.language);
    let system = system_prompt(language, input.shown);
    let transcript = input.transcript.join("\n");
    let notes_tokens = worker.count(input.notes)?;
    let budget = (worker.ctx as usize).saturating_sub(FINAL_TOKENS as usize + OVERHEAD + notes_tokens);
    let total = worker.count(&transcript)?.max(1);

    let mut n_parts = 0usize;
    let body = if total <= budget {
        transcript
    } else {
        // Split by lines into parts that fit with room for the part notes.
        let part_budget = (worker.ctx as usize).saturating_sub(PART_TOKENS as usize + OVERHEAD).max(1024) * 9 / 10;
        let chars_per_token = transcript.len() as f32 / total as f32;
        let mut parts: Vec<String> = vec![String::new()];
        for line in input.transcript {
            let current = parts.last_mut().expect("never empty");
            if !current.is_empty() && (current.len() + line.len()) as f32 / chars_per_token > part_budget as f32 {
                parts.push(String::new());
            }
            let current = parts.last_mut().expect("never empty");
            current.push_str(line);
            current.push('\n');
        }
        let n = parts.len();
        n_parts = n;
        log::info!("transcript of {total} tokens is read in {n} parts");
        let mut notes = Vec::with_capacity(n);
        for (i, part) in parts.iter().enumerate() {
            let text = worker.chat(&system, &part_prompt(i + 1, n, part.trim_end()), PART_TOKENS, |e| {
                if let Event::Reading(done, of) = e {
                    progress(Progress::Reading((i as f32 + done as f32 / of.max(1) as f32) / (n + 1) as f32));
                }
            })?;
            notes.push(format!("Part {} of {n}:\n{}", i + 1, clean(&text)));
        }
        notes.join("\n\n")
    };
    let label = if n_parts > 0 { "notes-on-each-part" } else { "transcript" };
    let prompt = final_prompt(input, language, label, &body);
    // With parts, they take all but the last share of the reading progress.
    let parts_done = n_parts as f32 / (n_parts + 1) as f32;
    let mut written = String::new();
    let text = worker.chat(&system, &prompt, FINAL_TOKENS, |e| match e {
        Event::Reading(done, of) => {
            let f = done as f32 / of.max(1) as f32;
            progress(Progress::Reading(parts_done + (1.0 - parts_done) * f))
        }
        Event::Token(t) => {
            written.push_str(t);
            progress(Progress::Writing(&written));
        }
    })?;
    let text = drop_repeated_tasks(&clean(&text));
    anyhow::ensure!(!text.trim().is_empty(), "the summary model wrote nothing");
    Ok(text)
}

/// Drops a task that says word for word what an earlier one says, whoever it's filed under
/// (small models sometimes list a task once under a speaker's label and again under their name).
fn drop_repeated_tasks(text: &str) -> String {
    let what = |line: &str| -> Option<String> {
        let item = line.trim_start().strip_prefix("- [ ]").or_else(|| line.trim_start().strip_prefix("- [x]"))?;
        // Past the owner: "**Who**: what" or "Who: what".
        let item = item.split_once(':').map(|(_, rest)| rest).unwrap_or(item);
        let words: Vec<String> = item.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
        (!words.is_empty()).then(|| words.join(" "))
    };
    let mut seen = std::collections::HashSet::new();
    text.lines()
        .filter(|l| match what(l) {
            Some(w) => seen.insert(w),
            None => true,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drops reasoning blocks and stray fences some models add.
fn clean(text: &str) -> String {
    let mut t = text.to_string();
    while let (Some(a), Some(b)) = (t.find("<think>"), t.find("</think>")) {
        if b < a {
            break;
        }
        t.replace_range(a..b + "</think>".len(), "");
    }
    let t = t.trim();
    let t = t.strip_prefix("```markdown").or_else(|| t.strip_prefix("```")).unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t);
    drop_empty_sections(t.trim())
}

/// Small models write "## Decisions / - None" instead of leaving the section out.
fn drop_empty_sections(text: &str) -> String {
    const EMPTY: &[&str] = &["none", "keine", "n/a", "-", "—", "nothing", "nichts", "aucune", "ninguna", "nessuna", "geen"];
    let is_empty_item = |l: &str| {
        let item = l.trim().trim_start_matches(['-', '*']).trim().trim_end_matches('.').to_lowercase();
        EMPTY.iter().any(|e| item == *e || item.starts_with(&format!("{e} ")))
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].starts_with("## ") {
            let mut j = i + 1;
            while j < lines.len() && !lines[j].starts_with("## ") && !lines[j].starts_with("# ") {
                j += 1;
            }
            let body: Vec<&str> = lines[i + 1..j].iter().copied().filter(|l| !l.trim().is_empty()).collect();
            if !body.is_empty() && body.iter().all(|l| is_empty_item(l)) {
                i = j;
                continue;
            }
        }
        out.push(lines[i]);
        i += 1;
    }
    out.join("
").trim().to_string()
}

/// The "# Title" line of generated notes.
pub fn title_of(summary: &str) -> Option<String> {
    summary
        .lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(|t| t.trim().trim_matches(['*', '"']).trim().to_string())
        .filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_reasoning_and_fences() {
        assert_eq!(clean("<think>hmm</think>\n```markdown\n# A\n```"), "# A");
    }

    #[test]
    fn drops_sections_that_say_none() {
        let text = "# T

Summary.

## Decisions
- None

## Notes
- A point";
        assert_eq!(clean(text), "# T

Summary.

## Notes
- A point");
        assert_eq!(clean("## Entscheidungen
- Keine.
## Aufgaben
- [ ] **Me**: x"), "## Aufgaben
- [ ] **Me**: x");
    }

    #[test]
    fn drops_a_task_repeated_under_another_name() {
        let text = "## Action items\n- [ ] **Speaker 1**: Prepare the release candidate by Wednesday.\n\
                    - [ ] **Me**: Write the help articles.\n- [ ] **Samantha**: prepare the release candidate by Wednesday\n\
                    - [ ] **Speaker 1**: Write the release notes.";
        assert_eq!(
            drop_repeated_tasks(text),
            "## Action items\n- [ ] **Speaker 1**: Prepare the release candidate by Wednesday.\n\
             - [ ] **Me**: Write the help articles.\n- [ ] **Speaker 1**: Write the release notes."
        );
    }

    #[test]
    fn the_system_prompt_explains_screen_and_photo_lines_only_when_there_are_some() {
        assert!(!system_prompt("German", false).contains("Screen:"));
        let p = system_prompt("German", true);
        assert!(p.contains("\"Screen:\" or \"Photo:\" are not speech") && p.contains("exact names, numbers"));
    }

    #[test]
    fn finds_title() {
        assert_eq!(title_of("# **Billing launch**\n\nText").as_deref(), Some("Billing launch"));
        assert_eq!(title_of("No title"), None);
    }
}
