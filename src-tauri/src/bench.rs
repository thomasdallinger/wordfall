//! `wordfall bench`: scores engines/configs on accuracy and latency over the benchmark
//! corpora, using the production audio pipeline and post-processing.
//! See `bench/README.md` for corpus layout, config format and the external engine protocol.

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::Instant,
};

use crate::{
    audio, metrics, postprocess,
    store::{DictEntry, Store},
    transcribe::{catalog_entry, DecodeOptions, Engine, Vad},
    vocab::{self, PromptMode},
};

const SPEECH_BUCKETS: &[(&str, f32, f32)] = &[("≤3s", 0.0, 3.0), ("3–15s", 3.0, 15.0), ("15–45s", 15.0, 45.0), (">45s", 45.0, 1e9)];

// ---------------------------------------------------------------- corpus

pub struct Clip {
    pub id: String,
    pub tier: String,
    pub reference: String,
    pub terms: Vec<String>,
    pub tags: Vec<String>,
    pub samples: Vec<f32>,
}

impl Clip {
    fn seconds(&self) -> f32 {
        self.samples.len() as f32 / 16_000.0
    }
}

#[derive(Deserialize)]
struct ClipMeta {
    #[serde(default)]
    reference: String,
    #[serde(default)]
    terms: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
}

fn load_tier_dir(dir: &Path, tier: &str, out: &mut Vec<Clip>) -> anyhow::Result<()> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "wav"))
        .collect();
    paths.sort();
    for wav in paths {
        let meta: ClipMeta = match std::fs::read(wav.with_extension("json")) {
            Ok(b) => serde_json::from_slice(&b).with_context(|| format!("bad metadata for {}", wav.display()))?,
            Err(_) => continue,
        };
        out.push(Clip {
            id: wav.file_stem().unwrap().to_string_lossy().to_string(),
            tier: tier.to_string(),
            reference: meta.reference,
            terms: meta.terms,
            tags: meta.tags,
            samples: audio::read_wav_16k(&wav)?,
        });
    }
    Ok(())
}

/// Real dictations whose transcript the user corrected or verified in History.
fn load_history_tier(data_dir: &Path, out: &mut Vec<Clip>) -> anyhow::Result<()> {
    let store = Store::load(data_dir);
    for r in store.all_records() {
        let Some(reference) = r.reference.clone() else { continue };
        let wav = data_dir.join("audio").join(format!("{}.wav", r.id));
        if !wav.exists() {
            continue;
        }
        out.push(Clip {
            id: format!("history-{}", &r.id[..8.min(r.id.len())]),
            tier: "history".into(),
            reference,
            terms: Vec::new(),
            tags: vec!["real".into()],
            samples: audio::read_wav_16k(&wav)?,
        });
    }
    Ok(())
}

pub fn load_corpus(root: &Path, data_dir: &Path, tiers: &[String]) -> anyhow::Result<Vec<Clip>> {
    let mut clips = Vec::new();
    let wanted = |t: &str| tiers.is_empty() || tiers.iter().any(|x| x == t);
    if let Ok(entries) = std::fs::read_dir(root) {
        let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        dirs.sort();
        for d in dirs {
            let tier = d.file_name().unwrap().to_string_lossy().to_string();
            if tier != "results" && wanted(&tier) {
                load_tier_dir(&d, &tier, &mut clips)?;
            }
        }
    }
    if wanted("history") {
        load_history_tier(data_dir, &mut clips)?;
    }
    Ok(clips)
}

// ---------------------------------------------------------------- configs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchConfig {
    pub name: String,
    #[serde(default = "default_engine")]
    pub engine: String,
    #[serde(default)]
    pub model: Option<String>,
    /// Load an arbitrary ggml file (e.g. a quantized variant) instead of a catalog model.
    #[serde(default)]
    pub model_path: Option<PathBuf>,
    #[serde(default = "default_prompt")]
    pub prompt: PromptMode,
    /// Force a language for multilingual models (None = auto-detect).
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default = "default_true")]
    pub postprocess: bool,
    /// External engine command (see protocol in bench/README.md).
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(flatten)]
    pub decode: DecodeOptions,
}

fn default_engine() -> String {
    "whisper".into()
}
fn default_prompt() -> PromptMode {
    PromptMode::None
}
fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct ConfigFile {
    #[serde(default)]
    defaults: toml::Table,
    config: Vec<toml::Table>,
}

pub fn load_configs(path: &Path) -> anyhow::Result<Vec<BenchConfig>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: ConfigFile = toml::from_str(&text)?;
    let mut known: Vec<String> = ["name", "engine", "model", "model_path", "prompt", "language", "postprocess", "command"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if let Value::Object(m) = serde_json::to_value(DecodeOptions::default())? {
        known.extend(m.keys().cloned());
    }
    let mut out = Vec::new();
    for table in file.config {
        let mut merged = file.defaults.clone();
        merged.extend(table);
        if let Some(bad) = merged.keys().find(|k| !known.contains(k)) {
            bail!("unknown config key `{bad}` (known: {})", known.join(", "));
        }
        let cfg: BenchConfig = merged.try_into().context("invalid config")?;
        out.push(cfg);
    }
    Ok(out)
}

// ---------------------------------------------------------------- engines

struct External {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl External {
    fn start(cmd: &[String]) -> anyhow::Result<(Self, u64)> {
        let started = Instant::now();
        let mut child = Command::new(&cmd[0])
            .args(&cmd[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("starting {}", cmd.join(" ")))?;
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            if stdout.read_line(&mut line)? == 0 {
                bail!("engine exited before becoming ready");
            }
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v["ready"].as_bool() == Some(true) {
                    let load = v["loadMs"].as_u64().unwrap_or(started.elapsed().as_millis() as u64);
                    return Ok((Self { child, stdin, stdout }, load));
                }
            }
        }
    }

    fn transcribe(&mut self, id: &str, wav: &Path, context: &[String]) -> anyhow::Result<(String, u64)> {
        writeln!(self.stdin, "{}", json!({ "id": id, "wav": wav, "context": context }))?;
        self.stdin.flush()?;
        let mut line = String::new();
        loop {
            line.clear();
            if self.stdout.read_line(&mut line)? == 0 {
                bail!("engine exited");
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if v["id"].as_str() != Some(id) {
                continue;
            }
            if let Some(e) = v["error"].as_str() {
                bail!("engine error: {e}");
            }
            return Ok((v["text"].as_str().unwrap_or("").to_string(), v["ms"].as_u64().unwrap_or(0)));
        }
    }
}

impl Drop for External {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------- results

#[derive(Debug, Clone, Serialize)]
pub struct ClipResult {
    pub config: String,
    pub clip: String,
    pub tier: String,
    pub tags: Vec<String>,
    pub seconds: f32,
    pub reference: String,
    pub raw: String,
    pub hypothesis: String,
    pub ms: u64,
    pub confidence: Option<f32>,
    pub score: metrics::Score,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub config: String,
    pub load_ms: u64,
    pub clips: usize,
    pub wer: f64,
    pub cer: f64,
    pub format_error: f64,
    pub jargon_recall: f64,
    pub hallucination_rate: f64,
    pub wer_by_tier: BTreeMap<String, f64>,
    pub latency_p50: u64,
    pub latency_p90: u64,
    pub latency_p95: u64,
    pub latency_max: u64,
    pub latency_by_bucket: BTreeMap<String, u64>,
    pub rtf: f64,
    /// Paired bootstrap 95% CI of WER difference vs the first config (negative = better).
    pub wer_delta_ci: Option<(f64, f64)>,
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn summarize(name: &str, load_ms: u64, results: &[ClipResult], baseline: Option<&[ClipResult]>) -> Summary {
    let speech: Vec<&ClipResult> = results.iter().filter(|r| r.tier != "noise").collect();
    let noise: Vec<&ClipResult> = results.iter().filter(|r| r.tier == "noise").collect();
    let sum = |f: fn(&metrics::Score) -> usize, rs: &[&ClipResult]| rs.iter().map(|r| f(&r.score)).sum::<usize>();
    let mut by_tier: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for r in &speech {
        let e = by_tier.entry(r.tier.clone()).or_default();
        e.0 += r.score.word_errors;
        e.1 += r.score.ref_words;
    }
    let mut lat: Vec<u64> = speech.iter().map(|r| r.ms).collect();
    lat.sort_unstable();
    let mut by_bucket = BTreeMap::new();
    for (label, lo, hi) in SPEECH_BUCKETS {
        let mut b: Vec<u64> = speech.iter().filter(|r| r.seconds > *lo && r.seconds <= *hi).map(|r| r.ms).collect();
        b.sort_unstable();
        if !b.is_empty() {
            by_bucket.insert(label.to_string(), percentile(&b, 0.5));
        }
    }
    let audio_s: f64 = speech.iter().map(|r| r.seconds as f64).sum();
    let wer_delta_ci = baseline.map(|base| bootstrap_delta(base, results));
    Summary {
        config: name.to_string(),
        load_ms,
        clips: results.len(),
        wer: metrics::ratio(sum(|s| s.word_errors, &speech), sum(|s| s.ref_words, &speech)),
        cer: metrics::ratio(sum(|s| s.char_errors, &speech), sum(|s| s.ref_chars, &speech)),
        format_error: metrics::ratio(sum(|s| s.format_errors, &speech), sum(|s| s.format_words, &speech)),
        jargon_recall: {
            let total = sum(|s| s.terms_total, &speech);
            if total == 0 { 1.0 } else { sum(|s| s.terms_hit, &speech) as f64 / total as f64 }
        },
        hallucination_rate: if noise.is_empty() {
            0.0
        } else {
            noise.iter().filter(|r| !r.hypothesis.trim().is_empty()).count() as f64 / noise.len() as f64
        },
        wer_by_tier: by_tier.into_iter().map(|(k, (e, n))| (k, metrics::ratio(e, n))).collect(),
        latency_p50: percentile(&lat, 0.5),
        latency_p90: percentile(&lat, 0.9),
        latency_p95: percentile(&lat, 0.95),
        latency_max: lat.last().copied().unwrap_or(0),
        latency_by_bucket: by_bucket,
        rtf: if audio_s > 0.0 { lat.iter().sum::<u64>() as f64 / 1000.0 / audio_s } else { 0.0 },
        wer_delta_ci,
    }
}

/// Paired bootstrap over clips: distribution of WER(candidate) − WER(baseline).
fn bootstrap_delta(base: &[ClipResult], cand: &[ClipResult]) -> (f64, f64) {
    let base_by: HashMap<&str, &ClipResult> = base.iter().map(|r| (r.clip.as_str(), r)).collect();
    let pairs: Vec<(usize, usize, usize)> = cand
        .iter()
        .filter(|r| r.tier != "noise")
        .filter_map(|r| base_by.get(r.clip.as_str()).map(|b| (b.score.word_errors, r.score.word_errors, r.score.ref_words)))
        .collect();
    if pairs.is_empty() {
        return (0.0, 0.0);
    }
    let mut seed: u64 = 0x9e3779b97f4a7c15;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut deltas: Vec<f64> = (0..2000)
        .map(|_| {
            let (mut eb, mut ec, mut n) = (0, 0, 0);
            for _ in 0..pairs.len() {
                let (b, c, w) = pairs[(next() % pairs.len() as u64) as usize];
                eb += b;
                ec += c;
                n += w;
            }
            metrics::ratio(ec, n) - metrics::ratio(eb, n)
        })
        .collect();
    deltas.sort_by(|a, b| a.total_cmp(b));
    (deltas[50], deltas[1949])
}

// ---------------------------------------------------------------- runner

struct Args {
    root: PathBuf,
    config: PathBuf,
    tiers: Vec<String>,
    runs: usize,
    out: Option<PathBuf>,
    only: Vec<String>,
    filter: Option<String>,
}

fn parse_args(args: &[String], data_dir: &Path) -> anyhow::Result<Args> {
    let mut a = Args {
        root: data_dir.join("bench"),
        config: PathBuf::from("bench/configs/default.toml"),
        tiers: Vec::new(),
        runs: 1,
        out: None,
        only: Vec::new(),
        filter: None,
    };
    let mut it = args.iter().skip_while(|s| *s != "bench").skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| anyhow::anyhow!("{flag} needs a value"));
        match flag.as_str() {
            "--root" => a.root = PathBuf::from(val()?),
            "--config" => a.config = PathBuf::from(val()?),
            "--tier" => a.tiers.push(val()?),
            "--runs" => a.runs = val()?.parse()?,
            "--out" => a.out = Some(PathBuf::from(val()?)),
            "--only" => a.only.extend(val()?.split(',').map(String::from)),
            "--filter" => a.filter = Some(val()?),
            other => bail!("unknown flag {other}"),
        }
    }
    Ok(a)
}

fn ensure_silero(models_dir: &Path) -> anyhow::Result<PathBuf> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(crate::transcribe::ensure_silero(models_dir))
}

pub fn run_cli(args: &[String]) -> anyhow::Result<()> {
    let data_dir = dirs::data_dir().context("no data dir")?.join("com.wordfall.app");
    let a = parse_args(args, &data_dir)?;
    let mut configs = load_configs(&a.config)?;
    if !a.only.is_empty() {
        configs.retain(|c| a.only.contains(&c.name));
    }
    let mut clips = load_corpus(&a.root, &data_dir, &a.tiers)?;
    if let Some(f) = &a.filter {
        clips.retain(|c| c.id.contains(f.as_str()));
    }
    if clips.is_empty() {
        bail!("no clips found under {} (run `node bench/make-corpus.mjs`)", a.root.display());
    }
    let tiers: BTreeMap<&str, usize> = clips.iter().fold(BTreeMap::new(), |mut m, c| {
        *m.entry(c.tier.as_str()).or_default() += 1;
        m
    });
    eprintln!("{} clips {:?} × {} configs", clips.len(), tiers, configs.len());

    let store = Store::load(&data_dir);
    let dictionary: Vec<DictEntry> = store.dictionary.clone();
    let dict_terms: Vec<String> = dictionary.iter().map(|e| e.replacement.clone()).collect();
    let user_terms: Vec<String> = store.vocabulary.clone();
    let projects = dirs::home_dir().map(|h| vocab::project_names(&h)).unwrap_or_default();
    let model_dirs = crate::settings::Settings::load(&data_dir).model_dirs(&data_dir);

    let out_dir = a.out.clone().unwrap_or_else(|| a.root.join("results").join(chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()));
    std::fs::create_dir_all(&out_dir)?;
    let mut jsonl = std::fs::File::create(out_dir.join("results.jsonl"))?;

    // Clips must exist as files for external engines; corpus clips already do, history clips get temp copies.
    let tmp = out_dir.join("tmp-wav");
    let engine = Engine::default();
    let mut all: Vec<(Summary, Vec<ClipResult>)> = Vec::new();

    for cfg in &configs {
        let terms = vocab::terms(cfg.prompt, &vocab::Sources { dictionary: &dict_terms, user_terms: &user_terms, projects: &projects });
        let prompt = vocab::prompt(&terms);
        let mut results = Vec::new();
        let load_ms;
        let mut external = None;

        match cfg.engine.as_str() {
            "whisper" => {
                let started = Instant::now();
                let model_path = cfg.model_path.as_ref().map(|p| match p.strip_prefix("~") {
                    Ok(rest) => dirs::home_dir().unwrap_or_default().join(rest),
                    Err(_) => p.clone(),
                });
                let (id, english_only, path) = match (&model_path, &cfg.model) {
                    (Some(p), m) => {
                        let id = m.clone().unwrap_or_else(|| p.file_stem().unwrap().to_string_lossy().to_string());
                        let english = catalog_entry(&id).map(|e| e.english_only).unwrap_or(true);
                        (format!("{id}@{}", p.display()), english, p.clone())
                    }
                    (None, m) => {
                        let id = m.clone().unwrap_or_else(|| "distil-large-v3.5".into());
                        let entry = catalog_entry(&id).with_context(|| format!("unknown model {id}"))?;
                        let (path, _) = model_dirs.locate(entry).with_context(|| format!("{id} is not downloaded"))?;
                        (id, entry.english_only, path)
                    }
                };
                if engine.loaded_model().as_deref() != Some(id.as_str()) {
                    engine.unload();
                    engine.load_file(&id, english_only, &path)?;
                }
                if cfg.decode.vad == Vad::Silero {
                    *engine.silero_path.lock() = Some(ensure_silero(&data_dir.join("models"))?);
                }
                load_ms = started.elapsed().as_millis() as u64;
            }
            "external" => {
                let cmd = cfg.command.as_ref().context("external engine needs `command`")?;
                let (ext, ms) = External::start(cmd)?;
                external = Some(ext);
                load_ms = ms;
            }
            other => bail!("unknown engine {other}"),
        }

        eprintln!("▶ {} (load {load_ms} ms, {} prompt terms)", cfg.name, terms.len());
        for (i, clip) in clips.iter().enumerate() {
            let (raw, ms, confidence) = if let Some(ext) = external.as_mut() {
                let wav = clip_path(&a.root, &tmp, clip)?;
                let mut times = Vec::new();
                let mut text = String::new();
                for run in 0..a.runs {
                    let (t, ms) = ext.transcribe(&clip.id, &wav, &terms)?;
                    if run == 0 {
                        text = t;
                    }
                    times.push(ms);
                }
                times.sort_unstable();
                (text, times[times.len() / 2], None)
            } else {
                let mut times = Vec::new();
                let mut first = None;
                for _ in 0..a.runs {
                    let t = engine.transcribe(&clip.samples, cfg.language.as_slice(), prompt.as_deref(), &cfg.decode)?;
                    times.push(t.elapsed_ms);
                    first.get_or_insert(t);
                }
                times.sort_unstable();
                let t = first.unwrap();
                (t.text, times[times.len() / 2], Some(t.confidence))
            };
            let post_started = Instant::now();
            let rms = (clip.samples.iter().map(|s| s * s).sum::<f32>() / clip.samples.len().max(1) as f32).sqrt();
            let hypothesis = if cfg.postprocess {
                postprocess::clean(
                    &raw,
                    &postprocess::Options {
                        remove_fillers: true,
                        dictionary: Some(&dictionary),
                        quiet_input: rms < 0.01,
                        ..Default::default()
                    },
                )
                .text
            } else {
                raw.clone()
            };
            let ms = ms + post_started.elapsed().as_millis() as u64;
            let score = metrics::score(&clip.reference, &hypothesis, &clip.terms);
            let r = ClipResult {
                config: cfg.name.clone(),
                clip: clip.id.clone(),
                tier: clip.tier.clone(),
                tags: clip.tags.clone(),
                seconds: clip.seconds(),
                reference: clip.reference.clone(),
                raw,
                hypothesis,
                ms,
                confidence,
                score,
            };
            writeln!(jsonl, "{}", serde_json::to_string(&r)?)?;
            if (i + 1) % 20 == 0 {
                eprintln!("  {}/{}", i + 1, clips.len());
            }
            results.push(r);
        }
        drop(external);
        let baseline = all.first().map(|(_, r)| r.as_slice());
        let summary = summarize(&cfg.name, load_ms, &results, baseline);
        eprintln!(
            "  WER {:.2}%  jargon {:.0}%  halluc {:.0}%  p50 {} ms  p95 {} ms",
            summary.wer * 100.0,
            summary.jargon_recall * 100.0,
            summary.hallucination_rate * 100.0,
            summary.latency_p50,
            summary.latency_p95
        );
        all.push((summary, results));
    }
    let _ = std::fs::remove_dir_all(&tmp);

    let summaries: Vec<&Summary> = all.iter().map(|(s, _)| s).collect();
    std::fs::write(out_dir.join("summary.json"), serde_json::to_vec_pretty(&summaries)?)?;
    std::fs::write(out_dir.join("configs.json"), serde_json::to_vec_pretty(&configs)?)?;
    let md = crate::bench_report::markdown(&all, &clips);
    std::fs::write(out_dir.join("report.md"), &md)?;
    std::fs::write(out_dir.join("report.html"), crate::bench_report::html(&all))?;
    println!("{md}");
    eprintln!("report: {}", out_dir.join("report.html").display());
    Ok(())
}

fn clip_path(root: &Path, tmp: &Path, clip: &Clip) -> anyhow::Result<PathBuf> {
    let p = root.join(&clip.tier).join(format!("{}.wav", clip.id));
    if p.exists() {
        return Ok(p);
    }
    std::fs::create_dir_all(tmp)?;
    let p = tmp.join(format!("{}.wav", clip.id));
    if !p.exists() {
        audio::write_wav_16k(&p, &clip.samples)?;
    }
    Ok(p)
}
