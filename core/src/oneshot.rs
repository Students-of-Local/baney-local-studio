use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use eukhe_client::daemon_client::{DaemonClient, LONG_RUNNING_REQUEST_TIMEOUT_MS};
use eukhe_client::saved_sessions;
use eukhe_types::daemon::{DaemonClientKind, DaemonCommand, DaemonSessionLifecycle};
use serde_json::{json, Map, Value};
use tokio::sync::{oneshot, watch};

use crate::session::model_ref;
use crate::workspace::providers::{claude_signed_in, eukhe_refuses};

#[derive(Clone)]
pub(crate) struct TextGen {
    client: watch::Receiver<Option<DaemonClient>>,
    session_dir: Option<PathBuf>,
    agent_dir: Option<PathBuf>,
    home: String,
    answers: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
}

impl TextGen {
    pub(crate) fn new(
        client: watch::Receiver<Option<DaemonClient>>,
        session_dir: Option<PathBuf>,
        agent_dir: Option<PathBuf>,
        home: String,
    ) -> Self {
        Self { client, session_dir, agent_dir, home, answers: Arc::default() }
    }

    pub(crate) fn answered(&self, event: &Value) {
        let Some(request) = event.get("id").and_then(Value::as_str) else { return };
        if event.get("status").and_then(Value::as_str) == Some("running") {
            return;
        }
        let sender = self.answers.lock().unwrap_or_else(PoisonError::into_inner).remove(request);
        if let Some(sender) = sender {
            let _ = sender.send(event.clone());
        }
    }

    pub(crate) async fn one_shot(
        &self,
        prompt: String,
        provider: Option<String>,
        model: Option<String>,
    ) -> Result<String, String> {
        eukhe_refuses(self.agent_dir.as_deref(), provider.as_deref())?;
        let client = self.client.borrow().clone().ok_or_else(|| "Not connected to the Eukhe daemon.".to_string())?;
        let unchecked = provider.is_none() && claude_signed_in(self.agent_dir.as_deref());
        let mut config = json!({ "cwd": self.home });
        if let Some(dir) = &self.session_dir {
            config["sessionDir"] = Value::String(dir.to_string_lossy().into_owned());
        }
        if let (Some(provider), Some(model)) = (provider, model) {
            config["provider"] = Value::String(provider);
            config["model"] = Value::String(model);
        }
        let created = client
            .request_ok(DaemonCommand::Create {
                id: None,
                session_path: None,
                continue_recent: None,
                no_session: Some(true),
                name: None,
                config: Some(config),
                runtime_metadata: None,
                lifecycle: Some(DaemonSessionLifecycle::ClientOwned),
                env: None,
                launch_env: None,
                rest: Map::default(),
            })
            .await
            .map_err(|error| format!("{error:#}"))?;
        let active = created
            .get("activeSessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| "The daemon opened a session without an id.".to_string())?
            .to_string();
        let answer = if unchecked {
            match resolved_provider(&client, &active).await {
                Ok(resolved) => match eukhe_refuses(self.agent_dir.as_deref(), resolved.as_deref()) {
                    Ok(()) => self.ask(&client, &active, prompt).await,
                    Err(refused) => Err(refused),
                },
                Err(error) => Err(error),
            }
        } else {
            self.ask(&client, &active, prompt).await
        };
        let killed =
            client.request_ok(DaemonCommand::Kill { id: None, active_session_id: active, rest: Map::default() }).await;
        let answer = answer?;
        killed.map_err(|error| format!("{error:#}"))?;
        Ok(answer)
    }

    async fn ask(&self, client: &DaemonClient, active: &str, prompt: String) -> Result<String, String> {
        let request = format!("oneshot:{active}");
        let (sender, receiver) = oneshot::channel();
        self.answers.lock().unwrap_or_else(PoisonError::into_inner).insert(request.clone(), sender);
        let event = async {
            saved_sessions::attach(client, active, DaemonClientKind::Watch)
                .await
                .map_err(|error| format!("{error:#}"))?;
            client
                .request_ok(DaemonCommand::StartSideQuestion {
                    id: None,
                    active_session_id: active.to_string(),
                    side_question_id: request.clone(),
                    question: prompt,
                    previous_turns: None,
                    rest: Map::default(),
                })
                .await
                .map_err(|error| format!("{error:#}"))?;
            tokio::time::timeout(Duration::from_millis(LONG_RUNNING_REQUEST_TIMEOUT_MS), receiver)
                .await
                .map_err(|_| format!("The model did not answer within {} s.", LONG_RUNNING_REQUEST_TIMEOUT_MS / 1000))?
                .map_err(|_| "The text request was dropped.".to_string())
        }
        .await;
        self.answers.lock().unwrap_or_else(PoisonError::into_inner).remove(&request);
        let event = event?;
        let status = event.get("status").and_then(Value::as_str).unwrap_or_default();
        if status != "complete" {
            let reason = event.get("errorMessage").and_then(Value::as_str).unwrap_or(status);
            return Err(format!("The model did not answer: {reason}"));
        }
        event
            .get("answer")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| "The model returned no text.".to_string())
    }
}

pub(crate) async fn resolved_provider(client: &DaemonClient, active: &str) -> Result<Option<String>, String> {
    let state = client
        .request_ok(DaemonCommand::GetState { id: None, active_session_id: active.to_string(), rest: Map::default() })
        .await
        .map_err(|error| format!("{error:#}"))?;
    Ok(state.get("model").and_then(model_ref).map(|model| model.provider))
}
