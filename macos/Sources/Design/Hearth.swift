import SwiftUI

struct HearthGlow: View {
    var lit = false
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        GeometryReader { proxy in
            let width = proxy.size.width
            let height = proxy.size.height
            let strength = (scheme == .dark ? 0.13 : 0.08) * (lit ? 1.5 : 1)
            ZStack {
                RadialGradient(
                    colors: [Theme.primary.opacity(strength), Theme.primary.opacity(0)],
                    center: .center, startRadius: 0, endRadius: width * 0.55
                )
                .frame(width: width * 1.1, height: width * 1.1)
                .scaleEffect(x: 1, y: min(0.36, 420 / max(width * 1.1, 1)))
                .position(x: width * 0.5, y: height + 30)
                RadialGradient(
                    colors: [Theme.woke.opacity(strength * 0.55), Theme.woke.opacity(0)],
                    center: .center, startRadius: 0, endRadius: width * 0.3
                )
                .frame(width: width * 0.6, height: width * 0.6)
                .scaleEffect(x: 1, y: min(0.5, 300 / max(width * 0.6, 1)))
                .position(x: width * 0.22, y: height + 10)
            }
        }
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

struct EmberEdge: ViewModifier {
    let lit: Bool
    let radius: CGFloat

    func body(content: Content) -> some View {
        content
            .overlay {
                if lit {
                    RoundedRectangle(cornerRadius: radius, style: .continuous)
                        .strokeBorder(
                            LinearGradient(
                                colors: [Theme.primary.opacity(0.9), Theme.woke.opacity(0.7), Theme.primary.opacity(0.35)],
                                startPoint: .topLeading, endPoint: .bottomTrailing
                            ),
                            lineWidth: 1.2
                        )
                        .allowsHitTesting(false)
                }
            }
            .background {
                if lit {
                    RoundedRectangle(cornerRadius: radius + 4, style: .continuous)
                        .fill(Theme.primary.opacity(0.16))
                        .padding(-3)
                        .blur(radius: 14)
                        .allowsHitTesting(false)
                }
            }
    }
}

extension View {
    func emberEdge(_ lit: Bool, radius: CGFloat) -> some View {
        modifier(EmberEdge(lit: lit, radius: radius))
    }
}
