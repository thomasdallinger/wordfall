import Foundation

/// Logs go to stderr only; stdout is reserved for the JSON-lines protocol.
func log(_ message: String) {
    FileHandle.standardError.write(Data(("[apple-speech] " + message + "\n").utf8))
}

/// Writes one JSON object as a single line to stdout (unbuffered).
func emit(_ object: [String: Any]) {
    guard var data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys, .withoutEscapingSlashes]) else {
        log("failed to encode response: \(object)")
        return
    }
    data.append(0x0A)
    FileHandle.standardOutput.write(data)
}

func elapsedMs(since start: ContinuousClock.Instant) -> Int {
    let d = ContinuousClock.now - start
    return Int((Double(d.components.seconds) * 1000 + Double(d.components.attoseconds) / 1e15).rounded())
}

func fail(_ message: String, code: Int32 = 1) -> Never {
    log("ERROR: " + message)
    exit(code)
}

struct Request {
    let id: String
    let wav: String
    let context: [String]

    init?(line: String) {
        guard let data = line.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let id = obj["id"] as? String,
              let wav = obj["wav"] as? String
        else { return nil }
        self.id = id
        self.wav = wav
        self.context = (obj["context"] as? [Any])?.compactMap { $0 as? String } ?? []
    }
}

/// Collapses whitespace runs and trims, so segment joins never produce double spaces.
func normalizeSpaces(_ s: String) -> String {
    s.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
}
