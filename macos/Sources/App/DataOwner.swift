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
        guard let pid, let other = NSRunningApplication(processIdentifier: pid_t(pid)), !other.isTerminated else {
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
            other.activate()
            next(false)
            return
        }
        other.terminate()
        let started = Date.now
        Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { timer in
            MainActor.assumeIsolated {
                let waited = Date.now.timeIntervalSince(started)
                if !other.isTerminated, waited < quitWait {
                    if waited >= 1, waited < 1.25, !other.isActive {
                        other.activate()
                    }
                    return
                }
                timer.invalidate()
                NSApp.activate()
                next(true)
            }
        }
    }

    static var appName: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String ?? "Local Studio"
    }

    static func describe(_ error: Error) -> String {
        switch error as? StudioError {
        case .Runtime(let message)?, .Config(let message)?:
            return message
        case .Owned(_, let path, _)?:
            return "Another Local Studio has \(path) open."
        case nil:
            return error.localizedDescription
        }
    }
}
