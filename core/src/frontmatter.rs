use serde_json::Value;

fn split(content: &str) -> (Option<String>, String) {
    let normalized = content.trim_start_matches('\u{feff}').replace("\r\n", "\n").replace('\r', "\n");
    let Some(rest) = normalized.strip_prefix("---") else { return (None, normalized) };
    let Some(end) = rest.find("\n---") else { return (None, normalized) };
    let Some(yaml) = rest.get(1..end).filter(|yaml| !yaml.is_empty()) else { return (None, normalized) };
    let body = rest.get(end + 4..).unwrap_or_default().trim().to_string();
    (Some(yaml.to_string()), body)
}

#[must_use]
pub fn field(content: &str, key: &str) -> Option<String> {
    let yaml = split(content).0?;
    let options = serde_saphyr::options! { strict_booleans: true };
    let parsed: Value = serde_saphyr::from_str_with_options(&yaml, options).ok()?;
    let value = parsed.get(key)?.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[must_use]
pub fn strip(content: &str) -> String {
    split(content).1
}
