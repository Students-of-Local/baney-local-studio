import SwiftUI

public struct QueuedSummary: Identifiable, Hashable, Sendable {
    public var id: String
    public var text: String
    public var steering: Bool
    public var sending: Bool

    public init(id: String, text: String, steering: Bool = false, sending: Bool = false) {
        self.id = id
        self.text = text
        self.steering = steering
        self.sending = sending
    }

    public var status: String {
        if sending { return "Sending" }
        return steering ? "Steering" : "Queued"
    }
}

public enum SendMode: Sendable, Hashable {
    case now
    case queue
    case steer
}

public enum AttachmentSource: Sendable, Hashable, CaseIterable {
    case photos
    case camera
    case files

    public var title: String {
        switch self {
        case .photos: "Photo library"
        case .camera: "Camera"
        case .files: "Files"
        }
    }

    public var symbol: String {
        switch self {
        case .photos: "photo.on.rectangle"
        case .camera: "camera"
        case .files: "folder"
        }
    }
}

public struct ComposerState: Hashable, Sendable {
    public var model: String
    public var models: [String]
    public var effort: String?
    public var efforts: [String]
    public var access: String
    public var accessOptions: [String]
    public var attachments: Int
    public var context: Double?

    public init(
        model: String, models: [String] = [], effort: String? = nil, efforts: [String] = [],
        access: String = "Ask", accessOptions: [String] = ["Ask", "Auto edit", "Full access"], attachments: Int = 0, context: Double? = nil
    ) {
        self.model = model
        self.models = models
        self.effort = effort
        self.efforts = efforts
        self.access = access
        self.accessOptions = accessOptions
        self.attachments = attachments
        self.context = context
    }
}

public struct ThreadContent: Sendable {
    public var thread: ThreadSummary
    public var rows: [TranscriptRow]
    public var phase: WorkingPhase?
    public var approvals: [ApprovalSummary]
    public var question: (request: String, questions: [QuestionSummary])?
    public var queued: [QueuedSummary]
    public var composer: ComposerState
    public var hasEarlier: Bool
    public var foldedTurns: Set<UInt64>

    public init(
        thread: ThreadSummary, items: [TranscriptItem], phase: WorkingPhase? = nil, approvals: [ApprovalSummary] = [],
        question: (request: String, questions: [QuestionSummary])? = nil, queued: [QueuedSummary] = [],
        composer: ComposerState, hasEarlier: Bool = false
    ) {
        self.thread = thread
        self.rows = TranscriptRow.rows(items)
        self.phase = phase
        self.approvals = approvals
        self.question = question
        self.queued = queued
        self.composer = composer
        self.hasEarlier = hasEarlier
        self.foldedTurns = Set(items.compactMap { item in
            if case .turnSummary = item.kind { return item.turn }
            return nil
        })
    }

    public var busy: Bool { phase != nil }
}

public struct ThreadActions {
    public var send: (String, SendMode) -> Void = { _, _ in }
    public var stop: () -> Void = {}
    public var answerApproval: (ApprovalSummary, ApprovalChoice) -> Void = { _, _ in }
    public var answerQuestion: (String, [String: String]) -> Void = { _, _ in }
    public var sendNow: (QueuedSummary) -> Void = { _ in }
    public var removeQueued: (QueuedSummary) -> Void = { _ in }
    public var attach: (AttachmentSource) -> Void = { _ in }
    public var clearAttachments: () -> Void = {}
    public var voice: () -> Void = {}
    public var setModel: (String) -> Void = { _ in }
    public var setEffort: (String) -> Void = { _ in }
    public var setAccess: (String) -> Void = { _ in }
    public var openDiff: (String) -> Void = { _ in }
    public var loadEarlier: () -> Void = {}
    public var rename: () -> Void = {}
    public var archive: () -> Void = {}
    public var openPullRequest: () -> Void = {}

    public init() {}
}

public struct QueueStrip: View {
    let queued: [QueuedSummary]
    let sendNow: (QueuedSummary) -> Void
    let remove: (QueuedSummary) -> Void

    public init(_ queued: [QueuedSummary], sendNow: @escaping (QueuedSummary) -> Void, remove: @escaping (QueuedSummary) -> Void) {
        self.queued = queued
        self.sendNow = sendNow
        self.remove = remove
    }

    public var body: some View {
        VStack(spacing: 6) {
            ForEach(queued) { message in
                HStack(spacing: 8) {
                    Image(systemName: message.steering ? "arrow.turn.down.right" : "clock")
                        .font(.caption)
                        .foregroundStyle(Theme.secondaryText)
                        .frame(width: 16)
                        .accessibilityHidden(true)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(message.text).font(.footnote).foregroundStyle(Theme.text).lineLimit(1)
                        Text(message.status).font(.caption2).foregroundStyle(Theme.secondaryText)
                    }
                    Spacer(minLength: 4)
                    if !message.steering, !message.sending {
                        Button { sendNow(message) } label: {
                            Image(systemName: "arrow.up").frame(width: 36, height: 36).contentShape(Rectangle())
                        }
                        .accessibilityLabel("Send now")
                    }
                    if !message.sending {
                        Button { remove(message) } label: {
                            Image(systemName: "xmark").frame(width: 36, height: 36).contentShape(Rectangle())
                        }
                        .accessibilityLabel("Remove and edit")
                    }
                }
                .font(.footnote.weight(.semibold))
                .foregroundStyle(Theme.secondaryText)
                .buttonStyle(.plain)
                .padding(.leading, 12)
                .padding(.trailing, 2)
                .frame(minHeight: 44)
                .background(Theme.card, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .strokeBorder(Theme.input, style: StrokeStyle(lineWidth: 1, dash: [4, 3]))
                )
                .accessibilityElement(children: .contain)
            }
        }
    }
}

public struct Composer: View {
    @Binding var text: String
    let state: ComposerState
    let busy: Bool
    let actions: ThreadActions
    @FocusState private var focused: Bool
    @State private var sent = 0

    public init(text: Binding<String>, state: ComposerState, busy: Bool, actions: ThreadActions) {
        self._text = text
        self.state = state
        self.busy = busy
        self.actions = actions
    }

    private var empty: Bool {
        text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && state.attachments == 0
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if state.attachments > 0 {
                HStack(spacing: 6) {
                    Label(state.attachments == 1 ? "1 attachment" : "\(state.attachments) attachments", systemImage: "paperclip")
                        .labelStyle(CompactLabelStyle())
                    Button { actions.clearAttachments() } label: {
                        Image(systemName: "xmark.circle.fill")
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Remove attachments")
                }
                .font(.caption)
                .foregroundStyle(Theme.secondaryText)
                .padding(.horizontal, 4)
            }
            TextField(busy ? "Queue a follow-up" : "Message", text: $text, axis: .vertical)
                .font(.callout)
                .lineLimit(1...8)
                .focused($focused)
                .padding(.horizontal, 4)
                .padding(.top, 4)
            HStack(spacing: 6) {
                Menu {
                    ForEach(AttachmentSource.allCases, id: \.self) { source in
                        Button(source.title, systemImage: source.symbol) { actions.attach(source) }
                    }
                } label: {
                    Image(systemName: "plus")
                        .font(.body.weight(.medium))
                        .frame(width: 36, height: 36)
                        .contentShape(Circle())
                }
                .accessibilityLabel("Attach")
                chip(state.model, options: state.models, select: actions.setModel)
                if let effort = state.effort, !state.efforts.isEmpty {
                    chip(effort, options: state.efforts, select: actions.setEffort)
                }
                chip(state.access, symbol: state.access == "Full access" ? "exclamationmark.shield" : "shield", options: state.accessOptions, select: actions.setAccess)
                Spacer(minLength: 0)
                trailing
            }
            .foregroundStyle(Theme.secondaryText)
        }
        .padding(8)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 24, style: .continuous))
        .sensoryFeedback(.impact(weight: .light), trigger: sent)
    }

    @ViewBuilder
    private var trailing: some View {
        if !empty, busy {
            Menu {
                Button("Steer: send after the next tool call", systemImage: "arrow.turn.down.right") { send(.steer) }
                Button("Queue: send when the turn ends", systemImage: "clock") { send(.queue) }
            } label: {
                round("arrow.up", tint: Theme.primary)
            } primaryAction: {
                send(.queue)
            }
            .accessibilityLabel("Queue message")
            .accessibilityHint("Touch and hold to steer instead")
        } else if !empty {
            Button { send(.now) } label: { round("arrow.up", tint: Theme.primary) }
                .buttonStyle(.plain)
                .accessibilityLabel("Send")
        } else if busy {
            Button(action: actions.stop) { round("stop.fill", tint: Theme.text) }
                .buttonStyle(.plain)
                .accessibilityLabel("Stop")
        } else {
            Button(action: actions.voice) {
                Image(systemName: "mic.fill")
                    .font(.body.weight(.medium))
                    .foregroundStyle(Theme.primary)
                    .frame(width: 36, height: 36)
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Speak a message")
        }
    }

    private func round(_ symbol: String, tint: Color) -> some View {
        Image(systemName: symbol)
            .font(.body.weight(.bold))
            .foregroundStyle(Theme.canvas)
            .frame(width: 36, height: 36)
            .background(tint, in: Circle())
    }

    private func send(_ mode: SendMode) {
        let message = text
        text = ""
        sent += 1
        actions.send(message, mode)
    }

    private func chip(_ title: String, symbol: String? = nil, options: [String], select: @escaping (String) -> Void) -> some View {
        Menu {
            ForEach(options, id: \.self) { option in
                Button {
                    select(option)
                } label: {
                    if option == title { Label(option, systemImage: "checkmark") } else { Text(option) }
                }
            }
        } label: {
            HStack(spacing: 4) {
                if let symbol { Image(systemName: symbol).font(.caption2) }
                Text(title).lineLimit(1)
                Image(systemName: "chevron.up.chevron.down").font(.system(size: 8, weight: .semibold))
            }
            .font(.caption.weight(.medium))
            .padding(.horizontal, 10)
            .frame(height: 30)
            .background(Theme.hover, in: Capsule())
            .contentShape(Capsule())
        }
        .disabled(options.isEmpty)
        .fixedSize()
    }
}

public struct PinnedRequest: View {
    let content: ThreadContent
    let actions: ThreadActions

    public init(content: ThreadContent, actions: ThreadActions) {
        self.content = content
        self.actions = actions
    }

    public var body: some View {
        Group {
            if let first = content.approvals.first {
                ApprovalBody(first, position: 1, count: content.approvals.count) { choice in
                    actions.answerApproval(first, choice)
                }
                .id(first.id)
            } else if let question = content.question {
                QuestionBody(question.questions) { answers in actions.answerQuestion(question.request, answers) }
                    .id(question.request)
            }
        }
        .padding(14)
        .background(Theme.card, in: RoundedRectangle(cornerRadius: Theme.Metrics.cardRadius, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Metrics.cardRadius, style: .continuous)
                .strokeBorder(Theme.approval.opacity(0.35), lineWidth: 1)
        )
        .shadow(color: .black.opacity(0.18), radius: 16, y: 6)
    }
}

public struct Transcript: View {
    let content: ThreadContent
    let actions: ThreadActions
    @State private var unfolded: Set<UInt64> = []

    public init(content: ThreadContent, actions: ThreadActions) {
        self.content = content
        self.actions = actions
    }

    private func folded(_ turn: UInt64) -> Bool {
        content.foldedTurns.contains(turn) && !unfolded.contains(turn)
    }

    public var body: some View {
        LazyVStack(alignment: .leading, spacing: 14) {
            if content.hasEarlier {
                Button("Load earlier messages", action: actions.loadEarlier)
                    .font(.footnote.weight(.medium))
                    .foregroundStyle(Theme.primary)
                    .frame(maxWidth: .infinity, minHeight: 44)
            }
            ForEach(content.rows) { row in
                rowView(row)
            }
            if let phase = content.phase {
                WorkingRow(content.approvals.isEmpty && content.question == nil ? phase : .waitingForApproval)
            }
        }
    }

    @ViewBuilder
    private func rowView(_ row: TranscriptRow) -> some View {
        switch row {
        case let .tools(_, turn, calls):
            if !folded(turn) {
                ToolGroup(calls, openDiff: actions.openDiff)
            }
        case let .item(item):
            switch item.kind {
            case let .user(text, images):
                UserBubble(text: text, images: images)
            case let .assistant(blocks, _, error, changed):
                let visible = folded(item.turn) ? blocks.filter { if case .text = $0 { true } else { false } } : blocks
                if !visible.isEmpty || error != nil {
                    AssistantMessage(blocks: visible, error: error)
                }
                if !changed.isEmpty {
                    ChangedFilesCard(changed, open: actions.openDiff)
                }
            case .tool:
                EmptyView()
            case let .turnSummary(milliseconds, tools):
                TurnFold(milliseconds: milliseconds, tools: tools, expanded: !folded(item.turn)) {
                    withAnimation(.smooth(duration: 0.22)) {
                        if unfolded.contains(item.turn) { unfolded.remove(item.turn) } else { unfolded.insert(item.turn) }
                    }
                }
            case let .notice(level, text):
                NoticeRow(level: level, text: text)
            case let .compaction(summary, tokens):
                CompactionDivider(summary: summary, tokensBefore: tokens)
            }
        }
    }
}

#if os(iOS)
public struct ThreadScreen: View {
    let content: ThreadContent
    let actions: ThreadActions
    @State private var draft = ""

    public init(content: ThreadContent, actions: ThreadActions) {
        self.content = content
        self.actions = actions
    }

    private var subtitle: String {
        [content.thread.project, content.thread.machine, content.thread.branch].compactMap(\.self).joined(separator: " \u{00B7} ")
    }

    public var body: some View {
        ScrollView {
            Transcript(content: content, actions: actions)
                .padding(.horizontal, Theme.Metrics.gutter)
                .padding(.top, 8)
                .padding(.bottom, 12)
        }
        .defaultScrollAnchor(.bottom)
        .scrollDismissesKeyboard(.interactively)
        .background(Theme.canvas)
        .safeAreaBar(edge: .bottom, spacing: 0) {
            VStack(spacing: 8) {
                if !content.approvals.isEmpty || content.question != nil {
                    PinnedRequest(content: content, actions: actions)
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                }
                if !content.queued.isEmpty {
                    QueueStrip(content.queued, sendNow: actions.sendNow, remove: actions.removeQueued)
                }
                Composer(text: $draft, state: content.composer, busy: content.busy, actions: actions)
            }
            .padding(.horizontal, 10)
            .padding(.bottom, 4)
        }
        .navigationTitle(content.thread.title)
        .navigationBarTitleDisplayMode(.inline)
        .navigationSubtitle(subtitle)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    if content.thread.pullRequest != nil {
                        Button("Open pull request", systemImage: "arrow.triangle.pull", action: actions.openPullRequest)
                    }
                    Button("Rename", systemImage: "pencil", action: actions.rename)
                    Button("Archive", systemImage: "archivebox", role: .destructive, action: actions.archive)
                } label: {
                    Image(systemName: "ellipsis")
                }
                .accessibilityLabel("Thread options")
            }
        }
    }
}
#endif
