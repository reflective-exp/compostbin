//===----------------------------------------------------------------------===//
// The build cache, under `build-cache` in the image store:
//
//   * `rootfs/<key>.ext4` — the block after some step. A build matching an
//     earlier one through step `i` resumes from it.
//   * `images/<key>.json` — the descriptor a build ingested, so an unchanged
//     build skips the export.
//
// Snapshots are APFS clones, so they cost only what they differ by. Off APFS
// they fall back to a full copy.
//
// Keys come from the Rust side's `cache` module; `Keys` salts them.
//===----------------------------------------------------------------------===//

import ContainerizationOCI
import CryptoKit
import Foundation

/// The plan's key chain, salted with what only the builder knows.
struct Keys {
    let base: String
    let steps: [String]
    /// The last rootfs plus the user and working directory, which a rootfs
    /// does not record.
    let image: String

    init(plan: BuildPlan, baseDigest: String) {
        // The base digest, so a moved base misses everything; the capacity,
        // because a snapshot keeps the size it was taken at.
        func salted(_ key: String) -> String {
            Self.digest("\(baseDigest)\n\(plan.rootfsSizeInBytes)\n\(key)")
        }

        let base = salted(plan.baseKey)
        let steps = plan.steps.map { salted($0.cacheKey) }

        self.base = base
        self.steps = steps
        self.image = Self.digest("\(steps.last ?? base)\n\(plan.user ?? "")\n\(plan.workingDirectory ?? "")")
    }

    static func digest(_ text: String) -> String {
        SHA256.hash(data: Data(text.utf8))
            .map { String(format: "%02x", $0) }
            .joined()
    }
}

struct Cache {
    let root: URL
    /// Snapshots kept, most recently used first. The caller's: how much disk a
    /// build cache is worth is not this crate's to decide.
    let keep: Int
    /// Anything unused this long goes regardless.
    let keepFor: TimeInterval

    private var rootfsDirectory: URL { root.appending(path: "rootfs") }
    private var imagesDirectory: URL { root.appending(path: "images") }

    private func rootfsPath(_ key: String) -> URL {
        rootfsDirectory.appending(path: "\(key).ext4")
    }

    private func imagePath(_ key: String) -> URL {
        imagesDirectory.appending(path: "\(key).json")
    }

    // MARK: - Rootfs snapshots

    func holdsRootfs(_ key: String) -> Bool {
        FileManager.default.fileExists(atPath: rootfsPath(key).path(percentEncoded: false))
    }

    /// Clones a snapshot to `destination`. The build writes to the clone, so a
    /// failed build leaves the cache untouched.
    func restore(_ key: String, to destination: URL) throws {
        try Self.clone(rootfsPath(key), to: destination)
        touch(rootfsPath(key))
    }

    /// Best effort: a failure only costs the next build time.
    func save(rootfs: URL, as key: String) {
        let destination = rootfsPath(key)

        guard !FileManager.default.fileExists(atPath: destination.path(percentEncoded: false)) else {
            touch(destination)
            return
        }

        do {
            try FileManager.default.createDirectory(at: rootfsDirectory, withIntermediateDirectories: true)
            // Cloned aside then moved, so an interrupted clone is never found.
            let partial = rootfsDirectory.appending(path: "\(key).partial")

            try Self.clone(rootfs, to: partial)
            try? FileManager.default.removeItem(at: destination)
            try FileManager.default.moveItem(at: partial, to: destination)
        } catch {
            note("could not cache the rootfs for \(key.prefix(12)): \(error)")
        }
    }

    // MARK: - Ingested images

    func image(_ key: String) -> Descriptor? {
        guard let data = try? Data(contentsOf: imagePath(key)) else {
            return nil
        }

        touch(imagePath(key))

        return try? JSONDecoder().decode(Descriptor.self, from: data)
    }

    func save(image descriptor: Descriptor, as key: String) {
        do {
            try FileManager.default.createDirectory(at: imagesDirectory, withIntermediateDirectories: true)
            try JSONEncoder().encode(descriptor).write(to: imagePath(key), options: .atomic)
        } catch {
            note("could not cache the image for \(key.prefix(12)): \(error)")
        }
    }

    /// Whether every blob under a descriptor is still stored; `reclaim` deletes
    /// them once the tag is gone.
    static func holds(_ descriptor: Descriptor, in contentStore: ContentStore) async -> Bool {
        guard let content = try? await contentStore.get(digest: descriptor.digest),
            let index: Index = try? content.decode()
        else {
            return false
        }

        for entry in index.manifests {
            guard let held = try? await contentStore.get(digest: entry.digest),
                let manifest: Manifest = try? held.decode()
            else {
                return false
            }

            for blob in manifest.layers + [manifest.config] {
                guard (try? await contentStore.get(digest: blob.digest)) != nil else {
                    return false
                }
            }
        }

        return true
    }

    // MARK: - Eviction

    /// By last use: restoring or re-saving touches an entry.
    func evict() {
        let cutoff = Date.now.addingTimeInterval(-keepFor)
        let snapshots = Self.entries(of: rootfsDirectory)

        for (index, entry) in snapshots.enumerated() where index >= keep || entry.used < cutoff {
            try? FileManager.default.removeItem(at: entry.path)
        }

        for entry in Self.entries(of: imagesDirectory) where entry.used < cutoff {
            try? FileManager.default.removeItem(at: entry.path)
        }
    }

    /// Most recently used first.
    private static func entries(of directory: URL) -> [(path: URL, used: Date)] {
        let listing =
            (try? FileManager.default.contentsOfDirectory(
                at: directory,
                includingPropertiesForKeys: [.contentModificationDateKey]
            )) ?? []

        return
            listing
            .map { path in
                let used =
                    (try? path.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate)
                    ?? .distantPast

                return (path: path, used: used)
            }
            .sorted { $0.used > $1.used }
    }

    private func touch(_ path: URL) {
        try? FileManager.default.setAttributes(
            [.modificationDate: Date.now],
            ofItemAtPath: path.path(percentEncoded: false)
        )
    }

    // MARK: - Copying

    /// `clonefile` where supported, else a real copy.
    static func clone(_ source: URL, to destination: URL) throws {
        try? FileManager.default.removeItem(at: destination)

        if clonefile(source.path(percentEncoded: false), destination.path(percentEncoded: false), 0) == 0 {
            return
        }

        let failure = errno

        guard [ENOTSUP, EXDEV, EINVAL, EPERM].contains(failure) else {
            throw BridgeError.notCloned(source.lastPathComponent, String(cString: strerror(failure)))
        }

        try FileManager.default.copyItem(at: source, to: destination)
    }
}
