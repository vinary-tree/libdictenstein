// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "VinaryTreeLibdictenstein",
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
            url: "https://github.com/vinary-tree/liblevenshtein-rust.git",
            from: "0.10.0"
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
                .product(name: "VinaryTreeInterop", package: "liblevenshtein-rust"),
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
