//===----------------------------------------------------------------------===//
// Images unpacked once, under `unpacked` in the image store.
//
// A session's rootfs is an ext4 block the guest writes to, so every run needs its
// own. Unpacking one from the image reads and rewrites the whole layer —
// gigabytes, on every `run`. Instead each image is unpacked once, keyed by its
// digest, and a session gets a clone of that: free to make on APFS, and costing
// only the blocks the session goes on to change. Off APFS the clone falls back
// to a copy, which is still cheaper than an unpack.
//
// `Build` evicts an entry once no image in the store has its digest.
//===----------------------------------------------------------------------===//

import Containerization
import ContainerizationOS
import Foundation

struct Unpacked {
    /// What `ContainerManager` gives a rootfs it unpacks itself. Sparse, so a
    /// ceiling rather than a cost.
    static let capacityInBytes = 8.gib()
    /// An unpack is minutes at most. A partial older than this was left by a
    /// process that died mid-unpack.
    static let abandonedAfter: TimeInterval = 60 * 60

    /// The image store's root.
    let store: URL

    private var root: URL { store.appending(path: "unpacked") }

    private func path(_ digest: String) -> URL {
        root.appending(path: "\(digest.replacing(":", with: "-")).ext4")
    }

    /// Whether the image is unpacked already, so its next run skips the unpack.
    /// Asked by the Rust side before a run, which says so when it will not.
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

        return .block(format: "ext4", source: destination.absolutePath(), destination: "/", options: [])
    }

    /// Unpacked aside and moved into place, so an interrupted unpack is never
    /// found, and under a name of its own, so two sessions first booting the
    /// same image never write to one file.
    private func unpack(_ image: Image, to destination: URL) async throws {
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)

        let partial = root.appending(path: "\(UUID().uuidString).partial")
        defer { try? FileManager.default.removeItem(at: partial) }

        let unpacker = EXT4Unpacker(capacityInBytes: Self.capacityInBytes)
        _ = try await unpacker.unpack(image, for: .current, at: partial)

        do {
            try FileManager.default.moveItem(at: partial, to: destination)
        } catch where FileManager.default.fileExists(atPath: destination.path(percentEncoded: false)) {
            // Another session unpacked the same image first; its copy is as
            // good as ours.
        }
    }

    /// Removes every entry whose digest no image in the store has, and every
    /// abandoned partial. A running session holds a clone, never the entry, so
    /// nothing in use goes with them.
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
