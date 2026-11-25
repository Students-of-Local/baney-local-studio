use std::future::Future;
use std::time::Duration;

use super::{Config, Ended, Event};
use crate::link::devices::now_ms;
use crate::tunnel::record::{Seen, AGAIN, STATE};
use crate::tunnel::{fail, Failure, Ports, Tunnel, DEFAULT_PORTS};

pub const WORKER_JS: &str = include_str!("../worker/relay.js");
pub const WORKER_CONFIG: &str = include_str!("../worker/wrangler.jsonc");
const PING: Duration = Duration::from_secs(30);
const LOOK: Duration = Duration::from_secs(2);

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
        let route = self.route()?.map(|route| route.ports);
        Ok(Ports {
            serve: serve.or(route.and_then(|ports| ports.serve)).unwrap_or(DEFAULT_PORTS.serve),
            mcp: mcp.or(route.map(|ports| ports.mcp)).unwrap_or(DEFAULT_PORTS.mcp),
        })
    }

    pub async fn relay_run(
        &self,
        serve: Option<u16>,
        mcp: Option<u16>,
        log: impl Fn(&str) + Send + Sync,
    ) -> Result<(), Failure> {
        let record = self.relay_record().transpose().ok_or_else(|| {
            fail("no_relay", format!("No relay on this machine: {} isn't there.", self.relay_path().display()))
        })?;
        let signal = stop_signal().map_err(|error| error.to_string())?;
        let stop = async {
            tokio::select! {
                () = signal => false,
                () = self.relay_gone() => true,
            }
        };
        tokio::pin!(stop);
        let ended = match record.and_then(|record| Ok((self.relay_ports(serve, mcp)?, record))) {
            Err(message) => Ok(Ended::BadConfig(message)),
            Ok((ports, record)) => {
                let name = record.url.rsplit('/').next().unwrap_or_default().to_string();
                self.relay_note("connecting", &format!("connecting to {name}"), &log);
                let on = |event| match event {
                    Event::Connected => self.relay_note("connected", &format!("connected to {name}"), &log),
                    Event::Disconnected(why) => self.relay_note("disconnected", &format!("{why}; trying again"), &log),
                };
                let config = Config { url: record.url, key: record.key, ports, ping: PING };
                tokio::select! {
                    ended = super::run(config, on) => Ok(ended),
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
}

fn ending(ended: Ended) -> (&'static str, String) {
    match ended {
        Ended::Replaced => {
            ("replaced", format!("another machine took this relay over; to take it back, run setup again: {AGAIN}"))
        }
        Ended::Unauthorized => {
            ("unauthorized", format!("the relay refused this machine's key; run setup again: {AGAIN}"))
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
