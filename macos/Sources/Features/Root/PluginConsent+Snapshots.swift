import Foundation
import LocalStudioKit
import SwiftUI

extension SnapshotCatalog {
    @MainActor static var consent: [SnapshotEntry] {
        [
            entry("plugin-consent", ConsentFixtures.install, height: 510),
            entry("plugin-consent-findings", ConsentFixtures.findings, height: 468, findingsOpen: true),
            entry("plugin-consent-update", ConsentFixtures.update, height: 318),
        ]
    }

    @MainActor private static func entry(
        _ name: String, _ consent: ConsentRequest, height: CGFloat, findingsOpen: Bool = false
    ) -> SnapshotEntry {
        SnapshotEntry(
            name: name,
            size: CGSize(width: 540, height: height),
            updates: [],
            view: { _ in AnyView(PluginConsentSheet(consent: consent, findingsOpen: findingsOpen) { _ in }.ignoresSafeArea()) }
        )
    }
}

enum ConsentFixtures {
    static let rows = [
        ConsentRow(
            id: "mcp-server", need: .needed, sentence: "Give agents tools",
            scope: "Starts field-notes-mcp --stdio when a thread uses it.", gates: nil
        ),
        ConsentRow(
            id: "ask-user", need: .needed, sentence: "Ask you questions in a thread",
            scope: "Shows a question card; it never answers for you.", gates: nil
        ),
        ConsentRow(
            id: "screen-capture", need: .sensitive, sentence: "See the windows or tabs it opened",
            scope: "Screenshots of its own tabs or windows, never your other windows.",
            gates: "page screenshots for the agent"
        ),
        ConsentRow(
            id: "site-secrets", need: .sensitive, sentence: "Fill sign-ins you approve",
            scope: "Reads one saved login per site, after you allow that site.", gates: nil
        ),
    ]

    static let install = request(rows: rows, findings: [])

    static let findings = request(
        rows: Array(rows.prefix(3)),
        findings: ["executable not named in the manifest: bin/helper"]
    )

    static let update = request(
        rows: [
            ConsentRow(
                id: "synthetic-input", need: .sensitive, sentence: "Click and type in those tabs",
                scope: "Input goes only to what the plugin opened.", gates: "clicking and typing in its own tabs"
            ),
        ],
        findings: [],
        version: "1.5.0",
        updateFrom: "1.4.0"
    )

    static func request(
        rows: [ConsentRow], findings: [String], version: String = "1.4.0", updateFrom: String? = nil
    ) -> ConsentRequest {
        ConsentRequest(
            request: "snapshot", plugin: "field-notes", name: "Field Notes", icon: nil,
            description: "Reads pages in its own tabs and keeps notes for a thread", version: version,
            install: true, updateFrom: updateFrom, source: "https://github.com/example/field-notes",
            pin: "3f9a2c1", files: 14, findings: findings, rows: rows
        )
    }
}
