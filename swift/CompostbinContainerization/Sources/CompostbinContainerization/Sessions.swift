//===----------------------------------------------------------------------===//
// The VMs this process owns, and the processes running in them.
//
// A `LinuxContainer` dies with the process that created it, so the registry is
// what makes `run` the owner of a session rather than a caller into a daemon:
// `boot` puts one here, every later `exec` finds it, and the process exiting is
// what stops it.
//
// Processes are held for the same reason on a smaller scale: a resize has to
// reach the `LinuxProcess` that owns the guest pty, and the SIGWINCH that
// prompts it arrives on a different call than the exec did.
//===----------------------------------------------------------------------===//

import Containerization
import ContainerizationOCI
import Synchronization

struct Booted {
    /// Held because dropping it would drop the network interface with it.
    let manager: ContainerManager
    let container: LinuxContainer
    /// The image's own process configuration — its user above all.
    ///
    /// `ContainerManager.create` seeds the container's first process from this,
    /// but `LinuxContainer.exec` starts from a bare configuration that runs as
    /// root. Every later attach has to seed itself, so the image config has to
    /// outlive the boot that read it.
    let imageConfig: ImageConfig?
}

/// Bridged calls arrive on Rust threads, one per attached terminal, so this is
/// reachable from several at once.
final class Sessions: Sendable {
    static let shared = Sessions()

    private let booted = Mutex<[String: Booted]>([:])
    /// Keyed by exec id, which the bridge makes unique per attach.
    private let processes = Mutex<[String: LinuxProcess]>([:])

    func insert(_ name: String, _ session: Booted) {
        booted.withLock { $0[name] = session }
    }

    func get(_ name: String) -> Booted? {
        booted.withLock { $0[name] }
    }

    func insert(process: LinuxProcess, id: String) {
        processes.withLock { $0[id] = process }
    }

    func process(_ id: String) -> LinuxProcess? {
        processes.withLock { $0[id] }
    }

    func removeProcess(_ id: String) {
        _ = processes.withLock { $0.removeValue(forKey: id) }
    }
}
