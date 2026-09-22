//===----------------------------------------------------------------------===//
// The VMs this process owns, and the processes running in them.
//
// A `LinuxContainer` dies with the process that created it, so `run` owns its
// session with no daemon: `boot` registers it here, later `exec`s find it, and
// process exit stops it.
//
// Processes are held because a resize must reach the `LinuxProcess` owning the
// guest pty, and the SIGWINCH arrives on a different call than the exec.
//===----------------------------------------------------------------------===//

import Containerization
import ContainerizationOCI
import Synchronization

struct Booted {
    /// Held because dropping it drops the network interface.
    let manager: ContainerManager
    let container: LinuxContainer
    /// The image's process configuration, chiefly its user.
    ///
    /// `ContainerManager.create` seeds the first process from this, but
    /// `LinuxContainer.exec` starts from a bare config running as root, so each
    /// attach seeds itself from this copy.
    let imageConfig: ImageConfig?
}

/// Accessed concurrently: bridged calls arrive on one Rust thread per attached
/// terminal.
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

    func remove(process id: String) {
        _ = processes.withLock { $0.removeValue(forKey: id) }
    }
}
