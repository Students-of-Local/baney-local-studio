use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

pub const HEADER_WITHIN: Duration = Duration::from_secs(5);
pub const HANDSHAKE_WITHIN: Duration = Duration::from_secs(10);
const OPENING_AT_ONCE: usize = 8;
const FAILURES: usize = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
const BAN: Duration = Duration::from_secs(600);

#[derive(Default)]
struct Record {
    failures: VecDeque<Instant>,
    banned_until: Option<Instant>,
}

#[derive(Default)]
pub struct Gate {
    records: Mutex<HashMap<IpAddr, Record>>,
    opening: Arc<AtomicUsize>,
}

pub struct Opening(Arc<AtomicUsize>);

impl Drop for Opening {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Gate {
    pub fn banned(&self, address: IpAddr) -> bool {
        let records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        records.get(&address).and_then(|record| record.banned_until).is_some_and(|until| until > Instant::now())
    }

    pub fn open(&self) -> Option<Opening> {
        let taken = self.opening.fetch_add(1, Ordering::SeqCst);
        let opening = Opening(Arc::clone(&self.opening));
        (taken < OPENING_AT_ONCE).then_some(opening)
    }

    pub fn fail(&self, address: IpAddr, pattern: &str, reason: &str) {
        eprintln!("local-studio-serve: refused {address} ({pattern}): {reason}");
        let now = Instant::now();
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        records.retain(|_, record| {
            record.failures.retain(|at| now.duration_since(*at) < FAILURE_WINDOW);
            !record.failures.is_empty() || record.banned_until.is_some_and(|until| until > now)
        });
        let record = records.entry(address).or_default();
        record.failures.push_back(now);
        if record.failures.len() >= FAILURES {
            record.failures.clear();
            record.banned_until = Some(now + BAN);
            eprintln!(
                "local-studio-serve: {address} failed {FAILURES} times within a minute and is refused for {} minutes",
                BAN.as_secs() / 60
            );
        }
    }
}
