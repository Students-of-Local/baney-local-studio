import Foundation
import Testing
@testable import LocalStudioRemote

/// Pairs with a real `local-studio-serve` through `XXpsk3`, then reconnects through `KK`, using
/// the app's own Noise state machine, records and frame codec, so a pass here means the phone
/// and `snow` agree end to end. Set LOCAL_STUDIO_PAIR_LINK to a fresh `pair` link to run it.
@Suite struct ServeInteropTests {
    struct Opened {
        var session: any SecureSession
        var reader = RecordReader()
        let socket: URLSessionWebSocketTask
    }

    static let link = ProcessInfo.processInfo.environment["LOCAL_STUDIO_PAIR_LINK"].flatMap(PairingLink.init(string:))

    func open(_ url: URL, role: HandshakeRole, staticKey: Data) async throws -> Opened {
        let socket = URLSession.shared.webSocketTask(with: url)
        socket.resume()
        let header = try FrameCodec.encode(OpeningHeader(role: role))
        try await socket.send(.string(try #require(String(bytes: header, encoding: .utf8))))
        let session = try await NoiseChannel().handshake(role, staticKey: staticKey, prologue: Data(remotePrologue.utf8) + header, over: SocketTransport(socket: socket))
        return Opened(session: session, socket: socket)
    }

    func hello(_ opened: inout Opened, resume: ResumeState) async throws -> HelloReplyValue {
        let frame = try FrameCodec.encode(HelloFrame(device: HelloDevice(name: "Interop Phone", app: "0.0.0", channel: "dev"), resume: resume))
        for record in try RecordWriter.frame(frame) {
            try await opened.socket.send(.data(try opened.session.seal(record)))
        }
        while true {
            guard case let .data(data) = try await opened.socket.receive() else {
                throw RemoteError.unexpected("a text frame after the handshake")
            }
            guard case let .frame(json) = try opened.reader.read(try opened.session.open(data)) else {
                continue
            }
            if case let .hello(reply) = try FrameCodec.decode(json) {
                return reply
            }
        }
    }

    @Test(.enabled(if: link != nil)) func pairsThenReconnectsThroughKK() async throws {
        let link = try #require(Self.link)
        let staticKey = StaticKey.generate()
        let device = DeviceInfo(id: UUID().uuidString.lowercased(), name: "Interop Phone", platform: "ios", app: "0.0.0", channel: "dev")
        var paired = try await open(link.url, role: .pair(id: link.id, psk: link.secret, device: device), staticKey: staticKey)
        let first = try await hello(&paired, resume: ResumeState(seq: nil, epoch: nil, watch: []))
        #expect(first.resume == .snapshot)
        let machineKey = paired.session.remoteStatic
        paired.socket.cancel(with: .goingAway, reason: nil)

        var known = try await open(link.url, role: .known(device: device.id, machineKey: machineKey), staticKey: staticKey)
        let second = try await hello(&known, resume: ResumeState(seq: first.seq, epoch: first.epoch, watch: []))
        #expect(second.epoch == first.epoch)
        #expect(second.resume == .delta)
        known.socket.cancel(with: .goingAway, reason: nil)
    }
}
