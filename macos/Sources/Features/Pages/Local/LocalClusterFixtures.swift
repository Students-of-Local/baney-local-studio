import Foundation
import LocalStudioKit

extension PagesFixtures {
    private static let egpuLink = "PCIe 3.0 x4, about 3.9 GB/s over the cable. Loading a model takes longer than on an internal slot; once it is loaded, decoding speed is nearly the same."

    private static var checksPassing: [LocalClusterCheck] {
        [
            LocalClusterCheck(name: "GPU programs", ok: true, detail: "only fleet work is on the GPU"),
            LocalClusterCheck(name: "GPU use", ok: true, detail: "idle"),
            LocalClusterCheck(name: "Power", ok: true, detail: "on AC power"),
            LocalClusterCheck(name: "parish-media", ok: true, detail: "ready"),
        ]
    }

    private static var fleetMachines: [LocalClusterMachine] {
        [
            LocalClusterMachine(id: "ctl-studio", name: "studio", lend: .always, state: .lent, reason: "Lending 2 GPUs to the fleet.", gpus: 2, freeMib: 44_000),
            LocalClusterMachine(
                id: "ctl-rig", name: "rig-02", lend: .idle, state: .reclaimed,
                reason: "Took its GPU back: a program (pid 63610) is using NVIDIA GeForce RTX 3090.", gpus: 1, freeMib: 0
            ),
            LocalClusterMachine(id: "ctl-mbp", name: "gil-mbp", lend: .never, state: .off, reason: "Lending is off on this machine.", gpus: 0, freeMib: 0),
        ]
    }

    private static var poolModels: [LocalPoolModel] {
        [
            LocalPoolModel(path: "/models/Llama-4-Scout-17B-16E-Q4_K_M.gguf", name: "Llama-4-Scout-17B-16E-Q4_K_M", sizeBytes: 65 * gib, fitsOne: false, fitsPool: true),
            LocalPoolModel(path: "/models/qwen3-32b-q8_0.gguf", name: "qwen3-32b-q8_0", sizeBytes: 34 * gib, fitsOne: true, fitsPool: true),
            LocalPoolModel(path: "/models/DeepSeek-V4.1-Q2_K-00001-of-00004.gguf", name: "DeepSeek-V4.1-Q2_K", sizeBytes: 210 * gib, fitsOne: false, fitsPool: false),
        ]
    }

    static var egpuMachine: LocalController {
        var pod = studio
        pod.name = "parish"
        pod.id = "ctl-parish"
        pod.headline = "qwen3-coder-30b-a3b"
        pod.detail = "parish · port 8001"
        pod.hardware = LocalHardware(
            cpus: 24, memoryTotalBytes: 64 * gib, memoryFreeBytes: 30 * gib,
            gpus: [
                LocalGpu(
                    index: 0, name: "NVIDIA GeForce RTX 4080 Laptop GPU", card: "rtx-4080", memoryUsedMib: 10_200, memoryTotalMib: 12_282, busy: true,
                    external: false, link: "PCIe 4.0 x16, about 31.5 GB/s."
                ),
                LocalGpu(
                    index: 1, name: "NVIDIA GeForce RTX 5090", card: "rtx-5090", memoryUsedMib: 1_100, memoryTotalMib: 32_607, busy: false,
                    external: true, link: egpuLink
                ),
            ],
            memoryUsedBytes: 11 * gib, memoryCapacityBytes: 44 * gib, memoryLabel: "VRAM", gpuSummary: "RTX 4080 Laptop + RTX 5090 eGPU",
            egpuNote: nil,
            thunderbolt: [
                LocalThunderbolt(uuid: "d5010000-0070-6418-2317-5c0a1a2b3c4d", name: "Core X V2", vendor: "Razer", state: .pending, message: nil),
            ],
            hotplug: [
                LocalHotplug(
                    at: "2026-11-21T14:02:11Z", added: true, gpu: "NVIDIA GeForce RTX 5090", external: true,
                    message: "NVIDIA GeForce RTX 5090 was plugged in over Thunderbolt.", suggestions: ["qwen3-32b (vllm)", "gpt-oss-20b (llama.cpp)"]
                ),
            ]
        )
        pod.cluster = LocalCluster(
            discover: true, lend: .idle, state: .lent, reason: "Lending: every check has passed for 60 s.", rpc: true, draining: false,
            freeMib: 31_500, checks: checksPassing,
            lent: [LocalLentServer(endpoint: "100.88.4.20:50052", forName: "studio", gpus: [1])],
            pools: [
                LocalPool(
                    id: "pool-1", alias: "Llama-4-Scout-17B-16E-Q4_K_M", port: 18_120, members: ["parish", "studio"],
                    phase: .ready, message: nil
                ),
            ],
            models: poolModels, machines: fleetMachines
        )
        return pod
    }

    static var reclaimedMachine: LocalController {
        var pod = egpuMachine
        guard var hardware = pod.hardware, var cluster = pod.cluster else { return pod }
        hardware.thunderbolt = [
            LocalThunderbolt(
                uuid: "d5010000-0070-6418-2317-5c0a1a2b3c4d", name: "Core X V2", vendor: "Razer", state: .failed,
                message: "Approving this device failed. Unplug it, plug it back in and try again."
            ),
        ]
        hardware.hotplug = [
            LocalHotplug(
                at: "2026-11-21T14:40:02Z", added: false, gpu: "NVIDIA GeForce RTX 5090", external: true,
                message: "NVIDIA GeForce RTX 5090 was unplugged. Its job failed.", suggestions: []
            ),
        ]
        hardware.gpus = hardware.gpus.filter { !$0.external }
        pod.hardware = hardware
        cluster.state = .reclaimed
        cluster.reason = "Took its GPUs back: a program (pid 63610) is using NVIDIA GeForce RTX 4080 Laptop GPU."
        cluster.checks = [
            LocalClusterCheck(name: "GPU programs", ok: false, detail: "pid 63610 is not fleet work"),
            LocalClusterCheck(name: "GPU use", ok: false, detail: "G0 is 84% busy"),
            LocalClusterCheck(name: "Power", ok: true, detail: "on AC power"),
        ]
        cluster.lent = []
        cluster.freeMib = 0
        cluster.pools = [
            LocalPool(
                id: "pool-1", alias: "Llama-4-Scout-17B-16E-Q4_K_M", port: 18_120, members: ["parish", "studio"], phase: .failed,
                message: "Machine parish took its GPU back: a program (pid 63610) is using NVIDIA GeForce RTX 4080 Laptop GPU."
            ),
        ]
        pod.cluster = cluster
        return pod
    }

    static var appleSiliconMachine: LocalController {
        var mac = laptop
        mac.target = ""
        mac.local = true
        mac.status = .noModel
        mac.headline = "No model loaded on gil-mbp"
        mac.detail = "gil-mbp is idle."
        mac.jobs = []
        mac.hardware?.egpuNote = "Apple Silicon Macs cannot use an external GPU. Use a GPU on another machine in your fleet instead."
        mac.cluster = LocalCluster(
            discover: true, lend: .never, state: .off, reason: "Lending is off. This Mac has no GPU the fleet can use.", rpc: false, draining: false,
            freeMib: 0, checks: [], lent: [], pools: [], models: [], machines: Array(fleetMachines.prefix(2))
        )
        return mac
    }

    static func clusterSnapshot(_ controller: LocalController, with other: LocalController) -> LocalSnapshot {
        LocalSnapshot(
            controllers: [controller, other], registry: nil, usage: localUsage, tailnet: nil,
            record: nil, share: nil, autoModel: nil, generatedAt: nil, error: nil
        )
    }
}
