use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use jiff::civil::DateTime;
use jiff::tz::Offset;
use jiff::Timestamp;
use local_studio_core::{Connection, DaemonLaunch, Studio, StudioConfig, Update, UsagePeriod, UsageReport, Waker};
use serde_json::{json, Map};

const LIMIT: Duration = Duration::from_secs(60);
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;

struct Nudge;

impl Waker for Nudge {
    fn wake(&self) {}
}

struct Zone {
    name: String,
    offset: i64,
    changed: Option<i64>,
}

impl Zone {
    fn far(now: i64) -> Self {
        let (name, hours) = if now.rem_euclid(DAY_MS) < 21 * HOUR_MS / 2 {
            ("Pacific/Pago_Pago", -11)
        } else {
            ("Pacific/Kiritimati", 14)
        };
        Self { name: name.to_string(), offset: hours * HOUR_MS, changed: None }
    }

    fn changed(now: i64) -> anyhow::Result<Self> {
        let changed = (now.div_euclid(HOUR_MS) - 3) * HOUR_MS;
        let name = format!("LSS0LSD-1,{},{}", rule(changed - 100 * DAY_MS)?, rule(changed + HOUR_MS)?);
        Ok(Self { name, offset: 0, changed: Some(changed) })
    }

    fn instant(&self, local: i64) -> i64 {
        match self.changed {
            Some(changed) if local - HOUR_MS < changed => local - HOUR_MS,
            _ => local - self.offset,
        }
    }
}

fn civil(ms: i64) -> anyhow::Result<DateTime> {
    Ok(Offset::UTC.to_datetime(Timestamp::from_millisecond(ms)?))
}

fn rule(ms: i64) -> anyhow::Result<String> {
    let at = civil(ms)?;
    Ok(format!("{}/{}", at.date().day_of_year() - 1, at.hour()))
}

fn now_ms() -> anyhow::Result<i64> {
    Ok(i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?)
}

fn reports(config: StudioConfig) -> anyhow::Result<(UsageReport, UsageReport)> {
    let studio = Studio::new(config, Arc::new(Nudge)).map_err(|error| anyhow::anyhow!("{error}"))?;
    let begun = Instant::now();
    let mut week = None;
    let found = loop {
        if begun.elapsed() > LIMIT {
            break Err(anyhow::anyhow!("no usage report within {LIMIT:?}"));
        }
        let mut day = None;
        for update in studio.tick() {
            match update {
                Update::Connection { state: Connection::Ready } if week.is_none() => {
                    studio.load_usage("usage".to_string(), UsagePeriod::Week);
                }
                Update::Usage { report, .. } if report.period == UsagePeriod::Week => {
                    week = Some(report);
                    studio.load_usage("usage".to_string(), UsagePeriod::Day);
                }
                Update::Usage { report, .. } => day = Some(report),
                _ => {}
            }
        }
        if let (Some(day), Some(week)) = (day, &week) {
            break Ok((week.clone(), day));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    studio.shutdown();
    found
}

fn tokens(starts: &[i64], calls: &[(i64, u64)]) -> Vec<(i64, u64)> {
    let end = |index: usize| starts.get(index + 1).copied().unwrap_or(i64::MAX);
    starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let held = calls.iter().filter(|(at, _)| (*start..end(index)).contains(at));
            (*start, held.map(|(_, output)| output).sum())
        })
        .collect()
}

fn check(zone: &str, want: &[(i64, u64)], report: &UsageReport) -> anyhow::Result<()> {
    let found: Vec<_> = report.series.iter().map(|bucket| (bucket.start_ms, bucket.tokens)).collect();
    ensure!(found == want, "{zone}: {:?} holds {found:?}, want {want:?}", report.period);
    Ok(())
}

async fn stop(socket: &Path) -> anyhow::Result<()> {
    let (client, _events) = DaemonClient::connect(socket).await?;
    client
        .request_ok(DaemonCommand::Shutdown { id: None, force: Some(true), if_idle: None, rest: Map::default() })
        .await?;
    Ok(())
}

fn run(eukhe: &Path, root: &Path, case: &str) -> anyhow::Result<()> {
    std::fs::create_dir(root).with_context(|| format!("create {} (it must not exist)", root.display()))?;
    let now = now_ms()?;
    let zone = match case {
        "far" => Zone::far(now),
        _ => Zone::changed(now)?,
    };
    let name = &zone.name;
    std::env::set_var("TZ", name);
    std::env::set_var("HOME", root.join("home"));
    std::env::set_var("EUKHE_CODING_AGENT_DIR", root.join("agent"));
    std::env::remove_var("LOCAL_STUDIO_DATA_DIR");
    let today = (now + zone.offset).div_euclid(DAY_MS);
    let days: Vec<i64> = (today - 6..=today).map(|day| zone.instant(day * DAY_MS)).collect();
    let hour = now.div_euclid(HOUR_MS) * HOUR_MS;
    let hours: Vec<i64> = (0..24).map(|index| hour - (23 - index) * HOUR_MS).collect();
    let edges = days.iter().skip(1).chain(&hours).flat_map(|start| [start - 1000, start + 1000]);
    let calls: Vec<(i64, u64)> = std::iter::once(days[0] + 1000)
        .chain(edges)
        .filter(|at| (days[0]..=now).contains(at))
        .zip(0_u32..)
        .map(|(at, index)| (at, 1 << (index % 50)))
        .collect();
    let data = root.join("data");
    std::fs::create_dir_all(data.join("harness"))?;
    std::fs::create_dir_all(root.join("home"))?;
    std::fs::create_dir_all(root.join("agent"))?;
    std::fs::write(data.join("settings.json"), r#"{"automaticUpdates":false,"startController":false}"#)?;
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
    let found = reports(StudioConfig {
        socket_path: Some(socket.to_string_lossy().into_owned()),
        session_dir: Some(root.join("agent").join("sessions").to_string_lossy().into_owned()),
        data_dir: Some(data.to_string_lossy().into_owned()),
        controller_main: None,
        take_over_serve: false,
        launch: Some(DaemonLaunch { program: eukhe.to_string_lossy().into_owned() }),
    });
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let stopped = runtime.block_on(stop(&socket));
    let (week, day) = found?;
    ensure!(now_ms()?.div_euclid(HOUR_MS) * HOUR_MS == hour, "the hour changed during the run, run it again");
    let last = civil(today * DAY_MS)?.date().to_string();
    let first = civil((today - 6) * DAY_MS)?.date().to_string();
    let life = &week.lifetime;
    ensure!(life.last_day == last, "{name}: the last day is {}, want {last}", life.last_day);
    ensure!(life.since_day.as_deref() == Some(&*first), "{name}: the first day is {:?}, want {first}", life.since_day);
    let want = tokens(&days, &calls);
    check(name, &want, &week)?;
    let tail = life.daily.len().saturating_sub(want.len());
    ensure!(
        life.daily[tail..].iter().eq(want.iter().map(|(_, held)| held)),
        "{name}: the last days hold {:?}, want {want:?}",
        &life.daily[tail..]
    );
    check(name, &tokens(&hours, &calls), &day)?;
    stopped?;
    println!("ok   {name}: {first} to {last} at {}, {} calls in 7 days and 24 hours", civil(now)?, calls.len());
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let usage = "usage: usage_days_smoke <eukhe> <new scratch dir>";
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let (eukhe, root) = match &*args {
        [eukhe, root] => (eukhe, root),
        [eukhe, root, case] => return run(eukhe, root, &case.to_string_lossy()),
        _ => bail!(usage),
    };
    std::fs::create_dir(root).with_context(|| format!("create {} (it must not exist)", root.display()))?;
    let root = root.canonicalize()?;
    for case in ["far", "change"] {
        let status = Command::new(std::env::current_exe()?).arg(eukhe).arg(root.join(case)).arg(case).status()?;
        ensure!(status.success(), "the {case} case failed");
    }
    println!("PASS usage days smoke");
    Ok(())
}
