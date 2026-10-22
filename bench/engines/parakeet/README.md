# parakeet: NVIDIA Parakeet TDT 0.6B engine

An external engine for the Wordfall bench. It speaks the JSON-lines protocol from
[`bench/README.md`](../../README.md) and runs NVIDIA Parakeet TDT 0.6B (`v2` is English-only,
`v3` is multilingual) on one of three backends:

| `--backend` | Runtime | Device | Weights | Context hints |
|---|---|---|---|---|
| `ggml` | whisper.cpp `include/parakeet.h` (upstream commit `231638a`), a small C++ server in `ggml/` | Metal GPU | `ggml-parakeet-tdt-0.6b-{v2,v3}-{f16,q8_0}.bin` | Ignored. The API only does greedy decoding. |
| `mlx` | [`parakeet-mlx`](https://github.com/senstella/parakeet-mlx) 0.5.3 on MLX 0.32 | Apple GPU | `mlx-community/parakeet-tdt-0.6b-{v2,v3}` (bf16 at runtime) | Ignored. parakeet-mlx has no biasing API. |
| `onnx` | [`sherpa-onnx`](https://github.com/k2-fsa/sherpa-onnx) 1.13.8 on onnxruntime | CPU | k2-fsa int8 exports `sherpa-onnx-nemo-parakeet-tdt-0.6b-{v2,v3}-int8` | **Used** as hotwords with `--decoding beam`. Ignored with greedy decoding. |

`ggml` matters most because Wordfall already embeds whisper.cpp.

## Setup

```sh
bench/engines/parakeet/setup.sh              # venv + every model + ggml server build (idempotent)
SKIP_GGML=1 bench/engines/parakeet/setup.sh  # Python backends only
```

- **Python.** The system Python 3.9 is too old for parakeet-mlx, so `setup.sh` uses `uv` to
  create a Python 3.13 environment in `.venv/`. Every package version is pinned in
  `requirements.txt`. If you don't have `uv`, install it with `brew install uv`.
- **Models.** They're downloaded to
  `~/Library/Application Support/com.wordfall.app/bench-models/`, never into the repo. Set
  `WORDFALL_BENCH_MODELS` to use a different folder. That folder holds `mlx/`, `sherpa-onnx/`,
  `ggml/`, `nemo/` and `build/`, about 12 GB in total.
- **ggml build.** `ggml/build.sh` clones whisper.cpp at the pinned commit into
  `bench-models/build/whisper.cpp-src`. To use an existing checkout, set `WHISPER_CPP_SRC`.
  It then builds `parakeet-server` and `parakeet-quantize` into `bench-models/build/`. The build
  uses `-DGGML_NATIVE=OFF -DGGML_METAL=ON -DGGML_METAL_EMBED_LIBRARY=ON`, because with
  `GGML_NATIVE=ON` the CPU-feature checks hang on this Mac.
- **ggml models.**
  - v3 f16 and q8_0 come from `ggml-org/parakeet-GGUF`.
  - ggml-org doesn't publish a v2 file. `setup.sh` downloads `nvidia/parakeet-tdt-0.6b-v2.nemo`
    (2.5 GB) and converts it with `models/convert-parakeet-to-ggml.py`. That step runs in a
    separate venv with torch and takes about 5 s.
  - `parakeet-quantize ... q8_0` then produces the v2 q8_0 file.
  - `handy-computer/parakeet-tdt-0.6b-v2-gguf` uses the transcribe.cpp format, which this
    server can't load.
- **Hotwords.** sherpa-onnx needs the sentencepiece vocabulary to turn hotwords into BPE tokens.
  `setup.sh` copies `tokenizer.vocab` from the MLX download into the sherpa model folder as
  `bpe.vocab`. Its tokens match `tokens.txt` 1:1.

## Run

```sh
bench/engines/parakeet/run --backend ggml --model parakeet-tdt-0.6b-v2 --quant q8_0
bench/engines/parakeet/run --backend ggml --model parakeet-tdt-0.6b-v3 --quant f16
bench/engines/parakeet/run --backend mlx  --model parakeet-tdt-0.6b-v2
bench/engines/parakeet/run --backend onnx --model parakeet-tdt-0.6b-v2                    # greedy, no context
bench/engines/parakeet/run --backend onnx --model parakeet-tdt-0.6b-v2 --decoding beam    # beam 4 + context as hotwords
```

Options:

- **All backends:** `--model parakeet-tdt-0.6b-v2|parakeet-tdt-0.6b-v3`. The default is v2.
- **`ggml`:**
  - `--quant f16|q8_0`, or pass `--model /abs/path.bin` instead.
  - `--threads N`. The default is 4.
  - `--no-gpu`.
- **`mlx`:**
  - `--decoding greedy|beam`.
  - `--beam-size N`.
  - `--dtype bf16|fp32`.
- **`onnx`:**
  - `--decoding greedy|beam`.
  - `--beam-size N`. The default is 4.
  - `--threads N`.
  - `--provider cpu|coreml`. CoreML is about 2× slower and adds about 3 s to load.
  - `--hotwords-score F`. The default is 1.5.
  - `--no-context`.
- **`mlx` and `onnx`:** `--vad energy` is a port of `src-tauri/src/vad.rs`. A clip it judges
  speech-free returns `""`. Any other clip is trimmed to the speech plus 250 ms on each side.

Bench config:

```toml
[[config]]
name = "parakeet-v2-ggml-q8"
engine = "external"
command = ["bench/engines/parakeet/run", "--backend", "ggml", "--model", "parakeet-tdt-0.6b-v2", "--quant", "q8_0"]
```

### Protocol notes

- **Ready line.** Before it prints the ready line, every backend loads the model and transcribes
  1 s of silence as a warm-up. `loadMs` covers both.
- **Timing.** `ms` measures `log-mel + encoder + decoder` for one clip. It doesn't include
  reading the WAV.
- **Errors.** A bad request, such as a missing file or invalid JSON, gets an
  `{"id", "error"}` line, and the engine keeps serving. On stdin EOF the engine exits 0.

## Measured (M3 Pro, 2026-10-20, smoke + noise = 112 clips)

> **The latency numbers are approximate.** A whisper bench sweep was running on the same machine
> at the same time. Re-measure on a quiet machine before deciding anything.
>
> **The "rough err%" column isn't the harness WER.** It's a quick lower-case,
> punctuation-stripped word edit distance with no number normalization, for example "5" vs.
> "five".
>
> "terms" counts clips where every term in the clip's `terms` appears verbatim, out of the 60
> clips that have terms.

| Config | loadMs (warm disk cache) | Median ms | p90 ms | <3 s median | >15 s median | Rough err% | Terms | Noise non-empty |
|---|---|---|---|---|---|---|---|---|
| ggml v2 f16 | 650 | 70 | 113 | 38 | 322 | 13.1 | 10/60 | 0/9 |
| ggml v2 q8_0 | 420 | 72 | 128 | 41 | 328 | 13.2 | 10/60 | 0/9 |
| ggml v3 f16 | 670 | 73 | 120 | 41 | 352 | 10.3 | 15/60 | 0/9 |
| ggml v3 q8_0 | 260–480 | 72 | 117 | 41 | 348 | 10.1 | 14/60 | 0/9 |
| mlx v2 bf16 greedy | 680 (1.8–2 s first load) | 102 | 162 | 70 | 402 | 14.1 | 11/60 | 0/9 |
| mlx v3 bf16 greedy | 900 | 110 | 195 | 76 | 494 | 11.3 | 15/60 | 0/9 |
| onnx v2 int8 greedy | 890 | 162 | 298 | 79 | 949 | 13.6 | 11/60 | 0/9 |
| onnx v2 beam 4, no hotwords | 855 | 168 | 303 | 82 | 916 | 16.4 | 7/60 | 3/9 |
| onnx v2 beam 4, clip's own terms as hotwords | 836 | 140 | 278 | 76 | 816 | 12.4 | **36/60** | 3/9 |
| onnx v2 beam 4, 41-term global list | 801 | 150 | 294 | 78 | 872 | 12.8 | 36/60 | 4/9 |
| onnx v2 beam 4, 41 terms + `--vad energy` | 807 | 137 | 277 | 62 | 848 | 13.2 | 37/60 | 1/9 |
| onnx v3 int8 greedy | 890 | 154 | 375 | 92 | 895 | 14.1 | 11/60 | 0/9 |
| onnx v3 beam 4, clip's own terms as hotwords | 877 | 191 | 349 | 95 | 1096 | 13.9 | 27/60 | 4/9 |

Peak memory footprint with one 32 s clip:

| Backend | Peak footprint |
|---|---|
| ggml q8_0 | about 0.78 GB |
| ggml f16 | about 1.3 GB |
| onnx int8 | about 1.33 GB |
| mlx | about 3.3–3.5 GB, because it loads the fp32 safetensors and then casts them to bf16 |

## Notes and limitations

- **Context and hotwords.**
  - Only `onnx --decoding beam` uses `context`. sherpa-onnx's
    `OfflineTransducerModifiedBeamSearchNeMoDecoder` handles TDT durations and applies a
    `ContextGraph` boost, and the engine passes the hotwords per stream. Without hotwords, beam
    search is slightly *worse* than greedy decoding.
  - Hotwords fix jargon well: "Zefirix" becomes "Zephyrix", "Zupa Baza" becomes "Supabase", and
    "Earth" becomes "ERP".
  - **Hotwords also hallucinate on noise.** With the 41-term list, near-silence and keyboard
    clips came back as "Claude Code Claude Code". Even without hotwords, sherpa's NeMo beam
    search turns silence or breath into "Okay." or "I'm not sure if I can do it.".
    `--vad energy` removes most of these, but one breath clip still got through.
  - Greedy decoding returned `""` for all 9 noise clips on every backend.
- **v2 vs. v3.** Most smoke clips use non-native TTS voices, including German-accented "Anna".
  On those clips the multilingual v3 does better: "dry six" is transcribed as "30", and it
  makes fewer accent errors. It also scored better on rough err% (about 10% vs. about 13% on
  ggml). v2 has no advantage here, even though the user only dictates English. Check this
  against real-voice `lab` / `history` clips.
- **Casing.** ggml v2 lower-cases some acronyms ("html", "earth"). v3 tends to write
  "superbase" and "chronics". Neither model knows the project names without biasing.
- **First-run Metal compile (ggml).** The first time a freshly built `parakeet-server` runs,
  ggml compiles its Metal libraries. That made the load about 6 s and made clips noticeably
  slower (p90 about 580 ms) for the rest of that first run. Runs after that are fast. Do a
  throwaway run after every rebuild.
- **ggml long clips.** `n_audio_ctx` is 5000 mel frames, which is 50 s. Clips longer than that
  go through a dynamically sized encoder graph, which is reallocated for each new length. No
  smoke clip is that long.
- **MLX first clip** takes about 40 ms longer than steady state, even after the warm-up.
  Medians aren't affected.
- **Clip length.** All smoke clips (up to 32 s) are processed in one pass. No backend chunks
  them.
- **Sample rate.** Input must be 16 kHz, either PCM16 or float32 WAV. Multi-channel audio is
  downmixed.
