mod apply;
pub mod minisign;

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use blake2::Blake2b512;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use self::apply::{apply, current_install, install_path, open_channel, relaunch, Applied};
use self::minisign::{PublicKey, SignatureFile};

pub const NIGHTLY_PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDc4QjIxNzIyMDY2NkJFNkMKUldSc3ZtWUdJaGV5ZU1zUnB3bi9heW1vdFlnMllXZG9LOTBnTGhNL0VLQ21OdGRsN1dHcEx5SWkK";
pub const STABLE_PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDE1NkMxREQ1MDdBNzBDNzUKUldSMURLY0gxUjFzRlF0L0hnZEdRanI0WHpYTmlzWml4VlB4cCtKY1JDNU5RZ241M0k4Um9RM3MK";
pub const REPO: &str = "baney75/local-studio";
pub const CHANNELS: [&str; 3] = ["stable", "nightly", "dev"];
pub const CHECK_EVERY: Duration = Duration::from_hours(6);

pub const PUBLIC_KEY: &str = if same(crate::CHANNEL, "stable") { STABLE_PUBLIC_KEY } else { NIGHTLY_PUBLIC_KEY };

const MAX_ARTIFACT: u64 = 4 << 30;
const READY_FILE: &str = "ready.json";

const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut index = 0;
    while index < a.len() {
        if a[index] != b[index] {
            return false;
        }
        index += 1;
    }
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub version: String,
    pub channel: String,
    pub platform: String,
    pub public_key: String,
    pub feed: String,
}

impl Build {
    #[must_use]
    pub fn current() -> Self {
        Self {
            version: crate::VERSION.to_string(),
            channel: crate::CHANNEL.to_string(),
            platform: platform().to_string(),
            public_key: PUBLIC_KEY.to_string(),
            feed: feed_url(crate::CHANNEL),
        }
    }

    #[must_use]
    pub fn is_local(&self) -> bool {
        self.version.contains("+local.") || self.version.contains("-dev.0+") || self.version.ends_with("-dev.0")
    }

    #[must_use]
    pub fn asset(&self) -> String {
        if self.platform.starts_with("darwin-") {
            format!("local-studio-{}.app.tar.gz", self.platform)
        } else {
            format!("local-studio-{}.tar.gz", self.platform)
        }
    }

    #[must_use]
    pub fn refusal(&self) -> Option<Refusal> {
        if self.is_local() {
            return Some(Refusal::LocalBuild);
        }
        if !CHANNELS.contains(&self.channel.as_str()) {
            return Some(Refusal::Unsupported(format!("{} is not a release channel", self.channel)));
        }
        if !matches!(self.platform.as_str(), "darwin-aarch64" | "darwin-x86_64" | "linux-x86_64" | "linux-aarch64") {
            return Some(Refusal::Unsupported(format!("this platform ({}) has no update feed", self.platform)));
        }
        None
    }
}

#[must_use]
pub fn feed_url(channel: &str) -> String {
    match channel {
        "stable" => format!("https://github.com/{REPO}/releases/latest/download/latest.json"),
        other => format!("https://github.com/{REPO}/releases/download/{other}/latest.json"),
    }
}

#[must_use]
pub fn channel_page(channel: &str) -> String {
    match channel {
        "stable" => format!("https://github.com/{REPO}/releases/latest"),
        other => format!("https://github.com/{REPO}/releases/tag/{other}"),
    }
}

#[must_use]
pub fn channel_name(channel: &str) -> &'static str {
    match channel {
        "stable" => "Local Studio",
        "nightly" => "Local Studio Nightly",
        _ => "Local Studio Dev",
    }
}

#[must_use]
pub const fn platform() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "darwin-aarch64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "darwin-x86_64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x86_64"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "linux-aarch64"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "windows-x86_64"
    } else {
        "unknown"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    LocalBuild,
    Unsupported(String),
    Network(String),
    BadFeed(String),
    OtherChannel { feed: String, build: String },
    NoPlatform(String),
    NotNewer { feed: String, build: String },
    BadSha256 { want: String, got: String },
    BadSignature(String),
    BadBundle(String),
    Apply(String),
    RolledBack(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalBuild => write!(formatter, "This is a local build. Local builds never update themselves."),
            Self::Unsupported(reason) => write!(formatter, "Updates are not available: {reason}."),
            Self::Network(reason) => write!(formatter, "Could not reach the update feed: {reason}"),
            Self::BadFeed(reason) => write!(formatter, "The update feed is not valid: {reason}"),
            Self::OtherChannel { feed, build } => {
                write!(formatter, "The feed is for the {feed} channel, and this build is on {build}. Switching channel is a separate install.")
            }
            Self::NoPlatform(platform) => write!(formatter, "The feed has no build for {platform}."),
            Self::NotNewer { feed, build } => {
                write!(formatter, "The feed's {feed} is not newer than this build ({build}).")
            }
            Self::BadSha256 { want, got } => {
                write!(
                    formatter,
                    "The download does not match its sha256 (feed {want}, file {got}), so it was discarded."
                )
            }
            Self::BadSignature(reason) => write!(formatter, "The download's signature was refused: {reason}."),
            Self::BadBundle(reason) => write!(formatter, "The new build was refused: {reason}."),
            Self::Apply(reason) => write!(formatter, "The update could not be installed: {reason}"),
            Self::RolledBack(reason) => write!(
                formatter,
                "The new build failed its check after install ({reason}), so the previous build was restored."
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub version: String,
    pub notes: String,
    pub url: String,
    pub sha256: String,
    pub signature: String,
    pub asset: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    UpToDate { feed: String },
    Available(Offer),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub version: String,
    pub notes: String,
    pub artifact: PathBuf,
    pub dir: PathBuf,
}

#[derive(Deserialize)]
struct FeedFile {
    product: Option<String>,
    channel: Option<String>,
    version: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    platforms: BTreeMap<String, FeedPlatform>,
}

#[derive(Deserialize)]
struct FeedPlatform {
    url: String,
    #[serde(default)]
    signature: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct ReadyFile {
    channel: String,
    version: String,
    notes: String,
    asset: String,
    sha256: String,
    signature: String,
}

fn release_version(text: &str) -> Result<semver::Version, String> {
    let mut version =
        semver::Version::parse(text).map_err(|error| format!("{text} is not a SemVer version: {error}"))?;
    version.build = semver::BuildMetadata::EMPTY;
    Ok(version)
}

fn channel_of(version: &semver::Version) -> Option<&'static str> {
    let pre = version.pre.as_str();
    if pre.is_empty() {
        Some("stable")
    } else if pre.starts_with("nightly.") {
        Some("nightly")
    } else if pre.starts_with("dev.") {
        Some("dev")
    } else {
        None
    }
}

fn newer(build: &Build, version: &str) -> Result<bool, Refusal> {
    let current = release_version(&build.version).map_err(Refusal::Unsupported)?;
    let offered = release_version(version).map_err(Refusal::BadFeed)?;
    let offered_channel = channel_of(&offered);
    if offered_channel != Some(build.channel.as_str()) {
        return Err(Refusal::OtherChannel {
            feed: offered_channel.unwrap_or("an unknown").to_string(),
            build: build.channel.clone(),
        });
    }
    Ok(offered > current)
}

pub fn evaluate(build: &Build, text: &str) -> Result<Checked, Refusal> {
    if let Some(refusal) = build.refusal() {
        return Err(refusal);
    }
    let feed: FeedFile = serde_json::from_str(text).map_err(|error| Refusal::BadFeed(error.to_string()))?;
    if feed.product.as_deref().is_some_and(|product| product != "local-studio") {
        return Err(Refusal::BadFeed(format!("it is the feed of {}", feed.product.unwrap_or_default())));
    }
    if let Some(channel) = feed.channel.as_deref().filter(|channel| *channel != build.channel) {
        return Err(Refusal::OtherChannel { feed: channel.to_string(), build: build.channel.clone() });
    }
    if !newer(build, &feed.version)? {
        return Ok(Checked::UpToDate { feed: feed.version });
    }
    let entry = feed.platforms.get(&build.platform).ok_or_else(|| Refusal::NoPlatform(build.platform.clone()))?;
    let signature = entry
        .signature
        .clone()
        .filter(|signature| !signature.trim().is_empty())
        .ok_or_else(|| Refusal::BadSignature("the feed carries no signature for this build".to_string()))?;
    let sha256 = entry
        .sha256
        .clone()
        .filter(|sha| sha.len() == 64 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| Refusal::BadFeed("the feed carries no sha256 for this build".to_string()))?
        .to_ascii_lowercase();
    let asset = asset_of(&entry.url)?;
    if asset != build.asset() {
        return Err(Refusal::BadFeed(format!("{asset} is not this platform's build ({})", build.asset())));
    }
    Ok(Checked::Available(Offer {
        version: feed.version,
        notes: feed.notes.unwrap_or_default(),
        url: entry.url.clone(),
        sha256,
        signature,
        asset,
    }))
}

fn asset_of(url: &str) -> Result<String, Refusal> {
    let parsed = reqwest::Url::parse(url).map_err(|error| Refusal::BadFeed(format!("{url}: {error}")))?;
    let loopback = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && loopback) {
        return Err(Refusal::BadFeed(format!("{url} is not https")));
    }
    parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Refusal::BadFeed(format!("{url} names no file")))
}

fn client() -> Result<reqwest::Client, Refusal> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .user_agent(format!("local-studio/{}", crate::VERSION))
        .build()
        .map_err(|error| Refusal::Network(error.to_string()))
}

pub async fn check(build: &Build) -> Result<Checked, Refusal> {
    if let Some(refusal) = build.refusal() {
        return Err(refusal);
    }
    let response = client()?
        .get(&build.feed)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|error| Refusal::Network(error.to_string()))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(Refusal::Network(format!("no {} build has been published yet", build.channel)));
    }
    if !response.status().is_success() {
        return Err(Refusal::Network(format!("{} answered HTTP {}", build.feed, response.status().as_u16())));
    }
    let text = response.text().await.map_err(|error| Refusal::Network(error.to_string()))?;
    evaluate(build, &text)
}

#[must_use]
pub fn staging_dir(data_dir: &Path, channel: &str) -> PathBuf {
    data_dir.join("updates").join(channel)
}

fn slot(version: &str) -> String {
    version.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect()
}

pub fn verify_file(build: &Build, path: &Path, sha256: &str, signature: &str, asset: &str) -> Result<(), Refusal> {
    let mut file = File::open(path).map_err(|error| Refusal::Apply(format!("open {}: {error}", path.display())))?;
    let mut sha = Sha256::new();
    let mut blake = Blake2b512::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read =
            file.read(&mut buffer).map_err(|error| Refusal::Apply(format!("read {}: {error}", path.display())))?;
        if read == 0 {
            break;
        }
        sha.update(&buffer[..read]);
        blake.update(&buffer[..read]);
    }
    check_digests(build, &hex(&sha.finalize()), &blake.finalize().into(), sha256, signature, asset)
}

fn check_digests(
    build: &Build,
    got_sha: &str,
    blake: &[u8; 64],
    sha256: &str,
    signature: &str,
    asset: &str,
) -> Result<(), Refusal> {
    if !got_sha.eq_ignore_ascii_case(sha256) {
        return Err(Refusal::BadSha256 { want: sha256.to_string(), got: got_sha.to_string() });
    }
    let key = PublicKey::decode(&build.public_key)
        .map_err(|error| Refusal::BadSignature(format!("this build's key: {error}")))?;
    let signature = SignatureFile::decode(signature).map_err(Refusal::BadSignature)?;
    signature.verify(&key, blake).map_err(Refusal::BadSignature)?;
    match signature.file_name() {
        Some(name) if name == asset => Ok(()),
        Some(name) => Err(Refusal::BadSignature(format!("it was made for {name}, not {asset}"))),
        None => Err(Refusal::BadSignature("its trusted comment names no file".to_string())),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{byte:02x}"));
        out
    })
}

pub async fn download(
    build: &Build,
    offer: &Offer,
    data_dir: &Path,
    mut progress: impl FnMut(u64, Option<u64>) + Send,
) -> Result<Staged, Refusal> {
    if let Some(refusal) = build.refusal() {
        return Err(refusal);
    }
    if !newer(build, &offer.version)? {
        return Err(Refusal::NotNewer { feed: offer.version.clone(), build: build.version.clone() });
    }
    if asset_of(&offer.url)? != build.asset() || offer.asset != build.asset() {
        return Err(Refusal::BadFeed(format!("{} is not this platform's build", offer.url)));
    }
    let root = staging_dir(data_dir, &build.channel);
    let dir = root.join(slot(&offer.version));
    clear_other_slots(&root, &dir);
    std::fs::create_dir_all(&dir).map_err(|error| Refusal::Apply(format!("create {}: {error}", dir.display())))?;
    let _ = std::fs::remove_file(dir.join(READY_FILE));
    let part = dir.join(format!("{}.part", offer.asset));
    let artifact = dir.join(&offer.asset);
    let _ = std::fs::remove_file(&artifact);
    let mut response = client()?
        .get(&offer.url)
        .timeout(Duration::from_mins(60))
        .send()
        .await
        .map_err(|error| Refusal::Network(error.to_string()))?;
    if !response.status().is_success() {
        return Err(Refusal::Network(format!("{} answered HTTP {}", offer.url, response.status().as_u16())));
    }
    let total = response.content_length();
    if total.is_some_and(|total| total > MAX_ARTIFACT) {
        return Err(Refusal::BadFeed("the download is larger than 4 GiB".to_string()));
    }
    let mut file =
        File::create(&part).map_err(|error| Refusal::Apply(format!("create {}: {error}", part.display())))?;
    let mut sha = Sha256::new();
    let mut blake = Blake2b512::new();
    let mut received = 0u64;
    progress(0, total);
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                drop(file);
                let _ = std::fs::remove_file(&part);
                return Err(Refusal::Network(error.to_string()));
            }
        };
        received += chunk.len() as u64;
        if received > MAX_ARTIFACT {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Err(Refusal::BadFeed("the download is larger than 4 GiB".to_string()));
        }
        sha.update(&chunk);
        blake.update(&chunk);
        if let Err(error) = file.write_all(&chunk) {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Err(Refusal::Apply(format!("write {}: {error}", part.display())));
        }
        progress(received, total);
    }
    let synced = file.sync_all();
    drop(file);
    if let Err(error) = synced {
        let _ = std::fs::remove_file(&part);
        return Err(Refusal::Apply(format!("write {}: {error}", part.display())));
    }
    if let Err(refusal) = check_digests(
        build,
        &hex(&sha.finalize()),
        &blake.finalize().into(),
        &offer.sha256,
        &offer.signature,
        &offer.asset,
    ) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(refusal);
    }
    std::fs::rename(&part, &artifact).map_err(|error| Refusal::Apply(format!("rename {}: {error}", part.display())))?;
    let ready = ReadyFile {
        channel: build.channel.clone(),
        version: offer.version.clone(),
        notes: offer.notes.clone(),
        asset: offer.asset.clone(),
        sha256: offer.sha256.clone(),
        signature: offer.signature.clone(),
    };
    let text = serde_json::to_vec_pretty(&ready).map_err(|error| Refusal::Apply(error.to_string()))?;
    std::fs::write(dir.join(READY_FILE), text)
        .map_err(|error| Refusal::Apply(format!("write {READY_FILE}: {error}")))?;
    Ok(Staged { version: offer.version.clone(), notes: offer.notes.clone(), artifact, dir })
}

fn clear_other_slots(root: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path != keep && path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

#[must_use]
pub fn staged(build: &Build, data_dir: &Path) -> Option<Staged> {
    if build.refusal().is_some() {
        return None;
    }
    let root = staging_dir(data_dir, &build.channel);
    let mut found = None;
    for entry in std::fs::read_dir(&root).ok()?.flatten() {
        let dir = entry.path();
        let ready = std::fs::read(dir.join(READY_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ReadyFile>(&bytes).ok())
            .filter(|ready| ready.channel == build.channel && ready.asset == build.asset());
        let Some(ready) = ready else {
            let _ = std::fs::remove_dir_all(&dir);
            continue;
        };
        let artifact = dir.join(&ready.asset);
        let usable = matches!(newer(build, &ready.version), Ok(true))
            && verify_file(build, &artifact, &ready.sha256, &ready.signature, &ready.asset).is_ok();
        if usable && found.is_none() {
            found = Some(Staged { version: ready.version, notes: ready.notes, artifact, dir });
        } else {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    found
}

#[must_use]
pub fn installed_channels() -> Vec<(String, bool)> {
    CHANNELS
        .iter()
        .map(|channel| ((*channel).to_string(), install_path(channel).is_some_and(|path| path.exists())))
        .collect()
}
