import AVFoundation
import CoreMedia
import Foundation
import Speech

/// Collects the results of one module's result stream (one stream per clip).
///
/// Both presets used here disable volatile results, so every delivered result is the module's
/// answer for its range. `resultsFinalizationTime` is not relied on: DictationTranscriber reports
/// 0 for it even on its last result before the stream closes. If a module ever re-issues a range,
/// the newer result replaces the older ones it overlaps (results arrive in order).
actor ResultCollector {
    private var segments: [(range: CMTimeRange, text: String)] = []
    private(set) var error: Error?
    let debug: Bool

    init(debug: Bool) { self.debug = debug }

    func add(range: CMTimeRange, finalization: CMTime, text: String) {
        if debug {
            log(String(format: "  result [%.2f-%.2f] fin=%.2f %@",
                       range.start.seconds, range.end.seconds, finalization.seconds, text.debugDescription))
        }
        segments.removeAll { CMTimeCompare($0.range.start, range.start) >= 0 }
        segments.append((range, text))
    }

    func end(error: Error?) { self.error = error }

    var text: String { normalizeSpaces(segments.map(\.text).joined(separator: " ")) }
}

/// SpeechAnalyzer-based engine (macOS 26). `.speech` = SpeechTranscriber (new long-form model),
/// `.dictation` = DictationTranscriber (system-dictation model, documented to honour contextual strings).
///
/// Each clip gets a fresh SpeechAnalyzer + module. Models stay resident across analyzers via
/// `modelRetention: .processLifetime`, so this costs only a few ms (reported as `setupMs`).
/// Reusing one analyzer for several files (analyzeSequence(from:) + finalize(through:) per file)
/// is allowed by the API, but on macOS 26.5 every file after the first came back garbled
/// (leading words dropped, ", , the ," / "....."), so it is not used.
final class AnalyzerEngine: SpeechEngine {
    enum Kind: String { case speech, dictation }

    let kind: Kind
    let requestedLocale: Locale
    let useContext: Bool
    let debug: Bool
    let warmup: URL?

    private var locale: Locale
    private var format: AVAudioFormat?
    /// AnalysisContext is documented as shareable among analyzers; reusing the object while the
    /// hint list is unchanged avoids re-applying the vocabulary for every clip.
    private var cachedContext: (strings: [String], context: AnalysisContext)?

    init(kind: Kind, locale: Locale, useContext: Bool, debug: Bool, warmup: URL?) {
        self.kind = kind
        self.requestedLocale = locale
        self.locale = locale
        self.useContext = useContext
        self.debug = debug
        self.warmup = warmup
    }

    var engineName: String { kind == .speech ? "apple-speech-analyzer" : "apple-dictation-analyzer" }

    var modelDescription: String {
        let id = locale.identifier(.bcp47)
        switch kind {
        case .speech: return "SpeechTranscriber \(id) preset=transcription (on-device)"
        case .dictation: return "DictationTranscriber \(id) preset=shortDictation (on-device)"
        }
    }

    private static let options = SpeechAnalyzer.Options(priority: .userInitiated, modelRetention: .processLifetime)

    /// Builds a fresh module and starts draining its result stream into a fresh collector.
    private func makeModule() -> (any SpeechModule, ResultCollector, Task<Void, Never>) {
        let collector = ResultCollector(debug: debug)
        switch kind {
        case .speech:
            // `.transcription`: final results only (no volatile/fast results, no alternatives).
            // SpeechTranscriber always punctuates and capitalises; there is no option for it.
            let m = SpeechTranscriber(locale: locale, preset: .transcription)
            return (m, collector, Self.drain(m, into: collector) { String($0.text.characters) })
        case .dictation:
            // `.shortDictation`: shortForm hint + punctuation, final results only.
            let m = DictationTranscriber(locale: locale, preset: .shortDictation)
            return (m, collector, Self.drain(m, into: collector) { String($0.text.characters) })
        }
    }

    private static func drain<M: SpeechModule>(_ module: M, into collector: ResultCollector,
                                               text: @escaping @Sendable (M.Result) -> String) -> Task<Void, Never> {
        Task {
            do {
                for try await r in module.results {
                    await collector.add(range: r.range, finalization: r.resultsFinalizationTime, text: text(r))
                }
                await collector.end(error: nil)
            } catch {
                await collector.end(error: error)
            }
        }
    }

    func prepare() async throws -> [String: Any] {
        switch kind {
        case .speech:
            guard SpeechTranscriber.isAvailable else {
                throw EngineError("SpeechTranscriber is not available on this device")
            }
            guard let l = await SpeechTranscriber.supportedLocale(equivalentTo: requestedLocale) else {
                throw EngineError("locale \(requestedLocale.identifier) not supported by SpeechTranscriber")
            }
            locale = l
        case .dictation:
            guard let l = await DictationTranscriber.supportedLocale(equivalentTo: requestedLocale) else {
                throw EngineError("locale \(requestedLocale.identifier) not supported by DictationTranscriber")
            }
            locale = l
        }
        log("api=\(kind == .speech ? "analyzer" : "dictation") locale=\(locale.identifier(.bcp47)) context=\(useContext)")

        // 1. Model assets: downloaded once, system-wide, managed by AssetInventory.
        let assetStart = ContinuousClock.now
        let (probe, _, probeTask) = makeModule()
        probeTask.cancel()
        let status = await AssetInventory.status(forModules: [probe])
        log("asset status: \(status)")
        if let request = try await AssetInventory.assetInstallationRequest(supporting: [probe]) {
            log("downloading on-device speech model for \(locale.identifier(.bcp47)) (first run only) ...")
            let progress = request.progress
            let reporter = Task {
                var lastStep = -1
                while !Task.isCancelled {
                    let step = Int(progress.fractionCompleted * 10)
                    if step != lastStep { log("  model download \(step * 10)%"); lastStep = step }
                    try? await Task.sleep(for: .milliseconds(500))
                }
            }
            defer { reporter.cancel() }
            try await request.downloadAndInstall()
            log("  model installed")
        }
        let assetMs = elapsedMs(since: assetStart)

        // 2. Load the model into memory (kept for the process lifetime).
        let modelStart = ContinuousClock.now
        format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [probe])
        log("analyzer audio format: \(format.map { "\($0)" } ?? "nil (analyzer converts on the fly)")")
        let loader = SpeechAnalyzer(modules: [probe], options: Self.options)
        try await loader.prepareToAnalyze(in: format)
        await loader.cancelAndFinishNow()
        let modelMs = elapsedMs(since: modelStart)

        // 3. Warm-up on a short clip (not reported).
        var warmupMs = 0
        if let warmup {
            let r = try await transcribe(url: warmup, context: [])
            warmupMs = r.ms
            log("warm-up (\(warmup.lastPathComponent)): \(r.ms) ms \(r.text.debugDescription)")
        }
        return ["assetMs": assetMs, "modelMs": modelMs, "warmupMs": warmupMs, "assetStatus": "\(status)"]
    }

    private func analysisContext(for strings: [String]) -> AnalysisContext {
        if let cached = cachedContext, cached.strings == strings { return cached.context }
        let ctx = AnalysisContext()
        // Docs: keep phrases short (1-2 words), at most 100 in total.
        if !strings.isEmpty { ctx.contextualStrings = [.general: Array(strings.prefix(100))] }
        cachedContext = (strings, ctx)
        return ctx
    }

    func transcribe(url: URL, context: [String]) async throws -> ClipResult {
        // Per-clip setup (excluded from `ms`, reported as `setupMs`).
        let setupStart = ContinuousClock.now
        let (module, collector, consumer) = makeModule()
        let analysisContext = self.analysisContext(for: useContext ? context : [])
        let analyzer = SpeechAnalyzer(modules: [module], options: Self.options)
        try await analyzer.setContext(analysisContext)
        let contextMs = elapsedMs(since: setupStart)
        try await analyzer.prepareToAnalyze(in: format)
        if debug { log("  setup: setContext done at \(contextMs) ms, prepared at \(elapsedMs(since: setupStart)) ms") }
        // The bench WAVs (16 kHz mono Int16) already match the analyzer's preferred format;
        // analyzeSequence(from:) converts anything else internally.
        let file = try AVAudioFile(forReading: url)
        let setupMs = elapsedMs(since: setupStart)

        let start = ContinuousClock.now
        if let last = try await analyzer.analyzeSequence(from: file) {
            try await analyzer.finalizeAndFinish(through: last)
        } else {
            await analyzer.cancelAndFinishNow() // empty file
        }
        await consumer.value // result stream ends once the analyzer has finished
        let ms = elapsedMs(since: start)
        if let err = await collector.error { throw err }
        return ClipResult(text: await collector.text, ms: ms, extra: ["setupMs": setupMs])
    }
}
