//===----------------------------------------------------------------------===//
// Building an image.
//
// Pull the base, unpack it to a writable ext4 block, boot it, run each step as an
// exec, export the block to a tar, and ingest that as a single-layer image. All
// in-process through Containerization, with no daemon.
//
// The image is one layer regardless of step count. The cache is block
// snapshots, not layers: a rebuild resumes from the deepest matching one. See
// `Cache.swift`.
//
// A build also removes stale builder rootfs and unreferenced blobs.
//
// The export is the fragile step: users, modes, symlinks, hardlinks and extended
// attributes must survive EXT4Reader's inode reading and libarchive's pax
// format. Only a session on the built image verifies they did.
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
    /// The step's label in the build log.
    var name: String
    /// The guest user, as the image names it. Root when absent.
    var user: String?
    var script: String
    /// Unsalted; see `Keys`.
    var cacheKey: String
}

/// How this build treats its rootfs snapshots. Snapshots are written whether
/// or not they are read.
struct CachePolicy: Decodable {
    /// Resume from the deepest matching snapshot.
    var restore: Bool
    /// Snapshots kept, most recently used first.
    var keep: Int
    /// Anything unused this long goes regardless.
    var keepForSeconds: Double
}

/// A host directory shared into the builder, and where it lands in the guest.
struct BuildMount: Decodable {
    var source: String
    var destination: String
    var readonly: Bool
}

struct BuildPlan: Decodable {
    /// The builder container's id; its store directory is removed before and
    /// after the build.
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
    /// Host directories shared into every step, where the caller asked for
    /// them: what its scripts read in place of `COPY`.
    var mounts: [BuildMount]
    var steps: [BuildStep]
    /// `NAME=VALUE`, written into the image config and visible to every step,
    /// like `ENV`.
    var environment: [String]
    /// The finished image's OCI labels, as the caller named them.
    var labels: [String: String]
    /// The user and directory the finished image runs as.
    var user: String?
    var workingDirectory: String?
    var ipv4Address: String
    var ipv4Gateway: String
    var baseKey: String
    var cache: CachePolicy
    /// What runs each step's script, with the script appended. Defaults to
    /// `bash -euo pipefail -c` on the Rust side: the generated scripts chain
    /// with `&&`, and a silent mid-step failure would be baked into the image.
    var shell: [String]
    /// The builder's first process. Steps are `exec`s and need the container
    /// to outlive them; the base's `Cmd` would exit.
    var keepalive: [String]
    /// Delete unreferenced blobs and unpacked rootfs once the image is stored.
    var reclaim: Bool
}

enum Build {
    static let cacheDirectory = "build-cache"

    static func run(_ plan: BuildPlan) async throws {
        guard Entitlement.hasVirtualization else {
            throw BridgeError.unentitled
        }

        let root = URL(filePath: plan.storeRoot)
        // Our own because `ImageStore.contentStore` is internal and `ingest`
        // needs it. Same directory the store would use, so readers find the
        // blobs.
        let contentStore = try LocalContentStore(path: root.appending(path: "content"))
        let imageStore = try ImageStore(path: root, contentStore: contentStore)
        let platform = Platform.current

        // Pulled up front: the finished image inherits the base's config, and
        // its digest salts every cache key.
        let base = try await imageStore.get(reference: plan.base, pull: true)
        let baseConfig = try? await base.config(for: platform).config
        let environment = merge(baseConfig?.env ?? [], plan.environment)

        let cache = Cache(
            root: root.appending(path: cacheDirectory),
            keep: plan.cache.keep,
            keepFor: plan.cache.keepForSeconds
        )
        let keys = Keys(plan: plan, baseDigest: base.digest)

        // Nothing changed: re-tag, skipping the export.
        if plan.cache.restore, let descriptor = cache.image(keys.image),
            await Cache.holds(descriptor, in: contentStore)
        {
            note("\(plan.tag) is already built")
            try await retag(plan.tag, to: descriptor, in: imageStore)
            cache.evict()
            return
        }

        let (containerDirectory, rootfsPath) = container(plan.name, in: root)

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

        try await retag(plan.tag, to: descriptor, in: imageStore)

        cache.save(image: descriptor, as: keys.image)

        // Not `manager.delete`: a fully cached build has no manager, and its only
        // extra work is releasing a network interface, which there isn't.
        try? FileManager.default.removeItem(at: containerDirectory)

        if plan.reclaim {
            await reclaim(imageStore, root: root)
        }

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
        if plan.cache.restore {
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

        let mounts: [Containerization.Mount] = plan.mounts.map {
            .share(source: $0.source, destination: $0.destination, options: [$0.readonly ? "ro" : "rw"])
        }
        let nat = try NAT(address: plan.ipv4Address, gateway: plan.ipv4Gateway)
        let block = Containerization.Mount.ext4Root(rootfs)

        for index in steps {
            let container = try await manager.create(
                plan.name,
                image: base,
                rootfs: block,
                networking: false
            ) { config in
                config.cpus = plan.cpus
                config.memoryInBytes = plan.memoryInBytes
                // A keepalive, as in a session: steps are execs and need the
                // container to outlive them. The base's `Cmd` would exit.
                config.process.arguments = plan.keepalive
                config.process.user = .init()
                config.process.workingDirectory = "/"
                config.process.environmentVariables = environment
                config.mounts += mounts
                // Same NAT as a session: a step that installs anything needs
                // the network.
                nat.join(&config)
            }

            try await container.create()
            try await container.start()

            do {
                try await step(
                    plan.steps[index],
                    index: index,
                    in: container,
                    shell: plan.shell,
                    environment: environment
                )
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
    private static let builderMarker = ".builder"

    private static func markBuilder(_ directory: URL) {
        try? Data("\(getpid())\n".utf8).write(to: directory.appending(path: builderMarker))
    }

    /// Removes rootfs left by failed builds and by tags since renamed.
    ///
    /// Only marked directories (sessions share `containers`), and only when the
    /// owning pid is gone (builds in other projects share this store).
    private static func sweepBuilders(in root: URL, keeping current: String) {
        let directories =
            (try? FileManager.default.contentsOfDirectory(at: containers(in: root), includingPropertiesForKeys: nil))
            ?? []

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

    /// Points a reference at what this build produced.
    ///
    /// Delete first: `create` will not replace an existing reference, which a
    /// rebuild always has.
    private static func retag(_ reference: String, to descriptor: Descriptor, in imageStore: ImageStore) async throws {
        try? await imageStore.delete(reference: reference)
        try await imageStore.create(description: .init(reference: reference, descriptor: descriptor))
    }

    /// Deletes unreferenced blobs (chiefly the previous build's multi-gigabyte
    /// layer, orphaned by the re-tag) and unpacked rootfs of removed images.
    ///
    /// Only after `create`: until then this build's own blobs are unreferenced.
    /// Failure is logged, not thrown; the image is already usable.
    private static func reclaim(_ imageStore: ImageStore, root: URL) async {
        if let images = try? await imageStore.list() {
            Unpacked(store: root).evict(keeping: images.map(\.digest))
        }

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
    /// `shell` is the caller's, with the script appended: whether a step that
    /// fails halfway through fails the build is its policy, not this one's.
    private static func step(
        _ step: BuildStep,
        index: Int,
        in container: LinuxContainer,
        shell: [String],
        environment: [String]
    ) async throws {
        let log = FileWriter(FileHandle.standardError)

        log.line("--> \(step.name)")

        let process = try await container.exec("build-\(index)") { config in
            config.arguments = shell + [step.script]
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
    /// The layer is an uncompressed tar: nothing pushes this image, and an
    /// uncompressed blob's digest is also its diffID, so both are correct in
    /// one pass. (Containerization's `InitImage.create` has a standing `TODO`
    /// for writing a gzip layer's compressed digest as its diffID.)
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
        // The base's labels carry over, as `LABEL` does; the plan's win on a
        // key both set.
        let labels = (base?.labels ?? [:]).merging(plan.labels) { _, mine in mine }
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
                    labels: labels
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

/// Carries a result out of `ingest`'s `@Sendable` body, which cannot return one
/// or capture a bare `Mutex`.
private final class Box<Value: Sendable>: Sendable {
    private let stored = Mutex<Value?>(nil)

    var value: Value? {
        get { stored.withLock { $0 } }
        set { stored.withLock { $0 = newValue } }
    }
}
