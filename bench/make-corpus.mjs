#!/usr/bin/env node
// Generates the synthetic benchmark corpora (smoke + noise) with macOS `say`.
//
//   node bench/make-corpus.mjs [--out <bench root>]
//
// Default root: ~/Library/Application Support/com.wordfall.app/bench
// Layout: <root>/<tier>/<id>.wav (16 kHz mono PCM16) + <id>.json { reference, tier, tags, terms, voice }

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const RATE = 16000;
const args = process.argv.slice(2);
const outIdx = args.indexOf('--out');
const root =
  outIdx >= 0 ? args[outIdx + 1] : path.join(os.homedir(), 'Library/Application Support/com.wordfall.app/bench');
const here = path.dirname(new URL(import.meta.url).pathname);
const spec = JSON.parse(fs.readFileSync(path.join(here, 'smoke-sentences.json'), 'utf8'));

// Deterministic PRNG so regenerated corpora are byte-identical.
let seed = 0x5eed;
const rand = () => ((seed = (seed * 1664525 + 1013904223) >>> 0) / 2 ** 32);
const gauss = () => Math.sqrt(-2 * Math.log(rand() + 1e-12)) * Math.cos(2 * Math.PI * rand());

function readWav(file) {
  const b = fs.readFileSync(file);
  let pos = 12;
  while (pos < b.length) {
    const id = b.toString('ascii', pos, pos + 4);
    const len = b.readUInt32LE(pos + 4);
    if (id === 'data') {
      const out = new Float32Array(len / 2);
      for (let i = 0; i < out.length; i++) out[i] = b.readInt16LE(pos + 8 + i * 2) / 32768;
      return out;
    }
    pos += 8 + len + (len & 1);
  }
  throw new Error(`no data chunk in ${file}`);
}

function writeWav(file, samples) {
  const data = Buffer.alloc(samples.length * 2);
  for (let i = 0; i < samples.length; i++) {
    data.writeInt16LE(Math.max(-32768, Math.min(32767, Math.round(samples[i] * 32767))), i * 2);
  }
  const h = Buffer.alloc(44);
  h.write('RIFF', 0); h.writeUInt32LE(36 + data.length, 4); h.write('WAVE', 8);
  h.write('fmt ', 12); h.writeUInt32LE(16, 16); h.writeUInt16LE(1, 20); h.writeUInt16LE(1, 22);
  h.writeUInt32LE(RATE, 24); h.writeUInt32LE(RATE * 2, 28); h.writeUInt16LE(2, 32); h.writeUInt16LE(16, 34);
  h.write('data', 36); h.writeUInt32LE(data.length, 40);
  fs.writeFileSync(file, Buffer.concat([h, data]));
}

/** Pads with the silence a real push-to-talk clip has around speech, plus faint room noise. */
function realistic(speech) {
  const lead = Math.round(RATE * (0.25 + rand() * 0.35));
  const tail = Math.round(RATE * (0.3 + rand() * 0.5));
  const out = new Float32Array(lead + speech.length + tail);
  out.set(speech, lead);
  for (let i = 0; i < out.length; i++) out[i] = out[i] * 0.8 + gauss() * 0.0015;
  return out;
}

function writeClip(tier, id, samples, meta) {
  const dir = path.join(root, tier);
  fs.mkdirSync(dir, { recursive: true });
  writeWav(path.join(dir, `${id}.wav`), samples);
  fs.writeFileSync(path.join(dir, `${id}.json`), JSON.stringify({ tier, ...meta }, null, 2));
}

// ---- smoke: sentences × voices ----
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'wordfall-corpus-'));
let n = 0;
for (const voice of spec.voices) {
  for (const rate of spec.rates) {
    for (const s of spec.sentences) {
      const raw = path.join(tmp, 'say.wav');
      execFileSync('say', ['-v', voice.name, '-r', String(rate), `--data-format=LEI16@${RATE}`, '-o', raw, s.text]);
      const id = `${s.id}--${voice.name.toLowerCase()}`;
      writeClip('smoke', id, realistic(readWav(raw)), {
        reference: s.text,
        tags: s.tags ?? [],
        terms: s.terms ?? [],
        voice: voice.name,
        accent: voice.accent,
        source: 'tts',
      });
      n++;
    }
  }
}
fs.rmSync(tmp, { recursive: true, force: true });

// ---- noise: inputs that must produce no text ----
const len = (s) => Math.round(RATE * s);
const noise = {
  'silence-1s': () => new Float32Array(len(1)),
  'silence-4s': () => new Float32Array(len(4)),
  'room-tone-3s': () => Float32Array.from({ length: len(3) }, () => gauss() * 0.003),
  'mic-hiss-2s': () => Float32Array.from({ length: len(2) }, () => gauss() * 0.01),
  'hum-50hz-3s': () => Float32Array.from({ length: len(3) }, (_, i) => 0.02 * Math.sin((2 * Math.PI * 50 * i) / RATE) + gauss() * 0.002),
  'keyboard-4s': () => {
    const out = Float32Array.from({ length: len(4) }, () => gauss() * 0.002);
    for (let t = 0.2; t < 3.8; t += 0.08 + rand() * 0.25) {
      const at = len(t);
      for (let k = 0; k < 400; k++) out[at + k] += gauss() * 0.25 * Math.exp(-k / 60);
    }
    return out;
  },
  'breath-3s': () => {
    const out = new Float32Array(len(3));
    let lp = 0;
    for (let i = 0; i < out.length; i++) {
      lp = lp * 0.97 + gauss() * 0.03;
      const env = Math.max(0, Math.sin((Math.PI * i) / len(1.5)));
      out[i] = lp * env * 0.6;
    }
    return out;
  },
  'chord-3s': () =>
    Float32Array.from({ length: len(3) }, (_, i) =>
      [220, 277.18, 329.63].reduce((a, f) => a + 0.05 * Math.sin((2 * Math.PI * f * i) / RATE), 0),
    ),
};
for (const [id, make] of Object.entries(noise)) {
  writeClip('noise', id, make(), { reference: '', tags: ['noise'], terms: [], source: 'synthetic' });
}

console.log(`wrote ${n} smoke clips and ${Object.keys(noise).length} noise clips to ${root}`);
