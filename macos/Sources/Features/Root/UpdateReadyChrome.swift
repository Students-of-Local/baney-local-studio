import LocalStudioKit
import SwiftUI

struct UpdateReadyChrome: View {
    let updates: UpdatesStore

    var body: some View {
        if updates.state.showsChrome {
            ViewThatFits(in: .horizontal) {
                UpdateReadyButton(updates: updates, labeled: true)
                UpdateReadyButton(updates: updates, labeled: false)
            }
        }
    }
}

private struct UpdateReadyButton: View {
    let updates: UpdatesStore
    let labeled: Bool
    @State private var hovering = false

    var body: some View {
        Button { updates.apply() } label: {
            HStack(spacing: 5) {
                if applying {
                    ProgressView()
                        .controlSize(.small)
                        .frame(width: Theme.Metrics.chromeGlyph.width, height: Theme.Metrics.chromeGlyph.height)
                } else {
                    Image(systemName: "arrow.down.to.line")
                        .toolbarIcon()
                }
                if labeled {
                    Text("Update")
                        .font(.system(size: 12.5, weight: .medium))
                        .foregroundStyle(Theme.text)
                        .lineLimit(1)
                        .fixedSize()
                }
            }
            .padding(.horizontal, labeled ? Theme.Metrics.chromeGlyphInset - 1 : Theme.Metrics.chromeGlyphInset)
            .frame(height: Theme.Metrics.chromeButton)
            .background(hovering && !applying ? Theme.hover : Theme.selection, in: RoundedRectangle(cornerRadius: 7, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 7, style: .continuous).strokeBorder(Theme.selectionStroke, lineWidth: 0.8))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(applying)
        .help(help)
        .accessibilityLabel(help)
        .onHover { hovering = $0 }
    }

    private var applying: Bool {
        if case .applying = updates.state { true } else { false }
    }

    private var help: String {
        let version = updates.state.version.map { " \($0)" } ?? ""
        return applying ? "Installing version\(version)…" : "Restart to update to version\(version)"
    }
}
