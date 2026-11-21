import SwiftUI

public enum Samples {
    public static var now: Date { .now }

    public static let approvalThread = ThreadSummary(
        id: "t1", machine: "chapel1", title: "Wire the remote serve into the desktop app", project: "local-studio",
        status: .approval, since: now.addingTimeInterval(-90), updated: now.addingTimeInterval(-90), unread: true,
        branch: "remote-serve", additions: 412, deletions: 37, model: "Opus 5.5"
    )
    public static let approval = ApprovalSummary(
        id: "a1", command: "cargo test -p local-studio-remote --test remote_smoke -- --nocapture",
        effects: ["runs code", "network"], agent: nil
    )
    public static let failedThread = ThreadSummary(
        id: "t2", machine: "parish1", title: "Nightly build for Linux GTK", project: "local-studio",
        status: .failed, updated: now.addingTimeInterval(-600), unread: true, branch: "nightly"
    )
    public static let questionThread = ThreadSummary(
        id: "t3", machine: "rectory1", title: "Tychicus store listing", project: "tychicus",
        status: .approval, updated: now.addingTimeInterval(-240), unread: true
    )
    public static let questions = [
        QuestionSummary(header: "Listing", question: "Which channel should the store listing point to?", options: [
            .init(label: "Stable", detail: "Recommended. Only builds that passed nightly."),
            .init(label: "Nightly", detail: "Faster updates, more risk."),
        ]),
    ]
    public static let wokeThread = ThreadSummary(
        id: "t4", machine: "chapel1", title: "Watch the merge gate on PR 4", project: "local-studio",
        status: .woke, updated: now.addingTimeInterval(-1_200), note: "Checks passed. Ready for review."
    )
    public static let working = [
        ThreadSummary(id: "t5", machine: "rectory1", title: "iOS foundation and wire client", project: "local-studio",
                      status: .working, since: now.addingTimeInterval(-1_520), updated: now),
        ThreadSummary(id: "t6", machine: "parish1", title: "Fonsviva model catalog", project: "fonsviva",
                      status: .monitoring, updated: now.addingTimeInterval(-60)),
    ]
    public static let done = [
        ThreadSummary(id: "t7", machine: "chapel1", title: "Tunnel hostnames per machine", project: "porthmeus",
                      status: .done, updated: now.addingTimeInterval(-3_000), pinned: true),
        ThreadSummary(id: "t8", machine: "rectory1", title: "Silas wake word study", project: "silas",
                      status: .idle, updated: now.addingTimeInterval(-7_400)),
    ]
    public static let machines = [
        MachineSummary(id: "m1", name: "rectory1", os: "macOS", version: "0.4.0", channel: "nightly", link: .local,
                       latency: 4, working: 1, needsYou: 1, threads: 61),
        MachineSummary(id: "m2", name: "chapel1", os: "Linux", version: "0.4.0", channel: "nightly", link: .tailscale,
                       latency: 38, working: 0, needsYou: 2, threads: 54),
        MachineSummary(id: "m3", name: "parish1", os: "Linux", version: "0.3.9", channel: "stable", link: .tunnel,
                       latency: nil, health: .reconnecting(retryIn: 8), working: 1, needsYou: 1, threads: 38),
    ]
}

public struct DesignGallery: View {
    public init() {}

    public var body: some View {
        VStack(alignment: .leading, spacing: Theme.Metrics.cardSpacing) {
            HStack(spacing: 10) {
                LogoMark(style: .mark, tint: Theme.primary, animated: false).frame(width: 28, height: 28)
                LogoMark(tint: Theme.working, animated: false).frame(width: 28, height: 28)
                ForEach(StatusKind.allCases, id: \.self) { status in
                    if let symbol = status.symbol {
                        Image(systemName: symbol).foregroundStyle(status.tint)
                    }
                }
            }
            NeedsYouCard(thread: Samples.approvalThread) {
                ApprovalBody(Samples.approval, position: 1, count: 2) { _ in }
            }
            NeedsYouCard(thread: Samples.questionThread) {
                QuestionBody(Samples.questions) { _ in }
            }
            NeedsYouCard(thread: Samples.failedThread) {
                FailedBody(error: "error[E0433]: failed to resolve: use of undeclared crate `gtk4`", open: {}, retry: {})
            }
            NeedsYouCard(thread: Samples.wokeThread) {
                WokeBody(note: Samples.wokeThread.note, open: {}, markRead: {})
            }
            SectionHeader("Working", count: Samples.working.count).padding(.top, 8)
            VStack(spacing: 0) {
                ForEach(Samples.working + Samples.done) { thread in
                    ThreadLine(thread).padding(.horizontal, 14)
                    if thread.id != Samples.done.last?.id { Divider().padding(.leading, 40) }
                }
            }
            .background(Theme.card, in: RoundedRectangle(cornerRadius: Theme.Metrics.cardRadius, style: .continuous))
            ThreadMeta(Samples.approvalThread)
            ForEach(Samples.machines) { machine in
                MachineCard(machine)
            }
        }
        .padding(Theme.Metrics.gutter)
        .frame(width: 390)
        .background(Theme.canvas)
    }
}

extension Samples {
    public static var home: HomeContent {
        HomeContent(
            needsYou: [
                .approval(approvalThread, [approval, ApprovalSummary(id: "a2", command: "git push origin remote-serve", effects: ["network"])]),
                .question(questionThread, requestID: "q1", questions),
                .failed(failedThread, error: "error[E0433]: failed to resolve: use of undeclared crate `gtk4`"),
                .woke(wokeThread),
            ],
            working: working,
            doneToday: done,
            earlier: [
                ThreadSummary(id: "t9", machine: "chapel1", title: "Gantrel smoke on Linux", project: "gantrel",
                              status: .idle, updated: now.addingTimeInterval(-90_000)),
                ThreadSummary(id: "t10", machine: "parish1", title: "UntoCaesar ledger import", project: "untocaesar",
                              status: .idle, updated: now.addingTimeInterval(-180_000)),
            ],
            snoozed: [
                ThreadSummary(id: "t11", machine: "rectory1", title: "Corunis brain schema", project: "corunis",
                              status: .idle, updated: now.addingTimeInterval(-40_000)),
            ],
            machines: machines,
            moreEarlier: true
        )
    }
}

extension Samples {
    public static let threadSummary = ThreadSummary(
        id: "t1", machine: "chapel1", title: "Wire the remote serve into the desktop app", project: "local-studio",
        status: .working, since: now.addingTimeInterval(-134), updated: now, branch: "remote-serve", pullRequest: 31,
        additions: 412, deletions: 37, model: "Opus 5.5"
    )

    public static let transcript: [TranscriptItem] = [
        TranscriptItem(id: 1, turn: 1, kind: .user(text: "Add a health endpoint to the serve and show its version in Settings.", images: 0)),
        TranscriptItem(id: 2, turn: 1, kind: .assistant(blocks: [.thinking("**Plan the endpoint**\nThe serve already answers /v1/ws. A GET on /v1/health should return protocol, min_protocol and the machine version.")], streaming: false, error: nil, changed: [])),
        TranscriptItem(id: 3, turn: 1, kind: .tool(ToolCallSummary(id: "c1", name: "read", kind: .read, summary: "remote/src/server.rs", state: .succeeded, output: "pub async fn serve(listener: TcpListener, studio: Studio) -> Result<()> {"))),
        TranscriptItem(id: 4, turn: 1, kind: .tool(ToolCallSummary(id: "c2", name: "grep", kind: .search, summary: "min_protocol in remote/", state: .succeeded, output: "remote/src/hello.rs:14: pub min_protocol: u32,"))),
        TranscriptItem(id: 5, turn: 1, kind: .tool(ToolCallSummary(id: "c3", name: "edit", kind: .edit, summary: "remote/src/health.rs", state: .succeeded, file: FileChangeSummary(path: "remote/src/health.rs", additions: 48, deletions: 0)))),
        TranscriptItem(id: 6, turn: 1, kind: .tool(ToolCallSummary(id: "c4", name: "edit", kind: .edit, summary: "remote/src/server.rs", state: .succeeded, file: FileChangeSummary(path: "remote/src/server.rs", additions: 9, deletions: 2)))),
        TranscriptItem(id: 7, turn: 1, kind: .tool(ToolCallSummary(id: "c5", name: "bash", kind: .command, summary: "cargo test -p local-studio-remote", state: .succeeded, output: "test result: ok. 14 passed; 0 failed"))),
        TranscriptItem(id: 8, turn: 1, kind: .turnSummary(milliseconds: 134_000, tools: 5)),
        TranscriptItem(id: 9, turn: 1, kind: .assistant(blocks: [.text("Added `GET /v1/health`. It returns:\n\n```json\n{ \"protocol\": 2, \"min_protocol\": 2, \"version\": \"0.4.0-nightly.20261117\" }\n```\n\n- The phone reads it before pairing.\n- Settings shows the machine's version and channel.")], streaming: false, error: nil, changed: [
            FileChangeSummary(path: "remote/src/health.rs", additions: 48, deletions: 0),
            FileChangeSummary(path: "remote/src/server.rs", additions: 9, deletions: 2),
        ])),
        TranscriptItem(id: 10, turn: 2, kind: .compaction(summary: "Added the health endpoint and its test.", tokensBefore: 182_000)),
        TranscriptItem(id: 11, turn: 2, kind: .user(text: "Now run the remote smoke against chapel1.", images: 1)),
        TranscriptItem(id: 12, turn: 2, kind: .assistant(blocks: [.text("Building the serve first, then the smoke.")], streaming: true, error: nil, changed: [])),
        TranscriptItem(id: 13, turn: 2, kind: .tool(ToolCallSummary(id: "c6", name: "bash", kind: .command, summary: "cargo build -p local-studio-serve --release", state: .succeeded))),
        TranscriptItem(id: 14, turn: 2, kind: .tool(ToolCallSummary(id: "c7", name: "bash", kind: .command, summary: "ssh chapel1 systemctl --user status local-studio", state: .failed, output: "Unit local-studio.service could not be found."))),
        TranscriptItem(id: 15, turn: 2, kind: .notice(.warning, "chapel1 has no local-studio unit yet. Asking before installing one.")),
        TranscriptItem(id: 16, turn: 2, kind: .tool(ToolCallSummary(id: "c8", name: "bash", kind: .command, summary: "cargo test -p local-studio-remote --test remote_smoke", state: .pending))),
    ]

    public static var thread: ThreadContent {
        ThreadContent(
            thread: threadSummary, items: transcript, phase: .working(since: now.addingTimeInterval(-74)),
            approvals: [approval], queued: [QueuedSummary(id: "q1", text: "Then open a draft PR against main.")],
            composer: ComposerState(
                model: "Opus 5.5", models: ["Opus 5.5", "Sonnet 5.5", "Haiku 5.5"], effort: "High", efforts: ["Low", "Medium", "High", "Max"],
                access: "Ask", context: 0.42
            ),
            hasEarlier: true
        )
    }
}

public struct ThreadGallery: View {
    @State private var draft = "Also bump the nightly"

    public init() {}

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Transcript(content: Samples.thread, actions: ThreadActions())
            PinnedRequest(content: Samples.thread, actions: ThreadActions())
            QueueStrip(Samples.thread.queued, sendNow: { _ in }, remove: { _ in })
            Composer(text: $draft, state: Samples.thread.composer, busy: true, actions: ThreadActions())
        }
        .padding(Theme.Metrics.gutter)
        .frame(width: 390)
        .background(Theme.canvas)
    }
}
