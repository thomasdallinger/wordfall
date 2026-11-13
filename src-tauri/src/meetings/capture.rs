//! Visuals from outside the audio: images the user adds to a meeting (dropped, picked, pasted, or
//! offered from Downloads when a phone sends one by AirDrop) and the shared screen, read while a
//! meeting records from one window the user picks. Text is read with the platform's OCR
//! (`platform::ocr`, macOS for now); pictures are kept as JPEGs under `<meeting>/visuals/` and
//! deleted with the recordings, their text stays. The pure rules are in `visuals.rs`.

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};

use super::{dir_of, edit, get, save, visuals, Meeting, Visual};
use crate::{
    platform::{self, ScreenWindow},
    store::new_id,
    Core,
};

/// Kept pictures are at most this big on their longer side…
const IMAGE_PX: u32 = 1600;
/// …their thumbnails this big…
const THUMB_PX: u32 = 480;
/// …and photos are read at up to this size (small print on a phone photo needs the pixels).
const OCR_PX: u32 = 3200;
/// Files bigger than this aren't images worth reading.
const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;
/// The shared screen is read this often…
const SCREEN_EVERY: Duration = Duration::from_secs(10);
/// Images arriving in Downloads are offered while recording and this long after.
const PHOTO_WATCH_AFTER: Duration = Duration::from_secs(30 * 60);
const PHOTO_POLL: Duration = Duration::from_secs(3);
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "heic", "heif", "tif", "tiff", "gif", "bmp", "webp"];

fn visuals_dir(data: &Path, id: &str) -> anyhow::Result<PathBuf> {
    Ok(dir_of(data, id)?.join("visuals"))
}

/// The thumbnail next to a kept picture ("visuals/x.jpg" → "visuals/x-thumb.jpg").
fn thumb_of(image: &str) -> String {
    match image.strip_suffix(".jpg") {
        Some(stem) => format!("{stem}-thumb.jpg"),
        None => format!("{image}-thumb.jpg"),
    }
}

/// Tells every window a meeting's visuals changed: `meeting-visuals`.
pub fn emit_visuals(app: &AppHandle, m: &Meeting) {
    let _ = app.emit("meeting-visuals", json!({ "id": m.id, "visuals": m.visuals }));
}

/// Names and terms the OCR should prefer: the dictionary's replacements and the vocabulary.
fn ocr_words(c: &Core) -> Vec<String> {
    let store = c.store.lock();
    store.dictionary.iter().map(|e| e.replacement.clone()).chain(store.vocabulary.iter().cloned()).collect()
}

/// The text rows of the image at `path` (see `visuals::rows`).
fn read_rows(c: &Core, path: &Path) -> anyhow::Result<Vec<String>> {
    let languages = c.settings.read().languages.clone();
    Ok(visuals::rows(&platform::ocr(path, &languages, &ocr_words(c))?))
}

/// The text rows of a picture of the shared screen (see `visuals::screen_rows`).
fn read_screen_rows(c: &Core, path: &Path) -> anyhow::Result<Vec<String>> {
    let languages = c.settings.read().languages.clone();
    Ok(visuals::screen_rows(&platform::ocr(path, &languages, &ocr_words(c))?))
}

/// The text rows of an image file, read upright and at a size that keeps small print (for the
/// command line; the app goes through `add_image`).
pub fn read_file(path: &Path, languages: &[String], words: &[String]) -> anyhow::Result<Vec<String>> {
    Ok(visuals::rows(&read_boxes(path, languages, words)?))
}

/// The text rows of an image file showing a shared screen, as the app's screen reading reads
/// them (for the command line's `--screen-frames`).
pub fn read_screen_file(path: &Path, languages: &[String], words: &[String]) -> anyhow::Result<Vec<String>> {
    Ok(visuals::screen_rows(&read_boxes(path, languages, words)?))
}

/// The OCR boxes of an image file, read upright and at a size that keeps small print.
fn read_boxes(path: &Path, languages: &[String], words: &[String]) -> anyhow::Result<Vec<platform::OcrLine>> {
    let upright = std::env::temp_dir().join(format!("wordfall-read-{}.jpg", new_id()));
    platform::normalize_image(path, &upright, OCR_PX)?;
    let lines = platform::ocr(&upright, languages, words);
    let _ = std::fs::remove_file(&upright);
    lines
}

/// Keeps a picture of `from` for meeting `id`: a JPEG and its thumbnail under `visuals/`.
/// Returns its path relative to the meeting's folder.
fn keep_picture(data: &Path, id: &str, from: &Path) -> anyhow::Result<String> {
    std::fs::create_dir_all(visuals_dir(data, id)?)?;
    let name = format!("visuals/{}.jpg", new_id());
    let meeting_dir = dir_of(data, id)?;
    platform::normalize_image(from, &meeting_dir.join(&name), IMAGE_PX)?;
    platform::normalize_image(from, &meeting_dir.join(thumb_of(&name)), THUMB_PX)?;
    Ok(name)
}

/// The image file a command names, checked before anything reads it: an existing regular file
/// (after following links) with an image extension, not empty and at most `MAX_FILE_BYTES`. One
/// offered from Downloads must still be in `downloads`. Returns the file's real path.
fn check_image(path: &Path, downloads: Option<&Path>) -> anyhow::Result<PathBuf> {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string());
    let real = path.canonicalize().map_err(|e| anyhow::anyhow!("{name} can't be read: {e}"))?;
    let meta = std::fs::metadata(&real).map_err(|e| anyhow::anyhow!("{name} can't be read: {e}"))?;
    anyhow::ensure!(meta.is_file() && is_image(path) && is_image(&real), "{name} isn't an image.");
    anyhow::ensure!(meta.len() > 0, "{name} is empty.");
    anyhow::ensure!(meta.len() <= MAX_FILE_BYTES, "{name} is too big to be an image worth reading.");
    if let Some(downloads) = downloads {
        let inside = downloads.canonicalize().is_ok_and(|d| real.starts_with(d));
        anyhow::ensure!(inside, "{name} isn't in Downloads any more.");
    }
    Ok(real)
}

/// Reads an image the user added and keeps it with meeting `id` at `at_ms` (default: now while
/// recording, else the end). `offered`: the user accepted an image offered from Downloads (see
/// `watch_downloads`), which must be there. A photo without text is kept too; the notes just
/// can't use it.
pub fn add_image(app: &AppHandle, id: &str, path: &Path, at_ms: Option<u64>, offered: bool) -> anyhow::Result<Visual> {
    let c = app.state::<Core>();
    get(app, id)?;
    let downloads = if offered { Some(dirs::download_dir().ok_or_else(|| anyhow::anyhow!("There's no Downloads folder."))?) } else { None };
    let real = check_image(path, downloads.as_deref())?;
    let started = Instant::now();
    let dir = visuals_dir(&c.dir, id)?;
    std::fs::create_dir_all(&dir)?;
    // Read upright and at a size that keeps small print.
    let upright = dir.join(format!(".read-{}.jpg", new_id()));
    platform::normalize_image(&real, &upright, OCR_PX)?;
    let rows = read_rows(&c, &upright);
    let kept = rows.as_ref().ok().map(|_| keep_picture(&c.dir, id, &upright));
    let _ = std::fs::remove_file(&upright);
    let rows = rows?;
    let image = kept.transpose()?;
    let label = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    log::info!("image read in {} ms: {} rows", started.elapsed().as_millis(), rows.len());
    let mut added = None;
    let m = edit(app, id, |m| {
        let at_ms = at_ms.unwrap_or(m.duration_ms).min(m.duration_ms);
        let v = Visual { id: m.new_visual_id(), at_ms, until_ms: None, source: visuals::Source::Photo, label, text: rows.join("\n"), image };
        m.visuals.push(v.clone());
        added = Some(v);
    })?;
    emit_visuals(app, &m);
    added.ok_or_else(|| anyhow::anyhow!("That meeting no longer exists."))
}

/// Adds what's on the clipboard: image files copied in Finder, or a copied picture (a screenshot).
pub fn paste_image(app: &AppHandle, id: &str) -> anyhow::Result<Vec<Visual>> {
    get(app, id)?;
    let mut clipboard = arboard::Clipboard::new()?;
    let files: Vec<PathBuf> = clipboard.get().file_list().unwrap_or_default().into_iter().filter(|p| is_image(p)).collect();
    if !files.is_empty() {
        return files.iter().map(|f| add_image(app, id, f, None, false)).collect();
    }
    let picture = clipboard.get_image().map_err(|_| anyhow::anyhow!("There's no image on the clipboard."))?;
    let c = app.state::<Core>();
    let dir = visuals_dir(&c.dir, id)?;
    std::fs::create_dir_all(&dir)?;
    let png = dir.join(format!("Pasted image {}.png", chrono::Local::now().format("%H.%M.%S")));
    let rgba = image::RgbaImage::from_raw(picture.width as u32, picture.height as u32, picture.bytes.into_owned())
        .ok_or_else(|| anyhow::anyhow!("The clipboard's image can't be read."))?;
    rgba.save_with_format(&png, image::ImageFormat::Png)?;
    let added = add_image(app, id, &png, None, false);
    let _ = std::fs::remove_file(&png);
    Ok(vec![added?])
}

fn is_image(path: &Path) -> bool {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.as_str()))
}

/// Deletes a visual and its picture.
pub fn delete_visual(app: &AppHandle, id: &str, visual: u32) -> anyhow::Result<()> {
    let c = app.state::<Core>();
    let mut gone = None;
    let m = edit(app, id, |m| {
        if let Some(i) = m.visuals.iter().position(|v| v.id == visual) {
            gone = Some(m.visuals.remove(i));
        }
    })?;
    let gone = gone.ok_or_else(|| anyhow::anyhow!("That picture is gone."))?;
    if let (Some(image), Ok(dir)) = (gone.image, dir_of(&c.dir, id)) {
        let _ = std::fs::remove_file(dir.join(&image));
        let _ = std::fs::remove_file(dir.join(thumb_of(&image)));
    }
    emit_visuals(app, &m);
    Ok(())
}

/// Moves a visual to another meeting time (a photo added afterwards belongs where it was talked about).
pub fn move_visual(app: &AppHandle, id: &str, visual: u32, at_ms: u64) -> anyhow::Result<()> {
    let mut found = false;
    let m = edit(app, id, |m| {
        let end = m.duration_ms;
        if let Some(v) = m.visuals.iter_mut().find(|v| v.id == visual) {
            v.at_ms = at_ms.min(end);
            v.until_ms = v.until_ms.map(|u| u.max(v.at_ms));
            found = true;
        }
    })?;
    anyhow::ensure!(found, "That picture is gone.");
    emit_visuals(app, &m);
    Ok(())
}

/// A visual's picture (its thumbnail unless `full`) as a data URL, or `None` once it's deleted.
pub fn image_data(app: &AppHandle, id: &str, visual: u32, full: bool) -> anyhow::Result<Option<String>> {
    use base64::Engine as _;
    let c = app.state::<Core>();
    let m = get(app, id)?;
    let Some(image) = m.visuals.iter().find(|v| v.id == visual).and_then(|v| v.image.clone()) else { return Ok(None) };
    let file = dir_of(&c.dir, id)?.join(if full { image } else { thumb_of(&image) });
    Ok(std::fs::read(file).ok().map(|bytes| format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))))
}

/// The full picture's path (to open it in the system viewer), if it's still there.
pub fn image_path(app: &AppHandle, id: &str, visual: u32) -> anyhow::Result<Option<PathBuf>> {
    let c = app.state::<Core>();
    let m = get(app, id)?;
    let dir = dir_of(&c.dir, id)?;
    Ok(m.visuals.iter().find(|v| v.id == visual).and_then(|v| v.image.as_ref()).map(|i| dir.join(i)).filter(|p| p.exists()))
}

/// Deletes the pictures of a meeting (with its recordings); the text stays.
pub fn prune_pictures(data: &Path, id: &str) {
    let Ok(dir) = visuals_dir(data, id) else { return };
    if dir.exists() {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => log::info!("deleted the pictures of meeting {id}"),
            Err(e) => log::warn!("could not delete {}: {e}", dir.display()),
        }
    }
}

// ---------- the shared screen ----------

/// What screen reading is doing, for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ScreenStatus {
    /// Turned off in Settings, not macOS, or not recording.
    #[default]
    Off,
    /// On, but no window picked yet.
    Choose,
    Reading,
    Paused,
    /// macOS hands out only the wallpaper: Screen Recording isn't allowed (or Wordfall wasn't
    /// reopened after it was).
    NeedsPermission,
    /// The picked window closed.
    SourceLost,
}

/// Screen reading of the meeting being recorded.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenState {
    pub status: ScreenStatus,
    pub window: Option<ScreenWindow>,
    #[serde(skip)]
    meeting: Option<String>,
    #[serde(skip)]
    paused: bool,
    /// Read the window at once (just picked), not at the next tick.
    #[serde(skip)]
    now: bool,
}

fn screen<'a>(app: &'a AppHandle) -> parking_lot::MutexGuard<'a, ScreenState> {
    app.state::<Core>().inner().meetings.screen.lock()
}

fn emit_screen(app: &AppHandle, id: &str) {
    let mut payload = json!(screen_of(app, id));
    payload["id"] = json!(id);
    let _ = app.emit("meeting-screen", payload);
}

/// Screen reading of meeting `id` (off unless it's the one being recorded).
pub fn screen_of(app: &AppHandle, id: &str) -> ScreenState {
    let s = screen(app);
    if s.meeting.as_deref() == Some(id) {
        s.clone()
    } else {
        ScreenState::default()
    }
}

fn set_status(app: &AppHandle, id: &str, status: ScreenStatus) {
    let changed = {
        let mut s = screen(app);
        let changed = s.status != status;
        s.status = status;
        changed
    };
    if changed {
        log::info!("screen reading: {status:?}");
        emit_screen(app, id);
    }
}

/// Windows that could show the shared screen, meeting apps first.
pub fn sources() -> Vec<ScreenWindow> {
    platform::screen_windows()
}

/// Picks the window to read (`None`: stop reading), or pauses reading it.
pub fn set_screen(app: &AppHandle, id: &str, window: Option<u32>, paused: bool) -> anyhow::Result<ScreenState> {
    let picked = match window {
        Some(w) => Some(sources().into_iter().find(|s| s.window_id == w).ok_or_else(|| anyhow::anyhow!("That window is gone. Pick another."))?),
        None => None,
    };
    {
        let mut s = screen(app);
        anyhow::ensure!(s.meeting.as_deref() == Some(id), "Screens are read only while the meeting records.");
        s.now = picked.as_ref().is_some_and(|p| s.window.as_ref().map(|w| w.window_id) != Some(p.window_id)) || (s.paused && !paused);
        s.window = picked;
        s.paused = paused;
        s.status = match (&s.window, paused) {
            (None, _) => ScreenStatus::Choose,
            (Some(_), true) => ScreenStatus::Paused,
            (Some(_), false) => ScreenStatus::Reading,
        };
    }
    emit_screen(app, id);
    Ok(screen_of(app, id))
}

/// Without Screen Recording, macOS lists other apps' windows without their titles.
fn titles_hidden(windows: &[ScreenWindow]) -> bool {
    !windows.is_empty() && windows.iter().all(|w| w.title.is_empty())
}

/// While meeting `meeting` records: every `SCREEN_EVERY`, a picture of the picked window is read;
/// a new screen (see `visuals::judge`) is kept as a visual, the same screen extends the latest one.
pub fn screen_loop(app: &AppHandle, meeting: &Arc<Mutex<Meeting>>, stop: &AtomicBool) {
    let c = app.state::<Core>();
    let id = meeting.lock().id.clone();
    let Ok(dir) = visuals_dir(&c.dir, &id) else { return };
    *screen(app) = ScreenState { meeting: Some(id.clone()), ..Default::default() };
    let frame = dir.join(".frame.png");
    let mut last: Option<Instant> = None;
    let mut empty_frames = 0;
    while !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(500));
        if !cfg!(target_os = "macos") || !c.settings.read().meeting_screen {
            set_status(app, &id, ScreenStatus::Off);
            continue;
        }
        let (window, paused, now) = {
            let mut s = screen(app);
            (s.window.clone(), s.paused, std::mem::take(&mut s.now))
        };
        let Some(window) = window else {
            set_status(app, &id, ScreenStatus::Choose);
            continue;
        };
        if paused {
            set_status(app, &id, ScreenStatus::Paused);
            continue;
        }
        if screen(app).status == ScreenStatus::SourceLost || (!now && last.is_some_and(|t| t.elapsed() < SCREEN_EVERY)) {
            continue;
        }
        if c.sessions.dictation_busy() {
            continue;
        }
        last = Some(Instant::now());
        if !platform::screen_permission() {
            set_status(app, &id, ScreenStatus::NeedsPermission);
            continue;
        }
        let _ = std::fs::create_dir_all(frame.parent().expect("in the meeting folder"));
        if let Err(e) = platform::capture_window(window.window_id, &frame) {
            log::info!("screen reading: {e:#}");
            set_status(app, &id, ScreenStatus::SourceLost);
            continue;
        }
        let started = Instant::now();
        let rows = match read_screen_rows(&c, &frame) {
            Ok(r) => r,
            Err(e) => {
                log::warn!("screen reading: {e:#}");
                continue;
            }
        };
        // No text at all, again and again, and no window titles: the pictures are wallpaper.
        empty_frames = if rows.is_empty() { empty_frames + 1 } else { 0 };
        if empty_frames >= 3 && titles_hidden(&sources()) {
            set_status(app, &id, ScreenStatus::NeedsPermission);
            continue;
        }
        set_status(app, &id, ScreenStatus::Reading);
        observe(app, meeting, &frame, &window, rows, started);
    }
    let _ = std::fs::remove_file(&frame);
    // A newer meeting's loop may have taken over the state already.
    {
        let mut s = screen(app);
        if s.meeting.as_deref() == Some(id.as_str()) {
            *s = ScreenState::default();
        }
    }
    emit_screen(app, &id);
}

/// Keeps a frame of the shared screen if it shows something new.
fn observe(app: &AppHandle, meeting: &Arc<Mutex<Meeting>>, frame: &Path, window: &ScreenWindow, rows: Vec<String>, started: Instant) {
    let c = app.state::<Core>();
    let (id, now_ms, verdict) = {
        let mut m = meeting.lock();
        let now_ms = m.duration_ms;
        let verdict = visuals::observe(&mut m.visuals, &rows, now_ms);
        (m.id.clone(), now_ms, verdict)
    };
    log::info!("screen reading: {} rows in {} ms, {verdict:?}", rows.len(), started.elapsed().as_millis());
    match verdict {
        visuals::Seen::New => {
            let image = match keep_picture(&c.dir, &id, frame) {
                Ok(i) => Some(i),
                Err(e) => {
                    log::warn!("keeping the screen's picture failed: {e:#}");
                    None
                }
            };
            let label = if window.title.is_empty() { window.app.clone() } else { format!("{}: {}", window.app, window.title) };
            let mut m = meeting.lock();
            let v = Visual { id: m.new_visual_id(), at_ms: now_ms, until_ms: Some(now_ms), source: visuals::Source::Screen, label, text: rows.join("\n"), image };
            m.visuals.push(v);
            save(&c.dir, &m);
            emit_visuals(app, &m);
        }
        _ => {}
    }
}

// ---------- photos arriving in Downloads ----------

/// Each meeting's watch has a number; a newer meeting's watch ends the older one.
static PHOTO_WATCH: AtomicU64 = AtomicU64::new(0);

/// Image files in a folder with their size.
fn images_in(dir: &Path) -> HashMap<PathBuf, u64> {
    let Ok(entries) = std::fs::read_dir(dir) else { return HashMap::new() };
    entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| is_image(p))
        .filter_map(|p| std::fs::metadata(&p).ok().filter(|m| m.is_file()).map(|m| (p, m.len())))
        .collect()
}

/// While meeting `id` records and for `PHOTO_WATCH_AFTER` after: images that newly arrive in
/// Downloads (and have stopped growing) are offered with `meeting-photo-offer`, once each. The
/// user adds or ignores them; nothing is added without asking.
pub fn watch_downloads(app: AppHandle, id: String) {
    let Some(downloads) = dirs::download_dir() else { return };
    let watch = PHOTO_WATCH.fetch_add(1, Ordering::SeqCst) + 1;
    let _ = thread::Builder::new().name("wordfall-photos".into()).spawn(move || {
        let there: HashSet<PathBuf> = images_in(&downloads).into_keys().collect();
        let mut offered: HashSet<PathBuf> = HashSet::new();
        let mut growing: HashMap<PathBuf, u64> = HashMap::new();
        let mut ended: Option<Instant> = None;
        loop {
            thread::sleep(PHOTO_POLL);
            if PHOTO_WATCH.load(Ordering::SeqCst) != watch {
                return;
            }
            let recording = app.state::<Core>().meetings.active().is_some_and(|a| a.id == id);
            if !recording {
                let since = *ended.get_or_insert_with(Instant::now);
                if since.elapsed() > PHOTO_WATCH_AFTER || get(&app, &id).is_err() {
                    return;
                }
            }
            if !app.state::<Core>().settings.read().meeting_photo_watch {
                continue;
            }
            for (path, size) in images_in(&downloads) {
                if there.contains(&path) || offered.contains(&path) {
                    continue;
                }
                // Offered once it has stopped growing (AirDrop writes it in pieces).
                if growing.insert(path.clone(), size) == Some(size) && size > 0 {
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    log::info!("offering a file from Downloads for meeting {id}");
                    let _ = app.emit("meeting-photo-offer", json!({ "id": id, "path": path, "name": name }));
                    offered.insert(path);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnails_sit_next_to_their_picture() {
        assert_eq!(thumb_of("visuals/ab.jpg"), "visuals/ab-thumb.jpg");
    }

    #[test]
    fn only_real_image_files_are_read() {
        let root = std::env::temp_dir().join(format!("wordfall-check-image-{}", std::process::id()));
        let downloads = root.join("Downloads");
        std::fs::create_dir_all(downloads.join("folder.png")).unwrap();
        let write = |p: &Path, bytes: &[u8]| std::fs::write(p, bytes).unwrap();
        let photo = downloads.join("IMG_2041.JPG");
        write(&photo, b"jpeg bytes");
        write(&root.join("secret.txt"), b"not for meetings");
        write(&root.join("elsewhere.png"), b"png bytes");
        write(&downloads.join("empty.png"), b"");
        assert_eq!(check_image(&photo, None).unwrap(), photo.canonicalize().unwrap());
        assert!(check_image(&root.join("secret.txt"), None).is_err(), "not an image");
        assert!(check_image(&downloads.join("folder.png"), None).is_err(), "a folder");
        assert!(check_image(&downloads.join("empty.png"), None).is_err(), "empty");
        assert!(check_image(&root.join("missing.png"), None).is_err(), "gone");
        #[cfg(unix)]
        {
            let link = downloads.join("innocent.png");
            std::os::unix::fs::symlink(root.join("secret.txt"), &link).unwrap();
            assert!(check_image(&link, None).is_err(), "a link to something that isn't an image");
        }
        // Offered from Downloads: it must be in there.
        assert!(check_image(&photo, Some(&downloads)).is_ok());
        assert!(check_image(&root.join("elsewhere.png"), Some(&downloads)).is_err());
        assert!(check_image(&downloads.join("../elsewhere.png"), Some(&downloads)).is_err(), "out of Downloads by ..");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn images_are_told_by_extension() {
        assert!(is_image(Path::new("/x/IMG_2041.HEIC")) && is_image(Path::new("a.jpeg")));
        assert!(!is_image(Path::new("a.pdf")) && !is_image(Path::new("a.jpg.download")));
    }

    /// `cargo test --lib reads_a_window -- --ignored --nocapture`: lists the windows, takes a
    /// picture of the first and reads it (needs Screen Recording for the terminal).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn reads_a_window() {
        let windows = sources();
        println!("permission: {}, {} windows, titles hidden: {}", platform::screen_permission(), windows.len(), titles_hidden(&windows));
        for w in windows.iter().take(8) {
            println!("  {} {} | {}", w.window_id, w.app, w.title);
        }
        let w = windows.first().expect("a window on screen");
        let frame = std::env::temp_dir().join("wordfall-window-test.png");
        let started = Instant::now();
        platform::capture_window(w.window_id, &frame).unwrap();
        let captured = started.elapsed().as_millis();
        let rows = read_file(&frame, &["de".into(), "en".into()], &[]).unwrap();
        println!("captured {} in {captured} ms, {} rows in {} ms", w.app, rows.len(), started.elapsed().as_millis() - captured);
        for r in rows.iter().take(12) {
            println!("  {r}");
        }
        let _ = std::fs::remove_file(frame);
    }

    #[test]
    fn hidden_titles_mean_no_permission() {
        let w = |t: &str| ScreenWindow { window_id: 1, app: "zoom.us".into(), title: t.into() };
        assert!(titles_hidden(&[w(""), w("")]));
        assert!(!titles_hidden(&[w(""), w("Zoom Meeting")]));
        assert!(!titles_hidden(&[]));
    }
}
