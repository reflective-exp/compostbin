//===----------------------------------------------------------------------===//
// Whether this process may use Virtualization.framework at all.
//===----------------------------------------------------------------------===//

import Foundation
import Security

enum Entitlement {
    static let virtualization = "com.apple.security.virtualization"

    /// Virtualization.framework refuses every call without the entitlement, and
    /// says so in terms that describe the process rather than what to do about
    /// it. Asking first turns that into an answer.
    ///
    /// Worth asking on every boot rather than trusting the build: a signature
    /// does not survive a rebuild, so the ordinary way to arrive here without
    /// the entitlement is `cargo build` followed by running the binary — which
    /// is exactly what anyone iterating does.
    static var hasVirtualization: Bool {
        guard let task = SecTaskCreateFromSelf(nil) else {
            return false
        }

        let value = SecTaskCopyValueForEntitlement(task, virtualization as CFString, nil)

        return (value as? Bool) ?? false
    }
}
