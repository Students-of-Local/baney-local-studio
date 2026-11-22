import Foundation
import LocalStudioKit
import SwiftUI

extension SnapshotCatalog {
    @MainActor static var updates: [SnapshotEntry] {
        [
            general("updates-general-up-to-date", .upToDate(latest: UpdateFixtures.current)),
            general("updates-general-available", .available(version: UpdateFixtures.next, notes: UpdateFixtures.shortNotes), automatic: false),
            general(
                "updates-general-downloading",
                .downloading(version: UpdateFixtures.next, notes: UpdateFixtures.shortNotes, received: 43_620_000, total: 109_050_000)
            ),
            general("updates-general-ready", .ready(version: UpdateFixtures.next, notes: UpdateFixtures.longNotes)),
            general(
                "updates-general-failed",
                .failed(
                    reason: "The download's signature was refused: the signature does not match the nightly channel key.",
                    version: UpdateFixtures.next, notes: UpdateFixtures.shortNotes
                )
            ),
            general(
                "updates-general-unavailable",
                .unavailable(reason: "This is a local build. Local builds never update themselves."),
                build: UpdateFixtures.localBuild
            ),
            window("updates-window-ready", .ready(version: UpdateFixtures.next, notes: UpdateFixtures.longNotes)),
            window("updates-window-applying", .applying(version: UpdateFixtures.next)),
            window("updates-window-ready-narrow", .ready(version: UpdateFixtures.next, notes: UpdateFixtures.shortNotes), sidebarWidth: 220),
        ]
    }

    @MainActor private static func general(_ name: String, _ state: AppUpdateState, automatic: Bool = true, build: BuildIdentity = UpdateFixtures.nightlyBuild) -> SnapshotEntry {
        var settings = PagesFixtures.settings
        settings.automaticUpdates = automatic
        return SnapshotEntry(
            name: name,
            size: CGSize(width: 860, height: 680),
            updates: PagesFixtures.base + [.settings(settings: settings), .appUpdate(state: state)]
        ) { store in
            store.updates.present(build: build, channels: UpdateFixtures.channels(current: build.channel))
            return AnyView(SettingsView(store: store, pane: .general, query: "update"))
        }
    }

    @MainActor private static func window(_ name: String, _ state: AppUpdateState, sidebarWidth: CGFloat? = nil) -> SnapshotEntry {
        SnapshotEntry(
            name: name,
            size: CGSize(width: 1280, height: 800),
            focus: "t-active-1",
            updates: ShellFixtures.updates + ShellFixtures.focused + [.appUpdate(state: state)]
        ) { store in
            store.updates.present(build: UpdateFixtures.nightlyBuild, channels: UpdateFixtures.channels(current: "nightly"))
            if let sidebarWidth {
                store.shell.sidebarWidth = sidebarWidth
            }
            return AnyView(RootView(store: store))
        }
    }
}

enum UpdateFixtures {
    static let current = "0.2.0-nightly.20261121070000+78494ed"
    static let next = "0.2.0-nightly.20261122070000+a1c9f30"
    static let nightlyBuild = BuildIdentity(version: current, channel: "nightly")
    static let localBuild = BuildIdentity(version: "0.2.0-dev.0+local.78494ed", channel: "dev")

    static func channels(current: String) -> [ChannelInstall] {
        [
            ChannelInstall(channel: "stable", name: "Local Studio", installed: true, current: current == "stable"),
            ChannelInstall(channel: "nightly", name: "Local Studio Nightly", installed: true, current: current == "nightly"),
            ChannelInstall(channel: "dev", name: "Local Studio Dev", installed: current == "dev", current: current == "dev"),
        ]
    }

    static let shortNotes = """
        Settings now shows update status, release notes and a channel switcher.
        The sidebar keeps its scroll position when a thread moves to Working.
        """

    static let longNotes = """
        New
        - In-app updates: Local Studio checks its channel's feed on launch and every 6 hours.
        - Settings, General, Updates shows the version, status and these notes.
        - Switch Channel opens or installs Local Studio, Nightly or Dev side by side.

        Fixed
        - The sidebar keeps its scroll position when a thread moves to Working.
        - Copying a reply keeps bold text and monospaced code in rich text.
        - The terminal drawer no longer loses its height after a restart.
        - Pull request checks refresh when the branch is pushed again.

        Under the hood
        - Every download is checked against its sha256 and the channel's minisign key.
        - A new build that fails its launch check is rolled back to the previous one.
        """
}
