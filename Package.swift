// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "Libdictenstein",
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "Libdictenstein", targets: ["Libdictenstein"]),
        .executable(
            name: "libdictenstein-collection-profile",
            targets: ["CollectionTraversalProfile"]
        ),
    ],
    dependencies: [
        .package(
            url: "https://github.com/vinary-tree/vinary-tree-interop.git",
            exact: "4.0.0-rc.6"
        ),
    ],
    targets: [
        .systemLibrary(
            name: "CLibdictenstein",
            path: "bindings/swift/libdictenstein/Sources/CLibdictenstein"
        ),
        .target(
            name: "Libdictenstein",
            dependencies: [
                "CLibdictenstein",
                .product(name: "VinaryTreeInterop", package: "vinary-tree-interop"),
            ],
            path: "bindings/swift/libdictenstein/Sources/Libdictenstein"
        ),
        .executableTarget(
            name: "CollectionTraversalProfile",
            dependencies: ["Libdictenstein"],
            path: "bindings/swift/libdictenstein/Sources/CollectionTraversalProfile"
        ),
    ]
)
