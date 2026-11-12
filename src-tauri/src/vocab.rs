//! Vocabulary hints for biasing recognition toward the user's jargon.
//!
//! Sources, in priority order: dictionary replacements, the user's own hint words,
//! project folder names (e.g. `~/IdeaProjects/zephyrix` → "Zephyrix"), and a small
//! built-in developer glossary.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptMode {
    None,
    /// Dictionary replacements + user hint words.
    Dictionary,
    /// … + project folder names.
    Projects,
    /// … + built-in developer glossary.
    Glossary,
}

pub const GLOSSARY: &[&str] = &[
    "Supabase", "Postgres", "SvelteKit", "Svelte", "Tailwind", "shadcn", "Tauri", "TypeScript",
    "JavaScript", "GitHub", "GitLab", "Playwright", "Claude Code", "MCP", "API", "CLI", "JSON",
    "Notion", "Gmail", "Next.js", "Node.js", "Vercel", "Docker", "Kubernetes", "OpenAI",
    "Whisper", "Parakeet", "macOS", "iOS", "PDF", "ERP", "HTML", "CSS", "UI", "UX", "CI",
];

const PROJECT_ROOTS: &[&str] = &["IdeaProjects", "Projects", "Developer", "code", "source/repos"];
const MAX_TERMS: usize = 60;

/// Folder names that look like product names: single alphabetic words of 4+ letters,
/// or CamelCase. Hyphenated names, worktrees and scratch folders are skipped.
pub fn project_names(home: &Path) -> Vec<String> {
    let mut out = BTreeSet::new();
    for root in PROJECT_ROOTS {
        let Ok(entries) = std::fs::read_dir(home.join(root)) else { continue };
        for e in entries.flatten() {
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let lower = name.to_lowercase();
            if name.len() < 4 || !name.chars().all(|c| c.is_ascii_alphabetic()) || ["test", "temp", "tmp", "jobs", "components", "portal"].contains(&lower.as_str()) {
                continue;
            }
            let has_inner_caps = name.chars().skip(1).any(|c| c.is_ascii_uppercase());
            let display = if has_inner_caps { name.clone() } else { capitalize(&lower) };
            out.insert(display);
        }
    }
    out.into_iter().collect()
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// The term of a vocabulary entry: an entry may say what it stands for ("PO = purchase order").
pub fn term_of(entry: &str) -> &str {
    entry.split_once('=').map_or(entry, |(term, _)| term).trim()
}

pub struct Sources<'a> {
    pub dictionary: &'a [String],
    pub user_terms: &'a [String],
    pub projects: &'a [String],
}

pub fn terms(mode: PromptMode, src: &Sources) -> Vec<String> {
    let mut list: Vec<String> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut push = |t: &str| {
        let t = t.trim();
        if !t.is_empty() && seen.insert(t.to_lowercase()) && list.len() < MAX_TERMS {
            list.push(t.to_string());
        }
    };
    if mode == PromptMode::None {
        return list;
    }
    // A replacement is pushed as written; only the user's terms may say what they stand for.
    src.dictionary.iter().for_each(|t| push(t));
    src.user_terms.iter().for_each(|t| push(term_of(t)));
    if matches!(mode, PromptMode::Projects | PromptMode::Glossary) {
        src.projects.iter().for_each(|t| push(t));
    }
    if mode == PromptMode::Glossary {
        GLOSSARY.iter().for_each(|t| push(t));
    }
    list
}

/// Whisper initial prompt. Written as a natural sentence fragment, because bare comma
/// lists sometimes get echoed back on silent audio.
pub fn prompt(terms: &[String]) -> Option<String> {
    (!terms.is_empty()).then(|| format!("Terms used in this note: {}.", terms.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_take_the_term_of_an_entry_with_a_meaning() {
        let user = ["PO = purchase order".to_string(), "Zephyrix".to_string()];
        let src = Sources { dictionary: &[], user_terms: &user, projects: &[] };
        assert_eq!(terms(PromptMode::Dictionary, &src), ["PO", "Zephyrix"]);
    }

    #[test]
    fn dictionary_replacements_are_hints_as_written() {
        let dictionary = ["E = mc²".to_string(), "Mit freundlichen Grüßen".to_string()];
        let src = Sources { dictionary: &dictionary, user_terms: &[], projects: &[] };
        assert_eq!(terms(PromptMode::Dictionary, &src), ["E = mc²", "Mit freundlichen Grüßen"]);
    }
}
