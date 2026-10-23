// swift-tools-version: 6.0
import PackageDescription

// Generated/ comes from scripts/build-ffi.sh (build-app.sh runs it when missing or stale).
let package = Package(
    name: "Mach",
    platforms: [.macOS(.v15)],
    targets: [
        .binaryTarget(name: "MachFFI", path: "Generated/MachFFI.xcframework"),
        .target(
            name: "MachBridge",
            dependencies: ["MachFFI"],
            path: "Generated",
            exclude: ["MachFFI.xcframework"],
            sources: ["mach_ffi.swift"],
            linkerSettings: [
                .linkedFramework("Security"),
                .linkedFramework("SystemConfiguration"),
                .linkedFramework("CoreFoundation"),
            ]),
        .executableTarget(name: "Mach", dependencies: ["MachBridge"], path: "Sources/Mach"),
        .testTarget(name: "MachTests", dependencies: ["Mach"]),
    ]
)
