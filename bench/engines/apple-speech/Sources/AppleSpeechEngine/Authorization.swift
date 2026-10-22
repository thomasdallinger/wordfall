import Foundation
import Speech

/// Exit code used when a human has to grant (or re-grant) the speech-recognition permission.
let exitNeedsPermission: Int32 = 3

enum Authorization {
    static func statusName(_ s: SFSpeechRecognizerAuthorizationStatus) -> String {
        switch s {
        case .authorized: return "authorized"
        case .denied: return "denied"
        case .restricted: return "restricted"
        case .notDetermined: return "notDetermined"
        @unknown default: return "unknown(\(s.rawValue))"
        }
    }

    /// Ensures SFSpeechRecognizer authorization. If the system has to ask, it shows a one-time
    /// prompt; we wait a bounded time for the human, then exit with instructions instead of hanging.
    static func ensureSpeechRecognition(timeoutSeconds: Int = 60) async throws {
        let status = SFSpeechRecognizer.authorizationStatus()
        log("speech recognition authorization: \(statusName(status))")
        switch status {
        case .authorized:
            return
        case .denied, .restricted:
            instructions(status: status)
        case .notDetermined:
            log("requesting speech-recognition permission. A macOS dialog should appear now:")
            log("  -> click \"Allow\" (waiting up to \(timeoutSeconds) s)")
            let granted = await request(timeoutSeconds: timeoutSeconds)
            guard let granted else {
                log("no answer to the permission prompt within \(timeoutSeconds) s")
                instructions(status: .notDetermined)
            }
            if granted != .authorized { instructions(status: granted) }
            log("speech recognition authorized")
        @unknown default:
            instructions(status: status)
        }
    }

    private static func request(timeoutSeconds: Int) async -> SFSpeechRecognizerAuthorizationStatus? {
        await withCheckedContinuation { cont in
            let once = Once()
            SFSpeechRecognizer.requestAuthorization { s in once.run { cont.resume(returning: s) } }
            DispatchQueue.global().asyncAfter(deadline: .now() + .seconds(timeoutSeconds)) {
                once.run { cont.resume(returning: nil) }
            }
        }
    }

    private static func instructions(status: SFSpeechRecognizerAuthorizationStatus) -> Never {
        log("""
        ERROR: speech-recognition permission is \(statusName(status)) (only --api legacy needs it;
               --api analyzer / dictation work without it).
        One-time fix:
          1. In a terminal run:   bench/engines/apple-speech/run --authorize
             macOS shows a dialog asking whether "apple-speech-engine" may access Speech
             Recognition. Click "Allow". The tool prints "permission OK" and exits 0.
          2. If the status is "denied" (someone clicked "Don't Allow"), no dialog appears. Open
             System Settings > Privacy & Security > Speech Recognition, switch ON
             "apple-speech-engine", then re-run.
          3. "restricted" means a configuration profile (MDM) blocks speech recognition.
        The permission is stored for this binary's path; a rebuild may ask again.
        """)
        exit(exitNeedsPermission)
    }
}
