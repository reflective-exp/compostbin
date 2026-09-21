//===----------------------------------------------------------------------===//
// Descriptors as the streams Containerization reads and writes.
//
// `Terminal` is already both a `ReaderStream` and a `Writer`, so it needs none
// of this. These cover every other stream: a build step's log, a caller's
// stdin, stdout and stderr, and the stdout of a process reading a terminal.
//===----------------------------------------------------------------------===//

import Containerization
import Foundation
import Synchronization

/// A descriptor as a handle, which neither closes nor is closed by the
/// descriptor; whoever wraps it says when it closes. Negative is no descriptor:
/// a stream the guest process leaves unattached.
func handle(_ descriptor: Int32) -> FileHandle? {
    descriptor < 0 ? nil : FileHandle(fileDescriptor: descriptor, closeOnDealloc: false)
}

/// A descriptor the guest reads, streamed until it reaches end of file.
///
/// Unchecked: `FileHandle` is not `Sendable`, and only the readability handler
/// touches it, on one queue at a time.
final class FileReader: ReaderStream, @unchecked Sendable {
    private let handle: FileHandle

    init(_ handle: FileHandle) {
        self.handle = handle
    }

    func stream() -> AsyncStream<Data> {
        .init { continuation in
            handle.readabilityHandler = { handle in
                let data = handle.availableData

                // Empty means end of file. Containerization closes the guest's
                // stdin once the stream finishes.
                guard !data.isEmpty else {
                    handle.readabilityHandler = nil
                    continuation.finish()
                    return
                }

                continuation.yield(data)
            }
        }
    }

    /// Drops the handler and closes the descriptor, so a process that exits
    /// with input unread leaves nothing reading.
    func close() {
        handle.readabilityHandler = nil
        try? handle.close()
    }
}

/// A descriptor the guest writes. Locked so stdout and stderr chunks never
/// interleave mid-write when they share one.
final class FileWriter: Writer, Sendable {
    private let handle: Mutex<FileHandle>
    /// Whether `close()` closes the handle. This process's own stderr outlives
    /// every writer that logs to it.
    private let owned: Bool

    init(_ handle: FileHandle, owned: Bool = false) {
        self.handle = Mutex(handle)
        self.owned = owned
    }

    func write(_ data: Data) throws {
        try handle.withLock { try $0.write(contentsOf: data) }
    }

    func line(_ text: String) {
        try? write(Data("\(text)\n".utf8))
    }

    func close() throws {
        guard owned else { return }

        try handle.withLock { try $0.close() }
    }
}
