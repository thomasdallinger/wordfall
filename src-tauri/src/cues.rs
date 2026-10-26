//! Recording cues: two quick low wooden knocks, rising when recording starts and falling (with a
//! soft bounce) when it stops. Synthesized once per process so macOS and Windows sound the same.

use std::{f32::consts::TAU, sync::LazyLock};

const RATE: u32 = 48_000;
/// Overall level the cues were tuned at.
const VOLUME: f32 = 0.7;
/// Each voice runs this long past its decay before it is cut.
const TAIL: f32 = 0.02;

/// One voice: frequency glides exponentially from `f0` to `f1` over `glide` seconds, the level rises
/// linearly to `gain` over `attack`, then decays exponentially until `dur` (all relative to `t`).
struct Tone {
    f0: f32,
    f1: f32,
    glide: f32,
    t: f32,
    dur: f32,
    gain: f32,
    attack: f32,
    triangle: bool,
}

/// A knock: a body that drops slightly in pitch, a short mallet click and a soft octave.
const fn knock(f: f32, f0: f32, t: f32, dur: f32, gain: f32) -> [Tone; 3] {
    [
        Tone { f0, f1: f, glide: 0.02, t, dur, gain, attack: 0.002, triangle: false },
        Tone { f0: f * 3.98, f1: f * 3.98, glide: 0.0, t, dur: 0.03, gain: 0.08, attack: 0.002, triangle: false },
        Tone { f0: f * 2.0, f1: f * 2.0, glide: 0.0, t, dur: 0.08, gain: 0.08, attack: 0.004, triangle: true },
    ]
}

const G4: f32 = 392.0;
const C5: f32 = 523.3;

fn start_tones() -> Vec<Tone> {
    let mut v = Vec::new();
    v.extend(knock(G4, 482.2, 0.0, 0.15, 0.45));
    v.extend(knock(C5, 643.7, 0.08, 0.22, 0.5));
    v
}

fn stop_tones() -> Vec<Tone> {
    let mut v = Vec::new();
    v.extend(knock(C5, 643.7, 0.0, 0.14, 0.42));
    v.extend(knock(G4, 482.2, 0.08, 0.24, 0.5));
    // The little bounce after the last knock.
    v.push(Tone { f0: 450.8, f1: G4, glide: 0.015, t: 0.15, dur: 0.06, gain: 0.13, attack: 0.002, triangle: false });
    v
}

fn render(tones: &[Tone]) -> Vec<f32> {
    let len = tones.iter().map(|t| t.t + t.dur + TAIL).fold(0.0, f32::max);
    let mut out = vec![0.0f32; (len * RATE as f32).ceil() as usize];
    for tone in tones {
        let first = (tone.t * RATE as f32) as usize;
        let count = ((tone.dur + TAIL) * RATE as f32) as usize;
        let peak = tone.gain * VOLUME;
        let mut phase = 0.0f32;
        for (i, sample) in out.iter_mut().skip(first).take(count).enumerate() {
            let t = i as f32 / RATE as f32;
            let freq = if tone.glide > 0.0 && t < tone.glide {
                tone.f0 * (tone.f1 / tone.f0).powf(t / tone.glide)
            } else {
                tone.f1
            };
            let level = if t < tone.attack {
                peak * t / tone.attack
            } else if t <= tone.dur {
                peak * (1e-4 / peak).powf((t - tone.attack) / (tone.dur - tone.attack))
            } else {
                0.0
            };
            let wave = if tone.triangle { (2.0 / std::f32::consts::PI) * phase.sin().asin() } else { phase.sin() };
            *sample += wave * level;
            phase = (phase + TAU * freq / RATE as f32) % TAU;
        }
    }
    out
}

/// 16-bit mono PCM WAV.
fn encode_wav(samples: &[f32]) -> Vec<u8> {
    let data_len = samples.len() as u32 * 2;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes());
    }
    wav
}

static START: LazyLock<Vec<u8>> = LazyLock::new(|| encode_wav(&render(&start_tones())));
static STOP: LazyLock<Vec<u8>> = LazyLock::new(|| encode_wav(&render(&stop_tones())));

/// The cue as a WAV file in memory; lives for the whole process.
pub fn wav(start: bool) -> &'static [u8] {
    if start {
        &START
    } else {
        &STOP
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_are_short_valid_wavs() {
        for (start, seconds) in [(true, 0.32), (false, 0.34)] {
            let wav = wav(start);
            assert_eq!(&wav[..4], b"RIFF");
            assert_eq!(&wav[8..16], b"WAVEfmt ");
            let samples = (wav.len() - 44) / 2;
            assert!((samples as f32 / RATE as f32 - seconds).abs() < 0.001, "{samples} samples");
        }
    }

    #[test]
    fn cues_are_audible_but_never_clip() {
        for tones in [start_tones(), stop_tones()] {
            let peak = render(&tones).iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak > 0.2 && peak < 0.95, "peak {peak}");
        }
    }

    #[test]
    fn start_rises_and_stop_falls() {
        // The second knock decides the direction: higher than the first to start, lower to stop.
        let second = |tones: &[Tone]| tones.iter().find(|t| t.t > 0.0 && t.dur > 0.1).map(|t| t.f1).unwrap();
        assert!(second(&start_tones()) > start_tones()[0].f1);
        assert!(second(&stop_tones()) < stop_tones()[0].f1);
    }
}
