import SwiftUI

public struct NeedsYouCard<Content: View>: View {
    let thread: ThreadSummary
    let content: Content

    public init(thread: ThreadSummary, @ViewBuilder content: () -> Content) {
        self.thread = thread
        self.content = content()
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 6) {
                    ProjectTile(name: thread.project, icon: thread.projectIcon)
                    Text([thread.project, thread.machine].compactMap(\.self).joined(separator: " on "))
                        .font(.caption)
                        .foregroundStyle(Theme.secondaryText)
                        .lineLimit(1)
                    Spacer(minLength: 6)
                    StatusBadge(status: thread.status, since: thread.since, updated: thread.updated)
                }
                Text(thread.title)
                    .font(.headline)
                    .foregroundStyle(Theme.text)
                    .lineLimit(2)
            }
            .accessibilityElement(children: .combine)
            content
        }
        .padding(14)
        .background(Theme.card, in: RoundedRectangle(cornerRadius: Theme.Metrics.cardRadius, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Metrics.cardRadius, style: .continuous)
                .strokeBorder(thread.status.tint.opacity(thread.status.needsYou ? 0.3 : 0), lineWidth: 1)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Metrics.cardRadius, style: .continuous)
                .strokeBorder(Theme.hairline, lineWidth: thread.status.needsYou ? 0 : 1)
        )
    }
}

public struct ApprovalBody: View {
    let approval: ApprovalSummary
    let position: Int
    let count: Int
    let answer: (ApprovalChoice) -> Void
    @State private var answered: ApprovalChoice?
    @State private var expanded = false

    public init(_ approval: ApprovalSummary, position: Int = 1, count: Int = 1, answer: @escaping (ApprovalChoice) -> Void) {
        self.approval = approval
        self.position = position
        self.count = count
        self.answer = answer
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                Image(systemName: "hand.raised.fill").accessibilityHidden(true)
                Text(approval.kind).fontWeight(.semibold)
                if let agent = approval.agent, !agent.isEmpty {
                    Text(agent).foregroundStyle(Theme.secondaryText)
                }
                Spacer(minLength: 8)
                if count > 1 {
                    Text("\(position)/\(count)")
                        .monospacedDigit()
                        .foregroundStyle(Theme.secondaryText)
                        .accessibilityLabel("\(position) of \(count) requests")
                }
            }
            .font(.footnote)
            .foregroundStyle(Theme.approval)
            Text(approval.command)
                .font(.footnote.monospaced())
                .foregroundStyle(Theme.text)
                .lineSpacing(2)
                .lineLimit(expanded ? nil : 5)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
                .background(Theme.code, in: RoundedRectangle(cornerRadius: Theme.Metrics.innerRadius, style: .continuous))
                .overlay(RoundedRectangle(cornerRadius: Theme.Metrics.innerRadius, style: .continuous).strokeBorder(Theme.hairline))
                .onTapGesture { expanded.toggle() }
                .accessibilityAction(named: expanded ? "Collapse command" : "Show full command") { expanded.toggle() }
            if !approval.effects.isEmpty {
                ChipRow(approval.effects)
            }
            HStack(spacing: 8) {
                Menu {
                    Button("Always allow this session", systemImage: "checkmark.seal") { choose(.alwaysAllowSession) }
                    Button("Decline", systemImage: "xmark", role: .destructive) { choose(.deny) }
                } label: {
                    Image(systemName: "ellipsis")
                        .frame(width: 36, height: 36)
                        .contentShape(Rectangle())
                }
                .accessibilityLabel("More approval options")
                Spacer(minLength: 0)
                Button("Decline") { choose(.deny) }
                    .buttonStyle(.bordered)
                Button("Approve") { choose(.approve) }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.primary)
            }
            .controlSize(.large)
            .buttonBorderShape(.capsule)
            .disabled(answered != nil)
        }
        .sensoryFeedback(trigger: answered) { _, choice in
            switch choice {
            case .approve, .alwaysAllowSession: .success
            case .deny: .impact(weight: .medium)
            case nil: nil
            }
        }
    }

    private func choose(_ choice: ApprovalChoice) {
        answered = choice
        answer(choice)
    }
}

public enum ApprovalChoice: Sendable, Hashable {
    case approve
    case deny
    case alwaysAllowSession
}

public struct FailedBody: View {
    let error: String
    let open: () -> Void
    let retry: () -> Void

    public init(error: String, open: @escaping () -> Void, retry: @escaping () -> Void) {
        self.error = error
        self.open = open
        self.retry = retry
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(error)
                .font(.footnote.monospaced())
                .foregroundStyle(Theme.danger)
                .lineLimit(3)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
                .background(Theme.danger.opacity(0.08), in: RoundedRectangle(cornerRadius: Theme.Metrics.innerRadius, style: .continuous))
            HStack(spacing: 8) {
                Spacer(minLength: 0)
                Button("Open", action: open).buttonStyle(.bordered)
                Button("Retry", systemImage: "arrow.clockwise", action: retry)
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.primary)
            }
            .controlSize(.large)
            .buttonBorderShape(.capsule)
        }
    }
}

public struct WokeBody: View {
    let note: String?
    let open: () -> Void
    let markRead: () -> Void

    public init(note: String?, open: @escaping () -> Void, markRead: @escaping () -> Void) {
        self.note = note
        self.open = open
        self.markRead = markRead
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let note, !note.isEmpty {
                Text(note)
                    .font(.subheadline)
                    .foregroundStyle(Theme.secondaryText)
                    .lineLimit(3)
            }
            HStack(spacing: 8) {
                Spacer(minLength: 0)
                Button("Mark read", action: markRead).buttonStyle(.bordered)
                Button("Open", action: open)
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.primary)
            }
            .controlSize(.large)
            .buttonBorderShape(.capsule)
        }
    }
}

public struct ChipRow: View {
    let items: [String]

    public init(_ items: [String]) {
        self.items = items
    }

    public var body: some View {
        FlowLayout(spacing: 6) {
            ForEach(items, id: \.self) { item in
                Text(item)
                    .font(.caption)
                    .foregroundStyle(Theme.secondaryText)
                    .padding(.horizontal, 8)
                    .frame(height: 22)
                    .background(Theme.hover, in: Capsule())
                    .overlay(Capsule().strokeBorder(Theme.hairline))
            }
        }
    }
}

public struct FlowLayout: Layout {
    let spacing: CGFloat

    public init(spacing: CGFloat = 6) {
        self.spacing = spacing
    }

    public func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let rows = arrange(subviews, width: proposal.width ?? .infinity)
        let width = rows.map(\.width).max() ?? 0
        let height = rows.map(\.height).reduce(0, +) + spacing * CGFloat(max(rows.count - 1, 0))
        return CGSize(width: width, height: height)
    }

    public func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for row in arrange(subviews, width: bounds.width) {
            var x = bounds.minX
            for index in row.indices {
                let size = subviews[index].sizeThatFits(.unspecified)
                subviews[index].place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
                x += size.width + spacing
            }
            y += row.height + spacing
        }
    }

    private struct Row {
        var indices: [Int] = []
        var width: CGFloat = 0
        var height: CGFloat = 0
    }

    private func arrange(_ subviews: Subviews, width: CGFloat) -> [Row] {
        var rows: [Row] = [Row()]
        for index in subviews.indices {
            let size = subviews[index].sizeThatFits(.unspecified)
            let extra = rows[rows.count - 1].indices.isEmpty ? size.width : size.width + spacing
            if rows[rows.count - 1].width + extra > width, !rows[rows.count - 1].indices.isEmpty {
                rows.append(Row())
            }
            let gap: CGFloat = rows[rows.count - 1].indices.isEmpty ? 0 : spacing
            rows[rows.count - 1].indices.append(index)
            rows[rows.count - 1].width += size.width + gap
            rows[rows.count - 1].height = max(rows[rows.count - 1].height, size.height)
        }
        return rows.filter { !$0.indices.isEmpty }
    }
}
