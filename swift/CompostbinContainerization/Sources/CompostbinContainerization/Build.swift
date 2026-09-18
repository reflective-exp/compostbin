//===----------------------------------------------------------------------===//
// Building an image.
//
// Pull the base, unpack it to a writable ext4 block, boot it, run each step as an
// exec, export the block back to a tar, and ingest that tar as a single-layer
// image. All of it is Containerization API, in this process, with nothing to start
// first.
//
// What it does not do: layer per step, or cache anything. A cache keyed on
// per-step snapshots of the block is the obvious next thing.
//
// The step that deserves suspicion is the export: a rootfs leaves as ext4 and
// comes back as a tar, so users, modes, symlinks, hardlinks and extended
// attributes all have to survive libarchive's pax format and EXT4Reader's reading
// of the inodes. A session run on the built image is what says they did.
//
// Unlike every other bridged call, the plan crosses as JSON rather than as
// newline-separated lines: a build step is arbitrary shell and may contain
// newlines, which the line encoding cannot carry.
//===----------------------------------------------------------------------===//

import Containerization
import ContainerizationEXT4
import ContainerizationExtras
import ContainerizationOCI
import Foundation
import SystemPackage

/// A step's shell script, and who runs it.
struct BuildStep: Decodable {
    /// What the build log calls it.
    var name: String
    /// The guest user, as the image names it. Root when absent — which is what
    /// `packages` and `run_as_root` need.
    var user: String?
    var script: String
}

struct BuildPlan: Decodable {
    /// The builder container's id. Its own directory under the store, removed
    /// before and after the build.
    var name: String
    var storeRoot: String
    var kernelPath: String
    var initfsReference: String
    /// The image every step runs on top of, registry-qualified.
    var base: String
    /// What the finished image is registered as.
    var tag: String
    var cpus: Int
    var memoryInBytes: UInt64
    var rootfsSizeInBytes: UInt64
    /// A host directory the steps can read, mounted read-only. This is what
    /// stands in for `COPY` — a step copies out of it.
    var context: String?
    var steps: [BuildStep]
    /// `NAME=VALUE`, written into the image's own config and visible to every
    /// step, as `ENV` is.
    var environment: [String]
    /// The user and directory the finished image runs as.
    var user: String?
    var workingDirectory: String?
    var ipv4Address: String
    var ipv4Gateway: String
}

enum Build {
    /// Where a build's context appears in the guest.
    static let contextDestination = "/mnt/compostbin-context"

    static func run(_ plan: BuildPlan) async throws {
        guard Entitlement.hasVirtualization else {
            throw BridgeError.unentitled
        }

        let root = URL(fileURLWithPath: plan.storeRoot)
        // Our own, because `ImageStore.contentStore` is internal to
        // Containerization and the ingest below needs it. The same directory the
        // store would have created for itself, so the blobs land where every
        // other reader looks for them.
        let contentStore = try LocalContentStore(path: root.appendingPathComponent("content"))
        let imageStore = try ImageStore(path: root, contentStore: contentStore)
        let platform = Platform.current

        let kernel = Kernel(path: URL(fileURLWithPath: plan.kernelPath), platform: .linuxArm)
        var manager = try await ContainerManager(
            kernel: kernel,
            initfsReference: plan.initfsReference,
            imageStore: imageStore,
            network: nil
        )

        // Pulled here rather than left to `create`, because the base's own
        // config is what the finished image inherits.
        let base = try await imageStore.get(reference: plan.base, pull: true)
        let baseConfig = try? await base.config(for: platform).config

        let containerDirectory =
            root
            .appendingPathComponent("containers")
            .appendingPathComponent(plan.name)
        // A previous build's rootfs, which `create` would refuse to overwrite.
        try? FileManager.default.removeItem(at: containerDirectory)

        let environment = Self.merge(baseConfig?.env ?? [], plan.environment)
        let mounts: [Containerization.Mount] =
            plan.context.map { [.share(source: $0, destination: contextDestination, options: ["ro"])] } ?? []
        let interface = NATInterface(
            ipv4Address: try CIDRv4(plan.ipv4Address),
            ipv4Gateway: try IPv4Address(plan.ipv4Gateway)
        )

        let cpus = plan.cpus
        let memoryInBytes = plan.memoryInBytes
        let gateway = plan.ipv4Gateway
        let stepEnvironment = environment

        let container = try await manager.create(
            plan.name,
            image: base,
            rootfsSizeInBytes: plan.rootfsSizeInBytes,
            networking: false
        ) { config in
            config.cpus = cpus
            config.memoryInBytes = memoryInBytes
            // A keepalive, exactly as a session's boot process is: the steps are
            // execs, and each one needs the container to outlive it. The base
            // image's own `Cmd` would exit immediately.
            config.process.arguments = ["/bin/sh", "-c", "while :; do sleep 86400; done"]
            config.process.user = .init()
            config.process.workingDirectory = "/"
            config.process.environmentVariables = stepEnvironment
            config.mounts.append(contentsOf: mounts)
            // `apt-get` and `claude.ai/install.sh` need the network, so a build
            // gets the same NAT a session does.
            config.interfaces = [interface]
            config.dns = DNS(nameservers: [gateway])
        }

        try await container.create()
        try await container.start()

        do {
            for (index, step) in plan.steps.enumerated() {
                try await Self.step(step, index: index, in: container, environment: environment)
            }
        } catch {
            // The rootfs is left behind on purpose: a failed step is worth
            // looking at, and the next build removes the directory anyway.
            try? await container.stop()
            throw error
        }

        try await container.stop()

        let descriptor = try await Self.ingest(
            rootfs: containerDirectory.appendingPathComponent("rootfs.ext4"),
            plan: plan,
            base: baseConfig,
            environment: environment,
            platform: platform,
            contentStore: contentStore
        )

        // A reference that already exists is the ordinary case — this is a
        // rebuild — and `create` will not replace one.
        try? await imageStore.delete(reference: plan.tag)
        try await imageStore.create(description: .init(reference: plan.tag, descriptor: descriptor))

        try? manager.delete(plan.name)
    }

    /// Runs one step to completion, throwing when it fails.
    ///
    /// `bash -euo pipefail` rather than Docker's `sh -c`: every script here is
    /// generated from a manifest or from compostbin's own Dockerfile, both of
    /// which already chain with `&&`, and a step that fails silently in the
    /// middle would be baked into the image.
    private static func step(
        _ step: BuildStep,
        index: Int,
        in container: LinuxContainer,
        environment: [String]
    ) async throws {
        let label = step.name
        let log = FileWriter(FileHandle.standardError)

        log.line("--> \(label)")

        let script = step.script
        let user = step.user

        let process = try await container.exec("build-\(index)") { config in
            config.arguments = ["/bin/bash", "-euo", "pipefail", "-c", script]
            config.environmentVariables = environment
            config.workingDirectory = "/"
            config.user = user.map { User(username: $0) } ?? User()
            config.stdout = log
            config.stderr = log
        }

        try await process.start()
        let status = try await process.wait()
        try? await process.delete()

        guard status.exitCode == 0 else {
            throw BridgeError.stepFailed(label, status.exitCode)
        }
    }

    /// Exports the built rootfs and writes it into the store as a single-layer
    /// image, returning the index descriptor the reference points at.
    ///
    /// The layer is an uncompressed tar, which is not what a registry would
    /// want but is exactly right here: nothing pushes this image, and an
    /// uncompressed blob makes the layer digest and the diffID the same value —
    /// so both are correct without a second pass over the bytes. (Note the
    /// standing `TODO` in Containerization's own `InitImage.create`, which
    /// writes a gzip layer's compressed digest as its diffID.)
    private static func ingest(
        rootfs: URL,
        plan: BuildPlan,
        base: ImageConfig?,
        environment: [String],
        platform: Platform,
        contentStore: ContentStore
    ) async throws -> Descriptor {
        let layer = rootfs.deletingLastPathComponent().appendingPathComponent("layer.tar")
        try? FileManager.default.removeItem(at: layer)

        let reader = try EXT4.EXT4Reader(blockDevice: FilePath(rootfs.path))
        try reader.export(archive: FilePath(layer.path))

        let index = Box<Descriptor>()
        let tag = plan.tag
        let user = plan.user ?? base?.user
        let workingDirectory = plan.workingDirectory ?? base?.workingDir
        let entrypoint = base?.entrypoint
        let command = base?.cmd

        try await contentStore.ingest { directory in
            let writer = try ContentWriter(for: directory)

            var result = try writer.create(from: layer)
            let layerDescriptor = Descriptor(
                mediaType: MediaTypes.imageLayer,
                digest: result.digest.digestString,
                size: result.size
            )
            let diffID = result.digest.digestString

            let config = ContainerizationOCI.Image(
                architecture: platform.architecture,
                os: platform.os,
                variant: platform.variant,
                config: ImageConfig(
                    user: user,
                    env: environment,
                    entrypoint: entrypoint,
                    cmd: command,
                    workingDir: workingDirectory,
                    labels: ["dev.compostbin.built-by": "containerization-framework-bridge"]
                ),
                rootfs: Rootfs(type: "layers", diffIDs: [diffID])
            )
            result = try writer.create(from: config)
            let configDescriptor = Descriptor(
                mediaType: MediaTypes.imageConfig,
                digest: result.digest.digestString,
                size: result.size
            )

            result = try writer.create(from: Manifest(config: configDescriptor, layers: [layerDescriptor]))
            let manifestDescriptor = Descriptor(
                mediaType: MediaTypes.imageManifest,
                digest: result.digest.digestString,
                size: result.size,
                platform: platform
            )

            result = try writer.create(from: Index(manifests: [manifestDescriptor]))
            index.value = Descriptor(
                mediaType: MediaTypes.index,
                digest: result.digest.digestString,
                size: result.size
            )
        }

        try? FileManager.default.removeItem(at: layer)

        guard let descriptor = index.value else {
            throw BridgeError.notIngested(tag)
        }

        return descriptor
    }

    /// `ENV` semantics: the plan's variables override the base's, and everything
    /// else the base declares is kept, in the base's order.
    private static func merge(_ base: [String], _ additions: [String]) -> [String] {
        func name(_ variable: String) -> String {
            String(variable.prefix(while: { $0 != "=" }))
        }

        let overridden = Set(additions.map(name))
        var merged = base.filter { !overridden.contains(name($0)) }

        merged.append(contentsOf: additions)

        return merged
    }
}

/// A build log, written to this process's stderr as the guest produces it.
private final class FileWriter: Writer, @unchecked Sendable {
    private let lock = NSLock()
    private let handle: FileHandle

    init(_ handle: FileHandle) {
        self.handle = handle
    }

    func write(_ data: Data) throws {
        lock.lock()
        defer { lock.unlock() }
        handle.write(data)
    }

    func line(_ text: String) {
        try? write(Data("\(text)\n".utf8))
    }

    /// The handle is this process's stderr, which outlives every build.
    func close() throws {}
}

/// Somewhere for an escaping closure to leave its result. `ingest` takes a
/// `@Sendable` body and has nothing to return through.
private final class Box<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Value?

    var value: Value? {
        get {
            lock.lock()
            defer { lock.unlock() }
            return stored
        }
        set {
            lock.lock()
            defer { lock.unlock() }
            stored = newValue
        }
    }
}
