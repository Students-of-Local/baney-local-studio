import LocalStudioRemote
import SwiftUI

struct ThreadRoute: Hashable {
    let machine: String
    let thread: String
}

struct RootView: View {
    let studio: StudioModel
    let smoke: Bool
    @State private var pairing = false

    var body: some View {
        NavigationStack {
            List {
                if studio.machines.isEmpty {
                    Text("No machines. Pair one with a localstudio:// link.")
                }
                ForEach(studio.machines) { machine in
                    MachineSection(machine: machine)
                }
                Section("This app") {
                Text("\(Channel.current) · \(Channel.version)").accessibilityIdentifier("app-version")
            }
            if smoke {
                    Section("Timings") {
                        Text(Perf.shared.json)
                            .font(.caption.monospaced())
                            .accessibilityIdentifier("perf-timings")
                    }
                }
            }
            .navigationTitle("Local Studio")
            .toolbar {
                Button("Pair") { pairing = true }
                    .accessibilityIdentifier("pair")
            }
            .sheet(isPresented: $pairing) {
                PairSheet(studio: studio)
            }
            .navigationDestination(for: ThreadRoute.self) { route in
                if let machine = studio.machines.first(where: { $0.id == route.machine }) {
                    ThreadDebugView(machine: machine, thread: route.thread)
                }
            }
        }
    }
}

struct MachineSection: View {
    let machine: MachineModel

    var body: some View {
        Section {
            Text("link \(machine.link.label)").accessibilityIdentifier("link-state")
            if let detail = machine.link.detail {
                Text(detail).font(.caption).accessibilityIdentifier("link-detail")
            }
            Text("list \(machine.listSource.rawValue)").accessibilityIdentifier("list-source")
            Text("cache \(machine.cacheSaved ? "saved" : "pending")").accessibilityIdentifier("cache-state")
            if let note = machine.lastNotification {
                Text("\(note.kind.rawValue): \(note.title)").accessibilityIdentifier("last-notification")
            }
            if let error = machine.lastError {
                Text(error).foregroundStyle(.red).accessibilityIdentifier("machine-error")
            }
            ForEach(machine.threads) { row in
                NavigationLink(value: ThreadRoute(machine: machine.id, thread: row.id)) {
                    Text("\(row.title) · \(row.status.rawValue)\(row.unread ? " · unread" : "")")
                }
                .accessibilityIdentifier("thread-\(row.id)")
            }
        } header: {
            Text(machine.name).accessibilityIdentifier("machine-header")
        }
    }
}

struct PairSheet: View {
    let studio: StudioModel
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""

    var body: some View {
        NavigationStack {
            Form {
                TextField("localstudio://pair?url=...&id=...#secret", text: $text, axis: .vertical)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .accessibilityIdentifier("pair-link")
                if !text.isEmpty && PairingLink(string: text) == nil {
                    Text("Not a pairing link").foregroundStyle(.red)
                }
            }
            .navigationTitle("Pair a machine")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Pair") {
                        if let link = PairingLink(string: text) {
                            studio.pair(link)
                            dismiss()
                        }
                    }
                    .disabled(PairingLink(string: text) == nil)
                    .accessibilityIdentifier("pair-confirm")
                }
            }
        }
    }
}

struct ThreadDebugView: View {
    let machine: MachineModel
    let thread: String
    @State private var draft = ""

    var body: some View {
        List {
            let approvals = machine.approvals[thread] ?? []
            if !approvals.isEmpty {
                Section("Approvals") {
                    ForEach(approvals) { approval in
                        VStack(alignment: .leading) {
                            Text(approval.command).font(.body.monospaced())
                            HStack {
                                Button("Approve") { machine.answer(approval, thread: thread, answer: .approve) }
                                    .accessibilityIdentifier("approve-\(approval.requestId)")
                                Spacer()
                                Button("Deny", role: .destructive) { machine.answer(approval, thread: thread, answer: .deny) }
                                    .accessibilityIdentifier("deny-\(approval.requestId)")
                            }
                            .buttonStyle(.borderless)
                        }
                    }
                }
            }
            let queue = machine.queues[thread] ?? []
            if !queue.isEmpty {
                Section("Queue") {
                    ForEach(queue) { queued in
                        Text("\(queued.steering ? "steer" : "queue"): \(queued.text)")
                            .accessibilityIdentifier("queued-\(queued.id)")
                    }
                }
            }
            if let info = machine.infos[thread] {
                Text("\(info.busy ? "busy" : "idle") · \(info.model?.id ?? "no model") · \(info.access.rawValue)")
                    .font(.caption)
                    .accessibilityIdentifier("thread-info")
            }
            if let window = machine.transcripts[thread] {
                Section("Transcript, newest first") {
                    Text("\(window.earlier) earlier · \(window.items.count) loaded")
                        .accessibilityIdentifier("transcript-window")
                    if window.earlier > 0 {
                        Button("Load earlier") { machine.loadEarlier(thread) }
                            .accessibilityIdentifier("load-earlier")
                    }
                    ForEach(window.items.reversed()) { item in
                        Text(Self.describe(item))
                            .font(.caption.monospaced())
                            .accessibilityIdentifier("item-\(item.id)")
                    }
                }
            }
        }
        .navigationTitle(machine.thread(thread)?.title ?? thread)
        .navigationBarTitleDisplayMode(.inline)
        .safeAreaInset(edge: .bottom) {
            HStack {
                TextField("Message", text: $draft)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("composer")
                Button("Send") { submit(.default) }
                    .accessibilityIdentifier("send")
                Button("Queue") { submit(.queue) }
                    .accessibilityIdentifier("send-queue")
            }
            .padding()
            .background(.bar)
        }
        .onAppear { machine.watch(thread) }
        .onDisappear { machine.unwatch(thread) }
    }

    private func submit(_ delivery: Delivery) {
        machine.send(draft, thread: thread, delivery: delivery)
        draft = ""
    }

    static func describe(_ item: Item) -> String {
        switch item.kind {
        case let .user(user):
            "user: \(user.text)"
        case let .assistant(assistant):
            "assistant\(assistant.streaming ? " (streaming)" : ""): \(assistant.blocks.map(\.text).joined(separator: "\n"))"
        case let .tool(tool):
            "tool \(tool.name) \(tool.state.rawValue): \(tool.summary)\(tool.output.isEmpty ? "" : " -> " + tool.output.trimmingCharacters(in: .whitespacesAndNewlines))"
        case let .turnSummary(summary):
            "turn \(summary.turn) done in \(summary.durationMs) ms, \(summary.toolCount) tools"
        case let .notice(notice):
            "notice \(notice.level.rawValue): \(notice.text)"
        case let .compaction(compaction):
            "compaction: \(compaction.summary)"
        }
    }
}
