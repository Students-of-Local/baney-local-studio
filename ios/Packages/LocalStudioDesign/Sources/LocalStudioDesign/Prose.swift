import SwiftUI

public enum ProseBlock: Hashable, Sendable {
    case paragraph(String)
    case heading(String, level: Int)
    case item(String, marker: String, depth: Int)
    case quote(String)
    case code(String, language: String)
    case rule

    public static func parse(_ text: String) -> [ProseBlock] {
        var blocks: [ProseBlock] = []
        var paragraph: [String] = []
        var code: [String]?
        var language = ""

        func flush() {
            if !paragraph.isEmpty {
                blocks.append(.paragraph(paragraph.joined(separator: "\n")))
                paragraph.removeAll()
            }
        }

        for raw in text.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(raw)
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("```") {
                if let lines = code {
                    blocks.append(.code(lines.joined(separator: "\n"), language: language))
                    code = nil
                } else {
                    flush()
                    language = String(trimmed.dropFirst(3)).trimmingCharacters(in: .whitespaces)
                    code = []
                }
                continue
            }
            if code != nil {
                code?.append(line)
                continue
            }
            if trimmed.isEmpty {
                flush()
                continue
            }
            if let level = heading(trimmed) {
                flush()
                blocks.append(.heading(String(trimmed.drop(while: { $0 == "#" })).trimmingCharacters(in: .whitespaces), level: level))
                continue
            }
            if trimmed == "---" || trimmed == "***" {
                flush()
                blocks.append(.rule)
                continue
            }
            if trimmed.hasPrefix("> ") {
                flush()
                blocks.append(.quote(String(trimmed.dropFirst(2))))
                continue
            }
            if let (marker, rest) = listItem(trimmed) {
                flush()
                let indent = line.prefix(while: { $0 == " " }).count
                blocks.append(.item(rest, marker: marker, depth: min(indent / 2, 3)))
                continue
            }
            paragraph.append(line)
        }
        if let lines = code {
            blocks.append(.code(lines.joined(separator: "\n"), language: language))
        }
        flush()
        return blocks
    }

    private static func heading(_ line: String) -> Int? {
        let hashes = line.prefix(while: { $0 == "#" }).count
        guard (1...4).contains(hashes), line.dropFirst(hashes).first == " " else { return nil }
        return hashes
    }

    private static func listItem(_ line: String) -> (String, String)? {
        for bullet in ["- ", "* ", "+ "] where line.hasPrefix(bullet) {
            let rest = String(line.dropFirst(2))
            if rest.hasPrefix("[ ] ") { return ("\u{25CB}", String(rest.dropFirst(4))) }
            if rest.hasPrefix("[x] ") { return ("\u{25C9}", String(rest.dropFirst(4))) }
            return ("\u{2022}", rest)
        }
        let digits = line.prefix(while: \.isNumber)
        if !digits.isEmpty, digits.count < 4, line.dropFirst(digits.count).hasPrefix(". ") {
            return ("\(digits).", String(line.dropFirst(digits.count + 2)))
        }
        return nil
    }
}

public struct Prose: View {
    let blocks: [ProseBlock]
    let secondary: Bool

    public init(_ text: String, secondary: Bool = false) {
        self.blocks = ProseBlock.parse(text)
        self.secondary = secondary
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                view(block)
            }
        }
        .font(secondary ? .footnote : .callout)
        .foregroundStyle(secondary ? Theme.secondaryText : Theme.text)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private func view(_ block: ProseBlock) -> some View {
        switch block {
        case let .paragraph(text):
            Text(Self.inline(text))
                .lineSpacing(3)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        case let .heading(text, level):
            Text(Self.inline(text))
                .font(level == 1 ? .title3.weight(.semibold) : level == 2 ? .headline : .subheadline.weight(.semibold))
                .padding(.top, 4)
        case let .item(text, marker, depth):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(marker)
                    .foregroundStyle(Theme.secondaryText)
                    .monospacedDigit()
                    .frame(minWidth: 12, alignment: .trailing)
                Text(Self.inline(text))
                    .lineSpacing(3)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.leading, CGFloat(depth) * 16)
        case let .quote(text):
            Text(Self.inline(text))
                .foregroundStyle(Theme.secondaryText)
                .padding(.leading, 12)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.input).frame(width: 2) }
        case let .code(text, language):
            CodeBlock(text: text, language: language)
        case .rule:
            Rectangle().fill(Theme.hairline).frame(height: 1).padding(.vertical, 4)
        }
    }

    static func inline(_ text: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace, failurePolicy: .returnPartiallyParsedIfPossible)
        return (try? AttributedString(markdown: text, options: options)) ?? AttributedString(text)
    }
}

public struct CodeBlock: View {
    let text: String
    let language: String
    @State private var copied = false

    public init(text: String, language: String = "") {
        self.text = text
        self.language = language
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(language.isEmpty ? "code" : language)
                    .font(.caption)
                    .foregroundStyle(Theme.tertiaryText)
                Spacer(minLength: 0)
                Button {
                    Clipboard.copy(text)
                    copied = true
                } label: {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc")
                        .font(.caption)
                        .foregroundStyle(Theme.secondaryText)
                        .frame(width: 32, height: 28)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(copied ? "Copied" : "Copy code")
                .sensoryFeedback(.success, trigger: copied) { _, new in new }
            }
            .padding(.leading, 12)
            .padding(.trailing, 4)
            Rectangle().fill(Theme.hairline).frame(height: 1)
            scroller(Text(text)
                .font(.footnote.monospaced())
                .foregroundStyle(Theme.text)
                .lineSpacing(2)
                .textSelection(.enabled)
                .padding(12))
        }
        .background(Theme.code, in: RoundedRectangle(cornerRadius: Theme.Metrics.innerRadius, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Metrics.innerRadius, style: .continuous).strokeBorder(Theme.hairline))
    }

    @ViewBuilder
    private func scroller(_ content: some View) -> some View {
        #if os(iOS)
        ScrollView(.horizontal, showsIndicators: false) {
            content.fixedSize(horizontal: true, vertical: false)
        }
        #else
        content.frame(maxWidth: .infinity, alignment: .leading)
        #endif
    }
}
