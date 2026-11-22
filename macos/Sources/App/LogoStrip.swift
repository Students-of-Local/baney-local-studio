import AppKit
import SwiftUI
import UniformTypeIdentifiers

@MainActor
enum LogoStrip {
    private static let frames = 12
    private static let cell: CGFloat = 64
    private static let sizes: [CGFloat] = [12, 16, 24, 48]
    private static let scale: CGFloat = 2

    static func write(into directory: URL) -> [URL] {
        var written: [URL] = []
        for (name, tint) in [("ember", Color?.none), ("mono", Theme.secondaryText)] {
            for (suffix, scheme) in [("dark", ColorScheme.dark), ("light", .light)] {
                guard let appearance = NSAppearance(named: scheme == .dark ? .darkAqua : .aqua) else { continue }
                var image: CGImage?
                appearance.performAsCurrentDrawingAppearance {
                    image = render(tint: tint, scheme: scheme)
                }
                guard let image else { continue }
                let url = directory.appendingPathComponent("logo-\(name)-\(suffix).png")
                guard let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil) else { continue }
                CGImageDestinationAddImage(destination, image, nil)
                if CGImageDestinationFinalize(destination) {
                    written.append(url)
                }
            }
        }
        return written
    }

    private static func render(tint: Color?, scheme: ColorScheme) -> CGImage? {
        let strip = HStack(spacing: 0) {
            ForEach(sizes, id: \.self) { size in
                EmberMark(tint: tint)
                    .frame(width: size, height: size)
                    .frame(width: cell, height: cell)
            }
            ForEach(0..<frames, id: \.self) { frame in
                Canvas(rendersAsynchronously: false) { context, size in
                    EmberPaint.draw(context, size: size, time: EmberMotion.period * Double(frame) / Double(frames), tint: tint)
                }
                .frame(width: cell - 16, height: cell - 16)
                .frame(width: cell, height: cell)
            }
        }
        .background(Theme.canvas)
        .environment(\.colorScheme, scheme)
        let renderer = ImageRenderer(content: strip)
        renderer.scale = scale
        return renderer.cgImage
    }
}
