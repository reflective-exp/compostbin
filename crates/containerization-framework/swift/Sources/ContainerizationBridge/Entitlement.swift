//===----------------------------------------------------------------------===//
// Whether this process may use Virtualization.framework at all.
//===----------------------------------------------------------------------===//

import Foundation
import Security

enum Entitlement {
    static let virtualization = "com.apple.security.virtualization"

    /// Virtualization.framework refuses every call without the entitlement,
    /// with an error that doesn't say how to fix it. Checking first is what
    /// produces a message naming the entitlement instead.
    ///
    /// Checked on every boot: a rebuild drops the signature, so a plain
    /// `cargo build` then run arrives here unentitled.
    static var hasVirtualization: Bool {
        guard let task = SecTaskCreateFromSelf(nil) else {
            return false
        }

        let value = SecTaskCopyValueForEntitlement(task, virtualization as CFString, nil)

        return (value as? Bool) ?? false
    }
}
