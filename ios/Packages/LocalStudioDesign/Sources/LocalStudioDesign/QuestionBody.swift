import SwiftUI

public struct QuestionBody: View {
    let questions: [QuestionSummary]
    let submit: ([String: String]) -> Void
    @State private var picks: [String: Set<String>] = [:]
    @State private var other: [String: String] = [:]
    @State private var sent = false

    public init(_ questions: [QuestionSummary], submit: @escaping ([String: String]) -> Void) {
        self.questions = questions
        self.submit = submit
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(questions) { question in
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 6) {
                        Image(systemName: "questionmark.bubble.fill").accessibilityHidden(true)
                        Text(question.header).fontWeight(.semibold)
                        if question.multiSelect {
                            Text("Pick any").foregroundStyle(Theme.secondaryText)
                        }
                    }
                    .font(.footnote)
                    .foregroundStyle(Theme.primary)
                    Text(question.question)
                        .font(.subheadline)
                        .foregroundStyle(Theme.text)
                        .fixedSize(horizontal: false, vertical: true)
                    VStack(spacing: 6) {
                        ForEach(question.options, id: \.label) { option in
                            OptionRow(option: option, multi: question.multiSelect, selected: picks[question.id, default: []].contains(option.label)) {
                                toggle(question, option.label)
                            }
                        }
                        TextField("Other answer", text: binding(for: question))
                            .font(.subheadline)
                            .padding(.horizontal, 12)
                            .frame(minHeight: 44)
                            .background(Theme.hover, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                            .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Theme.hairline))
                    }
                }
            }
            HStack {
                Spacer(minLength: 0)
                Button(questions.count > 1 ? "Send answers" : "Send answer") {
                    sent = true
                    submit(answers)
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.primary)
                .controlSize(.large)
                .buttonBorderShape(.capsule)
                .disabled(!complete || sent)
            }
        }
        .sensoryFeedback(.success, trigger: sent)
    }

    private var answers: [String: String] {
        Dictionary(uniqueKeysWithValues: questions.map { question in
            let typed = other[question.id, default: ""].trimmingCharacters(in: .whitespacesAndNewlines)
            let chosen = question.options.map(\.label).filter { picks[question.id, default: []].contains($0) }
            let value = typed.isEmpty ? chosen.joined(separator: ", ") : (chosen + [typed]).joined(separator: ", ")
            return (question.question, value)
        })
    }

    private var complete: Bool {
        questions.allSatisfy { question in
            !picks[question.id, default: []].isEmpty || !other[question.id, default: ""].trimmingCharacters(in: .whitespaces).isEmpty
        }
    }

    private func toggle(_ question: QuestionSummary, _ label: String) {
        var set = picks[question.id, default: []]
        if question.multiSelect {
            if set.contains(label) { set.remove(label) } else { set.insert(label) }
        } else {
            set = set.contains(label) ? [] : [label]
        }
        picks[question.id] = set
    }

    private func binding(for question: QuestionSummary) -> Binding<String> {
        Binding(get: { other[question.id, default: ""] }, set: { other[question.id] = $0 })
    }
}

struct OptionRow: View {
    let option: QuestionSummary.Option
    let multi: Bool
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Image(systemName: symbol)
                    .foregroundStyle(selected ? Theme.primary : Theme.tertiaryText)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(option.label)
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(Theme.text)
                    if let detail = option.detail, !detail.isEmpty {
                        Text(detail)
                            .font(.caption)
                            .foregroundStyle(Theme.secondaryText)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .frame(minHeight: 44)
            .background(selected ? Theme.primary.opacity(0.12) : Theme.hover, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(selected ? Theme.primary.opacity(0.5) : Theme.hairline)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private var symbol: String {
        if multi { return selected ? "checkmark.square.fill" : "square" }
        return selected ? "largecircle.fill.circle" : "circle"
    }
}
