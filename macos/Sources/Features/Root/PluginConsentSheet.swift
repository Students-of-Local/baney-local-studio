import AppKit
import LocalStudioKit
import SwiftUI

struct PluginConsentSheet: View {
    let consent: ConsentRequest
    let answer: (ConsentAnswer) -> Void
    @State private var allowed: Set<String> = []
    @State private var findingsOpen: Bool
    @FocusState private var focused: String?

    init(consent: ConsentRequest, findingsOpen: Bool = false, answer: @escaping (ConsentAnswer) -> Void) {
        self.consent = consent
        self.answer = answer
        _findingsOpen = State(initialValue: findingsOpen)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 14) {
                header
                scan
            }
            .padding(18)
            Rectangle().fill(Theme.hairline).frame(height: 1)
            VStack(alignment: .leading, spacing: 0) {
                Text("It asks to")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.secondaryText)
                    .padding(.bottom, 4)
                ForEach(Array(consent.rows.enumerated()), id: \.element.id) { index, row in
                    if index > 0 {
                        Rectangle().fill(Theme.hairline).frame(height: 1)
                    }
                    rowView(row)
                }
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 14)
            Rectangle().fill(Theme.hairline).frame(height: 1)
            footer
                .padding(18)
                .background(Theme.canvas)
        }
        .frame(width: 540)
        .background(Theme.card)
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            iconTile
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.system(size: 22, weight: .medium, design: .serif))
                    .foregroundStyle(Theme.text)
                    .fixedSize(horizontal: false, vertical: true)
                subtitle
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.secondaryText)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    @ViewBuilder private var iconTile: some View {
        let image = consent.icon.flatMap { NSImage(contentsOfFile: $0) }
        ZStack {
            RoundedRectangle(cornerRadius: 8).fill(Theme.primary.opacity(0.16))
            if let image {
                Image(nsImage: image).resizable().scaledToFit().padding(6)
            } else {
                Image(systemName: "puzzlepiece.extension").icon(.display).foregroundStyle(Theme.primary)
            }
        }
        .frame(width: 40, height: 40)
        .accessibilityHidden(true)
    }

    private var subtitle: Text {
        let description = consent.description.trimmingCharacters(in: CharacterSet(charactersIn: ". "))
        let lead = description.isEmpty ? "" : "\(description). "
        let replacing = consent.updateFrom.map { ", replacing \($0)," } ?? ""
        let version = "\(lead)Version \(consent.version)\(replacing) from \(consent.source)"
        guard let pin = consent.pin else { return Text("\(version).") }
        return Text("\(version), pinned to \(Text(pin).font(.system(size: 12, design: .monospaced))).")
    }

    private var scan: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Image(systemName: consent.findings.isEmpty ? "checkmark" : "exclamationmark.triangle")
                    .icon(.inline)
                    .foregroundStyle(consent.findings.isEmpty ? Theme.success : Theme.danger)
                Text(scanLine)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.text)
                if !consent.findings.isEmpty {
                    Button(findingsOpen ? "Hide findings" : "Show findings") { findingsOpen.toggle() }
                        .buttonStyle(.link)
                        .font(.system(size: 13))
                }
            }
            if findingsOpen {
                ForEach(consent.findings, id: \.self) { finding in
                    Text(finding)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(Theme.secondaryText)
                        .textSelection(.enabled)
                        .padding(.leading, 20)
                }
            }
        }
    }

    private var scanLine: String {
        let files = consent.files == 1 ? "1 file" : "\(consent.files) files"
        if consent.findings.isEmpty {
            return "Install scan found nothing: \(files), no binaries outside the manifest."
        }
        let count = consent.findings.count == 1 ? "1 finding" : "\(consent.findings.count) findings"
        return "Install scan: \(files), \(count) to read before you install."
    }

    private func rowView(_ row: ConsentRow) -> some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(row.sentence)
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(Theme.text)
                    if row.need == .sensitive {
                        Text("Sensitive")
                            .font(.system(size: 11, weight: .medium))
                            .foregroundStyle(Theme.danger)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 1)
                            .background(Theme.danger.opacity(0.12), in: RoundedRectangle(cornerRadius: 4))
                    }
                }
                Text("\(row.scope)  \(Text(row.id).font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.tertiaryText))")
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.secondaryText)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            if row.need == .sensitive {
                Toggle(row.sentence, isOn: binding(row.id))
                    .toggleStyle(.switch)
                    .labelsHidden()
                    .controlSize(.small)
                    .tint(Theme.primary)
                    .focusable()
                    .focused($focused, equals: row.id)
                    .onKeyPress(.return) {
                        binding(row.id).wrappedValue.toggle()
                        return .handled
                    }
            } else {
                Text("Needed")
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.secondaryText)
            }
        }
        .frame(minHeight: 44)
        .padding(.vertical, 4)
    }

    private var footer: some View {
        VStack(alignment: .leading, spacing: 12) {
            if consent.rows.contains(where: { $0.need == .sensitive }) {
                Text(
                    "The plugin's tools stay off until every permission it asks for is on. To grant one later, run \(Text("local-studio plugin grant \(consent.plugin) <capability>").font(.system(size: 11, design: .monospaced)))."
                )
                .font(.system(size: 12))
                .foregroundStyle(Theme.secondaryText)
                .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 8) {
                Spacer()
                Button("Cancel", role: .cancel) { answer(.cancel) }
                    .keyboardShortcut(.cancelAction)
                    .controlSize(.large)
                Button(confirmTitle) { answer(.install(sensitive: chosen)) }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.primary)
                    .controlSize(.large)
                    .disabled(!consent.findings.isEmpty && !findingsOpen)
                    .help(!consent.findings.isEmpty && !findingsOpen ? "Open the scan findings first" : "")
            }
        }
    }

    private var title: String {
        if !consent.install {
            return "Allow \(consent.name) to run?"
        }
        return consent.updateFrom == nil ? "Install \(consent.name)?" : "Update \(consent.name)?"
    }

    private var confirmTitle: String {
        let needed = consent.rows.filter { $0.need == .needed }.count
        let count = needed + chosen.count
        let noun = count == 1 ? "permission" : "permissions"
        let off = count < consent.rows.count
        if !consent.install {
            return off ? "Allow \(count) \(noun), tools off" : "Allow \(count) \(noun)"
        }
        let verb = consent.updateFrom == nil ? "Install" : "Update"
        if off {
            return "\(verb) with tools off"
        }
        return count == 0 ? "\(verb) with no new permissions" : "\(verb) with \(count) \(noun)"
    }

    private var chosen: [String] {
        consent.rows.filter { $0.need == .sensitive && allowed.contains($0.id) }.map(\.id)
    }

    private func binding(_ id: String) -> Binding<Bool> {
        Binding(
            get: { allowed.contains(id) },
            set: { on in
                if on { allowed.insert(id) } else { allowed.remove(id) }
            }
        )
    }
}

extension ConsentRequest: Identifiable {
    public var id: String { request }
}
