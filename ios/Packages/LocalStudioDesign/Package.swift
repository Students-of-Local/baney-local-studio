// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "LocalStudioDesign",
    platforms: [.iOS(.v26), .macOS(.v26)],
    products: [
        .library(name: "LocalStudioDesign", targets: ["LocalStudioDesign"]),
    ],
    targets: [
        .target(name: "LocalStudioDesign"),
        .executableTarget(name: "design-snapshots", dependencies: ["LocalStudioDesign"]),
    ]
)
