import Foundation

public struct OutboxEntry: Codable, Sendable, Hashable, Identifiable {
    public var args: SendArgs
    public var created: Date

    public var id: String { args.request }
}

public actor Outbox {
    private let file: URL
    private var entries: [OutboxEntry]

    public init(file: URL) {
        self.file = file
        if let data = try? Data(contentsOf: file), let saved = try? JSONDecoder().decode([OutboxEntry].self, from: data) {
            entries = saved
        } else {
            entries = []
        }
    }

    public func enqueue(thread: String, prompt: PromptDraft, delivery: Delivery) -> OutboxEntry {
        let entry = OutboxEntry(
            args: SendArgs(request: "ios-" + UUID().uuidString.lowercased(), thread: thread, prompt: prompt, delivery: delivery),
            created: Date()
        )
        entries.append(entry)
        persist()
        return entry
    }

    public func pending() -> [OutboxEntry] {
        entries
    }

    public func remove(_ request: String) {
        entries.removeAll { $0.args.request == request }
        persist()
    }

    public func clear() {
        entries = []
        persist()
    }

    private func persist() {
        do {
            try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            if entries.isEmpty {
                try? FileManager.default.removeItem(at: file)
            } else {
                try JSONEncoder().encode(entries).write(to: file, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
            }
        } catch {
            return
        }
    }
}
