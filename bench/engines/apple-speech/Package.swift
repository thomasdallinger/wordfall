// swift-tools-version: 6.0
// Wordfall bench engine: Apple on-device speech recognition (SpeechAnalyzer / SFSpeechRecognizer).
// Requires the macOS 26 SDK (Xcode 26 or Command Line Tools for Xcode 26.x).
import PackageDescription

let package = Package(
    name: "AppleSpeechEngine",
    platforms: [.macOS("26.0")],
    targets: [
        .executableTarget(
            name: "apple-speech-engine",
            path: "Sources/AppleSpeechEngine",
            swiftSettings: [.swiftLanguageMode(.v5)],
            linkerSettings: [
                // Embed Info.plist (bundle id + NSSpeechRecognitionUsageDescription) into the
                // Mach-O so TCC can attribute the speech-recognition permission to this binary.
                .unsafeFlags([
                    "-Xlinker", "-sectcreate",
                    "-Xlinker", "__TEXT",
                    "-Xlinker", "__info_plist",
                    "-Xlinker", "\(Context.packageDirectory)/Resources/Info.plist",
                ]),
            ]
        ),
    ]
)
