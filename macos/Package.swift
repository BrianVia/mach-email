// swift-tools-version: 6.0
import Foundation
import PackageDescription

var mach = Target.executableTarget(name: "Mach", path: "Sources/Mach")
var targets = [mach]

// RUST BRIDGE HOOK: once macos/Generated/MachFFI.xcframework exists, link it.
let ffi = "Generated/MachFFI.xcframework"
if FileManager.default.fileExists(atPath: Context.packageDirectory + "/" + ffi) {
    targets.append(.binaryTarget(name: "MachFFI", path: ffi))
    mach.dependencies.append("MachFFI")
}

let package = Package(
    name: "Mach",
    platforms: [.macOS(.v15)],
    targets: targets
)
