//! Kaldi-compatible log mel filterbank (the features WeSpeaker / 3D-Speaker models are trained on):
//! 25 ms Povey windows every 10 ms, pre-emphasis 0.97, DC removal, 512-point FFT, 80 mel bins
//! from 20 Hz to Nyquist, no dither, `snip_edges`.

use std::f32::consts::PI;

pub const NUM_BINS: usize = 80;
const FRAME_LEN: usize = 400;
const FRAME_SHIFT: usize = 160;
const FFT: usize = 512;
const RATE: f32 = 16_000.0;

pub struct Fbank {
    window: Vec<f32>,
    /// Per mel bin: first FFT bin and its weights.
    filters: Vec<(usize, Vec<f32>)>,
    twiddles: Vec<(f32, f32)>,
}

fn mel(f: f32) -> f32 {
    1127.0 * (1.0 + f / 700.0).ln()
}

impl Default for Fbank {
    fn default() -> Self {
        let window = (0..FRAME_LEN)
            .map(|i| (0.5 - 0.5 * (2.0 * PI * i as f32 / (FRAME_LEN - 1) as f32).cos()).powf(0.85))
            .collect();
        let (low, high) = (mel(20.0), mel(RATE / 2.0));
        let delta = (high - low) / (NUM_BINS + 1) as f32;
        let filters = (0..NUM_BINS)
            .map(|b| {
                let (left, center, right) =
                    (low + b as f32 * delta, low + (b + 1) as f32 * delta, low + (b + 2) as f32 * delta);
                let mut first = None;
                let mut weights = Vec::new();
                for i in 0..FFT / 2 {
                    let m = mel(RATE * i as f32 / FFT as f32);
                    if m > left && m < right {
                        first.get_or_insert(i);
                        weights.push(if m <= center { (m - left) / (center - left) } else { (right - m) / (right - center) });
                    } else if first.is_some() {
                        break;
                    }
                }
                (first.unwrap_or(0), weights)
            })
            .collect();
        let twiddles = (0..FFT / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f32 / FFT as f32;
                (a.cos(), a.sin())
            })
            .collect();
        Self { window, filters, twiddles }
    }
}

impl Fbank {
    /// Features for `samples` (16 kHz, already scaled as the model expects), one row of 80 per frame.
    pub fn compute(&self, samples: &[f32]) -> Vec<[f32; NUM_BINS]> {
        if samples.len() < FRAME_LEN {
            return Vec::new();
        }
        let frames = 1 + (samples.len() - FRAME_LEN) / FRAME_SHIFT;
        let mut out = Vec::with_capacity(frames);
        let mut re = vec![0.0f32; FFT];
        let mut im = vec![0.0f32; FFT];
        let mut power = vec![0.0f32; FFT / 2];
        for f in 0..frames {
            let frame = &samples[f * FRAME_SHIFT..f * FRAME_SHIFT + FRAME_LEN];
            let mean = frame.iter().sum::<f32>() / FRAME_LEN as f32;
            re.fill(0.0);
            im.fill(0.0);
            // Pre-emphasis (the first sample is its own predecessor, as in Kaldi), then the window.
            let mut prev = frame[0] - mean;
            for i in 0..FRAME_LEN {
                let x = frame[i] - mean;
                re[i] = (x - 0.97 * prev) * self.window[i];
                prev = x;
            }
            self.fft(&mut re, &mut im);
            for i in 0..FFT / 2 {
                power[i] = re[i] * re[i] + im[i] * im[i];
            }
            let mut row = [0.0f32; NUM_BINS];
            for (b, (first, w)) in self.filters.iter().enumerate() {
                let e: f32 = w.iter().zip(&power[*first..]).map(|(a, p)| a * p).sum();
                row[b] = e.max(f32::EPSILON).ln();
            }
            out.push(row);
        }
        out
    }

    /// In-place iterative radix-2 FFT.
    fn fft(&self, re: &mut [f32], im: &mut [f32]) {
        let n = re.len();
        let mut j = 0;
        for i in 1..n {
            let mut bit = n >> 1;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j |= bit;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let (wr, wi) = self.twiddles[k * step];
                    let (a, b) = (start + k, start + k + len / 2);
                    let (xr, xi) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                    re[b] = re[a] - xr;
                    im[b] = im[a] - xi;
                    re[a] += xr;
                    im[a] += xi;
                }
            }
            len <<= 1;
        }
    }
}
