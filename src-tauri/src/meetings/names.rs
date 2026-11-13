//! Who is who, from the shared screen: video-call apps (Meet, Teams, Zoom) label each participant's
//! tile with their name and show the person talking (a spotlight tile, a frame around theirs). A
//! frame of the screen is read every 10 s anyway; the names on it are kept with its time
//! (`record`), and at the end each voice gets the name that is on screen while it talks and less so
//! while others talk (`name_speakers`). No name on screen, or none that follows a voice: speakers
//! keep their numbers.

use serde::{Deserialize, Serialize};

/// The names read on one frame of the shared screen.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sighting {
    pub at_ms: u64,
    /// Name-shaped text on the frame (`candidates`); empty when there was none, which counts too.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

/// Frames kept per meeting (four hours of one every 10 s); later ones are not recorded.
pub const MAX_SIGHTINGS: usize = 1_500;

/// Words that may sit lower case inside a name ("Ursula von der Leyen", "Vincent van Gogh").
const PARTICLES: &[&str] = &["von", "van", "der", "den", "de", "da", "di", "del", "du", "la", "le", "zu", "ten", "bin", "al"];

/// Notes call apps put after a name ("Konrad Bauer (Presenter)", "Anna Berg (she/her)"); cut off.
fn without_suffix(cell: &str) -> &str {
    cell.split(['(', '[']).next().unwrap_or(cell).trim()
}

/// Whether `cell` reads like a person's name on a tile: two to four words, each a capital and
/// then lower-case letters (hyphens and apostrophes inside), or a particle between them; a comma
/// may follow the first ("Bauer, Konrad", as Teams shows some). Headings in capitals, sentences,
/// numbers and single words are not.
fn name_shaped(cell: &str) -> bool {
    let words: Vec<&str> = cell.split(' ').collect();
    if !(2..=4).contains(&words.len()) || cell.chars().count() > 40 {
        return false;
    }
    let proper = |w: &str| {
        let mut chars = w.chars();
        chars.next().is_some_and(char::is_uppercase)
            && w.chars().count() >= 2
            && chars.clone().any(char::is_lowercase)
            && chars.all(|c| c.is_lowercase() || c == '-' || c == '\'' || c == '’')
            && !w.ends_with(['-', '\'', '’'])
    };
    let last = words.len() - 1;
    words.iter().enumerate().all(|(i, &w)| {
        let w = if i == 0 { w.strip_suffix(',').unwrap_or(w) } else { w };
        proper(w) || (i > 0 && i < last && PARTICLES.contains(&w))
    })
}

/// Name-shaped text on a frame: whole rows, or whole cells of a row (the OCR joins what sits on
/// one line with " | ", so a tile's label is often the last cell of a line of shared content).
pub fn candidates(rows: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for row in rows {
        for cell in row.split(" | ") {
            let cell = without_suffix(cell);
            if name_shaped(cell) && !out.iter().any(|o| o == cell) {
                out.push(cell.to_string());
            }
        }
    }
    out
}

/// Keeps the names on a frame of the shared screen read at meeting time `at_ms` (every frame, also
/// one without names or one showing the same screen as before: who is on screen changes while the
/// content stays).
pub fn record(sightings: &mut Vec<Sighting>, rows: &[String], at_ms: u64) {
    if sightings.len() < MAX_SIGHTINGS {
        sightings.push(Sighting { at_ms, names: candidates(rows) });
    }
}

/// A name must be on this many frames while one voice talks to be theirs...
const MIN_FRAMES: usize = 3;
/// ...and on this share of the frames while that voice talks...
const MIN_SHARE: f32 = 0.3;
/// ...and this much more often than while the others talk: a page title or a gallery of every
/// participant's tile is on screen whoever talks, the active speaker's tile only while they do...
const MIN_LEAD: f32 = 0.25;
/// ...so much more often that chance would do it less than once in this many (a voice heard on few
/// frames isn't named after a page that happened to be up while it talked: 4 frames of 9 with a
/// title shown on 16 % of the others' frames happen once in 25).
const BY_CHANCE: f64 = 0.001;

/// The chance of `k` or more hits in `n` tries that each hit with chance `p`.
fn at_least(n: usize, k: usize, p: f64) -> f64 {
    if k == 0 {
        return 1.0;
    }
    if k > n || p <= 0.0 {
        return 0.0;
    }
    if p >= 1.0 {
        return 1.0;
    }
    // The first term in logs (n can be large), the rest by the ratio of neighbours.
    let ln_choose: f64 = (0..k).map(|i| ((n - i) as f64).ln() - ((i + 1) as f64).ln()).sum();
    let mut term = (ln_choose + k as f64 * p.ln() + (n - k) as f64 * (1.0 - p).ln()).exp();
    let mut sum = 0.0;
    for i in k..=n {
        sum += term;
        term *= (n - i) as f64 / (i + 1) as f64 * p / (1.0 - p);
    }
    sum.min(1.0)
}

/// Edits between two readings of one name the OCR may make: one per eight letters, at least one.
fn misreads(name: &str) -> usize {
    (name.chars().count() / 8).max(1)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (diagonal + usize::from(ca != *cb)).min(row[j] + 1).min(row[j + 1] + 1);
            diagonal = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// Each name seen with the frames it is on, readings the OCR got slightly wrong ("Ebersberqer")
/// counted under the reading seen most often.
fn frames_by_name(sightings: &[Sighting]) -> Vec<(String, Vec<usize>)> {
    let mut seen: Vec<(String, Vec<usize>)> = Vec::new();
    for (f, s) in sightings.iter().enumerate() {
        for n in &s.names {
            match seen.iter_mut().find(|(m, _)| m == n) {
                Some((_, frames)) => frames.push(f),
                None => seen.push((n.clone(), vec![f])),
            }
        }
    }
    seen.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    let mut out: Vec<(String, Vec<usize>)> = Vec::new();
    for (name, frames) in seen {
        let words = name.split(' ').count();
        let same = out.iter_mut().find(|(m, _)| m.split(' ').count() == words && edit_distance(m, &name) <= misreads(m));
        match same {
            Some((_, kept)) => {
                kept.extend(frames);
                kept.sort_unstable();
                kept.dedup();
            }
            None => out.push((name, frames)),
        }
    }
    out
}

/// Who talks at `at_ms`: the speaker of the segments around it, if only one does.
fn voice_at(segments: &[super::Segment], at_ms: u64) -> Option<u32> {
    let mut talking = segments.iter().filter(|s| s.start_ms <= at_ms && at_ms <= s.end_ms).map(|s| s.speaker);
    let first = talking.next()?;
    talking.all(|id| id == first).then_some(first)
}

/// Which name on screen goes with which voice: for each speaker of `segments`, the share of frames
/// while they talk that show a name, against the share while someone else talks. Pairs that pass
/// `MIN_FRAMES`, `MIN_SHARE`, `MIN_LEAD` and `BY_CHANCE` are taken by the widest lead first, each
/// speaker and each name once. Returns (speaker id, name).
pub fn links(sightings: &[Sighting], segments: &[super::Segment]) -> Vec<(u32, String)> {
    let voices: Vec<Option<u32>> = sightings.iter().map(|s| voice_at(segments, s.at_ms)).collect();
    let talked = voices.iter().flatten().count();
    let mut ids: Vec<u32> = voices.iter().flatten().copied().collect();
    ids.sort_unstable();
    ids.dedup();
    let mut pairs: Vec<(f32, u32, String)> = Vec::new();
    for (name, frames) in frames_by_name(sightings) {
        for &id in &ids {
            let own = voices.iter().filter(|v| **v == Some(id)).count();
            let others = talked - own;
            let with = frames.iter().filter(|&&f| voices[f] == Some(id)).count();
            let elsewhere = frames.iter().filter(|&&f| voices[f].is_some_and(|v| v != id)).count();
            let share = with as f32 / own.max(1) as f32;
            let lead = share - elsewhere as f32 / others.max(1) as f32;
            // Never on screen while others talked could still have been once (and with no others
            // to compare, nothing tells this voice's name from the screen's).
            let usual = elsewhere.max(1) as f64 / others.max(1) as f64;
            if with >= MIN_FRAMES && share >= MIN_SHARE && lead >= MIN_LEAD && at_least(own, with, usual) < BY_CHANCE {
                pairs.push((lead, id, name.clone()));
            }
        }
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut out: Vec<(u32, String)> = Vec::new();
    for (_, id, name) in pairs {
        if !out.iter().any(|(i, n)| *i == id || *n == name) {
            out.push((id, name));
        }
    }
    out
}

/// Names the speakers of `segments` after the names on screen while they talk (`links`). "Me" and
/// speakers the user named keep their names, and a name the user gave someone isn't given again.
pub fn name_speakers(sightings: &[Sighting], segments: &[super::Segment], speakers: &mut [super::Speaker]) {
    let given: Vec<String> = speakers.iter().filter(|s| s.renamed).map(|s| s.name.to_lowercase()).collect();
    for (id, name) in links(sightings, segments) {
        if id == super::ME || given.contains(&name.to_lowercase()) {
            continue;
        }
        if let Some(s) = speakers.iter_mut().find(|s| s.id == id && !s.renamed) {
            s.name = name;
        }
    }
}

/// Names on at least `MIN_FRAMES` frames, the most seen first: the people on the call's video
/// tiles, whether or not a voice could be tied to them yet (for the map's last look, which runs
/// before the final speakers).
pub fn on_screen(sightings: &[Sighting]) -> Vec<String> {
    frames_by_name(sightings).into_iter().filter(|(_, frames)| frames.len() >= MIN_FRAMES).map(|(name, _)| name).collect()
}

/// The people the screen showed with a voice (`links`), for the notes: "Me" by the name on screen
/// while the user talked, as "Name (Me)".
pub fn participants(sightings: &[Sighting], segments: &[super::Segment]) -> Vec<String> {
    links(sightings, segments).into_iter().map(|(id, name)| if id == super::ME { format!("{name} (Me)") } else { name }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn people_on_screen_are_names_on_several_frames() {
        let mut sightings = Vec::new();
        for (i, rows) in [vec!["Ada Lovelace"], vec!["Ada Lovelace", "Alan Turing"], vec!["Ada Lovelace"], vec!["Alan Turing"], vec!["Grace Hopper"]].iter().enumerate() {
            record(&mut sightings, &rows.iter().map(|r| r.to_string()).collect::<Vec<_>>(), i as u64 * 10_000);
        }
        // On three frames or more; once or twice is a page that went by.
        assert_eq!(on_screen(&sightings), ["Ada Lovelace"]);
    }

    #[test]
    fn tile_labels_are_found_among_shared_content() {
        let frame = rows(&[
            "ALPENTEC | Alle Anlagen / KX8 Nordhang | 29% | Work-in-Progress",
            "=001_LVD | Anzahl Stützen | 12 | Anzahl zulässige Förderleistungen | Konrad Bauer",
            "ZULÄSSIGE FÖRDERLEISTUNGEN",
            "Oskar Ebersberger (Presenter)",
            "You may duplicate whole Plants, or just Stations or Wings within one plant.",
            "Ursula von der Leyen",
            "Förderleistung 1:61 | R2-D2 | Konrad Bauer",
            "Bauer, Konrad (Guest)",
            "Konrad Bauer, Oskar",
        ]);
        assert_eq!(candidates(&frame), ["Anzahl Stützen", "Konrad Bauer", "Oskar Ebersberger", "Ursula von der Leyen", "Bauer, Konrad"]);
    }

    use super::super::{Channel, Segment, Speaker, ME};

    fn seg(speaker: u32, from_s: u64, to_s: u64) -> Segment {
        let channel = if speaker == ME { Channel::Mic } else { Channel::System };
        Segment { start_ms: from_s * 1000, end_ms: to_s * 1000, channel, speaker, text: String::new(), language: None, fixes: Vec::new() }
    }

    fn speaker(id: u32, name: &str, renamed: bool) -> Speaker {
        Speaker { id, name: name.into(), renamed }
    }

    /// A call of 30 turns of 20 s: speaker 1 presents, 2 and 3 ask now and then, "Me" answers
    /// once. A frame every 10 s shows a spotlight tile of whoever talks, a page title most of the
    /// time and the tile of someone who never talks.
    fn spotlight_call(tile: impl Fn(u32) -> &'static str) -> (Vec<Sighting>, Vec<Segment>) {
        let order = [1, 1, 2, 1, 1, 3, 1, 1, 2, 1, 1, 1, 3, 1, 2, 1, ME, 1, 1, 3, 1, 1, 2, 1, 1, 3, 1, 2, 1, 1];
        let segments: Vec<Segment> = order.iter().enumerate().map(|(i, &s)| seg(s, i as u64 * 20, i as u64 * 20 + 19)).collect();
        let sightings = (0..order.len() as u64 * 2)
            .map(|f| {
                let t = f * 10 + 5;
                let talking = segments.iter().find(|s| s.start_ms <= t * 1000 && t * 1000 <= s.end_ms).map(|s| s.speaker);
                let mut names = vec!["Lena Hofer".to_string()];
                if f % 4 != 0 {
                    names.push("Power Center".into());
                }
                names.extend(talking.map(|s| tile(s).to_string()));
                Sighting { at_ms: t * 1000, names }
            })
            .collect();
        (sightings, segments)
    }

    fn tile(id: u32) -> &'static str {
        match id {
            ME => "Tobias Dorfer",
            1 => "Konrad Bauer",
            2 => "Oskar Ebersberger",
            _ => "Anna Berg",
        }
    }

    #[test]
    fn voices_are_named_after_the_tile_shown_while_they_talk() {
        let (sightings, segments) = spotlight_call(tile);
        let mut speakers = vec![speaker(ME, "Me", false), speaker(1, "Speaker 1", false), speaker(2, "Speaker 2", false), speaker(3, "Speaker 3", false)];
        name_speakers(&sightings, &segments, &mut speakers);
        let names: Vec<&str> = speakers.iter().map(|s| s.name.as_str()).collect();
        // The page title and the silent participant's tile are on screen whoever talks: no one's.
        assert_eq!(names, ["Me", "Konrad Bauer", "Oskar Ebersberger", "Anna Berg"]);
        assert!(speakers.iter().all(|s| !s.renamed), "names from the screen aren't the user's");
        assert_eq!(participants(&sightings, &segments), ["Konrad Bauer", "Oskar Ebersberger", "Anna Berg"]);
    }

    #[test]
    fn the_users_names_stay_and_are_not_given_twice() {
        let (sightings, segments) = spotlight_call(tile);
        let mut speakers = vec![speaker(ME, "Me", false), speaker(1, "Konrad", true), speaker(2, "Anna Berg", true), speaker(3, "Speaker 3", false)];
        name_speakers(&sightings, &segments, &mut speakers);
        let names: Vec<&str> = speakers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Me", "Konrad", "Anna Berg", "Speaker 3"]);
    }

    #[test]
    fn misread_names_count_as_one_and_me_takes_mine_without_a_new_name() {
        // Speaker 3's turns are the user's ("Me", their tile shown), and a third of Oskar's tiles misread.
        let (mut sightings, mut segments) = spotlight_call(|id| if id == 3 { tile(ME) } else { tile(id) });
        for s in segments.iter_mut().filter(|s| s.speaker == 3) {
            s.speaker = ME;
        }
        for s in sightings.iter_mut().step_by(3) {
            for n in s.names.iter_mut().filter(|n| *n == "Oskar Ebersberger") {
                *n = "Oskar Ebersberqer".into();
            }
        }
        let mut speakers = vec![speaker(ME, "Me", false), speaker(1, "Speaker 1", false), speaker(2, "Speaker 2", false)];
        name_speakers(&sightings, &segments, &mut speakers);
        assert_eq!(speakers.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Me", "Konrad Bauer", "Oskar Ebersberger"]);
        assert_eq!(participants(&sightings, &segments), ["Konrad Bauer", "Tobias Dorfer (Me)", "Oskar Ebersberger"]);
    }

    #[test]
    fn no_names_or_names_that_follow_no_voice_leave_the_numbers() {
        let (_, segments) = spotlight_call(tile);
        let mut speakers = vec![speaker(ME, "Me", false), speaker(1, "Speaker 1", false), speaker(2, "Speaker 2", false)];
        name_speakers(&[], &segments, &mut speakers);
        // A gallery: every participant's tile on every frame.
        let gallery: Vec<Sighting> = (0..60)
            .map(|f| Sighting { at_ms: f * 10_000 + 5_000, names: ["Konrad Bauer", "Oskar Ebersberger", "Anna Berg"].map(String::from).to_vec() })
            .collect();
        name_speakers(&gallery, &segments, &mut speakers);
        assert_eq!(speakers.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Me", "Speaker 1", "Speaker 2"]);
    }

    #[test]
    fn chance_of_hits() {
        assert!((at_least(9, 4, 0.158) - 0.040).abs() < 0.001);
        assert!((at_least(9, 4, 0.02) - 1.86e-5).abs() < 1e-7);
        assert!((at_least(4, 0, 0.5) - 1.0).abs() < 1e-9);
        assert!((at_least(4, 4, 0.5) - 0.0625).abs() < 1e-9);
        assert!(at_least(1_500, 1_400, 0.5) < 1e-200);
        assert_eq!(at_least(3, 4, 0.5), 0.0);
    }

    #[test]
    fn a_page_up_while_a_quiet_voice_talked_names_no_one() {
        // Speaker 2 talks on 9 frames; a page title is up on 4 of them and on a tenth of the others:
        // shares and lead would do, chance wouldn't.
        let segments: Vec<Segment> = (0..60).map(|i| seg(if i % 6 == 0 && i < 54 { 2 } else { 1 }, i * 10, i * 10 + 9)).collect();
        let sightings: Vec<Sighting> = (0..60u64)
            .map(|i| {
                let quiet = i % 6 == 0 && i < 54;
                let page = if quiet { i < 24 } else { i % 12 == 1 };
                Sighting { at_ms: i * 10_000 + 5_000, names: if page { vec!["Software System".into()] } else { vec![] } }
            })
            .collect();
        assert_eq!(links(&sightings, &segments), []);
    }

    #[test]
    fn every_frame_is_recorded_until_the_cap() {
        let mut s = Vec::new();
        record(&mut s, &rows(&["Konrad Bauer"]), 10_000);
        record(&mut s, &rows(&["nothing here"]), 20_000);
        assert_eq!(s, [Sighting { at_ms: 10_000, names: vec!["Konrad Bauer".into()] }, Sighting { at_ms: 20_000, names: vec![] }]);
        let mut full = vec![Sighting::default(); MAX_SIGHTINGS];
        record(&mut full, &rows(&["Konrad Bauer"]), 30_000);
        assert_eq!(full.len(), MAX_SIGHTINGS);
    }
}
