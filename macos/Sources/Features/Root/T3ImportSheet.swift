import AppKit
import LocalStudioKit
import SwiftUI

struct T3ImportSheet: View {
    let report: T3Report
    let dismiss: () -> Void
    let bringOver: (T3Choices) -> Void
    @State private var choices: T3Choices
    @State private var open: Set<T3Kind>
    @State private var contentHeight: CGFloat = 0

    init(report: T3Report, open: Set<T3Kind> = [], dismiss: @escaping () -> Void, bringOver: @escaping (T3Choices) -> Void) {
        self.report = report
        self.dismiss = dismiss
        self.bringOver = bringOver
        _choices = State(initialValue: report.choices)
        _open = State(initialValue: open)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    header
                        .padding(18)
                    Rectangle().fill(Theme.hairline).frame(height: 1)
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(groups.enumerated()), id: \.element.kind) { index, group in
                            if index > 0 {
                                Rectangle().fill(Theme.hairline).frame(height: 1)
                            }
                            row(group)
                        }
                    }
                    .padding(.horizontal, 18)
                    .padding(.vertical, 8)
                }
                .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { contentHeight = $0 }
            }
            .scrollBounceBehavior(.basedOnSize)
            .frame(height: min(contentHeight, scrollLimit))
            Rectangle().fill(Theme.hairline).frame(height: 1)
            footer
                .padding(18)
                .background(Theme.canvas)
        }
        .frame(width: 560)
        .background(Theme.card)
    }

    private var groups: [T3Group] {
        report.groups.filter { !$0.items.isEmpty }
    }

    private var scrollLimit: CGFloat {
        min(540, (NSScreen.main?.visibleFrame.height ?? 800) - 240)
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 8).fill(Theme.primary.opacity(0.16))
                Image(systemName: "square.and.arrow.down.on.square").icon(.display).foregroundStyle(Theme.primary)
            }
            .frame(width: 40, height: 40)
            .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text("Bring over your T3 Code setup")
                    .font(.system(size: 22, weight: .medium, design: .serif))
                    .foregroundStyle(Theme.text)
                Text("Local Studio found T3 Code on this Mac. Pick what to bring over. It keeps syncing from T3 Code until you turn that off in Settings, and anything you change here stays yours.")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.secondaryText)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func row(_ group: T3Group) -> some View {
        let expanded = open.contains(group.kind)
        return VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .center, spacing: 12) {
                Button {
                    if expanded { open.remove(group.kind) } else { open.insert(group.kind) }
                } label: {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Image(systemName: expanded ? "chevron.down" : "chevron.right")
                            .icon(.inline)
                            .foregroundStyle(Theme.tertiaryText)
                            .frame(width: 12)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(group.title)
                                .font(.system(size: 13, weight: .medium))
                                .foregroundStyle(Theme.text)
                            Text(T3Summary.line(group))
                                .font(.system(size: 12))
                                .foregroundStyle(Theme.secondaryText)
                                .lineLimit(2)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("\(group.title), \(expanded ? "hide" : "show") details")
                Spacer(minLength: 8)
                Toggle(group.title, isOn: binding(group.kind))
                    .toggleStyle(.switch)
                    .labelsHidden()
                    .controlSize(.small)
                    .tint(Theme.primary)
                    .disabled(T3Summary.count(group, .import) == 0)
            }
            if expanded {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(group.items, id: \.key) { item in
                        T3ItemLine(item: item)
                    }
                }
                .padding(.leading, 20)
                .padding(.bottom, 4)
            }
        }
        .frame(minHeight: 48)
        .padding(.vertical, 4)
    }

    private var footer: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Sign-ins stay where they are. Local Studio uses the same Claude Code and Codex folders as T3 Code, so both apps share one login. Nothing is written back to T3 Code.")
                .font(.system(size: 12))
                .foregroundStyle(Theme.secondaryText)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                Spacer()
                Button("Not Now", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .controlSize(.large)
                Button(confirmTitle) { bringOver(choices) }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.primary)
                    .controlSize(.large)
                    .keyboardShortcut(.defaultAction)
                    .disabled(chosenCount == 0)
            }
        }
    }

    private var chosenCount: Int {
        groups.filter { T3Summary.chosen(choices, $0.kind) }.reduce(0) { $0 + T3Summary.count($1, .import) }
    }

    private var confirmTitle: String {
        chosenCount == 1 ? "Bring Over 1 Item" : "Bring Over \(chosenCount) Items"
    }

    private func binding(_ kind: T3Kind) -> Binding<Bool> {
        Binding(
            get: { T3Summary.chosen(choices, kind) },
            set: { T3Summary.set(&choices, kind, $0) }
        )
    }
}

struct T3ItemLine: View {
    let item: T3Item

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: symbol)
                .icon(.inline)
                .foregroundStyle(tint)
                .frame(width: 14)
            VStack(alignment: .leading, spacing: 1) {
                Text(item.label)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(item.state == .unsupported ? Theme.secondaryText : Theme.text)
                if !item.detail.isEmpty {
                    Text(item.detail)
                        .font(.system(size: 11.5))
                        .foregroundStyle(Theme.secondaryText)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(item.label), \(T3Summary.word(item.state))")
    }

    private var symbol: String {
        switch item.state {
        case .import: "plus.circle"
        case .synced: "checkmark.circle"
        case .kept: "person.crop.circle"
        case .unsupported: "minus.circle"
        }
    }

    private var tint: Color {
        switch item.state {
        case .import: Theme.primary
        case .synced: Theme.success
        case .kept: Theme.secondaryText
        case .unsupported: Theme.tertiaryText
        }
    }
}

enum T3Summary {
    static func count(_ group: T3Group, _ state: T3ItemState) -> Int {
        group.items.filter { $0.state == state }.count
    }

    static func line(_ group: T3Group) -> String {
        let importing = group.items.filter { $0.state == .import }
        var parts: [String] = []
        if !importing.isEmpty {
            let names = importing.prefix(3).map(\.label).joined(separator: ", ")
            let more = importing.count > 3 ? " and \(importing.count - 3) more" : ""
            parts.append("\(names)\(more)")
        }
        let synced = count(group, .synced)
        if synced > 0 { parts.append("\(synced) already in sync") }
        let kept = count(group, .kept)
        if kept > 0 { parts.append("\(kept) changed here, kept") }
        let unsupported = count(group, .unsupported)
        if unsupported > 0 { parts.append("\(unsupported) not supported yet") }
        return parts.joined(separator: " · ")
    }

    static func word(_ state: T3ItemState) -> String {
        switch state {
        case .import: "to bring over"
        case .synced: "in sync"
        case .kept: "changed in Local Studio, kept"
        case .unsupported: "not supported yet"
        }
    }

    static func chosen(_ choices: T3Choices, _ kind: T3Kind) -> Bool {
        switch kind {
        case .accounts: choices.accounts
        case .projects: choices.projects
        case .models: choices.models
        case .keybindings: choices.keybindings
        case .appearance: choices.appearance
        case .preferences: choices.preferences
        }
    }

    static func set(_ choices: inout T3Choices, _ kind: T3Kind, _ value: Bool) {
        switch kind {
        case .accounts: choices.accounts = value
        case .projects: choices.projects = value
        case .models: choices.models = value
        case .keybindings: choices.keybindings = value
        case .appearance: choices.appearance = value
        case .preferences: choices.preferences = value
        }
    }

    static func pending(_ report: T3Report) -> Int {
        report.groups.reduce(0) { $0 + count($1, .import) }
    }
}
