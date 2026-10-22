//! Cheap energy-based voice activity detection: trims leading/trailing non-speech.

use std::ops::Range;

const RATE: usize = 16_000;
const FRAME: usize = RATE / 50; // 20 ms
const PAD: usize = RATE / 4; // keep 250 ms around speech

/// Returns the sample range containing speech, or `None` if the clip looks speech-free.
pub fn trim(audio: &[f32]) -> Option<Range<usize>> {
    if audio.len() < FRAME {
        return None;
    }
    let rms: Vec<f32> = audio
        .chunks(FRAME)
        .map(|f| (f.iter().map(|s| s * s).sum::<f32>() / f.len() as f32).sqrt())
        .collect();
    let mut sorted = rms.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    // Noise floor = 10th percentile frame; speech must clearly exceed it.
    let floor = sorted[sorted.len() / 10];
    let threshold = (floor * 4.0).max(0.008);
    let voiced: Vec<bool> = rms.iter().map(|&r| r > threshold).collect();

    // Require ~120 ms of voiced frames in a 200 ms window to count as speech
    // (rejects clicks, coughs and key presses).
    let window = 10;
    let needed = 6;
    let n = voiced.len();
    let count = |lo: usize, hi: usize| voiced[lo..hi.min(n)].iter().filter(|v| **v).count();
    // First window that qualifies, then snap to its first voiced frame (and mirror for the end).
    let first_window = (0..n).find(|&i| count(i, i + window) >= needed)?;
    let first = (first_window..n).find(|&j| voiced[j])?;
    let last_window = (0..n).rev().find(|&i| count((i + 1).saturating_sub(window), i + 1) >= needed)?;
    let last = (0..=last_window).rev().find(|&j| voiced[j])?;
    let start = (first * FRAME).saturating_sub(PAD);
    let end = ((last + 1) * FRAME + PAD).min(audio.len());
    (end > start).then_some(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f32, amp: f32) -> Vec<f32> {
        (0..(seconds * RATE as f32) as usize).map(|i| amp * (i as f32 * 0.07).sin()).collect()
    }

    #[test]
    fn silence_is_rejected() {
        assert!(trim(&vec![0.0; RATE * 2]).is_none());
    }

    #[test]
    fn trims_padding_around_speech() {
        let mut clip = vec![0.0; RATE];
        clip.extend(tone(1.0, 0.2));
        clip.extend(vec![0.0; RATE]);
        let r = trim(&clip).unwrap();
        assert!(r.start >= RATE - PAD - FRAME && r.start <= RATE);
        assert!(r.end >= 2 * RATE && r.end <= 2 * RATE + PAD + FRAME);
    }

    #[test]
    fn single_click_is_not_speech() {
        let mut clip = vec![0.0; RATE * 2];
        for s in clip.iter_mut().skip(RATE).take(160) {
            *s = 0.5;
        }
        assert!(trim(&clip).is_none());
    }
}
