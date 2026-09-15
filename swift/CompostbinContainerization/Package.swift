// swift-tools-version: 6.2

import PackageDescription

// Pinned exactly, not `from:`. The initfs image in the `container` CLI's store
// is built by a specific Containerization release — `vminit:0.45.0` — and the
// guest agent it carries speaks that release's protocol. A library newer than
// the initfs on disk is a runtime mismatch, not a compile error.
let containerization = "0.45.0"

let package = Package(
    name: "CompostbinContainerization",
    platforms: [.macOS("26.0")],
    products: [
        .library(
            name: "CompostbinContainerization",
            type: .static,
            targets: ["CompostbinContainerization"]
        )
    ],
    dependencies: [
        .package(url: "https://github.com/apple/containerization.git", exact: .init(stringLiteral: containerization))
    ],
    targets: [
        .target(
            name: "CompostbinContainerization",
            dependencies: [
                .product(name: "Containerization", package: "containerization"),
                .product(name: "ContainerizationOCI", package: "containerization"),
                .product(name: "ContainerizationOS", package: "containerization"),
            ]
        )
    ]
)
