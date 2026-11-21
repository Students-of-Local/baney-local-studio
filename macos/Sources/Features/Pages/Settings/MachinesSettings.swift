import AppKit
import CoreImage.CIFilterBuiltins
import LocalStudioKit
import SwiftUI

struct PairedDevicesSection: View {
    let machines: MachinesStore
    @State private var pairing = false
    @State private var revoking: PairedDevice?

    var body: some View {
        SettingsSection(.pairedDevices, footer: "A paired phone or computer sees every thread on this Mac, reads them live, sends messages and answers approvals. Revoke a device to close its connection and make it pair again.") {
            SettingRow(.pairDevice) {
                LabeledContent {
                    Button("Pair a Device…") {
                        pairing = true
                        machines.openPairing()
                    }
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(SettingKey.pairDevice.title)
                        Text("Shows a code to scan with Local Studio on iPhone, or a link to paste on another computer.")
                            .font(.caption)
                            .foregroundStyle(Theme.secondaryText)
                    }
                }
            }
            SettingRow(.pairedDevices) {
                if machines.devices.isEmpty {
                    Text(machines.devicesLoaded ? "No devices are paired with this Mac." : "Loading…")
                        .foregroundStyle(Theme.secondaryText)
                } else {
                    ForEach(machines.devices) { device in
                        DeviceRow(device: device) { revoking = device }
                    }
                }
            }
        }
        .onAppear { machines.loadDevices() }
        .sheet(isPresented: $pairing, onDismiss: machines.closePairing) {
            PairingSheet(machines: machines) { pairing = false }
        }
        .confirmationDialog(
            "Revoke \(revoking?.name ?? "this device")?",
            isPresented: Binding(get: { revoking != nil }, set: { if !$0 { revoking = nil } }),
            presenting: revoking
        ) { device in
            Button("Revoke", role: .destructive) { machines.revoke(device) }
        } message: { _ in
            Text("Its connection closes now, and it has to pair again to reach this Mac.")
        }
    }
}

private struct DeviceRow: View {
    let device: PairedDevice
    let revoke: () -> Void

    var body: some View {
        LabeledContent {
            Button("Revoke…", action: revoke)
        } label: {
            HStack(spacing: 10) {
                Image(systemName: MachineGlyph.symbol(device.platform))
                    .icon(.control, .secondary)
                    .frame(width: 24)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(device.name)
                    Text(subtitle)
                        .font(.caption)
                        .foregroundStyle(Theme.secondaryText)
                }
            }
        }
    }

    private var subtitle: String {
        let seen = Date(timeIntervalSince1970: TimeInterval(device.lastSeen) / 1000)
        let parts = [MachineGlyph.platformName(device.platform), device.app.isEmpty ? nil : device.app, "last seen \(seen.formatted(.relative(presentation: .named)))"]
        return parts.compactMap { $0 }.joined(separator: " · ")
    }
}

struct PairingSheet: View {
    let machines: MachinesStore
    let done: () -> Void
    @State private var copied = false

    var body: some View {
        VStack(spacing: 16) {
            Text("Pair a Device")
                .font(.title2.bold())
            if let offer = machines.offer {
                offerView(offer)
            } else if let error = machines.offerError {
                Text(error)
                    .foregroundStyle(Theme.danger)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                ProgressView()
                    .controlSize(.small)
                    .frame(height: 220)
            }
            HStack {
                Spacer()
                Button("Done", action: done)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 420)
    }

    @ViewBuilder private func offerView(_ offer: PairingOffer) -> some View {
        if let code = QRCode.image(offer.link) {
            Image(nsImage: code)
                .interpolation(.none)
                .resizable()
                .frame(width: 200, height: 200)
                .padding(10)
                .background(Color.white, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                .accessibilityLabel("Pairing code")
        }
        Text("Scan the code with Local Studio on iPhone, or copy the link into Add a Machine on another computer.")
            .multilineTextAlignment(.center)
            .foregroundStyle(Theme.secondaryText)
            .fixedSize(horizontal: false, vertical: true)
        HStack {
            Text(offer.link)
                .font(.callout.monospaced())
                .lineLimit(1)
                .truncationMode(.middle)
                .textSelection(.enabled)
            Button(copied ? "Copied" : "Copy Link") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(offer.link, forType: .string)
                copied = true
            }
        }
        TimelineView(.periodic(from: .now, by: 1)) { context in
            Text(expiry(offer.expiresAt, now: context.date))
                .font(.caption)
                .foregroundStyle(Theme.secondaryText)
        }
        if !offer.serving {
            SettingsNote(
                symbol: "exclamationmark.triangle.fill",
                text: "This Mac is not serving remote devices yet, so the code will not connect. Run `local-studio-serve install --tailscale` in Terminal, then open this again."
            )
        } else if !offer.reachableElsewhere {
            SettingsNote(
                symbol: "exclamationmark.triangle.fill",
                text: "The link points at \(offer.url), which only this Mac can reach. Turn on Tailscale and run `local-studio-serve install --tailscale` so other devices can reach it."
            )
        }
    }

    private func expiry(_ date: Date, now: Date) -> String {
        let left = Int(date.timeIntervalSince(now).rounded(.down))
        guard left > 0 else { return "This code has expired. Close and open Pair a Device for a new one." }
        return "Works once, for the next \(left / 60):\(String(format: "%02d", left % 60))."
    }
}

struct OtherMachinesSection: View {
    let machines: MachinesStore
    @State private var adding = false
    @State private var forgetting: Machine?

    var body: some View {
        SettingsSection(.otherMachines, footer: "Threads on linked machines appear in the sidebar beside this Mac's, marked with the machine's name. Pair them the way a phone pairs: open Pair a Device on that machine and paste its link here.") {
            SettingRow(.otherMachines) {
                if machines.machines.isEmpty {
                    Text("No other machines are linked.")
                        .foregroundStyle(Theme.secondaryText)
                } else {
                    ForEach(machines.machines) { machine in
                        MachineRow(machine: machine) { forgetting = machine }
                    }
                }
            }
            SettingRow(.addMachine) {
                LabeledContent(SettingKey.addMachine.title) {
                    Button("Add a Machine…") { adding = true }
                }
            }
        }
        .sheet(isPresented: $adding) {
            AddMachineSheet { link in
                adding = false
                if let link { machines.pair(link) }
            }
        }
        .confirmationDialog(
            "Forget \(forgetting?.name ?? "this machine")?",
            isPresented: Binding(get: { forgetting != nil }, set: { if !$0 { forgetting = nil } }),
            presenting: forgetting
        ) { machine in
            Button("Forget", role: .destructive) { machines.forget(machine) }
        } message: { _ in
            Text("Its threads leave the sidebar. Nothing changes on that machine; pair again to bring them back.")
        }
    }
}

private struct MachineRow: View {
    let machine: Machine
    let forget: () -> Void

    var body: some View {
        LabeledContent {
            Button("Forget…", action: forget)
        } label: {
            HStack(spacing: 10) {
                Image(systemName: MachineGlyph.symbol(machine.os))
                    .icon(.control, .secondary)
                    .frame(width: 24)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(machine.name)
                    HStack(spacing: 5) {
                        Circle()
                            .fill(MachineStatus.color(machine.state))
                            .frame(width: 7, height: 7)
                            .accessibilityHidden(true)
                        Text(MachineStatus.text(machine))
                            .font(.caption)
                            .foregroundStyle(Theme.secondaryText)
                            .textSelection(.enabled)
                    }
                }
            }
        }
        .help(machine.url)
    }
}

struct AddMachineSheet: View {
    let finish: (String?) -> Void
    @State private var link = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Add a Machine")
                .font(.title2.bold())
            Text("On the other machine, open Settings › Connections › Pair a Device (or run `local-studio-serve pair`), copy the link and paste it here.")
                .foregroundStyle(Theme.secondaryText)
                .fixedSize(horizontal: false, vertical: true)
            TextField("Pairing link", text: $link, axis: .vertical)
                .lineLimit(2...4)
                .font(.callout.monospaced())
                .textFieldStyle(.roundedBorder)
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { finish(nil) }
                    .keyboardShortcut(.cancelAction)
                Button("Pair") { finish(link.trimmingCharacters(in: .whitespacesAndNewlines)) }
                    .keyboardShortcut(.defaultAction)
                    .disabled(link.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(20)
        .frame(width: 460)
        .onAppear {
            if let pasted = NSPasteboard.general.string(forType: .string), pasted.contains("://pair?") {
                link = pasted.trimmingCharacters(in: .whitespacesAndNewlines)
            }
        }
    }
}

enum MachineGlyph {
    static func symbol(_ platform: String) -> String {
        switch platform.lowercased() {
        case "ios", "iphone", "iphoneos": "iphone"
        case "ipados", "ipad": "ipad"
        case "macos", "mac", "darwin": "laptopcomputer"
        case "windows": "pc"
        default: "desktopcomputer"
        }
    }

    static func platformName(_ platform: String) -> String? {
        switch platform.lowercased() {
        case "ios", "iphone", "iphoneos": "iPhone"
        case "ipados", "ipad": "iPad"
        case "macos", "mac", "darwin": "Mac"
        case "linux": "Linux"
        case "windows": "Windows"
        case "": nil
        default: platform
        }
    }
}

enum MachineStatus {
    static func text(_ machine: Machine) -> String {
        switch machine.state {
        case .connecting:
            return "Connecting…"
        case .connected:
            let latency = machine.latencyMs.map { " · \($0) ms" } ?? ""
            return "Connected\(latency)"
        case .offline(let since):
            let date = Date(timeIntervalSince1970: TimeInterval(since) / 1000)
            return "Offline since \(date.formatted(date: .omitted, time: .shortened)), retrying"
        case .pairAgain:
            return "Pair again: \(machine.name) no longer accepts this computer"
        case .incompatible(let proto, let minProtocol):
            let spoken = minProtocol == proto ? "protocol \(proto)" : "protocols \(minProtocol) to \(proto)"
            return "Update Local Studio: \(machine.name) speaks \(spoken)"
        case .refused(let until):
            let date = Date(timeIntervalSince1970: TimeInterval(until) / 1000)
            return "Refused after failed attempts, retrying at \(date.formatted(date: .omitted, time: .shortened))"
        }
    }

    static func color(_ state: MachineState) -> Color {
        switch state {
        case .connected: Theme.success
        case .connecting, .offline: Theme.warning
        case .pairAgain, .incompatible, .refused: Theme.danger
        }
    }
}

enum QRCode {
    static func image(_ text: String) -> NSImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage else { return nil }
        let scaled = output.transformed(by: CGAffineTransform(scaleX: 8, y: 8))
        let representation = NSCIImageRep(ciImage: scaled)
        let image = NSImage(size: representation.size)
        image.addRepresentation(representation)
        return image
    }
}
