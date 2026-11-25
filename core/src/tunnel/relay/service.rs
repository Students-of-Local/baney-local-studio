use std::future::Future;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{address, Config, Ended, Event};
use crate::link::devices::now_ms;
use crate::tunnel::record::{Seen, AGAIN, STATE};
use crate::tunnel::{fail, Failure, Ports, Tunnel, UpLock, DEFAULT_PORTS};

pub const WORKER_JS: &str = include_str!("../worker/relay.js");
pub const WORKER_CONFIG: &str = include_str!("../worker/wrangler.jsonc");
const PING: Duration = Duration::from_secs(30);
const LOOK: Duration = Duration::from_secs(2);
const REFUSED_WAIT: Duration = Duration::from_mins(1);
const LOCK: &str = "relay.lock";
const BUSY: &str = "Another local-studio tunnel relay run is already running on this data folder.";

impl Tunnel {
    fn relay_note(&self, state: &str, line: &str, log: &impl Fn(&str)) {
        let now = Seen { state: state.to_string(), at: now_ms(), pid: std::process::id() };
        let wrote = serde_json::to_vec(&now)
            .map_err(|error| error.to_string())
            .and_then(|bytes| self.private_write(STATE, &bytes));
        if let Err(error) = wrote {
            log(&format!("could not write the relay's state: {error}"));
        }
        log(line);
    }

    pub(in crate::tunnel) fn relay_ports(&self, serve: Option<u16>, mcp: Option<u16>) -> Result<Ports, String> {
        let given = serve.is_some() && mcp.is_some();
        let route = if given { None } else { self.route()?.map(|route| route.ports) };
        Ok(Ports {
            serve: serve.or(route.and_then(|ports| ports.serve)).unwrap_or(DEFAULT_PORTS.serve),
            mcp: mcp.or(route.map(|ports| ports.mcp)).unwrap_or(DEFAULT_PORTS.mcp),
        })
    }

    fn relay_config(&self, serve: Option<u16>, mcp: Option<u16>) -> Result<(Config, String), String> {
        let record = self.relay_record()?.ok_or_else(|| self.no_relay())?;
        let ports = self.relay_ports(serve, mcp)?;
        let config = Config { url: record.url, key: record.key, ports, ping: PING };
        let address = address(&config)?;
        let host = address.split('/').nth(2).unwrap_or_default().to_string();
        Ok((config, host))
    }

    fn no_relay(&self) -> String {
        format!(
            "No relay on this machine: {} isn't there. To set one up, run setup: {AGAIN}",
            self.relay_path().display()
        )
    }

    pub async fn relay_run(
        &self,
        serve: Option<u16>,
        mcp: Option<u16>,
        log: impl Fn(&str) + Send + Sync,
    ) -> Result<(), Failure> {
        let signal = stop_signal().map_err(|error| error.to_string())?;
        if matches!(self.relay_path().try_exists(), Ok(false)) {
            if std::io::stdin().is_terminal() {
                return Err(fail("no_relay", self.no_relay()));
            }
            log(&self.no_relay());
            signal.await;
            log("stopped");
            return Ok(());
        }
        let _lock = UpLock::named(&self.dir, LOCK, Some(BUSY))?;
        let stop = async {
            tokio::select! {
                () = signal => false,
                () = self.relay_gone() => true,
            }
        };
        tokio::pin!(stop);
        let ended = match self.relay_config(serve, mcp) {
            Err(message) => Ok(Ended::BadConfig(message)),
            Ok((config, host)) => {
                self.relay_note("connecting", &format!("connecting to {host}"), &log);
                tokio::select! {
                    ended = self.relay_serve(&config, &host, &log) => Ok(ended),
                    gone = &mut stop => Err(gone),
                }
            }
        };
        let gone = match ended {
            Ok(ended) => {
                let (state, line) = ending(ended);
                self.relay_note(state, &line, &log);
                stop.await
            }
            Err(gone) => gone,
        };
        if gone {
            log("the relay was turned off on this machine; stopped");
        } else {
            self.relay_note("stopped", "stopped", &log);
        }
        Ok(())
    }

    async fn relay_gone(&self) {
        while !matches!(self.relay_path().try_exists(), Ok(false)) {
            tokio::time::sleep(LOOK).await;
        }
    }

    async fn relay_serve(&self, config: &Config, host: &str, log: &(impl Fn(&str) + Send + Sync)) -> Ended {
        let refused = AtomicBool::new(false);
        let on = |event| {
            refused.store(false, Ordering::Relaxed);
            match event {
                Event::Connected => self.relay_note("connected", &format!("connected to {host}"), log),
                Event::Disconnected(why) => self.relay_note("disconnected", &format!("{why}; trying again"), log),
            }
        };
        loop {
            match super::run(config.clone(), &on).await {
                Ended::Unauthorized if refused.swap(true, Ordering::Relaxed) => {}
                Ended::Unauthorized => {
                    let (state, line) = ending(Ended::Unauthorized);
                    self.relay_note(state, &line, log);
                }
                ended => return ended,
            }
            tokio::time::sleep(refused_wait()).await;
        }
    }
}

fn refused_wait() -> Duration {
    let set = std::env::var("LS_RELAY_RETRY_MS").ok().and_then(|ms| ms.parse().ok());
    set.map_or(REFUSED_WAIT, Duration::from_millis)
}

fn ending(ended: Ended) -> (&'static str, String) {
    match ended {
        Ended::Replaced => {
            ("replaced", format!("another machine took this relay over; to take it back, run setup again: {AGAIN}"))
        }
        Ended::Unauthorized => {
            ("unauthorized", format!("the relay refused this machine's key; asking again every minute. If that doesn't end, run setup again: {AGAIN}"))
        }
        Ended::Outdated => (
            "outdated",
            format!("the relay speaks a newer protocol; update Local Studio, then run setup again: {AGAIN}"),
        ),
        Ended::BadConfig(why) => ("bad_config", why),
    }
}

#[cfg(unix)]
pub(in crate::tunnel) fn stop_signal() -> std::io::Result<impl Future<Output = ()>> {
    use tokio::signal::unix::{signal, SignalKind};
    let (mut term, mut int) = (signal(SignalKind::terminate())?, signal(SignalKind::interrupt())?);
    let mut hup = signal(SignalKind::hangup())?;
    Ok(async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
            _ = hup.recv() => {}
        }
    })
}

#[cfg(not(unix))]
pub(in crate::tunnel) fn stop_signal() -> std::io::Result<impl Future<Output = ()>> {
    let mut stop = tokio::signal::windows::ctrl_c()?;
    Ok(async move {
        stop.recv().await;
    })
}
