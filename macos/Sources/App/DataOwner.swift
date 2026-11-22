import AppKit
import LocalStudioKit

@MainActor
enum DataOwner {
    static let quitWait: TimeInterval = 30

    static func resolve(_ error: Error, then next: @escaping @MainActor @Sendable (_ retry: Bool) -> Void) {
        guard case StudioError.Owned(let pid, let path, _) = error else {
            let alert = NSAlert()
            alert.alertStyle = .critical
            alert.messageText = "\(appName) could not start"
            alert.informativeText = describe(error)
            alert.addButton(withTitle: "Quit")
            alert.runModal()
            next(false)
            return
        }
        guard let pid, let other = NSRunningApplication(processIdentifier: pid_t(pid)), !other.isTerminated,
              other.bundleIdentifier?.hasPrefix("org.local.studio.macos") == true else {
            let alert = NSAlert()
            alert.messageText = "Your Local Studio data is in use"
            alert.informativeText = pid.map { "Process \($0) has \(path) open. Quit it, then try again." } ?? "Another process has \(path) open. Quit it, then try again."
            alert.addButton(withTitle: "Try Again")
            alert.addButton(withTitle: "Quit")
            next(alert.runModal() == .alertFirstButtonReturn)
            return
        }
        let theirs = other.localizedName ?? "Local Studio"
        let same = theirs == appName
        let alert = NSAlert()
        alert.messageText = "\(theirs) is already open"
        alert.informativeText = same
            ? "Another copy of \(appName) has your threads open, and only one can run at a time."
            : "\(appName) and \(theirs) share your threads, projects and sign-ins, so only one runs at a time."
        alert.addButton(withTitle: same ? "Quit the Other Copy" : "Quit \(theirs) and Open \(appName)")
        alert.addButton(withTitle: "Keep \(theirs)")
        guard alert.runModal() == .alertFirstButtonReturn else {
            raise(other)
            next(false)
            return
        }
        other.terminate()
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
            MainActor.assumeIsolated {
                if !other.isTerminated {
                    raise(other)
                }
            }
        }
        wait(for: other, since: .now, then: next)
    }

    static func wait(for other: NSRunningApplication, since started: Date, then next: @escaping @MainActor @Sendable (_ retry: Bool) -> Void) {
        guard !other.isTerminated, Date.now.timeIntervalSince(started) < quitWait else {
            NSApp.activate()
            next(true)
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) {
            MainActor.assumeIsolated {
                wait(for: other, since: started, then: next)
            }
        }
    }

    static func raise(_ other: NSRunningApplication) {
        NSApp.yieldActivation(to: other)
        other.activate()
    }

    static var appName: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String ?? "Local Studio"
    }

    static func describe(_ error: Error) -> String {
        switch error as? StudioError {
        case .Runtime(let message)?, .Config(let message)?:
            return message
        default:
            return error.localizedDescription
        }
    }
}
