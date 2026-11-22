use std::rc::Rc;

use local_studio_core::{
    LocalCluster, LocalClusterCheck, LocalClusterMachine, LocalController, LocalGpu, LocalHardware, LocalHotplug,
    LocalLendMode, LocalLendState, LocalLentServer, LocalMachineStatus, LocalPool, LocalPoolModel, LocalPoolPhase,
    LocalSnapshot, LocalThunderbolt, LocalThunderboltState, Update,
};

use crate::app::App;

pub const SCENES: [&str; 3] = ["local-cluster-lending", "local-cluster-reclaimed", "local-cluster-apple-silicon"];

const GIB: u64 = 1_073_741_824;
const EGPU_LINK: &str = "PCIe 3.0 x4, about 3.9 GB/s over the cable. Loading a model takes longer than on an internal slot; once it is loaded, decoding speed is nearly the same.";
const DEVICE: &str = "d5010000-0070-6418-2317-5c0a1a2b3c4d";
const LAPTOP_RTX: &str = "NVIDIA GeForce RTX 4080 Laptop GPU";
const SCOUT: &str = "Llama-4-Scout-17B-16E-Q4_K_M";

fn text(value: &str) -> String {
    value.to_string()
}

fn check(name: &str, ok: bool, detail: &str) -> LocalClusterCheck {
    LocalClusterCheck { name: text(name), ok, detail: text(detail) }
}

fn machine(
    id: &str,
    name: &str,
    lend: LocalLendMode,
    state: LocalLendState,
    reason: &str,
    gpus: u32,
    free_mib: u64,
) -> LocalClusterMachine {
    LocalClusterMachine { id: text(id), name: text(name), lend, state, reason: text(reason), gpus, free_mib }
}

fn gpu(index: u32, name: &str, used: u64, total: u64, external: bool, link: &str) -> LocalGpu {
    LocalGpu {
        index,
        name: text(name),
        card: None,
        memory_used_mib: used,
        memory_total_mib: total,
        busy: used > 2048,
        external,
        link: Some(text(link)),
    }
}

fn model(name: &str, gib: u64, fits_one: bool, fits_pool: bool) -> LocalPoolModel {
    LocalPoolModel {
        path: format!("/models/{name}.gguf"),
        name: text(name),
        size_bytes: gib * GIB,
        fits_one,
        fits_pool,
    }
}

fn pool(phase: LocalPoolPhase, message: Option<&str>) -> LocalPool {
    LocalPool {
        id: text("pool-1"),
        alias: text(SCOUT),
        port: 18_120,
        members: vec![text("parish"), text("studio")],
        phase,
        message: message.map(text),
    }
}

fn fleet() -> Vec<LocalClusterMachine> {
    vec![
        machine(
            "ctl-studio",
            "studio",
            LocalLendMode::Always,
            LocalLendState::Lent,
            "Lending 2 GPUs to the fleet.",
            2,
            44_000,
        ),
        machine(
            "ctl-rig",
            "rig-02",
            LocalLendMode::Idle,
            LocalLendState::Reclaimed,
            "Took its GPU back: a program (pid 63610) is using NVIDIA GeForce RTX 3090.",
            1,
            0,
        ),
        machine(
            "ctl-mbp",
            "gil-mbp",
            LocalLendMode::Never,
            LocalLendState::Off,
            "Lending is off on this machine.",
            0,
            0,
        ),
    ]
}

fn controller(name: &str, url: &str, local: bool, hardware: LocalHardware, cluster: LocalCluster) -> LocalController {
    LocalController {
        target: if local { String::new() } else { text(url) },
        url: text(url),
        id: Some(format!("ctl-{name}")),
        name: text(name),
        host: text("127.0.0.1"),
        local,
        has_controller: true,
        reachable: true,
        status: LocalMachineStatus::NoModel,
        headline: format!("No model loaded on {name}"),
        detail: format!("{name} is idle. Load a model from the registry, or run one on the pool."),
        summary: text("No model loaded"),
        note: None,
        elsewhere: None,
        error: None,
        version: Some(text("0.1.0")),
        hardware: Some(hardware),
        endpoints: Vec::new(),
        jobs: Vec::new(),
        recipes: Vec::new(),
        requests: 0,
        tokens: 0,
        measured_requests: 0,
        cluster: Some(cluster),
    }
}

fn parish(reclaimed: bool) -> LocalController {
    let mut gpus = vec![gpu(0, LAPTOP_RTX, 10_200, 12_282, false, "PCIe 4.0 x16, about 31.5 GB/s.")];
    if !reclaimed {
        gpus.push(gpu(1, "NVIDIA GeForce RTX 5090", 1_100, 32_607, true, EGPU_LINK));
    }
    let (state, message, hotplug) = if reclaimed {
        (
            LocalThunderboltState::Failed,
            Some(text("Approving this device failed. Unplug it, plug it back in and try again.")),
            LocalHotplug {
                at: text("2026-11-21T14:40:02Z"),
                added: false,
                gpu: text("NVIDIA GeForce RTX 5090"),
                external: true,
                message: text("NVIDIA GeForce RTX 5090 was unplugged. Its job failed."),
                suggestions: Vec::new(),
            },
        )
    } else {
        (
            LocalThunderboltState::Pending,
            None,
            LocalHotplug {
                at: text("2026-11-21T14:02:11Z"),
                added: true,
                gpu: text("NVIDIA GeForce RTX 5090"),
                external: true,
                message: text("NVIDIA GeForce RTX 5090 was plugged in over Thunderbolt."),
                suggestions: vec![text("qwen3-32b (vllm)"), text("gpt-oss-20b (llama.cpp)")],
            },
        )
    };
    let hardware = LocalHardware {
        cpus: 24,
        memory_total_bytes: 64 * GIB,
        memory_free_bytes: 30 * GIB,
        gpus,
        memory_used_bytes: 11 * GIB,
        memory_capacity_bytes: 44 * GIB,
        memory_label: text("VRAM"),
        gpu_summary: text("RTX 4080 Laptop + RTX 5090 eGPU"),
        egpu_note: None,
        thunderbolt: vec![LocalThunderbolt {
            uuid: text(DEVICE),
            name: text("Core X V2"),
            vendor: Some(text("Razer")),
            state,
            message,
        }],
        hotplug: vec![hotplug],
    };
    controller("parish", "http://127.0.0.1:18091", true, hardware, parish_cluster(reclaimed))
}

fn parish_cluster(reclaimed: bool) -> LocalCluster {
    let models = vec![
        model(SCOUT, 65, false, true),
        model("qwen3-32b-q8_0", 34, true, true),
        model("DeepSeek-V4.1-Q2_K", 210, false, false),
    ];
    if reclaimed {
        LocalCluster {
            discover: true,
            lend: LocalLendMode::Idle,
            state: LocalLendState::Reclaimed,
            reason: format!("Took its GPUs back: a program (pid 63610) is using {LAPTOP_RTX}."),
            rpc: true,
            draining: false,
            free_mib: 0,
            checks: vec![
                check("GPU programs", false, "pid 63610 is not fleet work"),
                check("GPU use", false, "G0 is 84% busy"),
                check("Power", true, "on AC power"),
            ],
            lent: Vec::new(),
            pools: vec![pool(
                LocalPoolPhase::Failed,
                Some(&format!("Machine parish took its GPU back: a program (pid 63610) is using {LAPTOP_RTX}.")),
            )],
            models,
            machines: fleet(),
        }
    } else {
        LocalCluster {
            discover: true,
            lend: LocalLendMode::Idle,
            state: LocalLendState::Lent,
            reason: text("Lending: every check has passed for 60 s."),
            rpc: true,
            draining: false,
            free_mib: 31_500,
            checks: vec![
                check("GPU programs", true, "only fleet work is on the GPU"),
                check("GPU use", true, "idle"),
                check("Power", true, "on AC power"),
                check("parish-media", true, "ready"),
            ],
            lent: vec![LocalLentServer {
                endpoint: text("100.88.4.20:50052"),
                for_name: text("studio"),
                gpus: vec![1],
            }],
            pools: vec![pool(LocalPoolPhase::Ready, None)],
            models,
            machines: fleet(),
        }
    }
}

fn apple() -> LocalController {
    let hardware = LocalHardware {
        cpus: 14,
        memory_total_bytes: 64 * GIB,
        memory_free_bytes: 30 * GIB,
        gpus: Vec::new(),
        memory_used_bytes: 34 * GIB,
        memory_capacity_bytes: 64 * GIB,
        memory_label: text("Memory"),
        gpu_summary: text("Apple M4 Max"),
        egpu_note: Some(text(
            "Apple Silicon Macs cannot use an external GPU. Use a GPU on another machine in your fleet instead.",
        )),
        thunderbolt: Vec::new(),
        hotplug: Vec::new(),
    };
    let cluster = LocalCluster {
        discover: true,
        lend: LocalLendMode::Never,
        state: LocalLendState::Off,
        reason: text("Lending is off. This Mac has no GPU the fleet can use."),
        rpc: false,
        draining: false,
        free_mib: 0,
        checks: Vec::new(),
        lent: Vec::new(),
        pools: Vec::new(),
        models: Vec::new(),
        machines: fleet().into_iter().take(2).collect(),
    };
    controller("gil-mbp", "http://127.0.0.1:18091", true, hardware, cluster)
}

pub fn stage(app: &Rc<App>, scene: &str) {
    let shown = match scene {
        "local-cluster-reclaimed" => parish(true),
        "local-cluster-apple-silicon" => apple(),
        _ => parish(false),
    };
    let url = shown.url.clone();
    let snapshot = LocalSnapshot { controllers: vec![shown], ..LocalSnapshot::default() };
    app.apply(vec![Update::Local { request: text("fixture"), snapshot }]);
    app.local.select(&url);
    app.local.reveal_cluster();
}
