import SwiftUI

#if os(iOS)

public struct ProjectSummary: Identifiable, Hashable, Sendable {
    public var id: String
    public var name: String
    public var machine: String
    public var path: String
    public var icon: String?
    public var branch: String?

    public init(id: String, name: String, machine: String, path: String, icon: String? = nil, branch: String? = nil) {
        self.id = id
        self.name = name
        self.machine = machine
        self.path = path
        self.icon = icon
        self.branch = branch
    }
}

public struct NewThreadDraft: Hashable, Sendable {
    public var machine: String
    public var project: String?
    public var worktree: Bool
    public var model: String
    public var effort: String?
    public var access: String
    public var text: String

    public init(machine: String, project: String? = nil, worktree: Bool = false, model: String, effort: String? = nil, access: String = "Ask", text: String = "") {
        self.machine = machine
        self.project = project
        self.worktree = worktree
        self.model = model
        self.effort = effort
        self.access = access
        self.text = text
    }
}

public struct NewThreadSheet: View {
    @State private var draft: NewThreadDraft
    let machines: [MachineSummary]
    let projects: [ProjectSummary]
    let composer: ComposerState
    let start: (NewThreadDraft) -> Void
    let cancel: () -> Void
    @FocusState private var focused: Bool

    public init(
        draft: NewThreadDraft, machines: [MachineSummary], projects: [ProjectSummary], composer: ComposerState,
        start: @escaping (NewThreadDraft) -> Void, cancel: @escaping () -> Void
    ) {
        self._draft = State(initialValue: draft)
        self.machines = machines
        self.projects = projects
        self.composer = composer
        self.start = start
        self.cancel = cancel
    }

    private var machineProjects: [ProjectSummary] {
        projects.filter { $0.machine == draft.machine }
    }

    private var ready: Bool {
        !draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && machines.first(where: { $0.name == draft.machine })?.health != .offline
    }

    public var body: some View {
        Form {
            Section {
                TextField("What should it do?", text: $draft.text, axis: .vertical)
                    .lineLimit(4...12)
                    .focused($focused)
            }
            Section("Where") {
                Picker("Machine", selection: $draft.machine) {
                    ForEach(machines) { machine in
                        HStack {
                            Text(machine.name)
                            HealthDot(machine.health)
                        }
                        .tag(machine.name)
                    }
                }
                Picker("Project", selection: $draft.project) {
                    Text("No project").tag(String?.none)
                    ForEach(machineProjects) { project in
                        Label {
                            Text(project.name)
                        } icon: {
                            ProjectTile(name: project.name, icon: project.icon)
                        }
                        .tag(Optional(project.id))
                    }
                }
                Toggle("New worktree", isOn: $draft.worktree)
                    .disabled(draft.project == nil)
            }
            Section("Agent") {
                Picker("Model", selection: $draft.model) {
                    ForEach(composer.models, id: \.self) { Text($0).tag($0) }
                }
                if !composer.efforts.isEmpty {
                    Picker("Effort", selection: $draft.effort) {
                        ForEach(composer.efforts, id: \.self) { Text($0).tag(Optional($0)) }
                    }
                }
                Picker("Access", selection: $draft.access) {
                    ForEach(composer.accessOptions, id: \.self) { Text($0).tag($0) }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.canvas)
        .tint(Theme.primary)
        .navigationTitle("New thread")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel", action: cancel)
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Start") { start(draft) }
                    .fontWeight(.semibold)
                    .disabled(!ready)
            }
        }
        .onAppear { focused = true }
        .onChange(of: draft.machine) {
            if let project = draft.project, !machineProjects.contains(where: { $0.id == project }) {
                draft.project = nil
            }
        }
    }
}

public struct MachinesSheet: View {
    let machines: [MachineSummary]
    let pair: () -> Void
    let open: (MachineSummary) -> Void
    let forget: (MachineSummary) -> Void

    public init(machines: [MachineSummary], pair: @escaping () -> Void, open: @escaping (MachineSummary) -> Void, forget: @escaping (MachineSummary) -> Void) {
        self.machines = machines
        self.pair = pair
        self.open = open
        self.forget = forget
    }

    public var body: some View {
        ScrollView {
            VStack(spacing: Theme.Metrics.cardSpacing) {
                ForEach(machines) { machine in
                    Button { open(machine) } label: { MachineCard(machine) }
                        .buttonStyle(.plain)
                        .contextMenu {
                            Button("Forget this machine", systemImage: "trash", role: .destructive) { forget(machine) }
                        }
                }
                Button(action: pair) {
                    Label("Pair a machine", systemImage: "qrcode.viewfinder")
                        .font(.headline)
                        .frame(maxWidth: .infinity, minHeight: 50)
                }
                .buttonStyle(.bordered)
                .buttonBorderShape(.roundedRectangle(radius: Theme.Metrics.cardRadius))
                .tint(Theme.primary)
            }
            .padding(Theme.Metrics.gutter)
        }
        .background(Theme.canvas)
        .navigationTitle("Machines")
    }
}

public enum PairingStep: Hashable, Sendable {
    case scan
    case connecting(machine: String, link: LinkKind)
    case confirm(machine: String, os: String, version: String, channel: String)
    case paired(machine: String)
    case failed(String)
}

public struct PairingView: View {
    let step: PairingStep
    let scanner: AnyView
    let paste: (String) -> Void
    let accept: () -> Void
    let retry: () -> Void
    let done: () -> Void
    @State private var link = ""
    @State private var pasting = false

    public init(
        step: PairingStep, scanner: AnyView = AnyView(Theme.code), paste: @escaping (String) -> Void,
        accept: @escaping () -> Void, retry: @escaping () -> Void, done: @escaping () -> Void
    ) {
        self.step = step
        self.scanner = scanner
        self.paste = paste
        self.accept = accept
        self.retry = retry
        self.done = done
    }

    public var body: some View {
        VStack(spacing: 20) {
            switch step {
            case .scan:
                scanner
                    .aspectRatio(1, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: 24, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: 24, style: .continuous).strokeBorder(Theme.hairline))
                    .accessibilityLabel("Camera viewfinder")
                Text("On the machine, run `local-studio pair` or open Settings, Remote, Pair a phone. Point the camera at the code.")
                    .font(.subheadline)
                    .foregroundStyle(Theme.secondaryText)
                    .multilineTextAlignment(.center)
                if pasting {
                    HStack {
                        TextField("localstudio://pair?...", text: $link)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .font(.footnote.monospaced())
                        Button("Pair") { paste(link) }.disabled(link.isEmpty)
                    }
                    .padding(12)
                    .background(Theme.card, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                } else {
                    Button("Paste a pairing link instead") { pasting = true }
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(Theme.primary)
                }
            case let .connecting(machine, link):
                Spacer()
                LogoMark(tint: Theme.working).frame(width: 56, height: 56)
                Text("Reaching \(machine) over \(link.title)").font(.headline)
                Spacer()
            case let .confirm(machine, os, version, channel):
                Spacer()
                Image(systemName: os == "macOS" ? "laptopcomputer" : "server.rack")
                    .font(.system(size: 44))
                    .foregroundStyle(Theme.secondaryText)
                VStack(spacing: 4) {
                    Text("Pair with \(machine)?").font(.title2.weight(.semibold))
                    Text("\(os), \(version) \(channel)").font(.subheadline).foregroundStyle(Theme.secondaryText)
                }
                Label("Encrypted end to end with a key only this phone and \(machine) hold", systemImage: "lock.fill")
                    .font(.footnote)
                    .foregroundStyle(Theme.secondaryText)
                    .multilineTextAlignment(.center)
                Spacer()
                Button(action: accept) {
                    Text("Pair").font(.headline).frame(maxWidth: .infinity, minHeight: 50)
                }
                .buttonStyle(.borderedProminent)
                .buttonBorderShape(.capsule)
                .tint(Theme.primary)
            case let .paired(machine):
                Spacer()
                Image(systemName: "checkmark.circle.fill").font(.system(size: 56)).foregroundStyle(Theme.success)
                Text("\(machine) is paired").font(.title2.weight(.semibold))
                Text("This phone keeps its own key for \(machine). Forget it any time from Machines.")
                    .font(.subheadline)
                    .foregroundStyle(Theme.secondaryText)
                    .multilineTextAlignment(.center)
                Spacer()
                Button(action: done) {
                    Text("Done").font(.headline).frame(maxWidth: .infinity, minHeight: 50)
                }
                .buttonStyle(.borderedProminent)
                .buttonBorderShape(.capsule)
                .tint(Theme.primary)
            case let .failed(reason):
                Spacer()
                Image(systemName: "xmark.octagon.fill").font(.system(size: 56)).foregroundStyle(Theme.danger)
                Text("Pairing failed").font(.title2.weight(.semibold))
                Text(reason).font(.subheadline).foregroundStyle(Theme.secondaryText).multilineTextAlignment(.center)
                Spacer()
                Button(action: retry) {
                    Text("Try again").font(.headline).frame(maxWidth: .infinity, minHeight: 50)
                }
                .buttonStyle(.borderedProminent)
                .buttonBorderShape(.capsule)
                .tint(Theme.primary)
            }
        }
        .padding(Theme.Metrics.gutter + 8)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.canvas)
        .navigationTitle("Pair a machine")
        .sensoryFeedback(trigger: step) { _, new in
            switch new {
            case .paired: .success
            case .failed: .error
            default: nil
            }
        }
    }
}

public enum VoicePhase: Hashable, Sendable {
    case listening
    case review
    case sending
}

public struct VoiceSheet: View {
    let phase: VoicePhase
    let transcript: String
    let level: Double
    let target: String
    let stop: () -> Void
    let send: (String) -> Void
    let cancel: () -> Void
    @State private var edited: String?

    public init(
        phase: VoicePhase, transcript: String, level: Double, target: String,
        stop: @escaping () -> Void, send: @escaping (String) -> Void, cancel: @escaping () -> Void
    ) {
        self.phase = phase
        self.transcript = transcript
        self.level = level
        self.target = target
        self.stop = stop
        self.send = send
        self.cancel = cancel
    }

    public var body: some View {
        VStack(spacing: 18) {
            HStack {
                Button("Cancel", action: cancel).foregroundStyle(Theme.secondaryText)
                Spacer()
                Text(target).font(.footnote).foregroundStyle(Theme.secondaryText).lineLimit(1)
            }
            Group {
                if phase == .review {
                    TextField("Message", text: Binding(get: { edited ?? transcript }, set: { edited = $0 }), axis: .vertical)
                        .lineLimit(3...10)
                } else {
                    Text(transcript.isEmpty ? "Listening" : transcript)
                        .foregroundStyle(transcript.isEmpty ? Theme.tertiaryText : Theme.text)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentTransition(.interpolate)
                }
            }
            .font(.title3)
            .frame(maxHeight: .infinity, alignment: .top)
            ZStack {
                Circle()
                    .fill(Theme.primary.opacity(0.18))
                    .frame(width: 96 + 60 * level, height: 96 + 60 * level)
                    .animation(.smooth(duration: 0.12), value: level)
                Button {
                    if phase == .listening { stop() } else { send(edited ?? transcript) }
                } label: {
                    Image(systemName: phase == .listening ? "stop.fill" : "arrow.up")
                        .font(.title.weight(.bold))
                        .foregroundStyle(Theme.canvas)
                        .frame(width: 84, height: 84)
                        .background(Theme.primary, in: Circle())
                }
                .buttonStyle(.plain)
                .disabled(phase == .sending)
                .accessibilityLabel(phase == .listening ? "Stop listening" : "Send")
            }
            .frame(height: 160)
            Text(phase == .listening ? "Tap to stop. Edit before sending." : phase == .review ? "Tap to send" : "Sending")
                .font(.footnote)
                .foregroundStyle(Theme.secondaryText)
        }
        .padding(Theme.Metrics.gutter + 4)
        .background(Theme.canvas)
        .sensoryFeedback(.start, trigger: phase == .listening)
    }
}

public struct AboutRow: View {
    let title: String
    let value: String

    public init(_ title: String, value: String) {
        self.title = title
        self.value = value
    }

    public var body: some View {
        LabeledContent(title) {
            Text(value).foregroundStyle(Theme.secondaryText).monospacedDigit().textSelection(.enabled)
        }
    }
}

public struct SettingsContent: Sendable {
    public var machines: [MachineSummary]
    public var version: String
    public var channel: String
    public var build: String
    public var protocolRange: String

    public init(machines: [MachineSummary], version: String, channel: String, build: String, protocolRange: String) {
        self.machines = machines
        self.version = version
        self.channel = channel
        self.build = build
        self.protocolRange = protocolRange
    }
}

public struct SettingsScreen: View {
    let content: SettingsContent
    let machines: () -> Void
    let pair: () -> Void
    @AppStorage("appearance") private var appearance = 0
    @AppStorage("haptics") private var haptics = true
    @AppStorage("showThinking") private var showThinking = true

    public init(content: SettingsContent, machines: @escaping () -> Void, pair: @escaping () -> Void) {
        self.content = content
        self.machines = machines
        self.pair = pair
    }

    public var body: some View {
        Form {
            Section("Machines") {
                Button(action: machines) {
                    LabeledContent("Paired machines") {
                        Text("\(content.machines.count)").foregroundStyle(Theme.secondaryText)
                    }
                }
                .foregroundStyle(Theme.text)
                Button("Pair a machine", systemImage: "qrcode.viewfinder", action: pair)
            }
            Section("Display") {
                Picker("Appearance", selection: $appearance) {
                    Text("System").tag(0)
                    Text("Light").tag(1)
                    Text("Dark").tag(2)
                }
                Toggle("Show reasoning", isOn: $showThinking)
                Toggle("Haptics", isOn: $haptics)
            }
            Section("About") {
                AboutRow("Version", value: content.version)
                AboutRow("Channel", value: content.channel)
                AboutRow("Build", value: content.build)
                AboutRow("Protocol", value: content.protocolRange)
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.canvas)
        .tint(Theme.primary)
        .navigationTitle("Settings")
    }
}
#endif
