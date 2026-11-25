use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use reqwest::Url;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use super::{
    argv, machine_slug, merge_dns, new_route, normalize_host, ready, serve_disallow, settle, tool, with, Halt, Manager,
    Out, Want, OWN, WRANGLER,
};
use crate::link::devices::random;
use crate::platform::Mode;
use crate::tunnel::record::{Record, AGAIN, FOLDER, RECORD, STATE};
use crate::tunnel::relay::{stop_signal, WORKER_CONFIG, WORKER_JS};
use crate::tunnel::{check_ports, fail, serve_allow, Failure, Ports, Tunnel, UpLock};
use crate::workspace::process::signal_pid_group;

const NO_WORKER: &str = "[code: 10007]";
const NO_ENVIRONMENT: &str = "[code: 10090]";
const NODE_DOWNLOAD: &str = "https://nodejs.org/en/download";
const OUTPUT: &str = "deploy-output.ndjson";
const WHOAMI_WAIT: Duration = Duration::from_mins(3);
const LOOKUP_WAIT: Duration = Duration::from_mins(3);
const DEPLOY_WAIT: Duration = Duration::from_mins(5);
const SECRET_WAIT: Duration = Duration::from_mins(3);
const LIVE_WAIT: Duration = Duration::from_mins(5);
const LIVE_EVERY: Duration = Duration::from_secs(5);

struct Wrangler {
    npx: PathBuf,
    dir: PathBuf,
    account: Option<String>,
    hide: Vec<String>,
}

struct Account {
    id: String,
    name: String,
}

impl Wrangler {
    async fn run(&self, code: &str, args: &[&str], input: &str, wait: Duration) -> Result<Out, Failure> {
        let mut command = tokio::process::Command::new(&self.npx);
        command.args(["--yes", WRANGLER]).args(args).current_dir(&self.dir).env("WRANGLER_SEND_METRICS", "false");
        command.envs(crate::workspace::process::joined_path().map(|path| ("PATH", path)));
        command.envs(self.account.as_ref().map(|id| ("CLOUDFLARE_ACCOUNT_ID", id)));
        command.envs((args[0] == "deploy").then(|| ("WRANGLER_OUTPUT_FILE_PATH", self.dir.join(OUTPUT))));
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        crate::platform::own_group_async(&mut command);
        let broke = |error| fail(code, format!("wrangler {}: {error}", args[0]));
        let stop = stop_signal().map_err(broke)?;
        let mut child = command.spawn().map_err(broke)?;
        let group = child.id();
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.as_bytes()).await;
        }
        let output = tokio::select! {
            output = child.wait_with_output() => output.map_err(broke),
            () = tokio::time::sleep(wait) => {
                Err(fail(code, format!("wrangler {} took over {} minutes.", args[0], wait.as_secs() / 60)))
            }
            () = stop => Err(fail(code, format!("wrangler {} was stopped.", args[0]))),
        };
        if let (Err(_), Some(group)) = (&output, group) {
            signal_pid_group("-KILL", group);
        }
        let output = output?;
        Ok(Out {
            code: output.status.code().unwrap_or(-1),
            stdout: self.hidden(&output.stdout),
            stderr: self.hidden(&output.stderr),
        })
    }

    fn hidden(&self, bytes: &[u8]) -> String {
        let text = String::from_utf8_lossy(bytes).into_owned();
        self.hide.iter().fold(text, |text, key| text.replace(key, "<key>"))
    }

    async fn accounts(&self) -> Result<Option<Vec<Account>>, Failure> {
        let out = self.run("relay_whoami", &["whoami", "--json"], "", WHOAMI_WAIT).await?;
        let text = out.stdout.find('{').map_or("", |at| &out.stdout[at..]);
        let said = serde_json::Deserializer::from_str(text).into_iter::<Value>().find_map(Result::ok);
        let lower = format!("{}\n{}", out.stdout, out.stderr).to_lowercase();
        let signed_out = ["not authenticated", "not logged in", "wrangler login"].iter().any(|s| lower.contains(s));
        match said.as_ref().and_then(|said| said["loggedIn"].as_bool()) {
            Some(true) => Ok(Some(listed(&said.unwrap_or_default()))),
            Some(false) => Ok(None),
            None if signed_out => Ok(None),
            None => Err(failed("relay_whoami", "wrangler whoami", &Out { stdout: String::new(), ..out })),
        }
    }

    async fn exists(&self, name: &str) -> Result<bool, Failure> {
        let args = ["deployments", "list", "--name", name, "--json"];
        let out = self.run("relay_lookup", &args, "", LOOKUP_WAIT).await?;
        let missing = [NO_WORKER, NO_ENVIRONMENT].iter().any(|code| out.text().contains(code));
        match (out.ok(), missing) {
            (true, _) => Ok(true),
            (false, true) => Ok(false),
            _ => Err(failed("relay_lookup", "wrangler deployments list", &out)),
        }
    }

    async fn deploy(&self, name: &str) -> Result<String, Halt> {
        let out = self.run("relay_deploy", &["deploy", "--name", name], "", DEPLOY_WAIT).await?;
        let lines = fs::read_to_string(self.dir.join(OUTPUT)).unwrap_or_default();
        if let Some(host) = deployed(&lines).filter(|_| out.ok()) {
            return Ok(host);
        }
        let lower = format!("{}\n{}", out.stdout, out.stderr).to_lowercase();
        if lower.contains("register a workers.dev subdomain") || lower.contains("/workers/onboarding") {
            let account = self.account.as_deref().unwrap_or_default();
            let url = format!("https://dash.cloudflare.com/{account}/workers/onboarding");
            let message = "This Cloudflare account has no workers.dev subdomain yet. Open the link, pick one, then run setup again with --yes: the Worker may be there already.";
            return Err(Halt::State(needs("needs_subdomain", "url", &url, message)));
        }
        if out.ok() {
            return Err(fail("relay_deploy", "wrangler deploy finished but named no workers.dev address.").into());
        }
        Err(failed("relay_deploy", "wrangler deploy", &out).into())
    }

    async fn put_key(&mut self, name: &str) -> Result<String, Failure> {
        let key = URL_SAFE_NO_PAD.encode(random::<32>().map_err(|error| error.to_string())?);
        self.hide.push(key.clone());
        let args = ["secret", "put", "RELAY_KEY", "--name", name];
        let put = self.run("relay_key", &args, &key, SECRET_WAIT).await?;
        if put.ok() {
            Ok(key)
        } else {
            Err(failed("relay_key", "wrangler secret put", &put))
        }
    }
}

fn needs(state: &str, field: &str, value: &str, message: &str) -> Value {
    json!({ "state": state, "next": state, "route": "cloudflare", (field): value, "message": message })
}

fn failed(code: &str, what: &str, out: &Out) -> Failure {
    let text = out.text();
    let skip = text.chars().count().saturating_sub(300);
    let said: String = text.chars().skip(skip).collect();
    fail(code, format!("{what} failed (exit {}): {said}", out.code))
}

fn listed(whoami: &Value) -> Vec<Account> {
    let accounts = whoami["accounts"].as_array().into_iter().flatten();
    let known = accounts.filter_map(|account| Some((account, account["id"].as_str().filter(|id| account_id(id))?)));
    known.map(|(account, id)| Account { id: id.into(), name: account["name"].as_str().unwrap_or(id).into() }).collect()
}

fn account_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn worker_name(text: &str) -> bool {
    (1..=63).contains(&text.len())
        && !text.starts_with('-')
        && !text.ends_with('-')
        && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'z' | b'-'))
}

fn deployed(lines: &str) -> Option<String> {
    lines
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["type"] == "deploy")
        .flat_map(|entry| entry["targets"].as_array().cloned().unwrap_or_default())
        .filter_map(|target| Url::parse(target.as_str()?).ok())
        .filter(|url| url.scheme() == "https" && url.username().is_empty() && url.password().is_none())
        .filter_map(|url| url.host_str().and_then(normalize_host))
        .find(|host| host.ends_with(".workers.dev"))
}

fn bad_account(id: &str) -> Failure {
    fail("bad_account", format!("{id} isn't one of the Cloudflare accounts you're signed in to. Nothing was changed."))
}

fn pick(accounts: &[Account], wanted: Option<&str>, again: &str) -> Result<String, Halt> {
    if let Some(id) = wanted {
        let listed = accounts.iter().any(|account| account.id == id);
        return if listed { Ok(id.to_string()) } else { Err(bad_account(id).into()) };
    }
    match accounts {
        [] => Err(fail("no_account", "Your Cloudflare sign-in lists no account to put the relay on.").into()),
        [only] => Ok(only.id.clone()),
        _ => Err(Halt::State(json!({
            "state": "needs_account",
            "next": "needs_account",
            "route": "cloudflare",
            "accounts": accounts.iter().map(|a| json!({ "id": a.id, "name": a.name })).collect::<Vec<_>>(),
            "template": format!("{again} --account ID"),
            "message": "Your Cloudflare sign-in has more than one account. Run the command with the id of the one the relay goes on.",
        }))),
    }
}

async fn asked(go: impl FnOnce(&Value) -> bool + Send + 'static, state: &Value) -> Result<bool, Failure> {
    let stop = stop_signal().map_err(|error| error.to_string())?;
    let (answer, answered) = tokio::sync::oneshot::channel();
    let state = state.clone();
    std::thread::spawn(move || answer.send(go(&state)));
    tokio::select! {
        yes = answered => Ok(yes.unwrap_or(false)),
        () = stop => Err(fail("stopped", "Setup was stopped. Nothing was changed.")),
    }
}

fn live_wait() -> Duration {
    let set = std::env::var("LS_RELAY_WAIT_MS").ok().and_then(|ms| ms.parse().ok());
    set.map_or(LIVE_WAIT, Duration::from_millis)
}

impl Tunnel {
    pub async fn setup_relay(
        &self,
        name: Option<String>,
        account: Option<String>,
        serve: Option<u16>,
        mcp: Option<u16>,
        go: impl FnOnce(&Value) -> bool + Send + 'static,
    ) -> Result<Value, Failure> {
        settle(self.setup_relay_steps(name, account, serve, mcp, go).await)
    }

    async fn setup_relay_steps(
        &self,
        name: Option<String>,
        account: Option<String>,
        serve: Option<u16>,
        mcp: Option<u16>,
        go: impl FnOnce(&Value) -> bool + Send + 'static,
    ) -> Result<Value, Halt> {
        if let Some(id) = account.as_ref().filter(|id| !account_id(id)) {
            return Err(bad_account(id).into());
        }
        let ports = self.relay_ports(serve, mcp)?;
        check_ports(ports.serve, ports.mcp)?;
        let given = [name.clone(), account.as_ref().map(|id| format!("--account {id}"))];
        let given_ports = [serve.map(|port| format!("--serve {port}")), mcp.map(|port| format!("--mcp {port}"))];
        let words = given.into_iter().chain(given_ports).flatten();
        let again = std::iter::once(AGAIN.to_string()).chain(words).collect::<Vec<_>>().join(" ");
        let unset = || format!("local-studio-{}", machine_slug());
        let _lock = UpLock::take(&self.dir, false)?;
        let old = self.on_relay()?;
        let name = name.or_else(|| old.as_ref().map(|old| old.name.clone())).unwrap_or_else(unset);
        if !worker_name(&name) {
            let text = format!(
                "{name} can't be a Worker's name: use 1 to 63 of a-z, 0-9 and -, not starting or ending with -."
            );
            return Err(fail("bad_name", text).into());
        }
        let account = account.or_else(|| old.as_ref().map(|old| old.account_id.clone()));
        if !old.as_ref().is_some_and(|old| old.name == name && account.as_ref() == Some(&old.account_id)) {
            self.refuse_for("relay")?;
        }
        Self::refuse_tychicus_tunnel(None)?;
        let manager = Manager::detect().await?;
        let Some(npx) = tool("LS_NPX", "npx").filter(|npx| npx.is_file()) else {
            let message = "The relay deploys with Cloudflare's wrangler through npx, and npx isn't on this machine. Install Node.js 22 or later, then run setup again.";
            return Ok(needs("needs_node", "url", NODE_DOWNLOAD, message));
        };
        let dir = self.relay_folder(true)?;
        let hide = old.iter().map(|old| old.key.clone()).collect();
        let mut wrangler = Wrangler { npx, dir, account: None, hide };
        let routed = self.route()?;
        let deployed = self.relay_deploy(&mut wrangler, &name, account.as_deref(), &again, old.as_ref(), go).await;
        let host = match deployed {
            Ok(host) => host,
            Err(halt) => {
                self.relay_folder(false)?;
                return Err(halt);
            }
        };
        let same = routed.as_ref().is_some_and(|route| route.hostname == host && route.ports == ports.into());
        let before = routed.map(|route| route.hostname).filter(|before| *before != host);
        let account_id = wrangler.account.clone().unwrap_or_default();
        let relay = json!({ "name": name, "accountId": account_id });
        let route = new_route("cloudflare", &host, &[], ports);
        let live = old.is_some_and(|old| old.name == name && old.account_id == account_id && old.host() == host);
        let key = match wrangler.put_key(&name).await {
            Err(failure) if live => {
                let message = format!("{} This machine's service and files are as they were, but the relay may already have the new key. If tunnel status says its key is refused, run setup again.", failure.message);
                return Err(fail(failure.code, message).into());
            }
            key => key,
        };
        let started = async {
            let key = key?;
            let record = Record { v: 1, url: format!("https://{host}"), name: name.clone(), account_id, key };
            let bytes = serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?;
            self.private_write(RECORD, &bytes)?;
            let stop = stop_signal().map_err(|error| error.to_string())?;
            let live = tokio::select! {
                live = self.start_relay(&manager, &host, ports) => live?,
                () = stop => return Err(fail("stopped", "Setup was stopped before the relay was live.")),
            };
            self.write_route(&route)?;
            Ok::<_, Failure>(live)
        };
        let started = started.await;
        let mut left = Vec::new();
        if let Some(before) = before {
            serve_disallow(&before, &mut left);
        }
        let (checks, refused) = match started {
            Ok(live) => live,
            Err(failure) if live && same && failure.code == "stopped" => {
                let message = match self.start_relay_service(&manager, ports).await {
                    Ok(_) => "Its service was started again with the new key. If tunnel status doesn't say it is connected, run setup again.".to_string(),
                    Err(error) => format!("Its service couldn't be started again ({}); run setup again.", error.message),
                };
                return Err(fail(failure.code, format!("{} {message}", failure.message)).into());
            }
            Err(failure) => return Err(self.relay_undo(&manager, &name, &host, failure, left).await.into()),
        };
        left.extend(refused.err());
        let message = with("Live on your own relay, a Worker on your own Cloudflare account.", &left);
        let mut value = ready(&route, &checks, &message);
        value["relay"] = relay;
        Ok(value)
    }

    async fn relay_undo(
        &self,
        manager: &Manager,
        name: &str,
        host: &str,
        failure: Failure,
        mut left: Vec<String>,
    ) -> Failure {
        serve_disallow(host, &mut left);
        let mut said = vec![failure.message];
        match manager.remove(OWN).await {
            Ok(had) => {
                let gone = if had { "The service was stopped and removed" } else { "No service is installed" };
                said.push(format!("{gone}, so nothing is exposed."));
                let route = match fs::remove_file(self.route_path()) {
                    Err(error) if error.kind() != ErrorKind::NotFound => Err(error.to_string()),
                    _ => Ok(()),
                };
                if let Err(error) = route.and_then(|()| self.remove_relay()) {
                    said.push(format!("Its files couldn't all be removed ({error}); run local-studio tunnel off."));
                }
                said.push(format!(
                    "The Worker {name} stays on your Cloudflare account, and this machine doesn't connect to it until setup runs again with --yes."
                ));
            }
            Err(error) => {
                said.push(format!("The service couldn't be removed ({error}); run local-studio tunnel off."));
            }
        }
        fail(failure.code, with(&said.join(" "), &left))
    }

    async fn relay_deploy(
        &self,
        wrangler: &mut Wrangler,
        name: &str,
        wanted: Option<&str>,
        again: &str,
        old: Option<&Record>,
        go: impl FnOnce(&Value) -> bool + Send + 'static,
    ) -> Result<String, Halt> {
        let Some(accounts) = wrangler.accounts().await? else {
            let command = format!("npx --yes {WRANGLER} login");
            let message = format!("Sign in to Cloudflare, or make a free account, first: run {command} in a terminal, sign in in the browser it opens, then run setup again.");
            return Err(Halt::State(needs("needs_signin", "nextCommand", &command, &message)));
        };
        let account = pick(&accounts, wanted, again)?;
        let ours = old.is_some_and(|old| old.name == name && old.account_id == account);
        wrangler.account = Some(account);
        if !ours && wrangler.exists(name).await? {
            let message = format!("Your Cloudflare account already has a Worker named {name}. Setup would replace its code and its relay key, so whatever uses it now stops working. Nothing was changed. If it is this machine's own, left by tunnel off or by a setup that didn't finish, run setup again with --yes. Otherwise give the relay another name, or use --yes to replace that Worker.");
            let state = needs("confirm", "nextCommand", &format!("{again} --yes"), &message);
            if !asked(go, &state).await? {
                return Err(Halt::State(state));
            }
        }
        wrangler.deploy(name).await
    }

    fn relay_folder(&self, fresh: bool) -> Result<PathBuf, String> {
        let dir = self.dir.join(FOLDER);
        let _ = fs::remove_dir_all(&dir);
        if fresh {
            self.ensure_dir()?;
            fs::DirBuilder::new().mode(0o700).create(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
            for (file, text) in [("relay.js", WORKER_JS), ("wrangler.jsonc", WORKER_CONFIG)] {
                let path = dir.join(file);
                fs::write(&path, text).map_err(|error| format!("{}: {error}", path.display()))?;
            }
        }
        Ok(dir)
    }

    async fn start_relay_service(&self, manager: &Manager, ports: Ports) -> Result<PathBuf, Failure> {
        let program = std::env::current_exe().and_then(fs::canonicalize).map_err(|error| error.to_string())?;
        let (serve, mcp) = (ports.serve.to_string(), ports.mcp.to_string());
        let args = argv(&["tunnel", "relay", "run", "--serve", &serve, "--mcp", &mcp]);
        let log = self.dir.join("tunnel-service.log");
        let env = match std::env::var_os("LOCAL_STUDIO_DATA_DIR").filter(|value| !value.is_empty()) {
            Some(dir) => {
                let dir = std::path::absolute(dir).map_err(|error| error.to_string())?;
                vec![("LOCAL_STUDIO_DATA_DIR", dir.display().to_string())]
            }
            None => Vec::new(),
        };
        let installed = match fs::remove_file(self.dir.join(STATE)) {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(error.to_string()),
            _ => manager.install(OWN, &program, &args, &env, &log).await,
        };
        installed.map_err(|error| fail("service_failed", error))?;
        Ok(log)
    }

    async fn start_relay(
        &self,
        manager: &Manager,
        host: &str,
        ports: Ports,
    ) -> Result<(Value, Result<(), String>), Failure> {
        let log = self.start_relay_service(manager, ports).await?;
        if !manager.wait_running(OWN).await {
            let text = format!("Local Studio's tunnel service didn't stay running. See {}.", log.display());
            return Err(fail("service_failed", text));
        }
        let allowed = serve_allow(host);
        let (total, live) = (live_wait(), |error: String| fail("going_live", error));
        let every = LIVE_EVERY.min(total / 5);
        let deadline = Instant::now() + total;
        let mut retaken = false;
        let dns = loop {
            let checked = self.check_once(host, Want { relay: true, ..Want::default() }).await;
            let mine = self.relay_seen().map_or_else(|| "connecting".to_string(), |seen| seen.state);
            let error = match checked {
                Ok(dns) if mine == "connected" => break dns,
                Ok(_) => format!("The relay answers, but this machine's own connection to it is {mine}."),
                Err(error) => error,
            };
            if Instant::now() + every > deadline {
                return Err(live(error));
            }
            tokio::time::sleep(every).await;
            if mine == "unauthorized" || (mine == "replaced" && !retaken) {
                retaken |= mine == "replaced";
                self.start_relay_service(manager, ports).await?;
            }
        };
        let hosts = [host.to_string()];
        let mut checks = self.public_checks(&hosts, ports, true, true).await.map_err(live)?;
        merge_dns(&mut checks, &json!({ "dns": dns }));
        checks["serveAllowed"] = json!(allowed.is_ok());
        Ok((checks, allowed))
    }
}
