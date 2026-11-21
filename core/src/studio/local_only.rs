use eukhe_types::daemon::DaemonCommand;
use serde_json::{Map, Value};

use super::Actor;
use crate::harness;
use crate::model::{Delivery, ModelKey, NoticeLevel, PromptDraft, Update};
use crate::plugins::{self, Plugin};
use crate::workspace::{configured_providers, Provider};

const SIDE_MODELS: [&str; 10] = [
    "modelRoles.all",
    "modelRoles.tiny",
    "modelRoles.judge",
    "modelRoles.translate",
    "auxiliaryModel",
    "subagentDefaultModel",
    "workerModel",
    "rlm.queryModel",
    "imageModel",
    "providerBackupModel",
];
const FALLBACK_CHAINS: [&str; 5] = [
    "retry.fallbackChains.tiny",
    "retry.fallbackChains.judge",
    "retry.fallbackChains.translate",
    "retry.fallbackChains.speech",
    "retry.fallbackChains.dictation",
];
const OFF: [&str; 3] = ["retry.failover.enabled", "memory.chat", "titles.generate"];

fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Actor {
    pub(super) fn local_provider(provider: &str) -> bool {
        crate::daemon::agent_dir().is_some_and(|agent| Provider::configured(&agent, provider).is_local())
    }

    fn thread_model(&self, id: &str) -> Option<(String, String)> {
        if let Some(harness) = self.harness(id) {
            return Some((harness::provider_of(harness).to_string(), String::new()));
        }
        let model = self.threads.get(id)?.live.as_ref()?.state.model.as_ref()?;
        Some((model.provider.clone(), model.id.clone()))
    }

    pub(super) fn keep_local(&self, id: &str, provider: &str) -> Result<(), String> {
        match self.store.flags(id).local_only {
            Some(plugin) if !Self::local_provider(provider) => Err(format!(
                "This thread ran {plugin}, which runs only on local models, so it stays on local models. Start a new thread to use {provider}."
            )),
            _ => Ok(()),
        }
    }

    pub(super) fn local_choice(&self) -> Option<ModelKey> {
        let agent = crate::daemon::agent_dir()?;
        let providers = configured_providers(&agent);
        let local = |provider: &str| Provider::from_models(&providers, provider).is_local();
        let key = |provider: &str, model: &str| ModelKey { provider: provider.to_string(), model: model.to_string() };
        self.store
            .favorites
            .iter()
            .find(|favorite| local(&favorite.provider))
            .cloned()
            .or_else(|| {
                self.models
                    .iter()
                    .find(|option| option.configured && local(&option.provider))
                    .map(|option| key(&option.provider, &option.id))
            })
            .or_else(|| {
                providers.iter().filter(|(id, _)| local(id)).find_map(|(id, entry)| {
                    let model = entry.get("models")?.as_array()?.first()?.get("id")?.as_str()?;
                    Some(key(id, model))
                })
            })
    }

    pub(super) async fn local_only_gate(
        &mut self,
        id: &str,
        active: &str,
        invoked: &[String],
        prompt: &PromptDraft,
        request: Option<String>,
    ) -> Result<bool, String> {
        let plugins = self.plugins();
        let named = invoked.iter().find(|owner| plugins.iter().any(|plugin| plugin.local_only && plugin.id == **owner));
        let Some(plugin) = named.cloned().or_else(|| self.store.flags(id).local_only) else {
            return Ok(true);
        };
        let current = self.thread_model(id);
        let Some((provider, model)) = current.clone().filter(|(provider, _)| Self::local_provider(provider)) else {
            let uses =
                current.map_or_else(|| "a provider Local Studio cannot place".to_string(), |(provider, _)| provider);
            let local = if self.harness(id).is_some() { None } else { self.local_choice() };
            let next = match &local {
                Some(key) => format!(" Switch this thread to {} to run it.", key.model),
                None if self.harness(id).is_some() => " Start a thread on a local model to run it.".to_string(),
                None => " Add a local model in Settings, then try again.".to_string(),
            };
            let text = format!(
                "{plugin} runs only on local models, and this thread uses {uses}, so Local Studio did not send it.{next}"
            );
            self.refuse(id, plugin, text, local, prompt, request);
            return Ok(false);
        };
        let open = self.network_servers(active, &plugins).await?;
        if !open.is_empty() {
            let text = format!(
                "{plugin} runs only on local models, and this session has MCP servers that can reach the network: {}. Remove them from mcpServers, then start a new thread.",
                open.join(", ")
            );
            self.refuse(id, plugin, text, None, prompt, request);
            return Ok(false);
        }
        self.pin_local(id, active, &provider, &model, &plugins).await?;
        self.store.update(id, |flags| flags.local_only = Some(plugin.clone()));
        self.save();
        Ok(true)
    }

    fn refuse(
        &mut self,
        id: &str,
        plugin: String,
        message: String,
        local: Option<ModelKey>,
        prompt: &PromptDraft,
        request: Option<String>,
    ) {
        if let Some(thread) = self.threads.get_mut(id) {
            thread.refused = Some(prompt.clone());
        }
        match request {
            Some(request) => self.mailbox.push(Update::RequestFailed { request, message: message.clone() }),
            None => self.mailbox.push(Update::ComposerText { thread: id.to_string(), text: prompt.text.clone() }),
        }
        self.mailbox.push(Update::LocalOnly { thread: id.to_string(), plugin, message, local });
    }

    async fn network_servers(&self, active: &str, plugins: &[Plugin]) -> Result<Vec<String>, String> {
        let command =
            DaemonCommand::GetMcpConnections { id: None, active_session_id: active.to_string(), rest: Map::default() };
        let data =
            self.call(command).await.map_err(|error| format!("Could not read this session's MCP servers: {error}"))?;
        let ours: Vec<String> = plugins.iter().filter(|plugin| plugin.local_only).map(Plugin::mcp_name).collect();
        Ok(data
            .get("connections")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|connection| {
                let flag = |key: &str| connection.get(key).and_then(Value::as_bool) != Some(false);
                flag("userDeclared") || flag("connected")
            })
            .filter_map(|connection| connection.get("server").and_then(Value::as_str))
            .filter(|server| !ours.iter().any(|own| own == server))
            .map(str::to_string)
            .collect())
    }

    async fn pin_local(
        &mut self,
        id: &str,
        active: &str,
        provider: &str,
        model: &str,
        plugins: &[Plugin],
    ) -> Result<(), String> {
        let selector = quoted(&format!("{provider}/{model}"));
        let mut sets: Vec<String> = OFF.iter().map(|name| format!("set {name} false")).collect();
        sets.extend(SIDE_MODELS.iter().map(|name| format!("set {name} {selector}")));
        sets.extend(FALLBACK_CHAINS.iter().map(|name| format!("set {name} []")));
        let command = DaemonCommand::Cvar {
            id: None,
            active_session_id: active.to_string(),
            line: sets.join("; "),
            global: false,
            rest: Map::default(),
        };
        let reply =
            self.call(command).await.map_err(|error| format!("Could not keep this thread on local models: {error}"))?;
        if let Some(errors) = reply.get("errors").and_then(Value::as_array).filter(|errors| !errors.is_empty()) {
            return Err(format!("Could not keep this thread on local models: {}", Value::Array(errors.clone())));
        }
        if self.running(id) {
            return Ok(());
        }
        let grants =
            self.plugins_dir.as_deref().map(|dir| plugins::Grants::load(dir).unwrap_or_default()).unwrap_or_default();
        let command = DaemonCommand::ReplaceAcpMcpServers {
            id: None,
            active_session_id: active.to_string(),
            owner_id: plugins::SESSION_OWNER.to_string(),
            servers: Value::Array(plugins::session_servers(plugins, &grants)),
            rest: Map::default(),
        };
        self.call(command).await.map_err(|error| format!("Could not start the plugin's tool server: {error}"))?;
        Ok(())
    }

    pub(super) async fn run_local(&mut self, id: &str) {
        let Some(prompt) = self.threads.get_mut(id).and_then(|thread| thread.refused.take()) else {
            self.mailbox.notice(NoticeLevel::Info, "That message was already sent or dropped.");
            return;
        };
        let Some(key) = self.local_choice() else {
            self.mailbox.notice(NoticeLevel::Error, "No local model is set up. Add one in Settings, then try again.");
            return;
        };
        if let Err(error) = self.apply_model(id, Some(key.provider), Some(key.model), None).await {
            self.mailbox.notice(NoticeLevel::Error, error);
            return;
        }
        self.mailbox.push(Update::ComposerText { thread: id.to_string(), text: String::new() });
        if let Err(error) = self.send_prompt(id, prompt, Delivery::Queue, None).await {
            self.mailbox.notice(NoticeLevel::Error, error);
        }
    }
}
