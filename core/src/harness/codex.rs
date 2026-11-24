use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use eukhe_types::daemon::{SessionEvent, StreamUpdate};
use eukhe_types::session::AgentMessage;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use super::{
    assistant, command, diff_details, emit_message, notice, now_ms, output, program, stop_reason, tool_result, trust,
    user, Agent, Event, Failure, Image, Launch, Sink, Spend, Stop, CODEX_PROVIDER,
};
use crate::model::{AccessMode, Approval, ApprovalAnswer, ContextUsage, ModelOption};

const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const CONTEXT_WINDOW: u64 = 272_000;
const REMEMBERED_TURNS: usize = 16;

type Replies = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

#[derive(Clone)]
struct Rpc {
    lines: mpsc::UnboundedSender<String>,
    replies: Replies,
    next: Arc<AtomicU64>,
}

impl Rpc {
    fn call(&self, method: &str, params: Value) -> impl Future<Output = Result<Value, String>> + Send + 'static {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        self.replies.lock().unwrap_or_else(PoisonError::into_inner).insert(id, reply);
        let mut frame = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        frame["params"] = params;
        let sent = self.lines.send(frame.to_string()).map_err(|_| "Codex has stopped.".to_string());
        let (replies, method) = (Arc::clone(&self.replies), method.to_string());
        async move {
            sent?;
            match tokio::time::timeout(CALL_TIMEOUT, answer).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err("Codex stopped before it answered.".to_string()),
                Err(_) => {
                    replies.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
                    Err(format!("Codex did not answer {method} within {} s.", CALL_TIMEOUT.as_secs()))
                }
            }
        }
    }

    fn notify(&self, method: &str, params: &Value) {
        self.lines.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string()).ok();
    }

    fn respond(&self, id: &Value, result: &Value) -> Result<(), String> {
        self.lines
            .send(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string())
            .map_err(|_| "Codex has stopped.".to_string())
    }

    fn refuse(&self, id: &Value, message: &str) {
        self.lines
            .send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": message } }).to_string())
            .ok();
    }
}

struct Process {
    rpc: Rpc,
    kill: Option<oneshot::Sender<()>>,
}

#[derive(Clone)]
struct PendingApproval {
    id: Value,
    legacy: bool,
}

#[derive(Default)]
struct Turns {
    running: Option<String>,
    starting: bool,
    launching: Option<Value>,
    finished: VecDeque<String>,
    held: VecDeque<Value>,
    thread: String,
    stop: bool,
}

impl Turns {
    fn busy(&self) -> bool {
        self.running.is_some() || self.starting
    }

    fn started(&mut self, id: &str) -> Option<Value> {
        if self.finished.iter().any(|done| done == id) {
            return None;
        }
        self.running = Some(id.to_string());
        self.starting = false;
        self.launching = None;
        std::mem::take(&mut self.stop).then(|| json!({ "threadId": self.thread, "turnId": id }))
    }

    fn completed(&mut self, id: Option<&str>) -> bool {
        if let Some(id) = id {
            self.finished.push_back(id.to_string());
            if self.finished.len() > REMEMBERED_TURNS {
                self.finished.pop_front();
            }
            if self.running.as_deref().is_some_and(|running| running != id) {
                return false;
            }
        }
        self.running = None;
        self.starting = false;
        self.stop = false;
        true
    }

    fn next(&mut self) -> Option<Value> {
        self.launching = self.held.pop_front();
        self.starting = self.launching.is_some();
        self.launching.clone()
    }

    fn forget(&mut self) -> VecDeque<Value> {
        self.running = None;
        self.starting = false;
        self.stop = false;
        let mut dropped = std::mem::take(&mut self.held);
        if let Some(launching) = self.launching.take() {
            dropped.push_front(launching);
        }
        dropped
    }
}

type SharedTurns = Arc<Mutex<Turns>>;

fn request_key(id: &Value) -> String {
    match id {
        Value::String(id) => id.clone(),
        other => other.to_string(),
    }
}

fn interrupt_turn(rpc: &Rpc, sink: Sink, params: Option<Value>) {
    let Some(params) = params else { return };
    let reply = rpc.call("turn/interrupt", params);
    tokio::spawn(async move {
        if let Err(error) = reply.await {
            sink.send(Event::Failed(Failure::Interrupt, error));
        }
    });
}

fn start_turn(rpc: Rpc, sink: Sink, turns: SharedTurns, mut params: Value) {
    let mut reply = rpc.call("turn/start", params.clone());
    tokio::spawn(async move {
        loop {
            match reply.await {
                Ok(started) => {
                    if let Some(id) = started.get("turn").and_then(|turn| turn.get("id")).and_then(Value::as_str) {
                        let stop = turns.lock().unwrap_or_else(PoisonError::into_inner).started(id);
                        interrupt_turn(&rpc, sink, stop);
                    }
                    return;
                }
                Err(error) => {
                    let next = {
                        let mut turns = turns.lock().unwrap_or_else(PoisonError::into_inner);
                        if turns.launching.take().is_none() {
                            return;
                        }
                        turns.next()
                    };
                    let text = format!("Codex did not start this queued message ({error}): {}", held_text(&params));
                    if let Some(message) = notice(&text, now_ms()) {
                        emit_message(&sink, message);
                    }
                    let Some(next) = next else {
                        sink.send(Event::Idle);
                        return;
                    };
                    reply = rpc.call("turn/start", next.clone());
                    params = next;
                }
            }
        }
    });
}

fn report_dropped(sink: &Sink, held: VecDeque<Value>, lead: &str) {
    for params in held {
        if let Some(message) = notice(&format!("{lead} this queued message: {}", held_text(&params)), now_ms()) {
            emit_message(sink, message);
        }
    }
}

fn usage_limit(error: &Value) -> bool {
    error.get("codexErrorInfo").and_then(Value::as_str) == Some("usageLimitExceeded")
}

fn held_text(params: &Value) -> String {
    params["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) struct Codex {
    rpc: Rpc,
    sink: Sink,
    thread: String,
    turns: SharedTurns,
    approvals: Arc<Mutex<HashMap<String, PendingApproval>>>,
    access: AccessMode,
    model: Option<String>,
    effort: Option<String>,
    cwd: String,
    kill: Option<oneshot::Sender<()>>,
}

fn approval_policy(access: AccessMode) -> Value {
    match access {
        AccessMode::Supervised => json!("untrusted"),
        AccessMode::AutoAcceptEdits | AccessMode::Auto => json!("on-request"),
        AccessMode::FullAccess => json!({
            "granular": {
                "mcp_elicitations": true,
                "request_permissions": false,
                "rules": false,
                "sandbox_approval": false,
                "skill_approval": false,
            }
        }),
    }
}

fn sandbox(access: AccessMode) -> &'static str {
    match access {
        AccessMode::Supervised | AccessMode::AutoAcceptEdits | AccessMode::Auto => "workspace-write",
        AccessMode::FullAccess => "danger-full-access",
    }
}

fn sandbox_policy(access: AccessMode) -> Value {
    match access {
        AccessMode::Supervised | AccessMode::AutoAcceptEdits | AccessMode::Auto => json!({ "type": "workspaceWrite" }),
        AccessMode::FullAccess => json!({ "type": "dangerFullAccess" }),
    }
}

fn reviewer(access: AccessMode) -> &'static str {
    match access {
        AccessMode::Auto => "auto_review",
        AccessMode::Supervised | AccessMode::AutoAcceptEdits | AccessMode::FullAccess => "user",
    }
}

#[allow(clippy::too_many_lines)]
fn start(
    account: Option<&Path>,
    sink: Option<Sink>,
    approvals: Arc<Mutex<HashMap<String, PendingApproval>>>,
    turns: SharedTurns,
) -> Result<Process, String> {
    let binary = program("LOCAL_STUDIO_CODEX_BIN", "codex");
    let mut command = command(&binary, &crate::platform::home_or_root());
    command.arg("app-server");
    if let Some(account) = account {
        command.env("CODEX_HOME", account);
    }
    let mut agent =
        Agent::new(command.spawn().map_err(|error| format!("Could not start {}: {error}", binary.display()))?, &binary);
    let stdin = agent.child.stdin.take().ok_or_else(|| "Codex has no input pipe.".to_string())?;
    let stdout = agent.child.stdout.take().ok_or_else(|| "Codex has no output pipe.".to_string())?;
    let stderr = agent.child.stderr.take().ok_or_else(|| "Codex has no error pipe.".to_string())?;
    let (lines, mut outgoing) = mpsc::unbounded_channel::<String>();
    let (kill, killed) = oneshot::channel::<()>();
    let replies: Replies = Arc::new(Mutex::new(HashMap::new()));
    let rpc = Rpc { lines, replies: Arc::clone(&replies), next: Arc::new(AtomicU64::new(1)) };
    tokio::spawn(async move {
        let mut stdin = stdin;
        while let Some(line) = outgoing.recv().await {
            if stdin.write_all(line.as_bytes()).await.is_err() || stdin.write_all(b"\n").await.is_err() {
                break;
            }
            if stdin.flush().await.is_err() {
                break;
            }
        }
    });
    let tail = Arc::new(Mutex::new(Vec::<String>::new()));
    let errors = Arc::clone(&tail);
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let mut tail = errors.lock().unwrap_or_else(PoisonError::into_inner);
            tail.push(line);
            if tail.len() > 8 {
                tail.remove(0);
            }
        }
    });
    let mut translator = sink.clone().map(|sink| Translator::new(sink, rpc.clone(), approvals, turns));
    let reader_replies = Arc::clone(&replies);
    let reader_rpc = rpc.clone();
    let done = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let Ok(frame) = serde_json::from_str::<Value>(&line) else { continue };
            let method = frame.get("method").and_then(Value::as_str);
            match (frame.get("id"), method) {
                (Some(id), None) => {
                    let Some(id) = id.as_u64() else { continue };
                    let Some(reply) = reader_replies.lock().unwrap_or_else(PoisonError::into_inner).remove(&id) else {
                        continue;
                    };
                    let result = match frame.get("error") {
                        Some(error) => Err(error
                            .get("message")
                            .and_then(Value::as_str)
                            .map_or_else(|| error.to_string(), str::to_string)),
                        None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    reply.send(result).ok();
                }
                (Some(id), Some(method)) => match translator.as_mut() {
                    Some(translator) => translator.request(id, method, frame.get("params").unwrap_or(&Value::Null)),
                    None => reader_rpc.refuse(id, "Local Studio does not handle this request."),
                },
                (None, Some(method)) => {
                    if let Some(translator) = translator.as_mut() {
                        translator.notification(method, frame.get("params").unwrap_or(&Value::Null));
                    }
                }
                (None, None) => {}
            }
        }
        if let Some(translator) = translator.as_mut() {
            translator.finish();
        }
    });
    tokio::spawn(async move {
        let status = tokio::select! {
            status = agent.child.wait() => status.ok(),
            _ = killed => {
                agent.kill().await;
                None
            }
        };
        done.await.ok();
        replies.lock().unwrap_or_else(PoisonError::into_inner).clear();
        if let Some(sink) = sink {
            let failure = status.filter(|status| !status.success()).map(|status| {
                let tail = tail.lock().unwrap_or_else(PoisonError::into_inner).join("\n");
                format!("Codex exited ({status}): {tail}")
            });
            sink.send(Event::Exited(failure));
        }
    });
    Ok(Process { rpc, kill: Some(kill) })
}

async fn initialize(rpc: &Rpc) -> Result<(), String> {
    rpc.call(
        "initialize",
        json!({ "clientInfo": { "name": "local-studio", "title": "Local Studio", "version": crate::VERSION } }),
    )
    .await?;
    rpc.notify("initialized", &json!({}));
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Listed {
    id: String,
    name: String,
    efforts: Vec<String>,
}

impl Listed {
    fn read(model: &Value) -> Option<Self> {
        let id = model.get("id").and_then(Value::as_str)?.to_string();
        Some(Self {
            name: model.get("displayName").and_then(Value::as_str).unwrap_or(&id).to_string(),
            id,
            efforts: model
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|effort| effort.get("reasoningEffort").and_then(Value::as_str).map(str::to_string))
                .collect(),
        })
    }

    fn option(self) -> ModelOption {
        ModelOption {
            provider: CODEX_PROVIDER.to_string(),
            id: self.id,
            name: format!("{} (Codex)", self.name),
            reasoning: true,
            thinking_levels: self.efforts,
            context_window: CONTEXT_WINDOW,
            configured: true,
            service_tiers: Vec::new(),
        }
    }
}

fn models_file(data_dir: &Path) -> PathBuf {
    data_dir.join("harness").join("codex-models.json")
}

pub(crate) fn saved_models(data_dir: &Path) -> Vec<ModelOption> {
    let listed: Option<Vec<Listed>> =
        std::fs::read(models_file(data_dir)).ok().and_then(|text| serde_json::from_slice(&text).ok());
    listed.unwrap_or_default().into_iter().map(Listed::option).collect()
}

pub(crate) async fn models(data_dir: &Path) -> Result<Vec<ModelOption>, String> {
    let mut process = start(None, None, Arc::default(), Arc::default())?;
    let listed = async {
        initialize(&process.rpc).await?;
        process.rpc.call("model/list", json!({})).await
    }
    .await;
    if let Some(kill) = process.kill.take() {
        kill.send(()).ok();
    }
    let listed = listed?;
    let listed: Vec<Listed> = listed
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|model| model.get("hidden").and_then(Value::as_bool) != Some(true))
        .filter_map(Listed::read)
        .collect();
    crate::workspace::settings::save_json(&models_file(data_dir), &listed)?;
    Ok(listed.into_iter().map(Listed::option).collect())
}

pub(crate) async fn read(account: Option<&Path>, thread: &str) -> Result<Vec<AgentMessage>, String> {
    let mut process = start(account, None, Arc::default(), Arc::default())?;
    let read = async {
        initialize(&process.rpc).await?;
        process.rpc.call("thread/read", json!({ "threadId": thread, "includeTurns": true })).await
    }
    .await;
    if let Some(kill) = process.kill.take() {
        kill.send(()).ok();
    }
    Ok(history(read?.get("thread").unwrap_or(&Value::Null)))
}

impl Codex {
    pub(crate) async fn spawn(launch: &Launch, sink: Sink) -> Result<(Self, String, Vec<AgentMessage>), String> {
        if !Path::new(&launch.cwd).is_dir() {
            return Err(format!("This thread's folder is gone: {}", launch.cwd));
        }
        let approvals = Arc::new(Mutex::new(HashMap::new()));
        let turns = SharedTurns::default();
        let mut process =
            start(launch.account.as_deref(), Some(sink.clone()), Arc::clone(&approvals), Arc::clone(&turns))?;
        let opened = async {
            initialize(&process.rpc).await?;
            let mut params = json!({
                "cwd": launch.cwd,
                "approvalPolicy": approval_policy(launch.access),
                "approvalsReviewer": reviewer(launch.access),
                "sandbox": sandbox(launch.access),
            });
            if let Some(model) = launch.model.as_deref().filter(|model| !model.is_empty()) {
                params["model"] = json!(model);
            }
            if !trust::own(&launch.cwd) {
                let level = json!({ "trust_level": if launch.project { "trusted" } else { "untrusted" } });
                let projects: Map<String, Value> =
                    trust::folders(&launch.cwd).into_iter().map(|folder| (folder, level.clone())).collect();
                params["config"] = json!({ "projects": projects });
            }
            if launch.resume {
                params["threadId"] = json!(launch.native);
                let resumed = process.rpc.call("thread/resume", params).await?;
                let thread = resumed.get("thread").cloned().unwrap_or(Value::Null);
                Ok::<_, String>((launch.native.clone(), history(&thread)))
            } else {
                let started = process.rpc.call("thread/start", params).await?;
                let id = started
                    .get("thread")
                    .and_then(|thread| thread.get("id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Codex started a thread without an id.".to_string())?;
                Ok((id.to_string(), Vec::new()))
            }
        }
        .await;
        let (thread, history) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                if let Some(kill) = process.kill.take() {
                    kill.send(()).ok();
                }
                return Err(error);
            }
        };
        turns.lock().unwrap_or_else(PoisonError::into_inner).thread.clone_from(&thread);
        let codex = Self {
            rpc: process.rpc.clone(),
            sink,
            thread: thread.clone(),
            turns,
            approvals,
            access: launch.access,
            model: launch.model.clone(),
            effort: launch.effort.clone(),
            cwd: launch.cwd.clone(),
            kill: process.kill.take(),
        };
        Ok((codex, thread, history))
    }

    pub(crate) fn send(
        &mut self,
        text: &str,
        images: &[Image],
        steer: bool,
        request: Option<String>,
    ) -> Result<(), String> {
        if !images.is_empty() {
            return Err("Codex threads in Local Studio do not take images yet.".to_string());
        }
        let input = json!([{ "type": "text", "text": text, "text_elements": [] }]);
        let params = self.turn_params(&input);
        let running = {
            let mut turns = self.turns.lock().unwrap_or_else(PoisonError::into_inner);
            if turns.busy() && (!steer || turns.running.is_none()) {
                turns.held.push_back(params);
                return Ok(());
            }
            turns.starting = turns.running.is_none();
            turns.running.clone()
        };
        let (rpc, sink, turns) = (self.rpc.clone(), self.sink.clone(), Arc::clone(&self.turns));
        if let Some(turn) = running {
            let steer = json!({ "threadId": self.thread, "expectedTurnId": turn, "input": input });
            let reply = rpc.call("turn/steer", steer);
            tokio::spawn(async move {
                if let Err(error) = reply.await {
                    sink.send(Event::Failed(Failure::Send(request), error));
                }
            });
            return Ok(());
        }
        let reply = rpc.call("turn/start", params);
        tokio::spawn(async move {
            match reply.await {
                Ok(started) => {
                    if let Some(id) = started.get("turn").and_then(|turn| turn.get("id")).and_then(Value::as_str) {
                        let stop = turns.lock().unwrap_or_else(PoisonError::into_inner).started(id);
                        interrupt_turn(&rpc, sink, stop);
                    }
                }
                Err(error) => {
                    let next = {
                        let mut turns = turns.lock().unwrap_or_else(PoisonError::into_inner);
                        turns.starting = false;
                        turns.stop = false;
                        if turns.running.is_none() {
                            turns.next()
                        } else {
                            None
                        }
                    };
                    sink.send(Event::Failed(Failure::Send(request), error));
                    match next {
                        Some(next) => start_turn(rpc, sink, turns, next),
                        None => sink.send(Event::Idle),
                    }
                }
            }
        });
        Ok(())
    }

    fn call(&self, method: &'static str, params: Value, failure: Failure) {
        let (reply, sink) = (self.rpc.call(method, params), self.sink.clone());
        tokio::spawn(async move {
            if let Err(error) = reply.await {
                sink.send(Event::Failed(failure, error));
            }
        });
    }

    fn turn_params(&self, input: &Value) -> Value {
        let mut params = json!({
            "threadId": self.thread,
            "input": input,
            "cwd": self.cwd,
            "approvalPolicy": approval_policy(self.access),
            "approvalsReviewer": reviewer(self.access),
            "sandboxPolicy": sandbox_policy(self.access),
        });
        if let Some(model) = self.model.as_deref().filter(|model| !model.is_empty()) {
            params["model"] = json!(model);
        }
        if let Some(effort) = self.effort.as_deref().filter(|effort| !effort.is_empty()) {
            params["effort"] = json!(effort);
        }
        params
    }

    pub(crate) fn interrupt(&mut self) -> Result<(), String> {
        let (turn, dropped, deferred) = {
            let mut turns = self.turns.lock().unwrap_or_else(PoisonError::into_inner);
            let deferred = turns.running.is_none() && turns.starting && turns.launching.is_none();
            turns.stop = deferred;
            (turns.running.clone(), std::mem::take(&mut turns.held), deferred)
        };
        report_dropped(&self.sink, dropped, "Stop dropped");
        if deferred {
            return Ok(());
        }
        let turn = turn.ok_or_else(|| "Codex has no turn running.".to_string())?;
        self.call("turn/interrupt", json!({ "threadId": self.thread, "turnId": turn }), Failure::Interrupt);
        Ok(())
    }

    pub(crate) fn answer(&mut self, request: &str, answer: ApprovalAnswer) -> Result<(), String> {
        let pending = self
            .approvals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(request)
            .ok_or_else(|| "That approval is no longer pending.".to_string())?;
        let decision = match (answer, pending.legacy) {
            (ApprovalAnswer::Approve, false) => "accept",
            (ApprovalAnswer::AlwaysAllowSession, false) => "acceptForSession",
            (ApprovalAnswer::Deny, false) => "decline",
            (ApprovalAnswer::Approve, true) => "approved",
            (ApprovalAnswer::AlwaysAllowSession, true) => "approved_for_session",
            (ApprovalAnswer::Deny, true) => "denied",
        };
        self.rpc.respond(&pending.id, &json!({ "decision": decision }))
    }

    pub(crate) fn set_access(&mut self, access: AccessMode) {
        self.access = access;
    }

    pub(crate) fn set_model(&mut self, model: &str) {
        self.model = Some(model.to_string());
    }

    pub(crate) fn set_effort(&mut self, effort: &str) {
        self.effort = Some(effort.to_string());
    }

    pub(crate) fn rename(&mut self, title: &str) {
        self.call("thread/name/set", json!({ "threadId": self.thread, "name": title }), Failure::Rename);
    }

    pub(crate) fn compact(&mut self) {
        self.call("thread/compact/start", json!({ "threadId": self.thread }), Failure::Compact);
    }

    pub(crate) fn kill(&mut self) {
        if let Some(kill) = self.kill.take() {
            kill.send(()).ok();
        }
    }
}

impl Drop for Codex {
    fn drop(&mut self) {
        self.kill();
    }
}

struct Live {
    kind: String,
    text: String,
    started: u64,
}

struct Translator {
    sink: Sink,
    rpc: Rpc,
    approvals: Arc<Mutex<HashMap<String, PendingApproval>>>,
    turns: SharedTurns,
    busy: bool,
    model: String,
    items: HashMap<String, Live>,
    commands: HashMap<String, String>,
    failure: Option<String>,
    total: Option<Spend>,
    stopped: bool,
    resets: Option<u64>,
}

impl Translator {
    fn new(sink: Sink, rpc: Rpc, approvals: Arc<Mutex<HashMap<String, PendingApproval>>>, turns: SharedTurns) -> Self {
        Self {
            sink,
            rpc,
            approvals,
            turns,
            busy: false,
            model: String::new(),
            items: HashMap::new(),
            commands: HashMap::new(),
            failure: None,
            total: None,
            stopped: false,
            resets: None,
        }
    }

    fn begin(&mut self) {
        if !self.busy {
            self.busy = true;
            self.failure = None;
            self.sink.session(SessionEvent::AgentStart);
        }
    }

    fn end(&mut self, error: Option<String>) {
        let settled: Vec<String> =
            self.approvals.lock().unwrap_or_else(PoisonError::into_inner).drain().map(|(id, _)| id).collect();
        for id in settled {
            self.sink.send(Event::ApprovalSettled(id));
        }
        if !self.busy {
            return;
        }
        self.busy = false;
        self.items.clear();
        self.sink.session(SessionEvent::TurnEnd { answer: None, error });
        self.sink.session(SessionEvent::AgentEnd { messages: Vec::new() });
    }

    fn stop(&mut self, reason: String, limit: bool) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        let mut stop = Stop::from_reason(reason);
        stop.limit |= limit;
        if stop.limit {
            stop.resets = self.resets;
        }
        let dropped = self.turns.lock().unwrap_or_else(PoisonError::into_inner).forget();
        if let Some(message) = notice(&stop.reason, now_ms()) {
            emit_message(&self.sink, message);
        }
        report_dropped(&self.sink, dropped, "The account stop dropped");
        self.end((!stop.limit).then(|| stop.reason.clone()));
        self.sink.send(Event::Stopped(stop));
    }

    fn rate_limits(&mut self, limits: Option<&Value>) {
        let resets = ["primary", "secondary"]
            .iter()
            .filter_map(|window| limits.and_then(|limits| limits.get(*window)))
            .filter(|window| window.get("usedPercent").and_then(Value::as_f64).is_some_and(|used| used >= 100.0))
            .filter_map(|window| window.get("resetsAt").and_then(Value::as_u64))
            .max();
        if let Some(seconds) = resets {
            self.resets = Some(seconds.saturating_mul(1000));
        }
    }

    fn finish(&mut self) {
        let dropped = self.turns.lock().unwrap_or_else(PoisonError::into_inner).forget();
        report_dropped(&self.sink, dropped, "Codex exited and dropped");
        self.end(None);
    }

    fn message(&self, kind: &str, text: &str, stop: &str, at: u64) -> Option<AgentMessage> {
        let block = if kind == "reasoning" {
            json!({ "type": "thinking", "thinking": text })
        } else {
            json!({ "type": "text", "text": text })
        };
        assistant(CODEX_PROVIDER, &self.model, &[block], &Spend::default(), stop, None, at)
    }

    fn notification(&mut self, method: &str, params: &Value) {
        let item = params.get("item");
        let text = |value: Option<&Value>, key: &str| {
            value.and_then(|value| value.get(key)).and_then(Value::as_str).unwrap_or_default().to_string()
        };
        match method {
            "turn/started" => {
                if let Some(id) = params.get("turn").and_then(|turn| turn.get("id")).and_then(Value::as_str) {
                    let stop = self.turns.lock().unwrap_or_else(PoisonError::into_inner).started(id);
                    interrupt_turn(&self.rpc, self.sink.clone(), stop);
                }
                self.begin();
            }
            "serverRequest/resolved" => {
                let Some(id) = params.get("requestId") else { return };
                let key = request_key(id);
                if self.approvals.lock().unwrap_or_else(PoisonError::into_inner).remove(&key).is_some() {
                    self.sink.send(Event::ApprovalSettled(key));
                }
            }
            "turn/completed" => {
                let turn = params.get("turn");
                let id = turn.and_then(|turn| turn.get("id")).and_then(Value::as_str);
                let status = text(turn, "status");
                let turn_error = turn.and_then(|turn| turn.get("error")).filter(|error| !error.is_null());
                let limit = turn_error.is_some_and(usage_limit);
                let error = turn_error.map(|error| {
                    error.get("message").and_then(Value::as_str).map_or_else(|| error.to_string(), str::to_string)
                });
                let failure = if status == "failed" {
                    let reason = error
                        .or_else(|| self.failure.take())
                        .unwrap_or_else(|| "Codex ended the turn with an error.".to_string());
                    if let Some(stop) = stop_reason(&reason).or_else(|| limit.then(|| reason.clone())) {
                        self.stop(stop, limit);
                        return;
                    }
                    Some(reason)
                } else {
                    None
                };
                let next = {
                    let mut turns = self.turns.lock().unwrap_or_else(PoisonError::into_inner);
                    turns.completed(id).then(|| turns.next()).flatten()
                };
                self.end(failure);
                if let Some(params) = next {
                    start_turn(self.rpc.clone(), self.sink.clone(), Arc::clone(&self.turns), params);
                }
            }
            "item/started" => self.item_started(item),
            "item/completed" => self.item_completed(item),
            "item/agentMessage/delta" | "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                let id = text(Some(params), "itemId");
                let delta = text(Some(params), "delta");
                let Some(live) = self.items.get_mut(&id) else { return };
                live.text.push_str(&delta);
                let (kind, current, started) = (live.kind.clone(), live.text.clone(), live.started);
                let update = if kind == "reasoning" {
                    StreamUpdate::ThinkingDelta { delta }
                } else {
                    StreamUpdate::TextDelta { delta }
                };
                if let Some(message) = self.message(&kind, &current, "stop", started) {
                    self.sink.session(SessionEvent::MessageUpdate {
                        message,
                        assistant_message_event: update,
                        output_tokens: None,
                    });
                }
            }
            "item/commandExecution/outputDelta" => {
                let id = text(Some(params), "itemId");
                let Some(live) = self.items.get_mut(&id) else { return };
                live.text.push_str(&text(Some(params), "delta"));
                let current = live.text.clone();
                self.sink.session(SessionEvent::ToolExecutionUpdate {
                    tool_call_id: id,
                    tool_name: "bash".to_string(),
                    args: Value::Null,
                    partial_result: output(&current),
                });
            }
            "thread/tokenUsage/updated" => self.usage(params.get("tokenUsage")),
            "account/rateLimits/updated" => self.rate_limits(params.get("rateLimits")),
            "error" => self.error(params),
            _ => {}
        }
    }

    fn error(&mut self, params: &Value) {
        let limit = params.get("error").is_some_and(usage_limit);
        let message = params
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("Codex reported an error.")
            .to_string();
        if params.get("willRetry").and_then(Value::as_bool) == Some(true) {
            return;
        }
        if let Some(reason) = stop_reason(&message).or_else(|| limit.then(|| message.clone())) {
            self.stop(reason, limit);
            return;
        }
        self.failure = Some(message);
    }

    fn item_started(&mut self, item: Option<&Value>) {
        let Some(item) = item else { return };
        let id = item.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let at = now_ms();
        match item.get("type").and_then(Value::as_str).unwrap_or_default() {
            "userMessage" => {
                self.begin();
                if let Some(message) = user(&user_text(item), at) {
                    emit_message(&self.sink, message);
                }
            }
            kind @ ("agentMessage" | "reasoning") => {
                self.begin();
                self.items.insert(id, Live { kind: kind.to_string(), text: String::new(), started: at });
                if let Some(message) = self.message(kind, "", "stop", at) {
                    self.sink.session(SessionEvent::MessageStart {
                        message,
                        assistant_message_event: Some(StreamUpdate::Start),
                    });
                }
            }
            kind => {
                let Some((name, arguments)) = tool_call(kind, item) else { return };
                self.begin();
                if kind == "commandExecution" {
                    self.commands.insert(id.clone(), arguments["command"].as_str().unwrap_or_default().to_string());
                }
                self.items.insert(id.clone(), Live { kind: kind.to_string(), text: String::new(), started: at });
                let block = json!({ "type": "toolCall", "id": id, "name": name, "arguments": arguments });
                if let Some(message) =
                    assistant(CODEX_PROVIDER, &self.model, &[block], &Spend::default(), "toolUse", None, at)
                {
                    emit_message(&self.sink, message);
                }
                self.sink.session(SessionEvent::ToolExecutionStart {
                    tool_call_id: id,
                    tool_name: name,
                    args: arguments,
                });
            }
        }
    }

    fn item_completed(&mut self, item: Option<&Value>) {
        let Some(item) = item else { return };
        let id = item.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let live = self.items.remove(&id);
        let at = now_ms();
        match item.get("type").and_then(Value::as_str).unwrap_or_default() {
            "userMessage" => {}
            "agentMessage" => {
                let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                if let Some(message) = self.message("agentMessage", text, "stop", live.map_or(at, |live| live.started))
                {
                    self.sink.session(SessionEvent::MessageEnd { message });
                }
            }
            "reasoning" => {
                let summary = reasoning_text(item);
                let text = if summary.is_empty() {
                    live.as_ref().map(|live| live.text.clone()).unwrap_or_default()
                } else {
                    summary
                };
                if let Some(message) = self.message("reasoning", &text, "stop", live.map_or(at, |live| live.started)) {
                    self.sink.session(SessionEvent::MessageEnd { message });
                }
            }
            kind => {
                let Some((name, _)) = tool_call(kind, item) else { return };
                let status = item.get("status").and_then(Value::as_str).unwrap_or_default();
                let (text, details, failed) = tool_outcome(kind, item, live.map(|live| live.text));
                let error = failed || matches!(status, "failed" | "declined");
                if let Some(message) = tool_result(&id, &name, &text, details, error, at) {
                    emit_message(&self.sink, message);
                }
            }
        }
    }

    fn request(&mut self, id: &Value, method: &str, params: &Value) {
        let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        let (summary, effect, legacy) = match method {
            "item/commandExecution/requestApproval" => {
                let command = params
                    .get("command")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| self.commands.get(&text("itemId")).cloned())
                    .unwrap_or_default();
                (format!("Run: {command}"), "exec", false)
            }
            "item/fileChange/requestApproval" => {
                let reason = text("reason");
                (
                    if reason.is_empty() { "Change files".to_string() } else { format!("Change files: {reason}") },
                    "write",
                    false,
                )
            }
            "execCommandApproval" => {
                let command = params
                    .get("command")
                    .and_then(Value::as_array)
                    .map(|parts| parts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
                    .unwrap_or_default();
                (format!("Run: {command}"), "exec", true)
            }
            "applyPatchApproval" => ("Change files".to_string(), "write", true),
            _ => {
                self.rpc.refuse(id, "Local Studio does not handle this request.");
                return;
            }
        };
        let request = request_key(id);
        self.approvals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(request.clone(), PendingApproval { id: id.clone(), legacy });
        self.sink.send(Event::Approval(Approval {
            request_id: request,
            step: 0,
            command: summary,
            effects: vec![effect.to_string()],
            agent: None,
        }));
    }

    fn usage(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage else { return };
        let read = |value: Option<&Value>| {
            let number = |key: &str| value.and_then(|value| value.get(key)).and_then(Value::as_u64).unwrap_or(0);
            let cached = number("cachedInputTokens");
            Spend {
                model: String::new(),
                input: number("inputTokens").saturating_sub(cached),
                cache_read: cached,
                cache_write: 0,
                output: number("outputTokens"),
                cost: 0.0,
            }
        };
        let total = read(usage.get("total"));
        let last = read(usage.get("last"));
        let delta = match self.total.replace(total.clone()) {
            Some(before) => Spend {
                model: String::new(),
                input: total.input.saturating_sub(before.input),
                cache_read: total.cache_read.saturating_sub(before.cache_read),
                cache_write: 0,
                output: total.output.saturating_sub(before.output),
                cost: 0.0,
            },
            None => last.clone(),
        };
        if delta.input + delta.cache_read + delta.output > 0 {
            self.sink.send(Event::Usage(Spend { model: self.model.clone(), ..delta }));
        }
        let window = usage.get("modelContextWindow").and_then(Value::as_u64).unwrap_or(CONTEXT_WINDOW);
        let tokens = last.input + last.cache_read + last.output;
        if tokens > 0 {
            self.sink.send(Event::Context(ContextUsage { tokens, window }));
        }
    }
}

fn user_text(item: &Value) -> Value {
    let text: Vec<&str> = item
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    Value::String(text.join("\n"))
}

fn reasoning_text(item: &Value) -> String {
    let parts = |key: &str| -> Vec<String> {
        item.get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|part| {
                part.as_str()
                    .map(str::to_string)
                    .or_else(|| part.get("text").and_then(Value::as_str).map(str::to_string))
            })
            .collect()
    };
    let summary = parts("summary");
    if summary.is_empty() {
        parts("content").join("\n")
    } else {
        summary.join("\n")
    }
}

fn tool_call(kind: &str, item: &Value) -> Option<(String, Value)> {
    match kind {
        "commandExecution" => {
            let command = item.get("command").cloned().unwrap_or_default();
            Some((
                "bash".to_string(),
                json!({ "command": command, "cwd": item.get("cwd").cloned().unwrap_or_default() }),
            ))
        }
        "fileChange" => {
            let paths: Vec<Value> = item
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|change| change.get("path").cloned())
                .collect();
            let path = paths.first().cloned().unwrap_or_default();
            Some(("edit".to_string(), json!({ "path": path, "paths": paths })))
        }
        "mcpToolCall" => {
            let server = item.get("server").and_then(Value::as_str).unwrap_or_default();
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or_default();
            Some((format!("{server}.{tool}"), item.get("arguments").cloned().unwrap_or_else(|| json!({}))))
        }
        "dynamicToolCall" => Some((
            item.get("tool").and_then(Value::as_str).unwrap_or("tool").to_string(),
            item.get("arguments").cloned().unwrap_or_else(|| json!({})),
        )),
        "webSearch" => {
            Some(("web_search".to_string(), json!({ "query": item.get("query").cloned().unwrap_or_default() })))
        }
        _ => None,
    }
}

fn tool_outcome(kind: &str, item: &Value, streamed: Option<String>) -> (String, Option<Value>, bool) {
    match kind {
        "commandExecution" => {
            let text = item
                .get("aggregatedOutput")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or(streamed)
                .unwrap_or_default();
            let failed = item.get("exitCode").and_then(Value::as_i64).is_some_and(|code| code != 0);
            (text, None, failed)
        }
        "fileChange" => {
            let changes = item.get("changes").and_then(Value::as_array).cloned().unwrap_or_default();
            let files: Vec<Value> = changes
                .iter()
                .map(|change| {
                    let path = change.get("path").and_then(Value::as_str).unwrap_or_default();
                    json!({ "path": path, "absolute": path, "diff": change.get("diff").and_then(Value::as_str).unwrap_or_default() })
                })
                .collect();
            let names: Vec<&str> =
                changes.iter().filter_map(|change| change.get("path").and_then(Value::as_str)).collect();
            let details = match files.as_slice() {
                [only] => Some(diff_details(
                    only["path"].as_str().unwrap_or_default(),
                    only["diff"].as_str().unwrap_or_default(),
                )),
                [] => None,
                _ => Some(json!({ "files": files })),
            };
            (names.join("\n"), details, false)
        }
        "mcpToolCall" => {
            let error = item.get("error").filter(|error| !error.is_null());
            let text =
                error.map(Value::to_string).or_else(|| item.get("result").map(Value::to_string)).unwrap_or_default();
            (text, None, error.is_some())
        }
        "dynamicToolCall" => {
            let text = item.get("contentItems").map(Value::to_string).unwrap_or_default();
            (text, None, item.get("success").and_then(Value::as_bool) == Some(false))
        }
        _ => (String::new(), None, false),
    }
}

fn history(thread: &Value) -> Vec<AgentMessage> {
    let mut messages = Vec::new();
    for turn in thread.get("turns").and_then(Value::as_array).into_iter().flatten() {
        let at =
            turn.get("startedAt")
                .and_then(Value::as_u64)
                .map_or(0, |at| if at < 10_000_000_000 { at * 1000 } else { at });
        for item in turn.get("items").and_then(Value::as_array).into_iter().flatten() {
            let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
            let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
            match kind {
                "userMessage" => messages.extend(user(&user_text(item), at)),
                "agentMessage" => {
                    let block = json!({ "type": "text", "text": item.get("text").cloned().unwrap_or_default() });
                    messages.extend(assistant(CODEX_PROVIDER, "", &[block], &Spend::default(), "stop", None, at));
                }
                "reasoning" => {
                    let text = reasoning_text(item);
                    if !text.is_empty() {
                        let block = json!({ "type": "thinking", "thinking": text });
                        messages.extend(assistant(CODEX_PROVIDER, "", &[block], &Spend::default(), "stop", None, at));
                    }
                }
                _ => {
                    let Some((name, arguments)) = tool_call(kind, item) else { continue };
                    let block = json!({ "type": "toolCall", "id": id, "name": name, "arguments": arguments });
                    messages.extend(assistant(CODEX_PROVIDER, "", &[block], &Spend::default(), "toolUse", None, at));
                    let status = item.get("status").and_then(Value::as_str).unwrap_or_default();
                    let (text, details, failed) = tool_outcome(kind, item, None);
                    let error = failed || matches!(status, "failed" | "declined");
                    messages.extend(tool_result(id, &name, &text, details, error, at));
                }
            }
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;

    #[test]
    fn a_held_message_never_looks_idle_when_its_turn_completes() -> anyhow::Result<()> {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        let _guard = runtime.enter();
        let (events, _received) = mpsc::unbounded_channel();
        let (lines, _written) = mpsc::unbounded_channel();
        let rpc = Rpc { lines, replies: Replies::default(), next: Arc::default() };
        let turns = SharedTurns::default();
        let mut translator = Translator::new(Sink::new("t", 0, events), rpc, Arc::default(), Arc::clone(&turns));
        let gap = Arc::new(AtomicBool::new(false));
        for round in 0..2000 {
            let id = format!("turn-{round}");
            *turns.lock().unwrap_or_else(PoisonError::into_inner) =
                Turns { running: Some(id.clone()), held: [json!({ "input": [] })].into(), ..Turns::default() };
            translator.begin();
            let (shared, seen) = (Arc::clone(&turns), Arc::clone(&gap));
            let watcher = std::thread::spawn(move || loop {
                let turns = shared.lock().unwrap_or_else(PoisonError::into_inner);
                if turns.held.is_empty() {
                    break;
                }
                if !turns.busy() {
                    seen.store(true, Ordering::Relaxed);
                }
            });
            translator.notification("turn/completed", &json!({ "turn": { "id": id, "status": "completed" } }));
            watcher.join().map_err(|_| anyhow::anyhow!("the watcher panicked"))?;
        }
        assert!(!gap.load(Ordering::Relaxed), "a send could start a turn while a held message was waiting to start");
        Ok(())
    }
}
