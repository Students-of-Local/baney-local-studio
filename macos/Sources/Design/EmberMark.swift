import AppKit
import SwiftUI

struct EmberDrop: Equatable {
    var center: CGPoint
    var radius: CGFloat
}

enum EmberGeometry {
    static let box = CGRect(x: 46, y: 34, width: 224, height: 224)
    static let orbitBox = CGRect(x: -18, y: 2, width: 312, height: 312)
    static let blur: CGFloat = 14
    static let rest = [
        EmberDrop(center: CGPoint(x: 138, y: 158), radius: 84),
        EmberDrop(center: CGPoint(x: 214, y: 98), radius: 48),
        EmberDrop(center: CGPoint(x: 220, y: 214), radius: 30),
    ]

    static let outline: Path = {
        var path = Path()
        path.move(to: CGPoint(x: 211.25, y: 50.32))
        path.addCurve(to: CGPoint(x: 191.06, y: 56.05), control1: CGPoint(x: 203.39, y: 51.0), control2: CGPoint(x: 197.73, y: 52.61))
        path.addCurve(to: CGPoint(x: 177.75, y: 64.51), control1: CGPoint(x: 187.83, y: 57.72), control2: CGPoint(x: 185.85, y: 58.97))
        path.addCurve(to: CGPoint(x: 162.75, y: 71.88), control1: CGPoint(x: 171.69, y: 68.66), control2: CGPoint(x: 167.68, y: 70.62))
        path.addCurve(to: CGPoint(x: 143.56, y: 73.25), control1: CGPoint(x: 158.34, y: 72.99), control2: CGPoint(x: 154.79, y: 73.24))
        path.addCurve(to: CGPoint(x: 126.12, y: 74.2), control1: CGPoint(x: 134.74, y: 73.25), control2: CGPoint(x: 131.53, y: 73.42))
        path.addCurve(to: CGPoint(x: 53.38, y: 158.06), control1: CGPoint(x: 84.35, y: 80.16), control2: CGPoint(x: 53.38, y: 115.88))
        path.addCurve(to: CGPoint(x: 139.62, y: 242.71), control1: CGPoint(x: 53.38, y: 205.5), control2: CGPoint(x: 92.16, y: 243.56))
        path.addCurve(to: CGPoint(x: 167.69, y: 237.88), control1: CGPoint(x: 148.79, y: 242.55), control2: CGPoint(x: 155.11, y: 241.45))
        path.addCurve(to: CGPoint(x: 185.0, y: 234.57), control1: CGPoint(x: 177.4, y: 235.11), control2: CGPoint(x: 180.18, y: 234.59))
        path.addCurve(to: CGPoint(x: 201.62, y: 238.38), control1: CGPoint(x: 190.2, y: 234.56), control2: CGPoint(x: 193.53, y: 235.32))
        path.addCurve(to: CGPoint(x: 220.31, y: 242.46), control1: CGPoint(x: 210.46, y: 241.7), control2: CGPoint(x: 214.59, y: 242.6))
        path.addCurve(to: CGPoint(x: 247.68, y: 207.38), control1: CGPoint(x: 238.75, y: 242.01), control2: CGPoint(x: 251.85, y: 225.22))
        path.addCurve(to: CGPoint(x: 236.14, y: 188.19), control1: CGPoint(x: 246.18, y: 200.93), control2: CGPoint(x: 243.5, y: 196.5))
        path.addCurve(to: CGPoint(x: 228.03, y: 177.19), control1: CGPoint(x: 231.24, y: 182.66), control2: CGPoint(x: 229.74, y: 180.62))
        path.addCurve(to: CGPoint(x: 230.04, y: 150.3), control1: CGPoint(x: 223.86, y: 168.82), control2: CGPoint(x: 224.64, y: 158.41))
        path.addCurve(to: CGPoint(x: 240.31, y: 139.3), control1: CGPoint(x: 232.06, y: 147.25), control2: CGPoint(x: 235.3, y: 143.77))
        path.addCurve(to: CGPoint(x: 254.94, y: 122.88), control1: CGPoint(x: 248.05, y: 132.39), control2: CGPoint(x: 251.58, y: 128.42))
        path.addCurve(to: CGPoint(x: 261.24, y: 90.38), control1: CGPoint(x: 260.79, y: 113.24), control2: CGPoint(x: 263.05, y: 101.51))
        path.addCurve(to: CGPoint(x: 247.83, y: 64.3), control1: CGPoint(x: 259.61, y: 80.41), control2: CGPoint(x: 255.04, y: 71.51))
        path.addCurve(to: CGPoint(x: 219.44, y: 50.56), control1: CGPoint(x: 240.05, y: 56.52), control2: CGPoint(x: 230.46, y: 51.89))
        path.addCurve(to: CGPoint(x: 211.25, y: 50.32), control1: CGPoint(x: 217.9, y: 50.38), control2: CGPoint(x: 212.48, y: 50.21))
        path.closeSubpath()
        return path
    }()

    static func fit(_ box: CGRect, in rect: CGRect) -> CGAffineTransform {
        let scale = min(rect.width / box.width, rect.height / box.height)
        let width = box.width * scale
        let height = box.height * scale
        return CGAffineTransform(translationX: rect.minX + (rect.width - width) / 2, y: rect.minY + (rect.height - height) / 2)
            .scaledBy(x: scale, y: scale)
            .translatedBy(x: -box.minX, y: -box.minY)
    }
}

enum EmberMotion {
    static let period = 5.2

    static func drops(at time: Double) -> [EmberDrop] {
        let turn = 2 * Double.pi * time
        let main = EmberGeometry.rest[0]
        func orbit(_ drop: EmberDrop, speed: Double, reach: Double, beat: Double, offset: Double) -> EmberDrop {
            let dx = Double(drop.center.x - main.center.x)
            let dy = Double(drop.center.y - main.center.y)
            let angle = atan2(dy, dx) + turn * speed
            let distance = (dx * dx + dy * dy).squareRoot() + reach * sin(turn * beat + offset)
            return EmberDrop(
                center: CGPoint(x: Double(main.center.x) + distance * cos(angle), y: Double(main.center.y) + distance * sin(angle)),
                radius: drop.radius
            )
        }
        return [
            EmberDrop(center: main.center, radius: main.radius * (1 + 0.04 * sin(turn / 1.3))),
            orbit(EmberGeometry.rest[1], speed: 1 / 2.6, reach: 10, beat: 1 / 1.3, offset: 0),
            orbit(EmberGeometry.rest[2], speed: -1 / 3.4, reach: 12, beat: 1 / 1.7, offset: 1),
        ]
    }
}

enum EmberPaint {
    static let body = Gradient(stops: [
        .init(color: Color(red: 0.898, green: 0.541, blue: 0.306), location: 0),
        .init(color: Color(red: 0.776, green: 0.353, blue: 0.251), location: 0.55),
        .init(color: Color(red: 0.478, green: 0.141, blue: 0.282), location: 1),
    ])
    static let glow = Gradient(stops: [
        .init(color: Color(red: 1, green: 0.957, blue: 0.839), location: 0),
        .init(color: Color(red: 0.965, green: 0.796, blue: 0.447, opacity: 0.95), location: 0.3),
        .init(color: Color(red: 0.933, green: 0.624, blue: 0.361, opacity: 0.32), location: 0.65),
        .init(color: Color(red: 0.933, green: 0.624, blue: 0.361, opacity: 0), location: 1),
    ])

    static func draw(_ context: GraphicsContext, size: CGSize, time: Double?, tint: Color?) {
        let bounds = CGRect(origin: .zero, size: size)
        guard let time else {
            let fit = EmberGeometry.fit(EmberGeometry.box, in: bounds)
            let shape = EmberGeometry.outline.applying(fit)
            if let tint {
                context.fill(shape, with: .color(tint))
                return
            }
            var clipped = context
            clipped.clip(to: shape)
            fill(clipped, drops: EmberGeometry.rest, fit: fit)
            return
        }
        let fit = EmberGeometry.fit(EmberGeometry.orbitBox, in: bounds)
        let drops = EmberMotion.drops(at: time)
        let scale = fit.a
        context.drawLayer { outer in
            outer.drawLayer { goo in
                var matrix = ColorMatrix()
                matrix.a4 = 28
                matrix.a5 = -12
                goo.addFilter(.colorMatrix(matrix))
                goo.addFilter(.blur(radius: EmberGeometry.blur * scale))
                goo.drawLayer { layer in
                    for drop in drops {
                        layer.fill(circle(drop.center, drop.radius).applying(fit), with: .color(.black))
                    }
                }
            }
            outer.blendMode = .sourceIn
            outer.drawLayer { body in
                if let tint {
                    body.fill(Path(bounds), with: .color(tint))
                } else {
                    fill(body, drops: drops, fit: fit)
                }
            }
        }
    }

    private static func fill(_ context: GraphicsContext, drops: [EmberDrop], fit: CGAffineTransform) {
        let cover = Path(CGRect(x: -40, y: -40, width: 380, height: 380)).applying(fit)
        context.fill(cover, with: .linearGradient(
            body, startPoint: CGPoint(x: 80, y: 60).applying(fit), endPoint: CGPoint(x: 240, y: 240).applying(fit)
        ))
        for drop in drops.sorted(by: { $0.radius < $1.radius }) {
            let light = CGPoint(x: drop.center.x - 0.22 * drop.radius, y: drop.center.y - 0.3 * drop.radius).applying(fit)
            context.fill(circle(drop.center, drop.radius * 1.3).applying(fit), with: .radialGradient(
                glow, center: light, startRadius: 0, endRadius: drop.radius * 1.2 * fit.a
            ))
        }
    }

    private static func circle(_ center: CGPoint, _ radius: CGFloat) -> Path {
        Path(ellipseIn: CGRect(x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2))
    }
}

struct EmberMark: View {
    var working = false
    var tint: Color?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var visible = true

    var body: some View {
        Group {
            if working, !reduceMotion {
                TimelineView(.animation(minimumInterval: 1.0 / 30, paused: !visible)) { timeline in
                    canvas(time: timeline.date.timeIntervalSinceReferenceDate)
                }
                .background(WindowVisibility(visible: $visible))
            } else {
                canvas(time: nil)
            }
        }
    }

    private func canvas(time: Double?) -> some View {
        Canvas(rendersAsynchronously: false) { context, size in
            EmberPaint.draw(context, size: size, time: time, tint: tint)
        }
    }
}

private struct WindowVisibility: NSViewRepresentable {
    @Binding var visible: Bool

    func makeNSView(context: Context) -> WindowVisibilityView {
        let view = WindowVisibilityView()
        view.changed = { visible = $0 }
        return view
    }

    func updateNSView(_ view: WindowVisibilityView, context: Context) {
        view.changed = { visible = $0 }
    }
}

private final class WindowVisibilityView: NSView {
    var changed: ((Bool) -> Void)?
    private weak var observed: NSWindow?

    override func viewWillMove(toWindow newWindow: NSWindow?) {
        super.viewWillMove(toWindow: newWindow)
        if let observed {
            NotificationCenter.default.removeObserver(self, name: NSWindow.didChangeOcclusionStateNotification, object: observed)
        }
        observed = nil
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let window {
            NotificationCenter.default.addObserver(
                self, selector: #selector(occlusionChanged(_:)), name: NSWindow.didChangeOcclusionStateNotification, object: window
            )
            observed = window
        }
        report()
    }

    @objc private func occlusionChanged(_ notification: Notification) {
        report()
    }

    private func report() {
        let visible = window?.occlusionState.contains(.visible) ?? false
        DispatchQueue.main.async { [changed] in changed?(visible) }
    }
}
