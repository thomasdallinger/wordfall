//! History, statistics, dictionary and suggestion persistence, plus folder import.

use chrono::{DateTime, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use crate::storage::{read_json, write_json};

const MAX_RECORDS: usize = 5000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub raw_text: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub word_count: u64,
    pub duration_seconds: u64,
    #[serde(default)]
    pub transcription_ms: Option<u64>,
    pub timestamp: String,
    /// Ground-truth transcript the user corrected or verified (benchmark "history" tier).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Whether a WAV of this dictation is kept in `<data>/audio/` (filled in at read time).
    #[serde(default, skip_deserializing)]
    pub has_audio: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Totals {
    pub total_words: u64,
    pub total_seconds: u64,
    pub total_sessions: u64,
    pub first_used: Option<String>,
    pub last_used: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct HistoryFile {
    version: u32,
    records: Vec<HistoryRecord>,
    stats: Totals,
    active_days: BTreeSet<String>,
    /// Folders whose lifetime totals were already folded into `stats`.
    #[serde(default)]
    imported_sources: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub total_words: u64,
    pub total_seconds: u64,
    pub total_sessions: u64,
    pub first_used: Option<String>,
    pub last_used: Option<String>,
    pub streak: u32,
    pub wpm: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictEntry {
    pub id: String,
    pub original: String,
    pub replacement: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub id: String,
    pub detected_word: String,
    pub suggested_replacement: String,
    pub reason: String,
    pub confidence: f32,
    pub occurrence_count: u32,
    #[serde(default)]
    pub dismissed: bool,
    #[serde(default)]
    pub accepted: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub history: usize,
    pub dictionary: usize,
    pub suggestions: usize,
    /// Folder with Whisper models found next to the data (added to the model folders).
    pub model_folder: Option<String>,
}

pub struct Store {
    dir: PathBuf,
    history: HistoryFile,
    pub dictionary: Vec<DictEntry>,
    pub suggestions: Vec<Suggestion>,
    /// Hint-only words (no replacement) used to bias recognition.
    pub vocabulary: Vec<String>,
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl Store {
    pub fn load(dir: &Path) -> Self {
        let mut history: HistoryFile = read_json(&dir.join("history.json")).unwrap_or_default();
        history.version = 1;
        Self {
            dir: dir.to_path_buf(),
            history,
            dictionary: read_json(&dir.join("dictionary.json")).unwrap_or_default(),
            suggestions: read_json(&dir.join("suggestions.json")).unwrap_or_default(),
            vocabulary: read_json(&dir.join("vocabulary.json")).unwrap_or_default(),
        }
    }

    fn save_history(&self) {
        if let Err(e) = write_json(&self.dir.join("history.json"), &self.history) {
            log::error!("saving history failed: {e}");
        }
    }

    pub fn save_dictionary(&self) {
        if let Err(e) = write_json(&self.dir.join("dictionary.json"), &self.dictionary) {
            log::error!("saving dictionary failed: {e}");
        }
    }

    pub fn save_suggestions(&self) {
        if let Err(e) = write_json(&self.dir.join("suggestions.json"), &self.suggestions) {
            log::error!("saving suggestions failed: {e}");
        }
    }

    // ---------- history ----------

    pub fn add_record(&mut self, record: HistoryRecord) {
        let t = &mut self.history.stats;
        t.total_words += record.word_count;
        t.total_seconds += record.duration_seconds;
        t.total_sessions += 1;
        if t.first_used.is_none() {
            t.first_used = Some(record.timestamp.clone());
        }
        t.last_used = Some(record.timestamp.clone());
        self.history
            .active_days
            .insert(Local::now().format("%Y-%m-%d").to_string());
        self.history.records.insert(0, record);
        self.history.records.truncate(MAX_RECORDS);
        self.save_history();
    }

    pub fn history_page(&self, query: Option<&str>, offset: usize, limit: usize) -> (usize, Vec<HistoryRecord>) {
        let q = query.map(str::trim).filter(|q| !q.is_empty()).map(str::to_lowercase);
        let filtered: Vec<&HistoryRecord> = self
            .history
            .records
            .iter()
            .filter(|r| q.as_ref().map_or(true, |q| r.text.to_lowercase().contains(q)))
            .collect();
        let total = filtered.len();
        let page = filtered.into_iter().skip(offset).take(limit).cloned().collect();
        (total, page)
    }

    pub fn delete_record(&mut self, id: &str) {
        self.history.records.retain(|r| r.id != id);
        self.save_history();
    }

    /// Store the user's corrected transcript as ground truth (and show it in History).
    pub fn set_reference(&mut self, id: &str, text: Option<String>) -> bool {
        let Some(r) = self.history.records.iter_mut().find(|r| r.id == id) else { return false };
        match text {
            Some(t) => {
                let t = t.trim().to_string();
                r.word_count = t.split_whitespace().count() as u64;
                r.text = t.clone();
                r.reference = Some(t);
            }
            None => r.reference = Some(r.text.clone()),
        }
        self.save_history();
        true
    }

    pub fn save_vocabulary(&self) {
        if let Err(e) = write_json(&self.dir.join("vocabulary.json"), &self.vocabulary) {
            log::error!("saving vocabulary failed: {e}");
        }
    }

    pub fn clear_history(&mut self) {
        self.history.records.clear();
        self.save_history();
    }

    pub fn all_records(&self) -> &[HistoryRecord] {
        &self.history.records
    }

    pub fn stats(&self) -> Stats {
        let t = &self.history.stats;
        let wpm = if t.total_seconds > 0 {
            (t.total_words as f64 / (t.total_seconds as f64 / 60.0)).round() as u32
        } else {
            0
        };
        Stats {
            total_words: t.total_words,
            total_seconds: t.total_seconds,
            total_sessions: t.total_sessions,
            first_used: t.first_used.clone(),
            last_used: t.last_used.clone(),
            streak: self.streak(),
            wpm,
        }
    }

    /// Consecutive active days ending today (or yesterday, so a streak survives until midnight).
    fn streak(&self) -> u32 {
        let days: BTreeSet<NaiveDate> = self
            .history
            .active_days
            .iter()
            .filter_map(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .collect();
        let today = Local::now().date_naive();
        let mut day = if days.contains(&today) { today } else { today.pred_opt().unwrap() };
        let mut n = 0;
        while days.contains(&day) {
            n += 1;
            day = day.pred_opt().unwrap();
        }
        n
    }

    // ---------- dictionary ----------

    pub fn upsert_dictionary(&mut self, id: Option<String>, original: String, replacement: String) -> DictEntry {
        let original = original.trim().to_string();
        let replacement = replacement.trim().to_string();
        let existing = self.dictionary.iter_mut().find(|e| {
            id.as_deref() == Some(e.id.as_str()) || e.original.eq_ignore_ascii_case(&original)
        });
        let entry = match existing {
            Some(e) => {
                e.original = original;
                e.replacement = replacement;
                e.clone()
            }
            None => {
                let e = DictEntry { id: new_id(), original, replacement, created_at: now() };
                self.dictionary.insert(0, e.clone());
                e
            }
        };
        self.save_dictionary();
        entry
    }

    pub fn delete_dictionary(&mut self, id: &str) {
        self.dictionary.retain(|e| e.id != id);
        self.save_dictionary();
    }

    // ---------- suggestions ----------

    pub fn open_suggestions(&self) -> Vec<Suggestion> {
        self.suggestions.iter().filter(|s| !s.dismissed && !s.accepted).cloned().collect()
    }

    pub fn note_suggestion(&mut self, detected: &str, replacement: &str, reason: &str, confidence: f32) -> bool {
        if self.dictionary.iter().any(|e| e.original.eq_ignore_ascii_case(detected)) {
            return false;
        }
        if let Some(s) = self.suggestions.iter_mut().find(|s| s.detected_word.eq_ignore_ascii_case(detected)) {
            s.occurrence_count += 1;
            let open = !s.dismissed && !s.accepted;
            self.save_suggestions();
            return open;
        }
        self.suggestions.insert(
            0,
            Suggestion {
                id: new_id(),
                detected_word: detected.to_string(),
                suggested_replacement: replacement.to_string(),
                reason: reason.to_string(),
                confidence,
                occurrence_count: 1,
                dismissed: false,
                accepted: false,
            },
        );
        self.save_suggestions();
        true
    }

    pub fn accept_suggestion(&mut self, id: &str, replacement: Option<String>) {
        let Some(s) = self.suggestions.iter_mut().find(|s| s.id == id) else { return };
        s.accepted = true;
        let (orig, repl) = (s.detected_word.clone(), replacement.unwrap_or_else(|| s.suggested_replacement.clone()));
        self.save_suggestions();
        self.upsert_dictionary(None, orig, repl);
    }

    pub fn dismiss_suggestion(&mut self, id: &str) {
        if let Some(s) = self.suggestions.iter_mut().find(|s| s.id == id) {
            s.dismissed = true;
        }
        self.save_suggestions();
    }

    pub fn dismiss_all_suggestions(&mut self) {
        for s in self.suggestions.iter_mut().filter(|s| !s.accepted) {
            s.dismissed = true;
        }
        self.save_suggestions();
    }

    // ---------- import ----------

    /// Merges history, statistics, dictionary and suggestions found in a folder of JSON
    /// exports. Files are recognised by structure, not name, so data from other dictation
    /// tools with similar shapes imports too. Idempotent: records merge by id, words by text,
    /// and lifetime totals are folded in once per source folder.
    pub fn import_folder(&mut self, dir: &Path) -> ImportResult {
        #[derive(Deserialize)]
        struct HistoryShape {
            records: Vec<HistoryRecord>,
            #[serde(default)]
            stats: Option<Totals>,
            #[serde(default)]
            active_days: Vec<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WordShape {
            original: String,
            replacement: String,
            #[serde(default)]
            created_at: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum DictShape {
            Wrapped { entries: Vec<WordShape> },
            Bare(Vec<WordShape>),
        }
        #[derive(Deserialize)]
        struct SuggestionShape {
            suggestions: Vec<Suggestion>,
        }

        let mut result = ImportResult { history: 0, dictionary: 0, suggestions: 0, model_folder: None };
        let source = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()).display().to_string();
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|it| it.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect())
            .unwrap_or_default();
        files.sort();

        for file in files {
            let Ok(bytes) = std::fs::read(&file) else { continue };
            if let Ok(h) = serde_json::from_slice::<HistoryShape>(&bytes) {
                let known: BTreeSet<String> = self.history.records.iter().map(|r| r.id.clone()).collect();
                let fresh: Vec<HistoryRecord> = h.records.into_iter().filter(|r| !known.contains(&r.id)).collect();
                result.history += fresh.len();
                if let Some(stats) = h.stats.filter(|_| !self.history.imported_sources.contains(&source)) {
                    let t = &mut self.history.stats;
                    t.total_words += stats.total_words;
                    t.total_seconds += stats.total_seconds;
                    t.total_sessions += stats.total_sessions;
                    t.first_used = earliest(t.first_used.take(), stats.first_used);
                    t.last_used = latest(t.last_used.take(), stats.last_used);
                    self.history.active_days.extend(h.active_days);
                    self.history.imported_sources.insert(source.clone());
                }
                self.history.records.extend(fresh);
                self.history.records.sort_by(|a, b| parse_ts(&b.timestamp).cmp(&parse_ts(&a.timestamp)));
                self.history.records.truncate(MAX_RECORDS);
                self.save_history();
            } else if let Ok(d) = serde_json::from_slice::<DictShape>(&bytes) {
                let words = match d {
                    DictShape::Wrapped { entries } => entries,
                    DictShape::Bare(v) => v,
                };
                for e in words {
                    if !self.dictionary.iter().any(|d| d.original.eq_ignore_ascii_case(&e.original)) {
                        self.dictionary.push(DictEntry {
                            id: new_id(),
                            original: e.original,
                            replacement: e.replacement,
                            created_at: e.created_at.unwrap_or_else(now),
                        });
                        result.dictionary += 1;
                    }
                }
                self.save_dictionary();
            } else if let Ok(sg) = serde_json::from_slice::<SuggestionShape>(&bytes) {
                for sug in sg.suggestions {
                    if !self.suggestions.iter().any(|x| x.detected_word.eq_ignore_ascii_case(&sug.detected_word)) {
                        self.suggestions.push(sug);
                        result.suggestions += 1;
                    }
                }
                self.save_suggestions();
            }
        }
        result
    }
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

fn earliest(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if parse_ts(&a) <= parse_ts(&b) { a } else { b }),
        (a, b) => a.or(b),
    }
}

fn latest(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if parse_ts(&a) >= parse_ts(&b) { a } else { b }),
        (a, b) => a.or(b),
    }
}

pub fn export_csv(records: &[HistoryRecord]) -> String {
    let mut out = String::from("timestamp,words,duration_seconds,model,text\n");
    for r in records {
        let text = r.text.replace('"', "\"\"");
        out.push_str(&format!(
            "{},{},{},{},\"{}\"\n",
            r.timestamp,
            r.word_count,
            r.duration_seconds,
            r.model.as_deref().unwrap_or(""),
            text
        ));
    }
    out
}

pub fn make_record(text: String, raw: String, model: &str, duration_ms: u64, transcription_ms: u64) -> HistoryRecord {
    HistoryRecord {
        id: new_id(),
        word_count: text.split_whitespace().count() as u64,
        text,
        raw_text: Some(raw),
        model: Some(model.to_string()),
        duration_seconds: ((duration_ms + 500) / 1000).max(1),
        transcription_ms: Some(transcription_ms),
        timestamp: now(),
        reference: None,
        has_audio: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wordfall-test-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn import_detects_files_by_structure_and_is_idempotent() {
        let src = temp_dir("src");
        std::fs::write(
            src.join("a.json"),
            r#"{"records":[{"id":"r1","text":"hello there","word_count":2,"duration_seconds":1,"timestamp":"2026-01-02T10:00:00Z"}],
               "stats":{"total_words":100,"total_seconds":60,"total_sessions":5,"first_used":"2026-01-01T00:00:00Z","last_used":"2026-01-02T10:00:00Z"},
               "active_days":["2026-01-02"]}"#,
        )
        .unwrap();
        std::fs::write(src.join("b.json"), r#"{"entries":[{"original":"super base","replacement":"Supabase"}]}"#).unwrap();
        std::fs::write(src.join("c.json"), r#"[{"original":"git hub","replacement":"GitHub"}]"#).unwrap();
        std::fs::write(
            src.join("d.json"),
            r#"{"suggestions":[{"id":"s1","detectedWord":"type script","suggestedReplacement":"TypeScript","reason":"x","confidence":0.5,"occurrenceCount":2}]}"#,
        )
        .unwrap();
        std::fs::write(src.join("unrelated.json"), r#"{"theme":"dark"}"#).unwrap();

        let data = temp_dir("data");
        let mut store = Store::load(&data);
        let r = store.import_folder(&src);
        assert_eq!((r.history, r.dictionary, r.suggestions), (1, 2, 1));
        assert_eq!(store.stats().total_words, 100);

        let again = store.import_folder(&src);
        assert_eq!((again.history, again.dictionary, again.suggestions), (0, 0, 0));
        assert_eq!(store.stats().total_words, 100, "totals fold in once per source");
    }
}
