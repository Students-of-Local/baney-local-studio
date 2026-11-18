use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

pub const HEADER_WITHIN: Duration = Duration::from_secs(5);
pub const HANDSHAKE_WITHIN: Duration = Duration::from_secs(10);
const OPENING_AT_ONCE: usize = 8;
const OPENING_PER_CLIENT: usize = 2;
const FAILURES: usize = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
const BAN: Duration = Duration::from_secs(600);

/// Who a socket counts against for the pending caps and the failure ban. The key comes from the
/// listener the socket arrived on, never from `Host` (docs/remote.md, Transport).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Client {
    /// A loopback client with neither `Tailscale-User-Login` nor `X-Forwarded-For`: held by the
    /// pending caps, never banned.
    Loopback,
    /// `Tailscale-User-Login`, the `X-Forwarded-For` that `tailscale serve` sets, or the tunnel's
    /// `CF-Connecting-IP`, prefixed so one listener's keys can't collide with another's.
    Key(String),
}

impl fmt::Display for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loopback => f.write_str("loopback"),
            Self::Key(key) => f.write_str(key),
        }
    }
}

#[derive(Default)]
struct Record {
    failures: VecDeque<Instant>,
    banned_until: Option<Instant>,
}

#[derive(Default)]
struct Pending {
    total: usize,
    per: HashMap<Client, usize>,
}

#[derive(Default)]
pub struct Gate {
    records: Mutex<HashMap<Client, Record>>,
    pending: Arc<Mutex<Pending>>,
}

pub struct Opening {
    pending: Arc<Mutex<Pending>>,
    client: Client,
}

impl Drop for Opening {
    fn drop(&mut self) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        pending.total = pending.total.saturating_sub(1);
        if let Some(count) = pending.per.get_mut(&self.client) {
            *count -= 1;
            if *count == 0 {
                pending.per.remove(&self.client);
            }
        }
    }
}

impl Gate {
    pub fn banned(&self, client: &Client) -> bool {
        if *client == Client::Loopback {
            return false;
        }
        let records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        records.get(client).and_then(|record| record.banned_until).is_some_and(|until| until > Instant::now())
    }

    /// Takes a pending slot, or `None` when 8 sockets are mid-handshake or this client already has 2.
    pub fn open(&self, client: &Client) -> Option<Opening> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let mine = pending.per.get(client).copied().unwrap_or_default();
        if pending.total >= OPENING_AT_ONCE || mine >= OPENING_PER_CLIENT {
            return None;
        }
        pending.total += 1;
        pending.per.insert(client.clone(), mine + 1);
        Some(Opening { pending: Arc::clone(&self.pending), client: client.clone() })
    }

    pub fn fail(&self, client: &Client, pattern: &str, reason: &str) {
        eprintln!("local-studio-serve: refused {client} ({pattern}): {reason}");
        if *client == Client::Loopback {
            return;
        }
        let now = Instant::now();
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        records.retain(|_, record| {
            record.failures.retain(|at| now.duration_since(*at) < FAILURE_WINDOW);
            !record.failures.is_empty() || record.banned_until.is_some_and(|until| until > now)
        });
        let record = records.entry(client.clone()).or_default();
        record.failures.push_back(now);
        if record.failures.len() >= FAILURES {
            record.failures.clear();
            record.banned_until = Some(now + BAN);
            eprintln!(
                "local-studio-serve: {client} failed {FAILURES} times within a minute and is refused for {} minutes",
                BAN.as_secs() / 60
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> Client {
        Client::Key(name.to_string())
    }

    #[test]
    fn two_pending_per_client_and_eight_in_all() {
        let gate = Gate::default();
        let a = [gate.open(&key("a")), gate.open(&key("a"))];
        assert!(a.iter().all(Option::is_some));
        assert!(gate.open(&key("a")).is_none(), "a third pending socket from one client");
        let loopback = [gate.open(&Client::Loopback), gate.open(&Client::Loopback)];
        assert!(loopback.iter().all(Option::is_some));
        assert!(gate.open(&Client::Loopback).is_none(), "loopback is held by the per client cap too");
        let others: Vec<_> =
            ["b", "c"].iter().flat_map(|name| [gate.open(&key(name)), gate.open(&key(name))]).collect();
        assert!(others.iter().all(Option::is_some));
        assert!(gate.open(&key("d")).is_none(), "a ninth pending socket");
        drop(others);
        assert!(gate.open(&key("d")).is_some(), "slots come back when sockets finish");
    }

    #[test]
    fn loopback_is_never_banned_and_keys_are_separate() {
        let gate = Gate::default();
        for _ in 0..FAILURES {
            gate.fail(&Client::Loopback, "KK", "test");
            gate.fail(&key("ts:a@example.com"), "KK", "test");
        }
        assert!(!gate.banned(&Client::Loopback));
        assert!(gate.banned(&key("ts:a@example.com")));
        assert!(!gate.banned(&key("ts:b@example.com")));
    }
}
