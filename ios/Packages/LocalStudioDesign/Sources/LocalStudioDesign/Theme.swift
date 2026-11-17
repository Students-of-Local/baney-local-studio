import SwiftUI
#if canImport(UIKit)
import UIKit
#else
import AppKit
#endif

public struct RGBA: Sendable, Hashable {
    public var red: Double
    public var green: Double
    public var blue: Double
    public var alpha: Double

    public init(red: Double, green: Double, blue: Double, alpha: Double = 1) {
        self.red = red
        self.green = green
        self.blue = blue
        self.alpha = alpha
    }

    static func gray(_ value: Double, _ alpha: Double = 1) -> RGBA {
        RGBA(red: value, green: value, blue: value, alpha: alpha)
    }
}

public enum Theme {
    public static let canvas = dynamic(.gray(0.0392), .gray(0.9895))
    public static let card = dynamic(.gray(0.068), .gray(1))
    public static let raised = dynamic(.gray(0.086), .gray(1))
    public static let primary = dynamic(RGBA(red: 0.2044, green: 0.4198, blue: 0.9449), RGBA(red: 0.1043, green: 0.3071, blue: 0.8463))
    public static let hairline = dynamic(.gray(1, 0.06), RGBA(red: 0.894, green: 0.894, blue: 0.906))
    public static let input = dynamic(.gray(1, 0.08), RGBA(red: 0.831, green: 0.831, blue: 0.847))
    public static let hover = dynamic(.gray(1, 0.04), RGBA(red: 0.957, green: 0.957, blue: 0.961))
    public static let message = dynamic(.gray(1, 0.06), RGBA(red: 0.957, green: 0.957, blue: 0.961))
    public static let code = dynamic(.gray(1, 0.036), RGBA(red: 0.957, green: 0.957, blue: 0.961, alpha: 0.6))
    public static let text = dynamic(.gray(1, 0.85), .gray(0, 0.85))
    public static let secondaryText = dynamic(.gray(0.5059), RGBA(red: 0.443, green: 0.443, blue: 0.478))
    public static let tertiaryText = dynamic(.gray(0.36), RGBA(red: 0.62, green: 0.62, blue: 0.65))
    public static let success = dynamic(RGBA(red: 0.204, green: 0.827, blue: 0.6), RGBA(red: 0.02, green: 0.588, blue: 0.412))
    public static let warning = dynamic(RGBA(red: 0.984, green: 0.749, blue: 0.141), RGBA(red: 0.851, green: 0.467, blue: 0.024))
    public static let danger = dynamic(RGBA(red: 0.973, green: 0.443, blue: 0.443), RGBA(red: 0.863, green: 0.149, blue: 0.149))
    public static let working = dynamic(RGBA(red: 0.22, green: 0.741, blue: 0.973), RGBA(red: 0.008, green: 0.518, blue: 0.78))
    public static let woke = dynamic(RGBA(red: 0.984, green: 0.573, blue: 0.235), RGBA(red: 0.918, green: 0.345, blue: 0.047))
    public static let approval = woke
    public static let diffAdded = dynamic(RGBA(red: 0.34, green: 0.80, blue: 0.50), RGBA(red: 0.10, green: 0.50, blue: 0.24))
    public static let diffRemoved = dynamic(RGBA(red: 0.96, green: 0.42, blue: 0.42), RGBA(red: 0.76, green: 0.16, blue: 0.16))

    public enum Metrics {
        public static let gutter: CGFloat = 16
        public static let cardRadius: CGFloat = 16
        public static let innerRadius: CGFloat = 8
        public static let rowHeight: CGFloat = 44
        public static let tile: CGFloat = 16
        public static let cardSpacing: CGFloat = 10
    }

    static func dynamic(_ dark: RGBA, _ light: RGBA) -> Color {
        #if canImport(UIKit)
        Color(uiColor: UIColor { traits in
            traits.userInterfaceStyle == .dark ? uiColor(dark) : uiColor(light)
        })
        #else
        Color(nsColor: NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? nsColor(dark) : nsColor(light)
        })
        #endif
    }

    #if canImport(UIKit)
    private static func uiColor(_ value: RGBA) -> UIColor {
        UIColor(red: value.red, green: value.green, blue: value.blue, alpha: value.alpha)
    }
    #else
    private static func nsColor(_ value: RGBA) -> NSColor {
        NSColor(srgbRed: value.red, green: value.green, blue: value.blue, alpha: value.alpha)
    }
    #endif
}

public enum Motion {
    public static func structural(_ reduced: Bool) -> Animation? {
        reduced ? nil : .smooth(duration: 0.22)
    }

    public static func confirm(_ reduced: Bool) -> Animation? {
        reduced ? nil : .snappy(duration: 0.2)
    }
}

public enum RelativeAge {
    public static func text(_ date: Date, now: Date = .now) -> String {
        let seconds = max(0, now.timeIntervalSince(date))
        switch seconds {
        case ..<60: return "now"
        case ..<3_600: return "\(Int(seconds / 60))m"
        case ..<86_400: return "\(Int(seconds / 3_600))h"
        default: return "\(Int(seconds / 86_400))d"
        }
    }

    public static func duration(since date: Date, now: Date = .now) -> String {
        let seconds = Int(max(0, now.timeIntervalSince(date)))
        switch seconds {
        case ..<60: return "\(seconds)s"
        case ..<3_600: return "\(seconds / 60)m"
        default: return "\(seconds / 3_600)h \(seconds % 3_600 / 60)m"
        }
    }
}
