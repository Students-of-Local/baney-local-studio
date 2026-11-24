import Foundation

public struct OutboxEntry: Codable, Sendable, Hashable, Identifiable {
    public var args: SendArgs
    public var created: Date
    /// The server epoch this send last went out in, once it has.
    public var sentEpoch: String?
    /// Sent in an earlier epoch and never acknowledged; waits for the user's Resend.
    public var unconfirmed: Bool?

    public var id: String { args.request }
}

public actor Outbox {
    private let file: URL
    private var entries: [OutboxEntry] = []
    private var loaded = false
    private var problem: String?

    public init(file: URL) {
        self.file = file
    }

    /// What kept messages from being saved or read since the last call, for the user.
    public func takeProblem() -> String? {
        defer { problem = nil }
        return problem
    }

    public func enqueue(thread: String, prompt: PromptDraft, delivery: Delivery) -> OutboxEntry {
        load()
        let entry = OutboxEntry(
            args: SendArgs(request: "ios-" + UUID().uuidString.lowercased(), thread: thread, prompt: prompt, delivery: delivery),
            created: Date()
        )
        entries.append(entry)
        persist()
        return entry
    }

    public func pending() -> [OutboxEntry] {
        load()
        return entries
    }

    public func markSent(_ request: String, epoch: String) {
        update(request) { $0.sentEpoch = epoch }
    }

    public func markUnconfirmed(_ request: String) {
        update(request) { $0.unconfirmed = true }
    }

    /// Clears the unconfirmed mark so the send goes out again, and returns it.
    public func release(_ request: String) -> OutboxEntry? {
        update(request) {
            $0.unconfirmed = nil
            $0.sentEpoch = nil
        }
        return entries.first { $0.args.request == request }
    }

    private func update(_ request: String, _ change: (inout OutboxEntry) -> Void) {
        guard let index = entries.firstIndex(where: { $0.args.request == request }) else {
            return
        }
        change(&entries[index])
        persist()
    }

    public func remove(_ request: String) {
        entries.removeAll { $0.args.request == request }
        persist()
    }

    public func clear() {
        loaded = true
        entries = []
        persist()
    }

    /// A file that cannot be read stays as it is until it can; one that does not decode is kept under another name.
    private func load() {
        guard !loaded else {
            return
        }
        do {
            let data = try Data(contentsOf: file)
            if let saved = try? JSONDecoder().decode([OutboxEntry].self, from: data) {
                entries = saved + entries
            } else {
                try FileManager.default.moveItem(at: file, to: file.appendingPathExtension("unreadable-" + UUID().uuidString))
                problem = "Messages waiting to be sent could not be read. They are kept in the app's folder."
            }
            loaded = true
        } catch CocoaError.fileReadNoSuchFile {
            loaded = true
        } catch {
            problem = "Messages waiting to be sent could not be read yet: \(error.localizedDescription)"
        }
    }

    private func persist() {
        do {
            load()
            guard loaded else {
                return
            }
            try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            if !entries.isEmpty {
                try JSONEncoder().encode(entries).write(to: file, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
            } else if FileManager.default.fileExists(atPath: file.path) {
                try FileManager.default.removeItem(at: file)
            }
        } catch {
            problem = "Messages waiting to be sent are not saved, so keep Local Studio open: \(error.localizedDescription)"
        }
    }
}
