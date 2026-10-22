import Foundation
import Speech

/// SFSpeechRecognizer forced on-device (same model family as system dictation).
final class LegacyEngine: SpeechEngine {
    let requestedLocale: Locale
    let useContext: Bool
    let debug: Bool
    let warmup: URL?
    let authTimeout: Int
    private var recognizer: SFSpeechRecognizer!
    private let queue: OperationQueue = {
        let q = OperationQueue()
        q.name = "apple-speech.legacy"
        q.maxConcurrentOperationCount = 1
        return q
    }()

    init(locale: Locale, useContext: Bool, debug: Bool, warmup: URL?, authTimeout: Int) {
        self.authTimeout = authTimeout
        self.requestedLocale = locale
        self.useContext = useContext
        self.debug = debug
        self.warmup = warmup
    }

    var engineName: String { "apple-speech-legacy" }
    var modelDescription: String { "SFSpeechRecognizer \(requestedLocale.identifier) on-device dictation" }

    func prepare() async throws -> [String: Any] {
        let loadStart = ContinuousClock.now
        try await Authorization.ensureSpeechRecognition(timeoutSeconds: authTimeout)
        guard let r = SFSpeechRecognizer(locale: requestedLocale) else {
            throw EngineError("SFSpeechRecognizer does not support locale \(requestedLocale.identifier)")
        }
        guard r.supportsOnDeviceRecognition else {
            throw EngineError("on-device recognition is not available for \(requestedLocale.identifier); "
                + "enable Dictation for this language in System Settings > Keyboard > Dictation so the model is downloaded")
        }
        r.queue = queue
        r.defaultTaskHint = .dictation
        recognizer = r
        log("api=legacy locale=\(r.locale.identifier) available=\(r.isAvailable) context=\(useContext)")
        let loadMs = elapsedMs(since: loadStart)

        var warmupMs = 0
        if let warmup {
            let res = try await transcribe(url: warmup, context: [])
            warmupMs = res.ms
            log("warm-up (\(warmup.lastPathComponent)): \(res.ms) ms \(res.text.debugDescription)")
        }
        return ["modelMs": loadMs, "warmupMs": warmupMs]
    }

    func transcribe(url: URL, context: [String]) async throws -> ClipResult {
        let request = SFSpeechURLRecognitionRequest(url: url)
        request.requiresOnDeviceRecognition = true
        request.addsPunctuation = true
        request.shouldReportPartialResults = false
        request.taskHint = .dictation
        if useContext { request.contextualStrings = context }

        let start = ContinuousClock.now
        let text: String = try await withCheckedThrowingContinuation { cont in
            let once = Once()
            _ = recognizer.recognitionTask(with: request) { result, error in
                if let result, result.isFinal {
                    once.run { cont.resume(returning: result.bestTranscription.formattedString) }
                } else if let error {
                    if Self.isNoSpeech(error) {
                        once.run { cont.resume(returning: "") }
                    } else {
                        once.run { cont.resume(throwing: error) }
                    }
                }
            }
        }
        return ClipResult(text: normalizeSpaces(text), ms: elapsedMs(since: start))
    }

    /// "No speech detected" is the expected answer for silence/noise clips, not a failure.
    private static func isNoSpeech(_ error: Error) -> Bool {
        let ns = error as NSError
        return (ns.domain == "kAFAssistantErrorDomain" && (ns.code == 1110 || ns.code == 203))
            || ns.localizedDescription.localizedCaseInsensitiveContains("no speech")
    }
}

/// Guards a continuation against double resumption from the recognizer callback.
final class Once: @unchecked Sendable {
    private var done = false
    private let lock = NSLock()
    func run(_ body: () -> Void) {
        lock.lock()
        defer { lock.unlock() }
        guard !done else { return }
        done = true
        body()
    }
}
