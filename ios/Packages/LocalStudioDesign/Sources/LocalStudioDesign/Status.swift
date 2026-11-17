import SwiftUI
#if canImport(UIKit)
import UIKit
#else
import AppKit
#endif

public enum StatusKind: String, Sendable, Hashable, CaseIterable {
    case idle
    case working
    case approval
    case failed
    case done
    case woke
    case monitoring

    public var title: String {
        switch self {
        case .idle: "Idle"
        case .working: "Working"
        case .approval: "Approval"
        case .failed: "Failed"
        case .done: "Done"
        case .woke: "Woke"
        case .monitoring: "Monitoring"
        }
    }

    public var symbol: String? {
        switch self {
        case .idle: nil
        case .working: "hourglass"
        case .approval: "hand.raised.fill"
        case .failed: "xmark"
        case .done: "checkmark"
        case .woke: "alarm"
        case .monitoring: "eye"
        }
    }

    public var tint: Color {
        switch self {
        case .idle: Theme.secondaryText
        case .working, .monitoring: Theme.working
        case .approval: Theme.warning
        case .failed: Theme.danger
        case .done: Theme.success
        case .woke: Theme.woke
        }
    }

    public var needsYou: Bool {
        self == .approval || self == .failed || self == .woke
    }
}

public struct StatusBadge: View {
    let status: StatusKind
    let since: Date?
    let updated: Date
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var attention = 0

    public init(status: StatusKind, since: Date? = nil, updated: Date) {
        self.status = status
        self.since = since
        self.updated = updated
    }

    public var body: some View {
        Group {
            if let symbol = status.symbol {
                HStack(spacing: 4) {
                    if status == .working {
                        LogoMark(tint: status.tint)
                            .frame(width: 12, height: 12)
                    } else {
                        Image(systemName: symbol)
                            .font(.caption.weight(.medium))
                            .symbolEffect(.bounce.up, options: .nonRepeating, value: attention)
                    }
                    if status == .working, let since {
                        TimelineView(.periodic(from: .now, by: 1)) { context in
                            Text("\(status.title) \(RelativeAge.duration(since: since, now: context.date))")
                                .contentTransition(.numericText())
                        }
                    } else {
                        Text(status.title)
                    }
                }
                .foregroundStyle(status.tint)
            } else {
                TimelineView(.periodic(from: .now, by: 30)) { context in
                    Text(RelativeAge.text(updated, now: context.date))
                }
                .foregroundStyle(Theme.secondaryText)
            }
        }
        .font(.caption)
        .monospacedDigit()
        .lineLimit(1)
        .fixedSize()
        .accessibilityElement(children: .combine)
        .onChange(of: status) { _, status in
            guard status == .approval || status == .failed, !reduceMotion else { return }
            attention += 1
        }
    }
}

public struct ProjectTile: View {
    let name: String
    let icon: String?
    let size: CGFloat
    let neutral: Bool

    public init(name: String?, icon: String? = nil, size: CGFloat = Theme.Metrics.tile) {
        self.name = name ?? "No project"
        self.icon = name == nil ? "minus" : icon
        self.size = size
        self.neutral = name == nil
    }

    public var body: some View {
        Group {
            if let icon, !icon.isEmpty, Self.isSymbol(icon) {
                Image(systemName: icon)
                    .font(.system(size: size * 0.6, weight: .semibold))
                    .symbolRenderingMode(.monochrome)
                    .foregroundStyle(.white)
            } else if let icon, !icon.isEmpty {
                Text(icon)
                    .font(.system(size: size * 0.7))
            } else {
                Text(initials)
                    .font(.system(size: size * 0.47, weight: .bold, design: .rounded))
                    .foregroundStyle(.white)
            }
        }
        .frame(width: size, height: size)
        .background(color.gradient, in: RoundedRectangle(cornerRadius: size * 0.28, style: .continuous))
        .accessibilityHidden(true)
    }

    private var initials: String {
        let words = name.split { !$0.isLetter && !$0.isNumber }
        let letters = words.count > 1 ? words.prefix(2).compactMap(\.first) : Array(name.prefix(2))
        return String(letters).uppercased()
    }

    private var color: Color {
        if neutral { return Theme.tertiaryText }
        let palette: [Color] = [.indigo, .purple, .pink, .orange, .teal, .blue, .green, .brown]
        let hash = name.unicodeScalars.reduce(UInt32(5_381)) { ($0 &* 33) &+ $1.value }
        return palette[Int(hash % UInt32(palette.count))]
    }

    private static func isSymbol(_ name: String) -> Bool {
        #if canImport(UIKit)
        UIImage(systemName: name) != nil
        #else
        NSImage(systemSymbolName: name, accessibilityDescription: nil) != nil
        #endif
    }
}
