//===----------------------------------------------------------------------===//
// The surface Rust calls.
//
// Flat and synchronous: no Swift types cross, every function blocks, and
// failure returns a negative code with the message in `lastError` for Rust to
// collect. swift-bridge handles `async` poorly in this direction.
//
// Argument labels are the Rust parameter names; swift-bridge's generated
// `@_cdecl` shims call with them, so they are part of the contract.
//
// Lists cross as newline-separated strings, so no element may contain a
// newline (`host::request::Request::render` likewise refuses them in guest
// requests).
//===----------------------------------------------------------------------===//

import Foundation
import Synchronization

/// Set by the last failing bridged call, read by `compostbin_last_error`.
/// Locked because two attached terminals can fail at once.
private let lastError = Mutex("")

/// Bridge failure. Outside the guest exit code range 0...255.
private let failed: Int32 = -1

/// Turns a thrown error into `failed`, storing its message in `lastError`.
private func reporting(_ body: () throws -> Int32) -> Int32 {
    do {
        return try body()
    } catch {
        lastError.withLock { $0 = "\(error)" }
        return failed
    }
}

/// A line in the build log, on stderr. Unprefixed: what to call this belongs to
/// whoever linked the bridge, not to the bridge.
func note(_ message: String) {
    try? FileHandle.standardError.write(contentsOf: Data("\(message)\n".utf8))
}

private func decoding<T: Decodable>(_ type: T.Type, from json: RustStr) throws -> T {
    try JSONDecoder().decode(type, from: Data(json.toString().utf8))
}

private func lines(_ text: RustStr) -> [String] {
    let string = text.toString()

    return string.isEmpty ? [] : string.components(separatedBy: "\n")
}

func compostbin_last_error() -> String {
    lastError.withLock { $0 }
}

/// Boots a session's VM and leaves it running, owned by this process.
func compostbin_boot(
    name: RustStr,
    store_root: RustStr,
    kernel_path: RustStr,
    initfs_reference: RustStr,
    image_reference: RustStr,
    cpus: Int32,
    memory_in_bytes: UInt64,
    mounts: RustStr,
    sockets: RustStr,
    environment: RustStr,
    arguments: RustStr,
    working_directory: RustStr,
    ipv4_address: RustStr,
    ipv4_gateway: RustStr
) -> Int32 {
    reporting {
        let spec = BootSpec(
            name: name.toString(),
            storeRoot: store_root.toString(),
            kernelPath: kernel_path.toString(),
            initfsReference: initfs_reference.toString(),
            imageReference: image_reference.toString(),
            cpus: Int(cpus),
            memoryInBytes: memory_in_bytes,
            mounts: lines(mounts),
            sockets: lines(sockets),
            environment: lines(environment),
            arguments: lines(arguments),
            workingDirectory: working_directory.toString(),
            ipv4Address: ipv4_address.toString(),
            ipv4Gateway: ipv4_gateway.toString()
        )

        try blocking { try await Session.boot(spec) }

        return 0
    }
}

/// Builds an image from a plan, with no builder and no daemon.
///
/// The plan crosses as JSON because it nests and build steps are arbitrary
/// shell that may contain newlines.
func compostbin_build(plan: RustStr) -> Int32 {
    reporting {
        let plan = try decoding(BuildPlan.self, from: plan)

        try blocking { try await Build.run(plan) }

        return 0
    }
}

/// Puts a kernel and an init image in the store, fetching whatever is missing.
func compostbin_provision(spec: RustStr) -> Int32 {
    reporting {
        let spec = try decoding(ProvisionSpec.self, from: spec)

        try blocking { try await Provision.run(spec) }

        return 0
    }
}

/// Runs a process in a booted session and blocks until it exits, returning its
/// exit code.
///
/// `terminal` is a descriptor in this process: its own when `run` attaches
/// Claude, or one `compostbin shell` passed over the control socket. `-1` runs
/// without a terminal. An empty `user` means the image's default.
func compostbin_exec(
    name: RustStr,
    id: RustStr,
    arguments: RustStr,
    environment: RustStr,
    user: RustStr,
    working_directory: RustStr,
    terminal: Int32,
    stdin: Int32,
    stdout: Int32,
    stderr: Int32
) -> Int32 {
    reporting {
        let user = user.toString()
        let request = ExecRequest(
            name: name.toString(),
            id: id.toString(),
            arguments: lines(arguments),
            environment: lines(environment),
            user: user.isEmpty ? nil : user,
            workingDirectory: working_directory.toString(),
            terminal: terminal,
            stdin: stdin,
            stdout: stdout,
            stderr: stderr
        )

        return try blocking { try await Session.exec(request) }
    }
}

/// Tells the guest the terminal changed size. A no-op once the process has gone.
///
/// A failure here is dropped rather than recorded: resizes run beside the
/// attach they belong to, and one that stored its message in `lastError` would
/// be read back as the reason that attach failed.
func compostbin_resize(id: RustStr, terminal: Int32) -> Int32 {
    do {
        let id = id.toString()

        try blocking { try await Session.resize(id: id, terminal: terminal) }

        return 0
    } catch {
        return failed
    }
}

/// 1 when this image is already unpacked, 0 when the next run must unpack it.
func compostbin_is_unpacked(store_root: RustStr, image_reference: RustStr) -> Int32 {
    reporting {
        let unpacked = Unpacked(store: URL(filePath: store_root.toString()))
        let reference = image_reference.toString()

        return try blocking { try await unpacked.holds(reference) } ? 1 : 0
    }
}

/// Whether this process owns a running session by that name.
func compostbin_is_running(name: RustStr) -> Bool {
    Sessions.shared.get(name.toString()) != nil
}
