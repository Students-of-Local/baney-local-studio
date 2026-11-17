use std::fmt;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const PAIRING_TTL_MS: i64 = 10 * 60 * 1000;
const SECRET_BYTES: usize = 32;

pub struct Secret(String);

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(..)")
    }
}

impl Secret {
    fn fresh() -> anyhow::Result<Self> {
        let mut bytes = [0u8; SECRET_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("get random bytes: {error}"))?;
        Ok(Self(hex(&bytes)))
    }

    pub fn reveal(&self) -> &str {
        &self.0
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

fn digest(secret: &str) -> String {
    hex(&Sha256::digest(secret.trim().to_ascii_lowercase().as_bytes()))
}

fn same_digest(presented: &str, stored: &str) -> bool {
    let presented = digest(presented);
    presented.len() == stored.len() && bool::from(presented.as_bytes().ct_eq(stored.as_bytes()))
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub channel: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    #[serde(flatten)]
    pub info: DeviceInfo,
    credential_hash: String,
    pub first_seen: i64,
    pub last_seen: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Ledger {
    machine_id: String,
    devices: Vec<Device>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pairing {
    secret_hash: String,
    expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairError {
    NoSecret,
    Expired,
    Wrong,
    BadDevice,
}

impl fmt::Display for PairError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NoSecret => "this machine has no open pairing: run `local-studio-serve pair` and scan the new code",
            Self::Expired => "this pairing code expired: run `local-studio-serve pair` again",
            Self::Wrong => "this pairing code is not valid: scan the code `local-studio-serve pair` shows",
            Self::BadDevice => "the pairing names no device id",
        })
    }
}

pub struct Devices {
    ledger: PathBuf,
    pairing: PathBuf,
    lock: PathBuf,
}

struct Held(std::fs::File);

impl Drop for Held {
    fn drop(&mut self) {
        self.0.unlock().ok();
    }
}

impl Devices {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            ledger: data_dir.join("remote-devices.json"),
            pairing: data_dir.join("remote-pairing.json"),
            lock: data_dir.join("remote-devices.lock"),
        }
    }

    fn hold(&self) -> anyhow::Result<Held> {
        if let Some(folder) = self.lock.parent() {
            std::fs::create_dir_all(folder).with_context(|| format!("create {}", folder.display()))?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.lock)
            .with_context(|| format!("open {}", self.lock.display()))?;
        file.lock().with_context(|| format!("lock {}", self.lock.display()))?;
        Ok(Held(file))
    }

    fn read(&self) -> anyhow::Result<Ledger> {
        match std::fs::read(&self.ledger) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("read {}", self.ledger.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::default()),
            Err(error) => Err(error).with_context(|| format!("read {}", self.ledger.display())),
        }
    }

    fn write(&self, ledger: &Ledger) -> anyhow::Result<()> {
        write_private(&self.ledger, &serde_json::to_vec_pretty(ledger)?)
    }

    fn edit<T>(&self, change: impl FnOnce(&mut Ledger) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let _held = self.hold()?;
        let mut ledger = self.read()?;
        let result = change(&mut ledger)?;
        self.write(&ledger)?;
        Ok(result)
    }

    pub fn machine_id(&self) -> anyhow::Result<String> {
        let known = self.read()?.machine_id;
        if !known.is_empty() {
            return Ok(known);
        }
        self.edit(|ledger| {
            if ledger.machine_id.is_empty() {
                ledger.machine_id = Secret::fresh()?.0[..32].to_string();
            }
            Ok(ledger.machine_id.clone())
        })
    }

    pub fn open_pairing(&self) -> anyhow::Result<(Secret, i64)> {
        let _held = self.hold()?;
        let secret = Secret::fresh()?;
        let expires_at = now_ms() + PAIRING_TTL_MS;
        let pairing = Pairing { secret_hash: digest(secret.reveal()), expires_at };
        write_private(&self.pairing, &serde_json::to_vec(&pairing)?)?;
        Ok((secret, expires_at))
    }

    pub fn pair(&self, secret: &str, info: DeviceInfo) -> anyhow::Result<Result<Secret, PairError>> {
        if info.id.trim().is_empty() {
            return Ok(Err(PairError::BadDevice));
        }
        let _held = self.hold()?;
        let pairing: Pairing = match std::fs::read(&self.pairing) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(pairing) => pairing,
                Err(_) => return Ok(Err(PairError::NoSecret)),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Err(PairError::NoSecret)),
            Err(error) => return Err(error).with_context(|| format!("read {}", self.pairing.display())),
        };
        if !same_digest(secret, &pairing.secret_hash) {
            return Ok(Err(PairError::Wrong));
        }
        std::fs::remove_file(&self.pairing).with_context(|| format!("remove {}", self.pairing.display()))?;
        if pairing.expires_at < now_ms() {
            return Ok(Err(PairError::Expired));
        }
        let credential = Secret::fresh()?;
        let mut ledger = self.read()?;
        let now = now_ms();
        let first_seen =
            ledger.devices.iter().find(|device| device.info.id == info.id).map_or(now, |device| device.first_seen);
        ledger.devices.retain(|device| device.info.id != info.id);
        ledger.devices.push(Device { info, credential_hash: digest(credential.reveal()), first_seen, last_seen: now });
        self.write(&ledger)?;
        Ok(Ok(credential))
    }

    pub fn check(&self, device: &str, credential: &str) -> anyhow::Result<Option<Device>> {
        let ledger = self.read()?;
        let mut found = None;
        for known in ledger.devices {
            if same_digest(credential, &known.credential_hash) && known.info.id == device {
                found = Some(known);
            }
        }
        Ok(found)
    }

    pub fn touch(&self, device: &str) -> anyhow::Result<()> {
        self.edit(|ledger| {
            let now = now_ms();
            for known in ledger.devices.iter_mut().filter(|known| known.info.id == device) {
                known.last_seen = now;
            }
            Ok(())
        })
    }

    pub fn list(&self) -> anyhow::Result<Vec<Device>> {
        Ok(self.read()?.devices)
    }

    pub fn revoke(&self, device: &str) -> anyhow::Result<Vec<Device>> {
        self.edit(|ledger| {
            let (gone, kept) =
                ledger.devices.drain(..).partition(|known| known.info.id == device || known.info.name == device);
            ledger.devices = kept;
            Ok(gone)
        })
    }
}

pub fn write_private(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let folder = path.parent().with_context(|| format!("{} has no folder", path.display()))?;
    std::fs::create_dir_all(folder).with_context(|| format!("create {}", folder.display()))?;
    let name = path.file_name().with_context(|| format!("{} has no file name", path.display()))?.to_string_lossy();
    let staging = folder.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&staging)
        .and_then(|mut file| {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            file.write_all(contents)?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&staging, path));
    if let Err(error) = written {
        std::fs::remove_file(&staging).ok();
        return Err(error).with_context(|| format!("write {}", path.display()));
    }
    Ok(())
}
