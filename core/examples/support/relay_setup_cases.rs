use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use local_studio_core::tunnel::relay::{WORKER_CONFIG, WORKER_JS};
use local_studio_core::tunnel::{SERVICE_LABEL, SERVICE_UNIT};
use serde_json::{json, Value};

use super::{chmod, ensure, head, header, mode, origin, read_json, text, Smoke, ACCOUNT};

const OTHER: &str = "fedcba9876543210fedcba9876543210";
const HOST: &str = "smoke.person.workers.dev";
const ONE: &str = r#"{"loggedIn":true,"accounts":[{"id":"0123456789abcdef0123456789abcdef","name":"Smoke"}]}"#;
const TWO: &str = r#"{"loggedIn":true,"accounts":[{"id":"0123456789abcdef0123456789abcdef","name":"Smoke"},{"id":"fedcba9876543210fedcba9876543210","name":"Other"}]}"#;

const NPX: &str = r#"#!/bin/sh
{ echo "npx $*"; echo "cwd $PWD"; ls -A; echo end; env; } >> "$SHIM_STATE/npx.log"
shift 2
case "$1" in
  whoami) cat "$SHIM_STATE/whoami"; grep -q '"loggedIn":true' "$SHIM_STATE/whoami" || exit 1 ;;
  deployments)
    [ -f "$SHIM_STATE/exists" ] && { echo "[]"; exit 0; }
    [ -f "$SHIM_STATE/lookup-fails" ] && { echo "fetch failed" >&2; exit 1; }
    echo "This Worker does not exist on your account. [code: 10007]" >&2
    exit 1 ;;
  deploy)
    [ -f "$SHIM_STATE/interrupt-deploy" ] && { (sleep 3; touch "$SHIM_STATE/late") & kill -INT "$PPID"; wait; }
    [ -f "$SHIM_STATE/no-subdomain" ] && { echo "You need to register a workers.dev subdomain before publishing to workers.dev" >&2; exit 1; }
    [ -f "$SHIM_STATE/deploy-fails" ] && { echo "early words"; printf '%0400d\n' 0; echo "deploy broke $(tail -n 1 "$SHIM_STATE/stdin.log" 2>/dev/null)"; exit 1; }
    { echo '{"type":"wrangler-session"}'; echo '{"type":"deploy","targets":["http://a.workers.dev","https://u@b.workers.dev","https://c.example.com","https://smoke.person.workers.dev"]}'; } > "$WRANGLER_OUTPUT_FILE_PATH" ;;
  secret)
    key=$(cat)
    echo "$key" >> "$SHIM_STATE/stdin.log"
    grep -q -- "$key" "$LOCAL_STUDIO_DATA_DIR/tunnel/relay.json" 2>/dev/null && touch "$SHIM_STATE/early-record"
    [ -f "$SHIM_STATE/secret-fails" ] && { echo "refused $key"; echo "refused $key" >&2; exit 1; } ;;
esac
exit 0
"#;

const MANAGER: &str = r#"#!/bin/sh
{ echo "$0 $*"; env; } >> "$SHIM_STATE/manager.log"
[ "$1" = "--user" ] && shift
up() {
  touch "$SHIM_STATE/up"
  rm -f "$SHIM_STATE/connected"
  turned=$(cd "$SHIM_STATE" && ls -d taken unauthorized replaced 2>/dev/null | head -n 1)
  if [ -n "$turned" ]; then
    [ "$turned" = unauthorized ] || touch "$SHIM_STATE/connected"
    [ "$turned" = taken ] && turned=replaced || rm "$SHIM_STATE/$turned"
  elif [ ! -f "$SHIM_STATE/never" ] && grep -q -- "$(tail -n 1 "$SHIM_STATE/stdin.log")" "$LOCAL_STUDIO_DATA_DIR/tunnel/relay.json"; then
    touch "$SHIM_STATE/connected"
    turned=connected
  fi
  [ -z "$turned" ] || echo "{\"state\":\"$turned\",\"at\":1,\"pid\":1}" > "$LOCAL_STUDIO_DATA_DIR/tunnel/relay-state.json"
  [ -f "$SHIM_STATE/interrupt" ] && kill -INT "$PPID"
  [ -f "$SHIM_STATE/hangup" ] && kill -HUP "$PPID"
}
case "$1" in
  restart|bootstrap) up ;;
  disable|bootout)
    [ -f "$SHIM_STATE/stuck" ] && exit 1
    rm -f "$SHIM_STATE/up" "$SHIM_STATE/connected" ;;
  is-active) [ -f "$SHIM_STATE/up" ] || exit 3; echo active ;;
  print) [ -f "$SHIM_STATE/up" ] || exit 113; echo "state = running" ;;
esac
exit 0
"#;

fn check(listener: &TcpListener, state: &Path, route: &Path) {
    for stream in listener.incoming().flatten() {
        let _ = answer(stream, state, route);
    }
}

fn answer(mut stream: TcpStream, state: &Path, route: &Path) -> Result<(), String> {
    let head = head(&mut stream)?;
    let path = head.split(' ').nth(1).unwrap_or_default();
    if header(&head, "host") == HOST {
        let mut log = fs::OpenOptions::new().create(true).append(true).open(state.join("checks.log")).map_err(text)?;
        let agent = header(&head, "user-agent").starts_with("LocalStudio/");
        writeln!(log, "{path} route={} agent={agent}", route.exists()).map_err(text)?;
        let mut heads = fs::OpenOptions::new().create(true).append(true).open(state.join("heads.log")).map_err(text)?;
        writeln!(heads, "{head}").map_err(text)?;
    }
    let (code, more, body) = match path {
        "/_relay/health" => (200, "", json!({"ok":true,"connected":state.join("connected").exists()})),
        "/v1/health" if state.join("strict").exists() => (403, "", json!({})),
        "/v1/health" => (200, "", json!({"ok":true})),
        "/mcp" => (401, "WWW-Authenticate: Bearer resource_metadata=\"x\"\r\n", json!({})),
        _ => (404, "", json!({})),
    };
    let body = body.to_string();
    let length = body.len();
    let reply = format!("HTTP/1.1 {code} X\r\n{more}Content-Length: {length}\r\nConnection: close\r\n\r\n{body}");
    stream.write_all(reply.as_bytes()).map_err(text)
}

pub struct Cases<'a> {
    s: &'a Smoke,
    state: PathBuf,
    check_url: String,
    mcp: String,
    said: String,
}

impl<'a> Cases<'a> {
    pub fn start(s: &'a Smoke) -> Result<Self, String> {
        use std::os::unix::fs::PermissionsExt;
        let state = s.root.join("state");
        fs::create_dir_all(&state).map_err(text)?;
        let real = format!("#!/bin/sh\ntouch '{}'\nexit 1\n", state.join("real-manager").display());
        fs::create_dir(s.root.join("stubs")).map_err(text)?;
        for (name, body) in
            [("npx", NPX), ("manager", MANAGER), ("stubs/systemctl", real.as_str()), ("stubs/launchctl", real.as_str())]
        {
            fs::write(s.root.join(name), body).map_err(text)?;
            fs::set_permissions(s.root.join(name), fs::Permissions::from_mode(0o755)).map_err(text)?;
        }
        let listener = TcpListener::bind("127.0.0.1:0").map_err(text)?;
        let check_url = format!("http://{}", listener.local_addr().map_err(text)?);
        let (seen, route) = (state.clone(), s.tunnel("route.json"));
        std::thread::spawn(move || check(&listener, &seen, &route));
        Ok(Self { s, state, check_url, mcp: origin(401)?.to_string(), said: String::new() })
    }

    fn set(&self, name: &str, on: bool) -> Result<(), String> {
        let path = self.state.join(name);
        if on {
            fs::write(path, "").map_err(text)
        } else {
            fs::remove_file(path).map_err(text)
        }
    }

    fn whoami(&self, said: &str) -> Result<(), String> {
        fs::write(self.state.join("whoami"), said).map_err(text)
    }

    fn log(&self, name: &str) -> String {
        fs::read_to_string(self.state.join(name)).unwrap_or_default()
    }

    fn keys(&self) -> Vec<String> {
        self.log("stdin.log").lines().map(ToString::to_string).collect()
    }

    fn ls(&mut self, args: &[&str], change: impl Fn(&mut Command)) -> Result<Value, String> {
        let mut command = self.s.command(args);
        self.shims(command.arg("--json"));
        change(&mut command);
        let out = command.output().map_err(text)?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        self.said.push_str(&stdout);
        self.said.push_str(&String::from_utf8_lossy(&out.stderr));
        println!("$ local-studio {}\n  {}", args.join(" "), stdout.trim());
        serde_json::from_str(stdout.trim()).map_err(|e| format!("not one JSON state ({e}): {stdout}"))
    }

    fn shims(&self, command: &mut Command) {
        command
            .env("LS_NPX", self.s.root.join("npx"))
            .env("LS_LAUNCHCTL", self.s.root.join("manager"))
            .env("LS_SYSTEMCTL", self.s.root.join("manager"))
            .env("LS_TUNNEL_CHECK_URL", &self.check_url)
            .env("LS_RELAY_WAIT_MS", "2000")
            .env("SHIM_STATE", &self.state)
            .env_remove("CLOUDFLARE_ACCOUNT_ID");
    }

    fn setup(&mut self, more: &[&str]) -> Result<Value, String> {
        let mcp = self.mcp.clone();
        let mut args = vec!["tunnel", "setup", "--relay"];
        args.extend(more);
        args.extend(["--serve", "18194", "--mcp", &mcp]);
        self.ls(&args, |_| {})
    }

    fn refused(&mut self, what: &str, more: &[&str], code: &str) -> Result<String, String> {
        let before = self.written()?;
        let value = self.setup(more)?;
        let message = value["message"].as_str().unwrap_or_default().to_string();
        ensure(value["state"] == "error" && value["code"] == code, format!("{what} is {code}: {value}"))?;
        ensure(self.written()? == before, format!("{what} changes nothing"))?;
        Ok(message)
    }

    fn halted(&mut self, state: &str, more: &[&str]) -> Result<Value, String> {
        let before = self.written()?;
        let value = self.setup(more)?;
        ensure(value["state"] == state && value["next"] == state, format!("{state}: {value}"))?;
        ensure(self.written()? == before, format!("{state} writes nothing"))?;
        Ok(value)
    }

    fn written(&self) -> Result<BTreeMap<PathBuf, Vec<u8>>, String> {
        let mut found = BTreeMap::new();
        let mut folders = vec![self.s.data.clone(), self.s.home.clone(), self.s.root.join("tmp")];
        while let Some(folder) = folders.pop() {
            let Ok(entries) = fs::read_dir(&folder) else { continue };
            for path in entries.flatten().map(|entry| entry.path()) {
                if path.is_dir() {
                    folders.push(path);
                } else if !path.ends_with("up.lock") {
                    found.insert(path.clone(), fs::read(&path).map_err(text)?);
                }
            }
        }
        Ok(found)
    }

    fn kept(&self) -> Result<BTreeMap<PathBuf, Vec<u8>>, String> {
        let (folder, mut files) = (self.s.tunnel("relay"), self.written()?);
        files.retain(|path, _| !path.starts_with(&folder));
        Ok(files)
    }

    fn service_file(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.s.home.join(format!("Library/LaunchAgents/{SERVICE_LABEL}.plist"))
        } else {
            self.s.home.join(".config/systemd/user").join(SERVICE_UNIT)
        }
    }

    fn started(&self) -> usize {
        let runs = |line: &&str| line.starts_with('/') && (line.contains(" restart ") || line.contains(" bootstrap "));
        self.log("manager.log").lines().filter(runs).count()
    }

    fn deploys(&self) -> usize {
        self.log("npx.log").matches("wrangler@4.163.0 deploy ").count()
    }

    fn hosts(&self) -> Result<Value, String> {
        Ok(read_json(&self.s.data.join("remote/serve.json"))?["allowTunnelHosts"].clone())
    }

    fn gone(&self, what: &str) -> Result<(), String> {
        let left = ["relay.json", "relay-state.json", "route.json"].iter().find(|name| self.s.tunnel(name).exists());
        ensure(left.is_none(), format!("{what} leaves {left:?}"))?;
        ensure(!self.service_file().exists() && !self.state.join("up").exists(), format!("{what} leaves the service"))?;
        ensure(!self.s.tunnel("relay").exists(), format!("{what} leaves the relay folder"))?;
        ensure(self.hosts()? == json!([]), format!("{what} leaves the host allowed"))
    }

    fn live(&mut self, value: &Value) -> Result<(), String> {
        let mcp: u16 = self.mcp.parse().map_err(text)?;
        ensure(
            value["state"] == "ready"
                && value["hostname"] == HOST
                && value["relay"] == json!({"name":"smoke","accountId":ACCOUNT})
                && value["mcpUrl"] == format!("https://{HOST}/mcp")
                && value["wsUrl"] == format!("wss://{HOST}/v1/ws")
                && value["checks"] == json!({"health":"ok","mcp":"ok","dns":"local","serveAllowed":true}),
            format!("ready with the relay: {value}"),
        )?;
        let route = read_json(&self.s.tunnel("route.json"))?;
        let want = json!({"v":1,"owner":"local-studio","route":"cloudflare","hostname":HOST,"aliases":[],"ports":{"mcp":mcp,"serve":18194}});
        ensure(route == want, format!("route.json {route} != {want}"))?;
        let record = read_json(&self.s.tunnel("relay.json"))?;
        let key = self.keys().pop().unwrap_or_default();
        let want = json!({"v":1,"url":format!("https://{HOST}"),"name":"smoke","accountId":ACCOUNT,"key":key});
        ensure(record == want && key.len() == 43, "relay.json holds exactly its five fields and the key wrangler got")?;
        for (path, want) in [("relay.json", 0o600), ("route.json", 0o600), ("relay", 0o700), ("", 0o700)] {
            ensure(mode(&self.s.tunnel(path)) == want, format!("{path:?} must be {want:o}"))?;
        }
        ensure(self.hosts()? == json!([HOST]), "serve.json allows the host")
    }

    fn no_key(&self, what: &str) -> Result<(), String> {
        let keys = self.keys();
        ensure(!keys.is_empty(), "the shim got a key")?;
        let has = |bytes: &[u8]| keys.iter().any(|key| String::from_utf8_lossy(bytes).contains(key.as_str()));
        ensure(!has(self.said.as_bytes()), format!("{what}: nothing printed holds a key"))?;
        for name in ["npx.log", "manager.log", "heads.log"] {
            ensure(!has(self.log(name).as_bytes()), format!("{what}: {name} (argv, env) holds no key"))?;
        }
        let record = self.s.tunnel("relay.json");
        let leak = self.written()?.into_iter().find(|(path, bytes)| *path != record && has(bytes));
        ensure(leak.is_none(), format!("{what}: {:?} holds a key", leak.map(|(path, _)| path)))
    }
}

pub fn before(c: &mut Cases) -> Result<(), String> {
    c.whoami(ONE)?;
    for name in ["-bad", "UPPER", &"a".repeat(64)] {
        c.refused("a bad name", &[name], "bad_name")?;
    }
    println!("ok: S15 bad names");

    let missing = c.s.root.join("no-npx");
    let value = c.ls(&["tunnel", "setup", "--relay", "smoke"], |command| {
        command.env("LS_NPX", &missing);
    })?;
    let node = "https://nodejs.org/en/download";
    ensure(value["state"] == "needs_node" && value["url"] == node, format!("S1 needs_node: {value}"))?;
    ensure(c.written()?.is_empty(), "S1 writes nothing")?;
    println!("ok: S1 no npx");

    for said in [r#"{"loggedIn":false}"#, "You are not authenticated. Please run `wrangler login`."] {
        c.whoami(said)?;
        let value = c.halted("needs_signin", &["smoke"])?;
        ensure(value["nextCommand"] == "npx --yes wrangler@4.163.0 login", format!("S2 the login command: {value}"))?;
    }
    ensure(c.written()?.is_empty() && !c.s.tunnel("relay").exists(), "S2 writes nothing")?;
    println!("ok: S2 signed out");

    c.whoami(TWO)?;
    let value = c.halted("needs_account", &["smoke"])?;
    let listed = json!([{"id":ACCOUNT,"name":"Smoke"},{"id":OTHER,"name":"Other"}]);
    let given = format!("local-studio tunnel setup --relay smoke --serve 18194 --mcp {}", c.mcp);
    let next = format!("{given} --account ID");
    ensure(value["accounts"] == listed && value["template"] == next, format!("S3 lists both, picks none: {value}"))?;
    c.set("no-subdomain", true)?;
    let value = c.halted("needs_subdomain", &["smoke", "--account", OTHER])?;
    let url = format!("https://dash.cloudflare.com/{OTHER}/workers/onboarding");
    ensure(value["url"] == url, format!("S5 the account's URL: {value}"))?;
    ensure(c.log("npx.log").contains(&format!("CLOUDFLARE_ACCOUNT_ID={OTHER}")), "S3 goes on with that account")?;
    c.set("no-subdomain", false)?;
    println!("ok: S3 S5 two accounts, no subdomain");

    c.whoami(ONE)?;
    for id in [OTHER, "not-an-id"] {
        c.refused("an account that isn't listed", &["smoke", "--account", id], "bad_account")?;
    }
    println!("ok: S4 bad account");

    c.set("lookup-fails", true)?;
    c.refused("a lookup that fails", &["smoke"], "relay_lookup")?;
    c.set("lookup-fails", false)?;
    c.set("exists", true)?;
    let deploys = c.deploys();
    let value = c.halted("confirm", &["smoke"])?;
    let named = value["message"].as_str().is_some_and(|said| said.contains("already has a Worker named smoke"));
    ensure(named && value["nextCommand"] == format!("{given} --yes"), format!("S17 asks first: {value}"))?;
    ensure(c.deploys() == deploys, "S17 a Worker already there is never deployed over without a yes")?;
    prompt(c)?;
    c.set("exists", false)?;
    println!("ok: S17 the name is taken");

    c.set("deploy-fails", true)?;
    let message = c.refused("a failed deploy", &["smoke"], "relay_deploy")?;
    ensure(message.contains("deploy broke") && !message.contains("early words"), "S6 the last of wrangler's output")?;
    ensure(message.chars().count() < 350 && c.started() == 0, "S6 cut, no service")?;
    c.set("deploy-fails", false)?;
    println!("ok: S6 deploy fails");

    c.set("secret-fails", true)?;
    let value = c.setup(&["smoke"])?;
    let message = value["message"].as_str().unwrap_or_default();
    ensure(value["code"] == "relay_key" && message.contains("refused <key>"), format!("S7 relay_key: {value}"))?;
    let left = ["relay.json", "relay", "route.json"].iter().any(|name| c.s.tunnel(name).exists());
    ensure(!left && !c.service_file().exists(), "S7 no relay.json, no service, no folder")?;
    let allowed = c.s.data.join("remote/serve.json").exists() && c.hosts()? != json!([]);
    ensure(c.started() == 0 && !allowed, "S7 starts nothing and allows no host")?;
    c.set("secret-fails", false)?;
    c.no_key("S7")?;
    println!("ok: S7 the secret fails");

    let mut args = vec!["tunnel", "setup", "--relay", "smoke"];
    args.extend(["--serve", "18194"]);
    let (before, deploys, keys) = (c.written()?, c.deploys(), c.keys());
    let path = format!("{}:{}", c.s.root.join("stubs").display(), std::env::var("PATH").map_err(text)?);
    let value = c.ls(&args, |command| {
        command.env_remove("LS_LAUNCHCTL").env_remove("LS_SYSTEMCTL").env("PATH", &path);
    })?;
    ensure(value["code"] == "unsafe_service_path", format!("S16 the real service manager: {value}"))?;
    let same = c.written()? == before && c.deploys() == deploys && c.keys() == keys;
    ensure(same && !c.state.join("real-manager").exists(), "S16 changes nothing and runs no service manager")?;
    println!("ok: S16 real service manager");
    Ok(())
}

fn prompt(c: &mut Cases) -> Result<(), String> {
    if cfg!(target_os = "macos") {
        return Ok(());
    }
    let before = c.written()?;
    let line = format!("exec '{}' tunnel setup --relay smoke --serve 18194 --mcp {}", c.s.bin.display(), c.mcp);
    let mut command = c.s.command(&[]);
    let mut script = Command::new("script");
    script.env_clear().args(["-qec", &line, "/dev/null"]);
    c.shims(&mut command);
    for (name, value) in command.get_envs() {
        match value {
            Some(value) => script.env(name, value),
            None => script.env_remove(name),
        };
    }
    let mut child = script.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(text)?;
    let (mut typed, mut shown) = (child.stdin.take().ok_or("no stdin")?, child.stdout.take().ok_or("no stdout")?);
    let mut said = Vec::new();
    while !String::from_utf8_lossy(&said).contains("Replace it? [y/N]") {
        let mut some = [0; 4096];
        let read = shown.read(&mut some).map_err(text)?;
        ensure(read > 0, format!("S17 asks in a terminal: {}", String::from_utf8_lossy(&said)))?;
        said.extend(&some[..read]);
    }
    typed.write_all(b"\x03").and_then(|()| typed.flush()).map_err(text)?;
    let ended = (0..50).any(|_| {
        std::thread::sleep(Duration::from_millis(100));
        matches!(child.try_wait(), Ok(Some(_)))
    });
    if !ended {
        let _ = child.kill();
    }
    ensure(ended, "S17 Ctrl-C at the question ends setup")?;
    ensure(c.written()? == before, "S17 Ctrl-C at the question changes nothing")
}

pub fn live(c: &mut Cases) -> Result<(), String> {
    c.set("unauthorized", true)?;
    let earlier = c.log("npx.log").len();
    let value = c.setup(&["smoke"])?;
    c.live(&value)?;
    ensure(!c.state.join("early-record").exists(), "relay.json is written only after the secret put")?;
    let checks = c.log("checks.log");
    let first = "/_relay/health route=false agent=true";
    ensure(checks.lines().next() == Some(first), format!("the relay's health comes first: {checks}"))?;
    ensure(checks.contains("/mcp ") && !checks.contains("route=true"), format!("route.json comes last: {checks}"))?;
    ensure(c.started() == 2, "a key the relay doesn't know yet (401) restarts the service")?;
    for (file, want) in [("relay.js", WORKER_JS), ("wrangler.jsonc", WORKER_CONFIG)] {
        ensure(fs::read_to_string(c.s.tunnel("relay").join(file)).map_err(text)? == want, "the vendored files deploy")?;
    }
    let npx = c.log("npx.log").split_off(earlier);
    let calls: Vec<&str> = npx.split("npx --yes ").skip(1).collect();
    ensure(calls.iter().all(|call| call.contains("\nWRANGLER_SEND_METRICS=false\n")), "wrangler's metrics are off")?;
    let account = format!("\nCLOUDFLARE_ACCOUNT_ID={ACCOUNT}\n");
    ensure(calls.len() == 4 && calls[1..].iter().all(|call| call.contains(&account)), "wrangler uses the account")?;
    for args in [
        "whoami --json",
        "deployments list --name smoke --json",
        "deploy --name smoke",
        "secret put RELAY_KEY --name smoke",
    ] {
        ensure(npx.contains(&format!("npx --yes wrangler@4.163.0 {args}\n")), format!("wrangler runs {args}"))?;
    }
    let cwd = c.s.tunnel("relay").display().to_string();
    for args in ["whoami --json", "deploy --name smoke"] {
        let ran = format!("wrangler@4.163.0 {args}\ncwd {cwd}\nrelay.js\nwrangler.jsonc\nend\n");
        ensure(npx.contains(&ran), format!("wrangler {args} runs in a folder that holds only the two files"))?;
    }
    let file = fs::read_to_string(c.service_file()).map_err(text)?;
    let quote = |word: &str| {
        if cfg!(target_os = "macos") {
            format!("<string>{word}</string>")
        } else {
            format!("\"{word}\"")
        }
    };
    let words = [&c.s.bin.display().to_string(), "tunnel", "relay", "run"].map(quote);
    let between = if cfg!(target_os = "macos") { "\n\t\t" } else { " " };
    ensure(file.contains(&words.join(between)), format!("the service runs tunnel relay run: {file}"))?;
    let data = c.s.data.display().to_string();
    ensure(file.contains("LOCAL_STUDIO_DATA_DIR") && file.contains(&data), format!("the data dir is set: {file}"))?;
    c.no_key("S8")?;
    println!("ok: S8 live");

    let first = c.keys();
    c.set("exists", true)?;
    c.set("replaced", true)?;
    let value = c.ls(&["tunnel", "setup", "--relay", "--yes"], |_| {})?;
    c.live(&value)?;
    let all = c.keys();
    ensure(all.len() == first.len() + 1 && !first.contains(&all[first.len()]), "S9 sets one new key")?;
    ensure(c.started() == 4, "S9 restarts the service, again when it was replaced")?;
    ensure(!c.state.join("early-record").exists(), "S9 relay.json changes only after the secret put")?;
    c.no_key("S9")?;
    c.set("deploy-fails", true)?;
    let before = c.kept()?;
    let value = c.setup(&["smoke"])?;
    ensure(value["code"] == "relay_deploy" && c.kept()? == before, "a failed deploy on a live relay changes nothing")?;
    ensure(
        value["message"].as_str().is_some_and(|said| said.contains("deploy broke <key>")),
        format!("the key in wrangler's output is replaced: {value}"),
    )?;
    ensure(c.started() == 4 && c.keys() == all, "a failed deploy sets no key and restarts nothing")?;
    c.no_key("S9 a failed deploy")?;
    c.set("deploy-fails", false)?;
    println!("ok: S9 again");
    Ok(())
}

fn unparsed(c: &mut Cases) -> Result<(), String> {
    let serve = c.s.data.join("remote/serve.json");
    let kept = fs::read(&serve).map_err(text)?;
    let why = format!("{HOST} isn't allowed in serve.json yet: parse {}: EOF", serve.display());
    fs::write(&serve, "{").map_err(text)?;
    for (off, on) in [("strict", "never"), ("never", "strict")] {
        c.set(off, true)?;
        c.set(off, false)?;
        c.set(on, true)?;
        let value = c.setup(&["smoke", "--yes"])?;
        ensure(
            value["code"] == "going_live" && value["message"].as_str().is_some_and(|said| said.contains(&why)),
            format!("going_live says why serve.json doesn't allow the host: {value}"),
        )?;
    }
    c.set("strict", false)?;
    c.set("never", true)?;
    fs::write(&serve, kept).map_err(text)
}

pub fn status(c: &mut Cases) -> Result<(), String> {
    let again = "local-studio tunnel setup --relay";
    let dead = {
        let mut child = Command::new("true").stdin(Stdio::null()).spawn().map_err(text)?;
        child.wait().map_err(text)?;
        child.id()
    };
    let me = std::process::id();
    for (state, pid, want, next, says) in [
        ("connected", me, "connected", "ready", "connected to its relay"),
        ("connecting", me, "connecting", "waiting", "connecting to its relay"),
        ("disconnected", me, "disconnected", "waiting", "lost its relay"),
        ("replaced", me, "replaced", "needs_setup", again),
        ("unauthorized", me, "unauthorized", "needs_setup", "for over ten minutes"),
        ("outdated", me, "outdated", "needs_setup", "Update Local Studio"),
        ("bad_config", me, "bad_config", "needs_setup", again),
        ("stopped", me, "stopped", "needs_setup", again),
        ("connected", dead, "stopped", "needs_setup", again),
    ] {
        c.s.write("relay-state.json", &json!({"state":state,"at":1_700_000_000_001_i64,"pid":pid}).to_string())?;
        let value = c.ls(&["tunnel", "status"], |_| {})?;
        let message = value["message"].as_str().unwrap_or_default();
        ensure(
            value["relay"] == json!({"state":want,"since":(pid == me).then_some(1_700_000_000_001_i64)})
                && value["next"] == next
                && value["hostname"] == HOST
                && message.contains(says)
                && (next != "needs_setup" || value["nextCommand"] == again),
            format!("status when {state}: {value}"),
        )?;
    }
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(text)?.as_millis();
    c.s.write("relay-state.json", &format!(r#"{{"state":"unauthorized","at":{at},"pid":{me}}}"#))?;
    let value = c.ls(&["tunnel", "status"], |_| {})?;
    ensure(
        value["relay"]["state"] == "unauthorized"
            && value["next"] == "waiting"
            && value["nextCommand"] == "local-studio tunnel status --json",
        format!("status when a new key is refused: {value}"),
    )?;
    fs::remove_file(c.s.tunnel("relay-state.json")).map_err(text)?;
    let value = c.ls(&["tunnel", "status"], |_| {})?;
    ensure(value["relay"] == json!({"state":"stopped","since":null}), format!("status with no state file: {value}"))?;
    chmod(&c.s.tunnel("relay.json"), 0o644)?;
    let value = c.ls(&["tunnel", "status"], |_| {})?;
    let next = format!("chmod 600 '{}'", c.s.tunnel("relay.json").display());
    ensure(
        value["relay"]["state"] == "bad_config" && value["nextCommand"] == next,
        format!("status when others can read relay.json: {value}"),
    )?;
    let message = c.refused("a relay.json others can read", &["smoke"], "error")?;
    ensure(message.contains(&next), format!("setup names the chmod: {message}"))?;
    chmod(&c.s.tunnel("relay.json"), 0o600)?;
    println!("ok: S12 status");
    Ok(())
}

pub fn others(c: &mut Cases) -> Result<(), String> {
    c.refused("another relay name", &["another"], "other_route")?;
    c.refused("another account", &["smoke", "--account", OTHER], "other_route")?;
    let value = c.ls(&["tunnel", "setup", "--cloudflare", "studio.example.com", "--yes"], |_| {})?;
    ensure(value["code"] == "other_route", format!("a domain tunnel on the relay route: {value}"))?;
    off(c)?;
    let route = json!({"v":1,"owner":"local-studio","route":"cloudflare","hostname":"studio.example.com","aliases":[],"ports":{"mcp":8787,"serve":18094}});
    c.s.write("route.json", &route.to_string())?;
    c.refused("a domain tunnel", &["smoke"], "other_route")?;
    fs::remove_file(c.s.tunnel("route.json")).map_err(text)?;
    let route = route.to_string().replace("\"local-studio\"", "\"local-studio-browser\"");
    c.s.write("route.json", &route)?;
    let message = c.refused("a route run by browser use", &["smoke"], "other_route")?;
    ensure(message.contains("local-studio-browser remote off"), format!("how to turn it off: {message}"))?;
    fs::remove_file(c.s.tunnel("route.json")).map_err(text)?;
    println!("ok: S14 other routes");

    let held = fs::File::create(c.s.tunnel("relay.lock")).map_err(text)?;
    held.lock().map_err(text)?;
    let keys = c.keys().len();
    let message = c.refused("setup beside another relay run", &["smoke", "--yes"], "busy")?;
    ensure(message.contains("Stop it, then run setup again"), format!("what to stop: {message}"))?;
    ensure(c.keys().len() == keys, "setup beside another relay run makes no new key")?;
    drop(held);
    println!("ok: S18 another relay run");

    c.set("never", true)?;
    let value = c.setup(&["smoke", "--yes"])?;
    let message = value["message"].as_str().unwrap_or_default();
    ensure(value["code"] == "going_live", format!("S10 going_live: {value}"))?;
    ensure(message.contains("doesn't connect to it until setup runs again"), format!("S10 the Worker stays: {value}"))?;
    unparsed(c)?;
    c.gone("S10")?;
    c.set("never", false)?;
    c.no_key("S10")?;
    println!("ok: S10 never connects");
    Ok(())
}

pub fn broken(c: &mut Cases) -> Result<(), String> {
    let value = c.setup(&["smoke", "--yes"])?;
    c.live(&value)?;
    c.set("secret-fails", true)?;
    let before = c.kept()?;
    let value = c.setup(&["smoke"])?;
    let message = value["message"].as_str().unwrap_or_default();
    ensure(value["code"] == "relay_key" && message.contains("as they were"), format!("S18 relay_key: {value}"))?;
    ensure(c.kept()? == before && c.state.join("up").exists(), "S18 a refused key leaves the live relay alone")?;
    c.set("secret-fails", false)?;
    println!("ok: S18 the secret fails on a live relay");
    for (flag, turned) in [("interrupt", "unauthorized"), ("interrupt", "replaced"), ("hangup", "unauthorized")] {
        c.set(flag, true)?;
        c.set(turned, true)?;
        let value = c.setup(&["smoke"])?;
        c.set(flag, false)?;
        let kept = ["relay.json", "route.json"].iter().all(|name| c.s.tunnel(name).exists());
        let kept = kept && c.hosts()? == json!([HOST]) && c.state.join("connected").exists();
        let seen = fs::read_to_string(c.s.tunnel("relay-state.json")).unwrap_or_default().contains("connected");
        let kept = kept && seen && value["code"] == "stopped";
        ensure(kept, format!("S20 {flag} while {turned} leaves the live relay running: {value}"))?;
    }
    c.set("interrupt", true)?;
    let mcp = std::mem::replace(&mut c.mcp, "8799".to_string());
    let value = c.setup(&["smoke"])?;
    c.mcp = mcp;
    c.set("interrupt", false)?;
    ensure(value["code"] == "stopped", format!("S20 stopped with another port: {value}"))?;
    c.gone("S20 another port, never checked")?;
    println!("ok: S20 a live relay stopped while set up again");

    c.set("taken", true)?;
    let starts = c.started();
    let value = c.setup(&["smoke", "--yes"])?;
    let says = value["message"].as_str().is_some_and(|said| said.contains("own connection to it is replaced"));
    let once = value["code"] == "going_live" && c.started() == starts + 2;
    ensure(says && once, format!("S9 a relay another machine keeps is taken back once and not live: {value}"))?;
    c.gone("S9 taken")?;
    c.set("taken", false)?;

    for flag in ["never", "stuck"] {
        c.set(flag, true)?;
    }
    let value = c.setup(&["smoke", "--yes"])?;
    let message = value["message"].as_str().unwrap_or_default();
    ensure(
        value["code"] == "going_live" && message.contains("couldn't be removed") && !message.contains("exposed"),
        format!("S19 says the service is still there: {value}"),
    )?;
    ensure(c.s.tunnel("relay.json").exists() && c.hosts()? == json!([]), "S19 keeps the record for off, not the host")?;
    for flag in ["never", "stuck"] {
        c.set(flag, false)?;
    }
    off(c)?;
    println!("ok: S19 a service that can't be removed");

    for flag in ["interrupt", "hangup"] {
        c.set(flag, true)?;
        let value = c.setup(&["smoke", "--yes"])?;
        ensure(value["code"] == "stopped", format!("S20 {flag} while going live: {value}"))?;
        c.gone("S20")?;
        c.set(flag, false)?;
    }
    c.set("interrupt-deploy", true)?;
    let value = c.setup(&["smoke", "--yes"])?;
    ensure(value["code"] == "relay_deploy", format!("S20 Ctrl-C in a deploy: {value}"))?;
    std::thread::sleep(Duration::from_secs(4));
    ensure(!c.state.join("late").exists(), "S20 nothing wrangler started goes on after Ctrl-C")?;
    c.gone("S20")?;
    c.set("interrupt-deploy", false)?;
    c.no_key("S20")?;
    println!("ok: S20 Ctrl-C");
    Ok(())
}

pub fn off(c: &mut Cases) -> Result<(), String> {
    c.s.write("relay-state.json", "{}")?;
    chmod(&c.s.tunnel("relay.json"), 0o644)?;
    let value = c.ls(&["tunnel", "off"], |_| {})?;
    let message = value["message"].as_str().unwrap_or_default();
    ensure(
        value["state"] == "off" && message.contains("npx --yes wrangler@4.163.0 delete --name smoke"),
        format!("S13 off names the Worker's delete command: {value}"),
    )?;
    c.gone("S13 off")?;
    c.no_key("S13")?;
    println!("ok: S13 off");
    Ok(())
}
