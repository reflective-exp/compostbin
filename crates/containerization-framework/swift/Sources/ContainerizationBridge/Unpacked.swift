//===----------------------------------------------------------------------===//
// Images unpacked once, under `unpacked` in the image store.
//
// Each run needs its own writable ext4 rootfs, and unpacking rewrites gigabytes.
// So each image is unpacked once, keyed by digest, and a session gets a clone:
// free on APFS, costing only the blocks it changes. Off APFS it falls back to a
// copy, still cheaper than an unpack.
//
// `Build` evicts an entry once no image in the store has its digest.
//===----------------------------------------------------------------------===//

import Containerization
import ContainerizationOS
import Foundation

extension Containerization.Mount {
    /// An ext4 image file as the guest's root filesystem.
    static func ext4Root(_ image: URL) -> Self {
        .block(format: "ext4", source: image.absolutePath(), destination: "/", options: [])
    }
}

struct Unpacked {
    /// An unpack takes minutes at most; an older partial was abandoned.
    static let abandonedAfter: TimeInterval = 60 * 60

    /// The image store's root.
    let store: URL
    /// Ceiling for an unpack. Sparse, so a ceiling, not a cost — but nothing
    /// in the container may outgrow it. The caller's, since only it knows the
    /// workload; `ContainerManager`'s own default is 8 GiB.
    ///
    /// Not part of the entry's key: an image unpacked once is reused whatever
    /// ceiling the next container asks for, so raising it only takes effect
    /// for images not yet unpacked. `evict` is how to force a re-unpack.
    var capacityInBytes: UInt64 = 8.gib()

    private var root: URL { store.appending(path: "unpacked") }

    private func path(_ digest: String) -> URL {
        root.appending(path: "\(digest.replacing(":", with: "-")).ext4")
    }

    /// Whether the image is already unpacked. The Rust side asks before a run
    /// so it can announce an unpack.
    func holds(_ reference: String) async throws -> Bool {
        let image = try await ImageStore(path: store).get(reference: reference)

        return holds(image)
    }

    private func holds(_ image: Image) -> Bool {
        FileManager.default.fileExists(atPath: path(image.digest).path(percentEncoded: false))
    }

    /// Clones the image's unpacked rootfs to `destination`, unpacking it first
    /// on the image's first run.
    func rootfs(for image: Image, at destination: URL) async throws -> Containerization.Mount {
        let source = path(image.digest)

        if !holds(image) {
            try await unpack(image, to: source)
        }

        try Cache.clone(source, to: destination)

        return .ext4Root(destination)
    }

    /// Unpacked aside and moved into place, so an interrupted unpack is never
    /// found; under a unique name, so two sessions first booting the same image
    /// never share a file.
    private func unpack(_ image: Image, to destination: URL) async throws {
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)

        let partial = root.appending(path: "\(UUID().uuidString).partial")
        defer { try? FileManager.default.removeItem(at: partial) }

        let unpacker = EXT4Unpacker(capacityInBytes: capacityInBytes)
        _ = try await unpacker.unpack(image, for: .current, at: partial)

        do {
            try FileManager.default.moveItem(at: partial, to: destination)
        } catch where FileManager.default.fileExists(atPath: destination.path(percentEncoded: false)) {
            // Another session unpacked it first; its copy is equivalent.
        }
    }

    /// Removes entries no stored image references, and abandoned partials.
    /// Running sessions hold clones, never entries, so nothing in use is lost.
    func evict(keeping digests: some Sequence<String>) {
        let kept = Set(digests.map { path($0).lastPathComponent })
        let cutoff = Date.now.addingTimeInterval(-Self.abandonedAfter)
        let entries =
            (try? FileManager.default.contentsOfDirectory(
                at: root,
                includingPropertiesForKeys: [.contentModificationDateKey]
            )) ?? []

        for entry in entries {
            let stale =
                switch entry.pathExtension {
                case "ext4":
                    !kept.contains(entry.lastPathComponent)
                case "partial":
                    ((try? entry.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate)
                        ?? .distantFuture) < cutoff
                default:
                    false
                }

            if stale {
                try? FileManager.default.removeItem(at: entry)
            }
        }
    }
}
