import AppKit
import LocalStudioKit
import SwiftUI

struct PluginsSettings: View {
    let store: StudioStore
    @State private var expanded: Set<String>
    @State private var source = ""
    @State private var removing: PluginRow?

    init(store: StudioStore, expanded: Set<String> = []) {
        self.store = store
        _expanded = State(initialValue: expanded)
    }

    private var plugins: PluginsStore { store.plugins }

    var body: some View {
        SettingsForm {
            if let error = plugins.error {
                Section {
                    HStack(alignment: .firstTextBaseline) {
                        IconLabel(error, symbol: "xmark.octagon.fill")
                            .foregroundStyle(Theme.danger)
                            .textSelection(.enabled)
                        Spacer()
                        Button("Dismiss") { plugins.dismiss() }
                    }
                }
            } else if let message = plugins.message {
                Section {
                    HStack(alignment: .firstTextBaseline) {
                        IconLabel(message, symbol: "checkmark")
                            .foregroundStyle(Theme.secondaryText)
                        Spacer()
                        Button("Dismiss") { plugins.dismiss() }
                    }
                }
            }
            SettingsSection(.installedPlugins, footer: updatesFooter) {
                SettingRow(.installedPlugins) {
                    if plugins.plugins.isEmpty {
                        Text(plugins.loaded ? "No plugins yet. Install one from the catalog below." : "Loading plugins…")
                            .foregroundStyle(Theme.secondaryText)
                    }
                    ForEach(plugins.plugins, id: \.id) { plugin in
                        DisclosureGroup(isExpanded: expansion(plugin.id)) {
                            PluginDetail(plugin: plugin, pending: plugins.isPending(plugin.id)) { capability, on in
                                plugins.setGrant(plugin.id, capability: capability, on: on)
                            } remove: {
                                removing = plugin
                            }
                        } label: {
                            PluginHeader(plugin: plugin, pending: plugins.isPending(plugin.id)) {
                                plugins.update(plugin.id)
                            } allow: {
                                plugins.allow(plugin.id)
                            }
                        }
                    }
                }
                SettingRow(.pluginUpdates) {
                    HStack {
                        if plugins.isPending(PluginsStore.checking) {
                            ProgressView().controlSize(.small)
                        }
                        Spacer()
                        Button("Check for Updates") { plugins.checkUpdates() }
                            .disabled(plugins.isPending(PluginsStore.checking))
                    }
                }
            }
            SettingsSection(.pluginCatalog, footer: "Catalog plugins are pinned to a commit and a content hash that Local Studio checks before anything runs.") {
                SettingRow(.pluginCatalog) {
                    ForEach(plugins.catalog, id: \.id) { entry in
                        CatalogRow(entry: entry, pending: plugins.isPending(entry.id)) {
                            plugins.install(entry.id, key: entry.id)
                        }
                    }
                }
            }
            SettingsSection(.pluginSource, footer: "A git URL follows the \(channelName) channel. Code from a URL or folder is not pinned by the catalog: you still see everything it asks for before it runs, but read it first if you do not know who wrote it.") {
                SettingRow(.pluginSource) {
                    HStack(spacing: 8) {
                        TextField("Git URL", text: $source, prompt: Text("https://github.com/owner/plugin"))
                            .labelsHidden()
                            .onSubmit(installSource)
                        Button("Install", action: installSource)
                            .disabled(source.trimmingCharacters(in: .whitespaces).isEmpty || plugins.isPending(PluginsStore.installing))
                    }
                    HStack {
                        IconLabel("Not pinned by the catalog", symbol: "exclamationmark.triangle.fill")
                            .foregroundStyle(Theme.warning)
                            .font(.callout)
                        Spacer()
                        if plugins.isPending(PluginsStore.installing) {
                            ProgressView().controlSize(.small)
                        }
                        Button("Choose Folder…", action: chooseFolder)
                            .disabled(plugins.isPending(PluginsStore.installing))
                    }
                }
            }
        }
        .onAppear {
            plugins.settingsVisible = true
            plugins.load()
        }
        .onDisappear { plugins.settingsVisible = false }
        .sheet(item: consentShown) { consent in
            PluginConsentSheet(consent: consent) { store.answerConsent(consent, $0) }
                .interactiveDismissDisabled()
        }
        .confirmationDialog(removeTitle, isPresented: removeShown, titleVisibility: .visible) {
            Button("Remove", role: .destructive) {
                if let removing { plugins.remove(removing.id) }
                removing = nil
            }
            Button("Cancel", role: .cancel) { removing = nil }
        } message: {
            Text("Its folder and its permissions are deleted. Its commands and tools leave every thread. You can install it again later.")
        }
    }

    private var channelName: String {
        plugins.channel.isEmpty ? "app's" : plugins.channel
    }

    private var updatesFooter: String {
        let channel = "Plugins installed from git follow the \(channelName) channel, like Local Studio. An update that asks for new permissions shows them first."
        guard let checked = plugins.checked else { return channel }
        let ago = RelativeDateTimeFormatter().localizedString(for: checked, relativeTo: .now)
        return "\(channel) Last checked \(ago)."
    }

    private var consentShown: Binding<ConsentRequest?> {
        Binding { store.consents.first { plugins.owns($0.request) } } set: { _ in }
    }

    private var removeTitle: String {
        "Remove \(removing?.name ?? "this plugin")?"
    }

    private var removeShown: Binding<Bool> {
        Binding { removing != nil } set: { if !$0 { removing = nil } }
    }

    private func expansion(_ id: String) -> Binding<Bool> {
        Binding(
            get: { expanded.contains(id) },
            set: { open in
                if open { expanded.insert(id) } else { expanded.remove(id) }
            }
        )
    }

    private func installSource() {
        let text = source.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        plugins.install(text)
        source = ""
    }

    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Install"
        panel.message = "Choose a plugin folder with a plugin.toml"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        plugins.install(url.path)
    }
}

private struct PluginIcon: View {
    let path: String?

    var body: some View {
        let image = path.flatMap { NSImage(contentsOfFile: $0) }
        ZStack {
            RoundedRectangle(cornerRadius: 6).fill(Theme.primary.opacity(0.16))
            if let image {
                Image(nsImage: image).resizable().scaledToFit().padding(4)
            } else {
                Image(systemName: "puzzlepiece.extension").icon(.inline).foregroundStyle(Theme.primary)
            }
        }
        .frame(width: 28, height: 28)
        .accessibilityHidden(true)
    }
}

struct PluginStateBadge: View {
    let state: PluginState

    var body: some View {
        IconLabel(title, symbol: symbol)
            .font(.system(size: 11.5, weight: .medium))
            .foregroundStyle(color)
            .labelStyle(.titleAndIcon)
    }

    private var title: String {
        switch state {
        case .running: "Running"
        case .waiting: "Waiting for you"
        case .off: "Off"
        case .failed: "Failed"
        }
    }

    private var symbol: String {
        switch state {
        case .running: "checkmark"
        case .waiting: "hand.raised.fill"
        case .off: "pause.fill"
        case .failed: "exclamationmark.triangle.fill"
        }
    }

    private var color: Color {
        switch state {
        case .running: Theme.success
        case .waiting: Theme.warning
        case .off: Theme.secondaryText
        case .failed: Theme.danger
        }
    }
}

private struct PluginHeader: View {
    let plugin: PluginRow
    let pending: Bool
    let update: () -> Void
    let allow: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            PluginIcon(path: plugin.icon)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(plugin.name)
                        .foregroundStyle(Theme.text)
                    if !plugin.version.isEmpty {
                        Text(plugin.version)
                            .font(.system(size: 11.5, design: .monospaced))
                            .foregroundStyle(Theme.tertiaryText)
                    }
                    PluginStateBadge(state: plugin.state)
                }
                Text(plugin.detail)
                    .font(.system(size: 11.5))
                    .foregroundStyle(Theme.secondaryText)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            if pending {
                ProgressView().controlSize(.small)
            } else if let next = plugin.update {
                Button("Update to \(next.version)", action: update)
                    .help(next.asks.isEmpty ? "Asks for nothing new" : "Asks for \(next.asks.joined(separator: ", ")); you choose before it installs")
            } else if plugin.state == .waiting {
                Button("Allow…", action: allow)
            }
        }
        .padding(.vertical, 2)
    }
}

private struct PluginDetail: View {
    let plugin: PluginRow
    let pending: Bool
    let toggle: (String, Bool) -> Void
    let remove: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if !plugin.description.isEmpty {
                Text(plugin.description)
                    .foregroundStyle(Theme.text)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if !plugin.grants.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    heading("Permissions")
                    ForEach(plugin.grants, id: \.id) { grant in
                        GrantRow(grant: grant, told: plugin.told, pending: pending) { toggle(grant.id, $0) }
                    }
                }
            }
            if !plugin.commands.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    heading("Commands")
                    ForEach(plugin.commands, id: \.name) { command in
                        entry("/\(command.name)", command.description)
                    }
                }
            }
            if !plugin.skills.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    heading("Skills")
                    ForEach(plugin.skills, id: \.name) { skill in
                        entry(skill.name, skill.description)
                    }
                }
            }
            VStack(alignment: .leading, spacing: 4) {
                heading("Source")
                Text(sourceLine)
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.secondaryText)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                if !plugin.pinned, plugin.channel != nil {
                    IconLabel("Not pinned by the catalog", symbol: "exclamationmark.triangle.fill")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.warning)
                }
            }
            HStack(spacing: 8) {
                Button("Open Folder") {
                    NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: plugin.folder)])
                }
                Spacer()
                Button("Remove…", role: .destructive, action: remove)
                    .disabled(pending)
            }
        }
        .padding(.vertical, 6)
    }

    private var sourceLine: String {
        var parts = [plugin.source.isEmpty ? "Copied in by hand" : plugin.source]
        if let revision = plugin.revision { parts.append("at \(revision)") }
        if plugin.pinned {
            parts.append("pinned")
        } else if let channel = plugin.channel {
            parts.append("follows \(channel)")
        }
        return parts.joined(separator: " · ")
    }

    private func heading(_ title: String) -> some View {
        Text(title)
            .font(.system(size: 12, weight: .semibold))
            .foregroundStyle(Theme.secondaryText)
    }

    private func entry(_ name: String, _ description: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(name)
                .font(.system(size: 12, design: .monospaced))
                .foregroundStyle(Theme.text)
            Text(description)
                .font(.system(size: 12))
                .foregroundStyle(Theme.secondaryText)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

private struct GrantRow: View {
    let grant: PluginGrant
    let told: Bool
    let pending: Bool
    let set: (Bool) -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 6) {
                    Text(grant.sentence)
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.text)
                    if grant.need == .sensitive {
                        Text("Sensitive")
                            .font(.system(size: 11, weight: .medium))
                            .foregroundStyle(Theme.danger)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 1)
                            .background(Theme.danger.opacity(0.12), in: RoundedRectangle(cornerRadius: 4))
                    }
                }
                Text(caption)
                    .font(.system(size: 11.5))
                    .foregroundStyle(Theme.secondaryText)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            if grant.need == .sensitive {
                Toggle(grant.sentence, isOn: Binding { grant.granted } set: { set($0) })
                    .toggleStyle(.switch)
                    .labelsHidden()
                    .controlSize(.small)
                    .tint(Theme.primary)
                    .disabled(pending)
            } else {
                Text(grant.granted ? "Allowed" : "Not allowed")
                    .font(.system(size: 12))
                    .foregroundStyle(grant.granted ? Theme.secondaryText : Theme.warning)
            }
        }
    }

    private var caption: String {
        guard grant.need == .sensitive else { return grant.id }
        if let gates = grant.gates, told {
            return "Off means no \(gates). The plugin is told; Local Studio cannot block one that ignores it."
        }
        if told {
            return "Off turns this off for the plugin. The plugin is told; Local Studio cannot block one that ignores it."
        }
        return "The plugin's tools stay off until this is on."
    }
}

private struct CatalogRow: View {
    let entry: CatalogPlugin
    let pending: Bool
    let install: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            PluginIcon(path: nil)
            VStack(alignment: .leading, spacing: 2) {
                Text(entry.name)
                    .foregroundStyle(Theme.text)
                Text(entry.description)
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.secondaryText)
                    .fixedSize(horizontal: false, vertical: true)
                Text(asks)
                    .font(.system(size: 11.5))
                    .foregroundStyle(Theme.tertiaryText)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            if pending {
                ProgressView().controlSize(.small)
            } else if let version = entry.installed {
                Text("Installed \(version)")
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.secondaryText)
            } else {
                Button("Install", action: install)
            }
        }
        .padding(.vertical, 2)
    }

    private var asks: String {
        let local = entry.localOnly ? " Local models only." : ""
        guard !entry.grants.isEmpty else { return "Asks for no permissions. Pinned to \(entry.revision).\(local)" }
        let sentences = entry.grants.map { $0.sentence.lowercased() }.joined(separator: ", ")
        return "Asks to \(sentences). Pinned to \(entry.revision).\(local)"
    }
}
