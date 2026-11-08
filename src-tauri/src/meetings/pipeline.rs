//! The meeting pipeline without any app state, so the app and `wordfall --meeting` share it:
//! cutting a live audio stream into utterances at pauses, transcribing them with timestamps,
//! dropping microphone echo of the other side, and labelling speakers.

use std::collections::HashMap;

use crate::{
    postprocess,
    speakers::{self, Embedder},
    store::DictEntry,
    transcribe::{DecodeOptions, Engine},
};

use super::{Channel, Segment, Speaker};

const RATE: usize = 16_000;
/// Audio waits until there's at least this much before a pass looks at it.
const MIN_PENDING: usize = 3 * RATE;
/// Speech counts as finished once this much silence follows it.
const SETTLE: usize = RATE * 8 / 10;
/// Without a pause for this long, the audio is cut anyway.
const FORCE: usize = 30 * RATE;
/// Speech separated by shorter pauses is transcribed together (whisper likes context)...
const MERGE_GAP: usize = RATE * 12 / 10;
/// ...up to this length.
const MAX_UTTERANCE: usize = 25 * RATE;

pub struct Ctx<'a> {
    pub engine: &'a Engine,
    pub embedder: Option<&'a Embedder>,
    pub languages: &'a [String],
    pub prompt: Option<&'a str>,
    pub decode: &'a DecodeOptions,
    pub dictionary: Option<&'a [DictEntry]>,
    pub remove_fillers: bool,
    /// Called before each use of the recognizer; blocks while a dictation needs it.
    pub yield_to_dictation: &'a dyn Fn(),
}

/// A transcribed stretch of one channel, before speakers are known.
pub struct Heard {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub language: Option<String>,
    pub embedding: Option<Vec<f32>>,
}

/// Holds one channel's not-yet-transcribed audio and cuts it into utterances at pauses.
pub struct ChannelProcessor {
    pub channel: Channel,
    pending: Vec<f32>,
    /// Position of `pending[0]` in the whole recording, in samples.
    offset: u64,
    /// Language of the previous utterance; ambiguous ones stay in it.
    pub language: Option<String>,
}

impl ChannelProcessor {
    pub fn new(channel: Channel) -> Self {
        Self { channel, pending: Vec::new(), offset: 0, language: None }
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.pending.extend_from_slice(samples);
    }

    pub fn pending_samples(&self) -> usize {
        self.pending.len()
    }

    /// Transcribes the speech that has finished (all of it with `flush`) and forgets that audio.
    pub fn process(&mut self, ctx: &Ctx, flush: bool) -> anyhow::Result<Vec<Heard>> {
        let len = self.pending.len();
        if len == 0 || (!flush && len < MIN_PENDING) {
            return Ok(Vec::new());
        }
        let regions = ctx.engine.speech_regions(&self.pending)?;
        let (done, cut): (Vec<std::ops::Range<usize>>, usize) = if flush {
            (regions, len)
        } else {
            let settled: Vec<_> = regions.iter().filter(|r| r.end + SETTLE <= len).cloned().collect();
            match (settled.last(), regions.is_empty()) {
                // Only silence so far: keep a second in case speech is starting.
                (None, true) => (Vec::new(), len.saturating_sub(RATE)),
                // Someone has been talking without a pause for a long time: cut anyway.
                (None, false) if len >= FORCE => (regions, len),
                (None, false) => return Ok(Vec::new()),
                (Some(last), _) => {
                    let end = last.end;
                    (settled, end)
                }
            }
        };

        let mut utterances: Vec<std::ops::Range<usize>> = Vec::new();
        for r in done {
            match utterances.last_mut() {
                Some(u) if r.start <= u.end + MERGE_GAP && r.end - u.start <= MAX_UTTERANCE => u.end = r.end,
                _ => utterances.push(r),
            }
        }
        let mut out = Vec::new();
        for u in utterances {
            let audio = &self.pending[u.clone()];
            if audio.len() < RATE / 4 {
                continue;
            }
            (ctx.yield_to_dictation)();
            let (timed, lang) = ctx.engine.transcribe_timed(audio, ctx.languages, self.language.as_deref(), ctx.prompt, ctx.decode)?;
            if lang.is_some() {
                self.language = lang;
            }
            let rms = (audio.iter().map(|s| s * s).sum::<f32>() / audio.len() as f32).sqrt();
            let base_ms = (self.offset + u.start as u64) * 1000 / RATE as u64;
            let len_ms = audio.len() as u64 * 1000 / RATE as u64;
            for t in timed {
                let cleaned = postprocess::clean(
                    &t.text,
                    &postprocess::Options {
                        remove_fillers: ctx.remove_fillers,
                        format_lists: false,
                        language: self.language.as_deref(),
                        dictionary: ctx.dictionary,
                        quiet_input: rms < 0.01,
                    },
                );
                if cleaned.text.is_empty() {
                    continue;
                }
                let (start, end) = (t.start_ms.min(len_ms), t.end_ms.min(len_ms).max(t.start_ms.min(len_ms)));
                let embedding = match ctx.embedder {
                    Some(e) => {
                        let (a, b) = ((start as usize * RATE / 1000).min(audio.len()), (end as usize * RATE / 1000).min(audio.len()));
                        e.embed(&audio[a..b]).unwrap_or_else(|err| {
                            log::warn!("speaker embedding failed: {err:#}");
                            None
                        })
                    }
                    None => None,
                };
                out.push(Heard {
                    start_ms: base_ms + start,
                    end_ms: base_ms + end,
                    text: cleaned.text,
                    language: self.language.clone(),
                    embedding,
                });
            }
        }
        let cut = cut.min(len);
        self.pending.drain(..cut);
        self.offset += cut as u64;
        Ok(out)
    }
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// Whether a microphone segment is the other side of the call coming out of the speakers:
/// mostly the same words as system audio at the same time.
pub fn is_echo(mic: &Segment, system: &[Segment]) -> bool {
    const SLACK_MS: u64 = 1500;
    let heard: std::collections::HashSet<String> = system
        .iter()
        .filter(|s| s.channel == Channel::System && s.start_ms <= mic.end_ms + SLACK_MS && s.end_ms + SLACK_MS >= mic.start_ms)
        .flat_map(|s| words(&s.text))
        .collect();
    if heard.is_empty() {
        return false;
    }
    let mine = words(&mic.text);
    if mine.is_empty() {
        return false;
    }
    let shared = mine.iter().filter(|w| heard.contains(*w)).count();
    shared as f32 / mine.len() as f32 >= 0.6
}

/// Who is "Me", and what to call the other side when voices aren't told apart.
pub const ME: u32 = 0;
const OTHERS: u32 = 1;

pub fn default_name(id: u32, diarize: bool) -> String {
    match id {
        ME => "Me".into(),
        OTHERS if !diarize => "Others".into(),
        n => format!("Speaker {n}"),
    }
}

/// Speaker labels while the meeting is still running: each new segment joins the closest voice
/// heard so far on its channel. The final labels come from `assign_speakers` at the end.
pub struct LiveSpeakers {
    diarize: bool,
    voices: HashMap<Channel, Vec<(Vec<f32>, u32)>>,
    last: HashMap<Channel, u32>,
    next_id: u32,
}

impl LiveSpeakers {
    pub fn new(diarize: bool, existing: &[Speaker]) -> Self {
        let next_id = existing.iter().map(|s| s.id + 1).max().unwrap_or(1).max(1);
        Self { diarize, voices: HashMap::new(), last: HashMap::new(), next_id }
    }

    pub fn assign(&mut self, channel: Channel, embedding: Option<&[f32]>, speakers: &mut Vec<Speaker>) -> u32 {
        let id = if !self.diarize {
            if channel == Channel::Mic { ME } else { OTHERS }
        } else {
            let voices = self.voices.entry(channel).or_default();
            match embedding {
                Some(e) => {
                    let best = voices
                        .iter_mut()
                        .map(|(sum, id)| (speakers::cosine_sim(sum, e), sum, *id))
                        .max_by(|a, b| a.0.total_cmp(&b.0));
                    match best {
                        Some((sim, sum, id)) if sim >= 0.55 => {
                            sum.iter_mut().zip(e).for_each(|(s, x)| *s += x);
                            id
                        }
                        _ => {
                            // The first voice on the microphone is taken to be the user.
                            let id = if channel == Channel::Mic && voices.is_empty() { ME } else { self.take_id() };
                            self.voices.entry(channel).or_default().push((e.to_vec(), id));
                            id
                        }
                    }
                }
                None => match self.last.get(&channel) {
                    Some(&id) => id,
                    None if channel == Channel::Mic => ME,
                    None => {
                        let id = self.take_id();
                        id
                    }
                },
            }
        };
        self.last.insert(channel, id);
        if !speakers.iter().any(|s| s.id == id) {
            speakers.push(Speaker { id, name: default_name(id, self.diarize), renamed: false });
            speakers.sort_by_key(|s| s.id);
        }
        id
    }

    fn take_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// Final speaker labels for the whole meeting: clusters each channel's voices, makes the
/// microphone's main voice "Me", numbers everyone else by first appearance, and keeps names the
/// user gave while the meeting ran.
pub fn assign_speakers(segments: &mut [Segment], embeddings: &[Option<Vec<f32>>], speakers: &mut Vec<Speaker>, diarize: bool) {
    let old_names: HashMap<u32, (String, bool)> = speakers.iter().map(|s| (s.id, (s.name.clone(), s.renamed))).collect();
    let old_ids: Vec<u32> = segments.iter().map(|s| s.speaker).collect();
    // (channel, cluster) per segment.
    let mut label: Vec<(Channel, usize)> = vec![(Channel::Mic, 0); segments.len()];
    let mut mic_me = 0;
    for channel in [Channel::Mic, Channel::System] {
        let idx: Vec<usize> = (0..segments.len()).filter(|&i| segments[i].channel == channel).collect();
        if idx.is_empty() {
            continue;
        }
        let items: Vec<speakers::Item> = idx
            .iter()
            .map(|&i| speakers::Item {
                embedding: if diarize { embeddings.get(i).and_then(|e| e.as_deref()) } else { None },
                start_ms: segments[i].start_ms,
                end_ms: segments[i].end_ms,
            })
            .collect();
        let (labels, n) = speakers::cluster(&items);
        if channel == Channel::Mic {
            mic_me = speakers::dominant(&labels, &items, n);
        }
        for (k, &i) in idx.iter().enumerate() {
            label[i] = (channel, labels[k]);
        }
    }
    // Ids: Me first, then by first appearance in time.
    let mut order: Vec<usize> = (0..segments.len()).collect();
    order.sort_by_key(|&i| segments[i].start_ms);
    let mut ids: HashMap<(Channel, usize), u32> = HashMap::new();
    ids.insert((Channel::Mic, mic_me), ME);
    let mut next = 1;
    for &i in &order {
        ids.entry(label[i]).or_insert_with(|| {
            let id = next;
            next += 1;
            id
        });
    }
    for (i, s) in segments.iter_mut().enumerate() {
        s.speaker = ids[&label[i]];
    }
    // Names: a speaker keeps the name the user gave the provisional speaker it mostly was.
    let mut new_speakers: Vec<Speaker> = Vec::new();
    let mut used_old: Vec<u32> = Vec::new();
    let mut new_ids: Vec<u32> = ids.values().copied().collect();
    new_ids.sort();
    new_ids.dedup();
    for id in new_ids {
        let mut votes: HashMap<u32, u64> = HashMap::new();
        for (i, s) in segments.iter().enumerate() {
            if s.speaker == id {
                *votes.entry(old_ids[i]).or_default() += s.end_ms.saturating_sub(s.start_ms) + 1;
            }
        }
        let inherited = votes
            .into_iter()
            .max_by_key(|(_, ms)| *ms)
            .and_then(|(old, _)| old_names.get(&old).filter(|(_, renamed)| *renamed).map(|(n, _)| (old, n.clone())))
            .filter(|(old, _)| !used_old.contains(old));
        let speaker = match inherited {
            Some((old, name)) => {
                used_old.push(old);
                Speaker { id, name, renamed: true }
            }
            None => Speaker { id, name: default_name(id, diarize), renamed: false },
        };
        new_speakers.push(speaker);
    }
    *speakers = new_speakers;
}

/// Drops microphone segments that are echo of system audio; returns how many were dropped.
pub fn drop_echo(segments: &mut Vec<Segment>, embeddings: &mut Vec<Option<Vec<f32>>>) -> usize {
    let system: Vec<Segment> = segments.iter().filter(|s| s.channel == Channel::System).cloned().collect();
    if system.is_empty() {
        return 0;
    }
    let keep: Vec<bool> = segments.iter().map(|s| s.channel == Channel::System || !is_echo(s, &system)).collect();
    let dropped = keep.iter().filter(|k| !**k).count();
    let mut i = 0;
    segments.retain(|_| {
        i += 1;
        keep[i - 1]
    });
    let mut i = 0;
    embeddings.retain(|_| {
        i += 1;
        keep.get(i - 1).copied().unwrap_or(true)
    });
    dropped
}

fn clock(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// Transcript lines for the summary: consecutive segments of one speaker become one line.
pub fn transcript_lines(segments: &[Segment], speakers: &[Speaker]) -> Vec<String> {
    let name = |id: u32| speakers.iter().find(|s| s.id == id).map(|s| s.name.clone()).unwrap_or_else(|| default_name(id, true));
    let mut sorted: Vec<&Segment> = segments.iter().collect();
    sorted.sort_by_key(|s| s.start_ms);
    let mut lines: Vec<(u64, u32, String)> = Vec::new();
    for s in sorted {
        match lines.last_mut() {
            Some((_, sp, text)) if *sp == s.speaker && text.len() < 1200 => {
                text.push(' ');
                text.push_str(&s.text);
            }
            _ => lines.push((s.start_ms, s.speaker, s.text.clone())),
        }
    }
    lines.into_iter().map(|(ms, sp, text)| format!("[{}] {}: {}", clock(ms), name(sp), text)).collect()
}

/// The language most of the meeting was in, weighted by how much was said.
pub fn meeting_language(segments: &[Segment]) -> Option<String> {
    let mut weight: HashMap<&str, usize> = HashMap::new();
    for s in segments {
        if let Some(l) = s.language.as_deref() {
            *weight.entry(l).or_default() += s.text.len();
        }
    }
    weight.into_iter().max_by_key(|(_, w)| *w).map(|(l, _)| l.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(channel: Channel, start: u64, end: u64, text: &str) -> Segment {
        Segment { start_ms: start, end_ms: end, channel, speaker: 0, text: text.into(), language: None }
    }

    #[test]
    fn echo_of_the_other_side_is_detected() {
        let system = vec![seg(Channel::System, 1000, 4000, "We should ship the billing page on Thursday.")];
        assert!(is_echo(&seg(Channel::Mic, 1200, 4100, "we should ship the billing page Thursday"), &system));
        assert!(!is_echo(&seg(Channel::Mic, 1200, 4100, "Sounds good, I'll prepare the slides."), &system));
        assert!(!is_echo(&seg(Channel::Mic, 9000, 9900, "we should ship the billing page"), &system));
    }

    #[test]
    fn transcript_lines_merge_one_speakers_turns() {
        let speakers = vec![Speaker { id: 0, name: "Me".into(), renamed: false }, Speaker { id: 1, name: "Anna".into(), renamed: true }];
        let mut a = seg(Channel::Mic, 0, 1000, "Hello.");
        a.speaker = 0;
        let mut b = seg(Channel::Mic, 1000, 2000, "Shall we start?");
        b.speaker = 0;
        let mut c = seg(Channel::System, 65_000, 66_000, "Yes.");
        c.speaker = 1;
        assert_eq!(transcript_lines(&[a, b, c], &speakers), ["[00:00] Me: Hello. Shall we start?", "[01:05] Anna: Yes."]);
    }

    #[test]
    fn final_labels_keep_user_names() {
        let a: Vec<f32> = (0..8).map(|i| if i == 1 { 1.0 } else { 0.05 }).collect();
        let b: Vec<f32> = (0..8).map(|i| if i == 5 { 1.0 } else { 0.05 }).collect();
        let mut segments = vec![
            seg(Channel::Mic, 0, 6000, "Hi"),
            seg(Channel::System, 7000, 13_000, "Hello"),
            seg(Channel::System, 14_000, 20_000, "Hey"),
        ];
        segments[1].speaker = 7;
        segments[2].speaker = 7;
        let embeddings = vec![Some(a.clone()), Some(b.clone()), Some(b)];
        let mut speakers = vec![Speaker { id: 0, name: "Me".into(), renamed: false }, Speaker { id: 7, name: "Anna".into(), renamed: true }];
        assign_speakers(&mut segments, &embeddings, &mut speakers, true);
        assert_eq!(segments.iter().map(|s| s.speaker).collect::<Vec<_>>(), [0, 1, 1]);
        assert_eq!(speakers.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Me", "Anna"]);
    }
}
