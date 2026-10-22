# Benchmark results (2026-10-20)

M3 Pro, 36 GB, quiet machine, `--runs 3` (median latency per clip). Corpus: 104 TTS clips
(26 scripts × 4 voices: Samantha en-US, Daniel en-GB, Rishi en-IN, Anna = German voice reading
English) + 8 noise clips. Latency = engine time for a finished clip (release → text, excluding paste).
Full reports: `~/Library/Application Support/com.wordfall.app/bench/results/`.

**Note:** these runs predate a rewording of a few corpus sentences (project names were replaced
with invented ones), so re-running `make-corpus.mjs` gives slightly different absolute numbers.

**Caveat:** TTS isn't real speech; "Anna" only approximates a German accent. Re-confirm with
Voice Lab + verified dictations (`--tier lab --tier history`) before big decisions.

## Step 2: cheap wins (distil-large-v3.5)

| Change | WER | Jargon | Halluc. on noise | p50 | Verdict |
|---|---|---|---|---|---|
| production (dictionary prompt) | 8.8% | 45% | 62% | 724 ms | baseline |
| threads 4 / no temperature fallback | ±0 | ±0 | ±0 | ±0 | no effect |
| no timestamps | +1.2 pt | −3 pt | – | −3% | ❌ |
| beam search 5 | ±0 | −4 pt | – | +2% | ❌ |
| dynamic encoder context | +8 pt | – | – | **−75%** | ❌ accuracy collapses (also on Turbo: 22.8%) |
| **no vocabulary prompt** | −0.5 pt | +4 pt | – | – | ✅ prompts hurt this model |
| project / glossary prompt | +0.9 / +2.3 pt | −6 / ±0 | – | – | ❌ |
| energy VAD | −0.6 pt | +6 pt | 12% | – | ✅ fallback |
| **Silero VAD** | −0.6 pt | +6 pt | **0%** | ±0 | ✅ adopted |

Harness bugs found and fixed along the way: a reused whisper state breaks after the encoder context
changes (now a fresh state on change), whisper.cpp ignores built-in VAD with external states (now run
explicitly), and the scorer counted "2.30pm" vs "2:30 PM" as two errors.

## Step 3: engine bake-off (all with Silero VAD / no prompt unless noted)

| Engine | WER | ΔWER 95% CI | Jargon | Halluc. | p50 | p95 | ≤3 s p50 | Load |
|---|---|---|---|---|---|---|---|---|
| distil-large-v3.5 f16 (old prod) | 8.8% | baseline | 45% | 62% | 724 | 946 | 695 | 1.2 s |
| distil-large-v3.5 f16 | 8.2% | −1.8…+0.4 | 51% | 0% | 732 | 999 | 697 | |
| distil-large-v3.5 q8_0 | 8.2% | −1.7…+0.3 | 54% | 0% | 742 | 938 | 717 | |
| distil-large-v3.5 q5_0 | 8.5% | −1.5…+0.8 | 45% | 0% | 777 | 958 | 755 | |
| large-v3-turbo f16 | 7.5% | −3.1…+0.1 | 56% | 0% | 756 | 1132 | 712 | |
| **large-v3-turbo q8_0** ★ | **7.5%** | −3.0…+0.1 | **56%** | 0% | 760 | 1032 | 729 | 1.1 s |
| large-v3-turbo q5_0 | 8.1% | −2.6…+0.9 | 54% | 0% | 795 | 1032 | 767 | |
| medium.en | 10.4% | −0.3…+3.5 | 45% | 0% | 589 | 1533 | 473 | |
| base.en | 16.6% | +4.8…+11.1 | 38% | 0% | 84 | 312 | 58 | 0.2 s |
| **Parakeet TDT v3 (whisper.cpp ggml, q8)** ★ | 9.9% | −0.7…+2.9 | 51% | 0% | **72** | **299** | **40** | 0.3 s |
| Parakeet TDT v2 (ggml, q8) | 12.7% | +1.2…+6.9 | 39% | 0% | 70 | 283 | 39 | 0.3 s |
| Parakeet v2 ONNX beam + hotwords | 13.5% | +0.9…+9.7 | 53% | 0% | 125 | 660 | 52 | 0.8 s |
| Apple SpeechAnalyzer | 18.6% | +5.9…+14.5 | 26% | 0% | 123 | 457 | 76 | 0.1 s |
| Apple DictationTranscriber + hints | 21.8% | +8.8…+17.8 | 29% | 12% | 226 | 1188 | 80 | 0.1 s |

WER by voice (US / GB / IN / German-accent):

| Engine | Samantha | Daniel | Rishi | Anna |
|---|---|---|---|---|
| distil f16 (old prod) | 2.3% | 6.5% | 5.2% | 21.4% |
| turbo q8 + VAD | 2.5% | 5.0% | 5.0% | **17.6%** |
| Parakeet v3 q8 | 4.1% | 4.5% | 4.7% | 26.4% |
| Apple SpeechAnalyzer | 7.2% | 8.3% | 9.9% | 49.1% |

## Decision

- **Shipped:** large-v3-turbo q8_0 + Silero VAD + no prompt + language forced to English.
  −1.3 pt WER, +11 pt jargon recall, no hallucinations on noise, same latency, 874 MB instead of 1.5 GB.
- **Speed option (not shipped):** Parakeet v3 via whisper.cpp is **10× faster** (72 ms p50, 40 ms on short
  clips) at +2.4 pt WER vs Turbo, and is clearly worse on the German-accent voice. Needs a newer
  whisper.cpp than whisper-rs vendors. Integrate it as a "Fast" engine if real-voice data shows the accent gap is smaller.
- **Rejected:** dynamic encoder context, prompts on distil, beam search, Apple Speech (accuracy),
  q5 quantizations (small accuracy loss for no speed gain on Metal).
