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
    src.dictionary.iter().chain(src.user_terms).for_each(|t| push(t));
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
