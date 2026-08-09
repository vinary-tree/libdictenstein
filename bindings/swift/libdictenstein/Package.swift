// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "libdictenstein",
    platforms: [.macOS(.v13)],
    products: [.library(name: "Libdictenstein", targets: ["Libdictenstein"])],
    dependencies: [
        .package(path: "../../../../liblevenshtein-rust/vinary-tree-interop/bindings/swift/vinary-tree-interop"),
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
        .testTarget(
            name: "LibdictensteinTests",
            dependencies: [
                "Libdictenstein",
                .product(name: "VinaryTreeInterop", package: "vinary-tree-interop"),
            ]
        ),
    ]
)
