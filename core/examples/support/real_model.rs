use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{ensure, Context};
use local_studio_core::{AccessMode, ThreadSettings, UsageReport, WorktreeChoice};
use serde_json::json;

pub const MODEL_ENV: &str = "LOCAL_STUDIO_SMOKE_MODEL";
pub const BASE_ENV: &str = "LOCAL_STUDIO_SMOKE_BASE_URL";
pub const CONTEXT_ENV: &str = "LOCAL_STUDIO_SMOKE_CONTEXT";
pub const IMAGE_ENV: &str = "LOCAL_STUDIO_SMOKE_IMAGE_MODEL";
const BASE: &str = "http://127.0.0.1:11434/v1";
const CONTEXT: u64 = 32_768;
const MAX_TOKENS: u64 = 8_192;
const PRICE_PER_MILLION: f64 = 1.0;
const STREAM_IDLE_MS: u64 = 900_000;

struct Target {
    provider: String,
    model: String,
    image: Option<String>,
    base: String,
    context: u64,
}

static TARGET: OnceLock<Target> = OnceLock::new();

fn target() -> anyhow::Result<&'static Target> {
    if let Some(target) = TARGET.get() {
        return Ok(target);
    }
    let value = std::env::var(MODEL_ENV).with_context(|| {
        format!("{MODEL_ENV} must name the local model as <provider>/<model>, e.g. ollama/fv-smoke-lfm3-1.7b")
    })?;
    let (provider, model) = value
        .split_once('/')
        .filter(|(provider, model)| !provider.is_empty() && !model.is_empty())
        .with_context(|| format!("{MODEL_ENV}={value:?} is not <provider>/<model>"))?;
    let base = std::env::var(BASE_ENV).ok().filter(|base| !base.is_empty()).unwrap_or_else(|| BASE.to_string());
    let context = match std::env::var(CONTEXT_ENV).ok().filter(|value| !value.is_empty()) {
        Some(value) => value.trim().parse().with_context(|| format!("{CONTEXT_ENV} must be a token count"))?,
        None => CONTEXT,
    };
    let image = match std::env::var(IMAGE_ENV).ok().filter(|value| !value.is_empty()) {
        Some(value) => {
            let (image_provider, image) = value
                .split_once('/')
                .filter(|(_, image)| !image.is_empty())
                .with_context(|| format!("{IMAGE_ENV}={value:?} is not <provider>/<model>"))?;
            ensure!(image_provider == provider, "{IMAGE_ENV} must use the provider of {MODEL_ENV} ({provider})");
            Some(image.to_string()).filter(|image| image != model)
        }
        None => None,
    };
    Ok(TARGET.get_or_init(|| Target { provider: provider.to_string(), model: model.to_string(), image, base, context }))
}

pub fn provider() -> &'static str {
    TARGET.get().map_or("", |target| target.provider.as_str())
}

pub fn model() -> &'static str {
    TARGET.get().map_or("", |target| target.model.as_str())
}

pub fn image_model() -> &'static str {
    TARGET.get().map_or("", |target| target.image.as_deref().unwrap_or(&target.model))
}

fn write_private(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    file.write_all(data)?;
    Ok(())
}

pub fn install(agent_dir: &Path) -> anyhow::Result<()> {
    let target = target()?;
    std::fs::create_dir_all(agent_dir)?;
    let price = json!({ "input": PRICE_PER_MILLION, "output": PRICE_PER_MILLION, "cacheRead": 0, "cacheWrite": 0 });
    let mut models = vec![json!({
        "id": &target.model,
        "name": &target.model,
        "reasoning": false,
        "input": if target.image.is_some() { json!(["text"]) } else { json!(["text", "image"]) },
        "contextWindow": target.context,
        "maxTokens": MAX_TOKENS,
        "cost": price,
    })];
    if let Some(image) = &target.image {
        models.push(json!({
            "id": image,
            "name": image,
            "reasoning": true,
            "thinkingLevelMap": { "off": "none" },
            "compat": { "thinkingFormat": "openai", "supportsReasoningEffort": true },
            "input": ["text", "image"],
            "contextWindow": target.context,
            "maxTokens": MAX_TOKENS,
            "cost": price,
        }));
    }
    let catalog = json!({
        "providers": {
            &target.provider: {
                "name": &target.provider,
                "baseUrl": &target.base,
                "api": "openai-completions",
                "apiKey": "none",
                "models": models
            }
        }
    });
    write_private(&agent_dir.join("models.json"), &serde_json::to_vec_pretty(&catalog)?)?;
    write_private(
        &agent_dir.join("settings.json"),
        &serde_json::to_vec_pretty(&json!({
            "defaultProvider": &target.provider,
            "defaultModel": &target.model,
            "retry": { "provider": { "streamIdleTimeoutMs": STREAM_IDLE_MS } },
        }))?,
    )?;
    write_private(&agent_dir.join("auth.json"), b"{}")?;
    local_studio_core::check_scratch_agent(agent_dir).map_err(anyhow::Error::msg)?;
    println!(
        "     local model {}/{} (images: {}) at {}, context {}, no key (auth.json is {{}}), priced at ${PRICE_PER_MILLION} per million tokens in the scratch catalog",
        target.provider,
        target.model,
        image_model(),
        target.base,
        target.context
    );
    Ok(())
}

pub fn thread_settings(access: AccessMode, worktree: WorktreeChoice) -> ThreadSettings {
    ThreadSettings {
        provider: Some(provider().to_string()),
        model: Some(model().to_string()),
        thinking_level: None,
        access,
        worktree,
    }
}

pub fn check_usage(report: &UsageReport) -> anyhow::Result<()> {
    let (provider, model) = (provider(), model());
    let local = report.by_model.iter().find(|entry| entry.provider == provider && entry.model == model);
    println!(
        "     usage: {} sessions, {} tokens (in {} out {}), ${:.4}; {provider}/{model} {} tokens ${:.4}",
        report.requests,
        report.total_tokens,
        report.input_tokens,
        report.output_tokens,
        report.cost_usd,
        local.map_or(0, |entry| entry.tokens),
        local.map_or(0.0, |entry| entry.cost_usd),
    );
    let local = local.context("the usage report has no entry for the local model")?;
    ensure!(
        report.input_tokens > 0 && report.output_tokens > 0 && local.tokens > 0,
        "the usage report counts no real tokens"
    );
    ensure!(local.cost_usd > 0.0, "the usage report prices the local model at $0");
    println!("ok   usage report counts real tokens and cost");
    Ok(())
}
