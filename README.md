<p align="center"><img src="app-icon.png" width="128" alt="Wordfall icon"></p>

<h1 align="center">Wordfall</h1>

<p align="center">Hold <kbd>fn</kbd>, speak, let go. Your words appear wherever your cursor is.<br>
Local speech recognition for macOS and Windows. Nothing leaves your computer.</p>

---

- **Push to talk anywhere.** Hold <kbd>fn</kbd> (Windows: <kbd>Right Ctrl</kbd>, or another right-hand modifier)
  in any app; release to paste. Double-tap for hands-free, <kbd>esc</kbd> to cancel.
- **Speaks your languages.** Pick the languages you dictate in (English and your system language by
  default). Each dictation is written in whichever of them you spoke; no switching, and short phrases
  don't flip to a language you don't speak.
- **Clean text.** Drops um, uh, äh, ähm. Say "first, … second, …" / "erstens, … zweitens, …" for a
  numbered list, "bullet point …" / "Stichpunkt …" for a bulleted one.
- **On-device Whisper.** whisper.cpp with Metal on macOS, CPU (or Vulkan/CUDA) on Windows. Large v3 Turbo by
  default; switch models in Settings.
- **Fast.** About 0.7 s from release to text for a typical dictation on an M-series Mac (about 0.1 s more
  when it also has to tell your languages apart).
- **Gets your words right.** A dictionary of replacements (“post gress” → “PostgreSQL”), words to
  recognise, and suggestions learned from your transcripts.
- **No hallucinated "Thank you."** Voice activity detection drops silence, hum and typing before decoding.
- **Measured, not guessed.** A built-in benchmark harness scores engines and settings on accuracy and
  latency (see [`bench/`](bench/README.md) and [results](docs/benchmark-results.md)).

## Install

Wordfall is built from source. One repository serves both platforms.

### macOS

**You need:** macOS 13 or later on Apple Silicon, the Xcode Command Line Tools (`xcode-select --install`),
[Rust](https://rustup.rs) (stable), Node 22+ and CMake (`brew install cmake`).

1. **Get the code**

   ```sh
   git clone https://github.com/thomasdallinger/wordfall.git
   cd wordfall
   npm install
   ```

2. **Create a signing identity (once).** macOS ties permission grants to the app's signature, so a stable
   identity means you grant permissions once and keep them across updates. In *Keychain Access → Certificate
   Assistant → Create a Certificate…*, name it `Wordfall Local Code Signing`, set *Certificate Type* to
   *Code Signing*, and create it. (Any other name works if you also change `bundle.macOS.signingIdentity`
   in `src-tauri/tauri.conf.json`.)

3. **Build and install**

   ```sh
   npm run tauri build
   cp -R src-tauri/target/release/bundle/macos/Wordfall.app /Applications/
   open /Applications/Wordfall.app
   ```

4. **First launch.** Setup walks you through three permissions, a model download (about 0.9 GB) and a
   practice dictation:

   | Permission | Why |
   |---|---|
   | Microphone | to hear you |
   | Input Monitoring | to notice the push-to-talk key |
   | Accessibility | to paste into the app you're using |

   If pressing <kbd>fn</kbd> opens the emoji picker, set *System Settings → Keyboard → Press 🌐 key to → Do
   Nothing* (Wordfall offers to do this for you).

**Updating.** Pull, rebuild, then replace the app while it isn't running. Settings, history, dictionary and
models stay where they are (`~/Library/Application Support/com.wordfall.app`), and permissions carry over as
long as the signing identity is the same.

```sh
git pull && npm install && npm run tauri build
osascript -e 'quit app "Wordfall"'
rm -rf /Applications/Wordfall.app
cp -R src-tauri/target/release/bundle/macos/Wordfall.app /Applications/
open /Applications/Wordfall.app
```

### Windows

**You need:** Windows 10 or 11 (x64), [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/)
with the *Desktop development with C++* workload, [Rust](https://rustup.rs) (stable, the default MSVC
toolchain), Node 22+, CMake and Git. WebView2 ships with Windows 11; on Windows 10 the installer fetches it.

1. **Get the code**

   ```powershell
   git clone https://github.com/thomasdallinger/wordfall.git
   cd wordfall
   npm install
   ```

2. **Build**

   ```powershell
   npm run tauri:windows                       # CPU
   npm run tauri:windows -- --features vulkan  # GPU via Vulkan (needs the Vulkan SDK); or --features cuda
   ```

   `tauri:windows` is `tauri build` with the C/C++ runtime linked statically (see
   `scripts/tauri-windows.mjs`), so the app runs without the Visual C++ Redistributable.

3. **Install.** Run `src-tauri\target\release\bundle\nsis\Wordfall_<version>_x64-setup.exe`. It installs
   for your user only (no admin needed); Wordfall lives in the notification area.

4. **First launch.** Windows only asks for the microphone (*Settings → Privacy & security → Microphone →
   Let desktop apps access your microphone*); typing into other apps and the push-to-talk key need no extra
   permission. Hold <kbd>Right Ctrl</kbd> to dictate (change it under *Settings → Recording*). Without a GPU
   build, *Large v3 Turbo (Q5)* or *Base* are noticeably faster than the default model.

**Updating.** `git pull`, `npm install`, `npm run tauri:windows`, then run the new installer over the old
one. Your data in `%APPDATA%\com.wordfall.app` and models in `%LOCALAPPDATA%\com.wordfall.app\models` are kept.

Known limits: Windows doesn't let Wordfall see keys pressed in, or paste into, apps running as administrator.

## Using it

- **Dictate:** hold the key, speak, let go. Double-tap it for hands-free; tap again to finish. <kbd>esc</kbd>
  cancels.
- **Languages:** *Settings → Model → Languages*. With more than one, each dictation is written in whichever
  you spoke. Keep the list to the languages you actually use; that's what keeps short phrases from being
  mistaken for something else.
- **Lists** (*Settings → General → Make lists*):

  | Say | You get |
  |---|---|
  | "Plan for today: first, review the PRs. Second, write the docs." | `Plan for today:` `1. Review the PRs` `2. Write the docs` |
  | "Erstens Präsentation fertig machen, zweitens Anna anrufen" | `1. Präsentation fertig machen` `2. Anna anrufen` |
  | "Shopping list, bullet point milk, bullet point eggs" | `Shopping list:` `- Milk` `- Eggs` |
  | "Einkaufsliste, Stichpunkt Milch, Stichpunkt Eier" | `Einkaufsliste:` `- Milch` `- Eier` |

  "number one / two", "finally", "als Erstes / Zweites" and "zuletzt" work too. A list needs at least two items.

## Models

Models are whisper.cpp `ggml` files, downloaded on demand into
`~/Library/Application Support/com.wordfall.app/models` (Windows: `%LOCALAPPDATA%\com.wordfall.app\models`).
Already have some? Add their folder under *Settings → Model → Model folders*; they're used in place, not copied.

**Settings → Data → Import from folder…** merges history, dictionary entries and suggestions from JSON
exports in any folder. Files are recognised by their structure, and model files found there are picked
up automatically.

## Development

```sh
npm run dev                     # UI in a browser against a mock backend (http://localhost:1420)
npm run tauri dev               # the real app
npm run check                   # svelte-check / TypeScript
(cd src-tauri && cargo test)    # Rust unit tests
src-tauri/target/release/wordfall bench     # accuracy/latency regression check
```

Cross-compiling the Windows app from macOS works with `cargo-xwin` (`npm run tauri:windows -- --runner cargo-xwin
--target x86_64-pc-windows-msvc`, with LLVM, lld and NSIS from Homebrew). whisper-rs-sys decides from the *host*
OS that it must link `ggml-blas`, so cross builds need an empty `ggml-blas.lib` on the library path
(`RUSTFLAGS="-L native=<dir>"`); native Windows builds don't.

Layout:

- `src/`: Svelte 5 UI. One bundle serves both windows (the main window and the floating pill), chosen by window label.
- `src-tauri/src/`: Rust core.
  - `hotkey.rs`: the hold / tap / double-tap state machine
  - `platform/`: OS integrations behind one interface: permissions, paste, focus, cues, muting and the
    push-to-talk key listener (`macos*.rs`: event tap, AppKit; `windows*.rs`: keyboard hook, Win32)
  - `audio.rs`: capture, metering, resampling
  - `transcribe.rs`: model catalog, whisper.cpp engine, language detection among the user's languages
  - `vad.rs`: silence detection
  - `postprocess.rs`: filler removal (language-aware), dictionary
  - `lists.rs`: spoken enumerations → numbered / bulleted lists
  - `session.rs`: record → transcribe → paste pipeline
  - `bench.rs`: benchmark harness
- `docs/ipc.md`: contract between UI and core. `DESIGN.md`: design principles and tokens.

## License

MIT. See [LICENSE](LICENSE) and [third-party notices](THIRD_PARTY_NOTICES.md).
