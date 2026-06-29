// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "AudeticMenuBar",
    platforms: [
        // MenuBarExtra (SwiftUI) requires macOS 13; the daemon targets 14.6, so
        // we align on 14 to keep one minimum-system story across the bundle.
        .macOS(.v14)
    ],
    dependencies: [
        // User-customizable global keyboard shortcuts. Same library the README
        // we modelled this on uses; fully sandbox/MAS compatible, no Carbon
        // permission prompts.
        .package(
            url: "https://github.com/sindresorhus/KeyboardShortcuts",
            from: "3.0.0"
        )
    ],
    targets: [
        .executableTarget(
            name: "AudeticMenuBar",
            dependencies: [
                .product(name: "KeyboardShortcuts", package: "KeyboardShortcuts")
            ],
            path: "Sources/AudeticMenuBar"
        )
    ]
)
