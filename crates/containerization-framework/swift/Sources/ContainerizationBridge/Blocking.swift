//===----------------------------------------------------------------------===//
// Containerization is async throughout; the bridge is not.
//===----------------------------------------------------------------------===//

import Foundation

/// Runs an async body in a detached task and blocks until it finishes.
///
/// Rust calls in on its own threads, never on Swift's cooperative pool, so
/// blocking here cannot deadlock the executor the body runs on.
func blocking<T>(_ body: @escaping @Sendable () async throws -> T) throws -> T {
    let semaphore = DispatchSemaphore(value: 0)
    nonisolated(unsafe) var outcome: Result<T, any Error>?

    Task.detached {
        do {
            outcome = .success(try await body())
        } catch {
            outcome = .failure(error)
        }
        semaphore.signal()
    }

    semaphore.wait()

    guard let outcome else {
        throw BridgeError.noOutcome
    }

    return try outcome.get()
}

enum BridgeError: Error, CustomStringConvertible {
    /// Cannot happen: the semaphore is only signalled after `outcome` is set.
    case noOutcome
    /// A value the bridge could not parse: a malformed wire string, or a
    /// setting that never made sense.
    case malformed(String, String)
    /// An `exec` for a session this process does not hold, e.g. a joining
    /// caller reaching the wrong process. The control socket prevents it.
    case notBooted(String)
    /// The binary is not signed for virtualization, usually a rebuild that was
    /// not re-signed.
    case unentitled
    /// A build step exited non-zero. Carries the step's label so the message
    /// names what failed.
    case stepFailed(String, Int32)
    /// The ingest body left no index descriptor: a bug in `Build.ingest`.
    case notIngested(String)
    /// The kernel download returned a non-success status.
    case kernelUnavailable(String, Int)
    /// The downloaded archive lacks the kernel at the expected path: the
    /// release layout has changed.
    case kernelMissing(String)
    /// A rootfs could not be copied to or from the build cache. Filesystems
    /// without clones fall back to a copy and never land here.
    case notCloned(String, String)

    var description: String {
        switch self {
        case .noOutcome:
            return "the bridged task signalled completion without an outcome"
        case .malformed(let what, let value):
            return "malformed \(what): \(value.debugDescription)"
        case .notBooted(let name):
            return "this process does not own a session named \(name)"
        case .stepFailed(let label, let code):
            return "build step failed with exit code \(code): \(label)"
        case .notIngested(let reference):
            return "nothing was ingested for \(reference)"
        case .kernelUnavailable(let url, let status):
            return "downloading a kernel from \(url) answered \(status)"
        case .kernelMissing(let path):
            return "the downloaded archive has no \(path)"
        case .notCloned(let name, let reason):
            return "could not copy \(name): \(reason)"
        case .unentitled:
            return "this build is not signed for virtualization!"
        }
    }
}
