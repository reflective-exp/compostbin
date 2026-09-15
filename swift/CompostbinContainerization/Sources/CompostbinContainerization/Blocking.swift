//===----------------------------------------------------------------------===//
// Containerization is async throughout; the bridge is not.
//===----------------------------------------------------------------------===//

import Foundation

/// Runs an async body to completion on a thread that is not ours, and blocks
/// until it finishes.
///
/// Every bridged function goes through this. Rust calls us on one of its own
/// threads — never on Swift's cooperative pool — so blocking here cannot
/// deadlock the executor the body runs on.
func blocking<T>(_ body: @escaping @Sendable () async throws -> T) throws -> T {
    let semaphore = DispatchSemaphore(value: 0)
    nonisolated(unsafe) var outcome: Result<T, Error>?

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
    /// A bridged string did not have the shape this side expects — a bug in the
    /// Rust encoder rather than anything a user did.
    case malformed(String, String)
    /// An `exec`, `resize` or `stop` for a session this process does not own.
    /// Ordinary when a second terminal reaches the wrong process; the control
    /// socket is what makes it not happen.
    case notBooted(String)
    /// The binary is not signed for virtualization. Almost always a rebuild
    /// that was not re-signed.
    case unentitled

    var description: String {
        switch self {
        case .noOutcome:
            return "the bridged task signalled completion without an outcome"
        case .malformed(let what, let value):
            return "malformed \(what): \(value.debugDescription)"
        case .notBooted(let name):
            return "this process does not own a session named \(name)"
        case .unentitled:
            return """
                this build is not signed for virtualization — run `bin/dev/sign \
                <binary>`, or `bin/dev/start`, which signs what it builds. \
                A signature does not survive a rebuild, so a plain `cargo build` \
                always lands here
                """
        }
    }
}
