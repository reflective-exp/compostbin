//===----------------------------------------------------------------------===//
// Booting a session's VM, and running processes in it.
//
// Shaped after `cctl`'s RunCommand, with the differences that matter to
// compostbin: the image is read from the store rather than pulled, the boot
// process is a keepalive rather than the workload, and the terminal a process is
// attached to is whichever one asked — the owner's own, or one handed over a
// control socket by `compostbin shell`.
//===----------------------------------------------------------------------===//

import Containerization
import ContainerizationExtras
import ContainerizationOCI
import ContainerizationOS
import Foundation
// For `FilePermissions`, which is how a relayed socket's mode is set.
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
    /// `source\tdestination\tro?`, one per line.
    var mounts: [String]
    /// `source\tdestination`, one per line. Host sockets relayed in, not mounted.
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
    var workingDirectory: String
    /// The terminal to attach. `-1` runs the process without one.
    var terminal: Int32
}

enum Session {
    static func boot(_ spec: BootSpec) async throws {
        // First, so that an unsigned build says so rather than failing later in
        // whatever Virtualization call happens to come first — which moves
        // around as this code changes, and never names the cause.
        guard Entitlement.hasVirtualization else {
            throw BridgeError.unentitled
        }

        let kernel = Kernel(path: URL(filePath: spec.kernelPath), platform: .linuxArm)

        // No `Network`: the interface is built below instead. `VmnetNetwork`
        // is what `cctl` uses and what `ContainerManager` would allocate from,
        // but creating a vmnet network from an ordinary process fails with
        // VMNET_MEM_FAILURE — the privilege to do it is why the `container`
        // CLI runs its vmnet plugin as a separate helper. Virtualization's own
        // NAT needs nothing we do not already have.
        var manager = try await ContainerManager(
            kernel: kernel,
            initfsReference: spec.initfsReference,
            root: URL(filePath: spec.storeRoot),
            network: nil
        )

        let mounts = try spec.mounts.map(share)
        let sockets = try spec.sockets.map(relay)

        // Static, because nothing hands out a lease: `Interface` wants an
        // address up front and vminitd sets it directly. Which address is the
        // Rust side's decision — see `spec::nat_address`.
        let interface = NATInterface(
            ipv4Address: try CIDRv4(spec.ipv4Address),
            ipv4Gateway: try IPv4Address(spec.ipv4Gateway)
        )

        // `networking: false` leaves the interfaces alone: with no `Network` on
        // the manager there is nothing for it to allocate, and ours is set here.
        let container = try await manager.create(
            spec.name,
            reference: spec.imageReference,
            networking: false
        ) { config in
            config.cpus = spec.cpus
            config.memoryInBytes = spec.memoryInBytes
            config.process.arguments = spec.arguments
            config.process.workingDirectory = spec.workingDirectory
            config.process.environmentVariables += spec.environment
            config.mounts += mounts
            config.sockets = sockets
            config.interfaces = [interface]
            config.dns = DNS(nameservers: [spec.ipv4Gateway])
        }

        try await container.create()
        try await container.start()

        // Read back rather than threaded through `create`: the reference is
        // already in the store by now, and `exec` needs the user it names.
        let image = try await manager.imageStore.get(reference: spec.imageReference, pull: false)
        let imageConfig = try? await image.config(for: .current).config

        Sessions.shared.insert(
            spec.name,
            Booted(manager: manager, container: container, imageConfig: imageConfig)
        )
    }

    /// `source\tdestination` — the wire form of
    /// `apple_container::model::SocketRelay`.
    ///
    /// `.into`, always: compostbin's ports are host services the guest reaches,
    /// never the other way round.
    ///
    /// The mode is the same 0666 the CLI path ends up with. There it is
    /// incidental — the host socket's mode copied verbatim onto a guest socket
    /// owned by root, which the unprivileged `claude` could not otherwise open.
    /// Here it is a choice, and a narrower one becomes possible as soon as the
    /// guest-side owner is known. The confinement is the session directory
    /// either way, not the mode.
    private static func relay(_ socket: String) throws -> UnixSocketConfiguration {
        let fields = socket.split(separator: "\t", omittingEmptySubsequences: false)

        guard fields.count == 2 else {
            throw BridgeError.malformed("socket", socket)
        }

        return UnixSocketConfiguration(
            source: URL(filePath: String(fields[0])),
            destination: URL(filePath: String(fields[1])),
            permissions: FilePermissions(rawValue: 0o666),
            direction: .into
        )
    }

    /// `source\tdestination\tro?` — the wire form of `apple_container::model::Mount`.
    private static func share(_ mount: String) throws -> Containerization.Mount {
        let fields = mount.split(separator: "\t", omittingEmptySubsequences: false)

        guard fields.count == 3 else {
            throw BridgeError.malformed("mount", mount)
        }

        return .share(
            source: String(fields[0]),
            destination: String(fields[1]),
            options: fields[2] == "ro" ? ["ro"] : []
        )
    }

    /// Runs a process to completion in an already-booted session and returns its
    /// exit code.
    ///
    /// The terminal is a descriptor rather than `Terminal.current` because the
    /// caller is not always this process: `compostbin shell` passes its own tty
    /// across the control socket, and from here the two cases are identical.
    static func exec(_ request: ExecRequest) async throws -> Int32 {
        guard let booted = Sessions.shared.get(request.name) else {
            throw BridgeError.notBooted(request.name)
        }

        let imageConfig = booted.imageConfig

        // `setInitState: false`: the terminal's attributes belong to whoever
        // handed it over — they put it in raw mode and they must put it back.
        // The descriptor, though, is ours: it is a duplicate made for this
        // attach, and closing it below is what stops us reading.
        let terminal = request.terminal < 0 ? nil : try Terminal(descriptor: request.terminal, setInitState: false)

        // Not on the exit path alone: an error between here and the wait would
        // otherwise leave a reader on a terminal someone is still typing at.
        defer { try? terminal?.close() }

        let process = try await booted.container.exec(request.id) { config in
            // Seeded from the image, because `exec` is not: a bare
            // configuration runs as uid 0 with nothing but a default PATH,
            // which would attach Claude as root to an image whose whole point
            // is that it ends `USER claude`.
            if let imageConfig {
                let fallback = config.environmentVariables
                config = .init(from: imageConfig)

                // Seeding replaces the environment wholesale, and an image that
                // declares no PATH would leave the guest unable to find
                // anything. Debian's does; not every base image would.
                if !config.environmentVariables.contains(where: { $0.hasPrefix("PATH=") }) {
                    config.environmentVariables += fallback
                }
            }

            config.arguments = request.arguments
            // Ours last, so a variable the session sets beats the image's.
            config.environmentVariables += request.environment
            config.workingDirectory = request.workingDirectory

            if let terminal {
                config.setTerminalIO(terminal: terminal)
            }
        }

        Sessions.shared.insert(process: process, id: request.id)
        defer { Sessions.shared.removeProcess(request.id) }

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
    /// Takes the descriptor rather than a size: the fd *is* the terminal that
    /// changed, so asking it is both simpler and immune to a stale size racing
    /// a second resize.
    static func resize(id: String, terminal: Int32) async throws {
        guard let process = Sessions.shared.process(id) else {
            // The process ended between the SIGWINCH and this call. Nothing to
            // resize, and nothing wrong.
            return
        }

        let size = try Terminal(descriptor: terminal, setInitState: false).size
        try await process.resize(to: size)
    }
}
