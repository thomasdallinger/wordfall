import Foundation

struct ClipResult {
    var text: String
    /// Recognition time for this clip only (warm).
    var ms: Int
    /// Extra diagnostic fields appended to the response line (e.g. per-clip setup time).
    var extra: [String: Any] = [:]
}

protocol SpeechEngine: AnyObject {
    /// `engine` field of the ready line.
    var engineName: String { get }
    /// `model` field of the ready line.
    var modelDescription: String { get }
    /// One-time setup (authorization, assets, model load). Returns extra ready-line fields.
    func prepare() async throws -> [String: Any]
    func transcribe(url: URL, context: [String]) async throws -> ClipResult
}

struct EngineError: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

func describe(_ error: Error) -> String {
    if let e = error as? EngineError { return e.description }
    let ns = error as NSError
    return "\(ns.domain) \(ns.code): \(ns.localizedDescription)"
}
