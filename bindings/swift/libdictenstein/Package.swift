// swift-tools-version: 6.0
import PackageDescription

let interopDependency: Package.Dependency = if let localRoot = Context.environment["VINARY_TREE_INTEROP_ROOT"] {
    .package(path: localRoot)
} else {
    .package(
        url: "https://github.com/vinary-tree/vinary-tree-interop.git",
        exact: "4.0.0-rc.2"
    )
}

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
    dependencies: [interopDependency],
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
