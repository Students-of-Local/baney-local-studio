import AppKit
import LocalStudioKit
import SwiftUI

enum OnboardingStep: Int, CaseIterable {
    case welcome
    case daemon
    case providers
    case project
    case done
}

extension StudioStore {
    var needsOnboarding: Bool {
        if shell.onboardingActive { return true }
        guard let settings = pages.settings, inbox.receivedProjects, pages.t3 != nil, !needsT3Import else { return false }
        return !settings.onboardingCompleted && inbox.projects.isEmpty && !shell.onboardingDismissed
    }

    var needsT3Import: Bool {
        guard !shell.onboardingActive, let report = pages.t3 else { return false }
        return report.found && !report.offered && T3Summary.pending(report) > 0
    }

    func completeOnboarding() {
        shell.onboardingActive = false
        shell.onboardingDismissed = true
        guard var settings = pages.settings, !settings.onboardingCompleted else { return }
        settings.onboardingCompleted = true
        core.run { $0.updateSettings(settings: settings) }
    }
}

struct OnboardingView: View {
    let store: StudioStore
    @State var step: OnboardingStep = .welcome
    @State private var cloning = false
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            content
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(.horizontal, 44)
                .padding(.top, 36)
            footer
        }
        .frame(width: 560, height: 440)
        .background(Theme.canvas)
        .task { store.pages.loadProviders() }
        .onAppear { if !store.shell.onboardingDismissed { store.shell.onboardingActive = true } }
    }

    @ViewBuilder private var content: some View {
        switch step {
        case .welcome:
            OnboardingPage(symbol: nil, title: "Welcome to Local Studio", detail: "Agent threads across your projects, running on your own machines through the Eukhe daemon. This takes a minute.") {
                EmptyView()
            }
        case .daemon:
            OnboardingPage(symbol: "server.rack", title: "Connect to Eukhe", detail: "Local Studio starts the Eukhe daemon bundled with the app and talks to it over a local socket.") {
                StatusLine(tint: daemonTint, symbol: daemonSymbol, text: daemonText)
            }
        case .providers:
            OnboardingPage(symbol: "cpu", title: "Model providers", detail: providerDetail) {
                ScrollView {
                    VStack(alignment: .leading, spacing: 6) {
                        providerRows
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        case .project:
            OnboardingPage(symbol: "folder", title: "Add your first project", detail: "Pick a folder you want agents to work in. You can add more later from the sidebar.") {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(store.inbox.projects, id: \.id) { project in
                        HStack(spacing: 8) {
                            ProjectTile(project: project)
                            Text(project.name)
                                .font(.system(size: 13, weight: .medium))
                            Text((project.path as NSString).abbreviatingWithTildeInPath)
                                .font(.system(size: 12))
                                .foregroundStyle(Theme.secondaryText)
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                    }
                    HStack {
                        Button(store.inbox.projects.isEmpty ? "Choose Folder…" : "Add Another Folder…") { chooseFolder() }
                        Button("Clone Repository…") { cloning = true }
                    }
                    .sheet(isPresented: $cloning) {
                        CloneRepositorySheet(store: store, startThread: false)
                    }
                }
            }
        case .done:
            OnboardingPage(symbol: "checkmark", title: "You're ready", detail: "Start a thread with ⌘N, find anything with ⌘K, and hold ⌘ to jump between threads.") {
                EmptyView()
            }
        }
    }

    @ViewBuilder private var providerRows: some View {
        ForEach(store.pages.engines, id: \.id) { engine in
            EngineRow(engine: engine, pending: store.pages.isProviderPending(engine.id)) {
                store.pages.useLocalEngine(engine)
            }
        }
        ForEach(onboardingErrors, id: \.self) { error in
            IconLabel(error, symbol: "exclamationmark.triangle.fill")
                .foregroundStyle(Theme.warning)
        }
        ForEach(onboardingProviders, id: \.id) { provider in
            StatusLine(tint: provider.configured ? Theme.success : Theme.tertiaryText, symbol: provider.configured ? "checkmark" : "minus", text: "\(provider.name) · \(provider.modelCount) model\(provider.modelCount == 1 ? "" : "s")")
        }
        if configuredProviders == 0 {
            if store.pages.engines.isEmpty {
                Text("Start a local server such as Ollama or LM Studio, paste an API key or add an OpenAI-compatible endpoint in Settings → Providers, or run `eukhe` in Terminal and use `/login` for a ChatGPT or Copilot subscription, then check again. For Claude, use Claude Code (your `claude` sign-in).")
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.secondaryText)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Button("Add Provider…") { store.openProvidersSettings() }
                Button("Open Terminal") { openTerminal() }
                Button("Check Again") { store.pages.loadProviders() }
            }
            .padding(.top, 4)
        }
    }

    private var footer: some View {
        HStack(spacing: 10) {
            HStack(spacing: 6) {
                ForEach(OnboardingStep.allCases, id: \.self) { item in
                    Circle()
                        .fill(item == step ? Theme.primary : Theme.input)
                        .frame(width: 6, height: 6)
                }
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Step \(step.rawValue + 1) of \(OnboardingStep.allCases.count)")
            Spacer()
            if step != .done {
                Button("Skip Setup") { finish() }
                    .buttonStyle(.borderless)
            }
            if step != .welcome {
                Button("Back") { move(-1) }
            }
            Button(step == .done ? "Start" : "Continue") {
                if step == .done { finish() } else { move(1) }
            }
            .keyboardShortcut(.defaultAction)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
        .background(Theme.sidebar)
        .overlay(alignment: .top) { Rectangle().fill(Theme.hairline).frame(height: 1) }
    }

    private var configuredProviders: Int {
        store.pages.providers.filter(\.configured).count
    }

    private var onboardingProviders: [ProviderStatus] {
        let engines = store.pages.engines.map(\.baseUrl)
        let rows = store.pages.providers.filter { provider in
            provider.configured == (configuredProviders > 0) && !engines.contains { $0 == provider.baseUrl }
        }
        return Array(rows.prefix(5))
    }

    private var onboardingErrors: [String] {
        [store.pages.providerActionError].compactMap(\.self) + store.pages.providerErrors
    }

    private var providerDetail: String {
        let models = store.pages.providers.filter(\.configured).reduce(0) { $0 + Int($1.modelCount) }
        if store.pages.providers.isEmpty {
            return "Checking which providers Eukhe can use…"
        }
        if configuredProviders == 0, !store.pages.engines.isEmpty {
            return "These model servers are running on your machines. Use one, and its models are ready."
        }
        if configuredProviders == 0 {
            return "No provider is signed in yet, so threads can't run."
        }
        return "\(configuredProviders) provider\(configuredProviders == 1 ? "" : "s") configured with \(models) model\(models == 1 ? "" : "s")."
    }

    private var daemonTint: Color {
        switch store.connection {
        case .ready: Theme.success
        case .connecting: Theme.warning
        case .lost: Theme.danger
        }
    }

    private var daemonSymbol: String {
        switch store.connection {
        case .ready: "checkmark"
        case .connecting: "hourglass"
        case .lost: "xmark"
        }
    }

    private var daemonText: String {
        switch store.connection {
        case .ready: "Connected to the Eukhe daemon"
        case .connecting: "Connecting…"
        case .lost(let reason): "Not connected: \(reason)"
        }
    }

    private func move(_ offset: Int) {
        step = OnboardingStep(rawValue: step.rawValue + offset) ?? step
    }

    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.prompt = "Add Project"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        store.inbox.addProject(path: url.path)
    }

    private func openTerminal() {
        guard let terminal = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.Terminal") else { return }
        NSWorkspace.shared.openApplication(at: terminal, configuration: NSWorkspace.OpenConfiguration())
    }

    private func finish() {
        store.completeOnboarding()
        if let project = store.inbox.projects.first?.id {
            store.newThread(in: project)
        }
        dismiss()
    }
}

private struct OnboardingPage<Extra: View>: View {
    let symbol: String?
    let title: String
    let detail: String
    @ViewBuilder let extra: () -> Extra

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Group {
                if let symbol {
                    Image(systemName: symbol)
                        .icon(.display, .primary)
                } else {
                    Image(nsImage: NSApp.applicationIconImage)
                        .resizable()
                        .frame(width: 56, height: 56)
                }
            }
            .accessibilityHidden(true)
            Text(title)
                .font(.system(size: 22, weight: .semibold))
            Text(detail)
                .font(.system(size: 13.5))
                .foregroundStyle(Theme.secondaryText)
                .fixedSize(horizontal: false, vertical: true)
            extra()
                .padding(.top, 6)
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct StatusLine: View {
    let tint: Color
    let symbol: String
    let text: String
    @Environment(\.accessibilityDifferentiateWithoutColor) private var differentiate

    var body: some View {
        HStack(spacing: 8) {
            Group {
                if differentiate {
                    Image(systemName: symbol)
                        .icon(.inline, .primary)
                } else {
                    Circle()
                        .fill(tint)
                        .frame(width: 7, height: 7)
                }
            }
            .accessibilityHidden(true)
            Text(text)
                .font(.system(size: 13))
        }
    }
}
