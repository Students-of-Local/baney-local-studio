use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use minisign_verify::{PublicKey, Signature};

pub fn public_key(text: &str) -> Result<PublicKey, String> {
    PublicKey::decode(&plain(text)?).map_err(|error| error.to_string())
}

pub fn signature(text: &str) -> Result<Signature, String> {
    Signature::decode(&plain(text)?).map_err(|error| error.to_string())
}

#[must_use]
pub fn file_name(signature: &Signature) -> Option<&str> {
    signature.trusted_comment().split('\t').find_map(|field| field.strip_prefix("file:"))
}

fn plain(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.starts_with("untrusted comment:") {
        return Ok(text.to_string());
    }
    let compact: String = text.split_whitespace().collect();
    let bytes = STANDARD.decode(compact).map_err(|error| format!("not base64: {error}"))?;
    String::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_string())
}
