import LocalStudioRemote
import SwiftUI
import UIKit

@main
struct LocalStudioApp: App {
    @Environment(\.scenePhase) private var scenePhase
    @State private var studio: StudioModel
    @State private var notifier: Notifier
    private let launch: LaunchOptions

    init() {
        _ = Perf.shared
        let launch = LaunchOptions()
        let environment = StudioEnvironment(
            directory: URL.applicationSupportDirectory.appendingPathComponent("LocalStudioRemote", isDirectory: true),
            keychainService: (Bundle.main.bundleIdentifier ?? "org.local.studio.ios") + ".remote",
            device: DeviceProfile(name: UIDevice.current.name, app: Channel.version, channel: Channel.current),
            connectDelay: .milliseconds(Int(launch.connectDelay * 1000))
        )
        if launch.reset {
            environment.wipe()
        }
        let studio = StudioModel(environment: environment)
        let notifier = Notifier(askPermission: !launch.smoke)
        studio.onNotification = { machine, note in
            notifier.post(note, machine: machine.name)
        }
        if let text = launch.pairLink, studio.machines.isEmpty, let link = PairingLink(string: text) {
            studio.pair(link)
        }
        studio.start()
        self.launch = launch
        _studio = State(initialValue: studio)
        _notifier = State(initialValue: notifier)
    }

    var body: some Scene {
        WindowGroup {
            RootView(studio: studio, smoke: launch.smoke)
                .onOpenURL { url in
                    if let link = PairingLink(url: url) {
                        studio.pair(link)
                    }
                }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active {
                studio.appBecameActive()
            }
        }
    }
}

struct LaunchOptions {
    let reset: Bool
    let smoke: Bool
    let pairLink: String?
    let connectDelay: Double

    init(defaults: UserDefaults = .standard) {
        reset = defaults.bool(forKey: "LSReset")
        smoke = defaults.bool(forKey: "LSSmoke")
        pairLink = defaults.string(forKey: "LSPairLink")
        connectDelay = defaults.double(forKey: "LSConnectDelay")
    }
}

enum Channel {
    static var current: String {
        Bundle.main.object(forInfoDictionaryKey: "LSChannel") as? String ?? "stable"
    }

    static var version: String {
        if let full = Bundle.main.object(forInfoDictionaryKey: "LSVersion") as? String, !full.isEmpty {
            return full
        }
        return Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.0.0"
    }
}
