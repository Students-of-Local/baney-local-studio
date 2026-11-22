import Foundation
import LocalStudioKit
import Observation

@MainActor
@Observable
final class PluginsStore {
    private(set) var plugins: [PluginRow] = []
    private(set) var catalog: [CatalogPlugin] = []
    private(set) var channel = ""
    private(set) var checked: Date?
    private(set) var loaded = false
    private(set) var pending: Set<String> = []
    private(set) var error: String?
    private(set) var message: String?
    var settingsVisible = false

    @ObservationIgnored private let core: CoreHandle
    @ObservationIgnored private let requests: RequestCenter
    @ObservationIgnored private var owned: Set<String> = []

    static let installing = "install"
    static let checking = "check"

    init(core: CoreHandle, requests: RequestCenter) {
        self.core = core
        self.requests = requests
    }

    func set(plugins: [PluginRow], catalog: [CatalogPlugin], channel: String, checked: Int64?) {
        self.plugins = plugins
        self.catalog = catalog
        self.channel = channel
        self.checked = checked.map { Date(timeIntervalSince1970: TimeInterval($0) / 1_000) }
        loaded = true
    }

    func load() {
        core.run { $0.loadPlugins() }
    }

    func owns(_ request: String) -> Bool {
        owned.contains(request)
    }

    func install(_ source: String, key: String = PluginsStore.installing) {
        let source = source.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !source.isEmpty else { return }
        act(key) { $0.installPlugin(request: $1, source: source) }
    }

    func checkUpdates() {
        act(Self.checking) { $0.checkPluginUpdates(request: $1) }
    }

    func update(_ id: String) {
        act(id) { $0.updatePlugin(request: $1, id: id) }
    }

    func remove(_ id: String) {
        act(id) { $0.removePlugin(request: $1, id: id) }
    }

    func allow(_ id: String, _ capabilities: [String] = []) {
        act(id) { $0.grantPlugin(request: $1, id: id, capabilities: capabilities) }
    }

    func setGrant(_ id: String, capability: String, on: Bool) {
        if on {
            allow(id, [capability])
        } else {
            act(id) { $0.revokePlugin(request: $1, id: id, capability: capability) }
        }
    }

    func isPending(_ key: String) -> Bool {
        pending.contains(key)
    }

    func dismiss() {
        error = nil
        message = nil
    }

    private func act(_ key: String, _ call: @escaping (Studio, String) -> Void) {
        guard core.studio != nil else {
            error = "Local Studio has not started its core yet."
            return
        }
        error = nil
        message = nil
        pending.insert(key)
        let request = requests.issue { [weak self] outcome in
            guard let self else { return }
            pending.remove(key)
            switch outcome {
            case .done(let text): message = text
            case .failed(let text): error = text
            }
        }
        owned.insert(request)
        core.run { call($0, request) }
    }
}
