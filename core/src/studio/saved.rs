use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::threads::BROWSER_PREFIX;

const POLL: Duration = Duration::from_secs(3);

type Seen = BTreeMap<OsString, Option<(SystemTime, u64)>>;

fn browser_chats(dir: &Path) -> Seen {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().as_encoded_bytes().starts_with(BROWSER_PREFIX.as_bytes()))
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            let stamp = meta.is_file().then(|| Some((meta.modified().ok()?, meta.len()))).flatten();
            Some((entry.file_name(), stamp))
        })
        .collect()
}

pub(super) async fn watch(sessions: PathBuf, changed: impl Fn()) {
    let mut every = tokio::time::interval(POLL);
    let mut seen = None;
    loop {
        every.tick().await;
        let dir = sessions.clone();
        let Ok(now) = tokio::task::spawn_blocking(move || browser_chats(&dir)).await else { continue };
        if seen.replace(now.clone()).is_some_and(|before| before != now) {
            changed();
        }
    }
}
