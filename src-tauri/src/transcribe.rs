//! Local Whisper models: catalog, discovery (app folder + user-added folders),
//! download, and a warm whisper.cpp engine (Metal).

use futures_util::StreamExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use tokio::io::AsyncWriteExt;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

pub struct CatalogEntry {
    pub id: &'static str,
    pub display_name: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    pub badge: &'static str,
    pub description: &'static str,
    pub english_only: bool,
}

pub const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        id: "large-v3-turbo-q8_0",
        display_name: "Large v3 Turbo (Q8)",
        file: "ggml-large-v3-turbo-q8_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin",
        bytes: 874_188_075,
        badge: "Best accuracy",
        description: "Most accurate in our benchmark (7.5% WER), same speed as Distil, half the download.",
        english_only: false,
    },
    CatalogEntry {
        id: "distil-large-v3.5",
        display_name: "Distil Large v3.5",
        file: "ggml-distil-large-v3.5.bin",
        url: "https://huggingface.co/distil-whisper/distil-large-v3.5-ggml/resolve/main/ggml-model.bin",
        bytes: 1_519_521_155,
        badge: "English only",
        description: "English specialist; smaller than full Whisper Large v3.",
        english_only: true,
    },
    CatalogEntry {
        id: "large-v3-turbo-q5_0",
        display_name: "Large v3 Turbo (Q5)",
        file: "ggml-large-v3-turbo-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
        bytes: 574_041_195,
        badge: "Multilingual",
        description: "Quantized Turbo: fast, small download, broad language coverage.",
        english_only: false,
    },
    CatalogEntry {
        id: "large-v3-turbo",
        display_name: "Large v3 Turbo",
        file: "ggml-large-v3-turbo.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        bytes: 1_624_555_275,
        badge: "Multilingual",
        description: "Compact Whisper model with broad language coverage.",
        english_only: false,
    },
    CatalogEntry {
        id: "medium.en",
        display_name: "Medium (English)",
        file: "ggml-medium.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin",
        bytes: 1_533_774_781,
        badge: "English only",
        description: "Solid English accuracy; slower than Distil.",
        english_only: true,
    },
    CatalogEntry {
        id: "base.en",
        display_name: "Base (English)",
        file: "ggml-base.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
        bytes: 147_964_211,
        badge: "Tiny & fast",
        description: "Very fast, lower accuracy. Good for older Macs.",
        english_only: true,
    },
    CatalogEntry {
        id: "large-v3",
        display_name: "Large v3",
        file: "ggml-large-v3.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin",
        bytes: 3_095_033_483,
        badge: "Full size",
        description: "Full multilingual Whisper model; needs more memory and time.",
        english_only: false,
    },
];

pub fn catalog_entry(id: &str) -> Option<&'static CatalogEntry> {
    CATALOG.iter().find(|e| e.id == id)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub size_mb: u64,
    pub downloaded: bool,
    pub path: Option<String>,
    pub source: Option<&'static str>,
    pub badge: String,
    pub description: String,
    pub english_only: bool,
    pub recommended: bool,
}

pub struct ModelDirs {
    pub own: PathBuf,
    /// Extra folders the user pointed us at (e.g. another app's model folder).
    pub extra: Vec<PathBuf>,
}

impl ModelDirs {
    /// Where a model lives on disk: the app's own folder first, then the extra folders.
    pub fn locate(&self, entry: &CatalogEntry) -> Option<(PathBuf, &'static str)> {
        let own = self.own.join(entry.file);
        if is_complete(&own, entry.bytes) {
            return Some((own, "app"));
        }
        self.extra
            .iter()
            .map(|d| d.join(entry.file))
            .find(|p| is_complete(p, entry.bytes))
            .map(|p| (p, "folder"))
    }

    /// The folder (or its `models/` subfolder) if it contains any catalog model.
    pub fn find_model_folder(dir: &Path) -> Option<PathBuf> {
        [dir.to_path_buf(), dir.join("models")]
            .into_iter()
            .find(|d| CATALOG.iter().any(|e| is_complete(&d.join(e.file), e.bytes)))
    }

    pub fn list(&self) -> Vec<ModelInfo> {
        CATALOG
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let found = self.locate(e);
                ModelInfo {
                    id: e.id.into(),
                    display_name: e.display_name.into(),
                    size_mb: e.bytes / 1_000_000,
                    downloaded: found.is_some(),
                    path: found.as_ref().map(|(p, _)| p.display().to_string()),
                    source: found.map(|(_, s)| s),
                    badge: e.badge.into(),
                    description: e.description.into(),
                    english_only: e.english_only,
                    recommended: i == 0,
                }
            })
            .collect()
    }
}

fn is_complete(path: &Path, expected: u64) -> bool {
    std::fs::metadata(path).map(|m| m.len() == expected).unwrap_or(false)
}

/// Streams a model to `<own>/<file>.part`, then renames it into place.
pub async fn download(
    entry: &'static CatalogEntry,
    own_dir: &Path,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    mut progress: impl FnMut(u64, u64),
) -> anyhow::Result<PathBuf> {
    tokio::fs::create_dir_all(own_dir).await?;
    let dest = own_dir.join(entry.file);
    let part = own_dir.join(format!("{}.part", entry.file));
    let resp = reqwest::Client::new().get(entry.url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(entry.bytes);
    let mut file = tokio::fs::File::create(&part).await?;
    let mut stream = resp.bytes_stream();
    let mut done = 0u64;
    let mut last_emit = Instant::now();
    while let Some(chunk) = stream.next().await {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            drop(file);
            let _ = tokio::fs::remove_file(&part).await;
            anyhow::bail!("download cancelled");
        }
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        if last_emit.elapsed().as_millis() > 150 {
            progress(done, total);
            last_emit = Instant::now();
        }
    }
    file.flush().await?;
    drop(file);
    if done != entry.bytes {
        let _ = tokio::fs::remove_file(&part).await;
        anyhow::bail!("download incomplete ({done} of {} bytes)", entry.bytes);
    }
    tokio::fs::rename(&part, &dest).await?;
    progress(done, total);
    Ok(dest)
}

/// Decoder knobs shared by the app and the benchmark harness (`wordfall bench`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DecodeOptions {
    /// 1 = greedy; >1 = beam search with this width.
    pub beam_size: i32,
    /// CPU threads for whisper.cpp; 0 = auto (P-cores).
    pub threads: i32,
    /// `full` encodes the fixed 30 s window; `dynamic` sizes the encoder context to the clip.
    pub audio_ctx: AudioCtx,
    /// Extra encoder context beyond the clip length when `audio_ctx = dynamic`.
    pub audio_ctx_margin_ms: u32,
    pub no_timestamps: bool,
    /// Re-decode at higher temperature when the output looks degenerate.
    pub temperature_fallback: bool,
    /// Trim/skip non-speech before decoding.
    pub vad: Vad,
    /// Clips shorter than this are padded with silence.
    pub min_audio_ms: u32,
    /// Segments with a higher no-speech probability are dropped.
    pub no_speech_skip: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCtx {
    Full,
    Dynamic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vad {
    None,
    /// Cheap RMS-based trim of leading/trailing silence; empty clips are skipped.
    Energy,
    /// whisper.cpp's built-in Silero VAD (needs `ggml-silero-v5.1.2.bin`).
    Silero,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            beam_size: 1,
            threads: 8,
            audio_ctx: AudioCtx::Full,
            audio_ctx_margin_ms: 1000,
            no_timestamps: false,
            temperature_fallback: true,
            vad: Vad::None,
            min_audio_ms: 1250,
            no_speech_skip: 0.85,
        }
    }
}

pub const SILERO_FILE: &str = "ggml-silero-v5.1.2.bin";
pub const SILERO_URL: &str = "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";
const SILERO_BYTES: u64 = 885_098;

/// Ensures the Silero VAD model is in `models_dir`, downloading it (885 KB) if needed.
pub async fn ensure_silero(models_dir: &Path) -> anyhow::Result<PathBuf> {
    let path = models_dir.join(SILERO_FILE);
    if is_complete(&path, SILERO_BYTES) {
        return Ok(path);
    }
    tokio::fs::create_dir_all(models_dir).await?;
    let bytes = reqwest::get(SILERO_URL).await?.error_for_status()?.bytes().await?;
    anyhow::ensure!(bytes.len() as u64 == SILERO_BYTES, "unexpected Silero model size {}", bytes.len());
    let part = path.with_extension("bin.part");
    tokio::fs::write(&part, &bytes).await?;
    tokio::fs::rename(&part, &path).await?;
    Ok(path)
}

struct Loaded {
    id: String,
    english_only: bool,
    // `state` borrows nothing from `ctx` at the type level but must not outlive it;
    // field order guarantees state drops first.
    state: WhisperState,
    /// Encoder context the state was last used with. whisper.cpp keeps buffers sized for it,
    /// and switching sizes on a live state corrupts later runs, so a change means a new state.
    state_audio_ctx: i32,
    /// Separate state for language detection, pinned to a short encoder window (created on first use).
    detect: Option<WhisperState>,
    ctx: WhisperContext,
}

/// Encoder frames used for language detection: about 5 s of audio, a sixth of the full window.
const DETECT_AUDIO_CTX: i32 = 256;

impl Loaded {
    /// Probability per language id for the start of `audio`.
    fn ensure_detect_state(&mut self) -> anyhow::Result<()> {
        if self.detect.is_none() {
            // A fresh state takes its encoder size from its first full run and keeps it, because
            // `lang_detect` never changes it. One short silent run pins it to the small window.
            let mut state = self.ctx.create_state().map_err(|e| anyhow::anyhow!("whisper state: {e:?}"))?;
            let mut warm = full_params(&DecodeOptions::default(), Some("en"), None, DETECT_AUDIO_CTX);
            warm.set_no_speech_thold(1.0);
            warm.set_max_len(1);
            state.full(warm, &vec![0.0f32; 16_000]).map_err(|e| anyhow::anyhow!("language detection: {e:?}"))?;
            self.detect = Some(state);
        }
        Ok(())
    }

    fn language_probs(&mut self, audio: &[f32], threads: usize) -> anyhow::Result<Vec<f32>> {
        self.ensure_detect_state()?;
        let state = self.detect.as_mut().expect("created above");
        state.pcm_to_mel(audio, threads).map_err(|e| anyhow::anyhow!("language detection: {e:?}"))?;
        let (_, probs) = state.lang_detect(0, threads).map_err(|e| anyhow::anyhow!("language detection: {e:?}"))?;
        Ok(probs)
    }
}

#[derive(Default)]
pub struct Engine {
    loaded: Mutex<Option<Loaded>>,
    /// Path to the Silero VAD model, used when `DecodeOptions::vad == Silero`.
    pub silero_path: Mutex<Option<PathBuf>>,
    silero: Mutex<Option<whisper_rs::WhisperVadContext>>,
    /// Language of the previous dictation; ambiguous clips stay in it.
    last_language: Mutex<Option<String>>,
}

pub struct Transcript {
    pub text: String,
    pub elapsed_ms: u64,
    /// Mean token probability of the kept segments (0..1), a rough confidence signal.
    pub confidence: f32,
    /// Audio actually sent to whisper after VAD trimming, in ms.
    pub decoded_audio_ms: u64,
    /// Language the audio was decoded as (ISO 639-1, e.g. "de"); `None` when nothing was decoded.
    pub language: Option<String>,
}

impl Transcript {
    fn empty(started: Instant) -> Self {
        Self { text: String::new(), elapsed_ms: started.elapsed().as_millis() as u64, confidence: 0.0, decoded_audio_ms: 0, language: None }
    }
}

impl Engine {
    pub fn loaded_model(&self) -> Option<String> {
        self.loaded.lock().as_ref().map(|l| l.id.clone())
    }

    pub fn unload(&self) {
        *self.loaded.lock() = None;
    }

    /// Sets up language detection ahead of the first multi-language dictation (no-op if done).
    pub fn prepare_detection(&self) -> anyhow::Result<()> {
        match self.loaded.lock().as_mut() {
            Some(l) if !l.english_only => l.ensure_detect_state(),
            _ => Ok(()),
        }
    }

    pub fn load(&self, entry: &CatalogEntry, path: &Path) -> anyhow::Result<()> {
        self.load_file(entry.id, entry.english_only, path)
    }

    /// Loads any ggml whisper file (e.g. a quantized variant) under the given id.
    pub fn load_file(&self, id: &str, english_only: bool, path: &Path) -> anyhow::Result<()> {
        if self.loaded_model().as_deref() == Some(id) {
            return Ok(());
        }
        let started = Instant::now();
        let mut params = WhisperContextParameters::default();
        params.use_gpu = true;
        params.flash_attn = true;
        let ctx = WhisperContext::new_with_params(path, params)
            .map_err(|e| anyhow::anyhow!("could not load {id}: {e:?}"))?;
        let mut state = ctx.create_state().map_err(|e| anyhow::anyhow!("whisper state: {e:?}"))?;
        // Warm-up pass so the first real dictation doesn't pay for Metal shader compilation.
        let silence = vec![0.0f32; 16_000];
        let opts = DecodeOptions::default();
        let mut warm = full_params(&opts, Some("en"), None, 0);
        warm.set_no_speech_thold(1.0);
        let _ = state.full(warm, &silence);
        *self.loaded.lock() = Some(Loaded { id: id.to_string(), english_only, state, state_audio_ctx: 0, detect: None, ctx });
        log::info!("loaded {id} in {:?}", started.elapsed());
        Ok(())
    }

    /// `languages` are the ones the user speaks: empty = any (whisper detects), one = forced,
    /// several = whichever of them the audio sounds most like.
    pub fn transcribe(
        &self,
        audio: &[f32],
        languages: &[String],
        prompt: Option<&str>,
        opts: &DecodeOptions,
    ) -> anyhow::Result<Transcript> {
        let started = Instant::now();
        let mut guard = self.loaded.lock();
        let loaded = guard.as_mut().ok_or_else(|| anyhow::anyhow!("no model loaded"))?;

        let trimmed: Vec<f32>;
        let speech: Vec<f32>;
        // Fall back to the energy gate until the Silero model has been downloaded.
        let vad = if opts.vad == Vad::Silero && self.silero_path.lock().is_none() { Vad::Energy } else { opts.vad };
        let audio = match vad {
            Vad::Silero => match self.silero_speech(audio)? {
                Some(s) => {
                    speech = s;
                    &speech[..]
                }
                None => return Ok(Transcript::empty(started)),
            },
            Vad::Energy => match crate::vad::trim(audio) {
                Some(range) => {
                    trimmed = audio[range].to_vec();
                    &trimmed[..]
                }
                None => return Ok(Transcript::empty(started)),
            },
            _ => audio,
        };
        // Whisper behaves poorly on very short clips; pad with silence.
        let min = (opts.min_audio_ms as usize) * 16;
        let padded: Vec<f32>;
        let audio = if audio.len() < min {
            padded = audio.iter().copied().chain(std::iter::repeat(0.0).take(min - audio.len())).collect();
            &padded[..]
        } else {
            audio
        };

        let audio_ctx = audio_ctx_for(opts, audio.len());
        if audio_ctx != loaded.state_audio_ctx {
            loaded.state = loaded.ctx.create_state().map_err(|e| anyhow::anyhow!("whisper state: {e:?}"))?;
            loaded.state_audio_ctx = audio_ctx;
        }
        let lang: String = match languages {
            _ if loaded.english_only => "en".into(),
            [] => "auto".into(),
            [one] => one.clone(),
            several => {
                // Like whisper's own "auto", but on a short window and only the user's languages compete.
                let probs = loaded.language_probs(audio, thread_count(opts) as usize)?;
                let prob = |code: &str| whisper_rs::get_lang_id(code).and_then(|id| probs.get(id as usize).copied()).unwrap_or(0.0);
                let previous = self.last_language.lock().clone();
                let chosen = pick_language(several, previous.as_deref(), prob).to_string();
                log::info!(
                    "language {chosen} ({})",
                    several.iter().map(|l| format!("{l} {:.2}", prob(l))).collect::<Vec<_>>().join(", ")
                );
                chosen
            }
        };
        let params = full_params(opts, Some(lang.as_str()), prompt, audio_ctx);
        loaded.state.full(params, audio).map_err(|e| anyhow::anyhow!("transcription failed: {e:?}"))?;
        let language = if lang == "auto" {
            whisper_rs::get_lang_str(loaded.state.full_lang_id_from_state()).map(str::to_string)
        } else {
            Some(lang)
        };
        if language.is_some() {
            self.last_language.lock().clone_from(&language);
        }

        let mut text = String::new();
        let (mut prob_sum, mut prob_n) = (0.0f32, 0usize);
        for seg in loaded.state.as_iter() {
            if seg.no_speech_probability() > opts.no_speech_skip {
                continue;
            }
            if let Ok(s) = seg.to_str_lossy() {
                text.push_str(&s);
            }
            for i in 0..seg.n_tokens() {
                if let Some(tok) = seg.get_token(i) {
                    // Skip special/timestamp tokens (their text starts with "[_" or "<|").
                    let is_special = tok.to_str_lossy().map(|t| t.starts_with("[_") || t.starts_with("<|")).unwrap_or(true);
                    if !is_special {
                        prob_sum += tok.token_probability();
                        prob_n += 1;
                    }
                }
            }
        }
        Ok(Transcript {
            text: text.trim().to_string(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            confidence: if prob_n > 0 { prob_sum / prob_n as f32 } else { 0.0 },
            decoded_audio_ms: (audio.len() / 16) as u64,
            language,
        })
    }
}

impl Engine {
    /// Speech-only audio according to Silero VAD (segments joined with short silences),
    /// or `None` if there's no speech. whisper.cpp only applies its built-in VAD in
    /// `whisper_full()`, not with an external state, so we run it ourselves.
    fn silero_speech(&self, audio: &[f32]) -> anyhow::Result<Option<Vec<f32>>> {
        let mut guard = self.silero.lock();
        if guard.is_none() {
            let path = self.silero_path.lock().clone().ok_or_else(|| anyhow::anyhow!("Silero VAD model not available"))?;
            let mut cp = whisper_rs::WhisperVadContextParams::new();
            cp.set_n_threads(2);
            cp.set_use_gpu(false);
            let vctx = whisper_rs::WhisperVadContext::new(&path.display().to_string(), cp)
                .map_err(|e| anyhow::anyhow!("Silero VAD: {e:?}"))?;
            *guard = Some(vctx);
        }
        let vctx = guard.as_mut().unwrap();
        let mut vp = whisper_rs::WhisperVadParams::new();
        vp.set_speech_pad(200);
        vp.set_min_silence_duration(300);
        let segments = vctx.segments_from_samples(vp, audio).map_err(|e| anyhow::anyhow!("Silero VAD: {e:?}"))?;
        let gap = vec![0.0f32; 1_600];
        let mut out = Vec::with_capacity(audio.len());
        for i in 0..segments.num_segments() {
            let Some(seg) = segments.get_segment(i) else { continue };
            let start = ((seg.start * 160.0) as usize).min(audio.len());
            let end = ((seg.end * 160.0) as usize).min(audio.len());
            if end > start {
                if !out.is_empty() {
                    out.extend_from_slice(&gap);
                }
                out.extend_from_slice(&audio[start..end]);
            }
        }
        Ok((!out.is_empty()).then_some(out))
    }
}

/// Encoder frames per second of audio (whisper: 1500 frames per 30 s window).
const CTX_PER_SECOND: f32 = 50.0;

/// Encoder context for a clip: 0 = whisper's full 30 s window.
fn audio_ctx_for(opts: &DecodeOptions, samples: usize) -> i32 {
    let seconds = samples as f32 / 16_000.0;
    if opts.audio_ctx != AudioCtx::Dynamic || seconds > 30.0 {
        return 0;
    }
    let frames = (seconds + opts.audio_ctx_margin_ms as f32 / 1000.0) * CTX_PER_SECOND;
    // Round up to a multiple of 64 frames; tiny contexts make whisper unstable.
    ((frames / 64.0).ceil() * 64.0).clamp(128.0, 1500.0) as i32
}

fn thread_count(opts: &DecodeOptions) -> i32 {
    if opts.threads > 0 {
        opts.threads
    } else {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(4) as i32
    }
}

/// The most likely of `allowed`, except that an ambiguous clip (e.g. a one-word "OK")
/// stays in the previous dictation's language: switching needs twice its probability.
pub fn pick_language<'a>(allowed: &'a [String], previous: Option<&str>, prob: impl Fn(&str) -> f32) -> &'a str {
    let best = allowed
        .iter()
        .max_by(|a, b| prob(a).total_cmp(&prob(b)))
        .map(String::as_str)
        .unwrap_or("en");
    match previous.and_then(|p| allowed.iter().find(|l| *l == p)) {
        Some(prev) if prob(prev) * 2.0 >= prob(best) => prev,
        _ => best,
    }
}

fn full_params<'a, 'b>(opts: &DecodeOptions, lang: Option<&'a str>, prompt: Option<&str>, audio_ctx: i32) -> FullParams<'a, 'b> {
    let strategy = if opts.beam_size > 1 {
        SamplingStrategy::BeamSearch { beam_size: opts.beam_size, patience: -1.0 }
    } else {
        SamplingStrategy::Greedy { best_of: 1 }
    };
    let mut p = FullParams::new(strategy);
    p.set_n_threads(thread_count(opts));
    p.set_translate(false);
    p.set_language(lang);
    p.set_no_context(true);
    p.set_single_segment(false);
    p.set_no_timestamps(opts.no_timestamps);
    p.set_print_special(false);
    p.set_print_progress(false);
    p.set_print_realtime(false);
    p.set_print_timestamps(false);
    p.set_suppress_blank(true);
    p.set_suppress_nst(true);
    p.set_temperature(0.0);
    if !opts.temperature_fallback {
        p.set_temperature_inc(0.0);
    }
    p.set_audio_ctx(audio_ctx);
    if let Some(prompt) = prompt.filter(|p| !p.is_empty()) {
        p.set_initial_prompt(prompt);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn langs(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|c| c.to_string()).collect()
    }

    fn probs(pairs: &'static [(&'static str, f32)]) -> impl Fn(&str) -> f32 {
        move |code| pairs.iter().find(|(c, _)| *c == code).map(|(_, p)| *p).unwrap_or(0.0)
    }

    #[test]
    fn picks_most_likely_allowed_language() {
        // Dutch scores highest but isn't one of the user's languages.
        let allowed = langs(&["en", "de"]);
        assert_eq!(pick_language(&allowed, None, probs(&[("nl", 0.6), ("de", 0.3), ("en", 0.1)])), "de");
    }

    #[test]
    fn clear_switch_leaves_previous_language() {
        let allowed = langs(&["en", "de"]);
        assert_eq!(pick_language(&allowed, Some("en"), probs(&[("de", 0.95), ("en", 0.03)])), "de");
    }

    #[test]
    fn ambiguous_clip_keeps_previous_language() {
        let allowed = langs(&["en", "de"]);
        assert_eq!(pick_language(&allowed, Some("de"), probs(&[("en", 0.5), ("de", 0.3)])), "de");
    }

    #[test]
    fn previous_language_outside_the_set_is_ignored() {
        let allowed = langs(&["en", "de"]);
        assert_eq!(pick_language(&allowed, Some("fr"), probs(&[("en", 0.4), ("de", 0.5)])), "de");
    }
}
