use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use blake2::Blake2b512;
use ed25519_dalek::{Signer, SigningKey};
use local_studio_core::update::{self, Build, Checked, Offer, Refusal, Staged};
use sha2::{Digest, Sha256};

const V0: &str = "0.1.0-nightly.20261121000000+aaaaaaa";
const V1: &str = "0.1.0-nightly.20261122000000+bbbbbbb";
const V2: &str = "0.1.0-nightly.20261123000000+ccccccc";
const V3: &str = "0.1.0-nightly.20261124000000+ddddddd";
const OLDER: &str = "0.1.0-nightly.20261120000000+0000000";
const NAME: &str = "Local Studio Nightly";
const BUNDLE_ID: &str = "org.local.studio.macos.nightly";
const IDENTITY: &str = "Local Studio Smoke Signing";

type Outcome = Result<(), String>;

struct Key {
    id: [u8; 8],
    signing: SigningKey,
}

impl Key {
    fn generate() -> Result<Self, String> {
        let mut seed = [0u8; 32];
        let mut id = [0u8; 8];
        getrandom::getrandom(&mut seed).map_err(|error| error.to_string())?;
        getrandom::getrandom(&mut id).map_err(|error| error.to_string())?;
        Ok(Self { id, signing: SigningKey::from_bytes(&seed) })
    }

    fn public(&self) -> String {
        let mut bytes = b"Ed".to_vec();
        bytes.extend_from_slice(&self.id);
        bytes.extend_from_slice(self.signing.verifying_key().as_bytes());
        let id = self.id.iter().rev().fold(String::new(), |mut out, byte| {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{byte:02X}"));
            out
        });
        let file = format!("untrusted comment: minisign public key: {id}\n{}\n", STANDARD.encode(bytes));
        STANDARD.encode(file)
    }

    fn sign(&self, data: &[u8], file_name: &str) -> String {
        let digest = Blake2b512::digest(data);
        let signature = self.signing.sign(&digest).to_bytes();
        let mut line = b"ED".to_vec();
        line.extend_from_slice(&self.id);
        line.extend_from_slice(&signature);
        let trusted = format!("timestamp:1795300000\tfile:{file_name}");
        let mut global = signature.to_vec();
        global.extend_from_slice(trusted.as_bytes());
        let global = self.signing.sign(&global).to_bytes();
        let file = format!(
            "untrusted comment: signature from tauri secret key\n{}\ntrusted comment: {trusted}\n{}\n",
            STANDARD.encode(line),
            STANDARD.encode(global)
        );
        STANDARD.encode(file)
    }
}

struct Smoke {
    root: PathBuf,
    web: PathBuf,
    base: String,
    hits: Arc<AtomicUsize>,
    key: Key,
    other: Key,
    platform: &'static str,
    asset: String,
    runtime: tokio::runtime::Runtime,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [scratch] = args.as_slice() else {
        eprintln!("usage: update_smoke <scratch dir>");
        return ExitCode::from(2);
    };
    match run(Path::new(scratch)) {
        Ok(()) => {
            println!("PASS update smoke");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("FAIL update smoke: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(scratch: &Path) -> Outcome {
    let marker = scratch.join(".update_smoke");
    let empty = std::fs::read_dir(scratch).map_or(true, |mut entries| entries.next().is_none());
    if !empty && !marker.is_file() {
        return Err(format!(
            "{} is not empty and was not made by update_smoke; give an empty or new folder",
            scratch.display()
        ));
    }
    if scratch.exists() {
        std::fs::remove_dir_all(scratch).map_err(|error| format!("clear {}: {error}", scratch.display()))?;
    }
    std::fs::create_dir_all(scratch).map_err(|error| error.to_string())?;
    write(&marker, "")?;
    let root = scratch.canonicalize().map_err(|error| error.to_string())?;
    let web = root.join("web");
    std::fs::create_dir_all(&web).map_err(|error| error.to_string())?;
    let (base, hits) = serve(&web)?;
    let platform = update::platform();
    let mut smoke = Smoke {
        root,
        web,
        base,
        hits,
        key: Key::generate()?,
        other: Key::generate()?,
        platform,
        asset: String::new(),
        runtime: tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?,
    };
    smoke.asset = smoke.build(V0).asset();
    update::minisign::public_key(&smoke.key.public())?;
    println!("feed on {} for {platform}", smoke.base);
    if cfg!(target_os = "macos") {
        smoke.make_identity()?;
    }
    step("a local build never updates", smoke.local_build())?;
    step("another channel's feed is refused", smoke.other_channel())?;
    step("an older or equal version is not offered", smoke.not_newer())?;
    step("a bad sha256 is refused and nothing stays staged", smoke.bad_sha())?;
    step("a bad signature is refused", smoke.bad_signature())?;
    let staged = smoke.newer()?;
    println!("ok: a newer signed build is offered, downloaded and staged ({})", staged.artifact.display());
    step("scripts/install.sh installs only a verified build", smoke.install_script())?;
    step("the Tauri signer's own output verifies", smoke.tauri_signer())?;
    let target = smoke.install(V0)?;
    step(
        "a second apply while one holds the update is refused and touches nothing",
        smoke.apply_busy(&staged, &target),
    )?;
    step("apply swaps in the new build and keeps the previous one", smoke.apply_good(&staged, &target))?;
    if cfg!(target_os = "macos") {
        step("a build signed by another identity is refused", smoke.other_identity(&target))?;
    }
    step("a new build that fails after the swap is rolled back", smoke.rollback(&target))?;
    Ok(())
}

fn step(name: &str, outcome: Outcome) -> Outcome {
    outcome.map_err(|error| format!("{name}: {error}"))?;
    println!("ok: {name}");
    Ok(())
}

fn serve(web: &Path) -> Result<(String, Arc<AtomicUsize>), String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    let port = listener.local_addr().map_err(|error| error.to_string())?.port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let web = web.to_path_buf();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut line = String::new();
            let Ok(clone) = stream.try_clone() else { continue };
            let mut reader = BufReader::new(clone);
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).map_or(true, |read| read == 0) || header.trim().is_empty() {
                    break;
                }
            }
            counter.fetch_add(1, Ordering::SeqCst);
            let path = line.split_whitespace().nth(1).unwrap_or("/").trim_start_matches('/').to_string();
            let file = web.join(&path);
            let reply = if !path.contains("..") && file.is_file() { std::fs::read(&file).ok() } else { None };
            let _ = match reply {
                Some(body) => stream
                    .write_all(
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
                            .as_bytes(),
                    )
                    .and_then(|()| stream.write_all(&body)),
                None => stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
            };
        }
    });
    Ok((format!("http://127.0.0.1:{port}"), hits))
}

fn sh(command: &mut Command) -> Result<String, String> {
    let output = command.output().map_err(|error| format!("{command:?}: {error}"))?;
    if !output.status.success() {
        return Err(format!("{command:?} failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn write(path: &Path, text: &str) -> Outcome {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(path, text).map_err(|error| format!("write {}: {error}", path.display()))
}

fn executable(path: &Path, text: &str) -> Outcome {
    write(path, text)?;
    sh(Command::new("chmod").arg("755").arg(path)).map(drop)
}

fn version_script(version: &str, broken_when_moved: bool) -> String {
    let guard = if broken_when_moved { "case \"$0\" in */unpacked/*) ;; *) exit 1 ;; esac\n" } else { "" };
    format!("#!/bin/sh\n{guard}[ \"$1\" = --version ] && echo \"local-studio {version} (nightly)\"\nexit 0\n")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{byte:02x}"));
        out
    })
}

fn expect(result: Result<impl std::fmt::Debug, Refusal>, want: impl Fn(&Refusal) -> bool) -> Outcome {
    match result {
        Err(refusal) if want(&refusal) => {
            println!("   refused as expected: {refusal}");
            Ok(())
        }
        Err(refusal) => Err(format!("refused for the wrong reason: {refusal:?}")),
        Ok(value) => Err(format!("was accepted: {value:?}")),
    }
}

impl Smoke {
    fn build(&self, version: &str) -> Build {
        Build {
            version: version.to_string(),
            channel: "nightly".to_string(),
            platform: self.platform.to_string(),
            public_key: self.key.public(),
            feed: format!("{}/nightly/latest.json", self.base),
        }
    }

    fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    fn check(&self, build: &Build) -> Result<Checked, Refusal> {
        self.runtime.block_on(update::check(build))
    }

    fn download(&self, build: &Build, offer: &Offer) -> Result<Staged, Refusal> {
        self.runtime.block_on(update::download(build, offer, &self.data_dir(), |_, _| {}))
    }

    fn publish_feed(&self, value: &serde_json::Value) -> Outcome {
        write(&self.web.join("nightly").join("latest.json"), &value.to_string())
    }

    fn publish(
        &self,
        case: &str,
        version: &str,
        artifact: &[u8],
        signature: Option<String>,
        sha: Option<String>,
    ) -> Outcome {
        let dir = self.web.join("files").join(case);
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        std::fs::write(dir.join(&self.asset), artifact).map_err(|error| error.to_string())?;
        let mut entry = serde_json::json!({
            "url": format!("{}/files/{case}/{}", self.base, self.asset),
            "sha256": sha.unwrap_or_else(|| hex(&Sha256::digest(artifact))),
        });
        if let Some(signature) = signature {
            entry["signature"] = serde_json::Value::String(signature);
        }
        let mut platforms = serde_json::Map::new();
        platforms.insert(self.platform.to_string(), entry);
        self.publish_feed(&serde_json::json!({
            "product": "local-studio",
            "channel": "nightly",
            "version": version,
            "commit": "0000000000000000000000000000000000000000",
            "pub_date": "2026-11-22T07:00:00Z",
            "notes": format!("Smoke build {version}."),
            "protocol": 1,
            "platforms": platforms,
        }))
    }

    fn offered(&self, build: &Build) -> Result<Offer, String> {
        match self.check(build) {
            Ok(Checked::Available(offer)) => Ok(offer),
            other => Err(format!("expected an offer, got {other:?}")),
        }
    }

    fn local_build(&self) -> Outcome {
        let mut build = self.build("0.1.0-dev.0+local.1234567");
        build.channel = "dev".to_string();
        let before = self.hits.load(Ordering::SeqCst);
        expect(self.check(&build), |refusal| *refusal == Refusal::LocalBuild)?;
        let staged = Staged {
            version: V1.to_string(),
            notes: String::new(),
            artifact: self.root.join("none"),
            dir: self.root.join("none-dir"),
        };
        expect(update::apply(&build, &staged, &self.root), |refusal| *refusal == Refusal::LocalBuild)?;
        if self.hits.load(Ordering::SeqCst) != before {
            return Err("a local build reached the feed".to_string());
        }
        Ok(())
    }

    fn other_channel(&self) -> Outcome {
        let build = self.build(V0);
        self.publish_feed(
            &serde_json::json!({"product": "local-studio", "channel": "stable", "version": "0.2.0", "platforms": {}}),
        )?;
        expect(
            self.check(&build),
            |refusal| matches!(refusal, Refusal::OtherChannel { feed, .. } if feed == "stable"),
        )?;
        self.publish_feed(&serde_json::json!({"product": "local-studio", "channel": "nightly", "version": "0.2.0-dev.20261122000000+abcdef0", "platforms": {}}))?;
        expect(self.check(&build), |refusal| matches!(refusal, Refusal::OtherChannel { feed, .. } if feed == "dev"))?;
        let mut dev = self.build("0.1.0-dev.20261121000000+aaaaaaa");
        dev.channel = "dev".to_string();
        self.publish("channel", V1, b"x", Some(self.key.sign(b"x", &self.asset)), None)?;
        expect(self.check(&dev), |refusal| matches!(refusal, Refusal::OtherChannel { feed, .. } if feed == "nightly"))
    }

    fn not_newer(&self) -> Outcome {
        let build = self.build(V0);
        for version in [OLDER, V0, "0.1.0-nightly.20261121000000+fffffff"] {
            self.publish("old", version, b"old", Some(self.key.sign(b"old", &self.asset)), None)?;
            match self.check(&build) {
                Ok(Checked::UpToDate { feed }) if feed == version => {}
                other => return Err(format!("{version} against {V0}: {other:?}")),
            }
        }
        let offer = Offer {
            version: OLDER.to_string(),
            notes: String::new(),
            url: format!("{}/files/old/{}", self.base, self.asset),
            sha256: hex(&Sha256::digest(b"old")),
            signature: self.key.sign(b"old", &self.asset),
            asset: self.asset.clone(),
        };
        expect(self.download(&build, &offer), |refusal| matches!(refusal, Refusal::NotNewer { .. }))
    }

    fn bad_sha(&self) -> Outcome {
        let build = self.build(V0);
        let body = b"the real artifact".to_vec();
        self.publish(
            "sha",
            V1,
            &body,
            Some(self.key.sign(&body, &self.asset)),
            Some(hex(&Sha256::digest(b"something else"))),
        )?;
        let offer = self.offered(&build)?;
        expect(self.download(&build, &offer), |refusal| matches!(refusal, Refusal::BadSha256 { .. }))?;
        if update::staged(&build, &self.data_dir()).is_some() {
            return Err("a download with a bad sha256 stayed staged".to_string());
        }
        let slot = update::staging_dir(&self.data_dir(), "nightly");
        if std::fs::read_dir(&slot).map_or(0, Iterator::count) != 0 {
            return Err(format!("{} still holds files", slot.display()));
        }
        Ok(())
    }

    fn bad_signature(&self) -> Outcome {
        let build = self.build(V0);
        let body = b"signed by someone else".to_vec();
        self.publish("other-key", V1, &body, Some(self.other.sign(&body, &self.asset)), None)?;
        let offer = self.offered(&build)?;
        expect(self.download(&build, &offer), |refusal| matches!(refusal, Refusal::BadSignature(_)))?;
        self.publish("tampered", V1, b"tampered bytes", Some(self.key.sign(b"original bytes", &self.asset)), None)?;
        let offer = self.offered(&build)?;
        expect(self.download(&build, &offer), |refusal| matches!(refusal, Refusal::BadSignature(_)))?;
        self.publish("wrong-file", V1, &body, Some(self.key.sign(&body, "local-studio-windows-x86_64.zip")), None)?;
        let offer = self.offered(&build)?;
        expect(self.download(&build, &offer), |refusal| matches!(refusal, Refusal::BadSignature(_)))?;
        self.publish("unsigned", V1, &body, None, None)?;
        expect(self.check(&build), |refusal| matches!(refusal, Refusal::BadSignature(_)))?;
        if update::staged(&build, &self.data_dir()).is_some() {
            return Err("a refused download stayed staged".to_string());
        }
        Ok(())
    }

    fn artifact(&self, version: &str, kind: Kind) -> Result<Vec<u8>, String> {
        let source = self.root.join("src").join(format!("{version}-{kind:?}"));
        let _ = std::fs::remove_dir_all(&source);
        let entry = self.bundle(&source, version, kind)?;
        let out = source.with_extension("tar.gz");
        sh(Command::new("tar").arg("-czf").arg(&out).arg("-C").arg(&source).arg(&entry))?;
        std::fs::read(&out).map_err(|error| error.to_string())
    }

    fn bundle(&self, dir: &Path, version: &str, kind: Kind) -> Result<String, String> {
        let broken = kind == Kind::BreaksWhenMoved;
        if cfg!(target_os = "macos") {
            let app = dir.join(format!("{NAME}.app"));
            let plist = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>{BUNDLE_ID}</string><key>CFBundleExecutable</key><string>LocalStudio</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleName</key><string>{NAME}</string><key>LocalStudioVersion</key><string>{version}</string><key>LocalStudioChannel</key><string>nightly</string></dict></plist>\n"
            );
            write(&app.join("Contents").join("Info.plist"), &plist)?;
            executable(&app.join("Contents").join("MacOS").join("LocalStudio"), &version_script(version, broken))?;
            executable(&app.join("Contents").join("MacOS").join("eukhe"), "#!/bin/sh\nexit 0\n")?;
            if kind == Kind::AdHoc {
                sh(Command::new("/usr/bin/codesign").args(["--force", "--deep", "--sign", "-"]).arg(&app))?;
            } else {
                self.codesign(&app)?;
            }
            Ok(format!("{NAME}.app"))
        } else {
            let bundle = dir.join("local-studio");
            executable(&bundle.join("local-studio"), &version_script(version, broken))?;
            executable(&bundle.join("eukhe"), "#!/bin/sh\nexit 0\n")?;
            Ok("local-studio".to_string())
        }
    }

    fn keychain(&self) -> PathBuf {
        self.root.join("signing").join("smoke.keychain-db")
    }

    fn security(&self) -> Command {
        let mut command = Command::new("/usr/bin/security");
        command.env("HOME", self.root.join("signing").join("home"));
        command
    }

    fn make_identity(&self) -> Outcome {
        let dir = self.root.join("signing");
        std::fs::create_dir_all(dir.join("home")).map_err(|error| error.to_string())?;
        let before = sh(Command::new("/usr/bin/security").args(["list-keychains", "-d", "user"]))?;
        write(
            &dir.join("cert.cnf"),
            &format!("[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN={IDENTITY}\n[ext]\nbasicConstraints=critical,CA:false\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=critical,codeSigning\n"),
        )?;
        sh(Command::new("openssl")
            .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2", "-keyout"])
            .arg(dir.join("key.pem"))
            .arg("-out")
            .arg(dir.join("cert.pem"))
            .arg("-config")
            .arg(dir.join("cert.cnf")))?;
        let export = |legacy: bool| {
            let mut command = Command::new("openssl");
            command.args(["pkcs12", "-export", "-passout", "pass:smoke", "-inkey"]).arg(dir.join("key.pem"));
            command.arg("-in").arg(dir.join("cert.pem")).arg("-out").arg(dir.join("id.p12"));
            if legacy {
                command.arg("-legacy");
            }
            sh(&mut command)
        };
        export(true).or_else(|_| export(false))?;
        let keychain = self.keychain();
        sh(self.security().args(["create-keychain", "-p", "smoke"]).arg(&keychain))?;
        sh(self.security().args(["unlock-keychain", "-p", "smoke"]).arg(&keychain))?;
        sh(self.security().arg("import").arg(dir.join("id.p12")).arg("-k").arg(&keychain).args([
            "-P",
            "smoke",
            "-T",
            "/usr/bin/codesign",
        ]))?;
        sh(self
            .security()
            .args(["set-key-partition-list", "-S", "apple-tool:,apple:", "-s", "-k", "smoke"])
            .arg(&keychain))?;
        let after = sh(Command::new("/usr/bin/security").args(["list-keychains", "-d", "user"]))?;
        if before != after {
            return Err("making the scratch keychain changed the real keychain search list".to_string());
        }
        Ok(())
    }

    fn codesign(&self, app: &Path) -> Outcome {
        for code in [app.join("Contents").join("MacOS").join("eukhe"), app.to_path_buf()] {
            sh(Command::new("/usr/bin/codesign")
                .env("HOME", self.root.join("signing").join("home"))
                .args(["--force", "--options", "runtime", "--keychain"])
                .arg(self.keychain())
                .args(["--sign", IDENTITY])
                .arg(code))?;
        }
        Ok(())
    }

    fn newer(&self) -> Result<Staged, String> {
        let build = self.build(V0);
        let body = self.artifact(V1, Kind::Good)?;
        self.publish("good", V1, &body, Some(self.key.sign(&body, &self.asset)), None)?;
        let offer = self.offered(&build)?;
        if offer.version != V1 || offer.notes != format!("Smoke build {V1}.") {
            return Err(format!("offered {offer:?}"));
        }
        let staged = self.download(&build, &offer).map_err(|refusal| format!("download: {refusal}"))?;
        let again = update::staged(&build, &self.data_dir())
            .ok_or_else(|| "nothing staged after a good download".to_string())?;
        if again != staged || staged.version != V1 {
            return Err(format!("staged {again:?}, downloaded {staged:?}"));
        }
        if std::fs::read(&staged.artifact).map_err(|error| error.to_string())? != body {
            return Err("the staged artifact is not the published one".to_string());
        }
        if !staged.artifact.starts_with(update::staging_dir(&self.data_dir(), "nightly")) {
            return Err(format!("staged outside the data dir: {}", staged.artifact.display()));
        }
        Ok(staged)
    }

    fn run_install_script(&self, prefix: &Path, verifier: &str) -> Result<(bool, String), String> {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("scripts").join("install.sh");
        let output = Command::new("bash")
            .arg(script)
            .args(["--channel", "nightly", "--no-open", "--feed"])
            .arg(format!("{}/nightly/latest.json", self.base))
            .arg("--public-key")
            .arg(self.key.public())
            .arg("--prefix")
            .arg(prefix)
            .env("LOCAL_STUDIO_INSTALL_VERIFIER", verifier)
            .env("TMPDIR", self.root.join("tmp"))
            .output()
            .map_err(|error| format!("install.sh: {error}"))?;
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        Ok((output.status.success(), text.trim().to_string()))
    }

    fn installed_by_script(prefix: &Path) -> PathBuf {
        if cfg!(target_os = "macos") {
            prefix.join("Applications").join(format!("{NAME}.app"))
        } else {
            prefix.join("lib").join("local-studio-nightly")
        }
    }

    fn install_script(&self) -> Outcome {
        std::fs::create_dir_all(self.root.join("tmp")).map_err(|error| error.to_string())?;
        let body = self.artifact(V1, Kind::Good)?;
        self.publish("install", V1, &body, Some(self.key.sign(&body, &self.asset)), None)?;
        let mut ran = 0;
        for verifier in ["minisign", "openssl", "python3"] {
            let prefix = self.root.join(format!("install-{verifier}"));
            let (ok, text) = self.run_install_script(&prefix, verifier)?;
            if !ok && text.ends_with("is not installed") {
                println!("   SKIP: {verifier} is not installed here");
                continue;
            }
            if !ok {
                return Err(format!("install.sh with {verifier} failed: {text}"));
            }
            let target = Self::installed_by_script(&prefix);
            let reports = Self::reports(&target)?;
            if reports != format!("local-studio {V1} (nightly)") {
                return Err(format!("install.sh with {verifier} installed a build that reports {reports}"));
            }
            if !cfg!(target_os = "macos") {
                for path in [
                    prefix.join("bin").join("local-studio-nightly"),
                    prefix.join("share").join("applications").join("dev.localstudio.LocalStudio.Nightly.desktop"),
                ] {
                    if !path.exists() {
                        return Err(format!("install.sh did not create {}", path.display()));
                    }
                }
            }
            println!("   installed with {verifier}: {}", target.display());
            ran += 1;
        }
        if ran == 0 {
            return Err("no signature verifier (minisign, OpenSSL 3, python3) is available to install.sh".to_string());
        }
        let refusals = [
            ("install-other-key", Some(self.other.sign(&body, &self.asset)), None, "signed with another key"),
            ("install-tampered", Some(self.key.sign(b"another file", &self.asset)), None, "does not verify"),
            ("install-bad-sha", Some(self.key.sign(&body, &self.asset)), Some("0".repeat(64)), "sha256"),
            ("install-unsigned", None, None, "no signature"),
        ];
        for (case, signature, sha, want) in refusals {
            self.publish(case, V1, &body, signature, sha)?;
            let prefix = self.root.join(case);
            let (ok, text) = self.run_install_script(&prefix, "")?;
            if ok || !text.contains(want) {
                return Err(format!("install.sh for {case} gave ok={ok}: {text}"));
            }
            if Self::installed_by_script(&prefix).exists() {
                return Err(format!("install.sh for {case} left an install behind"));
            }
            println!("   refused as expected: {}", text.lines().last().unwrap_or_default());
        }
        Ok(())
    }

    fn tauri_signer(&self) -> Outcome {
        if sh(Command::new("npx").arg("--version")).is_err() {
            println!("   SKIP: npx is not on PATH, so the Tauri signer cross-check did not run");
            return Ok(());
        }
        let dir = self.root.join("tauri");
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let key = dir.join("tauri.key");
        sh(Command::new("npx")
            .args(["-y", "@tauri-apps/cli@2.12.5", "signer", "generate", "-p", "smoke", "--ci", "-w"])
            .arg(&key))?;
        let file = dir.join(&self.asset);
        let body = b"signed by the Tauri CLI".to_vec();
        std::fs::write(&file, &body).map_err(|error| error.to_string())?;
        sh(Command::new("npx")
            .args(["-y", "@tauri-apps/cli@2.12.5", "signer", "sign", "-f"])
            .arg(&key)
            .env("TAURI_SIGNING_PRIVATE_KEY_PASSWORD", "smoke")
            .arg(&file))?;
        let signature =
            std::fs::read_to_string(dir.join(format!("{}.sig", self.asset))).map_err(|error| error.to_string())?;
        let public = std::fs::read_to_string(dir.join("tauri.key.pub")).map_err(|error| error.to_string())?;
        let sha = hex(&Sha256::digest(&body));
        let mut build = self.build(V0);
        build.public_key = public;
        update::verify_file(&build, &file, &sha, &signature, &self.asset).map_err(|refusal| refusal.to_string())?;
        let mine = self.build(V0);
        expect(update::verify_file(&mine, &file, &sha, &signature, &self.asset), |refusal| {
            matches!(refusal, Refusal::BadSignature(_))
        })
    }

    fn install(&self, version: &str) -> Result<PathBuf, String> {
        let parent = if cfg!(target_os = "macos") { self.root.join("Applications") } else { self.root.join("lib") };
        std::fs::create_dir_all(&parent).map_err(|error| error.to_string())?;
        let entry = self.bundle(&parent, version, Kind::Good)?;
        let target = parent.join(if cfg!(target_os = "macos") {
            format!("{NAME}.app")
        } else {
            "local-studio-nightly".to_string()
        });
        if entry != "local-studio-nightly" && !cfg!(target_os = "macos") {
            std::fs::rename(parent.join(&entry), &target).map_err(|error| error.to_string())?;
        }
        Ok(target)
    }

    fn reports(target: &Path) -> Result<String, String> {
        let program = if cfg!(target_os = "macos") {
            target.join("Contents").join("MacOS").join("LocalStudio")
        } else {
            target.join("local-studio")
        };
        Ok(sh(Command::new(program).arg("--version"))?.trim().to_string())
    }

    fn requirement(target: &Path) -> Result<String, String> {
        let output = Command::new("/usr/bin/codesign")
            .args(["-d", "-r-"])
            .arg(target)
            .output()
            .map_err(|error| error.to_string())?;
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        text.lines()
            .find_map(|line| line.strip_prefix("designated => "))
            .map(str::to_string)
            .ok_or_else(|| format!("{} has no designated requirement", target.display()))
    }

    fn leftovers(target: &Path) -> Outcome {
        let parent = target.parent().ok_or("no parent")?;
        let name = target.file_name().ok_or("no name")?.to_string_lossy().into_owned();
        for suffix in ["incoming", "failed"] {
            let path = parent.join(format!(".{name}.{suffix}"));
            if path.exists() {
                return Err(format!("{} was left behind", path.display()));
            }
        }
        Ok(())
    }

    fn apply_busy(&self, staged: &Staged, target: &Path) -> Outcome {
        let before = Self::reports(target)?;
        let name = target.file_name().and_then(|name| name.to_str()).ok_or("the install has no name")?;
        let parent = target.parent().ok_or("the install has no parent")?;
        let root = staged.dir.parent().ok_or("the staged download has no channel folder")?;
        for lock in [root.with_extension("lock"), parent.join(format!(".{name}.lock"))] {
            let held = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock)
                .map_err(|error| format!("open {}: {error}", lock.display()))?;
            held.try_lock().map_err(|error| format!("lock {}: {error}", lock.display()))?;
            expect(
                update::apply(&self.build(V0), staged, target),
                |refusal| matches!(refusal, Refusal::Apply(reason) if reason.contains("already downloading or installing")),
            )?;
            drop(held);
            if update::staged(&self.build(V0), &self.data_dir()).is_none_or(|again| again.dir != staged.dir) {
                return Err(format!("the staged download is gone after a refused apply under {}", lock.display()));
            }
            if Self::reports(target)? != before || parent.join(format!(".{name}.previous")).exists() {
                return Err(format!("a refused apply under {} changed the install", lock.display()));
            }
        }
        Ok(())
    }

    fn apply_good(&self, staged: &Staged, target: &Path) -> Outcome {
        let requirement = if cfg!(target_os = "macos") { Some(Self::requirement(target)?) } else { None };
        let applied = update::apply(&self.build(V0), staged, target).map_err(|refusal| refusal.to_string())?;
        let now = Self::reports(target)?;
        if now != format!("local-studio {V1} (nightly)") {
            return Err(format!("the installed build reports {now}"));
        }
        let previous = Self::reports(&applied.previous)?;
        if previous != format!("local-studio {V0} (nightly)") {
            return Err(format!("{} reports {previous}", applied.previous.display()));
        }
        if let Some(requirement) = requirement {
            sh(Command::new("/usr/bin/codesign")
                .args(["--verify", "--deep", "--strict", "-R"])
                .arg(format!("={requirement}"))
                .arg(target))?;
        }
        Self::leftovers(target)
    }

    fn stage(&self, running: &str, version: &str, kind: Kind, case: &str) -> Result<Staged, String> {
        let build = self.build(running);
        let body = self.artifact(version, kind)?;
        self.publish(case, version, &body, Some(self.key.sign(&body, &self.asset)), None)?;
        let offer = self.offered(&build)?;
        self.download(&build, &offer).map_err(|refusal| format!("download {version}: {refusal}"))
    }

    fn other_identity(&self, target: &Path) -> Outcome {
        let staged = self.stage(V1, V2, Kind::AdHoc, "adhoc")?;
        expect(update::apply(&self.build(V1), &staged, target), |refusal| matches!(refusal, Refusal::BadBundle(_)))?;
        let now = Self::reports(target)?;
        if now != format!("local-studio {V1} (nightly)") {
            return Err(format!("after the refusal the installed build reports {now}"));
        }
        Self::leftovers(target)
    }

    fn rollback(&self, target: &Path) -> Outcome {
        let staged = self.stage(V1, V3, Kind::BreaksWhenMoved, "corrupt")?;
        expect(update::apply(&self.build(V1), &staged, target), |refusal| matches!(refusal, Refusal::RolledBack(_)))?;
        let now = Self::reports(target)?;
        if now != format!("local-studio {V1} (nightly)") {
            return Err(format!("after the rollback the installed build reports {now}"));
        }
        if cfg!(target_os = "macos") {
            let requirement = Self::requirement(target)?;
            sh(Command::new("/usr/bin/codesign")
                .args(["--verify", "--deep", "--strict", "-R"])
                .arg(format!("={requirement}"))
                .arg(target))?;
        }
        Self::leftovers(target)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Good,
    AdHoc,
    BreaksWhenMoved,
}
