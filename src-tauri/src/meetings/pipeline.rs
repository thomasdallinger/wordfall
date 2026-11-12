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
/// Once a channel has this much speech (ms)...
const LANGUAGE_SETTLES_MS: u64 = 120_000;
/// ...and this share of it in one language (in tenths), it stays in that language: language
/// detection on a few seconds of far speech is often wrong.
const LANGUAGE_SHARE: u64 = 9;

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
    /// Speech heard per language (ms), until the language settles.
    language_ms: HashMap<String, u64>,
    /// The language the channel settled on (see `LANGUAGE_SETTLES_MS`).
    settled: Option<String>,
    /// The channel's noise floor (dBFS), for telling quiet speech from near-silence.
    noise_floor: Option<f32>,
}

impl ChannelProcessor {
    pub fn new(channel: Channel) -> Self {
        Self {
            channel,
            pending: Vec::new(),
            offset: 0,
            language: None,
            speech: Vec::new(),
            context: None,
            language_ms: HashMap::new(),
            settled: None,
            noise_floor: None,
        }
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
        // Far or quiet microphone speech is levelled first: speech detection misses it otherwise.
        let levelled = (self.channel == Channel::Mic).then(|| level(&self.pending));
        let regions = ctx.engine.speech_regions(levelled.as_deref().unwrap_or(&self.pending))?;
        if let Some(floor) = noise_floor_db(&self.pending) {
            self.noise_floor = Some(self.noise_floor.map_or(floor, |f| 0.9 * f + 0.1 * floor));
        }
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
            let languages = match (&self.settled, &self.language) {
                (Some(l), _) => std::slice::from_ref(l),
                (None, Some(l)) if audio.len() < SHORT_CLIP && ctx.languages.contains(l) => std::slice::from_ref(l),
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
            let mut padded = if self.channel == Channel::Mic { level(audio) } else { audio.to_vec() };
            padded.resize(audio.len() + TAIL_PAD, 0.0);
            (ctx.yield_to_dictation)();
            let (timed, lang) =
                ctx.engine.transcribe_timed(&padded, languages, self.language.as_deref(), prompt.as_deref(), ctx.decode)?;
            if lang.is_some() {
                self.language = lang;
            }
            settle_language(&mut self.language_ms, &mut self.settled, self.language.as_deref(), len_ms);
            for t in timed {
                let (start, end) = (t.start_ms.min(len_ms), t.end_ms.min(len_ms).max(t.start_ms.min(len_ms)));
                let (a, b) = ((start as usize * RATE / 1000).min(audio.len()), (end as usize * RATE / 1000).min(audio.len()));
                let cleaned = postprocess::clean(
                    &t.text,
                    &postprocess::Options {
                        remove_fillers: ctx.remove_fillers,
                        format_lists: false,
                        language: self.language.as_deref(),
                        dictionary: ctx.dictionary,
                        quiet_input: is_quiet(level_db(&audio[a..b]), self.noise_floor),
                    },
                );
                if cleaned.text.is_empty() {
                    continue;
                }
                let embedding = match ctx.embedder {
                    Some(e) => {
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

/// Speech per window of the final pass: whisper reads 30 s at a time; this leaves room for the
/// silence appended at the end.
const FINAL_WINDOW: usize = 28 * RATE;
/// The final pass reads the recording this much at a time.
const FINAL_BLOCK: usize = 10 * 60 * RATE;
/// Beam width of the final pass, where time matters less than in the live view.
const FINAL_BEAM: i32 = 5;
/// Pauses between the speech regions of a window are kept up to this long: whisper ends its
/// segments (and a turn) at pauses, and its times stay easy to map back.
const FINAL_PAUSE: usize = RATE * 3 / 2;

/// One window of the final pass: speech regions (samples of the block) read as one clip.
type Window = Vec<std::ops::Range<usize>>;
/// A voice embedding per segment (`None` where there was too little speech).
pub type Embeddings = Vec<Option<Vec<f32>>>;
/// Reads `len` samples from `start` of a recording (fewer at its end, none past it).
pub type Reader<'a> = Box<dyn Fn(usize, usize) -> anyhow::Result<Vec<f32>> + 'a>;

/// A block of a channel's recording as the final pass reads it: where it starts, how long it is
/// and its windows of speech.
struct Block {
    pos: usize,
    len: usize,
    windows: Vec<Window>,
}

/// Where the final pass reads a channel: its recording in blocks of up to `FINAL_BLOCK`, each
/// with its speech (microphone speech levelled first) in windows of up to `FINAL_WINDOW` cut at
/// pauses. A block's last window may go on in the next block, which then starts with it.
/// `scanned` gets the samples read so far.
fn plan_final(
    ctx: &Ctx,
    mic: bool,
    read: &dyn Fn(usize, usize) -> anyhow::Result<Vec<f32>>,
    scanned: &mut dyn FnMut(usize),
) -> anyhow::Result<Vec<Block>> {
    let mut blocks = Vec::new();
    let mut pos = 0usize;
    loop {
        let raw = read(pos, FINAL_BLOCK).map_err(|e| e.context(format!("reading the recording at {}", clock(pos as u64 * 1000 / RATE as u64))))?;
        if raw.is_empty() {
            break;
        }
        let last = raw.len() < FINAL_BLOCK;
        let regions = {
            let levelled = if mic { level(&raw) } else { raw.clone() };
            ctx.engine.speech_regions(&levelled)?
        };
        let mut windows = final_windows(&raw, &regions);
        // Unless the recording ends here, the last window may go on in the next block.
        let next = match windows.last() {
            Some(g) if !last && g[0].start > 0 => {
                let start = g[0].start;
                windows.pop();
                pos + start
            }
            _ => pos + raw.len(),
        };
        scanned(pos + raw.len());
        blocks.push(Block { pos, len: raw.len(), windows });
        if last {
            break;
        }
        pos = next;
    }
    Ok(blocks)
}

/// The final transcript of one channel, made once the meeting is over: the whole recording again,
/// in the windows `plan_final` found (the live view decodes a few seconds at a time, which whisper
/// gets far more wrong), with beam search, microphone speech levelled first (quieter voices lifted,
/// `lift_quiet`, then `level`). `read(start, len)` returns samples of the channel's recording
/// (fewer at its end; a failed read fails the channel, it isn't taken for the end of the
/// recording); `other_speech` is the other channel's speech (as returned for it), where microphone
/// windows are cut so echo of the call stays apart from the user's words. Where whisper's text comes out without punctuation even with the text before as
/// a prompt, the channel's `live` segments stay. `progress` is called after each window.
fn transcribe_final(
    ctx: &Ctx,
    channel: Channel,
    read: &dyn Fn(usize, usize) -> anyhow::Result<Vec<f32>>,
    blocks: &[Block],
    other_speech: &[std::ops::Range<u64>],
    live: &[Segment],
    progress: &mut dyn FnMut(),
) -> anyhow::Result<(Vec<Heard>, Vec<std::ops::Range<u64>>)> {
    let decode = DecodeOptions { beam_size: FINAL_BEAM.max(ctx.decode.beam_size), ..ctx.decode.clone() };
    let mic = channel == Channel::Mic;
    let (mut out, mut speech) = (Vec::new(), Vec::new());
    let mut state = FinalState::default();
    for &Block { pos, len, ref windows } in blocks {
        if windows.is_empty() {
            continue;
        }
        let raw = read(pos, len).map_err(|e| e.context(format!("reading the recording at {}", clock(pos as u64 * 1000 / RATE as u64))))?;
        anyhow::ensure!(raw.len() == len, "the recording at {} changed during the final pass", clock(pos as u64 * 1000 / RATE as u64));
        let floor = noise_floor_db(&raw);
        for g in windows {
            speech.extend(g.iter().map(|r| (pos + r.start) as u64..(pos + r.end) as u64));
            let pieces = if mic { split_at_edges(g, other_speech, pos as u64) } else { std::iter::once(g[0].start..g[g.len() - 1].end).collect() };
            for piece in pieces {
                let window: Window =
                    g.iter().map(|r| r.start.max(piece.start)..r.end.min(piece.end)).filter(|r| r.end > r.start).collect();
                match transcribe_window(ctx, &decode, &raw, &window, pos, mic, floor, &mut state)? {
                    Some(heard) => out.extend(heard),
                    None => out.extend(keep_live(ctx, &raw, &window, pos, channel, live, &mut state)),
                }
            }
            progress();
        }
    }
    Ok((out, speech))
}

/// Share of the final pass the scan for speech is shown to take before the decoding: Silero
/// reads an hour of a channel in about 7 s, decoding a long meeting's speech takes minutes.
const SCAN_SHARE: f32 = 0.05;

/// The share of the final pass done after each window, per channel in the order they're decoded,
/// from each window's clip length: after the scan (`SCAN_SHARE`), by the length of the clips
/// decoded, so a channel without speech takes nothing of the bar. Rises to exactly 1.
fn decode_shares(clips: &[Vec<usize>]) -> Vec<Vec<f32>> {
    let total = clips.iter().flatten().sum::<usize>().max(1);
    let mut done = 0;
    clips
        .iter()
        .map(|channel| {
            channel
                .iter()
                .map(|len| {
                    done += len;
                    1.0 - (1.0 - SCAN_SHARE) * (total - done) as f32 / total as f32
                })
                .collect()
        })
        .collect()
}

/// A channel's recording for the final pass: `read(start, len)` gives samples (fewer at the end).
pub struct Recorded<'a> {
    pub channel: Channel,
    pub len: usize,
    pub read: &'a dyn Fn(usize, usize) -> anyhow::Result<Vec<f32>>,
}

/// The final transcript of a meeting as segments with their voice embeddings, in time order:
/// `transcribe_final` on each channel, system audio first (the microphone is cut at its edges).
/// Each segment takes the speaker of the live segment it overlaps most, so names the user gave
/// while the meeting ran carry over. A channel whose pass fails (its recording can't be read)
/// keeps its `live` segments rather than a transcript that stops early; when every channel fails,
/// so does this. `progress` gets the share done (0..1, never less than before): every channel is
/// scanned for speech first, then the share follows the speech decoded (a silent channel takes
/// none of it).
pub fn final_segments(
    ctx: &Ctx,
    recorded: &[Recorded],
    live: &[Segment],
    progress: &mut dyn FnMut(f32),
) -> anyhow::Result<(Vec<Segment>, Embeddings)> {
    let total = recorded.iter().map(|r| r.len).sum::<usize>().max(1);
    let mut failed: Option<anyhow::Error> = None;
    let mut passed = false;
    let mut out: Vec<(Segment, Option<Vec<f32>>)> = Vec::new();
    let mut system_speech = Vec::new();
    let mut order: Vec<&Recorded> = recorded.iter().collect();
    order.sort_by_key(|r| r.channel != Channel::System);
    let mut scanned = 0;
    let mut plans = Vec::new();
    for r in &order {
        plans.push(plan_final(ctx, r.channel == Channel::Mic, r.read, &mut |at| {
            progress(SCAN_SHARE * (scanned + at).min(total) as f32 / total as f32)
        }));
        scanned += r.len;
    }
    let clips: Vec<Vec<usize>> = plans
        .iter()
        .map(|p| p.as_ref().map_or_else(|_| Vec::new(), |blocks| blocks.iter().flat_map(|b| b.windows.iter().map(|w| clip_len(w))).collect()))
        .collect();
    for ((r, plan), shares) in order.into_iter().zip(plans).zip(decode_shares(&clips)) {
        let channel = r.channel;
        let other = if channel == Channel::Mic { system_speech.as_slice() } else { &[] };
        let mut shares = shares.into_iter();
        let result = plan.and_then(|blocks| {
            transcribe_final(ctx, channel, r.read, &blocks, other, live, &mut || progress(shares.next().unwrap_or(1.0)))
        });
        match result {
            Ok((heard, speech)) => {
                passed = true;
                if channel == Channel::System {
                    system_speech = speech;
                }
                out.extend(heard.into_iter().map(|h| {
                    let overlap = |s: &&Segment| h.end_ms.min(s.end_ms).saturating_sub(h.start_ms.max(s.start_ms));
                    let speaker = live.iter().filter(|s| s.channel == channel).max_by_key(overlap).filter(|s| overlap(s) > 0).map_or(
                        if channel == Channel::Mic { ME } else { OTHERS },
                        |s| s.speaker,
                    );
                    (Segment { start_ms: h.start_ms, end_ms: h.end_ms, channel, speaker, text: h.text, language: h.language, fixes: Vec::new() }, h.embedding)
                }));
            }
            Err(e) => {
                log::error!("final transcript of the {channel:?} channel failed, keeping its live one: {e:#}");
                let kept: Vec<&Segment> = live.iter().filter(|s| s.channel == channel).collect();
                if channel == Channel::System {
                    system_speech = kept.iter().map(|s| s.start_ms * RATE as u64 / 1000..s.end_ms * RATE as u64 / 1000).collect();
                }
                out.extend(kept.into_iter().map(|s| (s.clone(), None)));
                failed = Some(e);
            }
        }
    }
    if let Some(e) = failed.filter(|_| !passed) {
        return Err(e);
    }
    progress(1.0);
    out.sort_by_key(|(s, _)| s.start_ms);
    Ok(out.into_iter().unzip())
}

/// Speech regions of a block grouped into windows of at most `FINAL_WINDOW` (speech and the
/// pauses between, shortened to `FINAL_PAUSE`); a region longer than that is cut at its quietest
/// moment. Windows end at the longest pauses within reach (`group_at_pauses`), not wherever one
/// fills up.
fn final_windows(audio: &[f32], regions: &[std::ops::Range<usize>]) -> Vec<Window> {
    let mut pieces = Vec::new();
    for r in regions {
        let mut r = r.clone();
        while r.len() > FINAL_WINDOW {
            // The quietest 100 ms in the window's last 8 s.
            let from = r.start + FINAL_WINDOW - 8 * RATE;
            let frame = RATE / 10;
            let cut = (0..(8 * RATE) / frame)
                .map(|k| from + k * frame)
                .min_by(|&a, &b| {
                    let e = |s: usize| audio[s..s + frame].iter().map(|x| x * x).sum::<f32>();
                    e(a).total_cmp(&e(b))
                })
                .map_or(r.start + FINAL_WINDOW, |c| c + frame / 2);
            pieces.push(r.start..cut);
            r = cut..r.end;
        }
        pieces.push(r);
    }
    group_at_pauses(&pieces)
}

/// What a window costs `group_at_pauses`, against the cost of the cuts between windows...
const WINDOW_COST: f32 = 0.5;
/// ...where a cut at a pause this long (seconds) costs 1/e: almost free at a clear break, close to
/// a whole window at a short pause, most likely inside a sentence. Whisper drops and garbles words
/// around a cut, least where a sentence or a speaker's turn ends (measured on a real meeting:
/// about 2 points less word error than filling each window up).
const CUT_PAUSE_S: f32 = 0.7;

/// The cost of cutting at a pause of `samples`.
fn cut_cost(samples: usize) -> f32 {
    (-(samples as f32 / RATE as f32) / CUT_PAUSE_S).exp()
}

/// `pieces` (in order, none longer than `FINAL_WINDOW`) grouped into windows of at most
/// `FINAL_WINDOW` at the least cost: `WINDOW_COST` per window plus `cut_cost` per cut.
fn group_at_pauses(pieces: &[std::ops::Range<usize>]) -> Vec<Window> {
    let n = pieces.len();
    // best[j]: the least cost of windows holding pieces[..j]; from[j]: where its last one starts.
    let mut best = vec![f32::INFINITY; n + 1];
    let mut from = vec![0; n + 1];
    best[0] = 0.0;
    for j in 1..=n {
        let cut = if j < n { cut_cost(pieces[j].start.saturating_sub(pieces[j - 1].end)) } else { 0.0 };
        let mut len = 0;
        for i in (0..j).rev() {
            len += pieces[i].len() + if i + 1 < j { pieces[i + 1].start.saturating_sub(pieces[i].end).min(FINAL_PAUSE) } else { 0 };
            if len > FINAL_WINDOW && i + 1 < j {
                break;
            }
            let cost = best[i] + WINDOW_COST + cut;
            if cost < best[j] {
                (best[j], from[j]) = (cost, i);
            }
        }
    }
    let mut windows = Vec::new();
    let mut j = n;
    while j > 0 {
        windows.push(pieces[from[j]..j].to_vec());
        j = from[j];
    }
    windows.reverse();
    windows
}

/// A stretch of speech must overlap a region by this much to count as being in it...
const GRAZE: usize = RATE * 4 / 10;

/// `a..b` (samples of the block) without the regions of `window` it only grazes at either end:
/// whisper often starts a segment where the previous one ended, which maps back to the end of
/// the previous region, and that region may be someone else talking.
fn trim_to_speech(window: &[std::ops::Range<usize>], a: usize, b: usize) -> (usize, usize) {
    let inside: Vec<std::ops::Range<usize>> =
        window.iter().map(|r| r.start.max(a)..r.end.min(b)).filter(|r| r.end > r.start).collect();
    let solid: Vec<&std::ops::Range<usize>> = inside.iter().filter(|r| r.len() >= GRAZE).collect();
    match (solid.first(), solid.last()) {
        (Some(first), Some(last)) => (first.start, last.end),
        _ => (a, b),
    }
}

/// What the final pass carries from one window to the next.
#[derive(Default)]
struct FinalState {
    language: Option<String>,
    /// The end of the text so far, a prompt when whisper loses punctuation.
    previous: String,
}

/// Whether `text` is long enough to have punctuation but has none (whisper's lowercase run-on).
fn lacks_punctuation(text: &str) -> bool {
    text.split_whitespace().count() >= 12 && !text.contains(['.', ',', '?', '!'])
}

/// Length of a window's clip: its speech and the pauses between, shortened to `FINAL_PAUSE`.
fn clip_len(window: &[std::ops::Range<usize>]) -> usize {
    let pauses: usize = window.windows(2).map(|p| p[1].start.saturating_sub(p[0].end).min(FINAL_PAUSE)).sum();
    window.iter().map(|r| r.len()).sum::<usize>() + pauses
}

/// Transcribes one window of the final pass: its speech regions joined with long pauses shortened
/// to `FINAL_PAUSE`, with whisper's times mapped back onto the recording.
#[allow(clippy::too_many_arguments)]
fn transcribe_window(
    ctx: &Ctx,
    decode: &DecodeOptions,
    block: &[f32],
    window: &Window,
    block_start: usize,
    mic: bool,
    floor: Option<f32>,
    state: &mut FinalState,
) -> anyhow::Result<Option<Vec<Heard>>> {
    let FinalState { language, previous } = state;
    let mut joined = Vec::new();
    // (start in the joined clip, start in the block, length)
    let mut map = Vec::new();
    for (k, r) in window.iter().enumerate() {
        if k > 0 {
            let from = window[k - 1].end;
            joined.extend_from_slice(&block[from..from + r.start.saturating_sub(from).min(FINAL_PAUSE)]);
        }
        map.push((joined.len(), r.start, r.len()));
        joined.extend_from_slice(&block[r.clone()]);
    }
    if joined.len() < RATE / 4 {
        return Ok(Some(Vec::new()));
    }
    let mut clip = if mic { level(&lift_quiet(&joined, floor)) } else { joined.clone() };
    clip.resize(clip.len() + TAIL_PAD, 0.0);
    let languages = match language.as_ref() {
        Some(l) if joined.len() < SHORT_CLIP && ctx.languages.contains(l) => std::slice::from_ref(l),
        _ => ctx.languages,
    };
    (ctx.yield_to_dictation)();
    let (mut timed, mut lang) = ctx.engine.transcribe_timed(&clip, languages, language.as_deref(), ctx.prompt, decode)?;
    // Without a prompt whisper sometimes slips into text without punctuation or capitals; the
    // text before, as the live view uses it, brings it back.
    let text: String = timed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
    if !previous.is_empty() && lacks_punctuation(&text) {
        let prompt = match ctx.prompt.filter(|p| !p.is_empty()) {
            Some(p) => format!("{p} {previous}"),
            None => previous.clone(),
        };
        (ctx.yield_to_dictation)();
        let again = ctx.engine.transcribe_timed(&clip, languages, language.as_deref(), Some(&prompt), decode)?;
        if !lacks_punctuation(&again.0.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ")) {
            (timed, lang) = again;
        }
    }
    if lacks_punctuation(&timed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ")) {
        return Ok(None);
    }
    if lang.is_some() {
        *language = lang;
    }
    // A time in the joined clip as a sample of the block; `end` maps the end of a stretch.
    let to_block = |ms: u64, end: bool| {
        let c = (ms as usize * RATE / 1000).min(joined.len());
        let hit = if end {
            map.iter().rev().find(|(j, _, _)| *j < c)
        } else {
            map.iter().find(|(j, _, len)| j + len > c)
        };
        let (first, last) = (window[0].start, window[window.len() - 1].end);
        // In a shortened pause: the start of the next region, or the end of the previous one.
        hit.map_or(if end { first } else { last }, |(j, b, len)| b + c.saturating_sub(*j).min(*len))
    };
    let mut out = Vec::new();
    for t in timed {
        let (a, b) = trim_to_speech(window, to_block(t.start_ms, false), to_block(t.end_ms, true));
        let audio = &block[a.min(b)..b];
        let cleaned = postprocess::clean(
            &t.text,
            &postprocess::Options {
                remove_fillers: ctx.remove_fillers,
                format_lists: false,
                language: language.as_deref(),
                dictionary: ctx.dictionary,
                quiet_input: is_quiet(level_db(audio), floor),
            },
        );
        if cleaned.text.is_empty() {
            continue;
        }
        let embedding = ctx.embedder.and_then(|e| {
            e.embed(audio).unwrap_or_else(|err| {
                log::warn!("speaker embedding failed: {err:#}");
                None
            })
        });
        let ms = |s: usize| ((block_start + s) * 1000 / RATE) as u64;
        *previous = tail_chars(&format!("{previous} {}", cleaned.text), CONTEXT_CHARS);
        out.push(Heard { start_ms: ms(a.min(b)), end_ms: ms(b), text: cleaned.text, language: language.clone(), embedding });
    }
    Ok(Some(out))
}

/// The live segments of `channel` in a window of the final pass, for when the final text failed.
fn keep_live(
    ctx: &Ctx,
    block: &[f32],
    window: &Window,
    block_start: usize,
    channel: Channel,
    live: &[Segment],
    state: &mut FinalState,
) -> Vec<Heard> {
    let ms = |s: usize| ((block_start + s) * 1000 / RATE) as u64;
    let (from, to) = (ms(window[0].start), ms(window[window.len() - 1].end));
    log::info!("final pass: kept the live text for {}–{} on {channel:?}", clock(from), clock(to));
    live.iter()
        .filter(|s| s.channel == channel && (from..to).contains(&((s.start_ms + s.end_ms) / 2)))
        .map(|s| {
            let sample = |t: u64| ((t * RATE as u64 / 1000) as usize).saturating_sub(block_start).min(block.len());
            let audio = &block[sample(s.start_ms)..sample(s.end_ms).max(sample(s.start_ms))];
            let embedding = ctx.embedder.and_then(|e| e.embed(audio).ok().flatten());
            state.previous = tail_chars(&format!("{} {}", state.previous, s.text), CONTEXT_CHARS);
            Heard { start_ms: s.start_ms, end_ms: s.end_ms, text: s.text.clone(), language: s.language.clone(), embedding }
        })
        .collect()
}

/// Counts `ms` of speech in `language` towards `heard`; `settled` becomes the language once it
/// clearly dominates.
fn settle_language(heard: &mut HashMap<String, u64>, settled: &mut Option<String>, language: Option<&str>, ms: u64) {
    let Some(l) = language.filter(|_| settled.is_none()) else { return };
    *heard.entry(l.to_string()).or_default() += ms;
    let total: u64 = heard.values().sum();
    if let Some((top, top_ms)) = heard.iter().max_by_key(|(_, ms)| **ms) {
        if total >= LANGUAGE_SETTLES_MS && top_ms * 10 >= total * LANGUAGE_SHARE {
            log::info!("meeting language settled on {top}");
            *settled = Some(top.clone());
        }
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

/// How close a voice must sound to the user's voice print to be them. Dictation is close to the
/// microphone and a meeting farther away, so the same person scores lower than within one recording.
const PRINT_MATCH: f32 = 0.5;
/// Live: a voice nobody has heard yet needs this much speech...
const NEW_VOICE_MS: u64 = 20_000;
/// ...in at least this many segments that sound alike, before it gets a speaker of its own.
const NEW_VOICE_SEGMENTS: usize = 2;
/// Live: groups of not-yet-placed speech remembered per channel (the smallest are forgotten).
const PENDING_GROUPS: usize = 32;

/// A voice heard live, as the duration-weighted sum of its segments' embeddings.
struct LiveVoice {
    sum: Vec<f32>,
    id: u32,
    ms: u64,
}

/// Speech that didn't sound like any known voice, grouped by how alike it sounds.
struct Pending {
    sum: Vec<f32>,
    ms: u64,
    segments: usize,
}

#[derive(Default)]
struct LiveChannel {
    voices: Vec<LiveVoice>,
    pending: Vec<Pending>,
    last: Option<u32>,
}

fn add_weighted(sum: &mut [f32], e: &[f32], ms: u64) {
    let w = ms.max(1) as f32 / 1000.0;
    sum.iter_mut().zip(e).for_each(|(s, x)| *s += x * w);
}

fn weighted(e: &[f32], ms: u64) -> Vec<f32> {
    let w = ms.max(1) as f32 / 1000.0;
    e.iter().map(|x| x * w).collect()
}

/// Speaker labels while the meeting is still running: each new segment joins the closest voice
/// heard so far on its channel. Speech that sounds like no one known is shown under the closest
/// voice until enough alike speech (`NEW_VOICE_MS`) makes it a new speaker, so odd segments
/// (laughter, two people at once) don't each become one; voices that turn out to sound alike are
/// merged into the older one. The final labels come from `assign_speakers` at the end.
pub struct LiveSpeakers {
    diarize: bool,
    /// The user's voice print: a microphone voice that matches it is "Me".
    print: Option<Vec<f32>>,
    channels: HashMap<Channel, LiveChannel>,
    next_id: u32,
}

impl LiveSpeakers {
    pub fn new(diarize: bool, existing: &[Speaker], print: Option<&[f32]>) -> Self {
        let next_id = existing.iter().map(|s| s.id + 1).max().unwrap_or(1).max(1);
        Self { diarize, print: print.map(<[f32]>::to_vec), channels: HashMap::new(), next_id }
    }

    /// The speaker of a new segment `ms` long on `channel`.
    pub fn assign(&mut self, channel: Channel, embedding: Option<&[f32]>, ms: u64, speakers: &mut Vec<Speaker>) -> u32 {
        let id = if !self.diarize {
            if channel == Channel::Mic { ME } else { OTHERS }
        } else {
            match embedding {
                Some(e) => self.place(channel, e, ms),
                None => match self.channels.get(&channel).and_then(|c| c.last) {
                    Some(id) => id,
                    None if channel == Channel::Mic => ME,
                    None => self.open_voice(channel, None),
                },
            }
        };
        self.channels.entry(channel).or_default().last = Some(id);
        if !speakers.iter().any(|s| s.id == id) {
            speakers.push(Speaker { id, name: default_name(id, self.diarize), renamed: false });
            speakers.sort_by_key(|s| s.id);
        }
        id
    }

    fn place(&mut self, channel: Channel, e: &[f32], ms: u64) -> u32 {
        let ch = self.channels.entry(channel).or_default();
        let best = ch
            .voices
            .iter()
            .enumerate()
            .map(|(i, v)| (i, speakers::cosine_sim(&v.sum, e)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let nearest = match best {
            Some((i, sim)) if sim >= speakers::SAME_VOICE => {
                let v = &mut ch.voices[i];
                add_weighted(&mut v.sum, e, ms);
                v.ms += ms;
                return self.merge_alike(channel, i);
            }
            Some((i, _)) => ch.voices[i].id,
            None => {
                let id = self.open_voice(channel, Some(e));
                self.channels.get_mut(&channel).expect("entry above").voices.push(LiveVoice { sum: weighted(e, ms), id, ms });
                return id;
            }
        };
        // Like no one known yet: wait for more speech like it.
        let group = ch
            .pending
            .iter()
            .enumerate()
            .map(|(i, g)| (i, speakers::cosine_sim(&g.sum, e)))
            .filter(|(_, sim)| *sim >= speakers::SAME_VOICE)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        let g = match group {
            Some(i) => {
                let g = &mut ch.pending[i];
                add_weighted(&mut g.sum, e, ms);
                g.ms += ms;
                g.segments += 1;
                i
            }
            None => {
                if ch.pending.len() >= PENDING_GROUPS {
                    let smallest = (0..ch.pending.len()).min_by_key(|&i| ch.pending[i].ms).expect("full");
                    ch.pending.remove(smallest);
                }
                ch.pending.push(Pending { sum: weighted(e, ms), ms, segments: 1 });
                ch.pending.len() - 1
            }
        };
        if ch.pending[g].ms < NEW_VOICE_MS || ch.pending[g].segments < NEW_VOICE_SEGMENTS {
            return nearest;
        }
        let Pending { sum, ms, .. } = ch.pending.remove(g);
        let id = self.open_voice(channel, Some(&sum));
        let ch = self.channels.get_mut(&channel).expect("entry above");
        ch.voices.push(LiveVoice { sum, id, ms });
        let i = ch.voices.len() - 1;
        self.merge_alike(channel, i)
    }

    /// The id for a new voice on `channel` that sounds like `e`: on the microphone, "Me" if it's
    /// the user's (by the voice print; without one, the first voice is taken to be the user).
    fn open_voice(&mut self, channel: Channel, e: Option<&[f32]>) -> u32 {
        let me_taken = self.channels.get(&Channel::Mic).is_some_and(|c| c.voices.iter().any(|v| v.id == ME));
        let is_me = channel == Channel::Mic
            && !me_taken
            && match (&self.print, e) {
                (Some(p), Some(e)) => speakers::cosine_sim(p, e) >= PRINT_MATCH,
                (Some(_), None) => false,
                (None, _) => true,
            };
        if is_me {
            return ME;
        }
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Merges voice `i` on `channel` with any voice that now sounds the same; the older id (or
    /// "Me") stays. Returns the id voice `i` ends up with.
    fn merge_alike(&mut self, channel: Channel, mut i: usize) -> u32 {
        let ch = self.channels.get_mut(&channel).expect("voice exists");
        loop {
            let other = (0..ch.voices.len())
                .filter(|&j| j != i)
                .map(|j| (j, speakers::cosine_sim(&ch.voices[i].sum, &ch.voices[j].sum)))
                .filter(|(_, sim)| *sim >= speakers::SAME_VOICE)
                .max_by(|a, b| a.1.total_cmp(&b.1));
            let Some((j, _)) = other else { return ch.voices[i].id };
            let rank = |v: &LiveVoice| (v.id != ME, v.id);
            let (keep, gone) = if rank(&ch.voices[i]) <= rank(&ch.voices[j]) { (i, j) } else { (j, i) };
            let gone_voice = ch.voices.remove(gone);
            let keep = if gone < keep { keep - 1 } else { keep };
            let v = &mut ch.voices[keep];
            v.sum.iter_mut().zip(&gone_voice.sum).for_each(|(s, x)| *s += x);
            v.ms += gone_voice.ms;
            i = keep;
        }
    }
}

/// What tells "Me" apart among the microphone's voices.
pub struct MeHints<'a> {
    /// The user's voice print, learnt from their dictations.
    pub print: Option<&'a [f32]>,
    /// Speech level of each segment in dBFS (`level_db`), parallel to the segments; may be empty.
    pub levels: &'a [Option<f32>],
}

/// Level of the speech in `audio` in dBFS: the louder half of its 20 ms frames, so pauses inside
/// a segment don't count.
pub fn level_db(audio: &[f32]) -> Option<f32> {
    let mut frames: Vec<f32> = audio.chunks(RATE / 50).map(|f| f.iter().map(|s| s * s).sum::<f32>() / f.len() as f32).collect();
    frames.sort_by(|a, b| b.total_cmp(a));
    let loud = &frames[..frames.len().div_ceil(2)];
    let power = loud.iter().sum::<f32>() / loud.len().max(1) as f32;
    (power > 0.0).then(|| 10.0 * power.log10())
}

/// Microphone speech is brought to about this level (active speech, dBFS) before speech detection
/// and recognition: far or quiet speakers otherwise sit 10–20 dB below what whisper is used to.
const LEVEL_TARGET_DB: f32 = -26.0;
/// Most gain levelling adds (more mostly brings up noise)...
const LEVEL_MAX_GAIN_DB: f32 = 20.0;
/// ...and most it takes away.
const LEVEL_MIN_GAIN_DB: f32 = -10.0;
/// One gain per this much audio, changing smoothly in between: slow enough not to pump.
const LEVEL_WINDOW: usize = 30 * RATE;
/// Levelling ignores audio with less active speech than this (frames of 20 ms).
const LEVEL_MIN_FRAMES: usize = 25;
/// Speech less than this far above the channel's noise floor (dB) counts as quiet for the
/// hallucination filter (a "Thank you." heard in near-silence).
const QUIET_SNR_DB: f32 = 15.0;
/// Without a noise floor yet, speech below this level (dBFS) is quiet.
const QUIET_DB: f32 = -60.0;

/// Frames of `frame_db`: 20 ms.
const FRAME: usize = RATE / 50;

/// Level of each 20 ms frame of `audio`, dBFS.
fn frame_db(audio: &[f32]) -> Vec<f32> {
    audio
        .chunks_exact(FRAME)
        .map(|f| 10.0 * (f.iter().map(|s| s * s).sum::<f32>() / f.len() as f32).max(1e-12).log10())
        .collect()
}

/// The noise floor of `audio`, dBFS: its quietest frames (10th percentile).
fn noise_floor_db(audio: &[f32]) -> Option<f32> {
    let mut db = frame_db(audio);
    db.sort_by(|a, b| a.total_cmp(b));
    db.get(db.len() / 10).copied()
}

/// Level of the active speech in `audio`, dBFS: the power average of the frames clearly above
/// its noise floor. `None` with too little of it (silence, steady noise).
fn active_level(audio: &[f32]) -> Option<f32> {
    let db = frame_db(audio);
    let floor = noise_floor_db(audio)?;
    let active: Vec<f32> = db.into_iter().filter(|d| *d > (floor + 10.0).max(-70.0)).collect();
    (active.len() >= LEVEL_MIN_FRAMES).then(|| 10.0 * (active.iter().map(|d| 10f32.powf(d / 10.0)).sum::<f32>() / active.len() as f32).log10())
}

/// `audio` with its speech brought to about `LEVEL_TARGET_DB`: one gain per `LEVEL_WINDOW`,
/// interpolated between window centres, then soft-limited below full scale. Not a compressor:
/// within a window everything keeps its level relative to the rest.
pub fn level(audio: &[f32]) -> Vec<f32> {
    let gain = |a: &[f32]| active_level(a).map_or(0.0, |l| (LEVEL_TARGET_DB - l).clamp(LEVEL_MIN_GAIN_DB, LEVEL_MAX_GAIN_DB));
    let gains: Vec<(f32, f32)> = audio
        .chunks(LEVEL_WINDOW)
        .enumerate()
        .map(|(i, w)| ((i * LEVEL_WINDOW + w.len() / 2) as f32, 10f32.powf(gain(w) / 20.0)))
        .collect();
    if gains.iter().all(|(_, g)| *g == 1.0) {
        return audio.to_vec();
    }
    let mut next = 0;
    audio
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let t = i as f32;
            while next < gains.len() && gains[next].0 <= t {
                next += 1;
            }
            let g = match (next.checked_sub(1).map(|p| gains[p]), gains.get(next)) {
                (Some((t0, g0)), Some(&(t1, g1))) => g0 + (g1 - g0) * (t - t0) / (t1 - t0),
                (Some((_, g)), None) | (None, Some(&(_, g))) => g,
                (None, None) => 1.0,
            };
            soft_limit(s * g)
        })
        .collect()
}

/// A voice is as loud as its active speech (as `active_level`) this long around a moment...
const LIFT_SHORT: usize = RATE / 2;
/// ...where at least this share of the frames is active (else it's a pause)...
const LIFT_SHORT_SHARE: f32 = 0.3;
/// ...and is lifted towards the level of all the active speech this long around it...
const LIFT_LONG: usize = 30 * RATE;
/// ...by at most this much (dB).
const LIFT_MAX_DB: f32 = 12.0;
/// The lift ramps over this long, never above what a moment of louder speech needs.
const LIFT_EASE: usize = RATE * 3 / 10;
/// Pauses up to this long keep the lift of the speech on both sides (the lower one); longer
/// ones, silence and noise, get none.
const LIFT_HOLD: usize = RATE;

/// The lift (dB, per frame of `db`) `lift_quiet` gives: for each moment of active speech, how
/// far it is below the speech around it, up to `LIFT_MAX_DB`.
fn lift_gains(db: &[f32], floor: f32) -> Vec<f32> {
    let n = db.len();
    let threshold = (floor + 10.0).max(-70.0);
    // Prefix sums of active power and of active frames.
    let (mut power, mut count) = (vec![0f64; n + 1], vec![0usize; n + 1]);
    for (k, d) in db.iter().enumerate() {
        let on = *d > threshold;
        power[k + 1] = power[k] + if on { 10f64.powf(*d as f64 / 10.0) } else { 0.0 };
        count[k + 1] = count[k] + on as usize;
    }
    let level = |k: usize, half: usize, min: usize| {
        let (a, b) = (k.saturating_sub(half), (k + half + 1).min(n));
        let c = count[b] - count[a];
        (c >= min.max(1)).then(|| 10.0 * ((power[b] - power[a]) / c as f64).log10() as f32)
    };
    let (short, long) = (LIFT_SHORT / FRAME / 2, LIFT_LONG / FRAME / 2);
    let short_min = (LIFT_SHORT_SHARE * (2 * short + 1) as f32).ceil() as usize;
    let want: Vec<Option<f32>> = (0..n)
        .map(|k| {
            let here = level(k, short, short_min)?;
            let around = level(k, long, LEVEL_MIN_FRAMES)?;
            Some((around - here).clamp(0.0, LIFT_MAX_DB))
        })
        .collect();
    // Pauses: the lower lift of the speech on both sides, none when long or at an edge.
    let hold = LIFT_HOLD / FRAME;
    let mut lift = vec![0f32; n];
    let mut k = 0;
    while k < n {
        if let Some(w) = want[k] {
            lift[k] = w;
            k += 1;
            continue;
        }
        let end = (k..n).find(|&j| want[j].is_some()).unwrap_or(n);
        let around = (k.checked_sub(1).and_then(|p| want[p]), want.get(end).copied().flatten());
        if let (Some(a), Some(b)) = around {
            if end - k <= hold {
                lift[k..end].fill(a.min(b));
            }
        }
        k = end;
    }
    // Ease: the lowest lift within reach, then the mean within reach, so the lift never exceeds
    // what any frame needs and changes gradually.
    let r = LIFT_EASE / FRAME / 2;
    let window = |k: usize| k.saturating_sub(r)..(k + r + 1).min(n);
    let low: Vec<f32> = (0..n).map(|k| lift[window(k)].iter().copied().fold(f32::INFINITY, f32::min)).collect();
    (0..n).map(|k| low[window(k)].iter().sum::<f32>() / window(k).len() as f32).collect()
}

/// `audio` with moments of speech quieter than the speech around them (a far or soft voice next
/// to a near one) brought towards its level, by up to `LIFT_MAX_DB`; `floor` is the noise floor
/// of the recording (`noise_floor_db`), what counts as speech is clearly above it. Louder speech,
/// silence and noise keep their level; peaks are soft-limited. `level` alone brings a stretch of
/// speech to one level, which leaves a far voice 8–10 dB under a near one, and whisper drops or
/// garbles it (measured on a real meeting's final pass: about 1.7 points less word error and
/// fewer dropped words with this first; it didn't help the live view).
fn lift_quiet(audio: &[f32], floor: Option<f32>) -> Vec<f32> {
    let Some(floor) = floor else { return audio.to_vec() };
    let gains = lift_gains(&frame_db(audio), floor);
    if gains.iter().all(|g| *g < 0.01) {
        return audio.to_vec();
    }
    let gains: Vec<f32> = gains.into_iter().map(|g| 10f32.powf(g / 20.0)).collect();
    let last = gains.len() - 1;
    audio
        .iter()
        .enumerate()
        .map(|(i, s)| {
            // Between frame centres.
            let t = (i as f32 - FRAME as f32 / 2.0) / FRAME as f32;
            let k = (t.max(0.0) as usize).min(last);
            let f = (t - k as f32).clamp(0.0, 1.0);
            let g = gains[k] + (gains[(k + 1).min(last)] - gains[k]) * f;
            soft_limit(s * g)
        })
        .collect()
}

/// Leaves samples below 0.71 alone and bends louder ones smoothly towards 0.89.
fn soft_limit(x: f32) -> f32 {
    const CEILING: f32 = 0.89;
    const KNEE: f32 = CEILING * 0.8;
    if x.abs() <= KNEE {
        return x;
    }
    x.signum() * (KNEE + (CEILING - KNEE) * ((x.abs() - KNEE) / (CEILING - KNEE)).tanh())
}

/// Whether speech at `level` (dBFS, `level_db`) is quiet for the hallucination filter, given
/// the channel's noise floor: relative to the recording, as a far voice in a quiet room is
/// soft in absolute terms but clearly speech.
fn is_quiet(level: Option<f32>, floor: Option<f32>) -> bool {
    match (level, floor) {
        (None, _) => true,
        (Some(l), Some(f)) => l < f + QUIET_SNR_DB,
        (Some(l), None) => l < QUIET_DB,
    }
}

/// One microphone voice after clustering, as `pick_me` sees it.
struct MicVoice {
    /// Duration-weighted sum of its embeddings.
    sum: Vec<f32>,
    ms: u64,
    /// Duration-weighted mean speech level, dBFS.
    level: Option<f32>,
}

/// Below this share of the microphone's speech, a voice isn't taken to be the user's by loudness.
const ME_MIN_SHARE: u64 = 10; // 1/10

/// Which microphone voice is the user: the one matching the voice print; without a print (or no
/// match), the loudest in a meeting in the room (the user sits at the laptop; everyone else is
/// farther away), and the one who talked longest on a call (the others are in system audio).
fn pick_me(voices: &[MicVoice], print: Option<&[f32]>, in_room: bool) -> usize {
    let by_print = print.and_then(|p| {
        voices
            .iter()
            .enumerate()
            .filter(|(_, v)| !v.sum.is_empty())
            .map(|(i, v)| (i, speakers::cosine_sim(&v.sum, p)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|(_, sim)| *sim >= PRINT_MATCH)
            .map(|(i, _)| i)
    });
    let total: u64 = voices.iter().map(|v| v.ms).sum();
    let loudest = || {
        voices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.ms * ME_MIN_SHARE >= total)
            .filter_map(|(i, v)| v.level.map(|l| (i, l)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    let longest = || (0..voices.len()).max_by_key(|&i| voices[i].ms).unwrap_or(0);
    by_print.or_else(|| in_room.then(loudest).flatten()).unwrap_or_else(longest)
}

/// Final speaker labels for the whole meeting: clusters each channel's voices, makes the user's
/// microphone voice "Me" (`pick_me`), numbers everyone else by first appearance, and keeps names
/// the user gave while the meeting ran.
pub fn assign_speakers(
    segments: &mut [Segment],
    embeddings: &[Option<Vec<f32>>],
    hints: &MeHints,
    speakers: &mut Vec<Speaker>,
    diarize: bool,
) {
    let old_names: HashMap<u32, (String, bool)> = speakers.iter().map(|s| (s.id, (s.name.clone(), s.renamed))).collect();
    let old_ids: Vec<u32> = segments.iter().map(|s| s.speaker).collect();
    // (channel, cluster) per segment.
    let mut label: Vec<(Channel, usize)> = vec![(Channel::Mic, 0); segments.len()];
    let mut mic_me = 0;
    let talk = |ch: Channel| segments.iter().filter(|s| s.channel == ch).map(|s| s.end_ms.saturating_sub(s.start_ms)).sum::<u64>();
    // No one on the call (or no call recorded): everyone is in the room.
    let in_room = talk(Channel::System) * 20 < talk(Channel::Mic);
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
        if channel == Channel::Mic && n > 1 {
            let mut voices: Vec<MicVoice> = (0..n).map(|_| MicVoice { sum: Vec::new(), ms: 0, level: None }).collect();
            let mut level_ms = vec![(0.0f32, 0u64); n];
            for (k, &i) in idx.iter().enumerate() {
                let (v, ms) = (&mut voices[labels[k]], segments[i].end_ms.saturating_sub(segments[i].start_ms));
                v.ms += ms;
                if let Some(e) = items[k].embedding {
                    if v.sum.is_empty() {
                        v.sum = vec![0.0; e.len()];
                    }
                    add_weighted(&mut v.sum, e, ms);
                }
                if let Some(Some(l)) = hints.levels.get(i) {
                    level_ms[labels[k]].0 += l * ms as f32;
                    level_ms[labels[k]].1 += ms;
                }
            }
            for (v, (sum, ms)) in voices.iter_mut().zip(level_ms) {
                v.level = (ms > 0).then(|| sum / ms as f32);
            }
            mic_me = pick_me(&voices, hints.print, in_room);
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

pub(super) fn clock(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// Transcript lines for the summary: consecutive segments of one speaker become one line.
pub fn transcript_lines(segments: &[Segment], speakers: &[Speaker]) -> Vec<String> {
    turns(segments, speakers, &[]).into_iter().map(|(_, line)| line).collect()
}

/// "[mm:ss] Speaker: text" turns with their start time: consecutive segments of one speaker are
/// one turn, unless a turn would pass 1200 characters or one of the `breaks` (meeting times).
fn turns(segments: &[Segment], speakers: &[Speaker], breaks: &[u64]) -> Vec<(u64, String)> {
    let name = |id: u32| speakers.iter().find(|s| s.id == id).map(|s| s.name.clone()).unwrap_or_else(|| default_name(id, true));
    let mut sorted: Vec<&Segment> = segments.iter().collect();
    sorted.sort_by_key(|s| s.start_ms);
    let mut lines: Vec<(u64, u32, String)> = Vec::new();
    for s in sorted {
        match lines.last_mut() {
            Some((start, sp, text))
                if *sp == s.speaker && text.len() < 1200 && !breaks.iter().any(|b| *start < *b && *b <= s.start_ms) =>
            {
                text.push(' ');
                text.push_str(&s.text);
            }
            _ => lines.push((s.start_ms, s.speaker, s.text.clone())),
        }
    }
    lines.into_iter().map(|(ms, sp, text)| (ms, format!("[{}] {}: {}", clock(ms), name(sp), text))).collect()
}

/// The transcript lines with what was shown (`visuals`, see `visuals::lines`) merged in by time:
/// "[03:20] Screen: …" and "[05:10] Photo: …" lines between the turns, a turn broken where a visual
/// comes in. `caps` limits how much visual text goes in. For the notes, the live questions and the
/// live map; with no visuals it is `transcript_lines`.
pub fn timeline_lines(segments: &[Segment], speakers: &[Speaker], visuals: &[super::Visual], caps: super::visuals::Caps) -> Vec<String> {
    let talk_chars = segments.iter().map(|s| s.text.len() + 1).sum();
    let shown = super::visuals::lines(visuals, caps, talk_chars);
    let breaks: Vec<u64> = shown.iter().map(|(ms, _)| *ms).collect();
    super::visuals::merge(turns(segments, speakers, &breaks), shown)
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
        Segment { start_ms: start, end_ms: end, channel, speaker: 0, text: text.into(), language: None, fixes: Vec::new() }
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
    fn timeline_lines_merge_what_was_shown_by_time() {
        use super::super::visuals::{self, Source, Visual};
        let speakers = vec![Speaker { id: 0, name: "Me".into(), renamed: false }, Speaker { id: 1, name: "Anna".into(), renamed: true }];
        let mut segments = vec![seg(Channel::Mic, 0, 1000, "Hello."), seg(Channel::Mic, 251_000, 256_000, "Die Peckl Ist."), seg(Channel::Mic, 266_000, 269_000, "Je Karton.")];
        segments[1].speaker = 1;
        segments[2].speaker = 1;
        let photo = Visual { id: 1, at_ms: 260_000, until_ms: None, source: Source::Photo, label: "IMG_1.HEIC".into(), text: "Packliste PL-0388\nKarton | Gewicht".into(), image: None };
        assert_eq!(
            timeline_lines(&segments, &speakers, &[photo], visuals::NOTES),
            ["[00:00] Me: Hello.", "[04:11] Anna: Die Peckl Ist.", "[04:20] Photo: Packliste PL-0388 / Karton | Gewicht", "[04:26] Anna: Je Karton."]
        );
        // Without visuals, the transcript lines as before (Anna's turn is one line).
        assert_eq!(timeline_lines(&segments, &speakers, &[], visuals::NOTES), transcript_lines(&segments, &speakers));
        assert_eq!(transcript_lines(&segments, &speakers)[1], "[04:11] Anna: Die Peckl Ist. Je Karton.");
    }

    /// Unit vector `cos` towards axis 0 and the rest towards `axis`.
    fn lean(axis: usize, cos: f32) -> Vec<f32> {
        (0..8).map(|k| if k == 0 { cos } else if k == axis { (1.0 - cos * cos).sqrt() } else { 0.0 }).collect()
    }

    #[test]
    fn live_odd_segments_show_under_the_nearest_voice() {
        let mut speakers = Vec::new();
        let mut live = LiveSpeakers::new(true, &[], None);
        let a = unit(1);
        assert_eq!(live.assign(Channel::Mic, Some(&a), 8_000, &mut speakers), ME);
        // Each odd segment is unlike the voice and unlike each other: no new speakers.
        for k in 2..7 {
            assert_eq!(live.assign(Channel::Mic, Some(&unit(k)), 6_000, &mut speakers), ME);
        }
        assert_eq!(speakers.iter().map(|s| s.id).collect::<Vec<_>>(), [ME]);
    }

    #[test]
    fn live_new_voice_once_enough_alike_speech_is_heard() {
        let mut speakers = Vec::new();
        let mut live = LiveSpeakers::new(true, &[], None);
        let (a, b) = (unit(1), unit(2));
        live.assign(Channel::Mic, Some(&a), 8_000, &mut speakers);
        // The newcomer's first lines show under the known voice until there are 20 s of them.
        assert_eq!(live.assign(Channel::Mic, Some(&b), 8_000, &mut speakers), ME);
        assert_eq!(live.assign(Channel::Mic, Some(&b), 8_000, &mut speakers), ME);
        assert_eq!(live.assign(Channel::Mic, Some(&b), 8_000, &mut speakers), 1);
        assert_eq!(live.assign(Channel::Mic, Some(&b), 3_000, &mut speakers), 1);
        assert_eq!(live.assign(Channel::Mic, Some(&a), 3_000, &mut speakers), ME);
        // Without a voice of its own, a segment keeps the channel's last speaker.
        assert_eq!(live.assign(Channel::Mic, None, 500, &mut speakers), ME);
        // System audio has its own voices, and the first one gets a number at once.
        assert_eq!(live.assign(Channel::System, Some(&a), 4_000, &mut speakers), 2);
    }

    #[test]
    fn live_voices_that_turn_out_alike_are_merged() {
        let mut speakers = Vec::new();
        let mut live = LiveSpeakers::new(true, &[], None);
        let a = unit(0);
        live.assign(Channel::Mic, Some(&a), 8_000, &mut speakers);
        // Similarity 0.5 to the first voice: a voice of its own after 24 s.
        let x = lean(1, 0.5);
        for _ in 0..3 {
            live.assign(Channel::Mic, Some(&x), 8_000, &mut speakers);
        }
        assert_eq!(live.assign(Channel::Mic, Some(&x), 8_000, &mut speakers), 1);
        // Speech in between pulls the two together: one voice, under the older id.
        live.assign(Channel::Mic, Some(&lean(1, 0.8)), 16_000, &mut speakers);
        assert_eq!(live.assign(Channel::Mic, Some(&x), 8_000, &mut speakers), ME);
    }

    #[test]
    fn live_me_is_the_voice_matching_the_print() {
        let mut speakers = Vec::new();
        let (a, b) = (unit(1), unit(2));
        let mut live = LiveSpeakers::new(true, &[], Some(&b));
        assert_eq!(live.assign(Channel::Mic, Some(&a), 8_000, &mut speakers), 1);
        for _ in 0..2 {
            live.assign(Channel::Mic, Some(&b), 8_000, &mut speakers);
        }
        assert_eq!(live.assign(Channel::Mic, Some(&b), 8_000, &mut speakers), ME);
    }

    fn mic_meeting(louder_second: bool, with_call: bool) -> (Vec<Segment>, Embeddings, Vec<Option<f32>>) {
        let (a, b) = (unit(1), unit(5));
        let (mut segments, mut embeddings, mut levels) = (Vec::new(), Vec::new(), Vec::new());
        // A talks 80 s, B 40 s.
        for i in 0..12u64 {
            let first = i % 3 != 2;
            segments.push(seg(Channel::Mic, i * 11_000, i * 11_000 + 10_000, "x"));
            embeddings.push(Some(if first { a.clone() } else { b.clone() }));
            levels.push(Some(if first == louder_second { -50.0 } else { -42.0 }));
        }
        if with_call {
            for i in 0..6u64 {
                segments.push(seg(Channel::System, 200_000 + i * 11_000, 210_000 + i * 11_000, "y"));
                embeddings.push(Some(unit(7)));
                levels.push(Some(-30.0));
            }
        }
        (segments, embeddings, levels)
    }

    fn me_speaks_at(segments: &[Segment]) -> Vec<u64> {
        segments.iter().filter(|s| s.speaker == ME).map(|s| s.start_ms).take(3).collect()
    }

    #[test]
    fn in_the_room_me_is_the_loudest_voice() {
        let (mut segments, embeddings, levels) = mic_meeting(true, false);
        let mut speakers = Vec::new();
        assign_speakers(&mut segments, &embeddings, &MeHints { print: None, levels: &levels }, &mut speakers, true);
        assert_eq!(me_speaks_at(&segments), [22_000, 55_000, 88_000]);
    }

    #[test]
    fn on_a_call_me_is_the_longest_microphone_voice() {
        let (mut segments, embeddings, levels) = mic_meeting(true, true);
        let mut speakers = Vec::new();
        assign_speakers(&mut segments, &embeddings, &MeHints { print: None, levels: &levels }, &mut speakers, true);
        assert_eq!(me_speaks_at(&segments), [0, 11_000, 33_000]);
    }

    #[test]
    fn me_is_the_voice_closest_to_the_print() {
        let (mut segments, embeddings, levels) = mic_meeting(false, false);
        let mut speakers = Vec::new();
        let print = lean(5, 0.3);
        assign_speakers(&mut segments, &embeddings, &MeHints { print: Some(&print), levels: &levels }, &mut speakers, true);
        assert_eq!(me_speaks_at(&segments), [22_000, 55_000, 88_000]);
        // A print that matches no one doesn't decide: the loudest voice is Me.
        let (mut segments, ..) = mic_meeting(false, false);
        assign_speakers(&mut segments, &embeddings, &MeHints { print: Some(&unit(3)), levels: &levels }, &mut speakers, true);
        assert_eq!(me_speaks_at(&segments), [0, 11_000, 33_000]);
    }

    /// `seconds` of a 200 Hz tone at `db` dBFS (RMS).
    fn tone(seconds: f32, db: f32) -> Vec<f32> {
        let amp = 10f32.powf(db / 20.0) * std::f32::consts::SQRT_2;
        (0..(seconds * RATE as f32) as usize).map(|i| amp * (i as f32 * 200.0 * std::f32::consts::TAU / RATE as f32).sin()).collect()
    }

    /// Quiet speech with pauses in between, as a far microphone hears it.
    fn talk(db: f32, seconds: usize) -> Vec<f32> {
        let mut audio = Vec::new();
        for _ in 0..seconds / 2 {
            audio.extend(tone(1.5, db));
            audio.extend(tone(0.5, db - 30.0));
        }
        audio
    }

    #[test]
    fn levelling_brings_far_speech_to_the_target() {
        let out = level(&talk(-46.0, 20));
        let got = active_level(&out).unwrap();
        assert!((got - LEVEL_TARGET_DB).abs() < 1.0, "{got}");
        // Very quiet speech gets at most the maximum gain.
        let got = active_level(&level(&talk(-60.0, 20))).unwrap();
        assert!((got - (-60.0 + LEVEL_MAX_GAIN_DB)).abs() < 1.0, "{got}");
    }

    #[test]
    fn levelling_leaves_steady_noise_alone_and_limits_peaks() {
        let hum = tone(10.0, -60.0);
        assert_eq!(level(&hum), hum);
        // A click in far speech: the speech gets its gain, the click stays below full scale.
        let mut clicked = talk(-46.0, 10);
        clicked[50_000] = 0.3;
        let out = level(&clicked);
        assert!(out.iter().all(|s| s.abs() <= 0.89) && out[50_000] > 0.8);
    }

    #[test]
    fn levelling_follows_the_level_slowly() {
        // A minute of quiet talk, then a minute of loud talk: both end up near the target.
        let audio: Vec<f32> = talk(-46.0, 60).into_iter().chain(talk(-20.0, 60)).collect();
        let out = level(&audio);
        let quiet = active_level(&out[..20 * RATE]).unwrap();
        let loud = active_level(&out[out.len() - 20 * RATE..]).unwrap();
        assert!((quiet - LEVEL_TARGET_DB).abs() < 1.5 && (loud - LEVEL_TARGET_DB).abs() < 1.5, "{quiet} {loud}");
    }

    /// `audio` with white noise at `noise_db` dBFS (RMS) added.
    fn noisy(audio: Vec<f32>, noise_db: f32) -> Vec<f32> {
        let amp = 10f32.powf(noise_db / 20.0) * 3f32.sqrt();
        let mut seed = 1u32;
        audio
            .into_iter()
            .map(|s| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                s + amp * ((seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0)
            })
            .collect()
    }

    /// 10 s of a voice at -30 dBFS, then 10 s of one at -40, with a noise floor at -70.
    fn two_voices() -> Vec<f32> {
        noisy(talk(-30.0, 10).into_iter().chain(talk(-40.0, 10)).collect(), -70.0)
    }

    #[test]
    fn lifting_brings_a_quieter_voice_close_to_the_louder_one() {
        let audio = two_voices();
        let floor = noise_floor_db(&audio);
        let out = lift_quiet(&audio, floor);
        let (loud, quiet) = (&out[RATE..9 * RATE], &out[11 * RATE..19 * RATE]);
        let (loud, quiet) = (active_level(loud).unwrap(), active_level(quiet).unwrap());
        assert!((loud - -30.0).abs() < 0.5, "the louder voice stays as it was: {loud}");
        assert!(quiet > -34.0 && quiet < -29.0, "the quieter voice comes up: {quiet}");
    }

    #[test]
    fn lifting_is_capped() {
        let audio = noisy(talk(-20.0, 10).into_iter().chain(talk(-45.0, 10)).collect(), -75.0);
        let out = lift_quiet(&audio, noise_floor_db(&audio));
        let quiet = active_level(&out[11 * RATE..19 * RATE]).unwrap();
        assert!((quiet - (-45.0 + LIFT_MAX_DB)).abs() < 1.0, "{quiet}");
        assert!(out.iter().all(|s| s.abs() <= 0.89));
    }

    #[test]
    fn lifting_leaves_silence_and_steady_noise_alone() {
        // Noise alone: nothing to lift.
        let hum = noisy(vec![0.0; 10 * RATE], -60.0);
        assert_eq!(lift_quiet(&hum, noise_floor_db(&hum)), hum);
        // A long silence between the two voices stays as quiet as it was.
        let audio: Vec<f32> =
            noisy(talk(-30.0, 10).into_iter().chain(vec![0.0; 5 * RATE]).chain(talk(-40.0, 10)).collect(), -70.0);
        let out = lift_quiet(&audio, noise_floor_db(&audio));
        let gap = |a: &[f32]| level_db(&a[11 * RATE..14 * RATE]).unwrap();
        assert!((gap(&out) - gap(&audio)).abs() < 0.5, "{} {}", gap(&out), gap(&audio));
    }

    #[test]
    fn lifting_changes_gain_smoothly() {
        let audio = two_voices();
        let gains = lift_gains(&frame_db(&audio), noise_floor_db(&audio).unwrap());
        assert!(gains.windows(2).all(|g| (g[1] - g[0]).abs() < 1.0), "no jumps of 1 dB or more per frame");
        assert!(gains.iter().all(|g| (0.0..=LIFT_MAX_DB).contains(g)));
    }

    #[test]
    fn quiet_means_close_to_the_noise_floor() {
        assert!(is_quiet(Some(-60.0), Some(-67.0)));
        assert!(!is_quiet(Some(-47.0), Some(-67.0)), "far speech is well above the floor");
        assert!(is_quiet(Some(-70.0), None));
        assert!(!is_quiet(Some(-45.0), None));
    }

    #[test]
    fn language_settles_once_it_clearly_dominates() {
        let (mut heard, mut settled) = (HashMap::new(), None);
        settle_language(&mut heard, &mut settled, Some("de"), 100_000);
        settle_language(&mut heard, &mut settled, Some("en"), 4_000);
        assert_eq!(settled, None, "not enough speech yet");
        settle_language(&mut heard, &mut settled, Some("de"), 30_000);
        assert_eq!(settled.as_deref(), Some("de"));
        // A meeting in two languages doesn't settle.
        let (mut heard, mut settled) = (HashMap::new(), None);
        for _ in 0..10 {
            settle_language(&mut heard, &mut settled, Some("de"), 20_000);
            settle_language(&mut heard, &mut settled, Some("en"), 5_000);
        }
        assert_eq!(settled, None);
    }

    #[test]
    fn run_on_text_without_punctuation_is_noticed() {
        assert!(lacks_punctuation("hat woran das gelegenheit hat sozusagen um dann auch sie wieder den mut zu nehmen"));
        assert!(!lacks_punctuation("Hat woran das gelegen, sozusagen um dann auch wieder den Mut zu nehmen und zu sagen."));
        assert!(!lacks_punctuation("ja genau"));
    }

    #[test]
    fn a_channel_whose_recording_cant_be_read_keeps_its_live_transcript() {
        let engine = Engine::default();
        let decode = DecodeOptions::default();
        let no_wait = || {};
        let ctx = Ctx {
            engine: &engine,
            embedder: None,
            languages: &[],
            prompt: None,
            decode: &decode,
            dictionary: None,
            remove_fillers: false,
            yield_to_dictation: &no_wait,
        };
        let mut live = vec![seg(Channel::System, 1_000, 4_000, "Die Lieferung kommt Donnerstag."), seg(Channel::Mic, 5_000, 6_000, "Gut.")];
        live[0].speaker = 2;
        // The system recording fails to read; the microphone's is empty (nothing to transcribe).
        let broken = |_: usize, _: usize| -> anyhow::Result<Vec<f32>> { anyhow::bail!("disk error") };
        let empty = |_: usize, _: usize| -> anyhow::Result<Vec<f32>> { Ok(Vec::new()) };
        let recorded = [
            Recorded { channel: Channel::System, len: 10 * S, read: &broken },
            Recorded { channel: Channel::Mic, len: 0, read: &empty },
        ];
        let (segments, embeddings) = final_segments(&ctx, &recorded, &live, &mut |_| {}).unwrap();
        assert_eq!(segments.len(), 1, "the system's live segment stays, the empty microphone has none");
        assert_eq!((segments[0].speaker, segments[0].text.as_str()), (2, "Die Lieferung kommt Donnerstag."));
        assert_eq!(embeddings, [None]);
        // Every channel failing fails the pass: the caller keeps the whole live transcript.
        let recorded = [Recorded { channel: Channel::Mic, len: 10 * S, read: &broken }];
        let e = final_segments(&ctx, &recorded, &live, &mut |_| {}).unwrap_err();
        assert!(format!("{e:#}").contains("disk error"), "{e:#}");
    }

    #[test]
    fn final_progress_follows_the_speech_decoded() {
        let flat = |shares: &[Vec<f32>]| shares.iter().flatten().copied().collect::<Vec<f32>>();
        let rises = |v: &[f32]| v.windows(2).all(|p| p[0] <= p[1]) && v.iter().all(|f| (SCAN_SHARE..=1.0).contains(f));
        // An in-room meeting: the system channel (decoded first) is silence, so the microphone
        // covers the whole bar after the scan instead of starting halfway.
        let shares = decode_shares(&[vec![], vec![28 * S, 20 * S, 12 * S]]);
        assert!(shares[0].is_empty());
        assert!((shares[1][0] - (SCAN_SHARE + (1.0 - SCAN_SHARE) * 28.0 / 60.0)).abs() < 1e-6, "{shares:?}");
        assert_eq!(shares[1].last(), Some(&1.0));
        assert!(rises(&flat(&shares)));
        // Both channels speak: each takes its share of the speech, the system's first.
        let shares = decode_shares(&[vec![10 * S], vec![10 * S, 20 * S]]);
        assert!((shares[0][0] - (SCAN_SHARE + (1.0 - SCAN_SHARE) / 4.0)).abs() < 1e-6, "{shares:?}");
        assert!((shares[1][0] - (SCAN_SHARE + (1.0 - SCAN_SHARE) / 2.0)).abs() < 1e-6, "{shares:?}");
        assert_eq!(shares[1][1], 1.0);
        assert!(rises(&flat(&shares)));
        // Nothing to decode at all.
        assert_eq!(decode_shares(&[vec![], vec![]]), vec![Vec::<f32>::new(), Vec::new()]);
    }

    #[test]
    fn final_segments_lose_what_they_only_graze() {
        let window = [0..10 * S, 20 * S..30 * S, 40 * S..50 * S];
        // Starts in the last 0.1 s of the first region: begins with the second.
        assert_eq!(trim_to_speech(&window, 10 * S - S / 10, 30 * S), (20 * S, 30 * S));
        // Ends a moment into the third: ends with the second.
        assert_eq!(trim_to_speech(&window, 22 * S, 40 * S + S / 10), (22 * S, 30 * S));
        // A short segment alone in a region stays as it is.
        assert_eq!(trim_to_speech(&window, 9 * S + S / 2, 10 * S), (9 * S + S / 2, 10 * S));
    }

    #[test]
    fn final_windows_hold_up_to_28_s_cut_at_pauses() {
        let audio = vec![0.1f32; 120 * S];
        let regions = [0..10 * S, 12 * S..22 * S, 25 * S..34 * S, 40 * S..45 * S];
        assert_eq!(final_windows(&audio, &regions), [vec![0..10 * S, 12 * S..22 * S], vec![25 * S..34 * S, 40 * S..45 * S]]);
        // A long stretch without a pause is cut at its quietest moment.
        let mut audio = vec![0.1f32; 70 * S];
        audio[24 * S..24 * S + S / 10].fill(0.0);
        let w = final_windows(&audio, &[0..70 * S]);
        assert_eq!(w.len(), 3);
        assert_eq!(w[0], [0..24 * S + S / 20]);
        assert!(w.iter().all(|w| clip_len(w) <= FINAL_WINDOW));
    }

    #[test]
    fn final_windows_end_at_long_pauses_not_short_ones() {
        let audio = vec![0.1f32; 60 * S];
        // Two sentences with a 2 s pause between; within each, words with short pauses. Packing
        // greedily would end the first window after "24.4..30", inside the second sentence.
        let regions = [0..8 * S, 8 * S + 4 * S / 10..16 * S, 18 * S..24 * S, 24 * S + 4 * S / 10..30 * S, 30 * S + 4 * S / 10..36 * S];
        assert_eq!(
            final_windows(&audio, &regions),
            [regions[..2].to_vec(), regions[2..].to_vec()],
            "the windows are cut at the long pause"
        );
    }

    #[test]
    fn final_windows_keep_what_fits_together() {
        let audio = vec![0.1f32; 30 * S];
        let regions = [0..10 * S, 12 * S..20 * S, 23 * S..26 * S];
        assert_eq!(final_windows(&audio, &regions), [regions.to_vec()]);
    }

    #[test]
    fn speech_level_ignores_pauses() {
        let mut audio = vec![0.0f32; 16_000];
        audio.extend((0..16_000).map(|i| if i % 2 == 0 { 0.1 } else { -0.1 }));
        let db = level_db(&audio).unwrap();
        assert!((db + 20.0).abs() < 0.5, "{db}");
        assert_eq!(level_db(&[]), None);
    }

    /// Speaker labelling of a recorded meeting through this code, for measuring changes:
    /// `WORDFALL_REPLAY=<meeting dir> cargo test --release --lib replay_meeting -- --ignored --nocapture`.
    /// The dir holds mic.wav (and system.wav) and the segments (meeting.json, or the file in
    /// `WORDFALL_REPLAY_SEGMENTS`). `WORDFALL_REPLAY_PRINT` is a voice.json, or another meeting's
    /// dir whose microphone segments (≥ 3 s) are learnt as if they were dictations.
    /// `WORDFALL_REPLAY_OUT` gets a TSV: segment, start, end, channel, saved, live and final speaker.
    #[test]
    #[ignore]
    fn replay_meeting() {
        use std::path::{Path, PathBuf};
        let Ok(dir) = std::env::var("WORDFALL_REPLAY").map(PathBuf::from) else { return };
        let data = dirs::data_dir().unwrap().join("com.wordfall.app");
        let model = speakers::model_path(&data.join("models")).expect("speaker model");
        let t = std::time::Instant::now();
        let embedder = Embedder::load(&model).unwrap();
        println!("speaker model loaded in {} ms", t.elapsed().as_millis());
        let file = std::env::var("WORDFALL_REPLAY_SEGMENTS").map(PathBuf::from).unwrap_or(dir.join("meeting.json"));
        let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let mut segments: Vec<Segment> = serde_json::from_value(saved["segments"].clone()).unwrap();
        let wav = |dir: &Path, ch: Channel| crate::audio::read_wav_16k(&dir.join(if ch == Channel::Mic { "mic.wav" } else { "system.wav" })).ok();
        let cut = |audio: &[f32], s: &Segment| audio[(s.start_ms as usize * 16).min(audio.len())..(s.end_ms as usize * 16).min(audio.len())].to_vec();
        let (mic, system) = (wav(&dir, Channel::Mic).unwrap(), wav(&dir, Channel::System).unwrap_or_default());
        let audio_of = |s: &Segment| cut(if s.channel == Channel::Mic { &mic } else { &system }, s);
        let t = std::time::Instant::now();
        let embeddings: Vec<Option<Vec<f32>>> = segments.iter().map(|s| embedder.embed(&audio_of(s)).unwrap()).collect();
        let levels: Vec<Option<f32>> = segments.iter().map(|s| (s.channel == Channel::Mic).then(|| level_db(&audio_of(s))).flatten()).collect();
        println!("embedded {} segments in {:.1}s", segments.len(), t.elapsed().as_secs_f32());
        let print: Option<Vec<f32>> = std::env::var("WORDFALL_REPLAY_PRINT").ok().map(|p| {
            let p = PathBuf::from(p);
            if p.is_file() {
                return crate::storage::read_json::<speakers::VoicePrint>(&p).unwrap().embedding;
            }
            let other: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p.join("meeting.json")).unwrap()).unwrap();
            let other: Vec<Segment> = serde_json::from_value(other["segments"].clone()).unwrap();
            let mic = wav(&p, Channel::Mic).unwrap();
            let mut print = None;
            for s in other.iter().filter(|s| s.channel == Channel::Mic && s.end_ms - s.start_ms >= 3_000) {
                if let Some(e) = embedder.embed(&cut(&mic, s)).unwrap() {
                    print = Some(speakers::VoicePrint::learn(print, &e, 0));
                }
            }
            let print = print.expect("no microphone speech to learn from");
            println!("voice print from {} segments", print.count);
            print.embedding
        });

        let ms = |s: &Segment| s.end_ms.saturating_sub(s.start_ms);
        let saved_ids: Vec<u32> = segments.iter().map(|s| s.speaker).collect();
        let mut speakers_live = Vec::new();
        let mut live = LiveSpeakers::new(true, &[], print.as_deref());
        let live_ids: Vec<u32> =
            segments.iter().zip(&embeddings).map(|(s, e)| live.assign(s.channel, e.as_deref(), ms(s), &mut speakers_live)).collect();
        let mut speakers = Vec::new();
        let t = std::time::Instant::now();
        assign_speakers(&mut segments, &embeddings, &MeHints { print: print.as_deref(), levels: &levels }, &mut speakers, true);
        println!("final pass in {} ms", t.elapsed().as_millis());
        let final_ids: Vec<u32> = segments.iter().map(|s| s.speaker).collect();

        let talk = |ids: &[u32]| {
            let mut t: HashMap<u32, (u64, usize)> = HashMap::new();
            for (s, id) in segments.iter().zip(ids) {
                let e = t.entry(*id).or_default();
                e.0 += ms(s);
                e.1 += 1;
            }
            t
        };
        let total: u64 = segments.iter().map(ms).sum();
        for (name, ids) in [("saved", &saved_ids), ("live", &live_ids), ("final", &final_ids)] {
            let t = talk(ids);
            let mut by_talk: Vec<(u32, u64)> = t.iter().map(|(id, (ms, _))| (*id, *ms)).collect();
            by_talk.sort_by_key(|(_, ms)| std::cmp::Reverse(*ms));
            let small: u64 = by_talk.iter().filter(|(_, ms)| *ms < 30_000).map(|(_, ms)| ms).sum();
            println!(
                "{name:>5}: {} ids, max id {}, {} with >= 30 s, {:.1} % of talk in ids < 30 s, {} single-segment ids; top {:?}",
                t.len(),
                t.keys().max().unwrap(),
                by_talk.iter().filter(|(_, ms)| *ms >= 30_000).count(),
                100.0 * small as f32 / total as f32,
                t.values().filter(|(_, n)| *n == 1).count(),
                by_talk.iter().take(8).map(|(id, ms)| format!("{id}:{}s", ms / 1000)).collect::<Vec<_>>()
            );
        }
        // Stability: the saved meeting's big voices against the final ones.
        let saved_talk = talk(&saved_ids);
        let mut agree = (0, 0);
        let mut targets = Vec::new();
        for (&id, &(t, _)) in saved_talk.iter().filter(|(_, (t, _))| *t >= 30_000) {
            let mut votes: HashMap<u32, u64> = HashMap::new();
            for ((s, a), b) in segments.iter().zip(&saved_ids).zip(&final_ids) {
                if *a == id {
                    *votes.entry(*b).or_default() += ms(s);
                }
            }
            let (to, kept) = votes.into_iter().max_by_key(|(_, ms)| *ms).unwrap();
            println!("saved {id} ({} s) -> final {to}: {:.1} % of its talk", t / 1000, 100.0 * kept as f32 / t as f32);
            agree = (agree.0 + kept, agree.1 + t);
            targets.push(to);
        }
        let apart = targets.iter().collect::<std::collections::HashSet<_>>().len() == targets.len();
        println!("big saved voices: {:.1} % of their talk kept together, kept apart: {apart}", 100.0 * agree.0 as f32 / agree.1.max(1) as f32);
        // Me: level and voice print similarity of each final microphone voice.
        for sp in &speakers {
            let idx: Vec<usize> = (0..segments.len()).filter(|&i| final_ids[i] == sp.id && segments[i].channel == Channel::Mic).collect();
            if idx.is_empty() {
                continue;
            }
            let mut sum = vec![0.0; embeddings.iter().flatten().next().map_or(0, Vec::len)];
            let (mut lv, mut lm) = (0.0, 0);
            for &i in &idx {
                if let Some(e) = &embeddings[i] {
                    add_weighted(&mut sum, e, ms(&segments[i]));
                }
                if let Some(l) = levels[i] {
                    lv += l * ms(&segments[i]) as f32;
                    lm += ms(&segments[i]);
                }
            }
            let sim = print.as_deref().map(|p| speakers::cosine_sim(&sum, p));
            println!("  {} ({}): {} s, level {:.1} dBFS, print similarity {sim:?}", sp.name, sp.id, idx.iter().map(|&i| ms(&segments[i])).sum::<u64>() / 1000, lv / lm.max(1) as f32);
        }
        if let Ok(out) = std::env::var("WORDFALL_REPLAY_OUT") {
            let rows: Vec<String> = segments
                .iter()
                .enumerate()
                .map(|(i, s)| format!("{i}\t{}\t{}\t{:?}\t{}\t{}\t{}", s.start_ms, s.end_ms, s.channel, saved_ids[i], live_ids[i], final_ids[i]))
                .collect();
            std::fs::write(out, rows.join("\n") + "\n").unwrap();
        }
    }

    /// The final pass of a recorded meeting through this code, for measuring accuracy changes:
    /// `WORDFALL_FINAL=<meeting dir> WORDFALL_FINAL_OUT=<file.json> cargo test --release --lib
    /// replay_final_pass -- --ignored --nocapture`. Uses the app's model and settings on the dir's
    /// mic.wav, with the live segments of its meeting.json. `WORDFALL_FINAL_SPANS="from-to,…"` (ms)
    /// decodes only the windows overlapping those spans (planned on the whole recording as usual):
    /// a quick stand-in for the whole pass when scoring excerpts. Writes `{"segments": […]}`.
    #[test]
    #[ignore]
    fn replay_final_pass() {
        use std::path::PathBuf;
        let Ok(dir) = std::env::var("WORDFALL_FINAL").map(PathBuf::from) else { return };
        let data = dirs::data_dir().unwrap().join("com.wordfall.app");
        let settings = crate::settings::Settings::load(&data);
        let dirs = settings.model_dirs(&data);
        let entry = crate::transcribe::catalog_entry(&settings.model).unwrap();
        let engine = Engine::default();
        engine.load(entry, &dirs.locate(entry).unwrap().0).unwrap();
        *engine.silero_path.lock() = Some(dirs.own.join(crate::transcribe::SILERO_FILE)).filter(|p| p.exists());
        if settings.languages.len() > 1 {
            engine.prepare_detection().unwrap();
        }
        let embedder = speakers::model_path(&dirs.own).map(|p| Embedder::load(&p).unwrap());
        let dictionary = crate::store::Store::load(&data).dictionary;
        let no_wait = || {};
        let ctx = Ctx {
            engine: &engine,
            embedder: embedder.as_ref(),
            languages: &settings.languages,
            prompt: None,
            decode: &settings.decode,
            dictionary: Some(&dictionary),
            remove_fillers: true,
            yield_to_dictation: &no_wait,
        };
        let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("meeting.json")).unwrap()).unwrap();
        let live: Vec<Segment> = serde_json::from_value(saved["segments"].clone()).unwrap();
        let mic = crate::audio::read_wav_16k(&dir.join("mic.wav")).unwrap();
        let read = |start: usize, len: usize| Ok(mic[start.min(mic.len())..(start + len).min(mic.len())].to_vec());
        let spans: Vec<std::ops::Range<usize>> = std::env::var("WORDFALL_FINAL_SPANS")
            .map(|s| {
                s.split(',')
                    .map(|p| {
                        let (a, b) = p.split_once('-').unwrap();
                        a.trim().parse::<usize>().unwrap() * 16..b.trim().parse::<usize>().unwrap() * 16
                    })
                    .collect()
            })
            .unwrap_or_default();
        let t = std::time::Instant::now();
        let segments: Vec<Segment> = if spans.is_empty() {
            let recorded = [Recorded { channel: Channel::Mic, len: mic.len(), read: &read }];
            final_segments(&ctx, &recorded, &live, &mut |_| {}).unwrap().0
        } else {
            let mut blocks = plan_final(&ctx, true, &read, &mut |_| {}).unwrap();
            for b in &mut blocks {
                let pos = b.pos;
                b.windows.retain(|w| spans.iter().any(|s| pos + w[0].start < s.end && pos + w[w.len() - 1].end > s.start));
            }
            let (heard, _) = transcribe_final(&ctx, Channel::Mic, &read, &blocks, &[], &live, &mut || {}).unwrap();
            heard
                .into_iter()
                .map(|h| Segment { start_ms: h.start_ms, end_ms: h.end_ms, channel: Channel::Mic, speaker: ME, text: h.text, language: h.language, fixes: Vec::new() })
                .collect()
        };
        println!("final pass: {} segments in {:.1}s", segments.len(), t.elapsed().as_secs_f32());
        let out = std::env::var("WORDFALL_FINAL_OUT").unwrap();
        std::fs::write(out, serde_json::to_string_pretty(&serde_json::json!({ "segments": segments })).unwrap()).unwrap();
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
        assign_speakers(&mut segments, &embeddings, &MeHints { print: None, levels: &[] }, &mut speakers, true);
        assert_eq!(segments.iter().map(|s| s.speaker).collect::<Vec<_>>(), [0, 1, 1]);
        assert_eq!(speakers.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Me", "Anna"]);
    }
}
