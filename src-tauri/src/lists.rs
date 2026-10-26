//! Spoken enumerations become lists. "First, … Second, …", "Erstens … zweitens …",
//! "number one … number two …" and inline "1. … 2. …" make a numbered list; "bullet point …
//! bullet point …" or "Stichpunkt … Stichpunkt …" a bulleted one.
//!
//! A marker only counts at the start of a clause (start of text or after punctuation, optionally
//! after "and"/"then"/"und"/"dann"); only unambiguous ones ("erstens", "secondly", "bullet point")
//! may follow a plain space, since Whisper often leaves dictated commands unpunctuated. Numbered markers must run 1, 2, 3, … and every list needs at least two non-empty
//! items, so ordinary prose ("First impressions matter", "we finally shipped") is left alone.

use regex::Regex;
use std::sync::LazyLock;

const ORDINALS_EN: [&str; 10] = ["first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth", "tenth"];
const ORDINALS_DE: [&str; 10] =
    ["erstens", "zweitens", "drittens", "viertens", "fünftens", "sechstens", "siebtens", "achtens", "neuntens", "zehntens"];
const ALS_DE: [&str; 5] = ["erstes", "zweites", "drittes", "viertes", "fünftes"];
const NUMBERS_EN: [&str; 10] = ["one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];
const NUMBERS_DE: [&str; 10] = ["eins", "zwei", "drei", "vier", "fünf", "sechs", "sieben", "acht", "neun", "zehn"];
const NUMBER_WORDS: &str = r"number|step|point|nummer|schritt|punkt";
const FINALS: &str = r"finally|lastly|last\s+but\s+not\s+least|zuletzt|schließlich|abschließend|zum\s+schluss|als\s+letztes";
const BULLETS: &str =
    r"(?:(?:next|new|another|nächster|neuer|weiterer)\s+)?(?:bullet(?:\s+point)?|stichpunkt|aufzählungspunkt|spiegelstrich)";

/// A marker word or phrase; `b` is what precedes it, `m` spans the connector and the marker.
static WORD_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    let ordinal = format!(
        r"(?:{en})(?:ly)?(?:\s+of\s+all)?|{de}|siebentens|als\s+(?:{als})",
        en = ORDINALS_EN.join("|"),
        de = ORDINALS_DE.join("|"),
        als = ALS_DE.join("|"),
    );
    let number = format!(r"(?:{NUMBER_WORDS})\s+(?:{}|{}|\d{{1,2}})", NUMBERS_EN.join("|"), NUMBERS_DE.join("|"));
    Regex::new(&format!(
        r"(?i)(?P<b>^|[.!?:;,]\s+|\s+)(?P<m>(?:(?:and\s+then|and|then|und\s+dann|und|dann),?\s+)?(?P<w>{ordinal}|{number}|{FINALS}|{BULLETS}))\b"
    ))
    .unwrap()
});
/// Inline numbering Whisper sometimes writes itself: "1. Milk 2. Eggs".
static DIGIT_MARKER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|\s)(?P<m>(?P<d>\d{1,2})[.):])\s").unwrap());
static FINAL_PHRASE: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!("^(?:{FINALS})$")).unwrap());
static BULLET_PHRASE: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!("^(?:{BULLETS})$")).unwrap());
static TRAILING_CONNECTOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[,;]?\s*\b(?:and\s+then|and|or|then|und\s+dann|und|oder|dann)$").unwrap());
static SENTENCE_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[.!?]\s+\p{Lu}").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Ordinal,
    Number,
    Digit,
    /// "finally", "zuletzt": may end a numbered list of two or more.
    Final,
    Bullet,
}

struct Kind {
    family: Family,
    value: u32,
    /// Never anything but a marker ("erstens", "secondly"), so it may follow a plain space.
    unambiguous: bool,
    /// Also an adjective ("first"); only a marker when a comma follows ("First, …").
    needs_comma: bool,
}

#[derive(Debug, Clone, Copy)]
struct Marker {
    /// Where the marker (including a leading connector) starts.
    start: usize,
    /// Where the item text starts.
    end: usize,
    family: Family,
    /// 1-based position for numbered markers.
    value: u32,
}

struct List {
    ordered: bool,
    markers: Vec<Marker>,
}

/// Rewrites spoken enumerations in `text` as lists, one item per line ("1. …" or "- …").
pub fn format(text: &str) -> String {
    let markers = find_markers(text);
    let lists = group(&markers);
    if lists.is_empty() {
        return text.to_string();
    }

    let mut blocks: Vec<String> = Vec::new();
    let mut pending = text[..lists[0].markers[0].start].to_string();
    for (k, list) in lists.iter().enumerate() {
        let list_end = lists.get(k + 1).map_or(text.len(), |l| l.markers[0].start);
        let mut items: Vec<String> = list
            .markers
            .iter()
            .enumerate()
            .map(|(i, m)| text[m.end..list.markers.get(i + 1).map_or(list_end, |n| n.start)].to_string())
            .collect();

        // Sentences after a one-sentence last item are a closing remark, not part of the item.
        let last = items.pop().unwrap_or_default();
        let last_sentences = sentences(&last);
        let trailing = if items.iter().all(|i| sentences(i).len() <= 1) && last_sentences.len() > 1 {
            items.push(last_sentences[0].to_string());
            last_sentences[1..].join(" ")
        } else {
            items.push(last);
            String::new()
        };

        let intro = clean_intro(&pending);
        if !intro.is_empty() {
            blocks.push(intro);
        }
        // Items that are all single sentences read like a checklist: no full stops.
        let drop_period = items.iter().all(|i| sentences(i).len() <= 1);
        for (i, item) in items.iter().enumerate() {
            let body = clean_item(item, drop_period);
            blocks.push(if list.ordered { format!("{}. {body}", i + 1) } else { format!("- {body}") });
        }
        // The last item ran up to the next list (or the end), so only its closing remark is left over.
        pending = trailing;
    }
    let rest = pending.trim();
    if !rest.is_empty() {
        blocks.push(rest.to_string());
    }
    blocks.join("\n")
}

fn find_markers(text: &str) -> Vec<Marker> {
    let mut out: Vec<Marker> = Vec::new();
    for c in WORD_MARKER.captures_iter(text) {
        let (b, m, w) = (c.name("b").unwrap(), c.name("m").unwrap(), c.name("w").unwrap());
        // "second-hand" is a word, not a marker.
        if text[m.end()..].starts_with('-') {
            continue;
        }
        let Some(kind) = classify(w.as_str()) else { continue };
        let clause_start = b.start() == 0 || !b.as_str().trim().is_empty();
        if !clause_start && !kind.unambiguous {
            continue;
        }
        if kind.needs_comma && !text[m.end()..].trim_start().starts_with([',', ':']) {
            continue;
        }
        out.push(Marker { start: m.start(), end: m.end(), family: kind.family, value: kind.value });
    }
    for c in DIGIT_MARKER.captures_iter(text) {
        let (m, d) = (c.name("m").unwrap(), c.name("d").unwrap());
        let value = d.as_str().parse().unwrap_or(0);
        if !out.iter().any(|o| o.start < m.end() && m.start() < o.end) {
            out.push(Marker { start: m.start(), end: m.end(), family: Family::Digit, value });
        }
    }
    out.sort_by_key(|m| m.start);
    out
}

fn classify(word: &str) -> Option<Kind> {
    let w = word.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    let position = |list: &[&str], s: &str| list.iter().position(|x| *x == s).map(|i| i as u32 + 1);
    let kind = |family, value, unambiguous, needs_comma| Kind { family, value, unambiguous, needs_comma };
    if FINAL_PHRASE.is_match(&w) {
        return Some(kind(Family::Final, 0, false, false));
    }
    if BULLET_PHRASE.is_match(&w) {
        // A bare "bullet" can be ammunition; "bullet point" and the German words are only list talk.
        return Some(kind(Family::Bullet, 0, w != "bullet" && !w.ends_with(" bullet"), false));
    }
    if let Some(rest) = w.strip_prefix("als ") {
        return position(&ALS_DE, rest).map(|v| kind(Family::Ordinal, v, false, false));
    }
    if let Some(v) = position(&ORDINALS_DE, &w).or((w == "siebentens").then_some(7)) {
        return Some(kind(Family::Ordinal, v, true, false));
    }
    if let Some((_, n)) = w.split_once(' ').filter(|(head, _)| NUMBER_WORDS.split('|').any(|x| x == *head)) {
        let v = position(&NUMBERS_EN, n).or_else(|| position(&NUMBERS_DE, n)).or_else(|| n.parse().ok())?;
        return Some(kind(Family::Number, v, false, false));
    }
    let base = w.trim_end_matches(" of all");
    if let Some(stem) = base.strip_suffix("ly") {
        return position(&ORDINALS_EN, stem).map(|v| kind(Family::Ordinal, v, true, false));
    }
    // "first of all" is unambiguous; plain "first" needs its comma.
    position(&ORDINALS_EN, base).map(|v| kind(Family::Ordinal, v, false, base == w))
}

/// Picks lists out of the markers, left to right: runs of bullets, or numbered runs 1, 2, 3, …
/// of one family (optionally closed by "finally"). Anything else stays prose.
fn group(markers: &[Marker]) -> Vec<List> {
    let mut lists = Vec::new();
    let mut i = 0;
    while i < markers.len() {
        let first = markers[i];
        let mut run = vec![first];
        let mut j = i + 1;
        if first.family == Family::Bullet {
            while j < markers.len() && markers[j].family == Family::Bullet {
                run.push(markers[j]);
                j += 1;
            }
        } else if first.value == 1 && first.family != Family::Final {
            while j < markers.len() {
                let m = markers[j];
                if m.family == first.family && m.value == run.len() as u32 + 1 {
                    run.push(m);
                    j += 1;
                } else {
                    if m.family == Family::Final && run.len() >= 2 {
                        run.push(m);
                        j += 1;
                    }
                    break;
                }
            }
        }
        // Every item but the last needs text before the next marker.
        if run.len() >= 2 && run.windows(2).all(|w| w[1].start > w[0].end) {
            lists.push(List { ordered: first.family != Family::Bullet, markers: run });
            i = j;
        } else {
            i += 1;
        }
    }
    lists
}

/// Splits after ". " / "? " / "! " when a capital letter follows.
fn sentences(s: &str) -> Vec<&str> {
    let s = s.trim();
    let mut out = Vec::new();
    let mut from = 0;
    for m in SENTENCE_END.find_iter(s) {
        let cut = m.end() - m.as_str().chars().last().map_or(0, char::len_utf8);
        out.push(s[from..cut].trim());
        from = cut;
    }
    out.push(s[from..].trim());
    out.retain(|x| !x.is_empty());
    out
}

fn clean_intro(s: &str) -> String {
    let s = TRAILING_CONNECTOR.replace(s.trim().trim_end_matches([',', ';']), "").trim().to_string();
    match s.chars().last() {
        None => s,
        Some(c) if c.is_alphanumeric() => capitalize(&format!("{s}:")),
        Some(_) => capitalize(&s),
    }
}

fn clean_item(s: &str, drop_period: bool) -> String {
    let s = s.trim_start_matches(|c: char| c == ',' || c == ':' || c == '-' || c.is_whitespace()).trim_end();
    let s = TRAILING_CONNECTOR.replace(s.trim_end_matches([',', ';', ':']), "");
    let mut s = s.trim_end_matches([',', ';', ':']).trim().to_string();
    if drop_period && s.ends_with('.') && !s.ends_with("..") {
        s.pop();
    }
    capitalize(&s)
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::format;

    #[test]
    fn english_ordinals_with_intro() {
        assert_eq!(
            format("Here is my plan for today: first, review the pull requests. Second, write the docs. Third, ship the release."),
            "Here is my plan for today:\n1. Review the pull requests\n2. Write the docs\n3. Ship the release"
        );
    }

    #[test]
    fn german_ordinals_in_one_sentence() {
        assert_eq!(
            format("Für morgen erstens die Präsentation fertig machen, zweitens Anna anrufen und drittens Milch kaufen."),
            "Für morgen:\n1. Die Präsentation fertig machen\n2. Anna anrufen\n3. Milch kaufen"
        );
    }

    #[test]
    fn spoken_bullets() {
        assert_eq!(
            format("Shopping list. Bullet point milk. Bullet point eggs. Bullet point bread."),
            "Shopping list.\n- Milk\n- Eggs\n- Bread"
        );
        assert_eq!(
            format("Einkaufsliste: Stichpunkt Milch, Stichpunkt Eier, Stichpunkt Brot."),
            "Einkaufsliste:\n- Milch\n- Eier\n- Brot"
        );
    }

    #[test]
    fn unpunctuated_bullets() {
        assert_eq!(
            format("shopping list bullet point milk bullet point eggs bullet point bread"),
            "Shopping list:\n- Milk\n- Eggs\n- Bread"
        );
    }

    #[test]
    fn inline_digits() {
        assert_eq!(format("Agenda: 1. Budget 2. Hiring 3. Roadmap"), "Agenda:\n1. Budget\n2. Hiring\n3. Roadmap");
    }

    #[test]
    fn number_words_and_finally() {
        assert_eq!(format("Number one, speed. Number two, accuracy."), "1. Speed\n2. Accuracy");
        assert_eq!(format("First, plan it. Second, build it. Finally, ship it."), "1. Plan it\n2. Build it\n3. Ship it");
        assert_eq!(format("Als Erstes testen wir, als Zweites deployen wir."), "1. Testen wir\n2. Deployen wir");
    }

    #[test]
    fn closing_remark_after_list() {
        assert_eq!(
            format("First, fix the login bug. Second, update the docs. That's all for today."),
            "1. Fix the login bug\n2. Update the docs\nThat's all for today."
        );
    }

    #[test]
    fn closing_remark_after_bullets() {
        assert_eq!(
            format("Bullet point milk. Bullet point Äpfel. Übrigens, das war's."),
            "- Milk\n- Äpfel\nÜbrigens, das war's."
        );
    }

    #[test]
    fn multi_sentence_items_keep_their_sentences() {
        assert_eq!(
            format("First, fix the bug. It crashes on Safari. Second, ship. Then celebrate."),
            "1. Fix the bug. It crashes on Safari.\n2. Ship. Then celebrate."
        );
    }

    #[test]
    fn prose_is_left_alone() {
        for text in [
            "First, let me say thanks.",
            "First impressions matter. Second chances too.",
            "We bought a second-hand car, first of its kind.",
            "Version 2. Then we wait.",
            "First, second and third place all get a prize.",
            "Give me a second, I'll be first in line.",
            "Um 5 Uhr treffen wir uns, zuerst im Büro.",
            "We finally shipped it after the first, painful week.",
            "Ich war als Erstes da, dann kam Anna.",
            "Our number one goal is speed. Number two is a myth.",
            "The bullet point about pricing is wrong.",
            "He dodged a bullet and then another bullet.",
        ] {
            assert_eq!(format(text), text, "{text}");
        }
    }

    #[test]
    fn two_lists_in_one_dictation() {
        assert_eq!(
            format("Todos: first, email Anna. Second, book flights. Ideas: bullet point dark mode. Bullet point offline sync."),
            "Todos:\n1. Email Anna\n2. Book flights\nIdeas:\n- Dark mode\n- Offline sync"
        );
    }
}
