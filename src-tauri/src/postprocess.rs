//! Transcript cleanup: whisper artifacts, hallucinations, filler words, dictionary.

use regex::Regex;
use std::sync::LazyLock;

use crate::store::DictEntry;

static ANNOTATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\[(?:blank_audio|music|silence|inaudible|no speech)[^\]]*\]|\((?:music|silence|inaudible|laughs|applause)[^)]*\)|\*[^*]{0,30}\*").unwrap());
static FILLER_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:u+m+|u+h+|e+r+m+|a+h+|h+m+|m+h*m+)$").unwrap());
static MULTISPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]{2,}").unwrap());
static SPACE_BEFORE_PUNCT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+([,.;:!?])").unwrap());
static DOUBLE_COMMA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s*([,.!?])").unwrap());

/// Phrases Whisper tends to invent on near-silent input.
const HALLUCINATIONS: &[&str] = &[
    "thank you.", "thank you", "thanks for watching!", "thanks for watching.", "thank you for watching.",
    "you", "you.", "bye.", "bye!", "so", "okay.", "subtitles by the amara.org community",
    ".", "...", "i'm sorry.", "thank you very much.",
];

pub struct Cleaned {
    pub text: String,
    #[cfg_attr(not(test), allow(dead_code))]
    pub dropped_as_hallucination: bool,
}

pub fn clean(raw: &str, remove_fillers: bool, dictionary: Option<&[DictEntry]>, quiet_input: bool) -> Cleaned {
    let mut text = ANNOTATION.replace_all(raw, " ").to_string();
    text = text.replace('\n', " ");
    text = MULTISPACE.replace_all(text.trim(), " ").to_string();

    let lowered = text.to_lowercase();
    if quiet_input && HALLUCINATIONS.contains(&lowered.as_str()) {
        return Cleaned { text: String::new(), dropped_as_hallucination: true };
    }

    if remove_fillers {
        text = strip_fillers(&text);
        text = DOUBLE_COMMA.replace_all(&text, "$1").to_string();
        text = text.trim_start_matches([',', ' ', '.']).to_string();
        text = MULTISPACE.replace_all(text.trim(), " ").to_string();
        text = SPACE_BEFORE_PUNCT.replace_all(&text, "$1").to_string();
        text = capitalize_first(&text);
    }

    if let Some(dict) = dictionary {
        text = apply_dictionary(&text, dict);
    }

    Cleaned { text: text.trim().to_string(), dropped_as_hallucination: false }
}

/// Drops filler tokens; sentence-ending punctuation on a dropped filler moves to the previous word.
fn strip_fillers(text: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    for token in text.split_whitespace() {
        let core = token.trim_matches(|c: char| !c.is_alphanumeric());
        if !core.is_empty() && FILLER_WORD.is_match(core) {
            let trailing: String = token.chars().rev().take_while(|c| ".!?".contains(*c)).collect();
            if !trailing.is_empty() {
                if let Some(prev) = kept.last_mut() {
                    let base = prev.trim_end_matches(|c: char| ",;:".contains(c)).to_string();
                    if !base.ends_with(['.', '!', '?']) {
                        *prev = format!("{base}{}", trailing.chars().rev().collect::<String>());
                    }
                }
            }
            continue;
        }
        kept.push(token.to_string());
    }
    kept.join(" ")
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_lowercase() => c.to_uppercase().chain(chars).collect(),
        _ => s.to_string(),
    }
}

pub fn apply_dictionary(text: &str, dict: &[DictEntry]) -> String {
    let mut out = text.to_string();
    // Longest phrases first so "super base" wins over "base".
    let mut entries: Vec<&DictEntry> = dict.iter().filter(|e| !e.original.trim().is_empty()).collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.original.len()));
    for e in entries {
        let words: Vec<String> = e.original.split_whitespace().map(regex::escape).collect();
        let pattern = format!(r"(?i)\b{}\b", words.join(r"[\s-]+"));
        if let Ok(re) = Regex::new(&pattern) {
            out = re.replace_all(&out, regex::NoExpand(&e.replacement)).to_string();
        }
    }
    out
}

/// Known Whisper mishearings of developer vocabulary → (heard, intended).
const KNOWN_FAILURES: &[(&str, &str)] = &[
    ("super base", "Supabase"),
    ("git hub", "GitHub"),
    ("java script", "JavaScript"),
    ("type script", "TypeScript"),
    ("chat gpt", "ChatGPT"),
    ("chat g p t", "ChatGPT"),
    ("next js", "Next.js"),
    ("node js", "Node.js"),
    ("post gress", "Postgres"),
    ("postgre sql", "PostgreSQL"),
    ("svelte kit", "SvelteKit"),
    ("spelt kit", "SvelteKit"),
    ("tail wind", "Tailwind"),
    ("cloud code", "Claude Code"),
    ("open ai", "OpenAI"),
    ("versel", "Vercel"),
    ("kubernetes", "Kubernetes"),
    ("j son", "JSON"),
    ("graph ql", "GraphQL"),
    ("mac os", "macOS"),
    ("i os", "iOS"),
    ("tauri", "Tauri"),
];

/// Returns (heard phrase, suggested replacement) pairs found in `text`.
pub fn detect_suggestions(text: &str) -> Vec<(String, String)> {
    let lower = text.to_lowercase();
    KNOWN_FAILURES
        .iter()
        .filter(|(heard, intended)| {
            let re = Regex::new(&format!(r"\b{}\b", regex::escape(heard))).unwrap();
            re.is_match(&lower) && !text.contains(intended)
        })
        .map(|(h, i)| (h.to_string(), i.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(o: &str, r: &str) -> DictEntry {
        DictEntry { id: "x".into(), original: o.into(), replacement: r.into(), created_at: String::new() }
    }

    #[test]
    fn strips_fillers_and_fixes_spacing() {
        let c = clean("Um, so I think, uh, we should ship it.", true, None, false);
        assert_eq!(c.text, "So I think, we should ship it.");
    }

    #[test]
    fn keeps_fillers_when_disabled() {
        let c = clean("Um, hello", false, None, false);
        assert_eq!(c.text, "Um, hello");
    }

    #[test]
    fn removes_annotations() {
        assert_eq!(clean("[BLANK_AUDIO] Hello there (music)", true, None, false).text, "Hello there");
    }

    #[test]
    fn drops_hallucination_only_when_quiet() {
        assert!(clean("Thank you.", true, None, true).dropped_as_hallucination);
        assert_eq!(clean("Thank you.", true, None, false).text, "Thank you.");
    }

    #[test]
    fn dictionary_is_case_insensitive_and_phrase_aware() {
        let d = vec![entry("super base", "Supabase"), entry("base", "BASE")];
        assert_eq!(apply_dictionary("We use Super Base and super-base.", &d), "We use Supabase and Supabase.");
    }

    #[test]
    fn dictionary_respects_word_boundaries() {
        let d = vec![entry("git", "Git")];
        assert_eq!(apply_dictionary("git digit", &d), "Git digit");
    }

    #[test]
    fn suggests_known_failures() {
        let s = detect_suggestions("deploy it to super base today");
        assert_eq!(s, vec![("super base".to_string(), "Supabase".to_string())]);
    }
}
