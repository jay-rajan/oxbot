// swift-tools-version:5.3
import PackageDescription

let package = Package(
  name: "tauri-plugin-open-record",
  platforms: [
    .macOS(.v10_13),
    .iOS(.v13),
  ],
  products: [
    .library(
      name: "tauri-plugin-open-record",
      type: .static,
      targets: ["tauri-plugin-open-record"])
  ],
  dependencies: [
    .package(name: "Tauri", path: "../.tauri/tauri-api")
  ],
  targets: [
    .target(
      name: "tauri-plugin-open-record",
      dependencies: [
        .byName(name: "Tauri")
      ],
      path: "Sources")
  ]
)
