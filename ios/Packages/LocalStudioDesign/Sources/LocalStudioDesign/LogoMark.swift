import SwiftUI

struct LogoShapes: Sendable {
    let cloud: Path
    let leaves: [Path]
    let mark: Path
    let cloudBox: CGRect
    let extent: CGFloat

    static let shared = LogoShapes()

    private init() {
        let lobe: CGFloat = 87
        let topLeft = CGPoint(x: 196.9, y: 187.0)
        let topRight = CGPoint(x: 327.1, y: 187.0)
        let bottomLeft = CGPoint(x: 131.7, y: 299.9)
        let bottomRight = CGPoint(x: 392.3, y: 299.9)
        let base = CGRect(x: 131.7, y: 244, width: 392.3 - 131.7, height: 386.7 - 244)
        func circle(_ center: CGPoint, _ radius: CGFloat) -> CGPath {
            CGPath(ellipseIn: CGRect(x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2), transform: nil)
        }
        let disc = circle(CGPoint(x: 262.008, y: 262.008), 262.008)
        let cloud = [topRight, bottomLeft, bottomRight]
            .reduce(circle(topLeft, lobe)) { $0.union(circle($1, lobe)) }
            .union(CGPath(rect: base, transform: nil))
        let leaves = [
            circle(topLeft, lobe).intersection(circle(bottomLeft, lobe)),
            circle(topLeft, lobe).intersection(circle(topRight, lobe)),
            circle(topRight, lobe).intersection(circle(bottomRight, lobe)),
        ]
        let mark = leaves.reduce(disc.subtracting(cloud)) { $0.union($1) }
        self.cloud = Path(cloud)
        self.leaves = leaves.map { Path($0) }
        self.mark = Path(mark)
        self.cloudBox = cloud.boundingBoxOfPath
        self.extent = 525
    }

    static func fit(_ box: CGRect, in bounds: CGRect) -> CGAffineTransform {
        let scale = min(bounds.width / box.width, bounds.height / box.height)
        let x = bounds.minX + (bounds.width - box.width * scale) / 2
        let y = bounds.minY + (bounds.height - box.height * scale) / 2
        return CGAffineTransform(translationX: -box.minX, y: -box.minY)
            .concatenating(CGAffineTransform(scaleX: scale, y: scale))
            .concatenating(CGAffineTransform(translationX: x, y: y))
    }
}

enum LogoMotion {
    static let leavesDuration = 1.4
    static let leafDelay = 0.2
    static let leafTimes: [Double] = [0, 0.2, 0.45, 1]
    static let leafOpacity: [Double] = [0.3, 1, 0.3, 0.3]
    static let leafScale: [Double] = [0.86, 1.14, 0.92, 0.86]
    static let cloudOpacity = 0.22

    static func sample(_ values: [Double], at fraction: Double) -> Double {
        let clamped = min(max(fraction, 0), 1)
        guard let upper = leafTimes.firstIndex(where: { $0 >= clamped }), upper > 0 else { return values.first ?? 0 }
        let span = leafTimes[upper] - leafTimes[upper - 1]
        let local = span > 0 ? (clamped - leafTimes[upper - 1]) / span : 1
        return values[upper - 1] + (values[upper] - values[upper - 1]) * easeInOut(local)
    }

    static func leafFraction(_ index: Int, at time: Double) -> Double {
        let local = time - Double(index) * leafDelay
        guard local > 0 else { return 0 }
        return local.truncatingRemainder(dividingBy: leavesDuration) / leavesDuration
    }

    private static func easeInOut(_ x: Double) -> Double {
        var low = 0.0
        var high = 1.0
        var t = x
        for _ in 0..<24 {
            t = (low + high) / 2
            if bezier(t, 0.42, 0.58) < x { low = t } else { high = t }
        }
        return bezier(t, 0, 1)
    }

    private static func bezier(_ t: Double, _ first: Double, _ second: Double) -> Double {
        let inverse = 1 - t
        return 3 * inverse * inverse * t * first + 3 * inverse * t * t * second + t * t * t
    }
}

public struct LogoMark: View {
    public enum Style: Sendable {
        case leaves
        case mark
    }

    let style: Style
    let tint: Color
    let animated: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    public init(style: Style = .leaves, tint: Color = Theme.primary, animated: Bool = true) {
        self.style = style
        self.tint = tint
        self.animated = animated
    }

    public var body: some View {
        if style == .leaves, animated, !reduceMotion {
            TimelineView(.animation(minimumInterval: 1.0 / 30)) { context in
                canvas(time: context.date.timeIntervalSinceReferenceDate)
            }
            .accessibilityHidden(true)
        } else {
            canvas(time: nil)
                .accessibilityHidden(true)
        }
    }

    private func canvas(time: Double?) -> some View {
        Canvas { context, size in
            let shapes = LogoShapes.shared
            let bounds = CGRect(origin: .zero, size: size)
            switch style {
            case .mark:
                let fit = LogoShapes.fit(CGRect(x: 0, y: 0, width: shapes.extent, height: shapes.extent), in: bounds)
                context.fill(shapes.mark.applying(fit), with: .color(tint))
            case .leaves:
                let fit = LogoShapes.fit(shapes.cloudBox, in: bounds)
                var cloud = context
                cloud.opacity = LogoMotion.cloudOpacity
                cloud.fill(shapes.cloud.applying(fit), with: .color(tint))
                for (index, leaf) in shapes.leaves.enumerated() {
                    let path = leaf.applying(fit)
                    var layer = context
                    if let time {
                        let fraction = LogoMotion.leafFraction(index, at: time)
                        let scale = LogoMotion.sample(LogoMotion.leafScale, at: fraction)
                        let center = CGPoint(x: path.boundingRect.midX, y: path.boundingRect.midY)
                        layer.opacity = LogoMotion.sample(LogoMotion.leafOpacity, at: fraction)
                        layer.translateBy(x: center.x, y: center.y)
                        layer.scaleBy(x: scale, y: scale)
                        layer.translateBy(x: -center.x, y: -center.y)
                    }
                    layer.fill(path, with: .color(tint))
                }
            }
        }
    }
}
