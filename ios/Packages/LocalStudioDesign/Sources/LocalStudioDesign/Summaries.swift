import Foundation

public struct ThreadSummary: Identifiable, Hashable, Sendable {
    public var id: String
    public var machine: String
    public var title: String
    public var project: String?
    public var projectIcon: String?
    public var status: StatusKind
    public var since: Date?
    public var updated: Date
    public var unread: Bool
    public var pinned: Bool
    public var branch: String?
    public var pullRequest: Int?
    public var additions: Int
    public var deletions: Int
    public var model: String?
    public var note: String?

    public init(
        id: String, machine: String, title: String, project: String? = nil, projectIcon: String? = nil,
        status: StatusKind = .idle, since: Date? = nil, updated: Date = .now, unread: Bool = false, pinned: Bool = false,
        branch: String? = nil, pullRequest: Int? = nil, additions: Int = 0, deletions: Int = 0, model: String? = nil, note: String? = nil
    ) {
        self.id = id
        self.machine = machine
        self.title = title
        self.project = project
        self.projectIcon = projectIcon
        self.status = status
        self.since = since
        self.updated = updated
        self.unread = unread
        self.pinned = pinned
        self.branch = branch
        self.pullRequest = pullRequest
        self.additions = additions
        self.deletions = deletions
        self.model = model
        self.note = note
    }
}

public struct ApprovalSummary: Identifiable, Hashable, Sendable {
    public var id: String
    public var command: String
    public var effects: [String]
    public var agent: String?

    public init(id: String, command: String, effects: [String] = [], agent: String? = nil) {
        self.id = id
        self.command = command
        self.effects = effects
        self.agent = agent
    }

    public var kind: String {
        let files = effects.contains { effect in
            let effect = effect.lowercased()
            return effect.contains("write") || effect.contains("edit") || effect.contains("file")
        }
        return files ? "File change approval" : "Command approval"
    }
}

public struct QuestionSummary: Identifiable, Hashable, Sendable {
    public struct Option: Hashable, Sendable {
        public var label: String
        public var detail: String?

        public init(label: String, detail: String? = nil) {
            self.label = label
            self.detail = detail
        }
    }

    public var id: String { question }
    public var header: String
    public var question: String
    public var options: [Option]
    public var multiSelect: Bool

    public init(header: String, question: String, options: [Option], multiSelect: Bool = false) {
        self.header = header
        self.question = question
        self.options = options
        self.multiSelect = multiSelect
    }
}

public enum LinkKind: String, Sendable, Hashable {
    case tailscale
    case tunnel
    case local

    public var title: String {
        switch self {
        case .tailscale: "Tailscale"
        case .tunnel: "Tunnel"
        case .local: "Local"
        }
    }

    public var symbol: String {
        switch self {
        case .tailscale: "point.3.connected.trianglepath.dotted"
        case .tunnel: "cloud"
        case .local: "wifi"
        }
    }
}

public enum MachineHealth: Sendable, Hashable {
    case online
    case slow
    case reconnecting(retryIn: Int?)
    case offline

    public var title: String {
        switch self {
        case .online: "Online"
        case .slow: "Slow"
        case .reconnecting: "Reconnecting"
        case .offline: "Offline"
        }
    }
}

public struct MachineSummary: Identifiable, Hashable, Sendable {
    public var id: String
    public var name: String
    public var os: String
    public var version: String
    public var channel: String
    public var link: LinkKind
    public var latency: Int?
    public var health: MachineHealth
    public var working: Int
    public var needsYou: Int
    public var threads: Int

    public init(
        id: String, name: String, os: String, version: String, channel: String, link: LinkKind,
        latency: Int? = nil, health: MachineHealth = .online, working: Int = 0, needsYou: Int = 0, threads: Int = 0
    ) {
        self.id = id
        self.name = name
        self.os = os
        self.version = version
        self.channel = channel
        self.link = link
        self.latency = latency
        self.health = health
        self.working = working
        self.needsYou = needsYou
        self.threads = threads
    }
}
