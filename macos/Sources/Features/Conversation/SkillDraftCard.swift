import AppKit
import LocalStudioKit
import SwiftUI

struct SkillDraftCard: View {
    let store: StudioStore

    var body: some View {
        if let thread = store.conversation.thread, let draft = store.conversation.skillDrafts[thread] {
            SkillDraftCardBody(store: store, thread: thread, draft: draft)
                .id(thread)
        }
    }
}

private struct SkillDraftCardBody: View {
    let store: StudioStore
    let thread: String
    let draft: SkillDraft
    @State private var reviewing: SkillDraft?

    var body: some View {
        let saving = store.conversation.savingSkills.contains(thread)
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: "star")
                    .icon(.inline)
                    .foregroundStyle(Theme.primary)
                    .accessibilityHidden(true)
                Text("Skill from this thread")
                    .font(.system(size: 12.5, weight: .semibold))
                    .foregroundStyle(Theme.primary)
                Spacer(minLength: 8)
                if draft.replaces {
                    Text("Replaces yours")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.secondaryText)
                }
            }
            Text(draft.name)
                .font(.system(size: 13, weight: .semibold))
            Text(draft.description)
                .font(.system(size: 13))
                .fixedSize(horizontal: false, vertical: true)
            if !draft.facts.isEmpty {
                Text(draft.facts)
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.secondaryText)
            }
            if !draft.marked.isEmpty {
                SkillWarning(text: SkillDraft.warning(draft.marked.count))
            }
            HStack(spacing: 6) {
                if draft.marked.isEmpty {
                    saveButton.buttonStyle(.borderedProminent)
                    reviewButton.buttonStyle(.bordered)
                } else {
                    saveButton.buttonStyle(.bordered)
                    reviewButton.buttonStyle(.borderedProminent)
                }
                Button("Discard") { store.discardSkill(thread: thread, draft: draft.id) }
                    .help("Drop this draft; the reply stays in the thread")
            }
            .tint(Theme.primary)
            .disabled(saving)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.card, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(Theme.primary.opacity(0.3)))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Skill from this thread: \(draft.name)")
        .sheet(item: $reviewing) { draft in
            SkillDraftSheet(draft: draft, saving: saving) { text, failed in
                store.saveSkill(thread: thread, draft: draft.id, text: text, failed: failed)
            } close: { text in
                store.conversation.keepSkillText(thread: thread, draft: draft.id, text: text)
                reviewing = nil
            }
        }
    }

    private var saveButton: some View {
        Button(draft.verb) { store.saveSkill(thread: thread, draft: draft.id, text: draft.text) }
            .help("Save it to your skills")
    }

    private var reviewButton: some View {
        Button("Review") { reviewing = draft }
            .help("Read and edit it before you save")
    }
}

struct SkillDraftSheet: View {
    let draft: SkillDraft
    let saving: Bool
    let save: (String, @escaping (String) -> Void) -> Void
    let close: (String) -> Void
    @State private var text: String
    @State private var error: String?

    init(
        draft: SkillDraft,
        saving: Bool,
        save: @escaping (String, @escaping (String) -> Void) -> Void,
        close: @escaping (String) -> Void
    ) {
        self.draft = draft
        self.saving = saving
        self.save = save
        self.close = close
        _text = State(initialValue: draft.text)
    }

    var body: some View {
        let marked = skillMarks(text: text, files: draft.files)
        VStack(alignment: .leading, spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    Text("Review \(draft.name)")
                        .font(.system(size: 22, weight: .medium, design: .serif))
                        .fixedSize(horizontal: false, vertical: true)
                    if !marked.isEmpty {
                        SkillWarning(text: SkillDraft.warning(marked.count))
                        ForEach(Array(marked.enumerated()), id: \.offset) { _, line in
                            Text(line)
                                .font(.system(size: 11, design: .monospaced))
                                .foregroundStyle(Theme.secondaryText)
                                .textSelection(.enabled)
                                .padding(.leading, 20)
                        }
                    }
                    if !draft.left.isEmpty {
                        Text("Left out of the skill: code blocks labeled \(draft.left.joined(separator: ", ")).")
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.secondaryText)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    PlainEditor(text: $text)
                        .frame(height: 320)
                        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Theme.hairline))
                        .accessibilityLabel("Skill text")
                    ForEach(draft.files, id: \.path) { file in
                        DisclosureGroup(file.path) {
                            Text(file.text)
                                .font(.system(size: 11, design: .monospaced))
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.vertical, 4)
                        }
                        .font(.system(size: 12))
                    }
                }
                .padding(18)
            }
            .scrollBounceBehavior(.basedOnSize)
            Rectangle().fill(Theme.hairline).frame(height: 1)
            HStack(spacing: 8) {
                if let error {
                    Text(error)
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.danger)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer()
                Button("Close", role: .cancel) { close(text) }
                    .keyboardShortcut(.cancelAction)
                    .disabled(saving)
                Button(draft.verb) {
                    error = nil
                    save(text) { error = $0 }
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.primary)
                .disabled(saving)
            }
            .controlSize(.large)
            .padding(18)
            .background(Theme.canvas)
        }
        .frame(width: 640, height: 600)
        .background(Theme.card)
        .interactiveDismissDisabled(saving)
        .onDisappear { close(text) }
    }
}

private struct SkillWarning: View {
    let text: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: "exclamationmark.triangle")
                .icon(.inline)
                .foregroundStyle(Theme.warning)
                .accessibilityHidden(true)
            Text(text)
                .font(.system(size: 13))
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

private struct PlainEditor: NSViewRepresentable {
    @Binding var text: String

    func makeCoordinator() -> Coordinator {
        Coordinator(text: $text)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSTextView.scrollableTextView()
        guard let view = scroll.documentView as? NSTextView else { return scroll }
        view.delegate = context.coordinator
        view.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        view.textContainerInset = NSSize(width: 6, height: 8)
        view.allowsUndo = true
        view.isRichText = false
        view.drawsBackground = false
        view.isAutomaticQuoteSubstitutionEnabled = false
        view.isAutomaticDashSubstitutionEnabled = false
        view.isAutomaticTextReplacementEnabled = false
        view.isAutomaticSpellingCorrectionEnabled = false
        view.smartInsertDeleteEnabled = false
        view.string = text
        scroll.drawsBackground = false
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let view = scroll.documentView as? NSTextView, view.string != text else { return }
        view.string = text
    }

    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        private let text: Binding<String>

        init(text: Binding<String>) {
            self.text = text
        }

        func textDidChange(_ notification: Notification) {
            guard let view = notification.object as? NSTextView else { return }
            text.wrappedValue = view.string
        }
    }
}

extension SkillDraft: Identifiable {}

extension SkillDraft {
    private static let asks = "asks.txt"
    private static let places = "where.txt"

    var verb: String {
        replaces ? "Replace" : "Save"
    }

    static func warning(_ marked: Int) -> String {
        let lines = marked == 1 ? "1 line" : "\(marked) lines"
        return "\(lines) may hold a password or a key. Read \(marked == 1 ? "it" : "them") before you save."
    }

    var facts: String {
        let beside = files.filter { $0.path != Self.asks && $0.path != Self.places }.count
        let parts = [
            lines(Self.places).joined(separator: ", "),
            Self.count(lines(Self.asks).count, "way to ask", "ways to ask"),
            Self.count(beside, "file", "files"),
        ]
        return parts.filter { !$0.isEmpty }.joined(separator: " · ")
    }

    private func lines(_ path: String) -> [String] {
        let text = files.first { $0.path == path }?.text ?? ""
        return text.split(separator: "\n").map(String.init)
    }

    private static func count(_ number: Int, _ one: String, _ many: String) -> String {
        switch number {
        case 0: ""
        case 1: "1 \(one)"
        default: "\(number) \(many)"
        }
    }
}
