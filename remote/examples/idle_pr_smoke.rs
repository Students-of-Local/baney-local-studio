use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use local_studio_core::link::snow::{self, TransportState};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

const CHANNEL: &str = env!("LOCAL_STUDIO_BUILD_CHANNEL");
const XX: &str = "Noise_XXpsk3_25519_ChaChaPoly_SHA256";
const PROLOGUE: &[u8] = b"local-studio-remote/1\n";
const BRANCHES: usize = 6;
const QUICK: Duration = Duration::from_secs(30);
const ASKED_AGAIN: Duration = Duration::from_secs(30);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (serve, scratch, idle) = match args.as_slice() {
        [serve, scratch] => (serve, scratch, 300),
        [serve, scratch, idle] if idle.parse::<u64>().is_ok() => (serve, scratch, idle.parse().unwrap_or(300)),
        _ => {
            eprintln!("usage: idle_pr_smoke <local-studio-serve> <new scratch dir> [idle seconds, default 300]");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            println!("FAIL idle_pr_smoke: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(Path::new(serve), Path::new(scratch), Duration::from_secs(idle))) {
        Ok(()) => {
            println!("PASS idle_pr_smoke");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("FAIL idle_pr_smoke: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn shims(scratch: &Path) -> Result<std::ffi::OsString> {
    let bin = scratch.join("bin");
    std::fs::create_dir_all(&bin)?;
    let git = which("git").context("git is not on PATH")?;
    let pulls = json!([
        { "number": 12, "title": "two", "url": "https://github.com/o/r/pull/12", "state": "OPEN", "headRefName": "thread-2",
          "baseRefName": "main", "statusCheckRollup": [{ "conclusion": "FAILURE", "status": "COMPLETED" }] },
        { "number": 11, "title": "one", "url": "https://github.com/o/r/pull/11", "state": "MERGED", "headRefName": "thread-1",
          "baseRefName": "main", "statusCheckRollup": [{ "conclusion": "SUCCESS", "status": "COMPLETED" }] },
    ]);
    std::fs::write(scratch.join("pulls.json"), pulls.to_string())?;
    let log = |name: &str| scratch.join(format!("{name}.log")).display().to_string();
    let scripts = [
        (
            "gh",
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncat '{}'\n",
                log("gh"),
                scratch.join("pulls.json").display()
            ),
        ),
        ("git", format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n", log("git"), git.display())),
    ];
    for (name, script) in scripts {
        std::fs::write(bin.join(name), script)?;
        std::fs::set_permissions(bin.join(name), std::fs::Permissions::from_mode(0o755))?;
    }
    let mut dirs = vec![bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    Ok(std::env::join_paths(dirs)?)
}

fn which(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(program)).find(|path| path.is_file())
}

fn lines(scratch: &Path, name: &str) -> Vec<String> {
    std::fs::read_to_string(scratch.join(format!("{name}.log")))
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn pr_calls(scratch: &Path) -> usize {
    lines(scratch, "gh").iter().filter(|line| line.starts_with("pr ")).count()
}

fn fetch_calls(scratch: &Path) -> usize {
    lines(scratch, "git").iter().filter(|line| line.split(' ').any(|word| word == "fetch")).count()
}

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git").args(args).current_dir(dir).status().with_context(|| format!("git {args:?}"))?;
    ensure!(status.success(), "git {args:?} failed in {}", dir.display());
    Ok(())
}

fn project(scratch: &Path, data: &Path) -> Result<()> {
    let origin = scratch.join("origin.git");
    let repo = scratch.join("repo");
    std::fs::create_dir_all(&repo)?;
    git(scratch, &["init", "--quiet", "--bare", &origin.display().to_string()])?;
    git(&repo, &["init", "--quiet", "--initial-branch", "main"])?;
    std::fs::write(repo.join("README.md"), "idle\n")?;
    git(&repo, &["add", "README.md"])?;
    git(&repo, &["-c", "user.name=Smoke", "-c", "user.email=smoke@localhost", "commit", "--quiet", "-m", "start"])?;
    git(&repo, &["remote", "add", "origin", &origin.display().to_string()])?;
    git(&repo, &["push", "--quiet", "--set-upstream", "origin", "main"])?;
    let mut threads = serde_json::Map::new();
    for index in 1..=BRANCHES {
        let worktree = scratch.join("worktrees").join(format!("thread-{index}"));
        git(&repo, &["worktree", "add", "--quiet", "-b", &format!("thread-{index}"), &worktree.display().to_string()])?;
        threads.insert(format!("t{index}"), json!({ "harness": "claudeCode", "cwd": worktree, "project": repo }));
    }
    let state = json!({ "version": 1, "projects": [{ "path": repo, "name": "repo" }], "threads": threads });
    std::fs::write(data.join("state.json"), state.to_string())?;
    Ok(())
}

fn command(serve: &Path, scratch: &Path, path: &std::ffi::OsString) -> Command {
    let mut command = Command::new(serve);
    command
        .env("HOME", scratch.join("home"))
        .env("PATH", path)
        .env("EUKHE_CODING_AGENT_DIR", scratch.join("agent"))
        .env("LOCAL_STUDIO_NO_UPDATE_CHECK", "1")
        .env_remove("LOCAL_STUDIO_DATA_DIR")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN");
    command
}

struct Serve(Child);

impl Drop for Serve {
    fn drop(&mut self) {
        if let Err(error) = Command::new("kill").args(["-TERM", &self.0.id().to_string()]).status() {
            eprintln!("could not stop serve: {error}");
        }
        if let Err(error) = self.0.wait() {
            eprintln!("could not reap serve: {error}");
        }
    }
}

async fn run(serve_bin: &Path, scratch: &Path, idle: Duration) -> Result<()> {
    std::fs::create_dir(scratch).with_context(|| format!("create {} (it must not exist)", scratch.display()))?;
    let scratch = scratch.canonicalize()?;
    let data = scratch.join("data");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(scratch.join("home"))?;
    project(&scratch, &data)?;
    let path = shims(&scratch)?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let data_arg = data.display().to_string();
    let log = std::fs::File::create(scratch.join("serve.log"))?;
    let child = command(serve_bin, &scratch, &path)
        .args(["serve", "--port", &port.to_string(), "--name", "idle", "--data-dir", &data_arg])
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .context("start local-studio-serve")?;
    let _serve = Serve(child);
    let deadline = Instant::now() + QUICK;
    while tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
        ensure!(Instant::now() < deadline, "serve did not listen on {port}; see {}/serve.log", scratch.display());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    tokio::time::sleep(idle).await;
    let idle_calls = pr_calls(&scratch);
    println!(
        "     idle {} s with no client: {idle_calls} PR-status gh calls, {} git calls",
        idle.as_secs(),
        lines(&scratch, "git").len()
    );
    ensure!(idle_calls == 0, "serve made PR-status gh calls with no client: {:?}", lines(&scratch, "gh"));
    println!("ok   idle serve with no client made no PR-status gh call");
    ensure!(fetch_calls(&scratch) == 0, "serve ran git fetch with no client: {:?}", lines(&scratch, "git"));
    println!("ok   idle serve with no client ran no git fetch");

    let url = format!("ws://127.0.0.1:{port}/v1/ws");
    let minted = command(serve_bin, &scratch, &path)
        .args(["pair", "--json", "--url", &url, "--name", "idle", "--data-dir", &data_arg])
        .output()?;
    ensure!(minted.status.success(), "pair failed: {}", String::from_utf8_lossy(&minted.stderr));
    let minted: Value = serde_json::from_slice(&minted.stdout)?;
    let link = minted["link"].as_str().context("pair printed no link")?;
    let threads = Arc::new(Mutex::new(Vec::<Value>::new()));
    let attached = Instant::now();
    let _phone = pair_phone(port, link, Arc::clone(&threads)).await?;
    let chips = || -> Vec<String> {
        let rows = threads.lock().unwrap_or_else(PoisonError::into_inner);
        let mut chips: Vec<String> = rows
            .iter()
            .map(|row| {
                let pull = &row["pullRequest"];
                format!("{} {} {} {}", row["id"].as_str().unwrap_or("?"), pull["number"], pull["state"], pull["checks"])
            })
            .collect();
        chips.sort();
        chips
    };
    let expected = [
        r#"t1 11 "MERGED" "passing""#,
        r#"t2 12 "OPEN" "failing""#,
        "t3 null null null",
        "t4 null null null",
        "t5 null null null",
        "t6 null null null",
    ];
    let deadline = Instant::now() + QUICK;
    while chips() != expected {
        ensure!(Instant::now() < deadline, "the phone never saw the chips; it has {:?}", chips());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    ensure!(
        pr_calls(&scratch) == 1,
        "a client attaching took {} gh calls: {:?}",
        pr_calls(&scratch),
        lines(&scratch, "gh")
    );
    println!("ok   a phone attaching made 1 gh call and its thread list shows the chips from it");
    tokio::time::sleep(ASKED_AGAIN).await;
    ensure!(pr_calls(&scratch) == 1, "within 120 s of the first call gh ran {} times", pr_calls(&scratch));
    println!("ok   no second call within {} s while the phone stays attached", ASKED_AGAIN.as_secs());
    let deadline = attached + Duration::from_secs(45);
    while fetch_calls(&scratch) == 0 {
        ensure!(Instant::now() < deadline, "serve ran no git fetch in 45 s with a phone attached");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!("ok   with a phone attached the project fetch runs again");
    Ok(())
}

type Sink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;

fn noise_write(state: &mut snow::HandshakeState, payload: &[u8]) -> Result<Vec<u8>> {
    let mut message = vec![0u8; 65_535];
    let length = state.write_message(payload, &mut message)?;
    message.truncate(length);
    Ok(message)
}

async fn pair_phone(port: u16, link: &str, threads: Arc<Mutex<Vec<Value>>>) -> Result<Sink> {
    let (query, secret) = link.split_once('#').context("the link has no secret")?;
    let pairing = query.split(['?', '&']).find_map(|part| part.strip_prefix("id=")).context("the link has no id")?;
    let psk = <[u8; 32]>::try_from(URL_SAFE_NO_PAD.decode(secret)?)
        .map_err(|_| anyhow::anyhow!("the pairing secret is not 32 bytes"))?;
    let keys = snow::Builder::new(XX.parse()?).generate_keypair()?;
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/v1/ws")).await?;
    let header = json!({ "v": 1, "pattern": "XXpsk3", "pair": pairing, "protocol": 2, "min_protocol": 2 }).to_string();
    socket.send(Message::text(header.clone())).await?;
    let mut prologue = PROLOGUE.to_vec();
    prologue.extend_from_slice(header.as_bytes());
    let mut state = snow::Builder::new(XX.parse()?)
        .local_private_key(&keys.private)?
        .psk(3, &psk)?
        .prologue(&prologue)?
        .build_initiator()?;
    socket.send(Message::binary(noise_write(&mut state, &[])?)).await?;
    let second = loop {
        match tokio::time::timeout(QUICK, socket.next()).await.context("no pairing reply in time")? {
            Some(Ok(Message::Binary(bytes))) => break bytes,
            Some(Ok(Message::Close(frame))) => bail!("serve closed with {frame:?} during pairing"),
            None | Some(Err(_)) => bail!("serve dropped the socket during pairing"),
            Some(Ok(_)) => {}
        }
    };
    let mut scratch = vec![0u8; 65_535];
    state.read_message(&second, &mut scratch)?;
    let record =
        json!({ "id": "idle-phone", "name": "idle phone", "platform": "linux", "app": "0.4.0", "channel": CHANNEL });
    socket.send(Message::binary(noise_write(&mut state, record.to_string().as_bytes())?)).await?;
    let mut transport = state.into_transport_mode()?;
    let (mut sink, stream) = socket.split();
    let mut hello = vec![1u8];
    hello.extend_from_slice(
        json!({ "op": "hello", "device": { "name": "idle phone", "app": "0.4.0", "channel": CHANNEL } })
            .to_string()
            .as_bytes(),
    );
    let mut sealed = vec![0u8; hello.len() + 16];
    let length = transport.write_message(&hello, &mut sealed)?;
    sealed.truncate(length);
    sink.send(Message::binary(sealed)).await?;
    tokio::spawn(read(stream, transport, threads));
    Ok(sink)
}

async fn read(
    mut stream: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    >,
    mut transport: TransportState,
    threads: Arc<Mutex<Vec<Value>>>,
) {
    let mut joined = Vec::new();
    while let Some(Ok(message)) = stream.next().await {
        let Message::Binary(bytes) = message else { continue };
        let mut record = vec![0u8; bytes.len()];
        let Ok(length) = transport.read_message(&bytes, &mut record) else {
            eprintln!("phone: a record failed to decrypt");
            return;
        };
        record.truncate(length);
        match record.first() {
            Some(1) => joined.extend_from_slice(&record[1..]),
            Some(2) => {
                joined.extend_from_slice(&record[1..]);
                continue;
            }
            _ => continue,
        }
        let Ok(frame) = serde_json::from_slice::<Value>(&std::mem::take(&mut joined)) else { continue };
        let updates = match frame["op"].as_str() {
            Some("hello") => frame["snapshot"].as_array().cloned().unwrap_or_default(),
            Some("update") => vec![frame["update"].clone()],
            _ => Vec::new(),
        };
        for update in updates.into_iter().filter(|update| update["type"] == "threads") {
            *threads.lock().unwrap_or_else(PoisonError::into_inner) =
                update["threads"].as_array().cloned().unwrap_or_default();
        }
    }
}
