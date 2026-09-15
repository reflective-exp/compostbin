//===----------------------------------------------------------------------===//
// The surface Rust calls.
//
// Flat and synchronous by construction: no Swift types cross, every function
// blocks, and failure comes back as a negative code with the message left in
// `lastError` for Rust to collect. swift-bridge carries `async` poorly in this
// direction, and a bridge that cannot fail informatively is not worth having.
//
// The argument labels are the Rust parameter names — swift-bridge's generated
// `@_cdecl` shims call these with them, so they are part of the contract.
//
// Lists cross as newline-separated strings. Nothing compostbin puts in one may
// contain a newline, and `host::request::Request::render` already refuses such
// an argument on the other side of the container.
//===----------------------------------------------------------------------===//

import Foundation

/// Set by the last failing bridged call, read by `compostbin_last_error`.
nonisolated(unsafe) private var lastError = Locked("")

/// `lastError` is per-process, and two attached terminals can fail at once.
private final class Locked<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Value

    init(_ value: Value) {
        self.value = value
    }

    var current: Value {
        get {
            lock.lock()
            defer { lock.unlock() }
            return value
        }
        set {
            lock.lock()
            defer { lock.unlock() }
            value = newValue
        }
    }
}

/// Failure, as the bridge reports it. Distinct from any exit code a guest
/// process can return, which is 0...255.
private let failed: Int32 = -1

/// Runs `body`, turning a thrown error into `failed` and a readable message.
private func reporting(_ body: () throws -> Int32) -> Int32 {
    do {
        return try body()
    } catch {
        lastError.current = "\(error)"
        return failed
    }
}

private func lines(_ text: RustStr) -> [String] {
    let string = text.toString()

    return string.isEmpty ? [] : string.components(separatedBy: "\n")
}

func compostbin_last_error() -> String {
    lastError.current
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

/// Runs a process in a booted session and blocks until it exits, returning its
/// exit code.
///
/// `terminal` is a descriptor this process can use: its own when `run` attaches
/// Claude, or one `compostbin shell` passed across the control socket. `-1`
/// runs without a terminal at all.
func compostbin_exec(
    name: RustStr,
    id: RustStr,
    arguments: RustStr,
    environment: RustStr,
    working_directory: RustStr,
    terminal: Int32
) -> Int32 {
    reporting {
        let request = ExecRequest(
            name: name.toString(),
            id: id.toString(),
            arguments: lines(arguments),
            environment: lines(environment),
            workingDirectory: working_directory.toString(),
            terminal: terminal
        )

        return try blocking { try await Session.exec(request) }
    }
}

/// Tells the guest the terminal changed size. A no-op once the process has gone.
func compostbin_resize(id: RustStr, terminal: Int32) -> Int32 {
    reporting {
        let id = id.toString()

        try blocking { try await Session.resize(id: id, terminal: terminal) }

        return 0
    }
}

/// Stops and deletes a session this process owns. Succeeds when it owns none.
func compostbin_stop(name: RustStr) -> Int32 {
    reporting {
        let name = name.toString()

        try blocking { try await Session.stop(name: name) }

        return 0
    }
}

/// Whether this process owns a running session by that name.
func compostbin_is_running(name: RustStr) -> Bool {
    Sessions.shared.get(name.toString()) != nil
}
