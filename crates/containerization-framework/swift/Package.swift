// swift-tools-version: 6.2

import Foundation
import PackageDescription

// Scoped to this target: a `-Xswiftc` flag would apply to every target, making
// swift-bridge's regenerated header an input to swift-nio, gRPC and
// Containerization, so each Rust bridge change costs a minute of rebuilding.
//
// Absolute, because the flag reaches the compiler verbatim and its working
// directory is unspecified.
let bridgingHeader = "\(URL(fileURLWithPath: #filePath).deletingLastPathComponent().path)/Sources/ContainerizationBridge/bridging-header.h"

// Pinned exactly: the initfs in the `container` CLI's store (`vminit:0.45.0`)
// carries a guest agent speaking that release's protocol. A newer library is a
// runtime mismatch, not a compile error.
let containerization: Version = "0.45.0"

let package = Package(
    name: "ContainerizationBridge",
    platforms: [.macOS("26.0")],
    products: [
        .library(
            name: "ContainerizationBridge",
            type: .static,
            targets: ["ContainerizationBridge"]
        )
    ],
    dependencies: [
        .package(url: "https://github.com/apple/containerization.git", exact: containerization)
    ],
    targets: [
        .target(
            name: "ContainerizationBridge",
            dependencies: [
                .product(name: "Containerization", package: "containerization"),
                .product(name: "ContainerizationOCI", package: "containerization"),
                .product(name: "ContainerizationOS", package: "containerization"),
            ],
            swiftSettings: [.unsafeFlags(["-import-objc-header", bridgingHeader])]
        )
    ]
)
