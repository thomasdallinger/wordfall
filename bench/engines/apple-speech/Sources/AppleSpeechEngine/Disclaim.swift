import Darwin
import Foundation

/// TCC attributes privacy requests to the *responsible process*. For a CLI started from a
/// terminal (or from the bench), that is the terminal app / parent app, not this binary. If that
/// app's Info.plist lacks `NSSpeechRecognitionUsageDescription`, `SFSpeechRecognizer
/// .requestAuthorization` kills us with SIGABRT (observed: exit 134, tccd subject = terminal app).
///
/// Fix: re-exec ourselves with the responsibility "disclaimed" (what Xcode/LLDB do for debugged
/// apps). TCC then treats this binary as its own responsible process and reads the Info.plist
/// embedded in its `__TEXT,__info_plist` section (see Package.swift). Exec-in-place keeps the pid
/// and the stdin/stdout pipes the bench talks to.
///
/// `responsibility_spawnattrs_setdisclaim` is exported by libquarantine but has no public header,
/// so it is looked up at runtime; if missing we continue undisclaimed.
func reexecDisclaimedIfNeeded() {
    let marker = "APPLE_SPEECH_DISCLAIMED"
    guard getenv(marker) == nil else { return }

    typealias SetDisclaim = @convention(c) (UnsafeMutablePointer<posix_spawnattr_t?>, Int32) -> Int32
    guard let sym = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "responsibility_spawnattrs_setdisclaim") else {
        log("warning: responsibility_spawnattrs_setdisclaim not found; TCC will attribute requests to the parent app")
        return
    }
    let setDisclaim = unsafeBitCast(sym, to: SetDisclaim.self)

    var pathBuf = [CChar](repeating: 0, count: Int(MAXPATHLEN))
    var size = UInt32(pathBuf.count)
    guard _NSGetExecutablePath(&pathBuf, &size) == 0 else { return }

    var attr: posix_spawnattr_t?
    posix_spawnattr_init(&attr)
    defer { posix_spawnattr_destroy(&attr) }
    posix_spawnattr_setflags(&attr, Int16(POSIX_SPAWN_SETEXEC))
    guard setDisclaim(&attr, 1) == 0 else { return }

    setenv(marker, "1", 1)
    let args = CommandLine.arguments.map { strdup($0) } + [nil]
    var envp: [UnsafeMutablePointer<CChar>?] = []
    var e = environ
    while let p = e.pointee { envp.append(strdup(p)); e += 1 }
    envp.append(nil)

    var pid: pid_t = 0
    let rc = posix_spawn(&pid, pathBuf, nil, &attr, args, envp)
    // Only reached if the exec failed.
    unsetenv(marker)
    log("warning: disclaimed re-exec failed (\(String(cString: strerror(rc)))); continuing without it")
}
