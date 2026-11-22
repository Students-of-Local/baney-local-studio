import Foundation
import LocalStudioKit
import SwiftUI

extension SnapshotCatalog {
    @MainActor static var t3Import: [SnapshotEntry] {
        [
            sheet("t3-import", T3Fixtures.firstRun, open: [], height: 560),
            sheet("t3-import-details", T3Fixtures.firstRun, open: [.accounts, .models], height: 600),
            sheet("t3-import-synced", T3Fixtures.synced, open: [.projects], height: 560),
            SnapshotEntry(
                name: "pages-settings-t3-sync",
                size: CGSize(width: 860, height: 520),
                updates: PagesFixtures.base + [.settings(settings: T3Fixtures.settings), .t3Sync(report: T3Fixtures.synced)],
                view: { store in AnyView(SettingsView(store: store, pane: .general, query: "t3")) }
            ),
            SnapshotEntry(
                name: "pages-settings-t3-sync-off",
                size: CGSize(width: 860, height: 520),
                updates: PagesFixtures.base + [.t3Sync(report: T3Fixtures.off)],
                view: { store in AnyView(SettingsView(store: store, pane: .general, query: "t3")) }
            ),
            SnapshotEntry(
                name: "pages-settings-accounts",
                size: CGSize(width: 860, height: 560),
                updates: PagesFixtures.base + [.settings(settings: T3Fixtures.settings)],
                view: { store in AnyView(SettingsView(store: store, pane: .providers, query: "accounts")) }
            ),
        ]
    }

    @MainActor private static func sheet(_ name: String, _ report: T3Report, open: Set<T3Kind>, height: CGFloat) -> SnapshotEntry {
        SnapshotEntry(
            name: name,
            size: CGSize(width: 560, height: height),
            updates: [],
            view: { _ in AnyView(T3ImportSheet(report: report, open: open) {} bringOver: { _ in }.ignoresSafeArea()) }
        )
    }
}

enum T3Fixtures {
    static let source = "/Users/demo/.t3/userdata"

    static var settings: LocalStudioKit.Settings {
        var settings = PagesFixtures.settings
        settings.harnessAccounts = HarnessAccounts(
            claude: ["/Users/demo/.claude", "/Users/demo/.claude-work"],
            codex: ["/Users/demo/.codex"],
            labels: [
                AccountLabel(dir: "/Users/demo/.claude", name: "personal", accent: nil, source: "T3 Code"),
                AccountLabel(dir: "/Users/demo/.claude-work", name: "work", accent: "#d4af5d", source: "T3 Code"),
                AccountLabel(dir: "/Users/demo/.codex", name: "Codex", accent: "#16a34a", source: "T3 Code"),
            ]
        )
        return settings
    }

    static func item(_ key: String, _ label: String, _ detail: String, _ state: T3ItemState) -> T3Item {
        T3Item(key: key, label: label, detail: detail, state: state)
    }

    static func groups(_ state: T3ItemState) -> [T3Group] {
        [
            T3Group(kind: .accounts, title: "Accounts", chosen: true, items: [
                item("account:claudeAgent", "personal", "Claude Code account at ~/.claude", state),
                item("account:claudeAgent_work", "work", "Claude Code account at ~/.claude-work", state),
                item("account:codex", "Codex", "Codex account at ~/.codex", state),
                item("account:cursor", "Cursor", "Not supported yet: Local Studio has no Cursor harness. Claude Code and Codex are supported. API key set in T3 Code.", .unsupported),
                item("account:opencode", "OpenCode", "Not supported yet: Local Studio has no OpenCode harness. Claude Code and Codex are supported. Server password set in T3 Code.", .unsupported),
            ]),
            T3Group(kind: .projects, title: "Projects", chosen: true, items: [
                item("project:/Users/demo/Projects/local-studio", "local-studio", "~/Projects/local-studio", state),
                item("project:/Users/demo/Projects/field-notes", "field-notes", "~/Projects/field-notes", state),
                item("project:/Users/demo/Projects/site", "site", "~/Projects/site", state == .synced ? .kept : state),
                item("project:/Users/demo/Projects/api", "api", "~/Projects/api", state),
                item("project:p-old", "old-prototype", "Its folder is gone: ~/Projects/old-prototype", .unsupported),
            ]),
            T3Group(kind: .models, title: "Models", chosen: true, items: [
                item("model.default", "Default model", "Claude Opus 5.5, thinking high", state),
                item("model.textGeneration", "Text generation model", "Claude Sonnet 5.5", state),
                item("favorite:claude-code/claude-opus-5-5", "Claude Opus 5.5", "Favorite", state),
                item("favorite:codex/gpt-6.1-sol", "gpt-6.1-sol", "Favorite (Codex)", state),
                item("favorite:cursor/kimi-k3p1-pro", "kimi-k3p1-pro", "Not supported yet: a Cursor model.", .unsupported),
                item("hidden:claude-code/claude-opus-4-8", "Claude Opus 4.8", "Hidden", state),
            ]),
            T3Group(kind: .keybindings, title: "Keybindings", chosen: true, items: [
                item("keybinding:chat.new", "New Thread", "Cmd+Shift+O", state),
                item("keybinding:usage.open", "Open Usage", "Cmd+Option+U", state),
                item("keybinding:unknown", "2 shortcuts", "Not supported yet: Local Studio has no composer.dictation or terminal.split command.", .unsupported),
            ]),
            T3Group(kind: .appearance, title: "Appearance", chosen: true, items: [
                item("setting.theme", "Theme", "Martyr Brand", state),
                item("setting.colorScheme", "Appearance", "System", state),
                item("setting.interfaceFontSize", "Interface font size", "15", state),
                item("setting.chatWidth", "Chat width", "Wide", state == .synced ? .kept : state),
            ]),
            T3Group(kind: .preferences, title: "Preferences", chosen: true, items: [
                item("setting.timeFormat", "Time format", "24-hour", state),
                item("setting.sendShortcut", "Send shortcut", "Cmd+Enter", state),
                item("setting.addProjectBaseDirectory", "Add project starts in", "~/Projects", state),
            ]),
        ]
    }

    static var firstRun: T3Report {
        T3Report(
            found: true, source: source, enabled: false, offered: false, applied: false, lastSyncedAt: nil,
            choices: T3Choices(accounts: true, projects: true, models: true, keybindings: true, appearance: true, preferences: true),
            groups: groups(.import), problems: []
        )
    }

    static var synced: T3Report {
        T3Report(
            found: true, source: source, enabled: true, offered: true, applied: true,
            lastSyncedAt: PagesFixtures.now - 4 * 60_000,
            choices: T3Choices(accounts: true, projects: true, models: true, keybindings: true, appearance: true, preferences: true),
            groups: groups(.synced), problems: []
        )
    }

    static var off: T3Report {
        var report = firstRun
        report.offered = true
        report.lastSyncedAt = PagesFixtures.now - 3 * 86_400_000
        report.groups = groups(.synced).map { group in
            var group = group
            if group.kind == .projects {
                group.items[3].state = .import
            }
            return group
        }
        return report
    }
}
