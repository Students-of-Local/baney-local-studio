import Foundation
import LocalStudioKit
import Observation

struct PairingOffer: Equatable {
    let link: String
    let url: String
    let serving: Bool
    let expiresAt: Date

    var reachableElsewhere: Bool {
        guard let host = URL(string: url)?.host() else { return false }
        return host != "127.0.0.1" && host != "localhost" && host != "::1"
    }
}

@MainActor
@Observable
final class MachinesStore {
    private(set) var machines: [Machine] = []
    private(set) var devices: [PairedDevice] = []
    private(set) var devicesLoaded = false
    private(set) var offer: PairingOffer?
    private(set) var offerError: String?
    private(set) var opening = false

    @ObservationIgnored private let core: CoreHandle
    @ObservationIgnored private let requests: RequestCenter
    @ObservationIgnored private(set) var offerRequest: String?
    @ObservationIgnored private var sheetRequest: String?

    init(core: CoreHandle, requests: RequestCenter) {
        self.core = core
        self.requests = requests
    }

    func setMachines(_ machines: [Machine]) {
        self.machines = machines.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    func setDevices(_ devices: [PairedDevice]) {
        self.devices = devices
        devicesLoaded = true
    }

    func setPairing(request: String, link: String, url: String, serving: Bool, expiresAt: Int64) {
        guard request == offerRequest else { return }
        requests.settle(request)
        offerRequest = nil
        opening = false
        offer = PairingOffer(link: link, url: url, serving: serving, expiresAt: Date(timeIntervalSince1970: TimeInterval(expiresAt) / 1000))
    }

    func machine(_ id: String?) -> Machine? {
        guard let id else { return nil }
        return machines.first { $0.id == id }
    }

    func name(_ id: String) -> String {
        machine(id)?.name ?? id
    }

    func loadDevices() {
        core.run { $0.listPairedDevices() }
    }

    func openPairing() {
        opening = true
        offerError = nil
        let request = requests.issue { [weak self] outcome in
            guard let self else { return }
            opening = false
            offerRequest = nil
            if case .failed(let message) = outcome { offerError = message }
        }
        offerRequest = request
        let previous = sheetRequest
        sheetRequest = request
        core.run { studio in
            if let previous { studio.closePairing(request: previous) }
            studio.openPairing(request: request)
        }
    }

    func closePairing() {
        if let offerRequest { requests.settle(offerRequest) }
        if let sheetRequest { core.run { $0.closePairing(request: sheetRequest) } }
        sheetRequest = nil
        offerRequest = nil
        opening = false
        offer = nil
        offerError = nil
        loadDevices()
    }

    func revoke(_ device: PairedDevice) {
        core.run { $0.revokePairedDevice(device: device.id) }
    }

    func pair(_ link: String) {
        core.run { $0.pairMachine(link: link) }
    }

    func forget(_ machine: Machine) {
        core.run { $0.forgetMachine(machine: machine.id) }
    }
}

extension Machine: Identifiable {}
extension PairedDevice: Identifiable {}

extension MachineState {
    var connected: Bool {
        if case .connected = self { return true }
        return false
    }
}
