//===----------------------------------------------------------------------===//
// Building an image.
//
// Pull the base, unpack it to a writable ext4 block, boot it, run each step as an
// exec, export the block back to a tar, and ingest that tar as a single-layer
// image. All of it is Containerization API, in this process, with nothing to start
// first.
//
// The image is one layer however many steps went into it. The cache is block
// snapshots, not layers: a rebuild resumes from the deepest one still matching.
// See `Cache.swift`.
//
// A build also removes stale builder rootfs and unreferenced blobs.
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
import Synchronization
import SystemPackage

/// A step's shell script, and who runs it.
struct BuildStep: Decodable {
    /// What the build log calls it.
    var name: String
    /// The guest user, as the image names it. Root when absent — which is what
    /// `packages` and `run_as_root` need.
    var user: String?
    var script: String
    /// Unsalted; see `Keys`.
    var cacheKey: String
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
    var baseKey: String
    /// False for `--no-cache`. Snapshots are written either way.
    var useCache: Bool
}

enum Build {
    /// Where a build's context appears in the guest. Must match the engine's
    /// `CONTEXT_MOUNT`, which the cache keys match on.
    static let contextDestination = "/mnt/compostbin-context"
    static let cacheDirectory = "build-cache"

    static func run(_ plan: BuildPlan) async throws {
        guard Entitlement.hasVirtualization else {
            throw BridgeError.unentitled
        }

        let root = URL(filePath: plan.storeRoot)
        // Our own, because `ImageStore.contentStore` is internal to
        // Containerization and the ingest below needs it. The same directory the
        // store would have created for itself, so the blobs land where every
        // other reader looks for them.
        let contentStore = try LocalContentStore(path: root.appending(path: "content"))
        let imageStore = try ImageStore(path: root, contentStore: contentStore)
        let platform = Platform.current

        // Pulled here rather than left to `create`, because the base's own
        // config is what the finished image inherits, and its digest salts
        // every cache key.
        let base = try await imageStore.get(reference: plan.base, pull: true)
        let baseConfig = try? await base.config(for: platform).config
        let environment = merge(baseConfig?.env ?? [], plan.environment)

        let cache = Cache(root: root.appending(path: cacheDirectory))
        let keys = Keys(plan: plan, baseDigest: base.digest)

        // Nothing changed: re-tag, skipping the export.
        if plan.useCache, let descriptor = cache.image(keys.image),
            await Cache.holds(descriptor, in: contentStore)
        {
            note("\(plan.tag) is already built")
            try? await imageStore.delete(reference: plan.tag)
            try await imageStore.create(description: .init(reference: plan.tag, descriptor: descriptor))
            cache.evict()
            return
        }

        let containerDirectory = root.appending(components: "containers", plan.name)
        let rootfsPath = containerDirectory.appending(path: "rootfs.ext4")

        try? FileManager.default.removeItem(at: containerDirectory)
        sweepBuilders(in: root, keeping: plan.name)
        try FileManager.default.createDirectory(at: containerDirectory, withIntermediateDirectories: true)
        markBuilder(containerDirectory)

        let start = try await prepare(
            rootfs: rootfsPath,
            plan: plan,
            keys: keys,
            cache: cache,
            base: base,
            platform: platform
        )

        if start < plan.steps.endIndex {
            try await run(
                steps: start..<plan.steps.endIndex,
                of: plan,
                on: rootfsPath,
                keys: keys,
                cache: cache,
                base: base,
                imageStore: imageStore,
                environment: environment
            )
        }

        let descriptor = try await ingest(
            rootfs: rootfsPath,
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

        cache.save(image: descriptor, as: keys.image)

        // Not `manager.delete`: there is no manager on a fully cached build, and
        // its only extra is releasing a network interface, of which there is none.
        try? FileManager.default.removeItem(at: containerDirectory)

        await reclaim(imageStore)
        cache.evict()
    }

    /// Puts a rootfs at `rootfs` and returns the index of the first step to run:
    /// from the deepest cached step, else the cached unpacked base, else a fresh
    /// unpack.
    private static func prepare(
        rootfs: URL,
        plan: BuildPlan,
        keys: Keys,
        cache: Cache,
        base: Containerization.Image,
        platform: Platform
    ) async throws -> Int {
        if plan.useCache {
            for index in plan.steps.indices.reversed() where cache.holdsRootfs(keys.steps[index]) {
                note("cached through \(plan.steps[index].name)")
                try cache.restore(keys.steps[index], to: rootfs)

                return index + 1
            }

            if cache.holdsRootfs(keys.base) {
                try cache.restore(keys.base, to: rootfs)

                return 0
            }
        }

        let unpacker = EXT4Unpacker(capacityInBytes: plan.rootfsSizeInBytes)

        _ = try await unpacker.unpack(base, for: platform, at: rootfs)
        cache.save(rootfs: rootfs, as: keys.base)

        return 0
    }

    /// Runs the remaining steps, snapshotting the rootfs after each.
    ///
    /// One container per step: the block is only consistent once `stop` has
    /// unmounted it in the guest, so each snapshot costs a boot.
    private static func run(
        steps: Range<Int>,
        of plan: BuildPlan,
        on rootfs: URL,
        keys: Keys,
        cache: Cache,
        base: Containerization.Image,
        imageStore: ImageStore,
        environment: [String]
    ) async throws {
        let kernel = Kernel(path: URL(filePath: plan.kernelPath), platform: .linuxArm)
        var manager = try await ContainerManager(
            kernel: kernel,
            initfsReference: plan.initfsReference,
            imageStore: imageStore,
            network: nil
        )

        let mounts: [Containerization.Mount] =
            plan.context.map { [.share(source: $0, destination: contextDestination, options: ["ro"])] } ?? []
        let interface = NATInterface(
            ipv4Address: try CIDRv4(plan.ipv4Address),
            ipv4Gateway: try IPv4Address(plan.ipv4Gateway)
        )

        let block = Containerization.Mount.block(
            format: "ext4",
            source: rootfs.absolutePath(),
            destination: "/",
            options: []
        )

        for index in steps {
            let container = try await manager.create(
                plan.name,
                image: base,
                rootfs: block,
                networking: false
            ) { config in
                config.cpus = plan.cpus
                config.memoryInBytes = plan.memoryInBytes
                // A keepalive, exactly as a session's boot process is: the steps
                // are execs, and each one needs the container to outlive it. The
                // base image's own `Cmd` would exit immediately.
                config.process.arguments = ["/bin/sh", "-c", "while :; do sleep 86400; done"]
                config.process.user = .init()
                config.process.workingDirectory = "/"
                config.process.environmentVariables = environment
                config.mounts += mounts
                // `apt-get` and `claude.ai/install.sh` need the network, so a
                // build gets the same NAT a session does.
                config.interfaces = [interface]
                config.dns = DNS(nameservers: [plan.ipv4Gateway])
            }

            try await container.create()
            try await container.start()

            do {
                try await step(plan.steps[index], index: index, in: container, environment: environment)
            } catch {
                // Left for inspection, not cached; the next build sweeps it.
                try? await container.stop()
                throw error
            }

            try await container.stop()

            cache.save(rootfs: rootfs, as: keys.steps[index])
        }
    }

    /// Marks a `containers` directory as a builder's, not a session's. Holds the
    /// owning build's pid.
    private static let builderMarker = ".compostbin-builder"

    private static func markBuilder(_ directory: URL) {
        try? Data("\(getpid())\n".utf8).write(to: directory.appending(path: builderMarker))
    }

    /// Removes rootfs left by failed builds and by tags since renamed.
    ///
    /// Only marked directories — sessions share `containers` — and only when the
    /// owning pid is gone, since builds in other projects share this store.
    private static func sweepBuilders(in root: URL, keeping current: String) {
        let containers = root.appending(path: "containers")
        let directories =
            (try? FileManager.default.contentsOfDirectory(at: containers, includingPropertiesForKeys: nil)) ?? []

        for directory in directories where directory.lastPathComponent != current {
            let marker = directory.appending(path: builderMarker)

            guard let owner = try? String(contentsOf: marker, encoding: .utf8) else {
                continue
            }

            // Signal 0: does the process exist.
            if let pid = pid_t(owner.trimmingCharacters(in: .whitespacesAndNewlines)), kill(pid, 0) == 0 {
                continue
            }

            note("removing the rootfs left by \(directory.lastPathComponent)")
            try? FileManager.default.removeItem(at: directory)
        }
    }

    /// Deletes blobs no image references — above all the previous build's
    /// multi-gigabyte layer, orphaned by the re-tag.
    ///
    /// Only after `create`: until then this build's own blobs are unreferenced.
    /// Failure is logged, not thrown; the image is already usable.
    private static func reclaim(_ imageStore: ImageStore) async {
        do {
            let (deleted, freed) = try await imageStore.cleanUpOrphanedBlobs()

            guard !deleted.isEmpty else {
                return
            }

            let size = ByteCountFormatter.string(fromByteCount: Int64(freed), countStyle: .file)

            note("reclaimed \(size) from \(deleted.count) unreferenced blob\(deleted.count == 1 ? "" : "s")")
        } catch {
            note("could not reclaim unreferenced blobs: \(error)")
        }
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
        let log = FileWriter(FileHandle.standardError)

        log.line("--> \(step.name)")

        let process = try await container.exec("build-\(index)") { config in
            config.arguments = ["/bin/bash", "-euo", "pipefail", "-c", step.script]
            config.environmentVariables = environment
            config.workingDirectory = "/"
            config.user = step.user.map { User(username: $0) } ?? User()
            config.stdout = log
            config.stderr = log
        }

        try await process.start()
        let status = try await process.wait()
        try? await process.delete()

        guard status.exitCode == 0 else {
            throw BridgeError.stepFailed(step.name, status.exitCode)
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
        let layer = rootfs.deletingLastPathComponent().appending(path: "layer.tar")
        try? FileManager.default.removeItem(at: layer)

        let reader = try EXT4.EXT4Reader(blockDevice: FilePath(rootfs.path(percentEncoded: false)))
        try reader.export(archive: FilePath(layer.path(percentEncoded: false)))

        let index = Box<Descriptor>()
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
            throw BridgeError.notIngested(plan.tag)
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
/// Locked so that stdout and stderr chunks never interleave mid-write.
private final class FileWriter: Writer, Sendable {
    private let handle: Mutex<FileHandle>

    init(_ handle: FileHandle) {
        self.handle = Mutex(handle)
    }

    func write(_ data: Data) throws {
        try handle.withLock { try $0.write(contentsOf: data) }
    }

    func line(_ text: String) {
        try? write(Data("\(text)\n".utf8))
    }

    /// The handle is this process's stderr, which outlives every build.
    func close() throws {}
}

/// Somewhere for an escaping closure to leave its result. `ingest` takes a
/// `@Sendable` body and has nothing to return through, and a `Mutex` alone
/// cannot be captured by one.
private final class Box<Value: Sendable>: Sendable {
    private let stored = Mutex<Value?>(nil)

    var value: Value? {
        get { stored.withLock { $0 } }
        set { stored.withLock { $0 = newValue } }
    }
}
