use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use local_studio_core::{Connection, DaemonLaunch, Studio, StudioConfig, Update, UsagePeriod, UsageReport, Waker};
use serde_json::{json, Map};

const LIMIT: Duration = Duration::from_secs(60);
const DAY_MS: i64 = 86_400_000;

struct Nudge;

impl Waker for Nudge {
    fn wake(&self) {}
}

fn date(arguments: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("date").args(arguments).output().context("run date")?;
    ensure!(output.status.success(), "date {arguments:?} failed");
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn report(config: StudioConfig) -> anyhow::Result<UsageReport> {
    let studio = Studio::new(config, Arc::new(Nudge)).map_err(|error| anyhow::anyhow!("{error}"))?;
    let begun = Instant::now();
    let mut asked = false;
    let found = loop {
        if begun.elapsed() > LIMIT {
            break Err(anyhow::anyhow!("no usage report within {LIMIT:?}"));
        }
        let mut usage = None;
        for update in studio.tick() {
            match update {
                Update::Connection { state: Connection::Ready } if !asked => {
                    asked = true;
                    studio.load_usage("usage".to_string(), UsagePeriod::Week);
                }
                Update::Usage { report, .. } => usage = Some(report),
                _ => {}
            }
        }
        if let Some(usage) = usage {
            break Ok(usage);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    studio.shutdown();
    found
}

fn check(report: &UsageReport, today: &str, midnight: i64) -> anyhow::Result<()> {
    let life = &report.lifetime;
    ensure!(life.last_day == today, "the last day is {}, want {today}, the date on this computer", life.last_day);
    let mut tail = life.daily.iter().rev().copied();
    let days = [tail.next(), tail.next(), tail.next()];
    ensure!(
        days == [Some(700), Some(40), Some(0)],
        "the last three days hold {days:?}, want 700, 40, 0 (newest first)"
    );
    ensure!(life.since_day.as_deref() < Some(today), "the first day is {:?}", life.since_day);
    let last = report.series.last().context("series")?;
    ensure!(last.start_ms == midnight, "today starts at {}, want {midnight}, midnight on this computer", last.start_ms);
    ensure!(last.tokens == 700, "today's bucket holds {} tokens", last.tokens);
    println!("ok   today is {today}, from midnight on this computer: {days:?}");
    Ok(())
}

async fn stop(socket: &Path) -> anyhow::Result<()> {
    let (client, _events) = DaemonClient::connect(socket).await?;
    client
        .request_ok(DaemonCommand::Shutdown { id: None, force: Some(true), if_idle: None, rest: Map::default() })
        .await?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let usage = "usage: usage_days_smoke <eukhe> <new scratch dir>";
    let mut args = std::env::args_os().skip(1);
    let daemon = PathBuf::from(args.next().context(usage)?);
    let root = PathBuf::from(args.next().context(usage)?);
    std::fs::create_dir(&root).with_context(|| format!("create {} (it must not exist)", root.display()))?;
    let root = root.canonicalize()?;
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let zone = if now.rem_euclid(DAY_MS) < DAY_MS / 2 { "Pacific/Pago_Pago" } else { "Pacific/Kiritimati" };
    std::env::set_var("TZ", zone);
    std::env::set_var("HOME", root.join("home"));
    std::env::set_var("EUKHE_CODING_AGENT_DIR", root.join("agent"));
    std::env::remove_var("LOCAL_STUDIO_DATA_DIR");
    let utc = date(&["-u", "+%F"])?;
    let today = date(&["+%F"])?;
    let mut clock = [0_i64; 3];
    for (slot, text) in clock.iter_mut().zip(date(&["+%H %M %S"])?.split(' ')) {
        *slot = text.parse()?;
    }
    let midnight = (now.div_euclid(1000) - clock[0] * 3600 - clock[1] * 60 - clock[2]) * 1000;
    ensure!(utc != today, "{zone} has the same date as UTC right now");
    println!("ok   {zone} says {today} while UTC says {utc}");

    let data = root.join("data");
    std::fs::create_dir_all(data.join("harness"))?;
    std::fs::create_dir_all(root.join("home"))?;
    std::fs::create_dir_all(root.join("agent"))?;
    std::fs::write(data.join("settings.json"), r#"{"automaticUpdates":false,"startController":false}"#)?;
    let calls = [(midnight - 60_000, 40), (midnight + 1000, 300), (now, 400)];
    let lines: Vec<String> = calls
        .iter()
        .map(|(at, output)| {
            json!({ "at": at, "provider": "codex", "model": "m", "thread": "t", "input": 0, "cacheRead": 0,
                "cacheWrite": 0, "output": output, "cost": 0.0 })
            .to_string()
        })
        .collect();
    std::fs::write(data.join("harness").join("usage.jsonl"), lines.join("\n") + "\n")?;
    let socket = root.join("d.sock");
    let found = report(StudioConfig {
        socket_path: Some(socket.to_string_lossy().into_owned()),
        session_dir: Some(root.join("agent").join("sessions").to_string_lossy().into_owned()),
        data_dir: Some(data.to_string_lossy().into_owned()),
        controller_main: None,
        take_over_serve: false,
        launch: Some(DaemonLaunch { program: daemon.to_string_lossy().into_owned() }),
    });
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let stopped = runtime.block_on(stop(&socket));
    if let Err(error) = check(&found?, &today, midnight) {
        bail!("{error:#}");
    }
    stopped?;
    println!("PASS usage days smoke");
    Ok(())
}
