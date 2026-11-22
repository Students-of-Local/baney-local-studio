import LocalStudioKit
import SwiftUI

struct LocalClusterPanel: View {
    let controller: LocalController
    let pages: PagesStore

    private var hardware: LocalHardware? { controller.hardware }
    private var devices: [LocalThunderbolt] { (hardware?.thunderbolt ?? []).filter { $0.state != .authorized } }
    private var hotplug: [LocalHotplug] { Array((hardware?.hotplug ?? []).prefix(3)) }

    var body: some View {
        VStack(alignment: .leading, spacing: 36) {
            if hardware != nil {
                HStack(alignment: .top, spacing: 44) {
                    LocalGpuList(controller: controller, pages: pages, devices: devices, hotplug: hotplug)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    if let cluster = controller.cluster {
                        LocalLendingBlock(controller: controller, cluster: cluster, pages: pages)
                            .frame(width: 360, alignment: .leading)
                    }
                }
            }
            if let cluster = controller.cluster, !cluster.machines.isEmpty || !cluster.pools.isEmpty || !cluster.models.isEmpty {
                HStack(alignment: .top, spacing: 44) {
                    LocalFleetMachines(machines: cluster.machines)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    LocalPoolBlock(controller: controller, cluster: cluster, pages: pages)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }
}

private struct PanelTitle: View {
    let title: String
    var detail: String?

    init(_ title: String, detail: String? = nil) {
        self.title = title
        self.detail = detail
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(title)
                .font(.system(size: 13, weight: .medium))
            if let detail {
                Text(detail)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.secondaryText)
                    .lineLimit(1)
            }
        }
        .padding(.bottom, 4)
    }
}

private struct ExternalBadge: View {
    var body: some View {
        IconLabel("eGPU", symbol: "cable.connector", size: .inline)
            .font(.system(size: 11, weight: .medium))
            .foregroundStyle(Theme.text)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(Theme.card, in: Capsule())
            .overlay { Capsule().strokeBorder(Theme.hairline) }
            .help("Connected over Thunderbolt or USB4")
            .accessibilityLabel("External GPU")
    }
}

private struct StateMark: View {
    let ok: Bool
    var warn = false

    var body: some View {
        Image(systemName: ok ? "checkmark" : warn ? "exclamationmark.triangle.fill" : "xmark")
            .icon(.inline, .primary)
            .foregroundStyle(ok ? Theme.success : warn ? Theme.warning : Theme.danger)
            .frame(width: 16)
            .accessibilityLabel(ok ? "Passes" : "Fails")
    }
}

private struct LocalGpuList: View {
    let controller: LocalController
    let pages: PagesStore
    let devices: [LocalThunderbolt]
    let hotplug: [LocalHotplug]

    private var gpus: [LocalGpu] { controller.hardware?.gpus ?? [] }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            PanelTitle("GPUs on \(controller.name)", detail: gpus.contains(where: \.external) ? "one connected by cable" : nil)
            if let note = controller.hardware?.egpuNote {
                IconLabel(note, symbol: "exclamationmark.triangle.fill")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.secondaryText)
            }
            if gpus.isEmpty {
                Text("No NVIDIA GPU reported.")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.secondaryText)
            }
            ForEach(gpus, id: \.index) { gpu in
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Text("G\(gpu.index)")
                        .font(.system(size: 12))
                        .monospacedDigit()
                        .foregroundStyle(Theme.secondaryText)
                        .frame(width: 24, alignment: .leading)
                    VStack(alignment: .leading, spacing: 3) {
                        HStack(spacing: 8) {
                            Text(gpu.name)
                                .font(.system(size: 13))
                                .lineLimit(1)
                            if gpu.external {
                                ExternalBadge()
                            }
                        }
                        if let link = gpu.link {
                            Text(link)
                                .font(.system(size: 12))
                                .foregroundStyle(Theme.secondaryText)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    Spacer(minLength: 8)
                    Text("\(LocalFormat.gigabytes(gpu.memoryUsedMib * 1_048_576)) / \(LocalFormat.gigabytes(gpu.memoryTotalMib * 1_048_576)) GB")
                        .font(.system(size: 12))
                        .monospacedDigit()
                        .foregroundStyle(Theme.secondaryText)
                }
            }
            ForEach(devices, id: \.uuid) { device in
                ThunderboltRow(controller: controller, device: device, pages: pages)
            }
            ForEach(Array(hotplug.enumerated()), id: \.offset) { _, event in
                VStack(alignment: .leading, spacing: 2) {
                    IconLabel(event.message, symbol: event.added ? "plus" : "minus")
                        .font(.system(size: 12.5))
                        .foregroundStyle(event.added ? Theme.text : Theme.warning)
                    if !event.suggestions.isEmpty {
                        Text("Fits this card: \(event.suggestions.joined(separator: ", "))")
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.secondaryText)
                            .padding(.leading, 22)
                    }
                }
            }
        }
    }
}

private struct ThunderboltRow: View {
    let controller: LocalController
    let device: LocalThunderbolt
    let pages: PagesStore

    private var pending: Bool { pages.isPending(.authorize(controller.target, device.uuid)) }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            StateMark(ok: false, warn: device.state != .failed)
            VStack(alignment: .leading, spacing: 2) {
                Text([device.name, device.vendor].compactMap { $0 }.joined(separator: " · "))
                    .font(.system(size: 13))
                    .lineLimit(1)
                Text(device.message ?? status)
                    .font(.system(size: 12))
                    .foregroundStyle(device.state == .failed ? Theme.danger : Theme.secondaryText)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            if controller.local, device.state != .authorizing {
                Button(pending ? "Authorizing…" : device.state == .failed ? "Try Again" : "Authorize") {
                    pages.perform(.authorize(controller: controller.target, uuid: device.uuid), pending: .authorize(controller.target, device.uuid))
                }
                .buttonStyle(DashboardButtonStyle(bordered: true))
                .disabled(pending)
                .help("Approve this Thunderbolt device so its GPU can be used")
            }
        }
    }

    private var status: String {
        switch device.state {
        case .pending: controller.local ? "Waiting for approval before its GPU can be used." : "Waiting for approval. Approve it on \(controller.name) itself."
        case .authorizing: "Approving…"
        case .failed: "Approving this device failed."
        case .authorized: "Approved"
        }
    }
}

private struct LocalLendingBlock: View {
    let controller: LocalController
    let cluster: LocalCluster
    let pages: PagesStore

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            PanelTitle("Lending")
            HStack(spacing: 8) {
                LendDot(state: cluster.state)
                Text(cluster.state.title)
                    .font(.system(size: 13, weight: .medium))
                if cluster.draining {
                    Text("finishing requests")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.secondaryText)
                }
            }
            Text(cluster.reason)
                .font(.system(size: 12.5))
                .foregroundStyle(cluster.state == .reclaimed ? Theme.warning : Theme.secondaryText)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
            Picker("Lend GPUs", selection: lendBinding) {
                Text("Never").tag(LocalLendMode.never)
                Text("When idle").tag(LocalLendMode.idle)
                Text("Always").tag(LocalLendMode.always)
            }
            .pickerStyle(.menu)
            .fixedSize()
            .disabled(pages.isPending(.lending(controller.target)))
            Toggle("Find machines on the tailnet", isOn: discoverBinding)
                .toggleStyle(.checkbox)
                .disabled(pages.isPending(.discovery(controller.target)))
            if cluster.lend != .never, !cluster.checks.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(cluster.checks, id: \.name) { check in
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            StateMark(ok: check.ok)
                            Text(check.name)
                                .font(.system(size: 12.5))
                            Text(check.detail)
                                .font(.system(size: 12))
                                .foregroundStyle(Theme.secondaryText)
                                .lineLimit(2)
                        }
                    }
                }
                .padding(.top, 2)
            }
            ForEach(cluster.lent, id: \.endpoint) { server in
                IconLabel("Lent to \(server.forName), \(server.gpus.map { "G\($0)" }.joined(separator: ", "))", symbol: "arrow.up.right")
                    .font(.system(size: 12.5))
            }
        }
    }

    private var lendBinding: Binding<LocalLendMode> {
        Binding(get: { cluster.lend }, set: { mode in
            guard mode != cluster.lend else { return }
            pages.perform(.setLending(controller: controller.target, lend: mode), pending: .lending(controller.target))
        })
    }

    private var discoverBinding: Binding<Bool> {
        Binding(get: { cluster.discover }, set: { on in
            pages.perform(.setDiscovery(controller: controller.target, discover: on), pending: .discovery(controller.target))
        })
    }
}

private struct LendDot: View {
    let state: LocalLendState
    @Environment(\.accessibilityDifferentiateWithoutColor) private var differentiate

    var body: some View {
        Group {
            if differentiate {
                Image(systemName: state == .lent ? "checkmark" : state == .reclaimed ? "arrow.uturn.backward" : "minus")
                    .icon(.inline, .primary)
            } else {
                switch state {
                case .lent: Circle().fill(Theme.success).frame(width: 7, height: 7)
                case .reclaimed: Circle().fill(Theme.warning).frame(width: 7, height: 7)
                case .off: Circle().fill(Theme.tertiaryText).frame(width: 7, height: 7)
                }
            }
        }
        .accessibilityLabel(state.title)
    }
}

extension LocalLendState {
    var title: String {
        switch self {
        case .lent: "Lending its GPUs"
        case .reclaimed: "Took its GPUs back"
        case .off: "Not lending"
        }
    }
}

private struct LocalFleetMachines: View {
    let machines: [LocalClusterMachine]

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            PanelTitle("Machines in the fleet", detail: "\(machines.filter { $0.state == .lent }.count) lending")
            if machines.isEmpty {
                Text("No other machine is linked yet. Turn on Find machines on the tailnet, or add one in Settings, Local.")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.secondaryText)
            }
            ForEach(machines, id: \.id) { machine in
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    LendDot(state: machine.state)
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: 8) {
                            Text(machine.name)
                                .font(.system(size: 13, weight: .medium))
                            Text(machine.state.title)
                                .font(.system(size: 12))
                                .foregroundStyle(Theme.secondaryText)
                        }
                        Text(machine.reason)
                            .font(.system(size: 12))
                            .foregroundStyle(machine.state == .reclaimed ? Theme.warning : Theme.secondaryText)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Spacer(minLength: 8)
                    Text(machine.gpus == 0 ? "no GPU" : "\(machine.gpus) GPU\(machine.gpus == 1 ? "" : "s"), \(LocalFormat.gigabytes(machine.freeMib * 1_048_576)) GB free")
                        .font(.system(size: 12))
                        .monospacedDigit()
                        .foregroundStyle(Theme.secondaryText)
                }
            }
        }
    }
}

private struct LocalPoolBlock: View {
    let controller: LocalController
    let cluster: LocalCluster
    let pages: PagesStore

    private var pooled: UInt64 { cluster.machines.filter { $0.state == .lent }.reduce(cluster.state == .lent ? cluster.freeMib : 0) { $0 + $1.freeMib } }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            PanelTitle("Pooled models", detail: "\(LocalFormat.gigabytes(pooled * 1_048_576)) GB lent across the fleet")
            ForEach(cluster.pools, id: \.id) { pool in
                PoolRow(controller: controller, pool: pool, pages: pages)
            }
            if cluster.models.isEmpty {
                Text("No GGUF models found on \(controller.name). Pooled models run with llama.cpp across every lending machine.")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.secondaryText)
            }
            ForEach(cluster.models, id: \.path) { model in
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(model.name)
                            .font(.system(size: 13))
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Text(fit(model))
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.secondaryText)
                    }
                    Spacer(minLength: 8)
                    Button(pages.isPending(.pool(controller.target)) ? "Starting…" : "Run on the Pool") {
                        pages.perform(.launchPool(controller: controller.target, model: model.path), pending: .pool(controller.target))
                    }
                    .buttonStyle(DashboardButtonStyle(bordered: true))
                    .disabled(!model.fitsPool || pages.isPending(.pool(controller.target)))
                    .help(model.fitsPool ? "Split \(model.name) across every lending machine" : "The lending machines do not have enough free GPU memory")
                }
            }
        }
    }

    private func fit(_ model: LocalPoolModel) -> String {
        let size = "\(LocalFormat.gigabytes(model.sizeBytes)) GB"
        if model.fitsOne { return "\(size), fits on one machine" }
        return model.fitsPool ? "\(size), needs the pool" : "\(size), too large for the GPUs lent now"
    }
}

private struct PoolRow: View {
    let controller: LocalController
    let pool: LocalPool
    let pages: PagesStore

    private var stopping: Bool { pages.isPending(.stopPool(controller.target, pool.id)) }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            StateMark(ok: pool.phase == .ready, warn: pool.phase == .starting)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 8) {
                    Text(pool.alias)
                        .font(.system(size: 13, weight: .medium))
                        .lineLimit(1)
                    Text(phase)
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.secondaryText)
                }
                Text(pool.message ?? "On \(pool.members.joined(separator: ", ")) · port \(String(pool.port))")
                    .font(.system(size: 12))
                    .foregroundStyle(pool.phase == .failed ? Theme.danger : Theme.secondaryText)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            if pool.phase == .starting || pool.phase == .ready {
                Button(stopping ? "Stopping…" : "Stop") {
                    pages.perform(.stopPool(controller: controller.target, pool: pool.id), pending: .stopPool(controller.target, pool.id))
                }
                .buttonStyle(DashboardButtonStyle())
                .disabled(stopping)
            }
        }
    }

    private var phase: String {
        switch pool.phase {
        case .starting: "starting"
        case .ready: "serving"
        case .failed: "stopped"
        case .stopped: "stopped"
        }
    }
}
