use eukhe_types::daemon::DaemonCommand;
use serde_json::{Map, Value};

use super::Actor;
use crate::harness;
use crate::model::{Delivery, ModelKey, NoticeLevel, PromptDraft, Update};
use crate::plugins::{self, Plugin};
use crate::store::HeldPrompt;
use crate::tools;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Gate {
    Send,
    Held,
    Refused,
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Actor {
    pub(super) fn local_provider(provider: &str) -> bool {
        crate::daemon::agent_dir().is_some_and(|agent| Provider::configured(&agent, provider).is_local())
    }

    pub(super) fn thread_model(&self, id: &str) -> Option<(String, String)> {
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

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) async fn local_only_gate(
        &mut self,
        id: &str,
        active: &str,
        invoked: &[String],
        prompt: &PromptDraft,
        delivery: Delivery,
        request: Option<String>,
        again: bool,
    ) -> Result<Gate, String> {
        let plugins = self.plugins();
        let local_only = |owner: &String| plugins.iter().any(|plugin| plugin.local_only && plugin.id == *owner);
        let recorded = self.store.flags(id).local_only;
        let mut wanted: Vec<String> = invoked.iter().filter(|owner| local_only(owner)).cloned().collect();
        let Some(plugin) = wanted.first().cloned().or_else(|| recorded.clone()) else {
            return Ok(Gate::Send);
        };
        if let Some(recorded) = recorded.filter(|recorded| !wanted.contains(recorded)) {
            wanted.push(recorded);
        }
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
            return Ok(Gate::Refused);
        };
        if let Err(reaches) = self.session_local(active, &provider).await {
            let text = format!(
                "{plugin} runs only on local models, and this thread's session sends {provider} requests to {reaches}, which is not a local address, so Local Studio did not send it. Start a new thread to use the provider as models.json lists it now."
            );
            self.refuse(id, plugin, text, None, prompt, request);
            return Ok(Gate::Refused);
        }
        if self.holds_tools(active) {
            if self.running(id) {
                if !again {
                    self.hold(id, prompt, delivery, request);
                }
                return Ok(Gate::Held);
            }
            self.drop_tools(active).await?;
        }
        let (open, clashes) = self.network_servers(active, &plugins).await?;
        if !open.is_empty() || !clashes.is_empty() {
            let mut parts = Vec::new();
            if !open.is_empty() {
                parts.push(format!("this session has MCP servers that can reach the network: {}", open.join(", ")));
            }
            if !clashes.is_empty() {
                parts.push(format!(
                    "mcpServers declares {}, the name of a local-only plugin's own tool server, which Local Studio starts only for the session",
                    clashes.join(", ")
                ));
            }
            let text = format!(
                "{plugin} runs only on local models, and {}. Remove them from mcpServers, then start a new thread.",
                parts.join(", and ")
            );
            self.refuse(id, plugin, text, None, prompt, request);
            return Ok(Gate::Refused);
        }
        let grants =
            self.plugins_dir.as_deref().map(|dir| plugins::Grants::load(dir).unwrap_or_default()).unwrap_or_default();
        let servers = match plugins::session_servers(&plugins, &grants, &wanted, self.store.flags(id).caller.as_ref()) {
            Ok(servers) => servers,
            Err(missing) => {
                let text = format!(
                    "{plugin} runs only on local models, and its tool server needs host variables that are not set where Local Studio runs ({}), so Local Studio did not send it. Set them before Local Studio starts, then try again.",
                    missing.join("; ")
                );
                self.refuse(id, plugin, text, None, prompt, request);
                return Ok(Gate::Refused);
            }
        };
        let names: Vec<String> = servers
            .iter()
            .filter_map(|server| server.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        let running = self.running(id);
        let served = self
            .threads
            .get(id)
            .and_then(|thread| thread.served.as_ref())
            .is_some_and(|(session, served)| session == active && names.iter().all(|name| served.contains(name)));
        if running && !served && !servers.is_empty() {
            if !again {
                self.hold(id, prompt, delivery, request);
            }
            return Ok(Gate::Held);
        }
        self.pin_local(id, active, &provider, &model, (!running).then_some(servers)).await?;
        self.store.update(id, |flags| flags.local_only = Some(plugin.clone()));
        self.save();
        Ok(Gate::Send)
    }

    async fn session_local(&self, active: &str, provider: &str) -> Result<(), String> {
        if active.is_empty() {
            return Ok(());
        }
        let command = DaemonCommand::GetState { id: None, active_session_id: active.to_string(), rest: Map::default() };
        let state =
            self.call(command).await.map_err(|error| format!("an address Local Studio could not read ({error})"))?;
        let model = state.get("model");
        let resolved = model.and_then(|model| model.get("provider")).and_then(Value::as_str);
        let base = model.and_then(|model| model.get("baseUrl")).and_then(Value::as_str);
        let session = Provider { id: provider.to_string(), base_url: base.map(str::to_string) };
        if resolved == Some(provider) && session.is_local() {
            return Ok(());
        }
        Err(match (resolved, base) {
            (Some(resolved), Some(base)) if resolved == provider => base.to_string(),
            (Some(resolved), _) if resolved != provider => format!("{resolved}, not {provider}"),
            _ => "an address it does not report".to_string(),
        })
    }

    pub(super) fn hold_behind(
        &mut self,
        id: &str,
        prompt: &PromptDraft,
        delivery: Delivery,
        request: Option<String>,
    ) -> bool {
        if self.store.threads.get(id).is_none_or(|flags| flags.held.is_empty()) {
            return false;
        }
        self.hold(id, prompt, delivery, request);
        if !self.running(id) {
            self.itself.send(super::Command::SendHeld { thread: id.to_string() });
        }
        true
    }

    pub(super) fn hold(&mut self, id: &str, prompt: &PromptDraft, delivery: Delivery, request: Option<String>) {
        let held = HeldPrompt::new(prompt, delivery, request);
        self.store.update(id, |flags| flags.held.push(held));
        self.save();
        self.mailbox.notice(
            NoticeLevel::Info,
            "This message waits for the current turn to end, so a local-only plugin's tool server can start first.",
        );
    }

    pub(super) fn held_threads(&self) -> Vec<String> {
        self.store.threads.iter().filter(|(_, flags)| !flags.held.is_empty()).map(|(id, _)| id.clone()).collect()
    }

    pub(super) async fn send_held(&mut self, id: &str) {
        if self.running(id) {
            return;
        }
        while let Some(next) = self.store.flags(id).held.first().cloned() {
            let prompt = next.prompt();
            let result = self.send_prompt_from(id, prompt.clone(), next.delivery(), next.request.clone(), true).await;
            if result == Ok(Gate::Held) {
                return;
            }
            self.store.update(id, |flags| {
                if flags.held.first() == Some(&next) {
                    flags.held.remove(0);
                }
            });
            self.save();
            if let Err(error) = result {
                match next.request {
                    Some(request) => self.mailbox.push(Update::RequestFailed { request, message: error.clone() }),
                    None => self.mailbox.push(Update::ComposerDraft { thread: id.to_string(), draft: prompt }),
                }
                self.mailbox.notice(NoticeLevel::Error, error);
            }
        }
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
            None => self.mailbox.push(Update::ComposerDraft { thread: id.to_string(), draft: prompt.clone() }),
        }
        self.mailbox.push(Update::LocalOnly { thread: id.to_string(), plugin, message, local });
    }

    async fn network_servers(&self, active: &str, plugins: &[Plugin]) -> Result<(Vec<String>, Vec<String>), String> {
        let command =
            DaemonCommand::GetMcpConnections { id: None, active_session_id: active.to_string(), rest: Map::default() };
        let data =
            self.call(command).await.map_err(|error| format!("Could not read this session's MCP servers: {error}"))?;
        let ours: Vec<String> = plugins.iter().filter(|plugin| plugin.local_only).map(Plugin::mcp_name).collect();
        let listed: Vec<String> = data
            .get("connections")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|connection| {
                let flag = |key: &str| connection.get(key).and_then(Value::as_bool) != Some(false);
                flag("userDeclared") || flag("connected")
            })
            .filter_map(|connection| connection.get("server").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        let (clashes, open) = listed.into_iter().partition(|server| ours.contains(server));
        Ok((open, clashes))
    }

    fn holds_tools(&self, active: &str) -> bool {
        self.ask_sent
            .get(active)
            .and_then(Value::as_array)
            .is_some_and(|sent| sent.iter().any(|one| one["name"] == tools::SERVER))
    }

    async fn drop_tools(&mut self, active: &str) -> Result<(), String> {
        let sent = self.ask_sent.get(active).and_then(Value::as_array).cloned().unwrap_or_default();
        let servers = Value::Array(sent.into_iter().filter(|one| one["name"] != tools::SERVER).collect());
        let command = DaemonCommand::ReplaceAcpMcpServers {
            id: None,
            active_session_id: active.to_string(),
            owner_id: plugins::ask::NOTE_TYPE.to_string(),
            servers: servers.clone(),
            rest: Map::default(),
        };
        self.call(command).await.map_err(|error| format!("Could not take browser and computer use off: {error}"))?;
        self.ask_sent.insert(active.to_string(), servers);
        Ok(())
    }

    async fn pin_local(
        &mut self,
        id: &str,
        active: &str,
        provider: &str,
        model: &str,
        servers: Option<Vec<Value>>,
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
        let Some(servers) = servers else { return Ok(()) };
        let names: Vec<String> = servers
            .iter()
            .filter_map(|server| server.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        let command = DaemonCommand::ReplaceAcpMcpServers {
            id: None,
            active_session_id: active.to_string(),
            owner_id: plugins::SESSION_OWNER.to_string(),
            servers: Value::Array(servers),
            rest: Map::default(),
        };
        self.call(command).await.map_err(|error| format!("Could not start the plugin's tool server: {error}"))?;
        if let Some(thread) = self.threads.get_mut(id) {
            thread.served = Some((active.to_string(), names));
        }
        Ok(())
    }

    pub(super) async fn run_local(&mut self, id: &str, local: ModelKey) {
        let Some(prompt) = self.threads.get_mut(id).and_then(|thread| thread.refused.take()) else {
            self.mailbox.notice(NoticeLevel::Info, "That message was already sent or dropped.");
            return;
        };
        let result = if !Self::local_provider(&local.provider) {
            Err(format!("{} is not a local model.", local.model))
        } else if let Err(error) = self.apply_model(id, Some(local.provider), Some(local.model), None).await {
            Err(error)
        } else {
            self.send_prompt(id, prompt.clone(), Delivery::Queue, None).await
        };
        let refused = self.threads.get(id).is_some_and(|thread| thread.refused.is_some());
        match result {
            Ok(()) if !refused => {
                self.mailbox.push(Update::ComposerText { thread: id.to_string(), text: String::new() });
            }
            Ok(()) => {}
            Err(error) => {
                if let Some(thread) = self.threads.get_mut(id) {
                    thread.refused = Some(prompt.clone());
                }
                self.mailbox.push(Update::ComposerDraft { thread: id.to_string(), draft: prompt });
                self.mailbox.notice(NoticeLevel::Error, error);
            }
        }
    }
}
