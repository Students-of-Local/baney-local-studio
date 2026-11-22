import Foundation
import LocalStudioKit
import SwiftUI

extension SnapshotCatalog {
    @MainActor static var plugins: [SnapshotEntry] {
        [
            SnapshotEntry(
                name: "settings-plugins-expanded",
                size: CGSize(width: 720, height: 1500),
                updates: PagesFixtures.base + [PluginsFixtures.update],
                view: { store in AnyView(PluginsSettings(store: store, expanded: ["field-notes", "hildegard"])) }
            ),
            SnapshotEntry(
                name: "settings-plugins-empty",
                size: CGSize(width: 720, height: 900),
                updates: PagesFixtures.base + [PluginsFixtures.empty],
                view: { store in AnyView(PluginsSettings(store: store)) }
            ),
        ]
    }
}

enum PluginsFixtures {
    static let mcp = PluginGrant(id: "mcp-server", need: .needed, sentence: "Give agents tools", gates: nil, granted: true)
    static let ask = PluginGrant(id: "ask-user", need: .needed, sentence: "Ask you questions in a thread", gates: nil, granted: true)
    static let capture = PluginGrant(
        id: "screen-capture", need: .sensitive, sentence: "See the windows or tabs it opened",
        gates: "page screenshots for the agent", granted: true
    )
    static let input = PluginGrant(
        id: "synthetic-input", need: .sensitive, sentence: "Click and type in those tabs",
        gates: "clicking and typing in its own tabs", granted: false
    )

    static let rows: [PluginRow] = [
        PluginRow(
            id: "field-notes", name: "Field Notes", version: "1.4.0",
            description: "Reads pages in its own tabs and keeps notes for a thread", icon: nil,
            state: .running,
            detail: "Its tools are on in every thread; it cannot click and type in those tabs until you turn that on",
            grants: [mcp, ask, capture, input], told: true,
            commands: [PluginEntry(name: "notes", description: "Summarize what this thread has read")],
            skills: [PluginEntry(name: "field-notes", description: "Keep dated notes from pages you read")],
            source: "https://github.com/example/field-notes", revision: "3f9a2c1", pinned: false, channel: "stable",
            update: PluginUpdate(version: "1.5.0", revision: "8b21e0d", asks: ["accessibility"]),
            folder: "/Users/demo/.local/share/local-studio/plugins/field-notes", localOnly: false
        ),
        PluginRow(
            id: "hildegard", name: "hildegard", version: "0.1.17",
            description: "Hildegard for Local Studio: make images, video, speech, designed voices and cleaned audio on your own GPU",
            icon: nil, state: .running, detail: "Its commands and skills are on", grants: [], told: false,
            commands: [
                PluginEntry(name: "image", description: "Draw an image on your GPU with Hildegard"),
                PluginEntry(name: "speak", description: "Turn a script into speech in one of your voices"),
            ],
            skills: [PluginEntry(name: "hildegard-image", description: "Draw an image with the local image model")],
            source: "Local Studio catalog", revision: "504d949", pinned: true, channel: "stable", update: nil,
            folder: "/Users/demo/.local/share/local-studio/plugins/hildegard", localOnly: false
        ),
        PluginRow(
            id: "ledger", name: "Ledger", version: "0.3.0", description: "Files a private ledger entry", icon: nil,
            state: .waiting, detail: "Waiting for you to allow it to give agents tools",
            grants: [PluginGrant(id: "mcp-server", need: .needed, sentence: "Give agents tools", gates: nil, granted: false)],
            told: false, commands: [PluginEntry(name: "ledger", description: "File today's entry")], skills: [],
            source: "/Users/demo/Projects/ledger", revision: nil, pinned: false, channel: nil, update: nil,
            folder: "/Users/demo/.local/share/local-studio/plugins/ledger", localOnly: true
        ),
        PluginRow(
            id: "broken", name: "broken", version: "", description: "", icon: nil, state: .failed,
            detail: "It did not load: bad version: \"1\" (use semver, such as 1.0.0)", grants: [], told: false,
            commands: [], skills: [], source: "", revision: nil, pinned: false, channel: nil, update: nil,
            folder: "/Users/demo/.local/share/local-studio/plugins/broken", localOnly: false
        ),
    ]

    static let catalog: [CatalogPlugin] = [
        catalogEntry("gantrel", "Gantrel", "Design 3D printable parts with a local model, then slice, upload and watch them on your printers", "5cdd089", nil),
        catalogEntry("hildegard", "Hildegard", "Make images, video, speech, designed voices and cleaned audio on your own GPU, each one checked before you see it", "504d949", "0.1.17"),
        catalogEntry("smoothstone", "Smoothstone", "Make 3D printable parts with the stone CLI and its tested parametric recipes", "715e0ac", nil),
        catalogEntry("tychicus-browser", "Tychicus Browser", "Work in Chromium tabs in the background through the Tychicus extension", "fa6fcbe", nil),
        catalogEntry("tychicus-computer", "Tychicus Computer", "Read and operate desktop apps through accessibility APIs", "fa6fcbe", nil),
    ]

    static var update: Update {
        .plugins(
            plugins: rows, catalog: catalog, channel: "stable",
            checked: Int64((Date.now.timeIntervalSince1970 - 600) * 1_000)
        )
    }

    static var empty: Update {
        .plugins(plugins: [], catalog: catalog.map { entry in
            var entry = entry
            entry.installed = nil
            return entry
        }, channel: "stable", checked: nil)
    }

    private static func catalogEntry(_ id: String, _ name: String, _ description: String, _ revision: String, _ installed: String?) -> CatalogPlugin {
        CatalogPlugin(
            id: id, name: name, description: description, grants: [], source: "https://github.com/baney75/\(id)",
            revision: revision, localOnly: false, installed: installed
        )
    }
}
