# apple-speech: Apple on-device speech recognition engine

An external engine for the Wordfall bench. It speaks the JSON-lines protocol from
[`bench/README.md`](../../README.md) and wraps Apple's built-in recognizers so they can be
compared with Whisper on the same corpus.

| `--api` | Framework | Model | Context hints |
|---|---|---|---|
| `analyzer` (default) | `SpeechAnalyzer` + `SpeechTranscriber`, preset `.transcription` | New macOS 26 long-form model, downloaded through `AssetInventory` | Accepted but **ignored** by this model (results are byte-identical with and without) |
| `dictation` | `SpeechAnalyzer` + `DictationTranscriber`, preset `.shortDictation` | System-dictation model, the same one as on-device `SFSpeechRecognizer` | Used: `AnalysisContext.contextualStrings[.general]` |
| `legacy` | `SFSpeechRecognizer`, `SFSpeechURLRecognitionRequest`, `requiresOnDeviceRecognition`, `addsPunctuation`, `taskHint = .dictation` | System-dictation model | Used: `contextualStrings` |

Everything runs on-device. Only `legacy` needs the Speech Recognition permission (see
[Permissions](#permissions)).

## Build and run

You need macOS 26 and the **macOS 26 SDK**, which ships with Xcode 26 or the Command Line Tools
for Xcode 26.x. Older SDKs don't contain `SpeechAnalyzer`.

```sh
bench/engines/apple-speech/run                       # --api analyzer, en-US
bench/engines/apple-speech/run --api dictation
bench/engines/apple-speech/run --api legacy
bench/engines/apple-speech/run --help
```

`run` rebuilds when the binary is missing or older than the sources (`swift build -c release`,
with build output on stderr), then execs `.build/release/apple-speech-engine`. Once the binary
exists, running it doesn't need the SDK.

Flags:

- `--locale en-US`: the default.
- `--no-context`: ignore the request's `context`. Use it for A/B runs.
- `--warmup PATH|none`: the clip transcribed before the ready line. The default is
  `Resources/warmup.wav`, a 2.5 s `say` clip.
- `--debug`: logs every module result to stderr.
- `--authorize`: only requests the legacy permission, then exits.
- `--auth-timeout S`: how long to wait for a human to answer the permission dialog. The default
  is 60 s, or 300 s with `--authorize`.

Bench config, using the `command = [...]` form from `bench/README.md`:

```toml
command = ["bench/engines/apple-speech/run", "--api", "analyzer"]
```

### Toolchain on this machine (2026-10)

This Mac had **Command Line Tools 15.3 (SDK 14.4, Swift 5.10)** selected, and no Xcode. If the SDK
is older than 26, `run` stops with instructions. The proper fix needs an admin password:

```sh
sudo softwareupdate -i "Command Line Tools for Xcode 26.5-26.5"   # or install Xcode 26
```

To build without root, I took the signed Apple packages for CLT 26.5 from the public
software-update catalog (`CLTools_Executables_Universal.pkg` and `CLTools_macOSNMOS_SDK.pkg`,
product `047-91568`). I unpacked them with `pkgutil --expand-full` into
`/tmp/apple-speech-verify/dev/CommandLineTools` (with `SDKs/MacOSX.sdk -> MacOSX26.5.sdk`) and
built with `DEVELOPER_DIR=/tmp/apple-speech-verify/dev/CommandLineTools`. This gives Swift 6.3.2
and SDK 26.5. `/tmp` is cleared on reboot. The built binary keeps working without it, but
installing CLT 26.x is the durable fix.

## Protocol details

The ready line is printed after setup and the warm-up:

```json
{"ready":true,"engine":"apple-speech-analyzer","model":"SpeechTranscriber en-US preset=transcription (on-device)",
 "loadMs":91,"assetMs":14,"modelMs":77,"warmupMs":59,"setupMs":222,"assetStatus":"installed","api":"analyzer","locale":"en-US"}
```

- `loadMs` = `assetMs` (asset check, plus the download on the first run) + `modelMs` (model
  load / `prepareToAnalyze`).
- `setupMs` is all setup inside the process, warm-up included. Process launch, dyld and framework
  loading come on top: the bench saw 0.25 to 1.5 s from spawn to the ready line.

Response lines look like `{"id":"…","text":"…","ms":142,"setupMs":5}`.

- `ms` is recognition time only: from `analyzeSequence(from:)` until `finalizeAndFinish` returns
  and the result stream closes. For `legacy`, it runs from `recognitionTask` to the final result.
- `setupMs` (analyzer and dictation modes) is the per-clip analyzer and module setup, which is
  **excluded** from `ms` (see below).
- Failures come back as `{"id":"…","error":"…"}`. "No speech detected" from `legacy` is mapped
  to `""`.

## How it works

- **Assets.** `AssetInventory.status(forModules:)` is checked first, then
  `assetInstallationRequest(supporting:)?.downloadAndInstall()` runs, with `progress` logged to
  stderr in 10 % steps. The first run on this Mac downloaded the en-US SpeechTranscriber model in
  about **150 s**. After that, `assetMs` is about 15 ms. Assets are stored system-wide. The locale
  reservation is filed under the bundle id `com.wordfall.bench.apple-speech`, taken from the
  embedded Info.plist.
- **Audio format.** `bestAvailableAudioFormat` is `1 ch, 16000 Hz, Int16`, which is exactly the
  corpus format, so the files go in unconverted through `analyzeSequence(from: AVAudioFile)`.
  That method also converts other formats internally.
- **One analyzer per clip, models kept resident.** Each clip gets a new `SpeechTranscriber` or
  `DictationTranscriber` and a new `SpeechAnalyzer` with
  `Options(priority: .userInitiated, modelRetention: .processLifetime)`. The flow is
  `prepareToAnalyze(in: format)` → `analyzeSequence(from:)` → `finalizeAndFinish(through:)`.
  This per-clip setup costs about 5 ms for `analyzer` and about 1 ms for `dictation` without
  hints.
  - **Reuse was tried and dropped.** One long-lived analyzer, fed one file after another with
    `analyzeSequence(from:)` + `finalize(through:)`, is what the API docs describe: the audio
    timeline continues across files. On macOS 26.5, every file after the first came back garbled
    (`", , the , and keep the row level security policies."`, `"....."`), with or without
    context.
- **Context hints.** The hints go into `AnalysisContext.contextualStrings[.general]`, capped at
  100 phrases. The `AnalysisContext` object is reused while the hint list stays the same.
  - The docs describe contextual strings only for `DictationTranscriber`. That matches what I
    measured: `SpeechTranscriber` ignores them.
  - With 28 hints, `prepareToAnalyze` on the dictation model takes about 65 to 115 ms per
    analyzer. That cost shows up in `setupMs`. In the app, it could be paid while the hotkey is
    held.
- **DictationTranscriber quirk.** It reports `resultsFinalizationTime = 0` even on its last
  result, so `isFinal` is always false. Neither preset emits volatile results, so the collector
  accepts every result (a later result replaces any earlier one it overlaps) and treats the end of
  the stream as completion.

## Permissions

- **`analyzer` and `dictation` need no permission.** No TCC prompt appeared and authorization
  stayed `notDetermined`. Microphone access isn't involved either, because the input is files.
- **`legacy` needs Speech Recognition (TCC) permission.** Without it, `SFSpeechRecognizer`
  refuses to run.

What goes wrong with a bare CLI, as seen in the `tccd` log:

1. TCC attributes the request to the *responsible process*. For a CLI, that is the terminal app
   (for example your terminal or IDE) or whatever app spawned the bench.
2. That app's Info.plist has no `NSSpeechRecognitionUsageDescription`, so
   `requestAuthorization` **kills the process with SIGABRT (exit 134)**. Adding an Info.plist to
   the binary alone doesn't help.

The fix has two parts:

1. **Embedded Info.plist.** `Resources/Info.plist` contains `CFBundleIdentifier` and
   `NSSpeechRecognitionUsageDescription`. It is linked into the Mach-O as `__TEXT,__info_plist`
   through `-sectcreate` in `Package.swift`.
2. **Disclaimed re-exec** (`Disclaim.swift`). In `legacy` / `--authorize` mode the binary
   re-execs itself in place with `posix_spawn` + `POSIX_SPAWN_SETEXEC` +
   `responsibility_spawnattrs_setdisclaim(attr, 1)`. This is what Xcode/LLDB do for apps they
   launch. The function is exported by libquarantine but has no public header, so it is looked up
   with `dlsym`. The pid and the stdin/stdout pipes stay the same.

After the re-exec, `tccd` logs `AUTHREQ_PROMPTING … subject=…/apple-speech-engine` and shows the
normal permission dialog for this binary. There is no crash.

The permission is stored per binary path. The binary is ad-hoc signed, so a rebuild can trigger
the prompt again.

**One-time grant (needs a human):**

```sh
bench/engines/apple-speech/run --authorize      # waits up to 300 s
```

When macOS asks whether "apple-speech-engine" may access Speech Recognition, click **Allow**.
The tool prints `permission OK` and exits 0.

- If someone clicked **Don't Allow**, the status becomes `denied` and the dialog doesn't come
  back. Fix it in **System Settings → Privacy & Security → Speech Recognition** by switching
  `apple-speech-engine` on.
- If a normal `--api legacy` run is unauthorized, it requests the permission, waits
  `--auth-timeout` (60 s), then prints these instructions to stderr and **exits 3**. It never
  loops.

## Findings (M3 Pro, macOS 26.5, smoke corpus: 104 TTS clips + 8 noise clips)

The driver sent every clip through `run`. For the "hints" columns, `context` was the global
28-term vocabulary from the corpus (Zephyrix, Steinhauer, Supabase, MCP, SvelteKit, …) and was the
same for every clip.

WER here is a crude word-level edit distance (lower case, punctuation stripped, "3" ≠ "three"),
**not** the bench's normalisation. Use it only to compare rows. Jargon terms must match exactly,
case included.

| mode | hints | WER | jargon terms | ms median | ms p90 | short <3 s (n=24) | long >15 s (n=8) | setupMs/clip | noise |
|---|---|---|---|---|---|---|---|---|---|
| analyzer | yes | 0.197 | 34/140 | 149 | 262 | 87 | 734 | 5 | all `""` |
| analyzer | no | 0.197 | 34/140 | 155 | 296 | 91 | 760 | 5 | all `""` |
| dictation | yes | 0.209 | **79/140** | 225 | 995 | 77 | 1229 | 114 | `""`, except `chord-3s` → `"No"` |
| dictation | no | 0.230 | 34/140 | 200 | 801 | 79 | 1159 | 1 | `""`, except `chord-3s` → `"No"` |
| legacy | – | not measured yet: the permission dialog wasn't answered (see Permissions) | | | | | | | |

WER by voice (analyzer / dictation+hints):

| voice | analyzer | dictation + hints |
|---|---|---|
| Daniel | 0.095 | 0.104 |
| Samantha | 0.084 | 0.107 |
| Rishi | 0.118 | 0.132 |
| Anna (German-accented TTS) | 0.492 | 0.492 |

SpeechTranscriber transcribes the accented voice phonetically: "Zose idea istat der argentrens
on a cloud machine…".

### What this means

- **Latency.** `analyzer` is fast: about 90 ms for short clips and about 0.75 s for 30 s of
  audio, warm, with essentially no setup. `dictation` is a little faster on short clips but
  slower on long ones (about 1.2 s; p90 about 1 s).
- **Accuracy without hints.** `analyzer` gives the best plain transcript: real punctuation and
  sentence casing, "Set the time out to 30 seconds". But it can't be biased, so jargon recall
  stays at 24 % (Chronics, super base, Jit Lab, Dequeis).
- **Accuracy with hints.** `dictation` with contextual strings more than doubles jargon recall
  (34 → 79 of 140): Steinhauer, MailPulse, Zephyrix, Claude Code, TypeScript, shadcn. The cost is
  sparse punctuation (often no final period, long run-on sentences on long clips), more
  substitutions, and occasional etiquette/LM artefacts ("Yes, shit it", "Set a timer for").
- **Noise.** Neither analyzer mode hallucinated on silence, hiss, hum, typing or breath. The only
  noise output was `dictation` returning "No" for `chord-3s`.

- **Legacy.** Untested so far. The disclaimed re-exec and the permission dialog work (exit 3
  with instructions, no crash), but nobody clicked Allow during the two test waits (2 min and
  10 min). After `run --authorize`, run `--api legacy` through the bench. It uses the same
  dictation model as `--api dictation`, so expect similar accuracy.

For the Whisper comparison, run the bench's own scorer on `analyzer` and on `dictation` with
hints. The numbers above are only a first read on a TTS corpus. Real-voice tiers (`lab`,
`history`) may rank the engines differently.
