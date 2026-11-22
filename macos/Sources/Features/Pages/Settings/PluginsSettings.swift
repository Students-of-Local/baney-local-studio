import LocalStudioKit
import SwiftUI

struct PluginsSettings: View {
    let store: StudioStore

    var body: some View {
        SettingsForm {
            if store.plugins.isEmpty {
                SettingsSection(.installedPlugins) {
                    SettingRow(.pluginPermissions) {
                        PageEmptyState(
                            symbol: "puzzlepiece.extension",
                            title: "No plugins installed",
                            message: "Install one with local-studio plugin install <path>."
                        )
                    }
                }
            } else {
                SettingsSection(.installedPlugins) {
                    SettingRow(.pluginPermissions) {
                        SettingsNote(
                            symbol: "hand.raised",
                            text: "Sensitive permissions start off. \(PluginTrust.text) A change reaches a plugin's tools the next time its server starts, such as in a new thread."
                        )
                    }
                }
                ForEach(store.plugins, id: \.id) { plugin in
                    PluginSection(store: store, plugin: plugin)
                }
            }
        }
        .onAppear { store.listPlugins() }
    }
}

private struct PluginSection: View {
    let store: StudioStore
    let plugin: PluginInfo

    var body: some View {
        SettingRow(.pluginPermissions) {
            Section {
                ForEach(plugin.rows, id: \.id) { row in
                    if row.need == .sensitive {
                        Toggle(isOn: binding(row.id)) {
                            label(row)
                        }
                        .disabled(!plugin.ready)
                    } else {
                        LabeledContent {
                            Text(plugin.granted.contains(row.id) ? "Needed" : "Needed, not granted")
                                .foregroundStyle(Theme.secondaryText)
                        } label: {
                            label(row)
                        }
                    }
                }
            } header: {
                Text("\(plugin.name) \(plugin.version)")
            } footer: {
                Text(plugin.status)
            }
        }
    }

    private func label(_ row: ConsentRow) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(row.sentence)
            Text(subtitle(row))
                .font(.caption)
                .foregroundStyle(Theme.secondaryText)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func subtitle(_ row: ConsentRow) -> String {
        let line = "\(row.scope)  \(row.id)"
        guard row.need == .sensitive, let gates = row.gates else { return line }
        return "\(line)\nOff means no \(gates)."
    }

    private func binding(_ capability: String) -> Binding<Bool> {
        Binding(
            get: { plugin.granted.contains(capability) },
            set: { store.setPluginPermission(plugin.id, capability, $0) }
        )
    }
}
