//! Telling speakers apart in meeting transcripts: a voice embedding per transcript segment
//! (3D-Speaker CAM++, run on the CPU through tract, ~40 ms per 2 s of speech), then clustering.
//!
//! Each audio channel is clustered on its own. The microphone's biggest voice is the user ("Me");
//! other voices on it are people in the room. Voices in system audio are the other participants.

use std::path::{Path, PathBuf};

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
/// Average cosine similarity above which two groups of segments are the same voice.
const SAME_VOICE: f32 = 0.55;
/// A first pass merges near-identical segments so the full clustering stays small.
const PRE_MERGE: f32 = 0.75;
/// Voices with less speech than this are folded into the closest other voice (coughs, noise).
const MIN_VOICE_MS: u64 = 4_000;

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
        if best.2 < SAME_VOICE {
            break;
        }
        merge(&mut clusters, best.0, best.1);
    }
    // Pass 3: fold voices with very little speech into the closest one.
    while clusters.len() > 1 {
        let Some((small, _)) = clusters.iter().enumerate().filter(|(_, c)| c.ms < MIN_VOICE_MS).min_by_key(|(_, c)| c.ms) else {
            break;
        };
        let target = (0..clusters.len())
            .filter(|&c| c != small)
            .max_by(|&a, &b| cosine_sim(&clusters[small].sum, &clusters[a].sum).total_cmp(&cosine_sim(&clusters[small].sum, &clusters[b].sum)))
            .expect("more than one cluster");
        let (a, b) = (small.min(target), small.max(target));
        merge(&mut clusters, a, b);
    }

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

/// Merges cluster `b` into `a` (`a < b`).
fn merge(clusters: &mut Vec<Cluster>, a: usize, b: usize) {
    let other = clusters.remove(b);
    let cl = &mut clusters[a];
    cl.sum.iter_mut().zip(&other.sum).for_each(|(s, x)| *s += x);
    cl.members.extend(other.members);
    cl.ms += other.ms;
}

/// Index of the cluster with the most speech (the microphone's owner).
pub fn dominant(labels: &[usize], items: &[Item], n: usize) -> usize {
    let mut ms = vec![0u64; n.max(1)];
    for (l, it) in labels.iter().zip(items) {
        ms[*l] += it.end_ms.saturating_sub(it.start_ms);
    }
    (0..ms.len()).max_by_key(|&c| ms[c]).unwrap_or(0)
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
            Item { embedding: Some(&b), start_ms: 20_000, end_ms: 26_000 },
        ];
        let (labels, n) = cluster(&items);
        assert_eq!(n, 2);
        assert_eq!(labels, [0, 0, 1]);
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
