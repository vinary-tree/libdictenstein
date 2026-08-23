// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "libdictenstein",
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
            exact: "4.0.0-rc.1"
        ),
    ],
    targets: [
        .systemLibrary(name: "CLibdictenstein"),
        .target(
            name: "Libdictenstein",
            dependencies: [
                "CLibdictenstein",
                .product(name: "VinaryTreeInterop", package: "vinary-tree-interop"),
            ]
        ),
        .executableTarget(
            name: "CollectionTraversalProfile",
            dependencies: ["Libdictenstein"]
        ),
        .testTarget(
            name: "LibdictensteinTests",
            dependencies: [
                "Libdictenstein",
                .product(name: "VinaryTreeInterop", package: "vinary-tree-interop"),
            ]
        ),
    ]
)
