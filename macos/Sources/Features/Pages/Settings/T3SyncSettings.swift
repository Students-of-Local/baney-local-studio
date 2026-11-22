import AppKit
import LocalStudioKit
import SwiftUI

struct T3SyncSection: View {
    let pages: PagesStore

    var body: some View {
        SettingsSection(.t3Code, footer: footer) {
            SettingRow(.t3Sync) {
                if let report = pages.t3, report.found {
                    Toggle(isOn: Binding(get: { report.enabled }, set: { pages.setT3Sync($0) })) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(SettingKey.t3Sync.title)
                            Text(T3SyncText.status(report))
                                .font(.system(size: 11.5))
                                .foregroundStyle(Theme.secondaryText)
                        }
                    }
                    LabeledContent("Last synced") {
                        HStack {
                            Text(T3SyncText.last(report))
                                .foregroundStyle(Theme.secondaryText)
                            Button("Import Now") { pages.importT3(report.choices) }
                        }
                    }
                    ForEach(report.problems, id: \.self) { problem in
                        IconLabel(problem, symbol: "exclamationmark.triangle.fill")
                            .foregroundStyle(Theme.warning)
                    }
                } else {
                    LabeledContent(SettingKey.t3Sync.title) {
                        Text(pages.t3 == nil ? "Looking for T3 Code…" : "T3 Code is not on this Mac.")
                            .foregroundStyle(Theme.secondaryText)
                    }
                }
            }
        }
    }

    private var footer: String {
        "Brings accounts, projects, model picks, favorites, hidden models, keybindings and appearance over from T3 Code and follows its changes. A value you change in Local Studio is never overwritten."
    }
}

enum T3SyncText {
    static func status(_ report: T3Report) -> String {
        let pending = T3Summary.pending(report)
        guard report.enabled else {
            return pending == 0 ? "Off. Nothing new in T3 Code." : "Off. \(pending) new in T3 Code."
        }
        let kept = report.groups.reduce(0) { $0 + T3Summary.count($1, .kept) }
        let synced = report.groups.reduce(0) { $0 + T3Summary.count($1, .synced) }
        let keptText = kept == 0 ? "" : ", \(kept) changed here and kept"
        return "Following \(report.source.isEmpty ? "T3 Code" : abbreviate(report.source)): \(synced) in sync\(keptText)."
    }

    static func last(_ report: T3Report) -> String {
        guard let at = report.lastSyncedAt else { return "Never" }
        let date = Date(timeIntervalSince1970: TimeInterval(at) / 1000)
        if abs(date.timeIntervalSinceNow) < 60 { return "Just now" }
        return date.formatted(.relative(presentation: .named))
    }

    static func abbreviate(_ path: String) -> String {
        let home = FileManager.default.homeDirectoryForCurrentUser.path(percentEncoded: false)
        let trimmed = home.hasSuffix("/") ? String(home.dropLast()) : home
        return path.hasPrefix(trimmed + "/") ? "~" + path.dropFirst(trimmed.count) : path
    }
}

struct HarnessAccountsSection: View {
    let settings: LocalStudioKit.Settings

    var body: some View {
        SettingsSection(.harnessAccounts, footer: "Each account is a config folder with its own sign-in. When one hits its usage limit, a thread can move to another.") {
            SettingRow(.accounts) {
                if rows.isEmpty {
                    Text("Local Studio finds signed-in ~/.claude and ~/.codex folders on its own.")
                        .foregroundStyle(Theme.secondaryText)
                }
                ForEach(rows, id: \.dir) { row in
                    HStack(spacing: 8) {
                        Circle()
                            .fill(row.label?.accent.flatMap(ThemeColorParser.parse).map { Color(nsColor: $0.nsColor) } ?? Theme.tertiaryText)
                            .frame(width: 8, height: 8)
                            .accessibilityHidden(true)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(row.name)
                            Text("\(row.harness) · \(T3SyncText.abbreviate(row.dir))")
                                .font(.system(size: 11.5))
                                .foregroundStyle(Theme.secondaryText)
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                        Spacer()
                        if let source = row.label?.source {
                            Text("From \(source)")
                                .font(.system(size: 11, weight: .medium))
                                .foregroundStyle(Theme.secondaryText)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 1)
                                .background(Theme.hairline, in: RoundedRectangle(cornerRadius: 4))
                        }
                    }
                }
            }
        }
    }

    private struct Row {
        let dir: String
        let harness: String
        let label: AccountLabel?

        var name: String {
            if let name = label?.name, !name.isEmpty { return name }
            return URL(fileURLWithPath: dir).lastPathComponent
        }
    }

    private var rows: [Row] {
        let accounts = settings.harnessAccounts
        let label = { (dir: String) in accounts.labels.first { $0.dir == dir } }
        return accounts.claude.map { Row(dir: $0, harness: "Claude Code", label: label($0)) }
            + accounts.codex.map { Row(dir: $0, harness: "Codex", label: label($0)) }
    }
}
