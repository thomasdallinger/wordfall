#!/usr/bin/env bash
# Builds bench/engines/parakeet/ggml/server.cpp against whisper.cpp's parakeet library (Metal).
# Build output stays outside the repo. Also builds parakeet-quantize from the same checkout.
#
#   WHISPER_CPP_SRC=/path/to/whisper.cpp  bench/engines/parakeet/ggml/build.sh
#
# If WHISPER_CPP_SRC is unset, whisper.cpp is cloned at the pinned commit below.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
PIN="231638a"   # first upstream commit verified with this server
BASE="${WORDFALL_BENCH_MODELS:-$HOME/Library/Application Support/com.wordfall.app/bench-models}"
BUILD="$BASE/build/parakeet-ggml"
SRC="${WHISPER_CPP_SRC:-$BASE/build/whisper.cpp-src}"

if [[ ! -f "$SRC/include/parakeet.h" ]]; then
  echo "cloning whisper.cpp@$PIN into $SRC" >&2
  git clone https://github.com/ggml-org/whisper.cpp "$SRC" >&2
  git -C "$SRC" checkout -q "$PIN"
fi

# GGML_NATIVE=OFF: the native CPU-feature try_run checks hang on this machine.
cmake -S "$HERE" -B "$BUILD" -DCMAKE_BUILD_TYPE=Release \
  -DWHISPER_CPP_SRC="$SRC" \
  -DGGML_NATIVE=OFF -DGGML_METAL=ON -DGGML_METAL_EMBED_LIBRARY=ON >&2
cmake --build "$BUILD" --target parakeet-server -j "$(sysctl -n hw.ncpu)" >&2

# parakeet-quantize (for producing q8_0 etc.) from the same checkout
QBUILD="$BASE/build/whisper.cpp-tools"
if [[ -f "$QBUILD/CMakeCache.txt" ]] && ! grep -qF "CMAKE_HOME_DIRECTORY:INTERNAL=$SRC" "$QBUILD/CMakeCache.txt"; then
  rm -rf "$QBUILD"   # configured against a different whisper.cpp checkout
fi
cmake -S "$SRC" -B "$QBUILD" -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
  -DGGML_NATIVE=OFF -DGGML_METAL=ON -DGGML_METAL_EMBED_LIBRARY=ON -DWHISPER_SDL2=OFF >&2
cmake --build "$QBUILD" --target parakeet-quantize -j "$(sysctl -n hw.ncpu)" >&2

echo "$BUILD/parakeet-server"
