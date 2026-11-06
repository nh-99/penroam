// swift-tools-version:5.9
// Swift side of tauri-plugin-native-print (iOS/iPadOS). `../.tauri/tauri-api` is created by the
// tauri-plugin build script (Tauri's iOS Swift API).
import PackageDescription

let package = Package(
    name: "tauri-plugin-native-print",
    platforms: [.iOS(.v16), .macOS(.v14)],
    products: [
        .library(name: "tauri-plugin-native-print", type: .static, targets: ["tauri-plugin-native-print"]),
    ],
    dependencies: [
        .package(name: "Tauri", path: "../.tauri/tauri-api"),
    ],
    targets: [
        .target(
            name: "tauri-plugin-native-print",
            dependencies: [.byName(name: "Tauri")],
            path: "Sources"
        ),
    ]
)
