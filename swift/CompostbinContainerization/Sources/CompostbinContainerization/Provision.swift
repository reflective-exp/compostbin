//===----------------------------------------------------------------------===//
// Putting a kernel and an init image in the store.
//
// A VM needs a kernel to boot and an init image to boot into. Both are fetched:
//
//   * the init image (`vminitd`, the agent the library talks to over vsock) is
//     an OCI image, pulled through ImageStore;
//   * the kernel is not published as an image, so it comes from Kata
//     Containers' static release, as in Containerization's Makefile and the
//     CLI's first-start offer.
//
// Idempotent and cheap when there is nothing to do, so `build` calls it every
// time.
//===----------------------------------------------------------------------===//

import Containerization
import ContainerizationArchive
import ContainerizationOCI
import Foundation
import SystemPackage

struct ProvisionSpec: Decodable {
    var storeRoot: String
    /// Inside the store, so removing the store removes it.
    var kernelPath: String
    var kernelURL: String
    /// The kernel's path inside the downloaded archive.
    var kernelInArchive: String
    var initfsReference: String

    enum CodingKeys: String, CodingKey {
        case storeRoot
        case kernelPath
        // serde's camelCase of `kernel_url`.
        case kernelURL = "kernelUrl"
        case kernelInArchive
        case initfsReference
    }
}

enum Provision {
    static func run(_ spec: ProvisionSpec) async throws {
        let root = URL(filePath: spec.storeRoot)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)

        async let kernel: Void = kernel(spec)
        async let initfs: Void = initfs(spec, root: root)

        _ = try await (kernel, initfs)
    }

    /// Downloads and unpacks the kernel, unless it is already there.
    private static func kernel(_ spec: ProvisionSpec) async throws {
        let destination = URL(filePath: spec.kernelPath)

        guard !FileManager.default.fileExists(atPath: destination.path(percentEncoded: false)) else {
            return
        }

        guard let url = URL(string: spec.kernelURL) else {
            throw BridgeError.malformed("kernel url", spec.kernelURL)
        }

        note("downloading a kernel from \(url.absoluteString)")

        // To a file, not memory: the archive is hundreds of megabytes and only
        // one entry is wanted.
        let (archive, response) = try await URLSession.shared.download(from: url)

        defer { try? FileManager.default.removeItem(at: archive) }

        if let status = (response as? HTTPURLResponse)?.statusCode, status != 200 {
            throw BridgeError.kernelUnavailable(url.absoluteString, status)
        }

        note("unpacking \(spec.kernelInArchive)")

        let binary = try extract(spec.kernelInArchive, from: archive)

        try FileManager.default.createDirectory(
            at: destination.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        // Written aside and moved, so an interrupted provision never leaves a
        // half-written kernel that looks complete.
        let partial = destination.appendingPathExtension("partial")
        try binary.write(to: partial, options: .atomic)
        _ = try FileManager.default.replaceItemAt(destination, withItemAt: partial)
    }

    /// Reads one file out of the archive, following a symlink: in Kata's release
    /// `vmlinux.container` links to the versioned kernel beside it, and a link
    /// entry has no contents.
    private static func extract(_ path: String, from archive: URL) throws -> Data {
        let (entry, data) = try ArchiveReader(file: archive).extractFile(path: path)

        guard entry.fileType == .symbolicLink, let target = entry.symlinkTarget else {
            guard !data.isEmpty else {
                throw BridgeError.kernelMissing(path)
            }

            return data
        }

        // Resolved against the link's directory (`pushing` replaces outright
        // when the target is absolute). A fresh reader, because extracting moved
        // the old one past entries the target may precede.
        let resolved = FilePath(path).removingLastComponent().pushing(FilePath(target)).string
        let (_, contents) = try ArchiveReader(file: archive).extractFile(path: resolved)

        guard !contents.isEmpty else {
            throw BridgeError.kernelMissing(resolved)
        }

        return contents
    }

    /// Pulls the init image, unless the store already holds it.
    ///
    /// `getInitImage` pulls on its own; the check exists to log first, since the
    /// pull is the slow part of a first build and a silent wait looks like a
    /// hang.
    private static func initfs(_ spec: ProvisionSpec, root: URL) async throws {
        let imageStore = try ImageStore(path: root)

        if (try? await imageStore.get(reference: spec.initfsReference)) != nil {
            return
        }

        note("pulling \(spec.initfsReference)")

        _ = try await imageStore.getInitImage(reference: spec.initfsReference)
    }
}
