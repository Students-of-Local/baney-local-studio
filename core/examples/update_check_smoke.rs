use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use local_studio_core::update::Build;
use local_studio_core::{AppUpdateState, Settings, Studio, StudioConfig, Update, Waker};

const FEED_VERSION: &str = "0.1.0-nightly.20261124000000+0000000";
const OFFERED_VERSION: &str = "0.1.0-nightly.20261125000000+0000000";
const FEED_REQUEST: &str = "GET /latest.json ";
const QUIET: Duration = Duration::from_secs(5);
const CHECK_LIMIT: Duration = Duration::from_secs(15);

struct Nudge;

impl Waker for Nudge {
    fn wake(&self) {}
}

struct Feed {
    url: String,
    hits: Arc<AtomicUsize>,
    downloads: Arc<AtomicUsize>,
    offers: Arc<AtomicBool>,
}

fn main() -> ExitCode {
    if !cfg!(feature = "test-hooks") {
        eprintln!("update_check_smoke needs --features test-hooks");
        return ExitCode::from(2);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [scratch] = args.as_slice() else {
        eprintln!("usage: update_check_smoke <new scratch dir>");
        return ExitCode::from(2);
    };
    match run(Path::new(scratch)) {
        Ok(()) => {
            println!("PASS update_check_smoke");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("FAIL update_check_smoke: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(scratch: &Path) -> Result<()> {
    std::fs::create_dir(scratch).with_context(|| format!("create {} (it must not exist)", scratch.display()))?;
    let scratch = scratch.canonicalize()?;
    let home = scratch.join("home");
    std::fs::create_dir_all(&home)?;
    std::env::set_var("HOME", &home);
    std::env::set_var("EUKHE_CODING_AGENT_DIR", scratch.join("agent"));
    for name in ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "LOCAL_STUDIO_DATA_DIR", "LOCAL_STUDIO_NO_UPDATE_CHECK"] {
        std::env::remove_var(name);
    }
    let feed = serve()?;
    std::env::set_var("LOCAL_STUDIO_TEST_UPDATE_FEED", &feed.url);

    automatic_on(&feed, &scratch.join("on"))?;
    quiet(&feed, &scratch.join("off"), false, "the setting is off")?;
    std::env::set_var("LOCAL_STUDIO_NO_UPDATE_CHECK", "1");
    quiet(&feed, &scratch.join("env"), true, "LOCAL_STUDIO_NO_UPDATE_CHECK=1")?;
    std::env::remove_var("LOCAL_STUDIO_NO_UPDATE_CHECK");
    turned_on_later(&feed, &scratch.join("later"))?;
    feed.offers.store(true, Ordering::SeqCst);
    turned_on_with_an_offer(&feed, &scratch.join("offer"))
}

fn serve() -> Result<Feed> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let base = format!("http://{}", listener.local_addr()?);
    let build = Build::current();
    let asset = build.asset();
    let download = format!("GET /{asset} ");
    let offer = serde_json::json!({
        "product": "local-studio",
        "channel": "nightly",
        "version": OFFERED_VERSION,
        "platforms": { (build.platform): { "url": format!("{base}/{asset}"), "sha256": "0".repeat(64), "signature": "none" } },
    })
    .to_string();
    let current = format!(r#"{{"product":"local-studio","channel":"nightly","version":"{FEED_VERSION}"}}"#);
    let feed = Feed {
        url: format!("{base}/latest.json"),
        hits: Arc::default(),
        downloads: Arc::default(),
        offers: Arc::default(),
    };
    let (hits, downloads, offers) = (Arc::clone(&feed.hits), Arc::clone(&feed.downloads), Arc::clone(&feed.offers));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut lines = BufReader::new(&stream).lines();
            let request = lines.next().and_then(Result::ok).unwrap_or_default();
            while lines.next().and_then(Result::ok).is_some_and(|line| !line.is_empty()) {}
            let body = if request.starts_with(FEED_REQUEST) {
                hits.fetch_add(1, Ordering::SeqCst);
                if offers.load(Ordering::SeqCst) {
                    offer.as_str()
                } else {
                    current.as_str()
                }
            } else if request.starts_with(&download) {
                downloads.fetch_add(1, Ordering::SeqCst);
                "not a build"
            } else {
                continue;
            };
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            if let Err(error) = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(body.as_bytes())) {
                eprintln!("feed reply: {error}");
            }
        }
    });
    Ok(feed)
}

struct App {
    studio: Arc<Studio>,
    last: Option<AppUpdateState>,
}

impl App {
    fn open(dir: &Path, automatic: bool) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let data = dir.join("data");
        std::fs::create_dir_all(&data)?;
        std::fs::write(data.join("settings.json"), format!(r#"{{"automaticUpdates":{automatic}}}"#))?;
        let config = StudioConfig {
            socket_path: Some(dir.join("d.sock").to_string_lossy().into_owned()),
            session_dir: None,
            data_dir: Some(data.to_string_lossy().into_owned()),
            launch: None,
            controller_main: None,
            take_over_serve: false,
        };
        let studio = Studio::new(config, Arc::new(Nudge)).map_err(|error| anyhow::anyhow!("{error}"))?;
        Ok(Self { studio, last: None })
    }

    fn pump(&mut self) {
        for update in self.studio.tick() {
            if let Update::AppUpdate { state } = update {
                self.last = Some(state);
            }
        }
    }

    fn settle(&mut self, what: &str, limit: Duration, done: impl Fn(&AppUpdateState) -> bool) -> Result<()> {
        let started = Instant::now();
        loop {
            self.pump();
            if self.last.as_ref().is_some_and(&done) {
                return Ok(());
            }
            if started.elapsed() > limit {
                bail!("timed out waiting for {what}; last state {:?}", self.last);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn watch_quiet(&mut self, hits: &AtomicUsize, before: usize) -> Result<()> {
        let started = Instant::now();
        while started.elapsed() < QUIET {
            self.pump();
            ensure!(
                hits.load(Ordering::SeqCst) == before,
                "the feed was fetched in the background ({} hits)",
                hits.load(Ordering::SeqCst) - before
            );
            ensure!(
                !matches!(self.last, Some(AppUpdateState::Checking | AppUpdateState::UpToDate { .. })),
                "the state reached {:?} without a check being asked for",
                self.last
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }
}

fn up_to_date(state: &AppUpdateState) -> bool {
    matches!(state, AppUpdateState::UpToDate { .. })
}

fn hits(feed: &Feed) -> usize {
    feed.hits.load(Ordering::SeqCst)
}

fn automatic_on(feed: &Feed, dir: &Path) -> Result<()> {
    let mut app = App::open(dir, true)?;
    let before = hits(feed);
    app.studio.start_update_checks();
    app.settle("the background check", CHECK_LIMIT, up_to_date)?;
    ensure!(hits(feed) == before + 1, "expected one feed fetch, saw {}", hits(feed) - before);
    app.studio.shutdown();
    println!("ok: Automatic updates on checks in the background");
    Ok(())
}

fn quiet(feed: &Feed, dir: &Path, automatic: bool, why: &str) -> Result<()> {
    let mut app = App::open(dir, automatic)?;
    let before = hits(feed);
    app.studio.start_update_checks();
    app.watch_quiet(&feed.hits, before).with_context(|| why.to_string())?;
    app.studio.check_for_update();
    app.settle("a manual check", CHECK_LIMIT, up_to_date).with_context(|| why.to_string())?;
    ensure!(hits(feed) == before + 1, "the manual check fetched the feed {} times", hits(feed) - before);
    app.studio.shutdown();
    println!("ok: no background check when {why}; Check Now still checks");
    Ok(())
}

fn turned_on_with_an_offer(feed: &Feed, dir: &Path) -> Result<()> {
    let mut app = App::open(dir, false)?;
    app.studio.start_update_checks();
    app.watch_quiet(&feed.hits, hits(feed))?;
    app.studio.check_for_update();
    app.settle("the offer", CHECK_LIMIT, |state| matches!(state, AppUpdateState::Available { .. }))?;
    let before = hits(feed);
    app.studio.update_settings(Settings { automatic_updates: true, ..Settings::default() });
    app.settle("the refused download", CHECK_LIMIT, |state| matches!(state, AppUpdateState::Failed { .. }))?;
    std::thread::sleep(QUIET);
    app.pump();
    ensure!(
        hits(feed) == before,
        "turning the setting on with an offer fetched the feed {} more times",
        hits(feed) - before
    );
    let downloads = feed.downloads.load(Ordering::SeqCst);
    ensure!(downloads == 1, "expected one download, saw {downloads}");
    app.studio.shutdown();
    println!("ok: turning Automatic updates on with an offer downloads it once");
    Ok(())
}

fn turned_on_later(feed: &Feed, dir: &Path) -> Result<()> {
    let mut app = App::open(dir, false)?;
    let before = hits(feed);
    app.studio.start_update_checks();
    app.watch_quiet(&feed.hits, before)?;
    app.studio.update_settings(Settings { automatic_updates: true, ..Settings::default() });
    app.settle("the check after turning Automatic updates on", CHECK_LIMIT, up_to_date)?;
    ensure!(hits(feed) == before + 1, "expected one feed fetch, saw {}", hits(feed) - before);
    app.studio.shutdown();
    println!("ok: turning Automatic updates on starts the check");
    Ok(())
}
