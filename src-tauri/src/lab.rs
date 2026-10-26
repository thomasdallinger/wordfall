//! Voice Lab: the user reads prompts aloud; each recording becomes a `lab` benchmark clip
//! (`<data>/bench/lab/<prompt>.wav` + `.json`) and gets an instant transcription for feedback.

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};

use crate::{metrics, postprocess, store::Store, transcribe::{DecodeOptions, Engine}, vocab};

const PROMPTS_JSON: &str = include_str!("../../bench/lab-prompts.json");
const MAX_PERSONAL: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabPrompt {
    pub id: String,
    pub category: String,
    pub text: String,
    #[serde(default)]
    pub terms: Vec<String>,
    #[serde(default)]
    pub recorded: bool,
    /// Last transcription of the user's recording, for feedback.
    #[serde(default)]
    pub hypothesis: Option<String>,
    #[serde(default)]
    pub wer: Option<f64>,
}

#[derive(Deserialize)]
struct PromptFile {
    prompts: Vec<LabPrompt>,
    templates: Templates,
}

#[derive(Deserialize)]
struct Templates {
    project: Vec<String>,
    word: Vec<String>,
}

pub fn lab_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("bench").join("lab")
}

fn slug(s: &str) -> String {
    s.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// Built-in prompts plus personalised ones from project names and dictionary words.
pub fn prompts(data_dir: &Path, store: &Store, projects: &[String]) -> Vec<LabPrompt> {
    let file: PromptFile = serde_json::from_str(PROMPTS_JSON).expect("valid lab-prompts.json");
    let mut list = file.prompts;
    for (i, p) in projects.iter().take(MAX_PERSONAL).enumerate() {
        let t = &file.templates.project[i % file.templates.project.len()];
        list.push(LabPrompt {
            id: format!("project-{}", slug(p)),
            category: "personal".into(),
            text: t.replace("{term}", p),
            terms: vec![p.clone()],
            recorded: false,
            hypothesis: None,
            wer: None,
        });
    }
    let words: Vec<&String> = store.dictionary.iter().map(|e| &e.replacement).chain(store.vocabulary.iter()).collect();
    for w in words.into_iter().take(MAX_PERSONAL) {
        list.push(LabPrompt {
            id: format!("word-{}", slug(w)),
            category: "personal".into(),
            text: file.templates.word[0].replace("{term}", w),
            terms: vec![w.clone()],
            recorded: false,
            hypothesis: None,
            wer: None,
        });
    }
    let dir = lab_dir(data_dir);
    for p in &mut list {
        let meta = dir.join(format!("{}.json", p.id));
        if dir.join(format!("{}.wav", p.id)).exists() {
            p.recorded = true;
            if let Ok(b) = std::fs::read(&meta) {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&b) {
                    p.hypothesis = v["lastHypothesis"].as_str().map(String::from);
                    p.wer = v["lastWer"].as_f64();
                }
            }
        }
    }
    list
}

pub struct LabResult {
    pub hypothesis: String,
    pub wer: f64,
    pub ms: u64,
}

/// Saves the clip + reference, transcribes it with the app's current settings, and
/// records the feedback in the clip metadata.
#[allow(clippy::too_many_arguments)]
pub fn save_and_score(
    data_dir: &Path,
    prompt: &LabPrompt,
    samples: &[f32],
    engine: &Engine,
    decode: &DecodeOptions,
    prompt_text: Option<&str>,
    dictionary: &[crate::store::DictEntry],
) -> anyhow::Result<LabResult> {
    let dir = lab_dir(data_dir);
    crate::audio::write_wav_16k(&dir.join(format!("{}.wav", prompt.id)), samples)?;
    let t = engine.transcribe(samples, &[], prompt_text, decode)?;
    let hypothesis = postprocess::clean(
        &t.text,
        &postprocess::Options { remove_fillers: true, language: t.language.as_deref(), dictionary: Some(dictionary), ..Default::default() },
    )
    .text;
    let score = metrics::score(&prompt.text, &hypothesis, &prompt.terms);
    let wer = metrics::ratio(score.word_errors, score.ref_words);
    let meta = json!({
        "tier": "lab",
        "reference": prompt.text,
        "terms": prompt.terms,
        "tags": [prompt.category],
        "source": "voice-lab",
        "recordedAt": chrono::Utc::now().to_rfc3339(),
        "lastHypothesis": hypothesis,
        "lastWer": wer,
    });
    std::fs::write(dir.join(format!("{}.json", prompt.id)), serde_json::to_vec_pretty(&meta)?)?;
    Ok(LabResult { hypothesis, wer, ms: t.elapsed_ms })
}

pub fn delete(data_dir: &Path, id: &str) {
    let dir = lab_dir(data_dir);
    let _ = std::fs::remove_file(dir.join(format!("{id}.wav")));
    let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
}

pub fn prompt_terms(store: &Store, projects: &[String], mode: vocab::PromptMode) -> Vec<String> {
    let dict: Vec<String> = store.dictionary.iter().map(|e| e.replacement.clone()).collect();
    vocab::terms(mode, &vocab::Sources { dictionary: &dict, user_terms: &store.vocabulary, projects })
}
