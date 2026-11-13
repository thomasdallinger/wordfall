//! Telling speakers apart in meeting transcripts: a voice embedding per transcript segment
//! (3D-Speaker CAM++, run on the CPU through tract, ~40 ms per 2 s of speech), then clustering.
//!
//! Each audio channel is clustered on its own. The microphone's biggest voice is the user ("Me");
//! other voices on it are people in the room. Voices in system audio are the other participants.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tract_onnx::prelude::*;

use crate::fbank::{Fbank, NUM_BINS};

pub const MODEL_FILE: &str = "3dspeaker_speech_campplus_sv_en_voxceleb_16k.onnx";
const MODEL_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_en_voxceleb_16k.onnx";
const MODEL_BYTES: u64 = 29_596_978;

/// Frames per model run (2 s); longer speech is averaged over overlapping windows.
const WINDOW: usize = 200;
/// Shorter speech gives unreliable embeddings; those segments take a neighbour's speaker.
const MIN_SAMPLES: usize = 12_800;
/// Live: cosine similarity above which a segment (or a group of segments) is a known voice.
pub const SAME_VOICE: f32 = 0.55;
/// Final: cosine similarity of two voices' centroids above which they are one voice. Higher than
/// `SAME_VOICE`: a centroid averages away what tells one segment from another, but not the colouring
/// of the channel everyone is heard through (a call's codec, a room's microphone). Two people on one
/// call came out at 0.59, two in one room at 0.55–0.6, while the first and second half of one
/// person's talk score about 0.9.
const SAME_VOICE_CENTROIDS: f32 = 0.64;
/// A first pass merges near-identical segments so the full clustering stays small.
const PRE_MERGE: f32 = 0.75;
/// A voice needs this much speech to stand on its own...
const MIN_VOICE_MS: u64 = 20_000;
/// ...or this share of the channel's speech in a long recording...
const MIN_VOICE_SHARE: u64 = 50; // 1/50 = 2 %
/// ...but never more than this share in a short one, where 20 s can be a real person's part.
const SHORT_SHARE: u64 = 10; // 1/10 = 10 %
/// A voice heard in a single segment isn't one (a laugh, two people at once).
const MIN_VOICE_SEGMENTS: usize = 2;

/// Downloads the speaker model (30 MB) into `dir` if it isn't there yet.
pub async fn ensure_model(dir: &Path) -> anyhow::Result<PathBuf> {
    let path = dir.join(MODEL_FILE);
    if crate::transcribe::is_complete(&path, MODEL_BYTES) {
        return Ok(path);
    }
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    crate::transcribe::download_file(MODEL_URL, MODEL_FILE, MODEL_BYTES, dir, cancel, |_, _| {}).await
}

pub fn model_path(dir: &Path) -> Option<PathBuf> {
    let path = dir.join(MODEL_FILE);
    crate::transcribe::is_complete(&path, MODEL_BYTES).then_some(path)
}

pub struct Embedder {
    plan: std::sync::Arc<TypedRunnableModel>,
    fbank: Fbank,
}

impl Embedder {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let plan = tract_onnx::onnx()
            .model_for_path(path)?
            .with_input_fact(0, f32::fact([1, WINDOW, NUM_BINS]).into())?
            .into_optimized()?
            .into_runnable()?;
        Ok(Self { plan, fbank: Fbank::default() })
    }

    /// Unit-length voice embedding of 16 kHz `audio`, or `None` when it's too short to tell.
    pub fn embed(&self, audio: &[f32]) -> anyhow::Result<Option<Vec<f32>>> {
        if audio.len() < MIN_SAMPLES {
            return Ok(None);
        }
        // The model was trained on 16-bit sample values.
        let scaled: Vec<f32> = audio.iter().map(|s| s * 32768.0).collect();
        let feats = self.fbank.compute(&scaled);
        if feats.is_empty() {
            return Ok(None);
        }
        // Per-utterance mean normalisation, as in training.
        let mut mean = [0.0f32; NUM_BINS];
        for row in &feats {
            for (m, v) in mean.iter_mut().zip(row) {
                *m += v;
            }
        }
        mean.iter_mut().for_each(|m| *m /= feats.len() as f32);
        let mut sum: Vec<f32> = Vec::new();
        let mut start = 0;
        loop {
            // Short speech wraps around to fill the window.
            let mut data = Vec::with_capacity(WINDOW * NUM_BINS);
            for i in 0..WINDOW {
                let row = &feats[(start + i) % feats.len()];
                data.extend(row.iter().zip(&mean).map(|(v, m)| v - m));
            }
            let out = self.plan.run(tvec!(Tensor::from_shape(&[1, WINDOW, NUM_BINS], &data)?.into()))?;
            let v = out[0].to_plain_array_view::<f32>()?;
            let v: Vec<f32> = v.iter().copied().collect();
            if sum.is_empty() {
                sum = vec![0.0; v.len()];
            }
            let n = norm(&v);
            for (s, x) in sum.iter_mut().zip(&v) {
                *s += x / n;
            }
            start += WINDOW / 2;
            if start + WINDOW > feats.len() {
                break;
            }
        }
        let n = norm(&sum);
        Ok(Some(sum.into_iter().map(|x| x / n).collect()))
    }
}

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9)
}

pub fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() / (norm(a) * norm(b))
}

/// One segment's input to clustering.
pub struct Item<'a> {
    pub embedding: Option<&'a [f32]>,
    pub start_ms: u64,
    pub end_ms: u64,
}

struct Cluster {
    /// Duration-weighted sum of member embeddings.
    sum: Vec<f32>,
    members: Vec<usize>,
    ms: u64,
}

/// Groups the segments of one channel by voice. Returns a cluster index per item, numbered by
/// first appearance (0 = whoever spoke first), and the number of clusters.
pub fn cluster(items: &[Item]) -> (Vec<usize>, usize) {
    let mut clusters: Vec<Cluster> = Vec::new();
    // Pass 1: greedy, in time order, against cluster centroids.
    for (i, it) in items.iter().enumerate() {
        let Some(e) = it.embedding else { continue };
        let ms = it.end_ms.saturating_sub(it.start_ms).max(1);
        let best = clusters
            .iter()
            .enumerate()
            .map(|(c, cl)| (c, cosine_sim(&cl.sum, e)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((c, sim)) if sim >= PRE_MERGE => {
                let cl = &mut clusters[c];
                cl.sum.iter_mut().zip(e).for_each(|(s, x)| *s += x * ms as f32);
                cl.members.push(i);
                cl.ms += ms;
            }
            _ => clusters.push(Cluster { sum: e.iter().map(|x| x * ms as f32).collect(), members: vec![i], ms }),
        }
    }
    // Pass 2: agglomerative on centroids until no two groups sound alike.
    loop {
        let mut best = (0, 0, f32::MIN);
        for a in 0..clusters.len() {
            for b in a + 1..clusters.len() {
                let sim = cosine_sim(&clusters[a].sum, &clusters[b].sum);
                if sim > best.2 {
                    best = (a, b, sim);
                }
            }
        }
        if best.2 < SAME_VOICE_CENTROIDS {
            break;
        }
        merge(&mut clusters, best.0, best.1);
    }
    // Pass 3: voices with little speech are odd segments (laughter, two people at once, someone
    // far away) rather than people; each of their segments goes to the voice it sounds most like.
    dissolve_minor(&mut clusters, items);

    let mut label: Vec<Option<usize>> = vec![None; items.len()];
    for (c, cl) in clusters.iter().enumerate() {
        for &m in &cl.members {
            label[m] = Some(c);
        }
    }
    // Segments too short to embed take the nearest labelled neighbour in time.
    let labelled: Vec<usize> = (0..items.len()).filter(|&i| label[i].is_some()).collect();
    let mut out: Vec<usize> = (0..items.len())
        .map(|i| {
            label[i].unwrap_or_else(|| {
                let mid = (items[i].start_ms + items[i].end_ms) / 2;
                labelled
                    .iter()
                    .min_by_key(|&&j| ((items[j].start_ms + items[j].end_ms) / 2).abs_diff(mid))
                    .and_then(|&j| label[j])
                    .unwrap_or(0)
            })
        })
        .collect();
    // Number by first appearance.
    let mut order: Vec<usize> = Vec::new();
    for &l in &out {
        if !order.contains(&l) {
            order.push(l);
        }
    }
    for l in out.iter_mut() {
        *l = order.iter().position(|o| o == l).unwrap_or(0);
    }
    (out, order.len())
}

/// Least speech a voice needs to stay a voice, given the channel's `total` speech.
fn min_voice_ms(total: u64) -> u64 {
    (total / MIN_VOICE_SHARE).max(MIN_VOICE_MS.min(total / SHORT_SHARE))
}

/// Dissolves the clusters with too little speech (the biggest always stays) and moves each of
/// their segments to the most similar remaining voice, one at a time: an odd cluster often
/// gathers bits of several people.
fn dissolve_minor(clusters: &mut Vec<Cluster>, items: &[Item]) {
    let total: u64 = clusters.iter().map(|c| c.ms).sum();
    let min_ms = min_voice_ms(total);
    let Some(biggest) = (0..clusters.len()).max_by_key(|&c| clusters[c].ms) else { return };
    let keep: Vec<bool> = clusters
        .iter()
        .enumerate()
        .map(|(c, cl)| c == biggest || (cl.ms >= min_ms && cl.members.len() >= MIN_VOICE_SEGMENTS))
        .collect();
    let mut kept: Vec<Cluster> = Vec::new();
    let mut gone: Vec<Cluster> = Vec::new();
    for (c, cl) in std::mem::take(clusters).into_iter().enumerate() {
        if keep[c] {
            kept.push(cl);
        } else {
            gone.push(cl);
        }
    }
    // Targets against the voices as they were, so the order of the moves doesn't matter.
    let moves: Vec<(usize, usize)> = gone
        .iter()
        .flat_map(|cl| cl.members.iter().copied())
        .map(|m| {
            let e = items[m].embedding.expect("clustered segments have embeddings");
            let target = (0..kept.len()).max_by(|&a, &b| cosine_sim(&kept[a].sum, e).total_cmp(&cosine_sim(&kept[b].sum, e))).unwrap_or(0);
            (m, target)
        })
        .collect();
    for (m, t) in moves {
        let it = &items[m];
        let ms = it.end_ms.saturating_sub(it.start_ms).max(1);
        let cl = &mut kept[t];
        cl.sum.iter_mut().zip(it.embedding.unwrap_or_default()).for_each(|(s, x)| *s += x * ms as f32);
        cl.members.push(m);
        cl.ms += ms;
    }
    *clusters = kept;
}

/// Merges cluster `b` into `a` (`a < b`).
fn merge(clusters: &mut Vec<Cluster>, a: usize, b: usize) {
    let other = clusters.remove(b);
    let cl = &mut clusters[a];
    cl.sum.iter_mut().zip(&other.sum).for_each(|(s, x)| *s += x);
    cl.members.extend(other.members);
    cl.ms += other.ms;
}

/// Where the voice print is kept, in the app's data folder.
pub const VOICE_FILE: &str = "voice.json";
/// Speech a dictation needs (after silence is cut) before it teaches the voice print.
pub const PRINT_MIN_SAMPLES: usize = 3 * 16_000;
/// After this many dictations the print is good; from then on it learns at most...
const PRINT_SETTLED: u32 = 30;
/// ...once in this long (seconds), enough to follow a new microphone.
const PRINT_EVERY_S: u64 = 3_600;
/// Dictations the running mean remembers at full weight.
const PRINT_MEMORY: u32 = 100;

/// The user's voice, learnt from their dictations, to find them among a meeting's voices. Only
/// the average embedding is kept, never audio.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoicePrint {
    /// Speaker model the embedding comes from; a print from another model means nothing.
    pub model: String,
    pub embedding: Vec<f32>,
    /// Dictations it was learnt from.
    pub count: u32,
    /// When it last learnt (Unix seconds).
    pub updated: u64,
}

impl VoicePrint {
    pub fn load(dir: &Path) -> Option<Self> {
        crate::storage::read_json::<Self>(&dir.join(VOICE_FILE)).filter(|p| p.model == MODEL_FILE && !p.embedding.is_empty())
    }

    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        crate::storage::write_json(&dir.join(VOICE_FILE), self)
    }

    /// Whether the next dictation should teach the print (it's cheap, but not free).
    pub fn wants(print: Option<&Self>, now: u64) -> bool {
        print.is_none_or(|p| p.count < PRINT_SETTLED || now >= p.updated + PRINT_EVERY_S)
    }

    /// `print` with one more dictation's embedding averaged in.
    pub fn learn(print: Option<Self>, embedding: &[f32], now: u64) -> Self {
        match print.filter(|p| p.embedding.len() == embedding.len()) {
            Some(p) => {
                let n = p.count.min(PRINT_MEMORY) as f32;
                let mean = p.embedding.iter().zip(embedding).map(|(m, x)| (m * n + x) / (n + 1.0)).collect();
                Self { embedding: mean, count: p.count + 1, updated: now, ..p }
            }
            None => Self { model: MODEL_FILE.into(), embedding: embedding.to_vec(), count: 1, updated: now },
        }
    }
}

fn now_s() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Whether a dictation now could teach the voice print in `data_dir`: it wants to learn (see
/// `VoicePrint::wants`) and the speaker model in `model_dir` is there. Cheap: check it before
/// finding the dictation's speech.
pub fn print_wants_voice(data_dir: &Path, model_dir: &Path) -> bool {
    VoicePrint::wants(VoicePrint::load(data_dir).as_ref(), now_s()) && model_path(model_dir).is_some()
}

/// Deletes the voice print in `data_dir` (nothing to do if there is none).
pub fn forget_voice(data_dir: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(data_dir.join(VOICE_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Teaches the voice print in `data_dir` a dictation's `speech` (silence already cut out), if
/// there's enough of it and the print wants to learn. Returns whether it learnt. Slow-ish (loads
/// the speaker model): call it after the dictation's text is delivered, off the hot path.
pub fn learn_voice(data_dir: &Path, model_dir: &Path, speech: &[f32]) -> anyhow::Result<bool> {
    let now = now_s();
    let print = VoicePrint::load(data_dir);
    if speech.len() < PRINT_MIN_SAMPLES || !VoicePrint::wants(print.as_ref(), now) {
        return Ok(false);
    }
    let Some(model) = model_path(model_dir) else { return Ok(false) };
    let Some(e) = Embedder::load(&model)?.embed(speech)? else { return Ok(false) };
    VoicePrint::learn(print, &e, now).save(data_dir)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(seed: usize) -> Vec<f32> {
        (0..16).map(|i| if i % 4 == seed { 1.0 } else { 0.1 }).collect()
    }

    fn noisy(v: &[f32], k: usize) -> Vec<f32> {
        v.iter().enumerate().map(|(i, x)| x + if (i + k) % 5 == 0 { 0.15 } else { 0.0 }).collect()
    }

    #[test]
    fn separates_two_voices_and_numbers_by_appearance() {
        let (a, b) = (voice(1), voice(2));
        let embs: Vec<Vec<f32>> = vec![noisy(&b, 0), noisy(&a, 1), noisy(&b, 2), noisy(&a, 3), noisy(&b, 4)];
        let items: Vec<Item> = embs
            .iter()
            .enumerate()
            .map(|(i, e)| Item { embedding: Some(e), start_ms: i as u64 * 5000, end_ms: i as u64 * 5000 + 4500 })
            .collect();
        let (labels, n) = cluster(&items);
        assert_eq!(n, 2);
        assert_eq!(labels, [0, 1, 0, 1, 0]);
    }

    #[test]
    fn short_segments_take_their_neighbours_voice() {
        let a = voice(1);
        let b = voice(3);
        let items = vec![
            Item { embedding: Some(&a), start_ms: 0, end_ms: 5000 },
            Item { embedding: None, start_ms: 5200, end_ms: 5600 },
            Item { embedding: Some(&a), start_ms: 6_000, end_ms: 11_000 },
            Item { embedding: Some(&b), start_ms: 20_000, end_ms: 26_000 },
            Item { embedding: Some(&b), start_ms: 27_000, end_ms: 33_000 },
        ];
        let (labels, n) = cluster(&items);
        assert_eq!(n, 2);
        assert_eq!(labels, [0, 0, 0, 1, 1]);
    }

    fn unit(dims: &[(usize, f32)]) -> Vec<f32> {
        let mut v = vec![0.0; 16];
        for &(i, x) in dims {
            v[i] = x;
        }
        v
    }

    #[test]
    fn voices_with_little_talk_are_dissolved_segment_by_segment() {
        // Two people talk for minutes; a third "voice" is two odd segments (overlap, laughter)
        // that sound alike but each leans towards a different real voice.
        let (a, b) = (unit(&[(0, 1.0)]), unit(&[(1, 1.0)]));
        let (odd_a, odd_b) = (unit(&[(2, 1.0), (0, 0.5)]), unit(&[(2, 1.0), (1, 0.5)]));
        let mut embs: Vec<&Vec<f32>> = Vec::new();
        for i in 0..20 {
            embs.push(if i % 2 == 0 { &a } else { &b });
        }
        embs.insert(7, &odd_a);
        embs.insert(15, &odd_b);
        let items: Vec<Item> = embs
            .iter()
            .enumerate()
            .map(|(i, e)| Item { embedding: Some(e), start_ms: i as u64 * 10_000, end_ms: i as u64 * 10_000 + 9_000 })
            .collect();
        let (labels, n) = cluster(&items);
        assert_eq!(n, 2);
        assert_eq!(labels[7], labels[0], "leans towards the first voice");
        assert_eq!(labels[15], labels[1], "leans towards the second voice");
    }

    #[test]
    fn a_short_recording_keeps_a_second_voice_with_a_fair_share() {
        // 50 s of talk: 14 s of a second person is little in absolute terms but a real voice.
        let (a, b) = (voice(0), voice(2));
        let mut items: Vec<Item> = (0..6).map(|i| Item { embedding: Some(&a), start_ms: i * 7_000, end_ms: i * 7_000 + 6_000 }).collect();
        items.extend((0..2).map(|i| Item { embedding: Some(&b), start_ms: 50_000 + i * 8_000, end_ms: 50_000 + i * 8_000 + 7_000 }));
        assert_eq!(cluster(&items).1, 2);
    }

    #[test]
    fn min_talk_grows_with_the_recording() {
        assert_eq!(min_voice_ms(30_000), 3_000);
        assert_eq!(min_voice_ms(600_000), 20_000);
        assert_eq!(min_voice_ms(3_000_000), 60_000);
    }

    #[test]
    fn voice_print_is_a_running_mean_of_dictations() {
        let p = VoicePrint::learn(None, &[1.0, 0.0], 100);
        assert_eq!((p.count, p.updated), (1, 100));
        let p = VoicePrint::learn(Some(p), &[0.0, 1.0], 200);
        assert_eq!(p.count, 2);
        assert!((p.embedding[0] - 0.5).abs() < 1e-6 && (p.embedding[1] - 0.5).abs() < 1e-6);
        // A print of another size (another model) starts over.
        let p = VoicePrint::learn(Some(p), &[0.0, 0.0, 1.0], 300);
        assert_eq!((p.count, p.embedding.len()), (1, 3));
    }

    #[test]
    fn voice_print_learns_less_often_once_settled() {
        assert!(VoicePrint::wants(None, 0));
        let mut p = VoicePrint::learn(None, &[1.0], 1_000);
        assert!(VoicePrint::wants(Some(&p), 1_001));
        p.count = PRINT_SETTLED;
        assert!(!VoicePrint::wants(Some(&p), 1_001));
        assert!(VoicePrint::wants(Some(&p), 1_000 + PRINT_EVERY_S));
    }

    #[test]
    fn voice_print_of_another_model_is_ignored() {
        let dir = std::env::temp_dir().join(format!("wordfall-voice-{}", std::process::id()));
        let mut p = VoicePrint::learn(None, &[0.6, 0.8], 1);
        p.save(&dir).unwrap();
        assert_eq!(VoicePrint::load(&dir).map(|p| p.count), Some(1));
        p.model = "other.onnx".into();
        p.save(&dir).unwrap();
        assert!(VoicePrint::load(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_settled_print_or_no_speaker_model_wants_no_voice() {
        let dir = std::env::temp_dir().join(format!("wordfall-voice-wants-{}", std::process::id()));
        let models = dir.join("models");
        std::fs::create_dir_all(&models).unwrap();
        assert!(!print_wants_voice(&dir, &models), "no speaker model: nothing to learn with");
        // A model file of the right size counts as downloaded.
        std::fs::File::create(models.join(MODEL_FILE)).unwrap().set_len(MODEL_BYTES).unwrap();
        assert!(print_wants_voice(&dir, &models), "no print yet");
        let mut p = VoicePrint::learn(None, &[0.6, 0.8], now_s());
        p.count = PRINT_SETTLED;
        p.save(&dir).unwrap();
        assert!(!print_wants_voice(&dir, &models), "settled and learnt just now");
        forget_voice(&dir).unwrap();
        assert!(!dir.join(VOICE_FILE).exists());
        forget_voice(&dir).unwrap();
        assert!(print_wants_voice(&dir, &models), "forgotten: learns again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_people_heard_through_one_channel_stay_apart() {
        // Both voices share the call's colouring (axis 2): their centroids score 0.6, as a
        // presenter and a visitor on one call did; each person's segments vary a little.
        let (a, b) = (unit(&[(0, 1.0), (2, 1.2247)]), unit(&[(1, 1.0), (2, 1.2247)]));
        assert!((cosine_sim(&a, &b) - 0.6).abs() < 0.01);
        let embs: Vec<Vec<f32>> = (0..24).map(|i| noisy(if i % 3 == 2 { &b } else { &a }, i)).collect();
        let items: Vec<Item> = embs
            .iter()
            .enumerate()
            .map(|(i, e)| Item { embedding: Some(e), start_ms: i as u64 * 10_000, end_ms: i as u64 * 10_000 + 9_000 })
            .collect();
        let (labels, n) = cluster(&items);
        assert_eq!(n, 2);
        assert!(labels.iter().enumerate().all(|(i, &l)| l == usize::from(i % 3 == 2)));
    }

    #[test]
    fn tiny_voices_are_folded_away() {
        let (a, b) = (voice(0), voice(2));
        let items = vec![
            Item { embedding: Some(&a), start_ms: 0, end_ms: 30_000 },
            Item { embedding: Some(&b), start_ms: 31_000, end_ms: 32_000 },
        ];
        assert_eq!(cluster(&items).1, 1);
    }
}
