//! Meeting notes: record a meeting (microphone and the other side of the call), transcribe it
//! while it runs, tell speakers apart, and write notes with a local language model at the end.
//!
//! Each meeting lives in `<data>/meetings/<id>/`: `meeting.json` plus `mic.wav` and `system.wav`
//! (16 kHz, deleted after `meetingAudioDays`). While a meeting is recorded or processed, the copy
//! in memory is the truth and every edit goes through it; otherwise edits load and save the file.

mod pipeline;
mod questions;
mod system_map;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    audio::{Recording, StreamResampler, WavWriter, TARGET_RATE},
    llm,
    session::notice,
    settings::Settings,
    speakers::{self, Embedder},
    storage::{read_json, write_json},
    store::new_id,
    vocab, Core,
};
pub use pipeline::ME;
pub use questions::{Act as QuestionAct, Question};
pub use system_map::SystemMap;
use pipeline::{ChannelProcessor, Ctx, EchoFilter, LiveSpeakers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Mic,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub channel: Channel,
    pub speaker: u32,
    pub text: String,
    /// Language it was transcribed in (ISO 639-1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Speaker {
    pub id: u32,
    pub name: String,
    /// The user named this speaker (the name survives re-labelling).
    #[serde(default)]
    pub renamed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Recording,
    /// Transcribing the last bit and telling speakers apart.
    Processing,
    Summarizing,
    #[default]
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Meeting {
    pub id: String,
    /// Empty until the user or the summary names it.
    pub title: String,
    pub title_from_user: bool,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub duration_ms: u64,
    pub status: Status,
    /// The user's own notes, typed during or after the meeting.
    pub notes: String,
    /// What the user wants out of the meeting; live questions steer towards it.
    pub goal: String,
    /// Questions proposed while it was recorded (see `questions.rs`).
    pub questions: Vec<Question>,
    /// Systems, people and how they connect, built while it was recorded (see `system_map.rs`).
    pub system_map: SystemMap,
    pub segments: Vec<Segment>,
    pub speakers: Vec<Speaker>,
    /// Markdown notes written by the language model.
    pub summary: Option<String>,
    pub summary_model: Option<String>,
    pub language: Option<String>,
    /// Why the summary is missing (or what went wrong while recording).
    pub error: Option<String>,
    pub system_audio: bool,
    /// Stopped by the time limit rather than by the user.
    pub stopped_by_limit: bool,
    /// Recordings are still on disk (filled in when read).
    #[serde(skip_deserializing)]
    pub has_audio: bool,
}

/// What the meeting list shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingItem {
    pub id: String,
    pub title: String,
    pub started_at: String,
    pub duration_ms: u64,
    pub status: Status,
    pub has_summary: bool,
    /// First sentence of the summary, or the first words said.
    pub preview: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveMeeting {
    pub id: String,
    pub started_at: String,
    /// When the time limit will stop it (RFC 3339), if there is one.
    pub stops_at: Option<String>,
}

struct Live {
    stop: Arc<AtomicBool>,
    info: ActiveMeeting,
}

#[derive(Default)]
pub struct Meetings {
    /// Meetings being recorded or processed; edits go to these copies.
    open: Mutex<HashMap<String, Arc<Mutex<Meeting>>>>,
    live: Mutex<Option<Live>>,
    /// One summary at a time: the model needs several GB.
    summarizing: Mutex<()>,
    /// What the live model (questions and system map) of the meeting being recorded is doing.
    question_status: Mutex<Option<(String, QuestionStatus)>>,
}

/// What the live model is doing (it writes both the questions and the system map), for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuestionStatus {
    /// Turned off in Settings, or the meeting isn't being recorded.
    Off,
    /// No summary model is downloaded.
    NoModel,
    /// Loading the model.
    Starting,
    /// Waiting for enough new talk.
    Listening,
    /// Reading the latest talk.
    Thinking,
    /// The model failed; questions and map stopped for this meeting.
    Failed,
}

impl Meetings {
    pub fn active(&self) -> Option<ActiveMeeting> {
        self.live.lock().as_ref().map(|l| l.info.clone())
    }
}

// ---------- storage ----------

pub fn root(data: &Path) -> PathBuf {
    data.join("meetings")
}

fn dir_of(data: &Path, id: &str) -> PathBuf {
    root(data).join(id)
}

fn load(data: &Path, id: &str) -> Option<Meeting> {
    let mut m: Meeting = read_json(&dir_of(data, id).join("meeting.json"))?;
    m.has_audio = has_audio(data, id);
    Some(m)
}

fn has_audio(data: &Path, id: &str) -> bool {
    let d = dir_of(data, id);
    d.join("mic.wav").exists() || d.join("system.wav").exists()
}

fn save(data: &Path, m: &Meeting) {
    if let Err(e) = write_json(&dir_of(data, &m.id).join("meeting.json"), m) {
        log::error!("saving meeting {} failed: {e:#}", m.id);
    }
}

fn all(data: &Path) -> Vec<Meeting> {
    let Ok(entries) = std::fs::read_dir(root(data)) else { return Vec::new() };
    let mut out: Vec<Meeting> =
        entries.flatten().filter_map(|e| load(data, &e.file_name().to_string_lossy())).collect();
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    out
}

fn preview(m: &Meeting) -> String {
    let from_summary = m.summary.as_deref().and_then(|s| {
        s.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-')).map(str::to_string)
    });
    let text = from_summary.unwrap_or_else(|| m.segments.iter().take(6).map(|s| s.text.as_str()).collect::<Vec<_>>().join(" "));
    let mut cut: String = text.chars().take(160).collect();
    if cut.len() < text.len() {
        cut.push('…');
    }
    cut
}

fn current(app: &AppHandle, id: &str) -> Option<Meeting> {
    let c = app.state::<Core>();
    let open = c.meetings.open.lock().get(id).cloned();
    match open {
        Some(m) => {
            let mut m = m.lock().clone();
            m.has_audio = has_audio(&c.dir, id);
            Some(m)
        }
        None => load(&c.dir, id),
    }
}

pub fn list(app: &AppHandle) -> Vec<MeetingItem> {
    let c = app.state::<Core>();
    let mut meetings = all(&c.dir);
    // Open meetings may be ahead of their files.
    for m in meetings.iter_mut() {
        if let Some(live) = c.meetings.open.lock().get(&m.id) {
            *m = live.lock().clone();
        }
    }
    meetings
        .iter()
        .map(|m| MeetingItem {
            id: m.id.clone(),
            title: m.title.clone(),
            started_at: m.started_at.clone(),
            duration_ms: m.duration_ms,
            status: m.status,
            has_summary: m.summary.is_some(),
            preview: preview(m),
        })
        .collect()
}

pub fn get(app: &AppHandle, id: &str) -> anyhow::Result<Meeting> {
    current(app, id).ok_or_else(|| anyhow::anyhow!("That meeting no longer exists."))
}

/// Applies `f` to a meeting (the open copy if there is one) and saves it.
pub fn edit(app: &AppHandle, id: &str, f: impl FnOnce(&mut Meeting)) -> anyhow::Result<Meeting> {
    let c = app.state::<Core>();
    let open = c.meetings.open.lock().get(id).cloned();
    let m = match open {
        Some(m) => {
            let mut g = m.lock();
            f(&mut g);
            save(&c.dir, &g);
            g.clone()
        }
        None => {
            let mut m = load(&c.dir, id).ok_or_else(|| anyhow::anyhow!("That meeting no longer exists."))?;
            f(&mut m);
            save(&c.dir, &m);
            m
        }
    };
    let _ = app.emit("meetings-changed", ());
    Ok(m)
}

pub fn delete(app: &AppHandle, id: &str) -> anyhow::Result<()> {
    let c = app.state::<Core>();
    anyhow::ensure!(!c.meetings.open.lock().contains_key(id), "Stop the meeting before deleting it.");
    std::fs::remove_dir_all(dir_of(&c.dir, id))?;
    let _ = app.emit("meetings-changed", ());
    Ok(())
}

/// Deletes recordings older than `days` (0 = keep them).
pub fn prune_audio(data: &Path, days: u32) {
    if days == 0 {
        return;
    }
    let cutoff = chrono::Utc::now() - chrono::Duration::days(days as i64);
    for m in all(data) {
        let Ok(started) = chrono::DateTime::parse_from_rfc3339(&m.started_at) else { continue };
        if started < cutoff && m.status == Status::Done {
            for f in ["mic.wav", "system.wav"] {
                let p = dir_of(data, &m.id).join(f);
                if p.exists() {
                    match std::fs::remove_file(&p) {
                        Ok(()) => log::info!("deleted {} of meeting {} (older than {days} days)", f, m.id),
                        Err(e) => log::warn!("could not delete {}: {e}", p.display()),
                    }
                }
            }
        }
    }
}

/// After a crash or forced quit, meetings may still say "recording". Their transcript up to the
/// last save is kept; the notes can be written from it.
pub fn recover(data: &Path) {
    for mut m in all(data) {
        if m.status != Status::Done {
            log::warn!("meeting {} was left {:?}; marking it done", m.id, m.status);
            if m.status == Status::Recording {
                m.error = Some("Wordfall quit while recording this meeting. The transcript up to that point was kept.".into());
            }
            m.status = Status::Done;
            if m.ended_at.is_none() {
                m.ended_at = Some(chrono::Utc::now().to_rfc3339());
            }
            save(data, &m);
        }
    }
}

// ---------- recording ----------

/// How often the live transcript catches up.
const PROCESS_EVERY: Duration = Duration::from_secs(8);
/// Process sooner when this much audio has piled up on a channel.
const PROCESS_AT: usize = 25 * 16_000;

fn emit_state(app: &AppHandle) {
    let active = app.state::<Core>().meetings.active();
    let _ = app.emit("meeting-state", json!({ "active": active }));
    crate::refresh_tray(app);
}

fn progress(app: &AppHandle, id: &str, phase: &str, fraction: Option<f32>, text: Option<&str>) {
    let _ = app.emit("meeting-progress", json!({ "id": id, "phase": phase, "fraction": fraction, "text": text }));
}

pub fn is_recording(app: &AppHandle) -> bool {
    app.state::<Core>().meetings.live.lock().is_some()
}

pub fn toggle(app: &AppHandle) {
    if is_recording(app) {
        stop(app);
    } else if let Err(e) = start(app) {
        notice(app, "error", "The meeting didn't start", &format!("{e:#}"));
    }
}

/// Starts recording a meeting. Returns its id.
pub fn start(app: &AppHandle) -> anyhow::Result<String> {
    let c = app.state::<Core>();
    let mut live = c.meetings.live.lock();
    anyhow::ensure!(live.is_none(), "A meeting is already being recorded.");
    let settings = c.settings.read().clone();
    if crate::platform::permissions().microphone != "granted" {
        crate::platform::request_microphone();
        anyhow::bail!("Wordfall needs microphone access to record meetings.");
    }
    crate::ensure_model_loaded(app);
    let mic = Recording::start(settings.input_device.as_deref(), settings.input_gain)
        .map_err(|e| anyhow::anyhow!("The microphone didn't start: {e}"))?;
    let system = if settings.meeting_system_audio {
        match Recording::start_system() {
            Ok(r) => Some(r),
            Err(e) => {
                log::warn!("system audio unavailable: {e:#}");
                notice(app, "warning", "Recording your microphone only", &format!("Other participants can't be recorded directly: {e}."));
                None
            }
        }
    } else {
        None
    };
    log::info!("meeting started: mic {}, system audio {}", mic.device_name, system.as_ref().map(|s| s.device_name.as_str()).unwrap_or("off"));

    let now = chrono::Utc::now();
    let meeting = Meeting {
        id: new_id(),
        started_at: now.to_rfc3339(),
        status: Status::Recording,
        system_audio: system.is_some(),
        speakers: vec![Speaker { id: ME, name: "Me".into(), renamed: false }],
        ..Default::default()
    };
    let id = meeting.id.clone();
    save(&c.dir, &meeting);
    let shared = Arc::new(Mutex::new(meeting));
    c.meetings.open.lock().insert(id.clone(), shared.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let limit = (settings.meeting_max_minutes > 0).then(|| Duration::from_secs(settings.meeting_max_minutes as u64 * 60));
    let info = ActiveMeeting {
        id: id.clone(),
        started_at: now.to_rfc3339(),
        stops_at: limit.map(|l| (now + chrono::Duration::from_std(l).unwrap_or_default()).to_rfc3339()),
    };
    *live = Some(Live { stop: stop.clone(), info });
    drop(live);

    // The speaker model is small; fetch it now if needed so it's there by the end at the latest.
    if settings.meeting_speakers {
        let dir = c.model_dirs.read().own.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = speakers::ensure_model(&dir).await {
                log::warn!("speaker model unavailable: {e:#}");
            }
        });
    }
    // Set once the last seconds are transcribed after a stop.
    let talk_done = Arc::new(AtomicBool::new(false));
    let assistant = if settings.live_questions() || settings.live_map() {
        let (app3, shared3, stop3, settings3, done3) = (app.clone(), shared.clone(), stop.clone(), settings.clone(), talk_done.clone());
        let handle = thread::Builder::new().name("wordfall-questions".into()).spawn(move || {
            assist(&app3, &shared3, &stop3, &done3, &settings3);
        })?;
        if settings.live_questions() && settings.meeting_questions_window {
            crate::questions_window::open(app);
        }
        Some(handle)
    } else {
        set_question_status(app, &id, QuestionStatus::Off);
        None
    };
    let app2 = app.clone();
    thread::Builder::new().name("wordfall-meeting".into()).spawn(move || {
        let busy = crate::platform::busy("Meeting");
        record(&app2, &shared, mic, system, &stop, &talk_done, limit, &settings);
        drop(busy);
        crate::questions_window::close(&app2);
        // Its last run lands in the notes, and its model is gone before the notes model loads.
        if let Some(h) = assistant {
            let _ = h.join();
        }
        finish(&app2, &shared, &settings);
    })?;
    emit_state(app);
    let _ = app.emit("meetings-changed", ());
    Ok(id)
}

pub fn stop(app: &AppHandle) {
    if let Some(l) = app.state::<Core>().meetings.live.lock().as_ref() {
        l.stop.store(true, Ordering::SeqCst);
    }
}

// ---------- live questions and system map ----------

/// A run of the live model waits for this much new talk (or a change to the notes)…
const QUESTION_WORDS: usize = 30;
/// …and this much meeting time since the last run.
const QUESTION_GAP_MS: u64 = 30_000;
/// Runs that may fail in a row before questions stop for the meeting.
const QUESTION_FAILURES: u32 = 3;

fn word_count(segments: &[Segment]) -> usize {
    segments.iter().map(|s| s.text.split_whitespace().count()).sum()
}

fn set_question_status(app: &AppHandle, id: &str, status: QuestionStatus) {
    *app.state::<Core>().meetings.question_status.lock() = Some((id.to_string(), status));
    emit_questions(app, id);
    emit_map(app, id);
}

/// The live questions of a meeting, what the model is doing, and the goal they steer towards.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveQuestions {
    pub questions: Vec<Question>,
    pub status: QuestionStatus,
    pub goal: String,
}

fn live_status(app: &AppHandle, m: &Meeting) -> QuestionStatus {
    match app.state::<Core>().meetings.question_status.lock().as_ref() {
        Some((sid, s)) if *sid == m.id && m.status == Status::Recording => *s,
        _ => QuestionStatus::Off,
    }
}

pub fn questions_of(app: &AppHandle, id: &str) -> anyhow::Result<LiveQuestions> {
    let m = get(app, id)?;
    let status = live_status(app, &m);
    Ok(LiveQuestions { questions: m.questions, status, goal: m.goal })
}

/// The system map of a meeting and what the live model is doing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveMap {
    pub map: SystemMap,
    pub status: QuestionStatus,
}

pub fn map_of(app: &AppHandle, id: &str) -> anyhow::Result<LiveMap> {
    let m = get(app, id)?;
    let status = live_status(app, &m);
    Ok(LiveMap { map: m.system_map, status })
}

/// Tells every window the system map (or the live model's status) changed: `meeting-map`.
pub fn emit_map(app: &AppHandle, id: &str) {
    if let Ok(map) = map_of(app, id) {
        let mut payload = json!(map);
        payload["id"] = json!(id);
        let _ = app.emit("meeting-map", payload);
    }
}

/// The user renames a box on the map (`name`) or deletes a wrong one.
pub fn edit_map_node(app: &AppHandle, id: &str, node: u32, name: Option<&str>) -> anyhow::Result<()> {
    let mut result = Ok(());
    edit(app, id, |m| {
        result = match name {
            Some(name) => m.system_map.rename(node, name),
            None => m.system_map.delete(node),
        }
    })?;
    result?;
    emit_map(app, id);
    Ok(())
}

/// Tells every window the questions (or the goal) changed: `meeting-questions`.
pub fn emit_questions(app: &AppHandle, id: &str) {
    if let Ok(q) = questions_of(app, id) {
        let mut payload = json!(q);
        payload["id"] = json!(id);
        let _ = app.emit("meeting-questions", payload);
    }
}

/// The user acted on a question (Later, Asked, Delete).
pub fn act_on_question(app: &AppHandle, id: &str, number: u32, act: QuestionAct) -> anyhow::Result<()> {
    let mut result = Ok(());
    edit(app, id, |m| {
        let now = m.duration_ms;
        result = questions::act(&mut m.questions, number, act, now);
    })?;
    result?;
    emit_questions(app, id);
    Ok(())
}

/// Where the system map's last run stopped reading.
#[derive(Default)]
struct MapCursor {
    /// Segments read so far.
    seen: usize,
    /// The notes as that run read them.
    notes: String,
}

/// The map's input for the talk since `cursor`. `earlier` holds the last lines before it.
fn map_input<'a>(m: &'a Meeting, cursor: &MapCursor, language: Option<&'a str>, earlier: &'a mut Vec<String>) -> system_map::Input<'a> {
    let seen = cursor.seen.min(m.segments.len());
    let mut before = pipeline::transcript_lines(&m.segments[..seen], &m.speakers);
    *earlier = before.split_off(before.len().saturating_sub(system_map::EARLIER_LINES));
    system_map::Input {
        notes: &m.notes,
        language,
        new: &m.segments[seen..],
        earlier,
        speakers: &m.speakers,
        map: &m.system_map,
        notes_changed: m.notes.trim() != cursor.notes.trim(),
    }
}

/// One map run on the meeting being recorded: reads the talk since `cursor`, applies what checks
/// out, tells the windows.
fn live_map_step(app: &AppHandle, w: &mut llm::Worker, meeting: &Arc<Mutex<Meeting>>, cursor: &mut MapCursor) -> anyhow::Result<()> {
    let c = app.state::<Core>();
    let snapshot = meeting.lock().clone();
    let language = pipeline::meeting_language(&snapshot.segments);
    let mut earlier = Vec::new();
    let input = map_input(&snapshot, cursor, language.as_deref(), &mut earlier);
    let started = Instant::now();
    let run = system_map::run(w, &input)?;
    *cursor = MapCursor { seen: snapshot.segments.len(), notes: snapshot.notes.clone() };
    let Some(run) = run else { return Ok(()) };
    let applied = {
        let mut m = meeting.lock();
        let now = m.duration_ms;
        let applied = system_map::apply(&mut m.system_map, run.ops.clone(), &run.evidence(), now);
        save(&c.dir, &m);
        applied
    };
    emit_map(app, &snapshot.id);
    log::info!(
        "system map: read the new talk in {:.1}s ({}), {}",
        started.elapsed().as_secs_f32(),
        stats_line(Some(run.cost)),
        if applied.lines.is_empty() { "no change".to_string() } else { applied.lines.join("; ") }
    );
    Ok(())
}

/// While the meeting records: whenever enough new talk has come in (or the notes changed), the
/// live model updates the system map with it, then proposes questions, which go after what the
/// map still doesn't know. One model process serves both.
///
/// When the meeting stops, the map reads the last of the talk once more (after `talk_done`: the
/// last seconds are transcribed), so nothing said at the end is left out.
fn assist(app: &AppHandle, meeting: &Arc<Mutex<Meeting>>, stop: &AtomicBool, talk_done: &AtomicBool, settings: &Settings) {
    let c = app.state::<Core>();
    let id = meeting.lock().id.clone();
    let model = llm::entry(&settings.summary_model).and_then(|e| llm::locate(&c.model_dirs.read().own, e));
    let Some(model) = model else {
        set_question_status(app, &id, QuestionStatus::NoModel);
        return;
    };
    set_question_status(app, &id, QuestionStatus::Listening);
    let mut worker: Option<llm::Worker> = None;
    let mut words_then = 0;
    let mut last_run: Option<u64> = None;
    let mut cursor = MapCursor::default();
    let mut failures = 0;
    while !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_secs(1));
        let (now_ms, words, expired, notes_changed) = {
            let mut m = meeting.lock();
            let now = m.duration_ms;
            let expired = questions::expire(&mut m.questions, now);
            (now, word_count(&m.segments), expired, settings.live_map() && m.notes.trim() != cursor.notes.trim())
        };
        if expired {
            emit_questions(app, &id);
        }
        let due = (words.saturating_sub(words_then) >= QUESTION_WORDS || (notes_changed && words > 0))
            && last_run.is_none_or(|t| now_ms.saturating_sub(t) >= QUESTION_GAP_MS);
        if !due || c.sessions.dictation_busy() || stop.load(Ordering::SeqCst) {
            continue;
        }
        if worker.is_none() {
            set_question_status(app, &id, QuestionStatus::Starting);
            match llm::Worker::spawn(&model, &c.dir.join("llm.log"), questions::CTX) {
                Ok(w) => worker = Some(w),
                Err(e) => {
                    log::error!("the questions model did not start: {e:#}");
                    set_question_status(app, &id, QuestionStatus::Failed);
                    return;
                }
            }
        }
        set_question_status(app, &id, QuestionStatus::Thinking);
        let w = worker.as_mut().expect("started above");
        words_then = words;
        last_run = Some(now_ms);
        let result = (|| -> anyhow::Result<()> {
            if settings.live_map() {
                live_map_step(app, w, meeting, &mut cursor)?;
            }
            if settings.live_questions() {
                let snapshot = meeting.lock().clone();
                let language = pipeline::meeting_language(&snapshot.segments);
                let unknowns = snapshot.system_map.unknowns();
                let input = questions::Input {
                    goal: &snapshot.goal,
                    notes: &snapshot.notes,
                    language: language.as_deref(),
                    segments: &snapshot.segments,
                    speakers: &snapshot.speakers,
                    questions: &snapshot.questions,
                    unknowns: &unknowns,
                    now_ms,
                };
                let started = Instant::now();
                let outcome = questions::run(w, &input)?;
                let changed = outcome.is_some_and(|o| {
                    let mut m = meeting.lock();
                    let now = m.duration_ms;
                    let changed = questions::apply(&mut m.questions, o, now);
                    if changed {
                        save(&c.dir, &m);
                    }
                    changed
                });
                let live = questions::live_count(&meeting.lock().questions);
                log::info!(
                    "meeting questions: read the talk in {:.1}s ({}), {live} open{}",
                    started.elapsed().as_secs_f32(),
                    stats_line(w.last),
                    if changed { ", changed" } else { "" }
                );
            }
            Ok(())
        })();
        match result {
            Ok(()) => failures = 0,
            Err(e) => {
                failures += 1;
                log::warn!("the live model failed ({failures} in a row): {e:#}");
                worker = None;
                if failures >= QUESTION_FAILURES {
                    set_question_status(app, &id, QuestionStatus::Failed);
                    return;
                }
            }
        }
        set_question_status(app, &id, QuestionStatus::Listening);
    }
    if let (true, Some(w)) = (settings.live_map(), worker.as_mut()) {
        let waited = Instant::now();
        while !talk_done.load(Ordering::SeqCst) && waited.elapsed() < Duration::from_secs(120) {
            thread::sleep(Duration::from_millis(200));
        }
        if let Err(e) = live_map_step(app, w, meeting, &mut cursor) {
            log::warn!("the system map's last run failed: {e:#}");
        }
    }
    drop(worker);
    set_question_status(app, &id, QuestionStatus::Off);
}

fn stats_line(s: Option<llm::ChatStats>) -> String {
    s.map(|s| format!("{} prompt tokens ({} kept), {} written, {} ms", s.prompt_tokens, s.kept_tokens, s.tokens, s.ms)).unwrap_or_default()
}

struct Chan {
    channel: Channel,
    rec: Recording,
    resampler: StreamResampler,
    wav: Option<WavWriter>,
    proc: ChannelProcessor,
    /// 16 kHz samples so far (including filled gaps).
    received: u64,
    /// The start of this channel's timeline has been lined up with the microphone's.
    aligned: bool,
}

fn wait_for_recognizer(app: &AppHandle) {
    let c = app.state::<Core>();
    while c.sessions.dictation_busy() {
        thread::sleep(Duration::from_millis(50));
    }
}

fn load_embedder(app: &AppHandle, settings: &Settings) -> Option<Embedder> {
    if !settings.meeting_speakers {
        return None;
    }
    let path = speakers::model_path(&app.state::<Core>().model_dirs.read().own)?;
    match Embedder::load(&path) {
        Ok(e) => Some(e),
        Err(e) => {
            log::warn!("speaker model failed to load: {e:#}");
            None
        }
    }
}

/// Records until `stop`; sets `talk_done` once the last seconds are transcribed.
#[allow(clippy::too_many_arguments)]
fn record(
    app: &AppHandle,
    meeting: &Arc<Mutex<Meeting>>,
    mic: Recording,
    system: Option<Recording>,
    stop: &AtomicBool,
    talk_done: &AtomicBool,
    limit: Option<Duration>,
    settings: &Settings,
) {
    let c = app.state::<Core>();
    let (id, dir) = {
        let m = meeting.lock();
        (m.id.clone(), dir_of(&c.dir, &m.id))
    };
    let open_wav = |name: &str| match WavWriter::open(&dir.join(name)) {
        Ok(w) => Some(w),
        Err(e) => {
            log::warn!("can't keep the meeting recording {name}: {e:#}");
            None
        }
    };
    let mut chans: Vec<Chan> = Vec::new();
    if let Some(sys) = system {
        chans.push(Chan {
            channel: Channel::System,
            resampler: StreamResampler::new(sys.sample_rate(), TARGET_RATE),
            rec: sys,
            wav: open_wav("system.wav"),
            proc: ChannelProcessor::new(Channel::System),
            received: 0,
            aligned: false,
        });
    }
    chans.push(Chan {
        channel: Channel::Mic,
        resampler: StreamResampler::new(mic.sample_rate(), TARGET_RATE),
        rec: mic,
        wav: open_wav("mic.wav"),
        proc: ChannelProcessor::new(Channel::Mic),
        received: 0,
        aligned: true,
    });

    let mut embedder = load_embedder(app, settings);
    let mut embeddings: Vec<Option<Vec<f32>>> = Vec::new();
    let mut live = LiveSpeakers::new(settings.meeting_speakers, &meeting.lock().speakers);
    let mut echo = EchoFilter::default();
    let started = Instant::now();
    let mut last_pass = Instant::now();
    let mut last_save = Instant::now();
    let mut stopped_by_limit = false;
    let mut buf = Vec::new();
    let mut device_check = Instant::now();
    loop {
        thread::sleep(Duration::from_millis(200));
        // Windows loopback follows one device; when the output changes (headphones plugged in),
        // follow it, or the other side would go silent.
        #[cfg(windows)]
        if device_check.elapsed() > Duration::from_secs(2) {
            device_check = Instant::now();
            if let Some(ch) = chans.iter_mut().find(|c| c.channel == Channel::System) {
                let current = crate::audio::default_output_name();
                if current.as_deref().is_some_and(|n| n != ch.rec.device_name) {
                    match Recording::start_system() {
                        Ok(r) => {
                            log::info!("system audio moved to {}", r.device_name);
                            ch.resampler = StreamResampler::new(r.sample_rate(), TARGET_RATE);
                            ch.rec = r;
                        }
                        Err(e) => log::warn!("could not follow the new output device: {e:#}"),
                    }
                }
            }
        }
        #[cfg(not(windows))]
        let _ = &mut device_check;
        let elapsed = started.elapsed();
        let expected = (elapsed.as_secs_f64() * TARGET_RATE as f64) as u64;
        let mut levels = json!({});
        let mic_first = chans.iter().find(|c| c.channel == Channel::Mic).and_then(|c| c.rec.first_sample_at());
        for ch in chans.iter_mut() {
            buf.clear();
            ch.resampler.push(&ch.rec.take(), &mut buf);
            // The system tap starts after the microphone (on macOS it builds an aggregate device
            // first), so its first sample was heard later. Unless its timeline starts that much
            // later too, everything on it is early against the microphone (by about a third of a
            // second on a MacBook), and echo of it is looked for in the wrong places.
            if !ch.aligned && !buf.is_empty() {
                if let (Some(first), Some(mic)) = (ch.rec.first_sample_at(), mic_first) {
                    ch.aligned = true;
                    let late = first.saturating_duration_since(mic);
                    if ch.received == 0 && late < Duration::from_secs(5) {
                        let pad = (late.as_secs_f64() * TARGET_RATE as f64) as usize;
                        buf.splice(0..0, std::iter::repeat(0.0).take(pad));
                        log::info!("system audio started {} ms after the microphone; lined up", late.as_millis());
                    }
                }
            }
            // Windows loopback delivers nothing while nothing plays: fill with silence so both
            // channels keep the same timeline.
            let behind = expected.saturating_sub(ch.received + buf.len() as u64);
            if ch.channel == Channel::System && behind > TARGET_RATE as u64 / 2 {
                let fill = (behind - TARGET_RATE as u64 / 10) as usize;
                buf.splice(0..0, std::iter::repeat(0.0).take(fill));
            }
            ch.received += buf.len() as u64;
            if let Some(w) = ch.wav.as_mut() {
                if let Err(e) = w.append(&buf) {
                    log::warn!("writing the meeting recording failed: {e:#}");
                    ch.wav = None;
                }
            }
            ch.proc.push(&buf);
            let level = ch.rec.levels().iter().copied().fold(0.0f32, f32::max);
            levels[if ch.channel == Channel::Mic { "mic" } else { "system" }] = json!(level);
        }
        let _ = app.emit("meeting-levels", &levels);
        meeting.lock().duration_ms = elapsed.as_millis() as u64;

        if limit.is_some_and(|l| elapsed >= l) && !stop.load(Ordering::SeqCst) {
            stopped_by_limit = true;
            stop.store(true, Ordering::SeqCst);
        }
        let stopping = stop.load(Ordering::SeqCst);
        let due = last_pass.elapsed() >= PROCESS_EVERY || chans.iter().any(|ch| ch.proc.pending_samples() >= PROCESS_AT);
        if due || stopping {
            last_pass = Instant::now();
            if stopping {
                meeting.lock().status = Status::Processing;
                let _ = app.emit("meeting-changed", json!({ "id": id }));
                progress(app, &id, "transcribing", None, None);
            }
            if embedder.is_none() && settings.meeting_speakers {
                embedder = load_embedder(app, settings);
            }
            let added =
                transcribe_pass(app, meeting, &mut chans, embedder.as_ref(), &mut embeddings, &mut live, &mut echo, settings, stopping);
            if added > 0 || last_save.elapsed() > Duration::from_secs(30) {
                save(&c.dir, &meeting.lock());
                last_save = Instant::now();
            }
        }
        if stopping {
            talk_done.store(true, Ordering::SeqCst);
            break;
        }
    }
    drop(chans);
    {
        let mut m = meeting.lock();
        m.duration_ms = started.elapsed().as_millis() as u64;
        m.ended_at = Some(chrono::Utc::now().to_rfc3339());
        m.stopped_by_limit = stopped_by_limit;
    }
    *c.meetings.live.lock() = None;
    emit_state(app);
    if stopped_by_limit {
        let hours = settings.meeting_max_minutes as f32 / 60.0;
        notice(app, "info", "Meeting stopped", &format!("It reached the {hours} h limit. Change it in Settings › Meetings."));
    }

    // Final speaker labels over the whole meeting.
    progress(app, &id, "speakers", None, None);
    let embedder = embedder.or_else(|| load_embedder(app, settings));
    let mut m = meeting.lock().clone();
    if let Some(e) = embedder.as_ref() {
        fill_embeddings(&dir, &m.segments, &mut embeddings, e);
    }
    embeddings.resize(m.segments.len(), None);
    let dropped = pipeline::drop_echo(&mut m.segments, &mut embeddings, &echo);
    if !dropped.is_empty() {
        let by_voice = dropped.iter().filter(|(_, why)| matches!(why, pipeline::Echo::Voice(..))).count();
        log::info!("dropped {} microphone segments that echoed the other side ({by_voice} by voice)", dropped.len());
    }
    pipeline::assign_speakers(&mut m.segments, &embeddings, &mut m.speakers, settings.meeting_speakers && embedder.is_some());
    m.language = pipeline::meeting_language(&m.segments);
    {
        // Only what this pass changed: the live model may have updated the map or questions meanwhile.
        let mut g = meeting.lock();
        g.segments = m.segments;
        g.speakers = m.speakers;
        g.language = m.language;
        save(&c.dir, &g);
    }
    let _ = app.emit("meeting-changed", json!({ "id": id }));
}

/// Computes embeddings for segments that don't have one yet (e.g. the model arrived late) from
/// the saved recordings.
fn fill_embeddings(dir: &Path, segments: &[Segment], embeddings: &mut Vec<Option<Vec<f32>>>, embedder: &Embedder) {
    embeddings.resize(segments.len(), None);
    for (s, e) in segments.iter().zip(embeddings.iter_mut()) {
        if e.is_some() {
            continue;
        }
        let file = dir.join(if s.channel == Channel::Mic { "mic.wav" } else { "system.wav" });
        let start = (s.start_ms * 16) as usize;
        let len = (s.end_ms.saturating_sub(s.start_ms) * 16) as usize;
        if let Ok(audio) = crate::audio::read_wav_16k_range(&file, start, len) {
            *e = embedder.embed(&audio).ok().flatten();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn transcribe_pass(
    app: &AppHandle,
    meeting: &Arc<Mutex<Meeting>>,
    chans: &mut [Chan],
    embedder: Option<&Embedder>,
    embeddings: &mut Vec<Option<Vec<f32>>>,
    live: &mut LiveSpeakers,
    echo: &mut EchoFilter,
    settings: &Settings,
    flush: bool,
) -> usize {
    let c = app.state::<Core>();
    if crate::session::wait_for_model(app).is_err() {
        // Keep the audio; the next pass tries again (the recording on disk is complete anyway).
        return 0;
    }
    let (dictionary, prompt) = {
        let store = c.store.lock();
        let dict_terms: Vec<String> = store.dictionary.iter().map(|e| e.replacement.clone()).collect();
        let terms = vocab::terms(
            settings.prompt_mode,
            &vocab::Sources { dictionary: &dict_terms, user_terms: &store.vocabulary, projects: &c.projects },
        );
        (settings.dictionary_enabled.then(|| store.dictionary.clone()), vocab::prompt(&terms))
    };
    let yield_fn = || wait_for_recognizer(app);
    let ctx = Ctx {
        engine: &c.engine,
        embedder,
        languages: &settings.languages,
        prompt: prompt.as_deref(),
        decode: &settings.decode,
        dictionary: dictionary.as_deref(),
        remove_fillers: settings.remove_fillers,
        yield_to_dictation: &yield_fn,
    };
    let mut new_segments = Vec::new();
    // System audio first, so microphone echo of it can be recognised.
    let mut system_speech = Vec::new();
    let mut system_language = None;
    for ch in chans.iter_mut() {
        if ch.channel == Channel::Mic {
            ch.proc.adopt_language(system_language.as_deref());
        }
        let heard = match ch.proc.process(&ctx, flush, &system_speech) {
            Ok(h) => h,
            Err(e) => {
                log::error!("meeting transcription failed: {e:#}");
                continue;
            }
        };
        if ch.channel == Channel::System {
            system_speech = ch.proc.speech.clone();
            system_language = ch.proc.language.clone();
        }
        let mut m = meeting.lock();
        for h in heard {
            let seg = Segment {
                start_ms: h.start_ms,
                end_ms: h.end_ms,
                channel: ch.channel,
                speaker: ME,
                text: h.text,
                language: h.language,
            };
            if seg.channel == Channel::Mic && echo.check(&seg, h.embedding.as_deref(), &m.segments).is_some() {
                continue;
            }
            let Meeting { speakers, .. } = &mut *m;
            let speaker = live.assign(seg.channel, h.embedding.as_deref(), speakers);
            let seg = Segment { speaker, ..seg };
            m.segments.push(seg.clone());
            embeddings.push(h.embedding);
            new_segments.push(seg);
        }
    }
    if !new_segments.is_empty() {
        let m = meeting.lock();
        let _ = app.emit("meeting-segments", json!({ "id": m.id, "segments": new_segments, "speakers": m.speakers }));
    }
    new_segments.len()
}

/// After recording: writes the notes if a summary model is there.
fn finish(app: &AppHandle, meeting: &Arc<Mutex<Meeting>>, settings: &Settings) {
    let c = app.state::<Core>();
    let id = meeting.lock().id.clone();
    let has_speech = !meeting.lock().segments.is_empty();
    let model = llm::entry(&settings.summary_model).filter(|e| llm::locate(&c.model_dirs.read().own, e).is_some());
    if !has_speech {
        let mut m = meeting.lock();
        m.status = Status::Done;
        m.error = Some("Nothing was said, so there's nothing to summarise.".into());
    } else if model.is_none() {
        let mut m = meeting.lock();
        m.status = Status::Done;
        m.error = Some("Download a summary model in Settings › Meetings to get notes for this meeting.".into());
    } else {
        write_summary(app, meeting);
    }
    save(&c.dir, &meeting.lock());
    c.meetings.open.lock().remove(&id);
    prune_audio(&c.dir, settings.meeting_audio_days);
    let _ = app.emit("meeting-changed", json!({ "id": id }));
    let _ = app.emit("meetings-changed", ());
}

/// Gets the summary model's GPU code compiled before the first notes need it (see
/// `llm::warm_up`), unless a meeting is being recorded or summarised.
pub fn warm_up_summaries(app: &AppHandle) {
    let c = app.state::<Core>();
    if is_recording(app) {
        return;
    }
    let Some(_one_at_a_time) = c.meetings.summarizing.try_lock() else { return };
    let chosen = c.settings.read().summary_model.clone();
    let own = c.model_dirs.read().own.clone();
    // The kernels don't depend on the model; any downloaded one will do.
    let model = llm::entry(&chosen)
        .and_then(|e| llm::locate(&own, e))
        .or_else(|| llm::CATALOG.iter().find_map(|e| llm::locate(&own, e)));
    if let Some(model) = model {
        llm::warm_up(&model, &c.dir);
    }
}

/// Writes (or rewrites) the notes of a finished meeting in the background.
pub fn summarize(app: &AppHandle, id: &str) -> anyhow::Result<()> {
    let c = app.state::<Core>();
    anyhow::ensure!(!c.meetings.open.lock().contains_key(id), "This meeting is still being processed.");
    let m = load(&c.dir, id).ok_or_else(|| anyhow::anyhow!("That meeting no longer exists."))?;
    anyhow::ensure!(!m.segments.is_empty(), "Nothing was said in this meeting.");
    let settings = c.settings.read().clone();
    let entry = llm::entry(&settings.summary_model).ok_or_else(|| anyhow::anyhow!("Pick a summary model in Settings › Meetings."))?;
    anyhow::ensure!(
        llm::locate(&c.model_dirs.read().own, entry).is_some(),
        "{} isn't downloaded yet. Download it in Settings › Meetings.",
        entry.display_name
    );
    let shared = Arc::new(Mutex::new(m));
    c.meetings.open.lock().insert(id.to_string(), shared.clone());
    let app = app.clone();
    let id = id.to_string();
    thread::spawn(move || {
        write_summary(&app, &shared);
        let c = app.state::<Core>();
        save(&c.dir, &shared.lock());
        c.meetings.open.lock().remove(&id);
        let _ = app.emit("meeting-changed", json!({ "id": id }));
        let _ = app.emit("meetings-changed", ());
    });
    Ok(())
}

fn write_summary(app: &AppHandle, meeting: &Arc<Mutex<Meeting>>) {
    let c = app.state::<Core>();
    let id = meeting.lock().id.clone();
    {
        let mut m = meeting.lock();
        m.status = Status::Summarizing;
        m.error = None;
    }
    let _ = app.emit("meeting-changed", json!({ "id": id }));
    progress(app, &id, "loading", None, None);
    let _one_at_a_time = c.meetings.summarizing.lock();
    let settings = c.settings.read().clone();
    let snapshot = meeting.lock().clone();
    let result = (|| -> anyhow::Result<(String, String)> {
        let entry = llm::entry(&settings.summary_model).ok_or_else(|| anyhow::anyhow!("unknown summary model"))?;
        let path = llm::locate(&c.model_dirs.read().own, entry)
            .ok_or_else(|| anyhow::anyhow!("{} isn't downloaded", entry.display_name))?;
        let started = Instant::now();
        let mut worker = llm::Worker::spawn(&path, &c.dir.join("llm.log"), llm::NOTES_CTX)?;
        let lines = pipeline::transcript_lines(&snapshot.segments, &snapshot.speakers);
        let used: Vec<u32> = snapshot.segments.iter().map(|s| s.speaker).collect();
        let speakers: Vec<String> =
            snapshot.speakers.iter().filter(|s| used.contains(&s.id)).map(|s| s.name.clone()).collect();
        let when = chrono::DateTime::parse_from_rfc3339(&snapshot.started_at)
            .map(|t| t.with_timezone(&chrono::Local).format("%A %-d %B %Y, %H:%M").to_string())
            .unwrap_or_default();
        let input = llm::SummaryInput {
            title: snapshot.title_from_user.then_some(snapshot.title.as_str()).filter(|t| !t.trim().is_empty()),
            goal: Some(snapshot.goal.as_str()).filter(|g| !g.trim().is_empty()),
            notes: &snapshot.notes,
            transcript: &lines,
            speakers: &speakers,
            language: snapshot.language.as_deref(),
            when: &when,
            minutes: snapshot.duration_ms / 60_000,
        };
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        let summary = llm::summarize(&mut worker, &input, |p| {
            if last_emit.elapsed() < Duration::from_millis(120) {
                return;
            }
            last_emit = Instant::now();
            match p {
                llm::Progress::Reading(f) => progress(app, &id, "reading", Some(f), None),
                llm::Progress::Writing(t) => progress(app, &id, "writing", None, Some(t)),
            }
        })?;
        log::info!("meeting notes written in {:.1}s", started.elapsed().as_secs_f32());
        // What the live questions left open, as the user saw them (not retold by the model).
        let heading = llm::open_questions_heading(llm::language_name(snapshot.language.as_deref()));
        let summary = match questions::open_section(&snapshot.questions, heading) {
            Some(open) => format!("{summary}\n\n{open}"),
            None => summary,
        };
        // The system map, in the fixed shape other programs read.
        let summary = match system_map::section(&snapshot.system_map) {
            Some(map) => format!("{summary}\n\n{map}"),
            None => summary,
        };
        Ok((summary, entry.id.to_string()))
    })();
    let mut m = meeting.lock();
    m.status = Status::Done;
    match result {
        Ok((summary, model)) => {
            if !m.title_from_user {
                if let Some(t) = llm::title_of(&summary) {
                    m.title = t;
                }
            }
            m.summary = Some(summary);
            m.summary_model = Some(model);
            drop(m);
            notice(app, "success", "Meeting notes are ready", &meeting.lock().title);
        }
        Err(e) => {
            log::error!("meeting summary failed: {e:#}");
            m.error = Some(format!("The notes couldn't be written: {e:#}"));
        }
    }
}

// ---------- `wordfall --meeting` ----------

/// The live model for `wordfall --meeting … --questions / --map`: runs at the same pace as in a live
/// meeting (by meeting time), printing what changes and what each run cost. Nobody presses anything,
/// so questions stay until the talk answers them or they expire. `--note-at "90:text"` adds a line to
/// the notes at that second, as if typed then.
struct CliAssist {
    worker: llm::Worker,
    ask: bool,
    map: bool,
    words_then: usize,
    last_run: Option<u64>,
    cursor: MapCursor,
    notes_at: Vec<(u64, String)>,
    /// (meeting ms, prompt tokens, written, ms) per map run and per questions run.
    map_runs: Vec<llm::ChatStats>,
    question_runs: Vec<llm::ChatStats>,
    /// The map after each run that changed it, for looking at how it grew.
    snapshots: Vec<serde_json::Value>,
}

impl CliAssist {
    fn step(&mut self, meeting: &mut Meeting, now_ms: u64) -> anyhow::Result<()> {
        let t = now_ms as f32 / 1000.0;
        while let Some(i) = self.notes_at.iter().position(|(at, _)| *at <= now_ms) {
            let (_, note) = self.notes_at.remove(i);
            eprintln!("[{t:>6.1}s] (note typed) {note}");
            if !meeting.notes.is_empty() {
                meeting.notes.push('\n');
            }
            meeting.notes.push_str(&note);
        }
        let before = meeting.questions.clone();
        questions::expire(&mut meeting.questions, now_ms);
        let words = word_count(&meeting.segments);
        let notes_changed = self.map && meeting.notes.trim() != self.cursor.notes.trim();
        let due = (words.saturating_sub(self.words_then) >= QUESTION_WORDS || (notes_changed && words > 0))
            && self.last_run.is_none_or(|t| now_ms.saturating_sub(t) >= QUESTION_GAP_MS);
        if due {
            self.words_then = words;
            self.last_run = Some(now_ms);
            let language = pipeline::meeting_language(&meeting.segments);
            if self.map {
                self.map_step(meeting, now_ms)?;
            }
            if self.ask {
                let unknowns = meeting.system_map.unknowns();
                let input = questions::Input {
                    goal: &meeting.goal,
                    notes: &meeting.notes,
                    language: language.as_deref(),
                    segments: &meeting.segments,
                    speakers: &meeting.speakers,
                    questions: &meeting.questions,
                    unknowns: &unknowns,
                    now_ms,
                };
                let started = Instant::now();
                let outcome = questions::run(&mut self.worker, &input)?;
                if outcome.is_some() {
                    let stats = self.worker.last.unwrap_or_default();
                    self.question_runs.push(stats);
                    eprintln!("[{t:>6.1}s] (questions read the talk in {:.1}s: {})", started.elapsed().as_secs_f32(), stats_line(Some(stats)));
                }
                if let Some(o) = outcome {
                    questions::apply(&mut meeting.questions, o, now_ms);
                }
            }
        }
        for q in &meeting.questions {
            match before.iter().find(|b| b.number == q.number) {
                None => eprintln!("[{t:>6.1}s] + Q{} ({:?}) {}", q.number, q.kind, q.text),
                Some(b) if b.state != q.state => eprintln!("[{t:>6.1}s] Q{} {:?}", q.number, q.state),
                _ => {}
            }
        }
        Ok(())
    }

    /// One map run on the talk since the last one, printing what it changed.
    fn map_step(&mut self, meeting: &mut Meeting, now_ms: u64) -> anyhow::Result<()> {
        let t = now_ms as f32 / 1000.0;
        let language = pipeline::meeting_language(&meeting.segments);
        let mut earlier = Vec::new();
        let input = map_input(meeting, &self.cursor, language.as_deref(), &mut earlier);
        let started = Instant::now();
        let run = system_map::run(&mut self.worker, &input)?;
        self.cursor = MapCursor { seen: meeting.segments.len(), notes: meeting.notes.clone() };
        let Some(run) = run else { return Ok(()) };
        self.map_runs.push(run.cost);
        eprintln!("[{t:>6.1}s] (map read the new talk in {:.1}s: {})", started.elapsed().as_secs_f32(), stats_line(Some(run.cost)));
        if std::env::var_os("WORDFALL_MAP_RAW").is_some() {
            eprintln!("[{t:>6.1}s] map answer:\n{}", run.answer.trim_end());
        }
        let applied = system_map::apply(&mut meeting.system_map, run.ops.clone(), &run.evidence(), now_ms);
        for l in &applied.lines {
            eprintln!("[{t:>6.1}s] map {l}");
        }
        self.snapshots.push(json!({ "atMs": now_ms, "notes": meeting.notes, "map": meeting.system_map }));
        Ok(())
    }

    /// From `--questions`, `--map`, `--note-at "90:text"` and `--llm <id>` (in `settings`).
    fn from_args(args: &[String], settings: &Settings, data_dir: &Path) -> anyhow::Result<Option<Self>> {
        let (ask, map) = (args.iter().any(|a| a == "--questions"), args.iter().any(|a| a == "--map"));
        if !ask && !map {
            return Ok(None);
        }
        let mut notes_at = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if a == "--note-at" {
                let v = args.get(i + 1).ok_or_else(|| anyhow::anyhow!("--note-at needs \"seconds:text\""))?;
                let (at, text) = v.split_once(':').ok_or_else(|| anyhow::anyhow!("--note-at needs \"seconds:text\""))?;
                notes_at.push((at.trim().parse::<u64>()? * 1000, text.trim().to_string()));
            }
        }
        crate::init_stderr_logging();
        let entry = llm::entry(&settings.summary_model).ok_or_else(|| anyhow::anyhow!("unknown summary model"))?;
        let path = llm::locate(&settings.model_dirs(data_dir).own, entry).ok_or_else(|| anyhow::anyhow!("{} is not downloaded", entry.id))?;
        eprintln!("live model: {}", entry.display_name);
        Ok(Some(Self {
            worker: llm::Worker::spawn(&path, &data_dir.join("llm.log"), questions::CTX)?,
            ask,
            map,
            words_then: 0,
            last_run: None,
            cursor: MapCursor::default(),
            notes_at,
            map_runs: Vec::new(),
            question_runs: Vec::new(),
            snapshots: Vec::new(),
        }))
    }

    /// At the end: the map reads what was said since its last run, as in a live meeting; then the
    /// cost report, and the map's snapshots to `map_json`.
    fn done(&mut self, meeting: &mut Meeting, now_ms: u64, map_json: Option<&str>) -> anyhow::Result<()> {
        if self.map && self.cursor.seen < meeting.segments.len() {
            self.map_step(meeting, now_ms)?;
        }
        eprintln!("--- live model cost ---\n{}", self.report());
        if let Some(path) = map_json {
            std::fs::write(path, serde_json::to_string_pretty(&self.snapshots)?)?;
        }
        Ok(())
    }

    /// Totals per kind of run: runs, prompt tokens, written tokens, time (sum and mean).
    fn report(&self) -> String {
        let line = |name: &str, runs: &[llm::ChatStats]| {
            if runs.is_empty() {
                return format!("{name}: no runs");
            }
            let n = runs.len() as u64;
            let (p, w, ms) = runs.iter().fold((0, 0, 0), |(p, w, ms), s| (p + s.prompt_tokens, w + s.tokens, ms + s.ms));
            let kept: u64 = runs.iter().map(|s| s.kept_tokens).sum();
            let max_ms = runs.iter().map(|s| s.ms).max().unwrap_or(0);
            format!(
                "{name}: {n} runs, {p} prompt ({kept} kept) + {w} written tokens in all ({} + {} per run), {:.1} s per run (max {:.1} s), {:.1} s in all",
                p / n, w / n, ms as f32 / n as f32 / 1000.0, max_ms as f32 / 1000.0, ms as f32 / 1000.0
            )
        };
        format!("{}\n{}", line("map", &self.map_runs), line("questions", &self.question_runs))
    }
}

/// `wordfall --meeting <mic.wav> [--system <system.wav>] [--no-summary] [--questions] [--goal "…"]`:
/// runs a recorded meeting through the same pipeline as a live one (fed in real-time-sized pieces)
/// and prints the transcript and notes; with `--questions`, also the live questions as they come.
pub fn cli(args: &[String]) -> anyhow::Result<()> {
    use crate::{store::Store, transcribe::{catalog_entry, Engine}};
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let mic_path = arg("--meeting").ok_or_else(|| anyhow::anyhow!("missing mic WAV"))?;
    let data_dir = dirs::data_dir().ok_or_else(|| anyhow::anyhow!("no data dir"))?.join("com.wordfall.app");
    let mut settings = Settings::load(&data_dir);
    if let Some(m) = arg("--llm") {
        settings.summary_model = m;
    }
    let model_dirs = settings.model_dirs(&data_dir);
    let entry = catalog_entry(&settings.model).ok_or_else(|| anyhow::anyhow!("unknown model"))?;
    let (path, _) = model_dirs.locate(entry).ok_or_else(|| anyhow::anyhow!("{} is not downloaded", entry.id))?;
    let engine = Engine::default();
    engine.load(entry, &path)?;
    *engine.silero_path.lock() = Some(model_dirs.own.join(crate::transcribe::SILERO_FILE)).filter(|p| p.exists());
    if settings.languages.len() > 1 {
        engine.prepare_detection()?;
    }
    let embedder = speakers::model_path(&model_dirs.own).map(|p| Embedder::load(&p)).transpose()?;
    let dictionary = Store::load(&data_dir).dictionary;

    let mut sources = vec![(Channel::Mic, crate::audio::read_wav_16k(Path::new(&mic_path))?)];
    if let Some(sys) = arg("--system") {
        sources.insert(0, (Channel::System, crate::audio::read_wav_16k(Path::new(&sys))?));
    }
    let started = Instant::now();
    let no_yield = || {};
    let ctx = Ctx {
        engine: &engine,
        embedder: embedder.as_ref(),
        languages: &settings.languages,
        prompt: None,
        decode: &settings.decode,
        dictionary: Some(&dictionary),
        remove_fillers: true,
        yield_to_dictation: &no_yield,
    };
    let mut procs: Vec<(ChannelProcessor, &[f32])> =
        sources.iter().map(|(ch, audio)| (ChannelProcessor::new(*ch), audio.as_slice())).collect();
    let longest = sources.iter().map(|(_, a)| a.len()).max().unwrap_or(0);
    let step = 8 * 16_000;
    let mut meeting = Meeting { id: "cli".into(), started_at: chrono::Utc::now().to_rfc3339(), ..Default::default() };
    meeting.speakers.push(Speaker { id: ME, name: "Me".into(), renamed: false });
    let mut embeddings = Vec::new();
    let mut live = LiveSpeakers::new(embedder.is_some(), &meeting.speakers);
    let mut echo = EchoFilter::default();
    meeting.goal = arg("--goal").unwrap_or_default();
    meeting.notes = arg("--notes").unwrap_or_default();
    let mut asker = CliAssist::from_args(args, &settings, &data_dir)?;
    let mut pos = 0;
    while pos < longest {
        let end = (pos + step).min(longest);
        let flush = end == longest;
        let mut system_speech = Vec::new();
        let mut system_language = None;
        for (proc, audio) in procs.iter_mut() {
            proc.push(&audio[pos.min(audio.len())..end.min(audio.len())]);
            if proc.channel == Channel::Mic {
                proc.adopt_language(system_language.as_deref());
            }
            let heard = proc.process(&ctx, flush, &system_speech)?;
            if proc.channel == Channel::System {
                system_speech = proc.speech.clone();
                system_language = proc.language.clone();
            }
            for h in heard {
                let seg = Segment {
                    start_ms: h.start_ms,
                    end_ms: h.end_ms,
                    channel: proc.channel,
                    speaker: ME,
                    text: h.text,
                    language: h.language,
                };
                if seg.channel == Channel::Mic {
                    if let Some(why) = echo.check(&seg, h.embedding.as_deref(), &meeting.segments) {
                        eprintln!("(echo dropped, {why:?}) {}", seg.text);
                        continue;
                    }
                }
                let speaker = live.assign(seg.channel, h.embedding.as_deref(), &mut meeting.speakers);
                eprintln!("[{:>6.1}s] {:?} {} | {}", seg.start_ms as f32 / 1000.0, seg.channel, speaker, seg.text);
                meeting.segments.push(Segment { speaker, ..seg });
                embeddings.push(h.embedding);
            }
        }
        if let Some(a) = asker.as_mut() {
            a.step(&mut meeting, end as u64 / 16)?;
        }
        pos = end;
    }
    if let Some(path) = arg("--save-transcript") {
        // What the live model saw, for replaying it with `--transcript` (no speech recognition).
        std::fs::write(&path, serde_json::to_string_pretty(&json!({ "segments": meeting.segments, "speakers": meeting.speakers }))?)?;
    }
    if let Some(mut a) = asker {
        a.done(&mut meeting, longest as u64 / 16, arg("--map-json").as_deref())?;
    }
    meeting.duration_ms = longest as u64 / 16;
    let transcribed = started.elapsed();
    for (seg, why) in pipeline::drop_echo(&mut meeting.segments, &mut embeddings, &echo) {
        eprintln!("(echo dropped at the end, {why:?}) [{:>6.1}s] {}", seg.start_ms as f32 / 1000.0, seg.text);
    }
    pipeline::assign_speakers(&mut meeting.segments, &embeddings, &mut meeting.speakers, embedder.is_some());
    meeting.language = pipeline::meeting_language(&meeting.segments);
    let lines = pipeline::transcript_lines(&meeting.segments, &meeting.speakers);
    println!("--- transcript ({:.1}s for {:.0}s of audio) ---", transcribed.as_secs_f32(), longest as f32 / 16_000.0);
    for l in &lines {
        println!("{l}");
    }
    if !meeting.questions.is_empty() {
        println!("--- questions ---");
        for q in &meeting.questions {
            println!("Q{} [{}] {:?} ({:?}): {}", q.number, pipeline::clock(q.at_ms), q.state, q.kind, q.text);
        }
    }
    if !meeting.notes.is_empty() {
        println!("--- my notes ---\n{}", meeting.notes);
    }
    let mut extra: Vec<String> = Vec::new();
    let heading = llm::open_questions_heading(llm::language_name(meeting.language.as_deref()));
    extra.extend(questions::open_section(&meeting.questions, heading));
    extra.extend(system_map::section(&meeting.system_map));
    if args.iter().any(|a| a == "--no-summary") {
        if let Some(map) = system_map::section(&meeting.system_map) {
            println!("--- system map ---\n{map}");
        }
        return Ok(());
    }
    let speakers: Vec<String> = meeting.speakers.iter().map(|s| s.name.clone()).collect();
    print_notes(&data_dir, &settings, &lines, &speakers, meeting.language.as_deref(), meeting.duration_ms / 60_000, &meeting.notes, &extra, args)
}

/// `wordfall --transcript <saved.json> --map [--questions] [--note-at "90:text"] [--llm <id>] [--map-json f]`:
/// the live model on a transcript `--meeting … --save-transcript` saved, with the segments arriving
/// at the pace they were said (no speech recognition, so runs are quick and comparable). Prints
/// what changes, the cost of each run and the map's section.
pub fn cli_transcript(args: &[String]) -> anyhow::Result<()> {
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let path = arg("--transcript").ok_or_else(|| anyhow::anyhow!("missing transcript file"))?;
    let data_dir = dirs::data_dir().ok_or_else(|| anyhow::anyhow!("no data dir"))?.join("com.wordfall.app");
    let mut settings = Settings::load(&data_dir);
    if let Some(m) = arg("--llm") {
        settings.summary_model = m;
    }
    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("{path}: {e}"))?)?;
    let segments: Vec<Segment> = serde_json::from_value(saved["segments"].clone())?;
    let mut meeting = Meeting { id: "cli".into(), started_at: chrono::Utc::now().to_rfc3339(), ..Default::default() };
    meeting.speakers = serde_json::from_value(saved["speakers"].clone())?;
    meeting.goal = arg("--goal").unwrap_or_default();
    meeting.notes = arg("--notes").unwrap_or_default();
    let mut asker = CliAssist::from_args(args, &settings, &data_dir)?.ok_or_else(|| anyhow::anyhow!("add --map and/or --questions"))?;
    let end_ms = segments.iter().map(|s| s.end_ms).max().unwrap_or(0) + 8_000;
    // As in `cli`: every 8 s of meeting time, what has been transcribed by then (about 3 s after it was said).
    let (mut next, mut t) = (0, 8_000u64);
    while t <= end_ms {
        while next < segments.len() && segments[next].end_ms + 3_000 <= t {
            meeting.segments.push(segments[next].clone());
            next += 1;
        }
        asker.step(&mut meeting, t)?;
        t += 8_000;
    }
    meeting.segments.extend(segments[next..].iter().cloned());
    asker.done(&mut meeting, end_ms, arg("--map-json").as_deref())?;
    if !meeting.questions.is_empty() {
        println!("--- questions ---");
        for q in &meeting.questions {
            println!("Q{} [{}] {:?} ({:?}): {}", q.number, pipeline::clock(q.at_ms), q.state, q.kind, q.text);
        }
    }
    println!("--- system map ---\n{}", system_map::section(&meeting.system_map).unwrap_or_default());
    Ok(())
}

/// `wordfall --meeting-notes <transcript.txt> [--language de] [--minutes 30] [--notes "…"]`: writes
/// notes for a transcript as `--meeting` prints it ("[mm:ss] Speaker: text" lines), without the
/// audio. For working on the notes prompt.
pub fn cli_notes(args: &[String]) -> anyhow::Result<()> {
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let path = arg("--meeting-notes").ok_or_else(|| anyhow::anyhow!("missing transcript file"))?;
    let data_dir = dirs::data_dir().ok_or_else(|| anyhow::anyhow!("no data dir"))?.join("com.wordfall.app");
    let settings = Settings::load(&data_dir);
    let line = regex::Regex::new(r"^\[[\d:]+\] ([^:]+): ")?;
    let lines: Vec<String> = std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("{path}: {e}"))?.lines().filter(|l| line.is_match(l)).map(str::to_string).collect();
    anyhow::ensure!(!lines.is_empty(), "no transcript lines in {path}");
    let mut speakers: Vec<String> = Vec::new();
    for l in &lines {
        let name = line.captures(l).expect("matched above")[1].to_string();
        if !speakers.contains(&name) {
            speakers.push(name);
        }
    }
    let minutes = arg("--minutes").and_then(|m| m.parse().ok()).unwrap_or(1);
    let notes = arg("--notes").unwrap_or_default();
    print_notes(&data_dir, &settings, &lines, &speakers, arg("--language").as_deref(), minutes, &notes, &[], args)
}

/// Writes and prints the notes, with `extra` sections below them as the app adds them.
#[allow(clippy::too_many_arguments)]
fn print_notes(
    data_dir: &Path,
    settings: &Settings,
    lines: &[String],
    speakers: &[String],
    language: Option<&str>,
    minutes: u64,
    notes: &str,
    extra: &[String],
    args: &[String],
) -> anyhow::Result<()> {
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let entry = llm::entry(&settings.summary_model).ok_or_else(|| anyhow::anyhow!("unknown summary model"))?;
    let path = llm::locate(&settings.model_dirs(data_dir).own, entry).ok_or_else(|| anyhow::anyhow!("{} is not downloaded", entry.id))?;
    let t = Instant::now();
    let mut worker = llm::Worker::spawn(&path, &data_dir.join("llm.log"), llm::NOTES_CTX)?;
    let goal = arg("--goal");
    let input = llm::SummaryInput {
        title: None,
        goal: goal.as_deref().filter(|g| !g.trim().is_empty()),
        notes,
        transcript: lines,
        speakers,
        language,
        when: &chrono::Local::now().format("%A %-d %B %Y, %H:%M").to_string(),
        minutes,
    };
    let mut summary = llm::summarize(&mut worker, &input, |_| {})?;
    for section in extra {
        summary = format!("{summary}\n\n{section}");
    }
    println!("--- notes ({:.1}s, language {language:?}) ---\n{summary}", t.elapsed().as_secs_f32());
    Ok(())
}
