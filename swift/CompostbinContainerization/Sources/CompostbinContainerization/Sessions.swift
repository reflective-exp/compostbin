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
import Foundation

struct Booted {
    /// Held because `delete` is on the manager, not the container, and because
    /// dropping it would drop the network interface with it.
    var manager: ContainerManager
    var container: LinuxContainer
    /// The image's own process configuration — its user above all.
    ///
    /// `ContainerManager.create` seeds the container's first process from this,
    /// but `LinuxContainer.exec` starts from a bare configuration that runs as
    /// root. Every later attach has to seed itself, so the image config has to
    /// outlive the boot that read it.
    var imageConfig: ImageConfig?
}

/// Bridged calls arrive on Rust threads, one per attached terminal, so this is
/// reachable from several at once.
final class Sessions: @unchecked Sendable {
    static let shared = Sessions()

    private let lock = NSLock()
    private var booted: [String: Booted] = [:]
    /// Keyed by exec id, which the bridge makes unique per attach.
    private var processes: [String: LinuxProcess] = [:]

    func insert(_ name: String, _ session: Booted) {
        lock.lock()
        defer { lock.unlock() }
        booted[name] = session
    }

    func get(_ name: String) -> Booted? {
        lock.lock()
        defer { lock.unlock() }
        return booted[name]
    }

    func remove(_ name: String) -> Booted? {
        lock.lock()
        defer { lock.unlock() }
        return booted.removeValue(forKey: name)
    }

    var names: [String] {
        lock.lock()
        defer { lock.unlock() }
        return Array(booted.keys)
    }

    func insert(process: LinuxProcess, id: String) {
        lock.lock()
        defer { lock.unlock() }
        processes[id] = process
    }

    func process(_ id: String) -> LinuxProcess? {
        lock.lock()
        defer { lock.unlock() }
        return processes[id]
    }

    func removeProcess(_ id: String) {
        lock.lock()
        defer { lock.unlock() }
        processes.removeValue(forKey: id)
    }
}
