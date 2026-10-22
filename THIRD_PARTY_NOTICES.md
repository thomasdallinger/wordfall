# Third-party notices

The app bundles or links the following open-source components. Full license texts ship with
each package (`node_modules/*/LICENSE`, `~/.cargo/registry/src/*/*/LICENSE*`).

## Native (Rust)

| Component | License |
|---|---|
| [whisper.cpp](https://github.com/ggml-org/whisper.cpp) / ggml (via whisper-rs-sys) | MIT |
| [whisper-rs](https://github.com/tazz4843/whisper-rs) | Unlicense |
| [Tauri](https://tauri.app) and plugins (autostart, dialog, global-shortcut, single-instance) | MIT OR Apache-2.0 |
| [cpal](https://github.com/RustAudio/cpal) | Apache-2.0 |
| [arboard](https://github.com/1Password/arboard) | MIT OR Apache-2.0 |
| [objc2](https://github.com/madsmtm/objc2) crates, core-foundation, core-graphics | MIT / Apache-2.0 |
| serde, tokio, reqwest, regex, chrono, uuid, parking_lot, anyhow, toml, dirs | MIT / Apache-2.0 |

## Interface

| Component | License |
|---|---|
| [Svelte](https://svelte.dev) | MIT |
| [Tailwind CSS](https://tailwindcss.com) | MIT |
| [Lucide](https://lucide.dev) icons | ISC |
| [Schibsted Grotesk](https://github.com/schibsted/schibsted-grotesk) (via @fontsource) | SIL Open Font License 1.1 |
| [Literata](https://github.com/googlefonts/literata) (via @fontsource) | SIL Open Font License 1.1 |

## Models (downloaded at runtime, not redistributed in this repository)

| Model | License |
|---|---|
| OpenAI Whisper large-v3-turbo / medium.en / base.en / large-v3 (ggml conversions by ggml-org) | MIT |
| distil-whisper distil-large-v3.5 | MIT |
| Silero VAD v5.1.2 (ggml conversion by ggml-org) | MIT |
| NVIDIA Parakeet TDT 0.6B v2/v3 (optional benchmark adapter only) | CC-BY-4.0 — attribution: NVIDIA |
