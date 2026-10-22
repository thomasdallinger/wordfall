//! Accuracy metrics for the benchmark: normalised WER/CER, formatting error, jargon recall.

use regex::Regex;
use std::sync::LazyLock;

static NON_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}'\s]").unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
/// "30pm" → "30 pm", "3x" → "3 x" (but keep ordinals like "21st").
static DIGIT_ALPHA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d)(am|pm|x|s|ms|k|gb|mb)\b").unwrap());

const CONTRACTIONS: &[(&str, &str)] = &[
    ("don't", "do not"), ("doesn't", "does not"), ("didn't", "did not"), ("can't", "can not"),
    ("cannot", "can not"), ("won't", "will not"), ("isn't", "is not"), ("aren't", "are not"),
    ("wasn't", "was not"), ("it's", "it is"), ("that's", "that is"), ("there's", "there is"),
    ("i'm", "i am"), ("i've", "i have"), ("i'll", "i will"), ("i'd", "i would"), ("we're", "we are"),
    ("we'll", "we will"), ("let's", "let us"), ("you're", "you are"), ("they're", "they are"),
    ("what's", "what is"), ("here's", "here is"), ("shouldn't", "should not"), ("couldn't", "could not"),
];

const NUMBERS: &[(&str, &str)] = &[
    ("zero", "0"), ("one", "1"), ("two", "2"), ("three", "3"), ("four", "4"), ("five", "5"),
    ("six", "6"), ("seven", "7"), ("eight", "8"), ("nine", "9"), ("ten", "10"), ("eleven", "11"),
    ("twelve", "12"), ("fifteen", "15"), ("twenty", "20"), ("thirty", "30"), ("forty", "40"),
    ("fifty", "50"), ("sixty", "60"), ("hundred", "100"), ("first", "1st"), ("second", "2nd"),
    ("third", "3rd"), ("twenty-first", "21st"),
];

/// Lowercase, strip punctuation, expand contractions, map simple number words to digits,
/// and treat hyphens, dots and colons inside tokens as word breaks ("2:30", "Next.js").
pub fn normalize(text: &str) -> Vec<String> {
    let lower = text.to_lowercase().replace(['-', '.', ':', '/', '_'], " ").replace(['’', '‘'], "'");
    let cleaned = NON_WORD.replace_all(&lower, " ");
    let cleaned = DIGIT_ALPHA.replace_all(&cleaned, "$1 $2");
    let mut out = Vec::new();
    for tok in SPACES.split(cleaned.trim()).filter(|t| !t.is_empty()) {
        let tok = tok.trim_matches('\'');
        if tok.is_empty() {
            continue;
        }
        if let Some((_, exp)) = CONTRACTIONS.iter().find(|(c, _)| *c == tok) {
            out.extend(exp.split(' ').map(String::from));
        } else if let Some((_, d)) = NUMBERS.iter().find(|(w, _)| *w == tok) {
            out.push((*d).to_string());
        } else {
            out.push(tok.to_string());
        }
    }
    // "p m" / "a m" after numbers → "pm"/"am".
    let mut merged: Vec<String> = Vec::with_capacity(out.len());
    for t in out {
        if let Some(prev) = merged.last_mut() {
            if (prev == "p" || prev == "a") && t == "m" {
                prev.push('m');
                continue;
            }
        }
        merged.push(t);
    }
    merged
}

/// Word-level edit distance.
pub fn edit_distance<T: PartialEq>(reference: &[T], hypothesis: &[T]) -> usize {
    let mut prev: Vec<usize> = (0..=hypothesis.len()).collect();
    let mut cur = vec![0; hypothesis.len() + 1];
    for (i, r) in reference.iter().enumerate() {
        cur[0] = i + 1;
        for (j, h) in hypothesis.iter().enumerate() {
            cur[j + 1] = if r == h { prev[j] } else { 1 + prev[j].min(prev[j + 1]).min(cur[j]) };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[hypothesis.len()]
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Score {
    /// Word errors after normalisation, and reference word count.
    pub word_errors: usize,
    pub ref_words: usize,
    pub char_errors: usize,
    pub ref_chars: usize,
    /// Errors when casing and punctuation are kept (whitespace tokens).
    pub format_errors: usize,
    pub format_words: usize,
    pub terms_total: usize,
    pub terms_hit: usize,
    pub missed_terms: Vec<String>,
}

pub fn ratio(a: usize, b: usize) -> f64 {
    if b == 0 {
        if a == 0 { 0.0 } else { 1.0 }
    } else {
        a as f64 / b as f64
    }
}

/// Exact, case-sensitive match of a term at word boundaries.
fn contains_term(hypothesis: &str, term: &str) -> bool {
    let pattern = format!(r"(^|[^\p{{L}}\p{{N}}]){}($|[^\p{{L}}\p{{N}}])", regex::escape(term));
    Regex::new(&pattern).map(|re| re.is_match(hypothesis)).unwrap_or(false)
}

pub fn score(reference: &str, hypothesis: &str, terms: &[String]) -> Score {
    let r = normalize(reference);
    let h = normalize(hypothesis);
    let rc: Vec<char> = r.join(" ").chars().collect();
    let hc: Vec<char> = h.join(" ").chars().collect();
    let rf: Vec<&str> = reference.split_whitespace().collect();
    let hf: Vec<&str> = hypothesis.split_whitespace().collect();
    let missed: Vec<String> = terms.iter().filter(|t| !contains_term(hypothesis, t)).cloned().collect();
    Score {
        word_errors: edit_distance(&r, &h),
        ref_words: r.len(),
        char_errors: edit_distance(&rc, &hc),
        ref_chars: rc.len(),
        format_errors: edit_distance(&rf, &hf),
        format_words: rf.len(),
        terms_total: terms.len(),
        terms_hit: terms.len() - missed.len(),
        missed_terms: missed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_ignores_case_punctuation_and_contractions() {
        assert_eq!(normalize("Don't stop, OK?"), vec!["do", "not", "stop", "ok"]);
        assert_eq!(normalize("Retry three times at 2:30 p.m."), vec!["retry", "3", "times", "at", "2", "30", "pm"]);
        assert_eq!(normalize("at 2.30pm"), normalize("at 2:30 PM"));
        assert_eq!(normalize("October 21st"), vec!["october", "21st"]);
    }

    #[test]
    fn wer_counts_substitutions_insertions_deletions() {
        let s = score("open the zephyrix repo", "open the zefirix repo now", &[]);
        assert_eq!((s.word_errors, s.ref_words), (2, 4));
    }

    #[test]
    fn jargon_terms_are_case_sensitive() {
        let terms = vec!["Supabase".to_string(), "MCP".to_string()];
        let s = score("Use Supabase with MCP", "Use supabase with MCP.", &terms);
        assert_eq!(s.terms_hit, 1);
        assert_eq!(s.missed_terms, vec!["Supabase"]);
    }

    #[test]
    fn multiword_terms_match() {
        let terms = vec!["Claude Code".to_string()];
        assert_eq!(score("x", "Ask Claude Code, please.", &terms).terms_hit, 1);
    }
}
