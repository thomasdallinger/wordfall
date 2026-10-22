//! Microphone capture (cpal), mono downmix, level metering and 16 kHz resampling.
//!
//! cpal streams are `!Send` on macOS, so each recording owns a dedicated thread that
//! builds the stream, keeps it alive until told to stop, then drops it.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use serde::Serialize;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

pub const TARGET_RATE: u32 = 16_000;
pub const LEVEL_BANDS: usize = 7;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevice {
    pub name: String,
    pub is_default: bool,
}

pub fn list_input_devices() -> Vec<AudioDevice> {
    let host = cpal::default_host();
    let default = host.default_input_device().and_then(|d| d.name().ok());
    host.input_devices()
        .map(|it| {
            it.filter_map(|d| d.name().ok())
                .map(|name| AudioDevice { is_default: Some(&name) == default.as_ref(), name })
                .collect()
        })
        .unwrap_or_default()
}

struct Shared {
    samples: Mutex<Vec<f32>>,
    /// Rolling window of the most recent samples for metering.
    recent: Mutex<Vec<f32>>,
    gain: f32,
    first_sample: Mutex<Option<Instant>>,
}

pub struct Recording {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    sample_rate: u32,
    pub started: Instant,
    /// When `start` was called (key down); compared with the first audio callback.
    requested: Instant,
    pub device_name: String,
}

pub struct Captured {
    pub samples_16k: Vec<f32>,
    pub duration: Duration,
    pub rms: f32,
    /// Key down → first audio callback; long values mean clipped first syllables.
    pub start_latency_ms: u64,
}

impl Recording {
    /// Opens the input device and starts capturing. Blocks until the stream is running
    /// (or failed) so the caller knows immediately whether the mic is usable.
    pub fn start(preferred: Option<&str>, gain: f32) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared {
            samples: Mutex::new(Vec::with_capacity(48_000 * 30)),
            recent: Mutex::new(Vec::new()),
            gain,
            first_sample: Mutex::new(None),
        });
        let requested = Instant::now();
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<(u32, String)>>();
        let preferred = preferred.map(str::to_string);

        let (sh, st) = (shared.clone(), stop.clone());
        let thread = thread::Builder::new().name("wordfall-audio".into()).spawn(move || {
            let built = build_stream(preferred.as_deref(), sh);
            match built {
                Ok((stream, rate, name)) => {
                    if let Err(e) = stream.play() {
                        let _ = ready_tx.send(Err(e.into()));
                        return;
                    }
                    let _ = ready_tx.send(Ok((rate, name)));
                    while !st.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(10));
                    }
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })?;

        let (sample_rate, device_name) = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow::anyhow!("microphone did not start in time"))??;
        Ok(Self { shared, stop, thread: Some(thread), sample_rate, started: Instant::now(), requested, device_name })
    }

    /// Per-band levels (0..1) of the most recent ~120 ms, for the waveform UI.
    pub fn levels(&self) -> [f32; LEVEL_BANDS] {
        let recent = self.shared.recent.lock();
        let mut out = [0.0; LEVEL_BANDS];
        if recent.is_empty() {
            return out;
        }
        let band = (recent.len() / LEVEL_BANDS).max(1);
        for (i, chunk) in recent.chunks(band).take(LEVEL_BANDS).enumerate() {
            let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32).sqrt();
            // Speech RMS sits roughly at 0.02..0.2; map that onto 0..1 perceptually.
            out[i] = (rms * 9.0).sqrt().min(1.0);
        }
        out
    }

    pub fn finish(mut self) -> Captured {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let duration = self.started.elapsed();
        let raw = std::mem::take(&mut *self.shared.samples.lock());
        let rms = if raw.is_empty() { 0.0 } else { (raw.iter().map(|s| s * s).sum::<f32>() / raw.len() as f32).sqrt() };
        let start_latency_ms = self
            .shared
            .first_sample
            .lock()
            .map(|t| t.duration_since(self.requested).as_millis() as u64)
            .unwrap_or(0);
        Captured { samples_16k: resample(&raw, self.sample_rate, TARGET_RATE), duration, rms, start_latency_ms }
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn pick_device(host: &cpal::Host, preferred: Option<&str>) -> Option<cpal::Device> {
    if let Some(name) = preferred {
        if let Ok(mut devices) = host.input_devices() {
            if let Some(d) = devices.find(|d| d.name().map(|n| n == name).unwrap_or(false)) {
                return Some(d);
            }
        }
        log::warn!("preferred input '{name}' not found, using default");
    }
    host.default_input_device()
}

fn build_stream(preferred: Option<&str>, shared: Arc<Shared>) -> anyhow::Result<(cpal::Stream, u32, String)> {
    let host = cpal::default_host();
    let device = pick_device(&host, preferred).ok_or_else(|| anyhow::anyhow!("no microphone found"))?;
    let name = device.name().unwrap_or_else(|_| "Microphone".into());
    let config = device.default_input_config()?;
    let rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();
    let err_fn = |e| log::error!("audio stream error: {e}");

    let push = move |mono: &mut dyn Iterator<Item = f32>, shared: &Shared| {
        let gain = shared.gain;
        shared.first_sample.lock().get_or_insert_with(Instant::now);
        let mut buf = shared.samples.lock();
        let start = buf.len();
        buf.extend(mono.map(|s| (s * gain).clamp(-1.0, 1.0)));
        let fresh = &buf[start..];
        let mut recent = shared.recent.lock();
        recent.extend_from_slice(fresh);
        let keep = (rate as usize) * 12 / 100;
        if recent.len() > keep {
            let cut = recent.len() - keep;
            recent.drain(..cut);
        }
    };

    macro_rules! stream_for {
        ($t:ty, $conv:expr) => {{
            let sh = shared.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[$t], _| {
                    let mut it = data.chunks(channels).map(|frame| {
                        frame.iter().map(|s| $conv(*s)).sum::<f32>() / channels as f32
                    });
                    push(&mut it, &sh);
                },
                err_fn,
                None,
            )?
        }};
    }

    let stream = match format {
        cpal::SampleFormat::F32 => stream_for!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => stream_for!(i16, |s: i16| s as f32 / i16::MAX as f32),
        cpal::SampleFormat::I32 => stream_for!(i32, |s: i32| s as f32 / i32::MAX as f32),
        cpal::SampleFormat::U16 => stream_for!(u16, |s: u16| (s as f32 - 32768.0) / 32768.0),
        other => anyhow::bail!("unsupported sample format {other:?}"),
    };
    Ok((stream, rate, name))
}

/// Resample with a box low-pass (when downsampling) followed by linear interpolation.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let smoothed: Vec<f32> = if ratio > 1.0 {
        let w = ratio.round().max(1.0) as usize;
        let mut acc = 0.0f32;
        let mut out = Vec::with_capacity(input.len());
        for (i, s) in input.iter().enumerate() {
            acc += s;
            if i >= w {
                acc -= input[i - w];
            }
            out.push(acc / w.min(i + 1) as f32);
        }
        out
    } else {
        input.to_vec()
    };
    let out_len = (input.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let a = smoothed[idx.min(smoothed.len() - 1)];
            let b = smoothed[(idx + 1).min(smoothed.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

/// Reads a 16-bit PCM WAV file into mono f32 samples at 16 kHz (diagnostics only).
pub fn read_wav_16k(path: &std::path::Path) -> anyhow::Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    anyhow::ensure!(bytes.len() > 44 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE", "not a WAV file");
    let (mut channels, mut rate, mut bits) = (1u16, 16_000u32, 16u16);
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into()?) as usize;
        let body = &bytes[pos + 8..(pos + 8 + len).min(bytes.len())];
        if id == b"fmt " {
            channels = u16::from_le_bytes(body[2..4].try_into()?);
            rate = u32::from_le_bytes(body[4..8].try_into()?);
            bits = u16::from_le_bytes(body[14..16].try_into()?);
        } else if id == b"data" {
            anyhow::ensure!(bits == 16, "only 16-bit PCM is supported");
            let mono: Vec<f32> = body
                .chunks_exact(2 * channels as usize)
                .map(|f| {
                    f.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0).sum::<f32>() / channels as f32
                })
                .collect();
            return Ok(resample(&mono, rate, TARGET_RATE));
        }
        pos += 8 + len + (len & 1);
    }
    anyhow::bail!("WAV has no data chunk")
}

/// Writes 16 kHz mono PCM16 (used for saved dictations and Voice Lab clips).
pub fn write_wav_16k(path: &std::path::Path, samples: &[f32]) -> anyhow::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&TARGET_RATE.to_le_bytes());
    out.extend_from_slice(&(TARGET_RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, out)?;
    Ok(())
}
