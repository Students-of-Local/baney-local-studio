use std::rc::Rc;

use local_studio_core::{
    LocalCluster, LocalClusterCheck, LocalClusterMachine, LocalClusterPower, LocalController, LocalEject,
    LocalEjectProcess, LocalEjectState, LocalGpu, LocalHardware, LocalHotplug, LocalLendMode, LocalLendState,
    LocalLentServer, LocalMachineStatus, LocalPool, LocalPoolModel, LocalPoolPhase, LocalPowerHelper, LocalPowerLever,
    LocalSnapshot, LocalThunderbolt, LocalThunderboltState, Update,
};

use crate::app::App;

pub const SCENES: [&str; 15] = [
    "local-cluster-lending",
    "local-cluster-reclaimed",
    "local-cluster-eject-busy",
    "local-cluster-eject-root",
    "local-cluster-apple-silicon",
    "local-cluster-power-in-use",
    "local-cluster-power-in-use-light",
    "local-cluster-power-away",
    "local-cluster-power-away-light",
    "local-cluster-power-duty-cycle",
    "local-cluster-power-duty-cycle-light",
    "local-cluster-power-helper-missing",
    "local-cluster-power-helper-missing-light",
    "local-cluster-power-presence-unknown",
    "local-cluster-power-presence-unknown-light",
];

const GIB: u64 = 1_073_741_824;
const EGPU_LINK: &str = "PCIe 3.0 x4, about 3.9 GB/s over the cable. Loading a model takes longer than on an internal slot; once it is loaded, decoding speed is nearly the same.";
const DEVICE: &str = "d5010000-0070-6418-2317-5c0a1a2b3c4d";
const LAPTOP_RTX: &str = "NVIDIA GeForce RTX 4080 Laptop GPU";
const SCOUT: &str = "Llama-4-Scout-17B-16E-Q4_K_M";
const REMOVE: &str = "echo 1 | sudo tee /sys/bus/pci/devices/0000:0b:00.0/remove";

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
        eject: external.then(|| LocalEject {
            state: LocalEjectState::NeedsRoot,
            draining: false,
            blockers: Vec::new(),
            command: Some(text(REMOVE)),
            message: Some(text("Removing the GPU from the PCI bus needs an administrator password.")),
        }),
    }
}

fn ejecting(state: LocalEjectState) -> LocalController {
    let mut shown = parish(false);
    let eject = if state == LocalEjectState::Busy {
        LocalEject {
            state,
            draining: true,
            blockers: vec![LocalEjectProcess { pid: 48_213, name: text("blender") }],
            command: None,
            message: Some(text("A program outside Local Studio is using NVIDIA GeForce RTX 5090. Quit it first.")),
        }
    } else {
        LocalEject {
            state,
            draining: true,
            blockers: Vec::new(),
            command: Some(text(REMOVE)),
            message: Some(text("Removing the GPU from the PCI bus needs an administrator password.")),
        }
    };
    if let Some(gpu) = shown.hardware.as_mut().and_then(|hardware| hardware.gpus.iter_mut().find(|gpu| gpu.external)) {
        gpu.eject = Some(eject);
    }
    shown
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
            power: LocalClusterPower::default(),
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
            power: LocalClusterPower {
                active_watts: 60,
                present: Some(false),
                presence_source: text("logind session 2 (wayland)"),
                helper: LocalPowerHelper::Installed,
                max_w: Some(495.0),
                ..LocalClusterPower::default()
            },
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
        power: LocalClusterPower::default(),
    };
    controller("gil-mbp", "http://127.0.0.1:18091", true, hardware, cluster)
}

fn powered(scene: &str) -> LocalController {
    let mut shown = if scene.starts_with("local-cluster-power-duty-cycle") { apple() } else { parish(false) };
    let Some(cluster) = shown.cluster.as_mut() else { return shown };
    let power = &mut cluster.power;
    if scene.starts_with("local-cluster-power-in-use") {
        cluster.reason = text("In use, so it lends at up to 60 W.");
        power.present = Some(true);
        power.cap_w = Some(60.0);
        power.lever = LocalPowerLever::DutyCycle;
        power.measured_w = Some(48.0);
    } else if scene.starts_with("local-cluster-power-away") {
        cluster.reason = text("Idle, so its GPU is lent to the fleet, at up to 150 W.");
        power.away_watts = Some(150);
        power.cap_w = Some(150.0);
        power.lever = LocalPowerLever::NvidiaLimit;
        power.measured_w = Some(141.0);
    } else if scene.starts_with("local-cluster-power-duty-cycle") {
        cluster.lend = LocalLendMode::Idle;
        cluster.state = LocalLendState::Lent;
        cluster.reason = text("In use, so it lends at up to 20 W. It can't read this machine's power draw (macmon is not installed), so it runs fleet work 66% of the time instead.");
        *power = LocalClusterPower {
            active_watts: 20,
            present: Some(true),
            presence_source: text("macOS input idle time"),
            cap_w: Some(20.0),
            lever: LocalPowerLever::DutyCycle,
            ..LocalClusterPower::default()
        };
    } else if scene.starts_with("local-cluster-power-presence-unknown") {
        cluster.reason = text("Idle, so its GPU is lent to the fleet, at up to 200 W.");
        power.present = None;
        power.presence_source =
            text("logind session 2 (wayland): the desktop does not report idle time (IdleHint is never set)");
        power.away_watts = Some(200);
        power.cap_w = Some(200.0);
        power.lever = LocalPowerLever::DutyCycle;
        power.measured_w = Some(143.0);
    } else {
        cluster.reason = text("In use, so it lends at up to 60 W.");
        power.present = Some(true);
        power.away_watts = Some(200);
        power.cap_w = Some(60.0);
        power.lever = LocalPowerLever::DutyCycle;
        power.measured_w = Some(57.0);
        power.helper = LocalPowerHelper::Missing;
    }
    shown
}

pub fn stage(app: &Rc<App>, scene: &str) {
    let shown = match scene {
        "local-cluster-reclaimed" => parish(true),
        "local-cluster-eject-busy" => ejecting(LocalEjectState::Busy),
        "local-cluster-eject-root" => ejecting(LocalEjectState::NeedsRoot),
        "local-cluster-apple-silicon" => apple(),
        _ if scene.starts_with("local-cluster-power-") => powered(scene),
        _ => parish(false),
    };
    let url = shown.url.clone();
    let snapshot = LocalSnapshot { controllers: vec![shown], ..LocalSnapshot::default() };
    app.apply(vec![Update::Local { request: text("fixture"), snapshot }]);
    app.local.select(&url);
    app.local.reveal_cluster();
}
