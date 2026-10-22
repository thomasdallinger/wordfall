"""Parakeet TDT engine for the Wordfall bench external engine protocol (bench/README.md).

Backends:
  mlx   parakeet-mlx (Apple GPU via MLX)
  onnx  sherpa-onnx with the k2-fsa int8 exports (CPU via onnxruntime)

Protocol: one ready line, then one JSON line in -> one JSON line out per clip. Logs on stderr.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path

import numpy as np
import soundfile as sf

MODELS_ROOT = Path(
    os.environ.get(
        "WORDFALL_BENCH_MODELS",
        Path.home() / "Library/Application Support/com.wordfall.app/bench-models",
    )
)
MODEL_NAMES = ("parakeet-tdt-0.6b-v2", "parakeet-tdt-0.6b-v3")
SAMPLE_RATE = 16000


def log(*args: object) -> None:
    print(*args, file=sys.stderr, flush=True)


def emit(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def read_wav(path: str) -> np.ndarray:
    audio, sr = sf.read(path, dtype="float32", always_2d=True)
    audio = audio.mean(axis=1)
    if sr != SAMPLE_RATE:
        raise ValueError(f"expected {SAMPLE_RATE} Hz audio, got {sr} Hz")
    return np.ascontiguousarray(audio, dtype=np.float32)


def energy_vad_trim(audio: np.ndarray) -> np.ndarray | None:
    """Port of src-tauri/src/vad.rs `trim`: None if speech-free, else audio trimmed to speech +-250 ms."""
    frame, pad = SAMPLE_RATE // 50, SAMPLE_RATE // 4
    if len(audio) < frame:
        return None
    n = -(-len(audio) // frame)
    rms = np.array([np.sqrt(np.mean(audio[i * frame:(i + 1) * frame] ** 2)) for i in range(n)])
    floor = np.sort(rms)[n // 10]
    voiced = rms > max(floor * 4.0, 0.008)
    window, needed = 10, 6
    count = lambda lo, hi: int(voiced[lo:min(hi, n)].sum())
    first_w = next((i for i in range(n) if count(i, i + window) >= needed), None)
    if first_w is None:
        return None
    first = next(j for j in range(first_w, n) if voiced[j])
    last_w = next(i for i in range(n - 1, -1, -1) if count(max(i + 1 - window, 0), i + 1) >= needed)
    last = next(j for j in range(last_w, -1, -1) if voiced[j])
    start, end = max(first * frame - pad, 0), min((last + 1) * frame + pad, len(audio))
    return audio[start:end] if end > start else None


class MlxBackend:
    engine = "parakeet-mlx"

    def __init__(self, model: str, args: argparse.Namespace) -> None:
        import mlx.core as mx
        from parakeet_mlx import Beam, DecodingConfig, Greedy, from_pretrained
        from parakeet_mlx.audio import get_logmel

        self.mx = mx
        self.get_logmel = get_logmel
        self.dtype = {"bf16": mx.bfloat16, "fp32": mx.float32}[args.dtype]
        path = MODELS_ROOT / "mlx" / model
        if not (path / "config.json").exists():
            raise SystemExit(f"MLX model not found at {path}; run setup.sh")
        self.model = from_pretrained(str(path), dtype=self.dtype)
        decoding = Beam(beam_size=args.beam_size) if args.decoding == "beam" else Greedy()
        self.config = DecodingConfig(decoding=decoding)
        self.label = f"{model} ({args.dtype}, {args.decoding})"
        if args.decoding == "beam":
            self.label = f"{model} ({args.dtype}, beam{args.beam_size})"

    def transcribe(self, audio: np.ndarray, context: list[str]) -> str:
        # parakeet-mlx has no hotword / contextual biasing API: context is ignored.
        # Same as BaseParakeet.transcribe(): float32 PCM -> log-mel (get_logmel needs float32).
        x = self.mx.array(audio)
        mel = self.get_logmel(x, self.model.preprocessor_config)
        result = self.model.generate(mel, decoding_config=self.config)[0]
        return result.text.strip()


class OnnxBackend:
    engine = "parakeet-sherpa-onnx"

    def __init__(self, model: str, args: argparse.Namespace) -> None:
        import sherpa_onnx

        d = MODELS_ROOT / "sherpa-onnx" / f"sherpa-onnx-nemo-{model}-int8"
        if not (d / "encoder.int8.onnx").exists():
            raise SystemExit(f"sherpa-onnx model not found at {d}; run setup.sh")
        self.use_context = args.decoding == "beam" and not args.no_context
        kwargs: dict = dict(
            encoder=str(d / "encoder.int8.onnx"),
            decoder=str(d / "decoder.int8.onnx"),
            joiner=str(d / "joiner.int8.onnx"),
            tokens=str(d / "tokens.txt"),
            num_threads=args.threads,
            sample_rate=SAMPLE_RATE,
            feature_dim=128,
            model_type="nemo_transducer",
            provider=args.provider,
            decoding_method="modified_beam_search" if args.decoding == "beam" else "greedy_search",
            max_active_paths=args.beam_size,
        )
        if self.use_context:
            # Per-stream hotwords need the sentencepiece vocab (with scores) to encode
            # the phrases into BPE tokens. setup.sh copies it in as bpe.vocab.
            vocab = d / "bpe.vocab"
            if not vocab.exists():
                raise SystemExit(f"{vocab} missing; run setup.sh (needed for hotwords)")
            kwargs.update(modeling_unit="bpe", bpe_vocab=str(vocab), hotwords_score=args.hotwords_score)
        self.recognizer = sherpa_onnx.OfflineRecognizer.from_transducer(**kwargs)
        mode = f"beam{args.beam_size}" if args.decoding == "beam" else "greedy"
        if self.use_context:
            mode += f"+hotwords{args.hotwords_score:g}"
        self.label = f"{model} (int8, {args.provider}, {mode})"

    def transcribe(self, audio: np.ndarray, context: list[str]) -> str:
        hot = [c.strip() for c in context if c and c.strip()] if self.use_context else []
        stream = self.recognizer.create_stream(hotwords="/".join(hot)) if hot else self.recognizer.create_stream()
        stream.accept_waveform(SAMPLE_RATE, audio)
        self.recognizer.decode_stream(stream)
        return stream.result.text.strip()


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--backend", choices=("mlx", "onnx"), required=True)
    p.add_argument("--model", choices=MODEL_NAMES, default="parakeet-tdt-0.6b-v2")
    p.add_argument("--decoding", choices=("greedy", "beam"), default="greedy")
    p.add_argument("--beam-size", type=int, default=4)
    p.add_argument("--dtype", choices=("bf16", "fp32"), default="bf16", help="mlx only")
    p.add_argument("--threads", type=int, default=4, help="onnx only")
    p.add_argument("--provider", default="cpu", help="onnx only: cpu | coreml")
    p.add_argument("--hotwords-score", type=float, default=1.5, help="onnx beam only")
    p.add_argument("--no-context", action="store_true", help="onnx: ignore context hints even with beam")
    p.add_argument("--vad", choices=("none", "energy"), default="none",
                   help="energy = Wordfall's RMS gate (vad.rs): speech-free clips return empty text, others are trimmed")
    args = p.parse_args()

    t0 = time.perf_counter()
    backend = (MlxBackend if args.backend == "mlx" else OnnxBackend)(args.model, args)
    # Warm-up on 1 s of silence (kernel compilation / graph allocation) before reporting ready.
    backend.transcribe(np.zeros(SAMPLE_RATE, dtype=np.float32), ["warmup"])
    load_ms = round((time.perf_counter() - t0) * 1000)
    log(f"{backend.engine}: loaded {backend.label} in {load_ms} ms")
    emit({"ready": True, "engine": backend.engine, "model": backend.label, "loadMs": load_ms})

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        clip_id = None
        try:
            req = json.loads(line)
            clip_id = req.get("id")
            audio = read_wav(req["wav"])
            context = [str(c) for c in (req.get("context") or [])]
            t = time.perf_counter()
            if args.vad == "energy":
                audio = energy_vad_trim(audio)
            text = backend.transcribe(audio, context) if audio is not None else ""
            ms = round((time.perf_counter() - t) * 1000)
            emit({"id": clip_id, "text": text, "ms": ms})
        except Exception as e:  # report per-clip failures, keep serving
            log(f"error on {clip_id}: {e!r}")
            emit({"id": clip_id, "error": f"{type(e).__name__}: {e}"})


if __name__ == "__main__":
    main()
