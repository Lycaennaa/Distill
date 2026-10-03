// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "DistillSmoke",
    products: [
        .executable(name: "DistillSmoke", targets: ["DistillSmoke"]),
    ],
    targets: [
        .executableTarget(name: "DistillSmoke"),
    ]
)