import SwiftUI
#if os(iOS)
import UIKit
#else
import AppKit
#endif

public enum ToolCallKind: String, Sendable, Hashable, CaseIterable {
    case read
    case edit
    case command
    case search
    case web
    case agent
    case other

    public var symbol: String {
        switch self {
        case .read: "eye"
        case .edit: "pencil"
        case .command: "terminal"
        case .search: "magnifyingglass"
        case .web: "globe"
        case .agent: "person"
        case .other: "wrench"
        }
    }
}

public enum ToolCallState: Sendable, Hashable {
    case pending
    case running
    case succeeded
    case failed
    case aborted

    public var title: String {
        switch self {
        case .pending: "Pending"
        case .running: "Running"
        case .succeeded: "Done"
        case .failed: "Failed"
        case .aborted: "Stopped"
        }
    }
}

public struct FileChangeSummary: Identifiable, Hashable, Sendable {
    public var path: String
    public var additions: Int
    public var deletions: Int
    public var id: String { path }

    public init(path: String, additions: Int, deletions: Int) {
        self.path = path
        self.additions = additions
        self.deletions = deletions
    }

    public var name: String { (path as NSString).lastPathComponent }

    public var folder: String {
        let folder = (path as NSString).deletingLastPathComponent
        return folder.isEmpty ? "" : folder + "/"
    }
}

public struct ToolCallSummary: Identifiable, Hashable, Sendable {
    public var id: String
    public var name: String
    public var kind: ToolCallKind
    public var summary: String
    public var state: ToolCallState
    public var output: String?
    public var arguments: String?
    public var file: FileChangeSummary?

    public init(
        id: String, name: String, kind: ToolCallKind, summary: String, state: ToolCallState,
        output: String? = nil, arguments: String? = nil, file: FileChangeSummary? = nil
    ) {
        self.id = id
        self.name = name
        self.kind = kind
        self.summary = summary
        self.state = state
        self.output = output
        self.arguments = arguments
        self.file = file
    }

    public var detail: String? {
        if let output, !output.isEmpty { return output }
        if let arguments, !arguments.isEmpty { return arguments }
        return nil
    }
}

public enum AssistantBlock: Hashable, Sendable {
    case text(String)
    case thinking(String)
}

public enum NoticeLevel: Sendable, Hashable {
    case info
    case warning
    case error

    var symbol: String {
        switch self {
        case .info: "info.circle"
        case .warning: "exclamationmark.triangle"
        case .error: "xmark.octagon"
        }
    }

    var tint: Color {
        switch self {
        case .info: Theme.secondaryText
        case .warning: Theme.warning
        case .error: Theme.danger
        }
    }
}

public enum TranscriptKind: Hashable, Sendable {
    case user(text: String, images: Int)
    case assistant(blocks: [AssistantBlock], streaming: Bool, error: String?, changed: [FileChangeSummary])
    case tool(ToolCallSummary)
    case turnSummary(milliseconds: Int, tools: Int)
    case notice(NoticeLevel, String)
    case compaction(summary: String, tokensBefore: Int)
}

public struct TranscriptItem: Identifiable, Hashable, Sendable {
    public var id: UInt64
    public var turn: UInt64
    public var kind: TranscriptKind

    public init(id: UInt64, turn: UInt64, kind: TranscriptKind) {
        self.id = id
        self.turn = turn
        self.kind = kind
    }
}

public enum TranscriptRow: Identifiable, Hashable, Sendable {
    case item(TranscriptItem)
    case tools(id: UInt64, turn: UInt64, [ToolCallSummary])

    public var id: UInt64 {
        switch self {
        case let .item(item): item.id
        case let .tools(id, _, _): id
        }
    }

    public var turn: UInt64 {
        switch self {
        case let .item(item): item.turn
        case let .tools(_, turn, _): turn
        }
    }

    public static func rows(_ items: [TranscriptItem]) -> [TranscriptRow] {
        var rows: [TranscriptRow] = []
        for item in items {
            if case let .tool(call) = item.kind {
                if case let .tools(id, turn, calls) = rows.last, turn == item.turn {
                    rows[rows.count - 1] = .tools(id: id, turn: turn, calls + [call])
                } else {
                    rows.append(.tools(id: item.id, turn: item.turn, [call]))
                }
            } else {
                rows.append(.item(item))
            }
        }
        return rows
    }
}

public enum WorkDuration {
    public static func text(milliseconds: Int) -> String {
        let seconds = milliseconds / 1_000
        switch seconds {
        case ..<1: return "\(max(milliseconds, 1))ms"
        case ..<60: return "\(seconds)s"
        case ..<3_600: return "\(seconds / 60)m \(seconds % 60)s"
        default: return "\(seconds / 3_600)h \((seconds % 3_600) / 60)m"
        }
    }
}

public struct DiffStat: View {
    let additions: Int
    let deletions: Int

    public init(additions: Int, deletions: Int) {
        self.additions = additions
        self.deletions = deletions
    }

    public var body: some View {
        HStack(spacing: 4) {
            Text("+\(additions)").foregroundStyle(Theme.diffAdded)
            Text("\u{2212}\(deletions)").foregroundStyle(Theme.diffRemoved)
        }
        .font(.caption.monospacedDigit())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(additions) added, \(deletions) removed")
    }
}

public struct UserBubble: View {
    let text: String
    let images: Int
    @State private var expanded = false

    public init(text: String, images: Int = 0) {
        self.text = text
        self.images = images
    }

    private var long: Bool {
        text.count > 900 || text.reduce(0) { $0 + ($1 == "\n" ? 1 : 0) } > 12
    }

    public var body: some View {
        VStack(alignment: .trailing, spacing: 6) {
            if images > 0 {
                Label(images == 1 ? "1 image" : "\(images) images", systemImage: "photo")
                    .labelStyle(CompactLabelStyle())
                    .font(.caption)
                    .foregroundStyle(Theme.secondaryText)
                    .padding(.horizontal, 10)
                    .frame(height: 26)
                    .background(Theme.message.opacity(0.6), in: Capsule())
            }
            if !text.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text(text)
                        .font(.callout)
                        .lineSpacing(2)
                        .foregroundStyle(Theme.text)
                        .lineLimit(long && !expanded ? 12 : nil)
                        .textSelection(.enabled)
                    if long {
                        Button(expanded ? "Show less" : "Show more") { expanded.toggle() }
                            .font(.footnote.weight(.medium))
                            .foregroundStyle(Theme.primary)
                            .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
                .background(Theme.message, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                .contextMenu {
                    Button("Copy", systemImage: "doc.on.doc") { Clipboard.copy(text) }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .trailing)
        .padding(.leading, 44)
    }
}

public struct AssistantMessage: View {
    let blocks: [AssistantBlock]
    let error: String?
    let showThinking: Bool

    public init(blocks: [AssistantBlock], error: String? = nil, showThinking: Bool = true) {
        self.blocks = blocks
        self.error = error
        self.showThinking = showThinking
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                switch block {
                case let .text(text):
                    Prose(text)
                        .contextMenu {
                            Button("Copy", systemImage: "doc.on.doc") { Clipboard.copy(text) }
                        }
                case let .thinking(text):
                    if showThinking { ThinkingRow(text: text) }
                }
            }
            if let error, !error.isEmpty {
                NoticeRow(level: .error, text: error)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

public struct ThinkingRow: View {
    let text: String
    let active: Bool
    @State private var expanded = false

    public init(text: String, active: Bool = false) {
        self.text = text
        self.active = active
    }

    private var preview: String {
        let line = text.split(whereSeparator: \.isNewline).first.map(String.init) ?? ""
        return line.replacingOccurrences(of: "**", with: "")
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(.smooth(duration: 0.2)) { expanded.toggle() }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "brain")
                        .foregroundStyle(Theme.tertiaryText)
                        .accessibilityHidden(true)
                    Text(active ? "Thinking" : "Thought").fontWeight(.medium)
                    if !expanded {
                        Text(preview).foregroundStyle(Theme.tertiaryText).lineLimit(1)
                    }
                    Spacer(minLength: 0)
                    Image(systemName: expanded ? "chevron.down" : "chevron.right")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(Theme.tertiaryText)
                        .accessibilityHidden(true)
                }
                .font(.footnote)
                .foregroundStyle(Theme.secondaryText)
                .frame(minHeight: 32)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(active ? "Thinking" : "Thought")
            .accessibilityHint(expanded ? "Hides the reasoning" : "Shows the reasoning")
            if expanded {
                Prose(text, secondary: true)
                    .padding(.leading, 12)
                    .overlay(alignment: .leading) {
                        Rectangle().fill(Theme.input).frame(width: 1.5)
                    }
            }
        }
    }
}

public struct ToolStateGlyph: View {
    let state: ToolCallState

    public init(_ state: ToolCallState) {
        self.state = state
    }

    public var body: some View {
        Group {
            switch state {
            case .pending:
                Image(systemName: "clock").foregroundStyle(Theme.tertiaryText)
            case .running:
                LogoMark(tint: Theme.working).frame(width: 14, height: 14)
            case .succeeded:
                Image(systemName: "checkmark").foregroundStyle(Theme.tertiaryText)
            case .failed:
                Image(systemName: "xmark").foregroundStyle(Theme.danger)
            case .aborted:
                Image(systemName: "minus").foregroundStyle(Theme.tertiaryText)
            }
        }
        .font(.caption.weight(.semibold))
        .frame(width: 18)
        .accessibilityLabel(state.title)
    }
}

public struct ToolRow: View {
    let call: ToolCallSummary
    let openDiff: ((String) -> Void)?
    @State private var expanded = false

    public init(_ call: ToolCallSummary, openDiff: ((String) -> Void)? = nil) {
        self.call = call
        self.openDiff = openDiff
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                withAnimation(.smooth(duration: 0.2)) { expanded.toggle() }
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: call.kind.symbol)
                        .foregroundStyle(Theme.secondaryText)
                        .frame(width: 20)
                        .accessibilityHidden(true)
                    Text(call.summary.isEmpty ? call.name : call.summary)
                        .foregroundStyle(call.state == .failed ? Theme.danger : Theme.text)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 4)
                    if let file = call.file {
                        DiffStat(additions: file.additions, deletions: file.deletions)
                    }
                    ToolStateGlyph(call.state)
                    if call.detail != nil {
                        Image(systemName: expanded ? "chevron.down" : "chevron.right")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(Theme.tertiaryText)
                            .accessibilityHidden(true)
                    }
                }
                .font(.footnote)
                .frame(minHeight: 36)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(call.detail == nil)
            .accessibilityElement(children: .combine)
            .accessibilityHint(call.detail == nil ? "" : expanded ? "Hides the output" : "Shows the output")
            if expanded, let detail = call.detail {
                VStack(alignment: .leading, spacing: 6) {
                    Text(detail)
                        .font(.caption.monospaced())
                        .foregroundStyle(call.state == .failed ? Theme.danger : Theme.secondaryText)
                        .lineLimit(14)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    if let file = call.file, let openDiff {
                        Button("Open diff", systemImage: "arrow.left.arrow.right") { openDiff(file.path) }
                            .font(.footnote.weight(.medium))
                            .foregroundStyle(Theme.primary)
                            .buttonStyle(.plain)
                    }
                }
                .padding(10)
                .background(Theme.code, in: RoundedRectangle(cornerRadius: Theme.Metrics.innerRadius, style: .continuous))
                .padding(.leading, 28)
                .contextMenu {
                    Button("Copy", systemImage: "doc.on.doc") { Clipboard.copy(detail) }
                }
            }
        }
    }
}

public struct ToolGroup: View {
    let calls: [ToolCallSummary]
    let openDiff: ((String) -> Void)?
    @State private var showAll = false

    static let visible = 4

    public init(_ calls: [ToolCallSummary], openDiff: ((String) -> Void)? = nil) {
        self.calls = calls
        self.openDiff = openDiff
    }

    public var body: some View {
        let hidden = showAll ? 0 : max(calls.count - Self.visible, 0)
        VStack(alignment: .leading, spacing: 0) {
            if hidden > 0 {
                Button {
                    withAnimation(.smooth(duration: 0.2)) { showAll = true }
                } label: {
                    Text(hidden == 1 ? "1 earlier tool call" : "\(hidden) earlier tool calls")
                        .font(.footnote)
                        .foregroundStyle(Theme.secondaryText)
                        .padding(.leading, 28)
                        .frame(minHeight: 32)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            ForEach(calls.suffix(calls.count - hidden)) { call in
                ToolRow(call, openDiff: openDiff)
            }
        }
    }
}

public struct TurnFold: View {
    let milliseconds: Int
    let tools: Int
    let expanded: Bool
    let toggle: () -> Void

    public init(milliseconds: Int, tools: Int, expanded: Bool, toggle: @escaping () -> Void) {
        self.milliseconds = milliseconds
        self.tools = tools
        self.expanded = expanded
        self.toggle = toggle
    }

    public var body: some View {
        Button(action: toggle) {
            HStack(spacing: 6) {
                Text("Worked for \(WorkDuration.text(milliseconds: milliseconds))")
                if tools > 0 {
                    Text(tools == 1 ? "1 tool" : "\(tools) tools").foregroundStyle(Theme.tertiaryText)
                }
                Image(systemName: expanded ? "chevron.down" : "chevron.right")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(Theme.tertiaryText)
                    .accessibilityHidden(true)
                Rectangle().fill(Theme.hairline).frame(height: 1).padding(.leading, 4)
            }
            .font(.footnote)
            .foregroundStyle(Theme.secondaryText)
            .frame(minHeight: 32)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityHint(expanded ? "Hides the work in this turn" : "Shows the work in this turn")
    }
}

public struct ChangedFilesCard: View {
    let files: [FileChangeSummary]
    let open: ((String) -> Void)?

    public init(_ files: [FileChangeSummary], open: ((String) -> Void)? = nil) {
        self.files = files
        self.open = open
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Text(files.count == 1 ? "1 file changed" : "\(files.count) files changed")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Theme.text)
                DiffStat(additions: files.reduce(0) { $0 + $1.additions }, deletions: files.reduce(0) { $0 + $1.deletions })
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12)
            .frame(height: 38)
            Rectangle().fill(Theme.hairline).frame(height: 1)
            ForEach(files) { file in
                Button { open?(file.path) } label: {
                    HStack(spacing: 6) {
                        Text(file.name).foregroundStyle(Theme.text).fontWeight(.medium)
                        Text(file.folder).foregroundStyle(Theme.tertiaryText).truncationMode(.head)
                        Spacer(minLength: 4)
                        DiffStat(additions: file.additions, deletions: file.deletions)
                        if open != nil {
                            Image(systemName: "chevron.right")
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(Theme.tertiaryText)
                                .accessibilityHidden(true)
                        }
                    }
                    .lineLimit(1)
                    .font(.footnote)
                    .padding(.horizontal, 12)
                    .frame(minHeight: 40)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(open == nil)
                .accessibilityHint(open == nil ? "" : "Opens the diff")
            }
        }
        .background(Theme.card, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Theme.hairline))
    }
}

public struct NoticeRow: View {
    let level: NoticeLevel
    let text: String

    public init(level: NoticeLevel, text: String) {
        self.level = level
        self.text = text
    }

    public var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: level.symbol).accessibilityHidden(true)
            Text(text).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
        }
        .font(.footnote)
        .foregroundStyle(level.tint)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

public struct CompactionDivider: View {
    let summary: String
    let tokensBefore: Int
    @State private var expanded = false

    public init(summary: String, tokensBefore: Int) {
        self.summary = summary
        self.tokensBefore = tokensBefore
    }

    private var label: String {
        "Context compacted from \(tokensBefore.formatted(.number.notation(.compactName))) tokens"
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Button {
                withAnimation(.smooth(duration: 0.2)) { expanded.toggle() }
            } label: {
                HStack(spacing: 10) {
                    Rectangle().fill(Theme.hairline).frame(height: 1)
                    Label(label, systemImage: "arrow.down.right.and.arrow.up.left")
                        .labelStyle(CompactLabelStyle())
                        .font(.caption)
                        .foregroundStyle(Theme.secondaryText)
                        .fixedSize()
                    Rectangle().fill(Theme.hairline).frame(height: 1)
                }
                .frame(minHeight: 32)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(label)
            .accessibilityHint(expanded ? "Hides the summary" : "Shows the summary")
            if expanded, !summary.isEmpty {
                Prose(summary, secondary: true)
            }
        }
    }
}

public enum WorkingPhase: Hashable, Sendable {
    case working(since: Date?)
    case waitingForApproval
    case compacting
    case retrying(attempt: Int)
}

public struct WorkingRow: View {
    let phase: WorkingPhase

    public init(_ phase: WorkingPhase) {
        self.phase = phase
    }

    public var body: some View {
        HStack(spacing: 8) {
            if phase == .waitingForApproval {
                Circle().fill(Theme.approval).frame(width: 7, height: 7).frame(width: 16)
            } else {
                LogoMark(tint: Theme.working).frame(width: 16, height: 16).accessibilityHidden(true)
            }
            switch phase {
            case let .working(since?):
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    Text("Working for \(WorkDuration.text(milliseconds: max(Int(context.date.timeIntervalSince(since) * 1_000), 0) / 1_000 * 1_000))")
                        .monospacedDigit()
                        .contentTransition(.numericText())
                }
            case .working:
                Text("Working")
            case .waitingForApproval:
                Text("Waiting for approval")
            case .compacting:
                Text("Compacting context")
            case let .retrying(attempt):
                Text("Retrying, attempt \(attempt)")
            }
        }
        .font(.footnote)
        .foregroundStyle(Theme.secondaryText)
        .frame(minHeight: 28)
        .accessibilityElement(children: .combine)
    }
}

enum Clipboard {
    static func copy(_ text: String) {
        #if os(iOS)
        UIPasteboard.general.string = text
        #else
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
        #endif
    }
}
