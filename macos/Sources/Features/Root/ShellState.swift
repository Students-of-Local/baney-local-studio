import Foundation
import LocalStudioKit
import Observation

enum PalettePage: Equatable {
    case root
    case newThread
    case goToFile
    case searchProject
    case appearance
}

enum ShellConfirmation: Equatable {
    case delete([String])
    case stopAndDelete([String])
    case archive([String])
    case unpin([String])
    case restartDaemon(UInt32)
    case localOnly(thread: String, plugin: String, message: String, local: ModelKey?)
    case studioLink(StudioLink)

    var threads: [String] {
        switch self {
        case .delete(let ids), .stopAndDelete(let ids), .archive(let ids), .unpin(let ids): ids
        case .restartDaemon, .localOnly, .studioLink: []
        }
    }
}

enum FocusRegion: CaseIterable {
    case sidebar
    case timeline
    case composer
    case rightPanel
    case terminal
}

@MainActor
@Observable
final class ShellState {
    var route = MainRoute.thread
    var sidebarVisible = true
    var sidebarWidth: CGFloat = 272
    var chromeTrailing = Theme.Metrics.trafficLightsWidth + Theme.Metrics.chromeButton
    var chromeEnd = Theme.Metrics.trafficLightsWidth + 3 * (Theme.Metrics.chromeGlyph.width + 2 * Theme.Metrics.chromeGlyphInset)
    var palette: PalettePage?
    var commandHeld = false
    var confirmation: ShellConfirmation?
    var headerRenaming = false
    var snoozeTargets: [String] = []
    var commitProject: String?
    var pullRequestProject: String?
    var publishProject: String?
    var onboardingDismissed = false
    var onboardingActive = false
    var cloneShown = false
    var settingsRequest: SettingsPane?
    var settingsProject: String?
    var region = FocusRegion.composer
    var sidebarCursor: String?
    var timelineCursor: UInt64?
    var searchFocusRequest = 0
    @ObservationIgnored let keys = KeyState()
    @ObservationIgnored let probe = RenderProbe()
    @ObservationIgnored private(set) var recent: [String] = []

    func touchRecent(_ thread: String?) {
        guard let thread, keys.recentCycle == nil else { return }
        recent.removeAll { $0 == thread }
        recent.insert(thread, at: 0)
        if recent.count > 30 { recent.removeLast() }
    }

    private(set) var back: [String?] = []
    private(set) var forward: [String?] = []

    func record(from previous: String?, to next: String?) {
        guard previous != next else { return }
        back.append(previous)
        if back.count > 50 { back.removeFirst() }
        forward = []
    }

    func popBack(current: String?) -> String?? {
        guard let target = back.popLast() else { return nil }
        forward.append(current)
        return .some(target)
    }

    func popForward(current: String?) -> String?? {
        guard let target = forward.popLast() else { return nil }
        back.append(current)
        return .some(target)
    }

    var canGoBack: Bool { !back.isEmpty }
    var canGoForward: Bool { !forward.isEmpty }
}
