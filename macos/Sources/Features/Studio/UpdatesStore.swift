import Foundation
import LocalStudioKit
import Observation

struct BuildIdentity: Equatable {
    let version: String
    let channel: String

    static var current: BuildIdentity {
        BuildIdentity(version: buildVersion(), channel: buildChannel())
    }
}

@MainActor
@Observable
final class UpdatesStore {
    private(set) var state: AppUpdateState = .idle
    private(set) var channels: [ChannelInstall] = []
    private(set) var build = BuildIdentity.current

    @ObservationIgnored var onRestart: (@MainActor () -> Void)?
    @ObservationIgnored private let core: CoreHandle

    init(core: CoreHandle) {
        self.core = core
    }

    var restarting: Bool {
        if case .restarting = state { true } else { false }
    }

    var currentName: String {
        channels.first(where: \.current)?.name ?? "Local Studio"
    }

    var otherChannels: [ChannelInstall] {
        channels.filter { !$0.current }
    }

    func setState(_ state: AppUpdateState) {
        self.state = state
        if case .restarting = state {
            onRestart?()
        }
    }

    func present(build: BuildIdentity, channels: [ChannelInstall]) {
        self.build = build
        self.channels = channels
    }

    func startChecks() {
        core.run { $0.startUpdateChecks() }
    }

    func check() {
        core.run { $0.checkForUpdate() }
    }

    func download() {
        core.run { $0.downloadUpdate() }
    }

    func apply() {
        core.run { $0.applyUpdate() }
    }

    func refreshChannels() {
        guard let studio = core.studio else { return }
        channels = studio.channelInstalls()
    }

    func open(_ channel: ChannelInstall) {
        core.run { $0.openChannel(channel: channel.channel) }
    }
}

extension AppUpdateState {
    var notes: String? {
        switch self {
        case .available(_, let notes), .downloading(_, let notes, _, _), .ready(_, let notes), .failed(_, _, let notes):
            notes.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : notes
        case .unavailable, .idle, .checking, .upToDate, .applying, .restarting:
            nil
        }
    }

    var version: String? {
        switch self {
        case .available(let version, _), .downloading(let version, _, _, _), .ready(let version, _), .applying(let version), .restarting(let version):
            version
        case .failed(_, let version, _):
            version
        case .unavailable, .idle, .checking, .upToDate:
            nil
        }
    }

    var checkBlocked: Bool {
        switch self {
        case .checking, .downloading, .applying, .restarting, .unavailable: true
        case .idle, .upToDate, .available, .ready, .failed: false
        }
    }

    var showsChrome: Bool {
        switch self {
        case .ready, .applying: true
        case .unavailable, .idle, .checking, .upToDate, .available, .downloading, .restarting, .failed: false
        }
    }
}
