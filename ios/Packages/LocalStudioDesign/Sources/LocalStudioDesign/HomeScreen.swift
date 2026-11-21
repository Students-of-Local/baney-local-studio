import SwiftUI

public enum NeedsYouItem: Identifiable, Hashable, Sendable {
    case approval(ThreadSummary, [ApprovalSummary])
    case question(ThreadSummary, requestID: String, [QuestionSummary])
    case failed(ThreadSummary, error: String)
    case woke(ThreadSummary)

    public var thread: ThreadSummary {
        switch self {
        case let .approval(thread, _), let .question(thread, _, _), let .failed(thread, _), let .woke(thread): thread
        }
    }

    public var id: String { thread.id }
}

public enum SnoozeChoice: String, Sendable, CaseIterable {
    case hour
    case tonight
    case tomorrow

    public var title: String {
        switch self {
        case .hour: "1 hour"
        case .tonight: "Tonight"
        case .tomorrow: "Tomorrow"
        }
    }

    public func date(from now: Date = .now, calendar: Calendar = .current) -> Date {
        switch self {
        case .hour:
            return now.addingTimeInterval(3_600)
        case .tonight:
            let evening = calendar.date(bySettingHour: 18, minute: 0, second: 0, of: now) ?? now
            return evening > now.addingTimeInterval(1_800) ? evening : now.addingTimeInterval(3 * 3_600)
        case .tomorrow:
            let tomorrow = calendar.date(byAdding: .day, value: 1, to: now) ?? now
            return calendar.date(bySettingHour: 9, minute: 0, second: 0, of: tomorrow) ?? tomorrow
        }
    }
}

public struct HomeContent: Sendable {
    public var needsYou: [NeedsYouItem]
    public var working: [ThreadSummary]
    public var doneToday: [ThreadSummary]
    public var earlier: [ThreadSummary]
    public var snoozed: [ThreadSummary]
    public var machines: [MachineSummary]
    public var moreEarlier: Bool

    public init(
        needsYou: [NeedsYouItem] = [], working: [ThreadSummary] = [], doneToday: [ThreadSummary] = [],
        earlier: [ThreadSummary] = [], snoozed: [ThreadSummary] = [], machines: [MachineSummary] = [], moreEarlier: Bool = false
    ) {
        self.needsYou = needsYou
        self.working = working
        self.doneToday = doneToday
        self.earlier = earlier
        self.snoozed = snoozed
        self.machines = machines
        self.moreEarlier = moreEarlier
    }

    public var health: MachineHealth {
        if machines.isEmpty { return .offline }
        if machines.contains(where: { $0.health == .offline }) { return .offline }
        if machines.contains(where: { $0.health != .online }) { return .slow }
        return .online
    }
}

public struct HomeActions {
    public var open: (ThreadSummary) -> Void = { _ in }
    public var answerApproval: (ThreadSummary, ApprovalSummary, ApprovalChoice) -> Void = { _, _, _ in }
    public var answerQuestion: (ThreadSummary, String, [String: String]) -> Void = { _, _, _ in }
    public var retry: (ThreadSummary) -> Void = { _ in }
    public var markRead: (ThreadSummary) -> Void = { _ in }
    public var snooze: (ThreadSummary, SnoozeChoice) -> Void = { _, _ in }
    public var settle: (ThreadSummary) -> Void = { _ in }
    public var pin: (ThreadSummary, Bool) -> Void = { _, _ in }
    public var rename: (ThreadSummary) -> Void = { _ in }
    public var archive: (ThreadSummary) -> Void = { _ in }
    public var newThread: () -> Void = {}
    public var voice: () -> Void = {}
    public var search: () -> Void = {}
    public var machines: () -> Void = {}
    public var loadEarlier: () -> Void = {}
    public var refresh: @Sendable () async -> Void = {}

    public init() {}
}

#if os(iOS)
public struct HomeScreen: View {
    let content: HomeContent
    let actions: HomeActions
    @State private var showSnoozed = false
    @State private var swiped = 0

    public init(content: HomeContent, actions: HomeActions) {
        self.content = content
        self.actions = actions
    }

    public var body: some View {
        List {
            if content.needsYou.isEmpty {
                Section {
                    ContentUnavailableView {
                        Label("Nothing needs you", systemImage: "checkmark.circle")
                    } description: {
                        Text(content.working.isEmpty ? "No threads are running." : "\(content.working.count) working.")
                    }
                    .listRowBackground(Color.clear)
                }
            } else {
                Section {
                    ForEach(content.needsYou) { item in
                        card(item)
                            .listRowBackground(Color.clear)
                            .listRowInsets(EdgeInsets(top: 5, leading: 0, bottom: 5, trailing: 0))
                            .listRowSeparator(.hidden)
                            .swipeActions(edge: .leading, allowsFullSwipe: true) { leading(item.thread, item: item) }
                            .swipeActions(edge: .trailing, allowsFullSwipe: false) { trailing(item.thread) }
                            .contextMenu { menu(item.thread) }
                    }
                }
            }
            rows("Working", content.working)
            rows("Done today", content.doneToday)
            rows("Earlier", content.earlier, more: content.moreEarlier)
            if !content.snoozed.isEmpty {
                Section {
                    if showSnoozed {
                        ForEach(content.snoozed) { thread in row(thread) }
                    }
                } header: {
                    Button {
                        withAnimation(.smooth(duration: 0.22)) { showSnoozed.toggle() }
                    } label: {
                        HStack(spacing: 4) {
                            SectionHeader("Snoozed", count: content.snoozed.count)
                            Image(systemName: showSnoozed ? "chevron.down" : "chevron.right")
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(Theme.tertiaryText)
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityHint(showSnoozed ? "Hides snoozed threads" : "Shows snoozed threads")
                }
            }
        }
        .listStyle(.insetGrouped)
        .listSectionSpacing(14)
        .scrollContentBackground(.hidden)
        .background(Theme.canvas)
        .refreshable { await actions.refresh() }
        .navigationTitle("Needs you")
        .navigationSubtitle(subtitle)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button(action: actions.machines) {
                    HStack(spacing: 6) {
                        Image(systemName: "desktopcomputer")
                        HealthDot(content.health)
                    }
                }
                .accessibilityLabel("Machines, \(content.health.title)")
            }
        }
        .safeAreaBar(edge: .bottom) { cluster }
        .sensoryFeedback(.success, trigger: swiped)
    }

    private var subtitle: String {
        let waiting = content.needsYou.count
        let working = content.working.count
        return [waiting > 0 ? "\(waiting) waiting" : nil, working > 0 ? "\(working) working" : nil]
            .compactMap(\.self)
            .joined(separator: ", ")
    }

    @ViewBuilder
    private func card(_ item: NeedsYouItem) -> some View {
        switch item {
        case let .approval(thread, approvals):
            if let first = approvals.first {
                NeedsYouCard(thread: thread) {
                    ApprovalBody(first, position: 1, count: approvals.count) { choice in
                        actions.answerApproval(thread, first, choice)
                    }
                    .id(first.id)
                }
                .onTapGesture { actions.open(thread) }
            }
        case let .question(thread, request, questions):
            NeedsYouCard(thread: thread) {
                QuestionBody(questions) { answers in actions.answerQuestion(thread, request, answers) }
                    .id(request)
            }
        case let .failed(thread, error):
            NeedsYouCard(thread: thread) {
                FailedBody(error: error, open: { actions.open(thread) }, retry: { actions.retry(thread) })
            }
        case let .woke(thread):
            NeedsYouCard(thread: thread) {
                WokeBody(note: thread.note, open: { actions.open(thread) }, markRead: { actions.markRead(thread) })
            }
        }
    }

    @ViewBuilder
    private func rows(_ title: String, _ threads: [ThreadSummary], more: Bool = false) -> some View {
        if !threads.isEmpty {
            Section {
                ForEach(threads) { thread in row(thread) }
                if more {
                    Button("Show more", action: actions.loadEarlier)
                        .font(.subheadline)
                        .foregroundStyle(Theme.primary)
                        .listRowBackground(Theme.card)
                }
            } header: {
                SectionHeader(title, count: threads.count)
            }
        }
    }

    private func row(_ thread: ThreadSummary) -> some View {
        Button { actions.open(thread) } label: {
            ThreadLine(thread)
        }
        .buttonStyle(.plain)
        .listRowBackground(Theme.card)
        .listRowSeparatorTint(Theme.hairline)
        .swipeActions(edge: .leading, allowsFullSwipe: true) { leading(thread, item: nil) }
        .swipeActions(edge: .trailing, allowsFullSwipe: false) { trailing(thread) }
        .contextMenu { menu(thread) }
    }

    @ViewBuilder
    private func leading(_ thread: ThreadSummary, item: NeedsYouItem?) -> some View {
        if case let .approval(_, approvals) = item, approvals.count == 1, let only = approvals.first {
            Button {
                swiped += 1
                actions.answerApproval(thread, only, .approve)
            } label: {
                Label("Approve", systemImage: "checkmark")
            }
            .tint(Theme.success)
        } else {
            Button {
                swiped += 1
                actions.markRead(thread)
            } label: {
                Label("Mark read", systemImage: "envelope.open")
            }
            .tint(Theme.primary)
        }
    }

    @ViewBuilder
    private func trailing(_ thread: ThreadSummary) -> some View {
        Button { actions.snooze(thread, .hour) } label: {
            Label("Snooze", systemImage: "moon.zzz")
        }
        .tint(Theme.woke)
        Button { actions.settle(thread) } label: {
            Label("Settle", systemImage: "checkmark.circle")
        }
        .tint(Theme.secondaryText)
        Button { actions.pin(thread, !thread.pinned) } label: {
            Label(thread.pinned ? "Unpin" : "Pin", systemImage: thread.pinned ? "pin.slash" : "pin")
        }
        .tint(Theme.warning)
    }

    @ViewBuilder
    private func menu(_ thread: ThreadSummary) -> some View {
        Button("Open", systemImage: "arrow.up.right") { actions.open(thread) }
        Button("Mark read", systemImage: "envelope.open") { actions.markRead(thread) }
        Menu("Snooze", systemImage: "moon.zzz") {
            ForEach(SnoozeChoice.allCases, id: \.self) { choice in
                Button(choice.title) { actions.snooze(thread, choice) }
            }
        }
        Button("Settle", systemImage: "checkmark.circle") { actions.settle(thread) }
        Button(thread.pinned ? "Unpin" : "Pin", systemImage: thread.pinned ? "pin.slash" : "pin") { actions.pin(thread, !thread.pinned) }
        Button("Rename", systemImage: "pencil") { actions.rename(thread) }
        Button("Archive", systemImage: "archivebox", role: .destructive) { actions.archive(thread) }
    }

    private var cluster: some View {
        GlassEffectContainer(spacing: 18) {
            HStack(spacing: 18) {
                Button(action: actions.newThread) {
                    Image(systemName: "plus")
                        .font(.title2.weight(.medium))
                        .frame(width: 54, height: 54)
                }
                .buttonStyle(.glass)
                .buttonBorderShape(.circle)
                .accessibilityLabel("New thread")
                Button(action: actions.voice) {
                    Image(systemName: "mic.fill")
                        .font(.title.weight(.medium))
                        .frame(width: 72, height: 72)
                }
                .buttonStyle(.glassProminent)
                .buttonBorderShape(.circle)
                .tint(Theme.primary)
                .accessibilityLabel("Speak a message")
                Button(action: actions.search) {
                    Image(systemName: "magnifyingglass")
                        .font(.title2.weight(.medium))
                        .frame(width: 54, height: 54)
                }
                .buttonStyle(.glass)
                .buttonBorderShape(.circle)
                .accessibilityLabel("Search threads")
            }
        }
        .padding(.bottom, 6)
    }
}
#endif
