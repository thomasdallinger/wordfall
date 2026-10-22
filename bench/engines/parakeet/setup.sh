#!/usr/bin/env bash
# One-shot setup for the Parakeet bench engine: Python venv + all models (+ optional ggml build).
#
#   bench/engines/parakeet/setup.sh            # venv, MLX + sherpa-onnx + ggml models, ggml server build
#   SKIP_GGML=1 bench/engines/parakeet/setup.sh
#
# Models go to ~/Library/Application Support/com.wordfall.app/bench-models/ (override: WORDFALL_BENCH_MODELS).
# Needs: uv (brew install uv); for ggml also cmake + Xcode CLT, and ~2.5 GB extra for the v2 conversion.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
M="${WORDFALL_BENCH_MODELS:-$HOME/Library/Application Support/com.wordfall.app/bench-models}"
mkdir -p "$M/mlx" "$M/sherpa-onnx" "$M/ggml"

# 1. Python env (system python 3.9 is too old for parakeet-mlx; uv fetches 3.13).
if [[ ! -x "$HERE/.venv/bin/python" ]]; then
  uv venv --python 3.13 "$HERE/.venv"
fi
uv pip install --python "$HERE/.venv/bin/python" -r "$HERE/requirements.txt"
HF="$HERE/.venv/bin/hf"

# 2. MLX weights (mlx-community conversions).
for v in v2 v3; do
  "$HF" download "mlx-community/parakeet-tdt-0.6b-$v" --local-dir "$M/mlx/parakeet-tdt-0.6b-$v"
done

# 3. sherpa-onnx int8 exports (k2-fsa). bpe.vocab (sentencepiece vocab with scores) enables hotwords.
for v in v2 v3; do
  d="$M/sherpa-onnx/sherpa-onnx-nemo-parakeet-tdt-0.6b-$v-int8"
  if [[ ! -f "$d/encoder.int8.onnx" ]]; then
    curl -fL "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-$v-int8.tar.bz2" \
      | tar xj -C "$M/sherpa-onnx"
  fi
  cp "$M/mlx/parakeet-tdt-0.6b-$v/tokenizer.vocab" "$d/bpe.vocab"
done

[[ -n "${SKIP_GGML:-}" ]] && exit 0

# 4. ggml (whisper.cpp parakeet): server build, v3 from ggml-org, v2 converted from the NeMo checkpoint.
"$HERE/ggml/build.sh"
SRC="${WHISPER_CPP_SRC:-$M/build/whisper.cpp-src}"
"$HF" download ggml-org/parakeet-GGUF ggml-parakeet-tdt-0.6b-v3-f16.bin ggml-parakeet-tdt-0.6b-v3-q8_0.bin --local-dir "$M/ggml"
if [[ ! -f "$M/ggml/ggml-parakeet-tdt-0.6b-v2-f16.bin" ]]; then
  "$HF" download nvidia/parakeet-tdt-0.6b-v2 parakeet-tdt-0.6b-v2.nemo --local-dir "$M/nemo/parakeet-tdt-0.6b-v2"
  CV="$M/build/convert-venv"
  [[ -x "$CV/bin/python" ]] || uv venv --python 3.13 "$CV"
  uv pip install --python "$CV/bin/python" torch pyyaml numpy
  TMPDIR="$M/build" "$CV/bin/python" -I "$SRC/models/convert-parakeet-to-ggml.py" \
    --model "$M/nemo/parakeet-tdt-0.6b-v2/parakeet-tdt-0.6b-v2.nemo" \
    --out-dir "$M/ggml" --out-name ggml-parakeet-tdt-0.6b-v2-f16.bin
fi
if [[ ! -f "$M/ggml/ggml-parakeet-tdt-0.6b-v2-q8_0.bin" ]]; then
  "$M/build/whisper.cpp-tools/bin/parakeet-quantize" \
    "$M/ggml/ggml-parakeet-tdt-0.6b-v2-f16.bin" "$M/ggml/ggml-parakeet-tdt-0.6b-v2-q8_0.bin" q8_0
fi
