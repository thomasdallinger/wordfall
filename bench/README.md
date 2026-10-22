# Wordfall benchmark harness

Scores speech engines and settings on **accuracy and latency** with the same audio,
normalisation and post-processing that Wordfall ships.

```sh
node bench/make-corpus.mjs                       # (re)generate smoke + noise corpora
src-tauri/target/release/wordfall bench            # run bench/configs/default.toml on all corpora
src-tauri/target/release/wordfall bench --config bench/configs/sweep.toml --tier smoke
```

Configs: `default.toml` (production regression gate), `sweep.toml` (decoder knobs),
`bakeoff.toml` (all engines). Results and the decision log: `docs/benchmark-results.md`.

Other engines (setup once):

```sh
bench/engines/parakeet/setup.sh          # Parakeet: whisper.cpp ggml, MLX and ONNX backends
bench/engines/apple-speech/run --help    # Apple SpeechAnalyzer / Dictation (needs CLT 26 SDK, see its README)
```

Reports land in `<bench root>/results/<timestamp>/` (`report.md`, `report.html`, `results.jsonl`).

## Corpus

Root: `~/Library/Application Support/com.wordfall.app/bench/` (override with `--root`).

```
<root>/<tier>/<id>.wav    16 kHz mono PCM16
<root>/<tier>/<id>.json   { "reference": "...", "tier": "...", "tags": [], "terms": [], ... }
```

| Tier | Source | Notes |
|---|---|---|
| `smoke` | `make-corpus.mjs`: TTS (4 voices incl. German-accented) | Regression gate; TTS isn't real speech |
| `noise` | `make-corpus.mjs`: silence, hiss, hum, typing, breath, chord | Expected output is empty |
| `lab` | In-app Voice Lab: you read prompts aloud | Real voice + mic |
| `history` | Opt-in saved dictation audio + transcripts you corrected/verified in History | Real usage (read from `history.json` + `audio/`) |

Collect real data in the app: **Settings → Voice → Keep recordings for accuracy testing**, then use
**Correct** / **Mark as correct** in History, and read prompts in **Voice Lab**.

`terms` are words that must appear verbatim (case-sensitive) in the transcript; they drive the
**jargon recall** metric.

## External engine protocol

Non-whisper engines (Parakeet, Apple Speech, …) run as a long-lived child process speaking JSON lines,
so model load happens once:

1. Bench starts the command from the config (`command = [...]`).
2. Engine prints one line once ready: `{"ready": true, "engine": "parakeet-mlx", "model": "...", "loadMs": 1234}`.
3. For each clip the bench writes one line to stdin:
   `{"id": "jargon-mcp--anna", "wav": "/abs/path.wav", "context": ["Zephyrix", "Supabase"]}`
   (`context` = vocabulary hints; engines may ignore it).
4. Engine answers one line on stdout: `{"id": "...", "text": "...", "ms": 412}`, or `{"id": "...", "error": "..."}`.
   `ms` = compute time for that clip (excluding process start and model load).
5. Logs go to stderr only. Stdin EOF means exit.

Engines live in `bench/engines/<name>/` with their own README and a `run` entry point.
