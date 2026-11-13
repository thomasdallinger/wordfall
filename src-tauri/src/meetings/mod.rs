//! Meeting notes: record a meeting (microphone and the other side of the call), transcribe it
//! while it runs, tell speakers apart, and write notes with a local language model at the end.
//!
//! Each meeting lives in `<data>/meetings/<id>/`: `meeting.json` plus `mic.wav` and `system.wav`
//! (16 kHz, deleted after `meetingAudioDays`). While a meeting is recorded or processed, the copy
//! in memory is the truth and every edit goes through it; otherwise edits load and save the file.

mod capture;
mod pipeline;
mod questions;
mod system_map;
mod terms;
mod visuals;

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
pub use capture::{add_image, delete_visual, image_data, image_path, move_visual, paste_image, screen_of, set_screen, sources as screen_sources, ScreenState};
pub use visuals::Visual;
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
    /// Misheard terms the term check replaced in `text`, with what was heard (see `terms.rs`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<terms::Fix>,
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
    /// What was shown: text read from a shared screen or a photo (see `visuals.rs`), in the order
    /// they were added.
    pub visuals: Vec<Visual>,
    /// The highest visual id handed out; a deleted visual's id isn't given again (see `new_visual_id`).
    pub last_visual_id: u32,
    /// Places the term check turned down (see `terms.rs`), not asked about again when the notes
    /// are written again.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub term_rejections: Vec<terms::Rejected>,
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

impl Meeting {
    /// The id for a visual about to be added: above every id handed out before, also those of
    /// deleted visuals (the live map remembers which ids it read). Meetings saved before this was
    /// kept start above their visuals' highest id.
    pub fn new_visual_id(&mut self) -> u32 {
        let id = visuals::next_id(&self.visuals, self.last_visual_id);
        self.last_visual_id = id;
        id
    }
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
    /// Reading the shared screen of the meeting being recorded (see `capture.rs`).
    screen: Mutex<ScreenState>,
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

/// Whether `id` can name a meeting's folder: ids are UUIDs, so letters, digits, '-' and '_' only
/// (no separators, no "..", not empty), and a command can't reach outside `meetings/`.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The folder of meeting `id`; an error for an id that isn't one (see `valid_id`).
pub fn dir_of(data: &Path, id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(valid_id(id), "That meeting no longer exists.");
    Ok(root(data).join(id))
}

fn load(data: &Path, id: &str) -> Option<Meeting> {
    let mut m: Meeting = read_json(&dir_of(data, id).ok()?.join("meeting.json"))?;
    m.has_audio = has_audio(data, id);
    Some(m)
}

fn has_audio(data: &Path, id: &str) -> bool {
    dir_of(data, id).is_ok_and(|d| d.join("mic.wav").exists() || d.join("system.wav").exists())
}

fn save(data: &Path, m: &Meeting) {
    if let Err(e) = dir_of(data, &m.id).and_then(|d| write_json(&d.join("meeting.json"), m)) {
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
    std::fs::remove_dir_all(dir_of(&c.dir, id)?)?;
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
            capture::prune_pictures(data, &m.id);
            let Ok(dir) = dir_of(data, &m.id) else { continue };
            for f in ["mic.wav", "system.wav"] {
                let p = dir.join(f);
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
    let dir = dir_of(&c.dir, &id)?;
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
    // Shared screens (when turned on and a window is picked) and photos arriving in Downloads.
    let (app4, shared4, stop4) = (app.clone(), shared.clone(), stop.clone());
    thread::Builder::new().name("wordfall-screen".into()).spawn(move || capture::screen_loop(&app4, &shared4, &stop4))?;
    capture::watch_downloads(app.clone(), id.clone());
    let app2 = app.clone();
    thread::Builder::new().name("wordfall-meeting".into()).spawn(move || {
        let busy = crate::platform::busy("Meeting");
        record(&app2, &shared, &dir, mic, system, &stop, &talk_done, limit, &settings);
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
    /// Names said but not (yet) on the map, the latest first: listed under the live map.
    pub also_mentioned: Vec<String>,
}

pub fn map_of(app: &AppHandle, id: &str) -> anyhow::Result<LiveMap> {
    let m = get(app, id)?;
    let status = live_status(app, &m);
    let also_mentioned = m.system_map.also_mentioned();
    Ok(LiveMap { map: m.system_map, status, also_mentioned })
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
    /// The highest id of the screens and photos read so far (ids aren't reused, so a visual
    /// deleted meanwhile doesn't hide the next one).
    visuals_seen: u32,
    /// The notes as that run read them.
    notes: String,
    /// Meeting time of the last tidy-up (see `system_map::tidy`).
    tidied_ms: u64,
}

impl MapCursor {
    /// After a run that read the meeting up to here.
    fn read(&mut self, m: &Meeting) {
        self.seen = m.segments.len();
        self.visuals_seen = m.visuals.iter().map(|v| v.id).fold(self.visuals_seen, u32::max);
        self.notes = m.notes.clone();
    }

    /// The visuals added since the last run: from the first one it didn't read on (they are kept
    /// in the order added).
    fn unseen<'a>(&self, m: &'a Meeting) -> &'a [Visual] {
        let from = m.visuals.iter().position(|v| v.id > self.visuals_seen).unwrap_or(m.visuals.len());
        &m.visuals[from..]
    }

    /// Something was shown since the last run (a screen changed, a photo was added).
    fn shown_since(&self, m: &Meeting) -> bool {
        !self.unseen(m).is_empty()
    }

    /// A tidy-up is due every `TIDY_EVERY_MS` of meeting time.
    fn tidy_due(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.tidied_ms) >= system_map::TIDY_EVERY_MS
    }
}

/// On a call (someone's voice came in as system audio) or all in one room on the microphone.
fn room_of(m: &Meeting) -> system_map::Room {
    if m.segments.iter().any(|s| s.channel == Channel::System) { system_map::Room::Call } else { system_map::Room::OneMic }
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
        visuals: cursor.unseen(m),
        earlier,
        speakers: &m.speakers,
        map: &m.system_map,
        notes_changed: m.notes.trim() != cursor.notes.trim(),
        room: room_of(m),
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
    cursor.read(&snapshot);
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
        // How many changes, not what they were: the log outlives the meeting.
        if applied.lines.is_empty() { "no change".to_string() } else { format!("{} changes", applied.lines.len()) }
    );
    Ok(())
}

/// The tidy-up on the meeting being recorded: the model looks at every box once (keep, merge, drop,
/// re-kind), the changes are applied to the map as it is by then.
fn live_tidy_step(app: &AppHandle, w: &mut llm::Worker, meeting: &Arc<Mutex<Meeting>>, cursor: &mut MapCursor) -> anyhow::Result<()> {
    let c = app.state::<Core>();
    let snapshot = meeting.lock().clone();
    cursor.tidied_ms = snapshot.duration_ms;
    let room = room_of(&snapshot);
    let started = Instant::now();
    let Some(t) = system_map::tidy(w, &snapshot.system_map, &snapshot.notes, room)? else { return Ok(()) };
    let applied = {
        let mut m = meeting.lock();
        let notes = m.notes.clone();
        let applied = system_map::apply_tidy(&mut m.system_map, &t.verdicts, &notes);
        save(&c.dir, &m);
        applied
    };
    emit_map(app, &snapshot.id);
    log::info!(
        "system map tidy-up in {:.1}s ({}), {}",
        started.elapsed().as_secs_f32(),
        stats_line(Some(t.cost)),
        // How many changes, not what they were: the log outlives the meeting.
        if applied.lines.is_empty() { "no change".to_string() } else { format!("{} changes", applied.lines.len()) }
    );
    Ok(())
}

/// While the meeting records: whenever enough new talk has come in (or the notes changed), the
/// live model updates the system map with it, then proposes questions, which go after what the
/// map still doesn't know. One model process serves both. Every `TIDY_EVERY_MS` of meeting time,
/// the map is tidied up once after its run.
///
/// When the meeting stops, the map is tidied up and then reads the last of the talk once more (after
/// `talk_done`: the last seconds are transcribed), so nothing said at the end is left out.
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
        let (now_ms, words, expired, notes_changed, shown) = {
            let mut m = meeting.lock();
            let now = m.duration_ms;
            let expired = questions::expire(&mut m.questions, now);
            let map = settings.live_map();
            (now, word_count(&m.segments), expired, map && m.notes.trim() != cursor.notes.trim(), map && cursor.shown_since(&m))
        };
        if expired {
            emit_questions(app, &id);
        }
        let due = (words.saturating_sub(words_then) >= QUESTION_WORDS || (notes_changed && words > 0) || shown)
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
                if cursor.tidy_due(now_ms) {
                    live_tidy_step(app, w, meeting, &mut cursor)?;
                }
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
                    visuals: &snapshot.visuals,
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
        if let Err(e) = live_tidy_step(app, w, meeting, &mut cursor) {
            log::warn!("the system map's last tidy-up failed: {e:#}");
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
    dir: &Path,
    mic: Recording,
    system: Option<Recording>,
    stop: &AtomicBool,
    talk_done: &AtomicBool,
    limit: Option<Duration>,
    settings: &Settings,
) {
    let c = app.state::<Core>();
    let id = meeting.lock().id.clone();
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
    let print = voice_print(&c.dir, settings);
    let mut live = LiveSpeakers::new(settings.meeting_speakers, &meeting.lock().speakers, print.as_deref());
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

    // The final transcript, then speaker labels over the whole meeting.
    let embedder = embedder.or_else(|| load_embedder(app, settings));
    let mut m = meeting.lock().clone();
    if settings.meeting_final_pass {
        if let Some((segments, final_embeddings)) = final_transcript(app, &m, dir, embedder.as_ref(), settings) {
            m.segments = segments;
            embeddings = final_embeddings;
        }
    }
    progress(app, &id, "speakers", None, None);
    if let Some(e) = embedder.as_ref() {
        fill_embeddings(dir, &m.segments, &mut embeddings, e);
    }
    embeddings.resize(m.segments.len(), None);
    let dropped = pipeline::drop_echo(&mut m.segments, &mut embeddings, &echo);
    if !dropped.is_empty() {
        let by_voice = dropped.iter().filter(|(_, why)| matches!(why, pipeline::Echo::Voice(..))).count();
        log::info!("dropped {} microphone segments that echoed the other side ({by_voice} by voice)", dropped.len());
    }
    let levels = segment_levels(&m.segments, |ch, start, len| {
        crate::audio::read_wav_16k_range(&dir.join(if ch == Channel::Mic { "mic.wav" } else { "system.wav" }), start, len).ok()
    });
    let hints = pipeline::MeHints { print: print.as_deref(), levels: &levels };
    settle_speakers(meeting, m.segments, &embeddings, &hints, settings.meeting_speakers && embedder.is_some());
    save(&c.dir, &meeting.lock());
    let _ = app.emit("meeting-changed", json!({ "id": id }));
}

/// Puts the finished meeting's `segments` into the open copy with speaker labels over the whole
/// meeting (`pipeline::assign_speakers`). The labels start from the speakers as they are now, not
/// as copied before the final pass: the user may have named one while it ran (it takes minutes).
/// Only what this changes is written; the live model may have updated the map or questions.
fn settle_speakers(meeting: &Mutex<Meeting>, mut segments: Vec<Segment>, embeddings: &pipeline::Embeddings, hints: &pipeline::MeHints, diarize: bool) {
    let mut g = meeting.lock();
    pipeline::assign_speakers(&mut segments, embeddings, hints, &mut g.speakers, diarize);
    g.language = pipeline::meeting_language(&segments);
    g.segments = segments;
}

/// The user's dictionary (if on) and the vocabulary prompt for whisper.
fn dictionary_and_prompt(c: &Core, settings: &Settings) -> (Option<Vec<crate::store::DictEntry>>, Option<String>) {
    let store = c.store.lock();
    let dict_terms: Vec<String> = store.dictionary.iter().map(|e| e.replacement.clone()).collect();
    let terms = vocab::terms(settings.prompt_mode, &vocab::Sources { dictionary: &dict_terms, user_terms: &store.vocabulary, projects: &c.projects });
    (settings.dictionary_enabled.then(|| store.dictionary.clone()), vocab::prompt(&terms))
}

/// The final transcript of a finished meeting from its recordings (`pipeline::final_segments`),
/// or `None` (the live transcript stays) if the recordings or the recognizer aren't there.
fn final_transcript(
    app: &AppHandle,
    m: &Meeting,
    dir: &Path,
    embedder: Option<&Embedder>,
    settings: &Settings,
) -> Option<(Vec<Segment>, pipeline::Embeddings)> {
    let c = app.state::<Core>();
    crate::session::wait_for_model(app).ok()?;
    let (dictionary, prompt) = dictionary_and_prompt(&c, settings);
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
    let files: Vec<(Channel, PathBuf)> = [(Channel::System, "system.wav"), (Channel::Mic, "mic.wav")]
        .into_iter()
        .map(|(ch, f)| (ch, dir.join(f)))
        .filter(|(ch, f)| f.exists() && (*ch == Channel::Mic || m.system_audio))
        .collect();
    let readers: Vec<(Channel, usize, pipeline::Reader)> = files
        .into_iter()
        .map(|(ch, f)| {
            let len = std::fs::metadata(&f).map(|md| md.len().saturating_sub(44) as usize / 2).unwrap_or(0);
            let read: pipeline::Reader = Box::new(move |start, len| crate::audio::read_wav_16k_range(&f, start, len));
            (ch, len, read)
        })
        .collect();
    if !readers.iter().any(|(ch, ..)| *ch == Channel::Mic) {
        return None;
    }
    let recorded: Vec<pipeline::Recorded> = readers.iter().map(|(channel, len, read)| pipeline::Recorded { channel: *channel, len: *len, read: read.as_ref() }).collect();
    let started = Instant::now();
    let mut last_report = Instant::now();
    // Minutes on a long meeting: the UI says so ("Improving the transcript… 40 %").
    progress(app, &m.id, "improving", Some(0.0), None);
    let result = pipeline::final_segments(&ctx, &recorded, &m.segments, &mut |f| {
        if last_report.elapsed() > Duration::from_millis(500) {
            last_report = Instant::now();
            progress(app, &m.id, "improving", Some(f), None);
        }
    });
    match result {
        Ok((segments, embeddings)) if !segments.is_empty() || m.segments.is_empty() => {
            log::info!("final transcript: {} segments (live: {}) in {:.0}s", segments.len(), m.segments.len(), started.elapsed().as_secs_f32());
            Some((segments, embeddings))
        }
        Ok(_) => {
            log::warn!("the final pass heard nothing; keeping the live transcript");
            None
        }
        Err(e) => {
            log::error!("final transcript failed, keeping the live one: {e:#}");
            None
        }
    }
}

/// The user's voice print (learnt from dictations), when finding "Me" by it is on.
fn voice_print(data: &Path, settings: &Settings) -> Option<Vec<f32>> {
    (settings.meeting_speakers && settings.meeting_voice_print).then(|| speakers::VoicePrint::load(data)).flatten().map(|p| p.embedding)
}

/// Speech level of each microphone segment (`None` for system audio), for telling which voice is
/// nearest the microphone. `audio(channel, start, len)` reads samples of the recording.
fn segment_levels(segments: &[Segment], audio: impl Fn(Channel, usize, usize) -> Option<Vec<f32>>) -> Vec<Option<f32>> {
    segments
        .iter()
        .map(|s| {
            let (start, len) = ((s.start_ms * 16) as usize, (s.end_ms.saturating_sub(s.start_ms) * 16) as usize);
            (s.channel == Channel::Mic).then(|| audio(s.channel, start, len)).flatten().and_then(|a| pipeline::level_db(&a))
        })
        .collect()
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
    let (dictionary, prompt) = dictionary_and_prompt(&c, settings);
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
                fixes: Vec::new(),
            };
            if seg.channel == Channel::Mic && echo.check(&seg, h.embedding.as_deref(), &m.segments).is_some() {
                continue;
            }
            let Meeting { speakers, .. } = &mut *m;
            let speaker = live.assign(seg.channel, h.embedding.as_deref(), seg.end_ms.saturating_sub(seg.start_ms), speakers);
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
    let result = (|| -> anyhow::Result<(String, String)> {
        let entry = llm::entry(&settings.summary_model).ok_or_else(|| anyhow::anyhow!("unknown summary model"))?;
        let path = llm::locate(&c.model_dirs.read().own, entry)
            .ok_or_else(|| anyhow::anyhow!("{} isn't downloaded", entry.display_name))?;
        let started = Instant::now();
        let mut worker = llm::Worker::spawn(&path, &c.dir.join("llm.log"), llm::NOTES_CTX)?;
        let (entries, dictionary) = {
            let store = c.store.lock();
            (store.vocabulary.clone(), store.dictionary.clone())
        };
        let vocabulary = meeting_vocabulary(&entries, &dictionary, &settings, &meeting.lock().clone());
        if settings.meeting_term_check {
            check_terms(app, meeting, &mut worker, &vocabulary);
        }
        let snapshot = meeting.lock().clone();
        let lines = pipeline::timeline_lines(&snapshot.segments, &snapshot.speakers, &snapshot.visuals, visuals::NOTES);
        let glossary = terms::glossary(&vocabulary, &lines.join("\n"));
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
            shown: visuals::any_shown(&lines),
            glossary: &glossary,
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

/// The vocabulary of `m` (see `terms::vocabulary`): `entries` (the user's words to recognise), the
/// words dictionary replacements write (when the dictionary is on), and what was typed into the
/// meeting or shown in it.
fn meeting_vocabulary(entries: &[String], dictionary: &[crate::store::DictEntry], settings: &Settings, m: &Meeting) -> Vec<terms::Term> {
    let replacements: Vec<String> =
        if settings.dictionary_enabled { dictionary.iter().map(|e| e.replacement.clone()).collect() } else { Vec::new() };
    let mut typed: Vec<&str> = vec![&m.notes, &m.goal];
    if m.title_from_user {
        typed.push(&m.title);
    }
    typed.extend(m.visuals.iter().map(|v| v.text.as_str()));
    terms::vocabulary(entries, &replacements, &typed)
}

/// Fixes misheard terms in the finished meeting's transcript before the notes read it (see
/// `terms.rs`): every place that sounds like a term of `vocabulary` goes to the summary model,
/// which says yes or no; places it turned down before are skipped. A segment changes only if its
/// text is still what was checked; what it said before stays in its `fixes`. Shown as "Checking
/// terms…" (with a share) while it runs. The log gets counts only: the transcript stays out of it.
fn check_terms(app: &AppHandle, meeting: &Mutex<Meeting>, worker: &mut llm::Worker, vocabulary: &[terms::Term]) {
    if vocabulary.is_empty() {
        return;
    }
    let (id, segments, rejected) = {
        let m = meeting.lock();
        (m.id.clone(), m.segments.clone(), m.term_rejections.clone())
    };
    let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
    let started = Instant::now();
    progress(app, &id, "checking", Some(0.0), None);
    let skip = turned_down(&segments, &rejected);
    let decisions = match terms::check(worker, &texts, vocabulary, skip, |f| progress(app, &id, "checking", Some(f), None)) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("the term check failed, the transcript stays as it was: {e:#}");
            return;
        }
    };
    let fixed = decisions.iter().filter(|d| d.yes).count();
    log::info!(
        "term check: {} candidates, {} accepted, {} rejected in {:.1}s",
        decisions.len(),
        fixed,
        decisions.len() - fixed,
        started.elapsed().as_secs_f32()
    );
    if decisions.is_empty() {
        return;
    }
    {
        let mut m = meeting.lock();
        record_decisions(&mut m, &segments, &texts, &decisions);
    }
    save(&app.state::<Core>().dir, &meeting.lock());
    if fixed > 0 {
        let _ = app.emit("meeting-changed", json!({ "id": id }));
        // The UI drops a meeting's progress on a change: the notes start reading straight away.
        progress(app, &id, "reading", Some(0.0), None);
    }
}

/// Whether a candidate in segment `i` of `segments` is a place the term check turned down before.
fn turned_down<'a>(segments: &'a [Segment], rejected: &'a [terms::Rejected]) -> impl Fn(usize, &terms::Candidate) -> bool + 'a {
    move |i, c| rejected.iter().any(|r| r.start_ms == segments[i].start_ms && r.heard == c.heard && r.written == c.written)
}

/// The term check's `decisions` on `segments` (with `texts`, as checked) into `m`: accepted ones
/// change a segment whose text is still the same (keeping what was heard in its `fixes`), turned
/// down ones are remembered.
fn record_decisions(m: &mut Meeting, segments: &[Segment], texts: &[&str], decisions: &[terms::Decision]) {
    for (i, text, fixes) in terms::fixed(texts, decisions) {
        let old = &segments[i];
        if let Some(seg) = m.segments.iter_mut().find(|s| s.start_ms == old.start_ms && s.channel == old.channel && s.text == old.text) {
            seg.text = text;
            terms::add_fixes(&mut seg.fixes, fixes);
        }
    }
    for d in decisions.iter().filter(|d| !d.yes) {
        let r = terms::Rejected { start_ms: segments[d.segment].start_ms, heard: d.candidate.heard.clone(), written: d.candidate.written.clone() };
        if !m.term_rejections.contains(&r) {
            m.term_rejections.push(r);
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
    /// (meeting ms, prompt tokens, written, ms) per map run, tidy-up and questions run.
    map_runs: Vec<llm::ChatStats>,
    tidy_runs: Vec<llm::ChatStats>,
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
        let shown = self.map && self.cursor.shown_since(meeting);
        let due = (words.saturating_sub(self.words_then) >= QUESTION_WORDS || (notes_changed && words > 0) || shown)
            && self.last_run.is_none_or(|t| now_ms.saturating_sub(t) >= QUESTION_GAP_MS);
        if due {
            self.words_then = words;
            self.last_run = Some(now_ms);
            let language = pipeline::meeting_language(&meeting.segments);
            if self.map {
                self.map_step(meeting, now_ms)?;
                if self.cursor.tidy_due(now_ms) {
                    self.tidy_step(meeting, now_ms)?;
                }
            }
            if self.ask {
                let unknowns = meeting.system_map.unknowns();
                let input = questions::Input {
                    goal: &meeting.goal,
                    notes: &meeting.notes,
                    language: language.as_deref(),
                    segments: &meeting.segments,
                    speakers: &meeting.speakers,
                    visuals: &meeting.visuals,
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
        self.cursor.read(meeting);
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

    /// The tidy-up, printing what it changed.
    fn tidy_step(&mut self, meeting: &mut Meeting, now_ms: u64) -> anyhow::Result<()> {
        let t = now_ms as f32 / 1000.0;
        self.cursor.tidied_ms = now_ms;
        let room = room_of(meeting);
        let started = Instant::now();
        let Some(tidy) = system_map::tidy(&mut self.worker, &meeting.system_map, &meeting.notes, room)? else { return Ok(()) };
        self.tidy_runs.push(tidy.cost);
        eprintln!("[{t:>6.1}s] (map tidy-up in {:.1}s: {})", started.elapsed().as_secs_f32(), stats_line(Some(tidy.cost)));
        if std::env::var_os("WORDFALL_MAP_RAW").is_some() {
            eprintln!("[{t:>6.1}s] tidy-up answer:\n{}", tidy.answer.trim_end());
        }
        for l in system_map::apply_tidy(&mut meeting.system_map, &tidy.verdicts, &meeting.notes.clone()).lines {
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
            tidy_runs: Vec::new(),
            question_runs: Vec::new(),
            snapshots: Vec::new(),
        }))
    }

    /// At the end: the map is tidied up and reads what was said since its last run, as in a live
    /// meeting; then the cost report, and the map's snapshots to `map_json`.
    fn done(&mut self, meeting: &mut Meeting, now_ms: u64, map_json: Option<&str>) -> anyhow::Result<()> {
        if self.map {
            self.tidy_step(meeting, now_ms)?;
        }
        if self.map && (self.cursor.seen < meeting.segments.len() || self.cursor.shown_since(meeting)) {
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
        format!("{}\n{}\n{}", line("map", &self.map_runs), line("tidy-up", &self.tidy_runs), line("questions", &self.question_runs))
    }
}

/// `wordfall --meeting <mic.wav> [--system <system.wav>] [--no-summary] [--questions] [--goal "…"]`:
/// runs a recorded meeting through the same pipeline as a live one (fed in real-time-sized pieces)
/// and prints the transcript and notes; with `--questions`, also the live questions as they come.
/// The final pass runs at the end as in the app (`--no-final` keeps the live transcript);
/// `--voice <voice.json>` uses another voice print than the user's; `--save-final <file>` keeps
/// the final segments, speakers and visuals; `--screen-frames <dir>` reads a shared screen's
/// frames as the app does (see `cli_screen_frames`).
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
    // `--voice <voice.json>`: a voice print other than the user's own.
    let print = match arg("--voice") {
        Some(p) => Some(crate::storage::read_json::<speakers::VoicePrint>(Path::new(&p)).ok_or_else(|| anyhow::anyhow!("can't read {p}"))?.embedding),
        None => voice_print(&data_dir, &settings),
    };
    let mut live = LiveSpeakers::new(embedder.is_some(), &meeting.speakers, print.as_deref());
    let mut echo = EchoFilter::default();
    meeting.goal = arg("--goal").unwrap_or_default();
    meeting.notes = arg("--notes").unwrap_or_default();
    let mut shown = cli_visuals(args, &settings)?;
    let mut frames = cli_screen_frames(args, &settings)?;
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
                    fixes: Vec::new(),
                };
                if seg.channel == Channel::Mic {
                    if let Some(why) = echo.check(&seg, h.embedding.as_deref(), &meeting.segments) {
                        eprintln!("(echo dropped, {why:?}) {}", seg.text);
                        continue;
                    }
                }
                let speaker = live.assign(seg.channel, h.embedding.as_deref(), seg.end_ms.saturating_sub(seg.start_ms), &mut meeting.speakers);
                eprintln!("[{:>6.1}s] {:?} {} | {}", seg.start_ms as f32 / 1000.0, seg.channel, speaker, seg.text);
                meeting.segments.push(Segment { speaker, ..seg });
                embeddings.push(h.embedding);
            }
        }
        show_due(&mut meeting, &mut shown, end as u64 / 16);
        observe_due(&mut meeting, &mut frames, end as u64 / 16);
        if let Some(a) = asker.as_mut() {
            a.step(&mut meeting, end as u64 / 16)?;
        }
        pos = end;
    }
    if let Some(path) = arg("--save-transcript") {
        // What the live model saw, for replaying it with `--transcript` (no speech recognition).
        std::fs::write(&path, serde_json::to_string_pretty(&json!({ "segments": meeting.segments, "speakers": meeting.speakers, "visuals": meeting.visuals }))?)?;
    }
    if let Some(mut a) = asker {
        a.done(&mut meeting, longest as u64 / 16, arg("--map-json").as_deref())?;
    }
    meeting.duration_ms = longest as u64 / 16;
    let live_ids = meeting.speakers.iter().map(|s| s.id).max().unwrap_or(0);
    // `--no-final`: keep the live transcript (as when the final pass is off).
    if !args.iter().any(|a| a == "--no-final") && settings.meeting_final_pass {
        let t = Instant::now();
        let readers: Vec<(Channel, usize, pipeline::Reader)> = sources
            .iter()
            .map(|(ch, audio)| {
                let read: pipeline::Reader = Box::new(move |start, len| Ok(audio[start.min(audio.len())..(start + len).min(audio.len())].to_vec()));
                (*ch, audio.len(), read)
            })
            .collect();
        let recorded: Vec<pipeline::Recorded> =
            readers.iter().map(|(channel, len, read)| pipeline::Recorded { channel: *channel, len: *len, read: read.as_ref() }).collect();
        // What the app shows as "Improving the transcript… N %", whenever it moves a whole percent.
        let mut shown = -1;
        let (segments, final_embeddings) = pipeline::final_segments(&ctx, &recorded, &meeting.segments, &mut |f| {
            let percent = (f * 100.0).floor() as i32;
            if percent > shown {
                shown = percent;
                eprintln!("(final pass {percent:>3} % at {:.1}s)", t.elapsed().as_secs_f32());
            }
        })?;
        eprintln!("final pass: {} segments (live: {}) in {:.1}s", segments.len(), meeting.segments.len(), t.elapsed().as_secs_f32());
        meeting.segments = segments;
        embeddings = final_embeddings;
    }
    let transcribed = started.elapsed();
    for (seg, why) in pipeline::drop_echo(&mut meeting.segments, &mut embeddings, &echo) {
        eprintln!("(echo dropped at the end, {why:?}) [{:>6.1}s] {}", seg.start_ms as f32 / 1000.0, seg.text);
    }
    let levels = segment_levels(&meeting.segments, |ch, start, len| {
        let audio = &sources.iter().find(|(c, _)| *c == ch)?.1;
        audio.get(start.min(audio.len())..(start + len).min(audio.len())).map(<[f32]>::to_vec)
    });
    let hints = pipeline::MeHints { print: print.as_deref(), levels: &levels };
    pipeline::assign_speakers(&mut meeting.segments, &embeddings, &hints, &mut meeting.speakers, embedder.is_some());
    eprintln!("speakers: live up to id {live_ids}, final {}", meeting.speakers.len());
    meeting.language = pipeline::meeting_language(&meeting.segments);
    meeting.visuals.append(&mut shown);
    let vocabulary = cli_vocabulary(args, &data_dir, &settings, &meeting)?;
    if args.iter().any(|a| a == "--check-terms") {
        cli_check_terms(&data_dir, &settings, &mut meeting, &vocabulary)?;
    }
    if let Some(path) = arg("--save-final") {
        // The final segments and speakers, as the app saves them in meeting.json.
        std::fs::write(&path, serde_json::to_string_pretty(&json!({ "segments": meeting.segments, "speakers": meeting.speakers, "visuals": meeting.visuals }))?)?;
    }
    let lines = pipeline::timeline_lines(&meeting.segments, &meeting.speakers, &meeting.visuals, visuals::NOTES);
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
    let glossary = terms::glossary(&vocabulary, &lines.join("\n"));
    print_notes(&data_dir, &settings, &lines, &speakers, meeting.language.as_deref(), meeting.duration_ms / 60_000, &meeting.notes, &glossary, &extra, args)
}

/// `wordfall --transcript <saved.json> --map [--questions] [--note-at "90:text"] [--llm <id>] [--map-json f]`:
/// the live model on a transcript `--meeting … --save-transcript` saved, with the segments arriving
/// at the pace they were said (no speech recognition, so runs are quick and comparable). Prints
/// what changes, the cost of each run and the map's section. Screens come from `--screen-frames`,
/// `--screen-text`/`--image`, or else the visuals saved with the transcript.
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
    let mut shown = cli_visuals(args, &settings)?;
    let mut frames = cli_screen_frames(args, &settings)?;
    // The screens saved with the transcript come back at their time, unless others are given.
    if shown.is_empty() && frames.is_empty() {
        shown = serde_json::from_value(saved["visuals"].clone()).unwrap_or_default();
        shown.sort_by_key(|v: &Visual| v.at_ms);
    }
    let mut asker = CliAssist::from_args(args, &settings, &data_dir)?.ok_or_else(|| anyhow::anyhow!("add --map and/or --questions"))?;
    let end_ms = segments.iter().map(|s| s.end_ms).max().unwrap_or(0) + 8_000;
    // As in `cli`: every 8 s of meeting time, what has been transcribed by then (about 3 s after it was said).
    let (mut next, mut t) = (0, 8_000u64);
    while t <= end_ms {
        while next < segments.len() && segments[next].end_ms + 3_000 <= t {
            meeting.segments.push(segments[next].clone());
            next += 1;
        }
        show_due(&mut meeting, &mut shown, t);
        observe_due(&mut meeting, &mut frames, t);
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

/// `wordfall --meeting-notes <transcript.txt | meeting.json> [--language de] [--minutes 30] [--notes "…"]
/// [--screen-text "90:f.txt"] [--image "90:f.png"]`: writes notes for a transcript as `--meeting` prints it
/// ("[mm:ss] Speaker: text" lines), or for a saved meeting (its segments, speakers and visuals, as the app
/// gives them to the notes), without the audio. For working on the notes prompt.
pub fn cli_notes(args: &[String]) -> anyhow::Result<()> {
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let path = arg("--meeting-notes").ok_or_else(|| anyhow::anyhow!("missing transcript file"))?;
    let data_dir = dirs::data_dir().ok_or_else(|| anyhow::anyhow!("no data dir"))?.join("com.wordfall.app");
    let settings = Settings::load(&data_dir);
    let content = std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
    let shown = cli_visuals(args, &settings)?;
    let notes = arg("--notes").unwrap_or_default();
    if path.ends_with(".json") {
        let mut m: Meeting = serde_json::from_str(&content).map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
        anyhow::ensure!(!m.segments.is_empty(), "no transcript in {path}");
        m.visuals.extend(shown);
        if m.notes.is_empty() {
            m.notes = notes.clone();
        }
        let vocabulary = cli_vocabulary(args, &data_dir, &settings, &m)?;
        if args.iter().any(|a| a == "--check-terms") {
            cli_check_terms(&data_dir, &settings, &mut m, &vocabulary)?;
            if let Some(out) = arg("--save-checked") {
                std::fs::write(&out, serde_json::to_string_pretty(&json!({ "segments": m.segments, "speakers": m.speakers }))?)?;
            }
        }
        if args.iter().any(|a| a == "--no-summary") {
            return Ok(());
        }
        let lines = pipeline::timeline_lines(&m.segments, &m.speakers, &m.visuals, visuals::NOTES);
        let used: Vec<u32> = m.segments.iter().map(|s| s.speaker).collect();
        let speakers: Vec<String> = m.speakers.iter().filter(|s| used.contains(&s.id)).map(|s| s.name.clone()).collect();
        let language = arg("--language").or(m.language.clone());
        let minutes = arg("--minutes").and_then(|v| v.parse().ok()).unwrap_or(m.duration_ms / 60_000);
        let notes = if notes.is_empty() { m.notes.clone() } else { notes };
        if args.iter().any(|a| a == "--print-lines") {
            lines.iter().filter(|l| visuals::any_shown(std::slice::from_ref(l))).for_each(|l| eprintln!("{l}"));
        }
        let glossary = terms::glossary(&vocabulary, &lines.join("\n"));
        return print_notes(&data_dir, &settings, &lines, &speakers, language.as_deref(), minutes, &notes, &glossary, &[], args);
    }
    let line = regex::Regex::new(r"^\[[\d:]+\] ([^:]+): ")?;
    let lines: Vec<String> = content.lines().filter(|l| line.is_match(l)).map(str::to_string).collect();
    anyhow::ensure!(!lines.is_empty(), "no transcript lines in {path}");
    let mut speakers: Vec<String> = Vec::new();
    for l in &lines {
        let name = line.captures(l).expect("matched above")[1].to_string();
        if !speakers.contains(&name) {
            speakers.push(name);
        }
    }
    let talk_chars = lines.iter().map(|l| l.len() + 1).sum();
    let lines = visuals::merge(
        lines.into_iter().map(|l| (visuals::line_ms(&l).unwrap_or(0), l)).collect(),
        visuals::lines(&shown, visuals::NOTES, talk_chars),
    );
    let minutes = arg("--minutes").and_then(|m| m.parse().ok()).unwrap_or(1);
    let vocabulary = cli_vocabulary(args, &data_dir, &settings, &Meeting { notes: notes.clone(), visuals: shown, ..Default::default() })?;
    let glossary = terms::glossary(&vocabulary, &lines.join("\n"));
    print_notes(&data_dir, &settings, &lines, &speakers, arg("--language").as_deref(), minutes, &notes, &glossary, &[], args)
}

/// `--screen-text "90:f.txt"` (a screen at 90 s showing the file's lines, the same on any computer)
/// and `--image "90:f.png"` (a photo at 90 s, read with the OCR) for the meeting commands.
fn cli_visuals(args: &[String], settings: &Settings) -> anyhow::Result<Vec<Visual>> {
    let mut out: Vec<Visual> = Vec::new();
    for (i, a) in args.iter().enumerate() {
        let source = match a.as_str() {
            "--screen-text" => visuals::Source::Screen,
            "--image" => visuals::Source::Photo,
            _ => continue,
        };
        let v = args.get(i + 1).ok_or_else(|| anyhow::anyhow!("{a} needs \"seconds:file\""))?;
        let (at, file) = v.split_once(':').ok_or_else(|| anyhow::anyhow!("{a} needs \"seconds:file\""))?;
        let at_ms = at.trim().parse::<u64>()? * 1000;
        let file = Path::new(file.trim());
        let text = match source {
            visuals::Source::Screen => std::fs::read_to_string(file).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?,
            visuals::Source::Photo => {
                let started = Instant::now();
                let rows = capture::read_file(file, &settings.languages, &[])?;
                eprintln!("(read {} in {} ms: {} rows)", file.display(), started.elapsed().as_millis(), rows.len());
                rows.join("\n")
            }
        };
        let label = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let id = visuals::next_id(&out, 0);
        let until_ms = (source == visuals::Source::Screen).then_some(at_ms);
        out.push(Visual { id, at_ms, until_ms, source, label, text, image: None });
    }
    out.sort_by_key(|v| v.at_ms);
    Ok(out)
}

/// `--screen-frames <dir>`: frames of a shared screen as the app reads one every 10 s, for the
/// meeting commands. Each file is named by its meeting time in seconds ("130.txt", "130.jpg"); a
/// `.txt` holds the rows the OCR read (one a line, as `--ocr` prints them), an image is read with
/// the OCR. They go through `visuals::observe` like the app's frames (see `observe_due`).
fn cli_screen_frames(args: &[String], settings: &Settings) -> anyhow::Result<Vec<(u64, Vec<String>)>> {
    let Some(dir) = args.iter().position(|a| a == "--screen-frames").and_then(|i| args.get(i + 1)) else { return Ok(Vec::new()) };
    let mut frames = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| anyhow::anyhow!("{dir}: {e}"))? {
        let path = entry?.path();
        let Some(secs) = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u64>().ok()) else { continue };
        let rows: Vec<String> = match path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref() {
            Some("txt") => std::fs::read_to_string(&path)?.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect(),
            Some("jpg" | "jpeg" | "png") => capture::read_file(&path, &settings.languages, &[])?,
            _ => continue,
        };
        frames.push((secs * 1000, rows));
    }
    frames.sort_by_key(|(ms, _)| *ms);
    if !frames.is_empty() {
        eprintln!("(screen frames: {} from {dir})", frames.len());
    }
    Ok(frames)
}

/// Reads the screen frames due by `now_ms` as the app does (`visuals::observe`): a frame showing
/// something new becomes a screen visual, the same screen lasts longer.
fn observe_due(meeting: &mut Meeting, frames: &mut Vec<(u64, Vec<String>)>, now_ms: u64) {
    while frames.first().is_some_and(|(at, _)| *at <= now_ms) {
        let (at_ms, rows) = frames.remove(0);
        if visuals::observe(&mut meeting.visuals, &rows, at_ms) == visuals::Seen::New {
            let id = meeting.new_visual_id();
            eprintln!("[{:>6.1}s] (screen {id} new) {}", at_ms as f32 / 1000.0, rows.first().map(String::as_str).unwrap_or_default());
            meeting.visuals.push(Visual {
                id,
                at_ms,
                until_ms: Some(at_ms),
                source: visuals::Source::Screen,
                label: "Screen".into(),
                text: rows.join("\n"),
                image: None,
            });
        }
    }
}

/// Moves the visuals due by `now_ms` into the meeting, as if shown then.
fn show_due(meeting: &mut Meeting, pending: &mut Vec<Visual>, now_ms: u64) {
    while pending.first().is_some_and(|v| v.at_ms <= now_ms) {
        let mut v = pending.remove(0);
        v.id = meeting.new_visual_id();
        eprintln!("[{:>6.1}s] ({} shown) {}", v.at_ms as f32 / 1000.0, v.label, v.text.lines().next().unwrap_or_default());
        meeting.visuals.push(v);
    }
}

/// `wordfall --ocr <image>`: the text rows the OCR reads in an image, and how long it took.
pub fn cli_ocr(args: &[String]) -> anyhow::Result<()> {
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let path = arg("--ocr").ok_or_else(|| anyhow::anyhow!("missing image file"))?;
    let data_dir = dirs::data_dir().ok_or_else(|| anyhow::anyhow!("no data dir"))?.join("com.wordfall.app");
    let settings = Settings::load(&data_dir);
    for round in 1..=2 {
        let started = Instant::now();
        let rows = capture::read_file(Path::new(&path), &settings.languages, &[])?;
        let ms = started.elapsed().as_millis();
        if round == 1 {
            for r in &rows {
                println!("{r}");
            }
        }
        println!("--- {} rows in {ms} ms ({}) ---", rows.len(), if round == 1 { "cold" } else { "warm" });
    }
    Ok(())
}

/// The vocabulary for the meeting commands, as the app makes it (`meeting_vocabulary`): with
/// `--vocab <file>` (one entry a line, "PO = what it stands for" or a term) instead of the
/// user's words to recognise.
fn cli_vocabulary(args: &[String], data_dir: &Path, settings: &Settings, m: &Meeting) -> anyhow::Result<Vec<terms::Term>> {
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let store = crate::store::Store::load(data_dir);
    let entries: Vec<String> = match arg("--vocab") {
        Some(f) => std::fs::read_to_string(&f).map_err(|e| anyhow::anyhow!("{f}: {e}"))?.lines().map(str::to_string).collect(),
        None => store.vocabulary,
    };
    Ok(meeting_vocabulary(&entries, &store.dictionary, settings, m))
}

/// `--check-terms`: the term check of `check_terms` on `m`, printing every candidate's verdict.
fn cli_check_terms(data_dir: &Path, settings: &Settings, m: &mut Meeting, vocabulary: &[terms::Term]) -> anyhow::Result<()> {
    let entry = llm::entry(&settings.summary_model).ok_or_else(|| anyhow::anyhow!("unknown summary model"))?;
    let path = llm::locate(&settings.model_dirs(data_dir).own, entry).ok_or_else(|| anyhow::anyhow!("{} is not downloaded", entry.id))?;
    let mut worker = llm::Worker::spawn(&path, &data_dir.join("llm.log"), llm::NOTES_CTX)?;
    eprintln!("(term check vocabulary: {})", vocabulary.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(", "));
    let segments = m.segments.clone();
    let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
    let t = Instant::now();
    let decisions = terms::check(&mut worker, &texts, vocabulary, turned_down(&segments, &m.term_rejections), |_| {})?;
    let ms = t.elapsed().as_millis();
    for d in &decisions {
        let s = &m.segments[d.segment];
        eprintln!(
            "(term {}) [{}] {} → {} | {} | {}",
            if d.yes { "yes" } else { "no " },
            pipeline::clock(s.start_ms),
            d.candidate.heard,
            d.candidate.written,
            s.text,
            d.reason
        );
    }
    record_decisions(m, &segments, &texts, &decisions);
    eprintln!("term check: {} of {} candidates fixed in {:.1}s", decisions.iter().filter(|d| d.yes).count(), decisions.len(), ms as f32 / 1000.0);
    Ok(())
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
    glossary: &[String],
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
        shown: visuals::any_shown(lines),
        glossary,
        speakers,
        language,
        when: &chrono::Local::now().format("%A %-d %B %Y, %H:%M").to_string(),
        minutes,
    };
    if !glossary.is_empty() {
        eprintln!("(glossary: {})", glossary.join("; "));
    }
    let mut summary = llm::summarize(&mut worker, &input, |_| {})?;
    for section in extra {
        summary = format!("{summary}\n\n{section}");
    }
    println!("--- notes ({:.1}s, language {language:?}) ---\n{summary}", t.elapsed().as_secs_f32());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visual(id: u32) -> Visual {
        Visual { id, at_ms: 0, until_ms: None, source: visuals::Source::Photo, label: String::new(), text: format!("Bild {id}"), image: None }
    }

    #[test]
    fn a_visual_deleted_meanwhile_doesnt_hide_the_next_from_the_map() {
        let mut m = Meeting::default();
        for _ in 0..2 {
            let id = m.new_visual_id();
            m.visuals.push(visual(id));
        }
        let mut cursor = MapCursor::default();
        cursor.read(&m);
        assert!(!cursor.shown_since(&m));
        // The newest is deleted, then a photo is added: it gets a new id and the map reads it.
        m.visuals.pop();
        let id = m.new_visual_id();
        assert_eq!(id, 3, "a deleted visual's id isn't handed out again");
        m.visuals.push(visual(id));
        assert!(cursor.shown_since(&m));
        assert_eq!(cursor.unseen(&m).iter().map(|v| v.id).collect::<Vec<_>>(), [3]);
        cursor.read(&m);
        // An older one deleted: nothing new to read.
        m.visuals.remove(0);
        assert!(!cursor.shown_since(&m));
    }

    #[test]
    fn meetings_saved_before_visual_ids_were_kept_go_on_above_their_visuals() {
        let mut m: Meeting = serde_json::from_str(r#"{"id":"a","visuals":[{"id":4,"atMs":0,"source":"photo","text":"x"}]}"#).unwrap();
        assert_eq!(m.last_visual_id, 0);
        assert_eq!(m.new_visual_id(), 5);
        assert_eq!(m.new_visual_id(), 6);
    }

    #[test]
    fn meeting_ids_cant_leave_the_meetings_folder() {
        let data = Path::new("/data");
        assert_eq!(dir_of(data, "3f2a8c1e-5b7d-4e90-a1c3-9d8e7f6a5b4c").unwrap(), Path::new("/data/meetings/3f2a8c1e-5b7d-4e90-a1c3-9d8e7f6a5b4c"));
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", "/etc", "a..b", "a b"] {
            assert!(dir_of(data, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_speaker_named_during_the_final_pass_keeps_the_name() {
        let meeting = Mutex::new(Meeting {
            speakers: vec![Speaker { id: ME, name: "Me".into(), renamed: false }, Speaker { id: 1, name: "Speaker 1".into(), renamed: false }],
            ..Default::default()
        });
        let seg = |channel, start_ms, speaker| Segment { start_ms, end_ms: start_ms + 5_000, channel, speaker, text: "Hallo.".into(), language: None, fixes: Vec::new() };
        // Copied before the final pass…
        let before = meeting.lock().clone();
        // …which takes minutes; meanwhile the user names the other side.
        if let Some(s) = meeting.lock().speakers.iter_mut().find(|s| s.id == 1) {
            s.name = "Anna".into();
            s.renamed = true;
        }
        let segments = vec![seg(Channel::Mic, 0, ME), seg(Channel::System, 6_000, 1)];
        assert_eq!(before.speakers[1].name, "Speaker 1");
        settle_speakers(&meeting, segments, &vec![None, None], &pipeline::MeHints { print: None, levels: &[] }, false);
        let m = meeting.lock();
        let other = m.segments.iter().find(|s| s.channel == Channel::System).unwrap().speaker;
        assert_eq!(m.speakers.iter().find(|s| s.id == other).map(|s| (s.name.as_str(), s.renamed)), Some(("Anna", true)));
        assert_eq!(m.segments.len(), 2);
    }
}
