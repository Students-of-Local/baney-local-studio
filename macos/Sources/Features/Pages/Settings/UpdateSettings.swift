import LocalStudioKit
import SwiftUI

struct UpdatesSection: View {
    let settings: LocalStudioKit.Settings
    let store: StudioStore

    private var updates: UpdatesStore { store.updates }
    private var bind: SettingsBinder { SettingsBinder(settings: settings, pages: store.pages) }

    var body: some View {
        SettingsSection(.updates) {
            SettingRow(.version) {
                LabeledContent(SettingKey.version.title) {
                    Text("\(updates.currentName) \(updates.build.version)")
                        .foregroundStyle(Theme.secondaryText)
                        .textSelection(.enabled)
                }
            }
            SettingRow(.updateStatus) {
                UpdateStatusRow(updates: updates, automatic: settings.automaticUpdates)
            }
            if let notes = updates.state.notes {
                SettingRow(.releaseNotes) {
                    ReleaseNotesBlock(version: updates.state.version, notes: notes)
                }
            }
            SettingRow(.automaticUpdates) {
                Toggle(isOn: bind(\.automaticUpdates)) {
                    Text(SettingKey.automaticUpdates.title)
                    Text("Download new versions in the background. Restarting to update is always your choice.")
                }
            }
            SettingRow(.channel) {
                LabeledContent {
                    if !updates.otherChannels.isEmpty {
                        Menu("Switch Channel") {
                            ForEach(updates.otherChannels, id: \.channel) { channel in
                                Button(channel.installed ? "Open \(channel.name)" : "Get \(channel.name)") {
                                    updates.open(channel)
                                }
                            }
                        }
                        .fixedSize()
                    }
                } label: {
                    Text(SettingKey.channel.title)
                    Text("Each channel is its own app, and switching installs or opens that app.")
                }
                .task { updates.refreshChannels() }
            }
        }
    }
}

private struct UpdateStatusRow: View {
    let updates: UpdatesStore
    let automatic: Bool

    private var state: AppUpdateState { updates.state }

    var body: some View {
        LabeledContent {
            HStack(spacing: 8) {
                switch state {
                case .available where !automatic:
                    Button("Download") { updates.download() }
                case .ready:
                    Button("Restart to Update") { updates.apply() }
                        .buttonStyle(.borderedProminent)
                case .applying:
                    Button("Restart to Update") {}
                        .buttonStyle(.borderedProminent)
                        .disabled(true)
                default:
                    EmptyView()
                }
                Button("Check Now") { updates.check() }
                    .disabled(state.checkBlocked)
            }
            .fixedSize()
        } label: {
            VStack(alignment: .leading, spacing: 4) {
                Text(headline)
                    .foregroundStyle(failed ? Theme.danger : Theme.text)
                if let detail {
                    Text(detail)
                        .font(.callout)
                        .foregroundStyle(Theme.secondaryText)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if let fraction {
                    ProgressView(value: fraction)
                        .frame(maxWidth: 320)
                        .accessibilityLabel("Download progress")
                }
            }
        }
    }

    private var failed: Bool {
        if case .failed = state { true } else { false }
    }

    private var fraction: Double? {
        guard case .downloading(_, _, let received, let total) = state, let total, total > 0 else { return nil }
        return min(Double(received) / Double(total), 1)
    }

    private var headline: String {
        let name = updates.currentName
        switch state {
        case .idle: return "Not checked yet"
        case .checking: return "Checking for updates…"
        case .upToDate: return "\(name) is up to date"
        case .available: return "Update available"
        case .downloading: return "Downloading the update"
        case .ready: return "Update ready to install"
        case .applying: return "Installing the update…"
        case .restarting: return "Restarting to finish the update…"
        case .failed(_, let version, _): return version == nil ? "Could not check for updates" : "Could not update"
        case .unavailable: return "No updates for this build"
        }
    }

    private var detail: String? {
        let name = updates.currentName
        switch state {
        case .idle, .checking, .restarting:
            return nil
        case .upToDate(let latest):
            return "Latest on this channel: \(latest)"
        case .available(let version, _):
            return automatic ? "Version \(version)" : "Version \(version). Download it now, or turn on Automatic updates."
        case .downloading(let version, _, let received, let total):
            let got = ByteCountFormatter.string(fromByteCount: Int64(clamping: received), countStyle: .file)
            guard let total else { return "Version \(version): \(got) received" }
            return "Version \(version): \(got) of \(ByteCountFormatter.string(fromByteCount: Int64(clamping: total), countStyle: .file))"
        case .ready(let version, _):
            return "Version \(version). Restart \(name) to finish updating."
        case .applying(let version):
            return "Version \(version). \(name) restarts once the new build passes its check."
        case .failed(let reason, let version, _):
            return version.map { "Version \($0): \(reason)" } ?? reason
        case .unavailable(let reason):
            return reason
        }
    }
}

private struct ReleaseNotesBlock: View {
    let version: String?
    let notes: String
    @State private var textHeight: CGFloat = 0

    private static let maxHeight: CGFloat = 180

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(version.map { "What's new in \($0)" } ?? SettingKey.releaseNotes.title)
            ScrollView {
                Text(verbatim: notes)
                    .font(.callout)
                    .foregroundStyle(Theme.text)
                    .lineSpacing(2)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                    .onGeometryChange(for: CGFloat.self, of: \.size.height) { textHeight = $0 }
            }
            .scrollIndicators(textHeight > Self.maxHeight ? .automatic : .never)
            .frame(height: min(max(textHeight, 1), Self.maxHeight))
            .background(Theme.input, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).strokeBorder(Theme.hairline))
        }
        .accessibilityElement(children: .contain)
    }
}
