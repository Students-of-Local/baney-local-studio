use base64::engine::general_purpose::STANDARD;
use std::fmt::Write;

use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};

const KEY_ALG: &[u8; 2] = b"Ed";
const PREHASHED_ALG: &[u8; 2] = b"ED";
const LEGACY_ALG: &[u8; 2] = b"Ed";

#[derive(Debug, Clone)]
pub struct PublicKey {
    id: [u8; 8],
    key: VerifyingKey,
}

#[derive(Debug, Clone)]
pub struct SignatureFile {
    id: [u8; 8],
    signature: [u8; 64],
    trusted: String,
    global: [u8; 64],
}

impl PublicKey {
    pub fn decode(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let file = if text.starts_with("untrusted comment:") { text.to_string() } else { unwrap_base64(text)? };
        let line = file
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("untrusted comment:"))
            .ok_or_else(|| "the public key has no key line".to_string())?;
        let bytes = STANDARD.decode(line).map_err(|error| format!("the public key is not base64: {error}"))?;
        if bytes.len() != 42 || &bytes[..2] != KEY_ALG {
            return Err("the public key is not a minisign Ed25519 key".to_string());
        }
        let mut id = [0; 8];
        id.copy_from_slice(&bytes[2..10]);
        let mut raw = [0; 32];
        raw.copy_from_slice(&bytes[10..]);
        let key = VerifyingKey::from_bytes(&raw).map_err(|error| format!("the public key is invalid: {error}"))?;
        Ok(Self { id, key })
    }

    #[must_use]
    pub fn id(&self) -> String {
        key_id(self.id)
    }
}

impl SignatureFile {
    pub fn decode(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let file = if text.starts_with("untrusted comment:") { text.to_string() } else { unwrap_base64(text)? };
        let mut lines = file.lines().map(str::trim_end);
        let _untrusted = lines
            .next()
            .filter(|line| line.starts_with("untrusted comment:"))
            .ok_or_else(|| "the signature has no untrusted comment line".to_string())?;
        let signature = lines.next().ok_or_else(|| "the signature has no signature line".to_string())?;
        let trusted = lines
            .next()
            .and_then(|line| line.strip_prefix("trusted comment: "))
            .ok_or_else(|| "the signature has no trusted comment".to_string())?
            .to_string();
        let global = lines.next().ok_or_else(|| "the signature has no global signature".to_string())?;
        let bytes =
            STANDARD.decode(signature.trim()).map_err(|error| format!("the signature is not base64: {error}"))?;
        if bytes.len() != 74 {
            return Err("the signature line has the wrong length".to_string());
        }
        if &bytes[..2] == LEGACY_ALG {
            return Err("the signature is not prehashed (legacy minisign format)".to_string());
        }
        if &bytes[..2] != PREHASHED_ALG {
            return Err("the signature is not a minisign Ed25519 signature".to_string());
        }
        let mut id = [0; 8];
        id.copy_from_slice(&bytes[2..10]);
        let mut sig = [0; 64];
        sig.copy_from_slice(&bytes[10..]);
        let global =
            STANDARD.decode(global.trim()).map_err(|error| format!("the global signature is not base64: {error}"))?;
        let global: [u8; 64] =
            global.try_into().map_err(|_| "the global signature has the wrong length".to_string())?;
        Ok(Self { id, signature: sig, trusted, global })
    }

    #[must_use]
    pub fn trusted_comment(&self) -> &str {
        &self.trusted
    }

    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        self.trusted.split('\t').find_map(|field| field.strip_prefix("file:"))
    }

    pub fn verify(&self, key: &PublicKey, blake2b512: &[u8; 64]) -> Result<(), String> {
        if self.id != key.id {
            let other = key_id(self.id);
            return Err(format!("signed with key {other}, not this channel's key {}", key.id()));
        }
        key.key
            .verify_strict(blake2b512, &Signature::from_bytes(&self.signature))
            .map_err(|_| "the signature does not match the file".to_string())?;
        let mut global = Vec::with_capacity(64 + self.trusted.len());
        global.extend_from_slice(&self.signature);
        global.extend_from_slice(self.trusted.as_bytes());
        key.key
            .verify_strict(&global, &Signature::from_bytes(&self.global))
            .map_err(|_| "the trusted comment's signature does not match".to_string())
    }
}

fn unwrap_base64(text: &str) -> Result<String, String> {
    let compact: String = text.split_whitespace().collect();
    let bytes = STANDARD.decode(compact).map_err(|error| format!("not base64: {error}"))?;
    String::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_string())
}

fn key_id(id: [u8; 8]) -> String {
    id.iter().rev().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02X}");
        out
    })
}
