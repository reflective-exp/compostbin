//===----------------------------------------------------------------------===//
// Putting a kernel and an init image in the store.
//
// A VM needs two things that are not images of the workload: a kernel to boot and
// an init image to boot into. Both are fetched rather than built:
//
//   * the init image — `vminitd`, the agent the library talks to over vsock —
//     is an OCI image, so pulling it is an ImageStore call;
//   * the kernel is not. Nobody publishes one as an image, so it comes out of
//     Kata Containers' static release, the same place Containerization's own
//     Makefile takes it from and the same one the CLI offers on first start.
//
// Idempotent and cheap when there is nothing to do: `build` calls it every time
// rather than leaving it to a separate command nobody remembers to run.
//===----------------------------------------------------------------------===//

import Containerization
import ContainerizationArchive
import ContainerizationOCI
import Foundation

struct ProvisionSpec: Decodable {
    var storeRoot: String
    /// Where the kernel goes. Inside the store, so removing the store removes it.
    var kernelPath: String
    var kernelURL: String
    /// The kernel's path inside the downloaded archive.
    var kernelInArchive: String
    var initfsReference: String

    enum CodingKeys: String, CodingKey {
        case storeRoot
        case kernelPath
        // The Rust side writes `kernelUrl`: serde's camelCase of `kernel_url`,
        // which does not know that URL is an initialism.
        case kernelURL = "kernelUrl"
        case kernelInArchive
        case initfsReference
    }
}

enum Provision {
    static func run(_ spec: ProvisionSpec) async throws {
        let root = URL(fileURLWithPath: spec.storeRoot)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)

        try await kernel(spec)
        try await initfs(spec, root: root)
    }

    /// Downloads and unpacks the kernel, unless it is already there.
    private static func kernel(_ spec: ProvisionSpec) async throws {
        let destination = URL(fileURLWithPath: spec.kernelPath)

        guard !FileManager.default.fileExists(atPath: destination.path) else {
            return
        }

        guard let url = URL(string: spec.kernelURL) else {
            throw BridgeError.malformed("kernel url", spec.kernelURL)
        }

        note("downloading a kernel from \(url.absoluteString)")

        // To a file rather than into memory: the archive is a few hundred
        // megabytes, and only one entry of it is wanted.
        let (archive, response) = try await URLSession.shared.download(from: url)

        defer { try? FileManager.default.removeItem(at: archive) }

        if let status = (response as? HTTPURLResponse)?.statusCode, status != 200 {
            throw BridgeError.kernelUnavailable(url.absoluteString, status)
        }

        note("unpacking \(spec.kernelInArchive)")

        let binary = try Self.extract(spec.kernelInArchive, from: archive)

        try FileManager.default.createDirectory(
            at: destination.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        // Written beside the destination and moved, so an interrupted
        // provision cannot leave a half-written kernel that looks complete.
        let partial = destination.appendingPathExtension("partial")
        try binary.write(to: partial, options: .atomic)
        _ = try FileManager.default.replaceItemAt(destination, withItemAt: partial)
    }

    /// Reads one file out of the archive, following a symlink if that is what it
    /// finds: in Kata's release `vmlinux.container` is a link to the versioned
    /// kernel beside it, and a link's own entry carries no contents.
    private static func extract(_ path: String, from archive: URL) throws -> Data {
        let (entry, data) = try ArchiveReader(file: archive).extractFile(path: path)

        guard entry.fileType == .symbolicLink, let target = entry.symlinkTarget else {
            guard !data.isEmpty else {
                throw BridgeError.kernelMissing(path)
            }

            return data
        }

        // Relative to the link's own directory, and the reader has to start over:
        // extracting moved it past the entry the target may precede.
        let directory = (path as NSString).deletingLastPathComponent
        let resolved = target.hasPrefix("/") ? target : "\(directory)/\(target)"
        let (_, contents) = try ArchiveReader(file: archive).extractFile(path: resolved)

        guard !contents.isEmpty else {
            throw BridgeError.kernelMissing(resolved)
        }

        return contents
    }

    /// Pulls the init image, unless the store already holds it.
    ///
    /// `getInitImage` pulls what it cannot find, so this is only about saying so
    /// first: the pull is the slow part of a first build, and a silent wait looks
    /// like a hang.
    private static func initfs(_ spec: ProvisionSpec, root: URL) async throws {
        let imageStore = try ImageStore(path: root)

        if (try? await imageStore.get(reference: spec.initfsReference)) != nil {
            return
        }

        note("pulling \(spec.initfsReference)")

        _ = try await imageStore.getInitImage(reference: spec.initfsReference)
    }

    /// Provisioning is slow and only happens when something is missing, so it
    /// says what it is doing. On stderr, where a build log goes.
    private static func note(_ message: String) {
        FileHandle.standardError.write(Data("compostbin: \(message)\n".utf8))
    }
}
