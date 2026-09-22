//===----------------------------------------------------------------------===//
// Booting a session's VM, and running processes in it.
//
// Modeled on `cctl`'s RunCommand, except: the image is read from the store, not
// pulled; the boot process is a keepalive, not the workload; and a process
// attaches to whichever terminal asked (the owner's, or one a joining caller
// passed over the control socket).
//===----------------------------------------------------------------------===//

import Containerization
import ContainerizationExtras
import ContainerizationOCI
import ContainerizationOS
import Foundation
// For `FilePermissions` (relayed socket mode).
import SystemPackage

/// What `RunSpec` carries, flattened for the bridge.
struct BootSpec {
    var name: String
    var storeRoot: String
    var kernelPath: String
    var initfsReference: String
    var imageReference: String
    var cpus: Int
    var memoryInBytes: UInt64
    /// Ceiling for the image's unpacked rootfs, which is sparse.
    var rootfsCapacityInBytes: UInt64
    /// `source\tdestination\tro?`, one per line.
    var mounts: [String]
    /// `source\tdestination\tmode\tdirection`, one per line. Sockets are
    /// relayed, not mounted.
    var sockets: [String]
    var environment: [String]
    var arguments: [String]
    var workingDirectory: String
    /// The guest's address on the NAT network, as CIDR.
    var ipv4Address: String
    var ipv4Gateway: String
}

/// What `ExecSpec` carries.
struct ExecRequest {
    var name: String
    var id: String
    var arguments: [String]
    var environment: [String]
    /// The guest user, as the image names it. The image's default when absent.
    var user: String?
    var workingDirectory: String
    /// What a process on a terminal reports as `TERM`. Nil leaves it to the
    /// image, and is ignored by a process without one.
    var term: String?
    /// The terminal the process reads and is sized against, or `-1` for a
    /// caller that has none and attaches `stdin` instead.
    var terminal: Int32
    /// The caller's own streams. Each is `-1` when it leaves that stream
    /// unattached, and the guest reads or writes nothing on it. A process on a
    /// terminal still writes to `stdout`, wherever that points, and has no
    /// separate `stderr`.
    var stdin: Int32
    var stdout: Int32
    var stderr: Int32
}

/// Virtualization's built-in NAT, as a guest joins it.
///
/// Static, since nothing hands out leases: vminitd sets the address directly.
/// The Rust side chooses it; see `spec::nat_address`.
struct NAT {
    let interface: NATInterface
    let gateway: String

    init(address: String, gateway: String) throws {
        interface = NATInterface(ipv4Address: try CIDRv4(address), ipv4Gateway: try IPv4Address(gateway))
        self.gateway = gateway
    }

    /// Sets the guest's only interface, resolving through the gateway.
    func join(_ config: inout LinuxContainer.Configuration) {
        config.interfaces = [interface]
        config.dns = DNS(nameservers: [gateway])
    }
}

/// The store's `containers` directory, shared by sessions and builders.
func containers(in root: URL) -> URL {
    root.appending(path: "containers")
}

/// A container's directory (its rootfs and the library's boot log) and the
/// rootfs within it.
func container(_ name: String, in root: URL) -> (directory: URL, rootfs: URL) {
    let directory = containers(in: root).appending(path: name)

    return (directory, directory.appending(path: "rootfs.ext4"))
}

enum Session {
    static func boot(_ spec: BootSpec) async throws {
        // First, so an unsigned build says so instead of failing in whichever
        // Virtualization call comes first, with an error that never names the
        // cause.
        guard Entitlement.hasVirtualization else {
            throw BridgeError.unentitled
        }

        let root = URL(filePath: spec.storeRoot)
        let kernel = Kernel(path: URL(filePath: spec.kernelPath), platform: .linuxArm)

        // No `Network`; the interface is built below. `VmnetNetwork` (what
        // `cctl` uses) fails with VMNET_MEM_FAILURE from an unprivileged
        // process, which is why the `container` CLI runs vmnet as a separate
        // helper. Virtualization's own NAT needs no extra privilege.
        var manager = try await ContainerManager(
            kernel: kernel,
            initfsReference: spec.initfsReference,
            root: root,
            network: nil
        )

        // Not `create(reference:)`, which unpacks a new rootfs every run. On
        // this path we must create the container directory; the library writes
        // its boot log there.
        let image = try await manager.imageStore.get(reference: spec.imageReference)
        let paths = container(spec.name, in: root)
        try FileManager.default.createDirectory(at: paths.directory, withIntermediateDirectories: true)
        let rootfs = try await Unpacked(store: root, capacityInBytes: spec.rootfsCapacityInBytes)
            .rootfs(for: image, at: paths.rootfs)

        let mounts = try spec.mounts.map(share)
        let sockets = try spec.sockets.map(relay)
        let nat = try NAT(address: spec.ipv4Address, gateway: spec.ipv4Gateway)

        // `networking: false`: the manager has no `Network` to allocate from,
        // and the interface is set here.
        let container = try await manager.create(
            spec.name,
            image: image,
            rootfs: rootfs,
            networking: false
        ) { config in
            config.cpus = spec.cpus
            config.memoryInBytes = spec.memoryInBytes
            config.process.arguments = spec.arguments
            config.process.workingDirectory = spec.workingDirectory
            config.process.environmentVariables += spec.environment
            config.mounts += mounts
            config.sockets = sockets
            nat.join(&config)
        }

        try await container.create()
        try await container.start()

        // Kept for `exec`, which needs the image's user.
        let imageConfig = try? await image.config(for: .current).config

        Sessions.shared.insert(
            spec.name,
            Booted(manager: manager, container: container, imageConfig: imageConfig)
        )
    }

    /// `source\tdestination\tmode\tdirection`, as `wire::sockets` writes it,
    /// the mode in octal.
    ///
    /// The mode is the caller's: 0666 lets an unprivileged guest user open a
    /// socket the guest owns as root, and a caller that knows its guest user
    /// can narrow it. Confinement comes from the directory, not the mode.
    private static func relay(_ socket: String) throws -> UnixSocketConfiguration {
        let parts = try fields(socket, count: 4, kind: "socket")

        // `CModeT` is 16-bit, so a mode that doesn't fit is malformed rather
        // than silently truncated to a wider one the caller meant.
        guard let mode = CModeT(parts[2], radix: 8) else {
            throw BridgeError.malformed("socket mode", String(parts[2]))
        }

        let direction: UnixSocketConfiguration.Direction =
            switch parts[3] {
            case "into": .into
            case "outof": .outOf
            default: throw BridgeError.malformed("socket direction", String(parts[3]))
            }

        return UnixSocketConfiguration(
            source: URL(filePath: String(parts[0])),
            destination: URL(filePath: String(parts[1])),
            permissions: FilePermissions(rawValue: mode),
            direction: direction
        )
    }

    /// `source\tdestination\tro?`, as `spec::mounts` writes it.
    private static func share(_ mount: String) throws -> Containerization.Mount {
        let parts = try fields(mount, count: 3, kind: "mount")

        return .share(
            source: String(parts[0]),
            destination: String(parts[1]),
            options: parts[2] == "ro" ? ["ro"] : []
        )
    }

    /// A tab-separated line's fields, throwing unless there are exactly `count`.
    private static func fields(_ line: String, count: Int, kind: String) throws -> [Substring] {
        let fields = line.split(separator: "\t", omittingEmptySubsequences: false)

        guard fields.count == count else {
            throw BridgeError.malformed(kind, line)
        }

        return fields
    }

    /// Runs a process to completion in an already-booted session and returns its
    /// exit code.
    ///
    /// Takes a descriptor rather than `Terminal.current` because a joining
    /// caller passes its tty over the control socket; from here the two cases
    /// are identical.
    static func exec(_ request: ExecRequest) async throws -> Int32 {
        guard let booted = Sessions.shared.get(request.name) else {
            throw BridgeError.notBooted(request.name)
        }

        let imageConfig = booted.imageConfig

        // `setInitState: false`: the caller set raw mode and restores it. The
        // descriptor is ours, a duplicate for this attach; closing it stops our
        // reads.
        let terminal = request.terminal < 0 ? nil : try Terminal(descriptor: request.terminal, setInitState: false)

        // Every descriptor is a duplicate made for this attach, and closes with
        // it; the caller keeps the streams they came from.
        let reader = handle(request.stdin).map(FileReader.init)
        let out = handle(request.stdout).map { FileWriter($0, owned: true) }
        let error = handle(request.stderr).map { FileWriter($0, owned: true) }

        // `defer`, so an error before the wait doesn't leave a reader on a
        // terminal someone is still typing at.
        defer {
            try? terminal?.close()
            reader?.close()
        }

        let process = try await booted.container.exec(request.id) { config in
            // Seeded from the image: a bare exec config runs as uid 0 with only
            // a default PATH, so an image ending in a non-root `USER` would
            // still run this process as root.
            if let imageConfig {
                let fallback = config.environmentVariables
                config = .init(from: imageConfig)

                // Seeding replaces the environment; keep the default PATH if the
                // image declares none.
                if !config.environmentVariables.contains(where: { $0.hasPrefix("PATH=") }) {
                    config.environmentVariables += fallback
                }
            }

            config.arguments = request.arguments
            // Ours last, so a variable the session sets beats the image's.
            config.environmentVariables += request.environment
            config.workingDirectory = request.workingDirectory

            if let user = request.user {
                config.user = User(username: user)
            }

            // Not `setTerminalIO`, which writes back to the terminal it reads.
            // The caller's stdout may be somewhere else entirely, and this is
            // what keeps a redirection its shell made. Its stderr has nowhere
            // else to go: one pty carries every stream, and Containerization
            // refuses a separate stderr beside `terminal`.
            if let terminal {
                config.terminal = true

                if let term = request.term {
                    config.environmentVariables.append("TERM=\(term)")
                }

                config.stdin = terminal
                config.stdout = out
            } else {
                config.stdin = reader
                config.stdout = out
                config.stderr = error
            }
        }

        Sessions.shared.insert(process: process, id: request.id)
        defer { Sessions.shared.remove(process: request.id) }

        try await process.start()

        if let terminal {
            try? await process.resize(to: try terminal.size)
        }

        let status = try await process.wait()
        try? await process.delete()

        return status.exitCode
    }

    /// Re-reads the size from the attached terminal and tells the guest.
    ///
    /// Takes the descriptor rather than a size, so a stale size can't race a
    /// second resize.
    static func resize(id: String, terminal: Int32) async throws {
        guard let process = Sessions.shared.process(id) else {
            // The process ended after the SIGWINCH; not an error.
            return
        }

        let size = try Terminal(descriptor: terminal, setInitState: false).size
        try await process.resize(to: size)
    }
}
