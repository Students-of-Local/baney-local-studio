import SwiftUI

public struct MachineCard: View {
    let machine: MachineSummary

    public init(_ machine: MachineSummary) {
        self.machine = machine
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Image(systemName: machine.os == "macOS" ? "laptopcomputer" : "server.rack")
                    .font(.title3)
                    .foregroundStyle(Theme.secondaryText)
                    .frame(width: 28)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(machine.name).font(.headline).foregroundStyle(Theme.text)
                    Text("\(machine.os), \(machine.version) \(machine.channel)")
                        .font(.caption)
                        .foregroundStyle(Theme.secondaryText)
                }
                Spacer(minLength: 6)
                HealthLabel(machine: machine)
            }
            HStack(spacing: 14) {
                Count(value: machine.needsYou, title: "need you", tint: machine.needsYou > 0 ? Theme.approval : Theme.secondaryText)
                Count(value: machine.working, title: "working", tint: machine.working > 0 ? Theme.working : Theme.secondaryText)
                Count(value: machine.threads, title: "threads", tint: Theme.secondaryText)
                Spacer(minLength: 0)
            }
        }
        .padding(14)
        .background(Theme.card, in: RoundedRectangle(cornerRadius: Theme.Metrics.cardRadius, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Metrics.cardRadius, style: .continuous).strokeBorder(Theme.hairline))
        .accessibilityElement(children: .combine)
    }
}

public struct HealthDot: View {
    let health: MachineHealth

    public init(_ health: MachineHealth) {
        self.health = health
    }

    public var body: some View {
        Circle()
            .fill(Self.tint(health))
            .frame(width: 8, height: 8)
            .accessibilityLabel(health.title)
    }

    public static func tint(_ health: MachineHealth) -> Color {
        switch health {
        case .online: Theme.success
        case .slow, .reconnecting: Theme.warning
        case .offline: Theme.danger
        }
    }
}

struct HealthLabel: View {
    let machine: MachineSummary

    var body: some View {
        HStack(spacing: 6) {
            HealthDot(machine.health)
            Label(text, systemImage: machine.link.symbol)
                .labelStyle(CompactLabelStyle())
        }
        .font(.caption)
        .monospacedDigit()
        .foregroundStyle(Theme.secondaryText)
    }

    private var text: String {
        switch machine.health {
        case .online, .slow:
            if let latency = machine.latency { return "\(machine.link.title) \(latency) ms" }
            return machine.link.title
        case let .reconnecting(retry):
            if let retry { return "Retrying in \(retry) s" }
            return "Reconnecting"
        case .offline:
            return "Offline"
        }
    }
}

struct Count: View {
    let value: Int
    let title: String
    let tint: Color

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 4) {
            Text("\(value)").font(.subheadline.weight(.semibold)).monospacedDigit().foregroundStyle(tint)
            Text(title).font(.caption).foregroundStyle(Theme.secondaryText)
        }
    }
}
