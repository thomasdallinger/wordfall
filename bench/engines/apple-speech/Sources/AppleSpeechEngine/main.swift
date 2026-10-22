import Foundation

// Wordfall bench external engine: Apple on-device speech recognition.
// Protocol (bench/README.md): ready line, then one JSON request per stdin line -> one JSON
// response per stdout line. Logs on stderr. Stdin EOF exits.

let usage = """
usage: apple-speech-engine [--api analyzer|dictation|legacy] [--locale en-US]
                           [--no-context] [--warmup PATH|none] [--authorize]
                           [--auth-timeout SECONDS] [--debug]

  --api analyzer   SpeechAnalyzer + SpeechTranscriber (macOS 26 long-form model)   [default]
  --api dictation  SpeechAnalyzer + DictationTranscriber (system-dictation model, uses context)
  --api legacy     SFSpeechRecognizer, requiresOnDeviceRecognition, addsPunctuation
  --no-context     ignore the request's `context` vocabulary hints
  --warmup         short WAV transcribed before the ready line (default: bundled warmup.wav)
  --authorize      only request speech-recognition permission (for --api legacy), then exit
  --auth-timeout   how long to wait for the human to answer the permission dialog
                   [default 60, or 300 with --authorize]
"""

var api = "analyzer"
var localeID = "en-US"
var useContext = true
var warmupArg: String?
var debug = false
var authorizeOnly = false
var authTimeout: Int?

var argv = CommandLine.arguments.dropFirst().makeIterator()
func value(_ flag: String) -> String {
    guard let v = argv.next() else { fail("\(flag) needs a value\n\(usage)", code: 64) }
    return v
}
while let arg = argv.next() {
    switch arg {
    case "--api": api = value(arg)
    case "--locale": localeID = value(arg)
    case "--no-context": useContext = false
    case "--warmup": warmupArg = value(arg)
    case "--debug": debug = true
    case "--authorize": authorizeOnly = true
    case "--auth-timeout": authTimeout = Int(value(arg))
    case "-h", "--help": print(usage); exit(0)
    default: fail("unknown argument \(arg)\n\(usage)", code: 64)
    }
}

// SFSpeechRecognizer needs TCC speech-recognition permission (SpeechAnalyzer modules do not).
if api == "legacy" || authorizeOnly { reexecDisclaimedIfNeeded() }

if authorizeOnly {
    try await Authorization.ensureSpeechRecognition(timeoutSeconds: authTimeout ?? 300)
    log("speech recognition permission OK")
    exit(0)
}

// Default warm-up clip ships next to the package: <pkg>/Resources/warmup.wav. The `run` script
// passes the absolute path via APPLE_SPEECH_WARMUP; fall back to a path relative to the binary.
func defaultWarmup() -> URL? {
    if let env = ProcessInfo.processInfo.environment["APPLE_SPEECH_WARMUP"] { return URL(fileURLWithPath: env) }
    let exe = URL(fileURLWithPath: CommandLine.arguments[0]).resolvingSymlinksInPath()
    // <pkg>/.build/<triple>/release/apple-speech-engine -> <pkg>/Resources/warmup.wav
    let candidate = exe.deletingLastPathComponent().appendingPathComponent("../../../Resources/warmup.wav").standardized
    return FileManager.default.fileExists(atPath: candidate.path) ? candidate : nil
}
let warmup: URL? = warmupArg == "none" ? nil : (warmupArg.map { URL(fileURLWithPath: $0) } ?? defaultWarmup())
if warmup == nil && warmupArg != "none" { log("warning: no warm-up clip found; first clip will be cold") }

let locale = Locale(identifier: localeID)
let engine: SpeechEngine
switch api {
case "analyzer":
    engine = AnalyzerEngine(kind: .speech, locale: locale, useContext: useContext,
                            debug: debug, warmup: warmup)
case "dictation":
    engine = AnalyzerEngine(kind: .dictation, locale: locale, useContext: useContext,
                            debug: debug, warmup: warmup)
case "legacy":
    engine = LegacyEngine(locale: locale, useContext: useContext, debug: debug, warmup: warmup,
                          authTimeout: authTimeout ?? 60)
default:
    fail("unknown --api \(api)\n\(usage)", code: 64)
}

let setupStart = ContinuousClock.now
var readyExtra: [String: Any]
do {
    readyExtra = try await engine.prepare()
} catch {
    fail("setup failed: \(describe(error))", code: 2)
}
let totalSetupMs = elapsedMs(since: setupStart)
var ready: [String: Any] = ["ready": true, "engine": engine.engineName, "model": engine.modelDescription,
                            "api": api, "locale": localeID, "setupMs": totalSetupMs]
ready.merge(readyExtra) { _, new in new }
ready["loadMs"] = (readyExtra["assetMs"] as? Int ?? 0) + (readyExtra["modelMs"] as? Int ?? 0)
log("ready in \(totalSetupMs) ms (\(readyExtra))")
emit(ready)

for try await line in FileHandle.standardInput.bytes.lines {
    let trimmed = line.trimmingCharacters(in: .whitespaces)
    if trimmed.isEmpty { continue }
    guard let req = Request(line: trimmed) else {
        log("bad request line: \(trimmed)")
        emit(["id": "", "error": "bad request: expected {\"id\",\"wav\",\"context\"}"])
        continue
    }
    do {
        guard FileManager.default.fileExists(atPath: req.wav) else { throw EngineError("wav not found: \(req.wav)") }
        let r = try await engine.transcribe(url: URL(fileURLWithPath: req.wav), context: req.context)
        var out: [String: Any] = ["id": req.id, "text": r.text, "ms": r.ms]
        out.merge(r.extra) { _, new in new }
        if debug { log("\(req.id): \(r.ms) ms \(r.text.debugDescription)") }
        emit(out)
    } catch {
        log("\(req.id): \(describe(error))")
        emit(["id": req.id, "error": describe(error)])
    }
}
log("stdin closed, exiting")
exit(0)
