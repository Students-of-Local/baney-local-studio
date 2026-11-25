use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;

use local_studio_core::tunnel::relay::{WORKER_CONFIG, WORKER_JS};
use local_studio_core::tunnel::{SERVICE_LABEL, SERVICE_UNIT};
use serde_json::{json, Value};

use super::{ensure, head, header, mode, origin, read_json, text, Smoke, ACCOUNT};

const OTHER: &str = "fedcba9876543210fedcba9876543210";
const HOST: &str = "smoke.person.workers.dev";
const ONE: &str = r#"{"loggedIn":true,"accounts":[{"id":"0123456789abcdef0123456789abcdef","name":"Smoke"}]}"#;
const TWO: &str = r#"{"loggedIn":true,"accounts":[{"id":"0123456789abcdef0123456789abcdef","name":"Smoke"},{"id":"fedcba9876543210fedcba9876543210","name":"Other"}]}"#;

const NPX: &str = r#"#!/bin/sh
{ echo "npx $*"; echo "cwd $PWD"; ls -A; env; } >> "$SHIM_STATE/npx.log"
shift 2
case "$1" in
  whoami) cat "$SHIM_STATE/whoami"; grep -q '"loggedIn":true' "$SHIM_STATE/whoami" || exit 1 ;;
  deployments)
    [ -f "$SHIM_STATE/exists" ] && { echo "[]"; exit 0; }
    [ -f "$SHIM_STATE/lookup-fails" ] && { echo "fetch failed" >&2; exit 1; }
    echo "This Worker does not exist on your account. [code: 10007]" >&2
    exit 1 ;;
  deploy)
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
echo "$0 $*" >> "$SHIM_STATE/manager.log"
[ "$1" = "--user" ] && shift
up() {
  touch "$SHIM_STATE/up"
  if [ -f "$SHIM_STATE/refused" ]; then
    rm "$SHIM_STATE/refused"
    echo '{"state":"unauthorized","at":1,"pid":1}' > "$LOCAL_STUDIO_DATA_DIR/tunnel/relay-state.json"
  elif [ ! -f "$SHIM_STATE/never" ]; then
    touch "$SHIM_STATE/connected"
  fi
}
case "$1" in
  restart|bootstrap) up ;;
  disable|bootout) rm -f "$SHIM_STATE/up" "$SHIM_STATE/connected" ;;
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
    }
    let (code, more, body) = match path {
        "/_relay/health" => (200, "", json!({"ok":true,"connected":state.join("connected").exists()})),
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
        for (name, body) in [("npx", NPX), ("manager", MANAGER)] {
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
        command
            .arg("--json")
            .env("LS_NPX", self.s.root.join("npx"))
            .env("LS_LAUNCHCTL", self.s.root.join("manager"))
            .env("LS_SYSTEMCTL", self.s.root.join("manager"))
            .env("LS_TUNNEL_CHECK_URL", &self.check_url)
            .env("LS_RELAY_WAIT_MS", "2000")
            .env("SHIM_STATE", &self.state)
            .env_remove("CLOUDFLARE_ACCOUNT_ID");
        change(&mut command);
        let out = command.output().map_err(text)?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        self.said.push_str(&stdout);
        self.said.push_str(&String::from_utf8_lossy(&out.stderr));
        println!("$ local-studio {}\n  {}", args.join(" "), stdout.trim());
        serde_json::from_str(stdout.trim()).map_err(|e| format!("not one JSON state ({e}): {stdout}"))
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
        let mut folders = vec![self.s.data.clone(), self.s.home.clone()];
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

    fn service_file(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.s.home.join(format!("Library/LaunchAgents/{SERVICE_LABEL}.plist"))
        } else {
            self.s.home.join(".config/systemd/user").join(SERVICE_UNIT)
        }
    }

    fn hosts(&self) -> Result<Value, String> {
        Ok(read_json(&self.s.data.join("remote/serve.json"))?["allowTunnelHosts"].clone())
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
        for name in ["npx.log", "manager.log", "checks.log"] {
            ensure(!has(self.log(name).as_bytes()), format!("{what}: {name} (argv, env) holds no key"))?;
        }
        let leak = self.written()?.into_iter().find(|(path, bytes)| !path.ends_with("relay.json") && has(bytes));
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
    let deploys = c.log("npx.log").matches("wrangler@4.163.0 deploy ").count();
    let value = c.halted("confirm", &["smoke"])?;
    let named = value["message"].as_str().is_some_and(|said| said.contains("already has a Worker named smoke"));
    ensure(named && value["nextCommand"] == format!("{given} --yes"), format!("S17 asks first: {value}"))?;
    let after = c.log("npx.log").matches("wrangler@4.163.0 deploy ").count();
    ensure(after == deploys, "S17 a Worker already there is never deployed over without a yes")?;
    c.set("exists", false)?;
    println!("ok: S17 the name is taken");

    c.set("deploy-fails", true)?;
    let message = c.refused("a failed deploy", &["smoke"], "relay_deploy")?;
    ensure(message.contains("deploy broke") && !message.contains("early words"), "S6 the last of wrangler's output")?;
    ensure(message.chars().count() < 350 && !c.log("manager.log").contains("restart"), "S6 cut, no service")?;
    c.set("deploy-fails", false)?;
    println!("ok: S6 deploy fails");

    c.set("secret-fails", true)?;
    let value = c.setup(&["smoke"])?;
    let message = value["message"].as_str().unwrap_or_default();
    ensure(value["code"] == "relay_key" && message.contains("refused <key>"), format!("S7 relay_key: {value}"))?;
    let left = ["relay.json", "relay", "route.json"].iter().any(|name| c.s.tunnel(name).exists());
    ensure(!left && !c.service_file().exists(), "S7 no relay.json, no service, no folder")?;
    c.set("secret-fails", false)?;
    c.no_key("S7")?;
    println!("ok: S7 the secret fails");

    let mut args = vec!["tunnel", "setup", "--relay", "smoke"];
    args.extend(["--serve", "18194"]);
    let value = c.ls(&args, |command| {
        command.env_remove("LS_LAUNCHCTL").env_remove("LS_SYSTEMCTL");
    })?;
    ensure(value["code"] == "unsafe_service_path", format!("S16 the real service manager: {value}"))?;
    println!("ok: S16 real service manager");
    Ok(())
}

pub fn live(c: &mut Cases) -> Result<(), String> {
    c.set("refused", true)?;
    c.set("exists", true)?;
    let value = c.setup(&["smoke", "--yes"])?;
    c.live(&value)?;
    ensure(!c.state.join("early-record").exists(), "relay.json is written only after the secret put")?;
    let checks = c.log("checks.log");
    let first = "/_relay/health route=false agent=true";
    ensure(checks.lines().next() == Some(first), format!("the relay's health comes first: {checks}"))?;
    ensure(checks.contains("/mcp ") && !checks.contains("route=true"), format!("route.json comes last: {checks}"))?;
    let started = |c: &Cases| {
        c.log("manager.log").lines().filter(|l| l.contains(" restart ") || l.contains(" bootstrap ")).count()
    };
    ensure(started(c) == 2, "a key the relay doesn't know yet (401) restarts the service")?;
    for (file, want) in [("relay.js", WORKER_JS), ("wrangler.jsonc", WORKER_CONFIG)] {
        ensure(fs::read_to_string(c.s.tunnel("relay").join(file)).map_err(text)? == want, "the vendored files deploy")?;
    }
    let npx = c.log("npx.log");
    ensure(npx.contains("WRANGLER_SEND_METRICS=false"), "wrangler's metrics are off")?;
    for args in [
        "whoami --json",
        "deployments list --name smoke --json",
        "deploy --name smoke",
        "secret put RELAY_KEY --name smoke",
    ] {
        ensure(npx.contains(&format!("npx --yes wrangler@4.163.0 {args}\n")), format!("wrangler runs {args}"))?;
    }
    let cwd = c.s.tunnel("relay").display().to_string();
    let deploy = format!("deploy --name smoke\ncwd {cwd}\nrelay.js\nwrangler.jsonc\n");
    ensure(npx.contains(&deploy), "the deploy folder holds only the two files")?;
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
    let value = c.ls(&["tunnel", "setup", "--relay", "--yes"], |_| {})?;
    c.live(&value)?;
    let all = c.keys();
    ensure(all.len() == first.len() + 1 && !first.contains(&all[first.len()]), "S9 sets one new key")?;
    ensure(started(c) == 3, "S9 restarts the service")?;
    ensure(!c.state.join("early-record").exists(), "S9 relay.json changes only after the secret put")?;
    c.no_key("S9")?;
    c.set("deploy-fails", true)?;
    let kept = |c: &Cases| {
        let (folder, mut files) = (c.s.tunnel("relay"), c.written()?);
        files.retain(|path, _| !path.starts_with(&folder));
        Ok::<_, String>(files)
    };
    let before = kept(c)?;
    let value = c.setup(&["smoke"])?;
    ensure(value["code"] == "relay_deploy" && kept(c)? == before, "a failed deploy on a live relay changes nothing")?;
    ensure(
        value["message"].as_str().is_some_and(|said| said.contains("deploy broke <key>")),
        format!("the key in wrangler's output is replaced: {value}"),
    )?;
    c.set("deploy-fails", false)?;
    println!("ok: S9 again");
    Ok(())
}
