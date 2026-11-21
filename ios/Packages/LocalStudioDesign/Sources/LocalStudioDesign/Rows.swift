import SwiftUI

public struct ThreadLine: View {
    let thread: ThreadSummary
    let showMachine: Bool

    public init(_ thread: ThreadSummary, showMachine: Bool = true) {
        self.thread = thread
        self.showMachine = showMachine
    }

    public var body: some View {
        HStack(spacing: 10) {
            ProjectTile(name: thread.project, icon: thread.projectIcon)
            Text(thread.title)
                .font(.subheadline.weight(thread.unread ? .semibold : .regular))
                .foregroundStyle(Theme.text)
                .lineLimit(1)
            if thread.pinned {
                Image(systemName: "pin.fill")
                    .font(.caption2)
                    .foregroundStyle(Theme.tertiaryText)
                    .accessibilityLabel("Pinned")
            }
            Spacer(minLength: 6)
            if showMachine {
                Text(thread.machine)
                    .font(.caption)
                    .foregroundStyle(Theme.tertiaryText)
                    .lineLimit(1)
            }
            StatusBadge(status: thread.status, since: thread.since, updated: thread.updated)
        }
        .frame(minHeight: Theme.Metrics.rowHeight)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }
}

public struct ThreadMeta: View {
    let thread: ThreadSummary

    public init(_ thread: ThreadSummary) {
        self.thread = thread
    }

    public var body: some View {
        HStack(spacing: 8) {
            if let branch = thread.branch {
                Label(branch, systemImage: "arrow.triangle.branch")
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            if let number = thread.pullRequest {
                Text("#\(number)")
                    .padding(.horizontal, 6)
                    .frame(height: 18)
                    .background(Theme.hover, in: Capsule())
                    .overlay(Capsule().strokeBorder(Theme.hairline))
                    .accessibilityLabel("Pull request \(number)")
            }
            if thread.additions > 0 || thread.deletions > 0 {
                HStack(spacing: 4) {
                    Text("+\(thread.additions)").foregroundStyle(Theme.diffAdded)
                    Text("\u{2212}\(thread.deletions)").foregroundStyle(Theme.diffRemoved)
                }
                .monospacedDigit()
                .accessibilityLabel("\(thread.additions) added, \(thread.deletions) removed")
            }
            Spacer(minLength: 0)
            if let model = thread.model {
                Text(model).lineLimit(1)
            }
        }
        .font(.caption)
        .foregroundStyle(Theme.secondaryText)
        .labelStyle(CompactLabelStyle())
    }
}

struct CompactLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 3) {
            configuration.icon.imageScale(.small)
            configuration.title
        }
    }
}

public struct SectionHeader: View {
    let title: String
    let count: Int?

    public init(_ title: String, count: Int? = nil) {
        self.title = title
        self.count = count
    }

    public var body: some View {
        HStack(spacing: 6) {
            Text(title)
            if let count {
                Text("\(count)").monospacedDigit().foregroundStyle(Theme.tertiaryText)
            }
        }
        .font(.footnote.weight(.semibold))
        .foregroundStyle(Theme.secondaryText)
        .textCase(nil)
    }
}
