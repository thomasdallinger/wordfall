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
/// Silero pads speech by 200 ms on each side; the cut where the other channel starts talking
/// sits this far inside its padded region, close to where the other side really starts.
const EDGE_INSET: u64 = RATE as u64 / 10;
/// Pieces shorter than this aren't cut off an utterance.
const MIN_PIECE: usize = RATE / 2;
/// A pause in the channel's own speech this close to an edge of the other channel's is cut at
/// instead of the edge.
const PAUSE_REACH: u64 = RATE as u64;
/// Clips shorter than this keep the channel's language: there's too little speech to switch on,
/// and a wrong switch makes whisper translate or invent ("Die Aufgaben sind" → "Fuck up.").
const SHORT_CLIP: usize = 4 * RATE;
/// The previous text of a channel is whisper's prompt for the next utterance if it ended at most
/// this long before, so a sentence that goes on reads on (no capital, same punctuation).
const CONTEXT_GAP_MS: u64 = 10_000;
/// Characters of previous text kept as context.
const CONTEXT_CHARS: usize = 220;
/// Silence appended to each utterance: whisper tends to drop a last word that ends the audio.
const TAIL_PAD: usize = RATE * 3 / 10;
/// Speech found on a channel, as sample ranges of the whole recording, is kept this long.
const SPEECH_MEMORY: u64 = 10 * 60 * RATE as u64;

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
    /// Where this channel has had speech, in samples of the whole recording. The microphone cuts
    /// its utterances where system audio starts or stops, so the user's words and echo of the
    /// other side don't end up in one segment.
    pub speech: Vec<std::ops::Range<u64>>,
    /// The end of the previous text (for whisper's prompt) and where it ended, in ms.
    context: Option<(String, u64)>,
}

impl ChannelProcessor {
    pub fn new(channel: Channel) -> Self {
        Self { channel, pending: Vec::new(), offset: 0, language: None, speech: Vec::new(), context: None }
    }

    /// Starts in `language` if this channel hasn't settled on one yet (the microphone takes the
    /// language the other side speaks).
    pub fn adopt_language(&mut self, language: Option<&str>) {
        if self.language.is_none() {
            self.language = language.map(str::to_string);
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.pending.extend_from_slice(samples);
    }

    pub fn pending_samples(&self) -> usize {
        self.pending.len()
    }

    /// Transcribes the speech that has finished (all of it with `flush`) and forgets that audio.
    /// `other_speech` is the other channel's `speech`: utterances are cut at its edges.
    pub fn process(&mut self, ctx: &Ctx, flush: bool, other_speech: &[std::ops::Range<u64>]) -> anyhow::Result<Vec<Heard>> {
        let len = self.pending.len();
        if len == 0 || (!flush && len < MIN_PENDING) {
            return Ok(Vec::new());
        }
        let regions = ctx.engine.speech_regions(&self.pending)?;
        // Regions past the last cut were provisional; this pass found them again (or longer).
        let offset = self.offset;
        self.speech.retain(|r| r.end <= offset && r.end + SPEECH_MEMORY > offset);
        self.speech.extend(regions.iter().map(|r| offset + r.start as u64..offset + r.end as u64));
        let (done, cut): (Vec<std::ops::Range<usize>>, usize) = if flush {
            (regions, len)
        } else {
            let mut settled: Vec<_> = regions.iter().filter(|r| r.end + SETTLE <= len).cloned().collect();
            // A short bit of speech that would be transcribed together with what is still being
            // said waits for it: cut off by a pass on its own ("Ja, mache ich."), it is too little
            // for whisper, which then mishears it or invents something. Longer speech doesn't
            // wait, or utterances would grow long, which whisper punctuates worse.
            if let (Some(next), Some(held_end)) = (regions.get(settled.len()), settled.last().map(|r| r.end)) {
                let mut held_start = next.start;
                while let Some(last) = settled.last() {
                    if held_start <= last.end + MERGE_GAP && held_end - last.start < SHORT_CLIP {
                        held_start = last.start;
                        settled.pop();
                    } else {
                        break;
                    }
                }
            }
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

        // Utterances as the speech regions they're made of.
        let mut grouped: Vec<Vec<std::ops::Range<usize>>> = Vec::new();
        for r in done {
            match grouped.last_mut() {
                Some(g) if r.start <= g[g.len() - 1].end + MERGE_GAP && r.end - g[0].start <= MAX_UTTERANCE => g.push(r),
                _ => grouped.push(vec![r]),
            }
        }
        let utterances: Vec<std::ops::Range<usize>> =
            grouped.iter().flat_map(|g| split_at_edges(g, other_speech, offset)).collect();
        let mut out = Vec::new();
        for u in utterances {
            let audio = &self.pending[u.clone()];
            if audio.len() < RATE / 4 {
                continue;
            }
            let base_ms = (self.offset + u.start as u64) * 1000 / RATE as u64;
            let len_ms = audio.len() as u64 * 1000 / RATE as u64;
            let languages = match &self.language {
                Some(l) if audio.len() < SHORT_CLIP && ctx.languages.contains(l) => std::slice::from_ref(l),
                _ => ctx.languages,
            };
            let context = self
                .context
                .as_ref()
                .filter(|(_, end)| base_ms.saturating_sub(*end) <= CONTEXT_GAP_MS)
                .map(|(text, _)| text.as_str());
            let prompt = match (ctx.prompt.filter(|p| !p.is_empty()), context) {
                (Some(p), Some(c)) => Some(format!("{p} {c}")),
                (p, c) => p.or(c).map(str::to_string),
            };
            let mut padded = Vec::with_capacity(audio.len() + TAIL_PAD);
            padded.extend_from_slice(audio);
            padded.resize(audio.len() + TAIL_PAD, 0.0);
            (ctx.yield_to_dictation)();
            let (timed, lang) =
                ctx.engine.transcribe_timed(&padded, languages, self.language.as_deref(), prompt.as_deref(), ctx.decode)?;
            if lang.is_some() {
                self.language = lang;
            }
            let rms = (audio.iter().map(|s| s * s).sum::<f32>() / audio.len() as f32).sqrt();
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
                let joined = match self.context.take().filter(|(_, e)| (base_ms + start).saturating_sub(*e) <= CONTEXT_GAP_MS) {
                    Some((before, _)) => format!("{before} {}", cleaned.text),
                    None => cleaned.text.clone(),
                };
                self.context = Some((tail_chars(&joined, CONTEXT_CHARS), base_ms + end));
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

/// Splits an utterance, given as its speech regions (samples of the pending audio, which starts
/// at `offset`), where the other channel's speech starts or stops, leaving no piece shorter than
/// `MIN_PIECE`. A pause in this channel's speech near an edge is the best cut, as it can't split a
/// word. Without one, an utterance is still cut where the other side stops talking (what follows
/// is this channel's own speech, which mustn't go down with the echo before it), but not where
/// the other side starts: this channel may still be finishing a sentence there.
fn split_at_edges(regions: &[std::ops::Range<usize>], other: &[std::ops::Range<u64>], offset: u64) -> Vec<std::ops::Range<usize>> {
    let Some((first, last)) = regions.first().zip(regions.last()) else { return Vec::new() };
    let abs = |s: usize| offset + s as u64;
    let (start, end) = (abs(first.start), abs(last.end));
    // Middles of the pauses between this channel's regions.
    let pauses: Vec<u64> = regions.windows(2).map(|w| (abs(w[0].end) + abs(w[1].start)) / 2).collect();
    let near_pause = |edge: u64| pauses.iter().copied().filter(|p| p.abs_diff(edge) <= PAUSE_REACH).min_by_key(|p| p.abs_diff(edge));
    let mut cuts: Vec<u64> = Vec::new();
    for r in other.iter().filter(|r| r.start < end && r.end > start) {
        let starts = r.start + EDGE_INSET;
        if starts > start && starts < end {
            cuts.extend(near_pause(starts));
        }
        // The echo outlasts the other side by the speakers' latency and the room's reverb: cut
        // at the end of the padded region.
        let stops = r.end;
        if stops > start && stops < end {
            cuts.push(near_pause(stops).unwrap_or(stops));
        }
    }
    cuts.sort_unstable();
    let mut pieces = Vec::new();
    let mut from = start;
    for p in cuts {
        if p >= from + MIN_PIECE as u64 && p + MIN_PIECE as u64 <= end {
            pieces.push((from - offset) as usize..(p - offset) as usize);
            from = p;
        }
    }
    pieces.push((from - offset) as usize..last.end);
    pieces
}

/// The last `max` characters of `text` at most, starting at a word.
fn tail_chars(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let from = chars.len() - max;
    let tail: String = chars[from..].iter().collect();
    // Cut inside a word: start at the next one.
    match tail.find(' ').filter(|_| !chars[from - 1].is_whitespace()) {
        Some(i) => tail[i + 1..].to_string(),
        None => tail,
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

/// Share of `mic`'s time during which the other side was talking.
fn overlap_with_system(mic: &Segment, system: &[Segment]) -> f32 {
    let len = mic.end_ms.saturating_sub(mic.start_ms);
    if len == 0 {
        return 0.0;
    }
    let mut spans: Vec<(u64, u64)> = system
        .iter()
        .filter(|s| s.channel == Channel::System && s.start_ms < mic.end_ms && s.end_ms > mic.start_ms)
        .map(|s| (s.start_ms.max(mic.start_ms), s.end_ms.min(mic.end_ms)))
        .collect();
    spans.sort_unstable();
    let (mut covered, mut reach) = (0, mic.start_ms);
    for (a, b) in spans {
        let a = a.max(reach);
        if b > a {
            covered += b - a;
            reach = b;
        }
    }
    covered as f32 / len as f32
}

/// The text of `mic` without a lead-in that is echo: the last words the other side said just
/// before, caught at the start of the user's own speech ("Gegründet habe. Okay, wie war …").
/// The lead-in must end a sentence (a comma could end the user's own "Okay,"), and something
/// must remain after it.
pub fn without_echo_lead_in(mic: &Segment, system: &[Segment]) -> Option<String> {
    const BEFORE_MS: u64 = 3000;
    const MAX_WORDS: usize = 8;
    let mut recent: Vec<&Segment> = system
        .iter()
        .filter(|s| s.channel == Channel::System && s.end_ms + BEFORE_MS >= mic.start_ms && s.start_ms < mic.start_ms + 1500)
        .collect();
    recent.sort_by_key(|s| s.end_ms);
    let heard: Vec<String> = recent.iter().flat_map(|s| words(&s.text)).collect();
    if heard.is_empty() {
        return None;
    }
    let mut best = None;
    for (i, c) in mic.text.char_indices() {
        if !matches!(c, '.' | '!' | '?' | '…') {
            continue;
        }
        let end = i + c.len_utf8();
        let (lead, rest) = mic.text.split_at(end);
        let lead_words = words(lead);
        if lead_words.len() > MAX_WORDS {
            break;
        }
        let rest = rest.trim_start_matches(['.', '…', ' ']).trim();
        if lead_words.is_empty() || words(rest).is_empty() {
            continue;
        }
        // Most of the lead-in is among the last words heard (speech recognition hears the two
        // copies a little differently).
        let tail = &heard[heard.len().saturating_sub(lead_words.len() + 2)..];
        let found = lead_words.iter().filter(|w| tail.contains(w)).count();
        if found * 3 >= lead_words.len() * 2 {
            best = Some(capitalize(rest));
        }
    }
    best
}

fn capitalize(text: &str) -> String {
    let mut c = text.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Below this share of overlap with the other side, microphone speech is taken to be the user's.
const QUIET_OVERLAP: f32 = 0.15;
/// From this share of overlap on, a segment may be echo even if its words didn't match.
const TALK_OVERLAP: f32 = 0.5;
/// Echo by voice needs to sound at least this much like the echoed voices...
const ECHO_VOICE: f32 = 0.5;
/// ...and this much more like them than like the user.
const VOICE_MARGIN: f32 = 0.1;
/// Voices remembered per side.
const VOICE_MEMORY: usize = 64;

/// Recognises microphone segments that are the other side of the call coming out of the speakers.
/// By words first: mostly the same words as system audio at the same time. Speech recognition
/// often hears the two copies differently, though (a short German "Ja, mache ich" became "Yeah,
/// mafui" on one of them), so it also learns two kinds of voice on the microphone: the echoed
/// voices (from segments the words gave away) and the user's (speech while the other side was
/// quiet). A segment said over the other side that sounds like the echo, not the user, is echo too.
#[derive(Default)]
pub struct EchoFilter {
    echo: Vec<Vec<f32>>,
    mine: Vec<Vec<f32>>,
}

/// Why a microphone segment counts as echo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Echo {
    Words,
    /// Similarity to the echoed voices and to the user's.
    Voice(f32, f32),
}

fn remember(list: &mut Vec<Vec<f32>>, e: &[f32]) {
    if list.len() >= VOICE_MEMORY {
        list.remove(0);
    }
    list.push(e.to_vec());
}

/// Mean similarity of `e` to its three closest voices in `list`.
fn closeness(list: &[Vec<f32>], e: &[f32]) -> f32 {
    let mut sims: Vec<f32> = list.iter().map(|v| speakers::cosine_sim(v, e)).collect();
    sims.sort_by(|a, b| b.total_cmp(a));
    let top = &sims[..sims.len().min(3)];
    top.iter().sum::<f32>() / top.len().max(1) as f32
}

impl EchoFilter {
    /// Whether `mic` is echo, given the system segments heard so far; learns its voice either way.
    pub fn check(&mut self, mic: &Segment, embedding: Option<&[f32]>, system: &[Segment]) -> Option<Echo> {
        if is_echo(mic, system) {
            if let Some(e) = embedding {
                remember(&mut self.echo, e);
            }
            return Some(Echo::Words);
        }
        let e = embedding?;
        let overlap = overlap_with_system(mic, system);
        if overlap < QUIET_OVERLAP {
            remember(&mut self.mine, e);
            return None;
        }
        (overlap >= TALK_OVERLAP).then(|| self.by_voice(e)).flatten()
    }

    fn by_voice(&self, e: &[f32]) -> Option<Echo> {
        if self.echo.is_empty() || self.mine.is_empty() {
            return None;
        }
        let (echo, mine) = (closeness(&self.echo, e), closeness(&self.mine, e));
        (echo >= ECHO_VOICE && echo > mine + VOICE_MARGIN).then_some(Echo::Voice(echo, mine))
    }
}

/// Drops microphone segments that are echo of system audio, now that all of it is known; returns
/// what was dropped. `filter` brings the echoed voices recognised while the meeting ran.
pub fn drop_echo(segments: &mut Vec<Segment>, embeddings: &mut Vec<Option<Vec<f32>>>, filter: &EchoFilter) -> Vec<(Segment, Echo)> {
    let system: Vec<Segment> = segments.iter().filter(|s| s.channel == Channel::System).cloned().collect();
    if system.is_empty() {
        return Vec::new();
    }
    // Words first, learning the voices from the whole meeting; then voices for the rest.
    let mut voices = EchoFilter { echo: filter.echo.clone(), mine: Vec::new() };
    let mut verdict: Vec<Option<Echo>> = vec![None; segments.len()];
    let mut undecided = Vec::new();
    for (i, s) in segments.iter().enumerate() {
        if s.channel != Channel::Mic {
            continue;
        }
        let e = embeddings.get(i).and_then(|e| e.as_deref());
        if is_echo(s, &system) {
            verdict[i] = Some(Echo::Words);
            if let Some(e) = e {
                remember(&mut voices.echo, e);
            }
        } else if let Some(e) = e {
            if overlap_with_system(s, &system) < QUIET_OVERLAP {
                remember(&mut voices.mine, e);
            } else if overlap_with_system(s, &system) >= TALK_OVERLAP {
                undecided.push(i);
            }
        }
    }
    for i in undecided {
        if let Some(e) = embeddings[i].as_deref() {
            verdict[i] = voices.by_voice(e);
        }
    }
    for (s, v) in segments.iter_mut().zip(&verdict) {
        if s.channel == Channel::Mic && v.is_none() {
            if let Some(text) = without_echo_lead_in(s, &system) {
                s.text = text;
            }
        }
    }
    let dropped: Vec<(Segment, Echo)> =
        verdict.iter().enumerate().filter_map(|(i, v)| v.map(|v| (segments[i].clone(), v))).collect();
    let keep: Vec<bool> = verdict.iter().map(Option::is_none).collect();
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
#[allow(clippy::single_range_in_vec_init)] // one region of speech is a common case
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

    const S: usize = RATE;

    #[test]
    fn utterance_is_cut_where_the_other_side_stops() {
        // Echo of the other side (0-8 s, no pause) runs straight into the user's reply; system
        // speech (padded) ends at 8 s.
        let other = [0..8 * S as u64];
        assert_eq!(split_at_edges(&[0..14 * S], &other, 0), [0..8 * S, 8 * S..14 * S]);
        // With a pause in the microphone's speech close by, the cut goes there.
        let regions = [0..7 * S + S / 2, 8 * S + S / 4..14 * S];
        let pause = (7 * S + S / 2 + 8 * S + S / 4) / 2;
        assert_eq!(split_at_edges(&regions, &other, 0), [0..pause, pause..14 * S]);
        // Positions are relative to the pending audio.
        let shifted = [100 * S as u64..108 * S as u64];
        assert_eq!(split_at_edges(&[0..14 * S], &shifted, 100 * S as u64), [0..8 * S, 8 * S..14 * S]);
    }

    #[test]
    fn utterance_is_not_cut_mid_speech_where_the_other_side_starts() {
        // The other side starts at 5 s while the user is still talking: no pause, no cut.
        let other = [5 * S as u64..20 * S as u64];
        assert_eq!(split_at_edges(&[0..8 * S], &other, 0), [0..8 * S]);
        // With a pause there, it is cut at the pause.
        let regions = [0..5 * S, 5 * S + S / 2..8 * S];
        let pause = (5 * S + 5 * S + S / 2) / 2;
        assert_eq!(split_at_edges(&regions, &other, 0), [0..pause, pause..8 * S]);
    }

    #[test]
    fn no_tiny_pieces_are_cut_off() {
        let other = [0..(S as u64 / 4)];
        assert_eq!(split_at_edges(&[0..6 * S], &other, 0), [0..6 * S]);
    }

    #[test]
    fn overlap_counts_each_moment_once() {
        let mic = seg(Channel::Mic, 1000, 3000, "x");
        let system = [seg(Channel::System, 0, 1500, "a"), seg(Channel::System, 1200, 2000, "b")];
        assert!((overlap_with_system(&mic, &system) - 0.5).abs() < 1e-6);
        assert_eq!(overlap_with_system(&mic, &[seg(Channel::System, 5000, 6000, "c")]), 0.0);
    }

    fn unit(i: usize) -> Vec<f32> {
        (0..8).map(|k| if k == i { 1.0 } else { 0.05 }).collect()
    }

    #[test]
    fn echo_is_recognised_by_voice_once_both_voices_are_known() {
        let (me, them) = (unit(1), unit(5));
        let mut filter = EchoFilter::default();
        let system = vec![
            seg(Channel::System, 0, 4000, "We should ship the billing page on Thursday."),
            seg(Channel::System, 10_000, 12_000, "Ja, mache ich."),
        ];
        // Echo given away by its words: its voice is learnt as the echo's.
        let echo = seg(Channel::Mic, 100, 4100, "we should ship the billing page Thursday");
        assert_eq!(filter.check(&echo, Some(&them), &system), Some(Echo::Words));
        // The user while the other side is quiet: learnt as the user's voice.
        let mine = seg(Channel::Mic, 5000, 8000, "Sounds good, I'll prepare the slides.");
        assert_eq!(filter.check(&mine, Some(&me), &system), None);
        // Over the other side, misheard differently: the voice gives it away.
        let misheard = seg(Channel::Mic, 10_100, 12_000, "Yeah, mafui.");
        assert!(matches!(filter.check(&misheard, Some(&them), &system), Some(Echo::Voice(..))));
        // The user talking over the other side is kept.
        let over = seg(Channel::Mic, 10_100, 12_000, "Richtig.");
        assert_eq!(filter.check(&over, Some(&me), &system), None);
    }

    #[test]
    fn final_pass_uses_the_whole_meeting_and_the_live_echo_voices() {
        let (me, them) = (unit(1), unit(5));
        let mut live = EchoFilter::default();
        let system = vec![seg(Channel::System, 0, 4000, "We should ship the billing page on Thursday.")];
        live.check(&seg(Channel::Mic, 100, 4100, "we should ship the billing page Thursday"), Some(&them), &system);
        let mut segments = vec![
            seg(Channel::System, 0, 4000, "We should ship the billing page on Thursday."),
            seg(Channel::System, 9000, 12_000, "Ja, mache ich."),
            seg(Channel::Mic, 5000, 8000, "Sounds good, I'll prepare the slides."),
            seg(Channel::Mic, 9100, 12_000, "Yeah, mafui."),
        ];
        let mut embeddings = vec![None, None, Some(me), Some(them)];
        let dropped = drop_echo(&mut segments, &mut embeddings, &live);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].0.text, "Yeah, mafui.");
        assert_eq!(segments.len(), 3);
        assert_eq!(embeddings.len(), 3);
    }

    #[test]
    fn echo_lead_in_is_trimmed() {
        let system = vec![seg(Channel::System, 20_000, 26_000, "Dass ich gleich am nächsten Tag wieder gegründet habe.")];
        let mic = seg(Channel::Mic, 26_200, 30_000, "Gegründet habe. Okay, wie war die Gründung dann für dich eigentlich?");
        assert_eq!(without_echo_lead_in(&mic, &system).as_deref(), Some("Okay, wie war die Gründung dann für dich eigentlich?"));
        // A comma doesn't end a lead-in, and the user's own words stay.
        let own = seg(Channel::Mic, 26_200, 30_000, "Okay, wie war die Gründung dann für dich eigentlich?");
        assert_eq!(without_echo_lead_in(&own, &system), None);
        // Nothing is trimmed long after the other side spoke.
        let later = seg(Channel::Mic, 60_000, 63_000, "Gegründet habe. Okay, weiter.");
        assert_eq!(without_echo_lead_in(&later, &system), None);
    }

    #[test]
    fn context_keeps_the_end_of_the_text_from_a_word() {
        assert_eq!(tail_chars("short", 220), "short");
        assert_eq!(tail_chars("one two three four", 10), "three four");
        assert_eq!(tail_chars("one two three four", 9), "four");
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
