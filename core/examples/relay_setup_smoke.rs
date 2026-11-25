use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use local_studio_core::tunnel::{SERVICE_LABEL, SERVICE_UNIT};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role};
use tokio_tungstenite::tungstenite::{Message, WebSocket};

#[path = "support/relay_setup_cases.rs"]
mod cases;

const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";
const RUN_KEY: &str = "RunKeyRunKeyRunKeyRunKeyRunKeyRunKeyRunKey_";
const QUICK: Duration = Duration::from_secs(10);
const NO_REDIAL: Duration = Duration::from_secs(3);
const RETRY_MS: &str = "5000";

type Link = WebSocket<TcpStream>;

struct Smoke {
    bin: PathBuf,
    root: PathBuf,
    home: PathBuf,
    data: PathBuf,
}

struct Service {
    child: Child,
    log: PathBuf,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => {
            println!("relay_setup_smoke: every step passed");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("relay_setup_smoke: FAILED: {error}");
            ExitCode::FAILURE
        }
    }
}

fn ensure(ok: bool, what: impl Into<String>) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(what.into())
    }
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).map_or(0, |meta| meta.permissions().mode() & 0o777)
}

fn chmod(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn until<T>(what: &str, within: Duration, mut find: impl FnMut() -> Option<T>) -> Result<T, String> {
    let since = Instant::now();
    loop {
        if let Some(found) = find() {
            return Ok(found);
        }
        ensure(since.elapsed() < within, format!("never saw {what}"))?;
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn head(stream: &mut TcpStream) -> Result<String, String> {
    stream.set_nonblocking(false).map_err(text)?;
    stream.set_read_timeout(Some(QUICK)).map_err(text)?;
    let mut got = Vec::new();
    let mut byte = [0u8; 1];
    while !got.ends_with(b"\r\n\r\n") {
        ensure(stream.read(&mut byte).map_err(text)? == 1, "the connection ended before its head")?;
        got.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&got).to_string())
}

fn header<'a>(head: &'a str, name: &str) -> &'a str {
    let found = head.lines().filter_map(|line| line.split_once(':')).find(|(key, _)| key.eq_ignore_ascii_case(name));
    found.map_or("", |(_, value)| value.trim())
}

fn origin(status: u16) -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(text)?;
    let port = listener.local_addr().map_err(text)?.port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let reply = format!("HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = head(&mut stream).map(|_| stream.write_all(reply.as_bytes()));
        }
    });
    Ok(port)
}

struct StandIn {
    listener: TcpListener,
    url: String,
}

impl StandIn {
    fn start() -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(text)?;
        listener.set_nonblocking(true).map_err(text)?;
        let url = format!("http://{}", listener.local_addr().map_err(text)?);
        Ok(Self { listener, url })
    }

    fn next(&self) -> Option<(TcpStream, String)> {
        loop {
            let (mut stream, _) = self.listener.accept().ok()?;
            if let Some(head) = head(&mut stream).ok().filter(|head| head.starts_with("GET /_relay/connect ")) {
                return Some((stream, head));
            }
        }
    }

    fn dial(&self, key: &str) -> Result<(TcpStream, String), String> {
        let (stream, head) = until("a dial of /_relay/connect", QUICK, || self.next())?;
        ensure(header(&head, "authorization") == format!("Bearer {key}"), "the connector sends the key as bearer")?;
        Ok((stream, derive_accept_key(header(&head, "sec-websocket-key").as_bytes())))
    }

    fn open(&self, key: &str, version: u64) -> Result<Link, String> {
        let (mut stream, accept) = self.dial(key)?;
        let reply = format!(
            "HTTP/1.1 101 X\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        stream.write_all(reply.as_bytes()).map_err(text)?;
        let mut link = WebSocket::from_raw_socket(stream, Role::Server, None);
        let hello = json!({"v":version,"t":"hello","maxBody":10 << 20,"maxInFlight":64,"headTimeoutMs":60_000});
        link.send(Message::text(hello.to_string())).map_err(text)?;
        Ok(link)
    }

    fn refuse(&self, key: &str) -> Result<(), String> {
        let (mut stream, _) = self.dial(key)?;
        stream.write_all(b"HTTP/1.1 401 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").map_err(text)
    }

    fn no_dial(&self) -> Result<(), String> {
        std::thread::sleep(NO_REDIAL);
        ensure(self.next().is_none(), "an ended connector dialed again")
    }
}

fn answered(link: &mut Link) -> Result<u64, String> {
    let request = json!({"v":1,"t":"req","id":7,"method":"GET","path":"/mcp","body":false,"ws":false,"headers":[["host","relay.example.com"]]});
    link.send(Message::text(request.to_string())).map_err(text)?;
    loop {
        let Message::Text(got) = link.read().map_err(text)? else { continue };
        let frame: Value = serde_json::from_str(&got).map_err(text)?;
        if frame["t"] == "res" && frame["id"] == 7 {
            return frame["status"].as_u64().ok_or_else(|| format!("no status: {frame}"));
        }
    }
}

impl Smoke {
    fn tunnel(&self, name: &str) -> PathBuf {
        self.data.join("tunnel").join(name)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.bin);
        command
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TMPDIR", self.root.join("tmp"))
            .env("HOME", &self.home)
            .env("LOCAL_STUDIO_DATA_DIR", &self.data)
            .stdin(Stdio::null());
        command
    }

    fn service(&self, name: &str, args: &[&str]) -> Result<Service, String> {
        let log = self.root.join(format!("{name}.log"));
        let stderr = fs::File::create(&log).map_err(text)?;
        let stdout = stderr.try_clone().map_err(text)?;
        let mut all = vec!["tunnel", "relay", "run"];
        all.extend(args);
        let mut command = self.command(&all);
        command.env("LS_RELAY_RETRY_MS", RETRY_MS);
        let child = command.stdout(stdout).stderr(stderr).spawn().map_err(text)?;
        Ok(Service { child, log })
    }

    fn record(&self, url: &str) -> Result<(), String> {
        let record = json!({"v":1,"url":url,"name":"smoke","accountId":ACCOUNT,"key":RUN_KEY});
        self.write("relay.json", &record.to_string())
    }

    fn write(&self, name: &str, contents: &str) -> Result<(), String> {
        let path = self.tunnel(name);
        fs::create_dir_all(self.data.join("tunnel")).map_err(text)?;
        fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
        chmod(&path, 0o600)
    }

    fn state(&self, service: &Service, want: &str) -> Result<(), String> {
        let path = self.tunnel("relay-state.json");
        let pid = service.child.id();
        let seen = || read_json(&path).ok().filter(|got| got["state"] == want && got["pid"] == pid);
        let got = until(&format!("the state {want}"), QUICK, seen)
            .map_err(|e| format!("{e}; the log says {}", fs::read_to_string(&service.log).unwrap_or_default()))?;
        ensure(
            got.as_object().is_some_and(|fields| fields.len() == 3)
                && got["at"].as_i64().is_some_and(|at| at > 1_700_000_000_000),
            format!("the state file holds state, at and pid: {got}"),
        )?;
        ensure(mode(&path) == 0o600, "relay-state.json must be 0600")
    }

    fn stays(&self, service: &mut Service, relay: &StandIn, state: &str) -> Result<(), String> {
        self.state(service, state)?;
        relay.no_dial()?;
        ensure(service.child.try_wait().map_err(text)?.is_none(), format!("{state} stays up"))?;
        self.state(service, state)
    }

    fn stop(&self, service: &mut Service, lines: &[&str]) -> Result<(), String> {
        term(service)?;
        self.state(service, "stopped")?;
        let log = fs::read_to_string(&service.log).map_err(text)?;
        let got: Vec<&str> = log.lines().collect();
        let same = got.len() == lines.len() && got.iter().zip(lines).all(|(got, want)| got.contains(want));
        ensure(same && !log.contains(RUN_KEY), format!("one plain line per state {lines:?}, no key: {log}"))
    }
}

fn term(service: &mut Service) -> Result<(), String> {
    let pid = service.child.id().to_string();
    let sent = Command::new("kill").args(["-TERM", &pid]).status().map_err(text)?;
    ensure(sent.success(), format!("SIGTERM to {pid}: {sent}"))?;
    let status = until("the exit after SIGTERM", QUICK, || service.child.try_wait().ok().flatten())?;
    ensure(status.code() == Some(0), format!("SIGTERM exits 0, not {status}"))
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [bin, scratch] = args.as_slice() else {
        return Err("usage: relay_setup_smoke <local-studio binary> <scratch dir>".into());
    };
    let bin = fs::canonicalize(bin).map_err(|e| format!("{bin}: {e}"))?;
    let root = PathBuf::from(scratch).join(format!("relay-setup-{}", std::process::id()));
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&root).map_err(text)?;
    for dir in ["home", "run-home", "tmp"] {
        fs::create_dir(root.join(dir)).map_err(text)?;
    }
    let root = fs::canonicalize(&root).map_err(text)?;
    let smoke = Smoke { bin, home: root.join("home"), data: root.join("data"), root };
    println!("scratch: {}", smoke.root.display());
    let mut setup = cases::Cases::start(&smoke)?;
    cases::before(&mut setup)?;
    cases::live(&mut setup)?;
    cases::status(&mut setup)?;
    cases::others(&mut setup)?;
    cases::broken(&mut setup)?;
    let [data, home] = ["run-data", "run-home"].map(|name| smoke.root.join(name));
    s11_run(&Smoke { data, home, bin: smoke.bin.clone(), root: smoke.root.clone() })
}

fn s11_run(s: &Smoke) -> Result<(), String> {
    let mut service = s.service("run-none", &[])?;
    until("the wait with no relay.json", QUICK, || {
        fs::read_to_string(&service.log).ok().filter(|log| log.contains("relay.json isn't there"))
    })?;
    std::thread::sleep(Duration::from_millis(500));
    ensure(
        service.child.try_wait().map_err(text)?.is_none()
            && !s.tunnel("relay-state.json").exists()
            && !s.tunnel("relay.lock").exists(),
        "relay run as a service with no relay.json stays up, takes no lock and writes nothing",
    )?;
    term(&mut service)?;
    ensure(!s.tunnel("relay-state.json").exists(), "nor when it stops")?;
    let relay = StandIn::start()?;
    let mut service = s.service("run-late", &[])?;
    until("the wait for relay.json", QUICK, || {
        fs::read_to_string(&service.log).ok().filter(|log| log.contains("relay.json isn't there"))
    })?;
    s.record(&relay.url)?;
    let link = relay.open(RUN_KEY, 1)?;
    s.state(&service, "connected")?;
    s.stop(&mut service, &["relay.json isn't there", "connecting to", "connected to", "stopped"])?;
    drop(link);
    let (serve, mcp) = (origin(200)?, origin(418)?);
    let route = json!({"v":1,"owner":"local-studio","route":"cloudflare","hostname":"smoke.workers.dev","aliases":[],"ports":{"mcp":mcp,"serve":serve}});
    s.write("route.json", &route.to_string())?;
    s.record(&relay.url)?;

    let mut service = s.service("run-401", &[])?;
    s.state(&service, "connecting")?;
    let mut link = relay.open(RUN_KEY, 1)?;
    s.state(&service, "connected")?;
    ensure(answered(&mut link)? == 418, "relay run uses route.json's ports")?;
    drop(link);
    s.state(&service, "disconnected")?;
    relay.refuse(RUN_KEY)?;
    s.stays(&mut service, &relay, "unauthorized")?;
    let since = read_json(&s.tunnel("relay-state.json"))?;
    relay.refuse(RUN_KEY)?;
    s.stays(&mut service, &relay, "unauthorized")?;
    ensure(read_json(&s.tunnel("relay-state.json"))? == since, "a second 401 leaves the state as it was")?;
    let link = relay.open(RUN_KEY, 1)?;
    s.state(&service, "connected")?;
    let before = fs::read(s.tunnel("relay-state.json")).map_err(text)?;
    let mut second = s.service("run-second", &[])?;
    let status = until("a second relay run's exit", QUICK, || second.child.try_wait().ok().flatten())?;
    let said = fs::read_to_string(&second.log).map_err(text)?;
    ensure(
        status.code() == Some(1) && said.lines().count() == 1 && said.contains("already running"),
        format!("a second relay run prints one line and exits 1: {said}"),
    )?;
    ensure(fs::read(s.tunnel("relay-state.json")).map_err(text)? == before, "a second relay run writes no state")?;
    ensure(relay.next().is_none(), "a second relay run dials nothing")?;
    s.stop(
        &mut service,
        &["connecting to", "connected to", "trying again", "refused this machine's key", "connected to", "stopped"],
    )?;
    drop(link);

    let other = origin(204)?.to_string();
    let mut service = s.service("run-replaced", &["--mcp", &other])?;
    let mut link = relay.open(RUN_KEY, 1)?;
    s.state(&service, "connected")?;
    ensure(answered(&mut link)? == 204, "a port flag wins over route.json's")?;
    let close = CloseFrame { code: CloseCode::Library(4000), reason: "replaced".into() };
    link.close(Some(close)).map_err(text)?;
    s.stays(&mut service, &relay, "replaced")?;
    s.stop(&mut service, &["connecting to", "connected to", "took this relay over", "stopped"])?;

    let mut service = s.service("run-outdated", &[])?;
    let link = relay.open(RUN_KEY, 2)?;
    s.stays(&mut service, &relay, "outdated")?;
    drop(link);
    s.stop(&mut service, &["connecting to", "update Local Studio", "stopped"])?;

    s.write("route.json", "{")?;
    let mut service = s.service("run-flags", &["--serve", &serve.to_string(), "--mcp", &other])?;
    let mut link = relay.open(RUN_KEY, 1)?;
    s.state(&service, "connected")?;
    ensure(answered(&mut link)? == 204, "both port flags leave route.json unread")?;
    s.stop(&mut service, &["connecting to", "connected to", "stopped"])?;
    drop(link);
    s.write("route.json", &route.to_string())?;
    records(s, &relay)?;
    taken(s)?;
    println!("ok: S11 relay run: connecting, connected, disconnected, unauthorized and back, replaced, outdated, bad_config, one per folder");
    Ok(())
}

fn records(s: &Smoke, relay: &StandIn) -> Result<(), String> {
    for (name, host) in [("run-path", "relay.example.com/path"), ("run-user", "u:LEAK@h.example?t=LEAK")] {
        s.record(&format!("https://{host}"))?;
        let mut service = s.service(name, &[])?;
        s.stays(&mut service, relay, "bad_config")?;
        s.stop(&mut service, &["https://<host>", "stopped"])?;
        ensure(!fs::read_to_string(&service.log).map_err(text)?.contains("LEAK"), "no raw address in the log")?;
        let out = s.command(&["tunnel", "status", "--json"]).output().map_err(text)?;
        let said: Value = serde_json::from_slice(&out.stdout).map_err(text)?;
        ensure(
            said["relay"]["state"] == "bad_config",
            format!("a relay.json for another address is bad_config: {said}"),
        )?;
    }

    s.record(&relay.url)?;
    chmod(&s.tunnel("relay.json"), 0o640)?;
    let mut service = s.service("run-open", &[])?;
    s.stays(&mut service, relay, "bad_config")?;
    s.stop(&mut service, &["chmod 600 '", "stopped"])?;
    s.record("https://smoke.workers.dev")?;
    chmod(&s.tunnel("relay.json"), 0o640)?;
    s.write("cloudflared.yml", "")?;
    let out = s.command(&["tunnel", "status", "--json"]).output().map_err(text)?;
    let said: Value = serde_json::from_slice(&out.stdout).map_err(text)?;
    let fix = said["nextCommand"].as_str().unwrap_or_default();
    ensure(
        said["relay"]["state"] == "bad_config" && fix.starts_with("chmod 600 '"),
        format!("an old cloudflared.yml doesn't hide an open relay.json: {said}"),
    )?;
    fs::remove_file(s.tunnel("cloudflared.yml")).map_err(text)?;

    let extra = json!({"v":1,"url":relay.url,"name":"smoke","accountId":ACCOUNT,"key":RUN_KEY,"more":RUN_KEY});
    for (name, record) in [("run-extra", extra.to_string()), ("run-cut", format!("{{\"v\":1,\"key\":\"{RUN_KEY}"))] {
        s.write("relay.json", &record)?;
        let mut service = s.service(name, &[])?;
        s.stays(&mut service, relay, "bad_config")?;
        s.stop(&mut service, &["relay.json isn't a relay record", "stopped"])?;
    }
    s.record(&relay.url)?;
    let mut service = s.service("run-off", &[])?;
    let _link = relay.open(RUN_KEY, 1)?;
    s.state(&service, "connected")?;
    for name in ["relay-state.json", "relay.json"] {
        fs::remove_file(s.tunnel(name)).map_err(text)?;
    }
    let status = until("the exit after relay.json goes", QUICK, || service.child.try_wait().ok().flatten())?;
    ensure(
        status.code() == Some(0) && !s.tunnel("relay-state.json").exists(),
        "a relay turned off under a running connector ends it and leaves no state",
    )?;
    let record = json!({"v":1,"url":"https://smoke.workers.dev","name":"smoke","accountId":ACCOUNT,"key":RUN_KEY});
    s.write("relay.json", &record.to_string())?;
    let seen = json!({"state":"connected","at":1_700_000_000_001_i64,"pid":std::process::id()});
    s.write("relay-state.json", &seen.to_string())?;
    let out = s.command(&["tunnel", "status"]).output().map_err(text)?;
    let shown = String::from_utf8_lossy(&out.stdout).to_string();
    ensure(
        shown.contains("connected to its relay") && shown.contains("relay: connected since 2023-11-14T22:13:20Z"),
        format!("status without --json shows the relay: {shown}"),
    )
}

fn taken(s: &Smoke) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let state = s.tunnel("relay-state.json");
    let seen = json!({"state":"replaced","at":1_700_000_000_001_i64,"pid":std::process::id()}).to_string();
    s.write("relay-state.json", &seen)?;
    let manager = s.root.join("stopping-manager");
    let stopped =
        format!("#!/bin/sh\necho '{}' > '{}'\nexit 113\n", seen.replace("replaced", "stopped"), state.display());
    fs::write(&manager, stopped).map_err(text)?;
    fs::set_permissions(&manager, fs::Permissions::from_mode(0o755)).map_err(text)?;
    for (dir, file) in
        [("Library/LaunchAgents", format!("{SERVICE_LABEL}.plist")), (".config/systemd/user", SERVICE_UNIT.into())]
    {
        let dir = s.home.join(dir);
        fs::create_dir_all(&dir).map_err(text)?;
        fs::write(dir.join(file), "").map_err(text)?;
    }
    let mut off = s.command(&["tunnel", "off"]);
    let out = off.env("LS_LAUNCHCTL", &manager).env("LS_SYSTEMCTL", &manager).output().map_err(text)?;
    let shown = String::from_utf8_lossy(&out.stdout).to_string();
    ensure(
        shown.contains("Another machine uses the Worker smoke") && !shown.contains("delete --name") && !state.exists(),
        format!("off on a relay another machine took never says how to delete its Worker: {shown}"),
    )?;
    println!("ok: off on a relay another machine took");
    Ok(())
}
