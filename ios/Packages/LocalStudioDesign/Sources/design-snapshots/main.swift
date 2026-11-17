import AppKit
import LocalStudioDesign
import SwiftUI

@MainActor
func render(_ scheme: ColorScheme, to url: URL) throws {
    let view = DesignGallery()
        .environment(\.colorScheme, scheme)
    let renderer = ImageRenderer(content: view)
    renderer.scale = 2
    let appearance = NSAppearance(named: scheme == .dark ? .darkAqua : .aqua)
    var image: CGImage?
    appearance?.performAsCurrentDrawingAppearance {
        image = renderer.cgImage
    }
    guard let image else { throw CocoaError(.fileWriteUnknown) }
    let rep = NSBitmapImageRep(cgImage: image)
    guard let data = rep.representation(using: .png, properties: [:]) else { throw CocoaError(.fileWriteUnknown) }
    try data.write(to: url)
    print("\(url.path) \(image.width)x\(image.height)")
}

let output = URL(fileURLWithPath: CommandLine.arguments.dropFirst().first ?? "snapshots", isDirectory: true)
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
try MainActor.assumeIsolated {
    try render(.dark, to: output.appendingPathComponent("gallery-dark.png"))
    try render(.light, to: output.appendingPathComponent("gallery-light.png"))
}
