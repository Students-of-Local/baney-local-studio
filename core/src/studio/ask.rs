use std::collections::HashMap;

use eukhe_types::daemon::DaemonCommand;
use serde_json::{json, Map, Value};
use tokio::sync::oneshot;

use super::{now_ms, truncate, Actor, BODY_LIMIT};
use crate::harness::Asked;
use crate::model::{NoticeLevel, NotificationKind, Question, QuestionOption, Update};
use crate::plugins::ask::{self, Ask, Outcome};
use crate::plugins::{self, Grants};

pub(super) struct Pending {
    pub(super) request: String,
    pub(super) plugin: String,
    pub(super) text: String,
    options: Vec<QuestionOption>,
    multi_select: bool,
    pub(super) at: i64,
    reply: Reply,
}

enum Reply {
    Plugin(oneshot::Sender<Outcome>),
    Harness { base: String, index: usize },
}

impl Pending {
    fn update(&self, thread: &str) -> Update {
        let source = match &self.reply {
            Reply::Plugin(_) => format!("plugin:{}", self.plugin),
            Reply::Harness { .. } => format!("harness:{}", self.plugin),
        };
        Update::Questions {
            thread: thread.to_string(),
            request: self.request.clone(),
            source,
            questions: vec![Question {
                header: self.plugin.clone(),
                question: self.text.clone(),
                options: self.options.clone(),
                multi_select: self.multi_select,
            }],
        }
    }

    fn harness_base(&self) -> Option<&str> {
        match &self.reply {
            Reply::Harness { base, .. } => Some(base),
            Reply::Plugin(_) => None,
        }
    }
}

impl Actor {
    fn granted_askers(&mut self) -> (Vec<plugins::Plugin>, Grants) {
        let Some(dir) = self.plugins_dir.clone() else { return (Vec::new(), Grants::default()) };
        let scanned = self.plugins();
        let grants = Grants::load(&dir).unwrap_or_default();
        let asking = scanned.into_iter().filter(|plugin| ask::asks(plugin, &grants)).collect();
        (asking, grants)
    }

    pub(super) fn plugin_ask(&mut self, ask: Ask) {
        let Ask { plugin, thread, request, text, choices, reply } = ask;
        let refuse = |reply: oneshot::Sender<Outcome>, why: &str| {
            let _ = reply.send(Outcome::Refused(why.to_string()));
        };
        if !self.known(&thread) || self.harness(&thread).is_some() {
            refuse(reply, "the thread is gone");
            return;
        }
        if self.store.flags(&thread).archived {
            refuse(reply, "the thread is archived");
            return;
        }
        let (asking, _) = self.granted_askers();
        if !asking.iter().any(|known| known.id == plugin) {
            self.ask_tokens.retain(|holder, _| holder != plugin);
            refuse(reply, "the plugin no longer holds the ask-user grant");
            return;
        }
        let options = choices.into_iter().map(|label| QuestionOption { label, description: String::new() }).collect();
        let pending =
            Pending { request, plugin, text, options, multi_select: false, at: now_ms(), reply: Reply::Plugin(reply) };
        let update = pending.update(&thread);
        let body = truncate(&format!("{} asks: {}", pending.plugin, pending.text), BODY_LIMIT);
        self.questions.entry(thread.clone()).or_default().push(pending);
        self.mailbox.push(update);
        let title = self.title(&thread);
        self.mailbox.push(Update::Notification { thread, kind: NotificationKind::NeedsAnswer, title, body });
        self.dirty = true;
    }

    pub(super) fn harness_questions(&mut self, id: &str, source: &str, asked: Asked) {
        let Asked { request_id, questions } = asked;
        let count = questions.len();
        let mut body = None;
        for (index, question) in questions.into_iter().enumerate() {
            let request = if count == 1 { request_id.clone() } else { format!("{request_id}#{}", index + 1) };
            let pending = Pending {
                request,
                plugin: source.to_string(),
                text: question.question,
                options: question.options,
                multi_select: question.multi_select,
                at: now_ms(),
                reply: Reply::Harness { base: request_id.clone(), index },
            };
            body.get_or_insert_with(|| truncate(&format!("{source} asks: {}", pending.text), BODY_LIMIT));
            self.mailbox.push(pending.update(id));
            self.questions.entry(id.to_string()).or_default().push(pending);
        }
        self.harness_answers.insert((id.to_string(), request_id), vec![None; count]);
        let title = self.title(id);
        let body = body.unwrap_or_default();
        self.mailbox.push(Update::Notification {
            thread: id.to_string(),
            kind: NotificationKind::NeedsAnswer,
            title,
            body,
        });
        self.dirty = true;
    }

    pub(super) fn harness_questions_gone(&mut self, id: &str, base: &str) {
        self.harness_answers.remove(&(id.to_string(), base.to_string()));
        let gone: Vec<String> = self
            .questions
            .get(id)
            .into_iter()
            .flatten()
            .filter(|pending| pending.harness_base() == Some(base))
            .map(|pending| pending.request.clone())
            .collect();
        for request in gone {
            self.take_question(id, &request);
        }
    }

    pub(super) fn emit_questions(&self, id: &str) {
        for pending in self.questions.get(id).into_iter().flatten() {
            self.mailbox.push(pending.update(id));
        }
    }

    fn take_question(&mut self, id: &str, request: &str) -> Option<Pending> {
        let list = self.questions.get_mut(id)?;
        let index = list.iter().position(|pending| pending.request == request)?;
        let pending = list.remove(index);
        if list.is_empty() {
            self.questions.remove(id);
        }
        self.mailbox.push(Update::QuestionsSettled { thread: id.to_string(), request: request.to_string() });
        self.dirty = true;
        Some(pending)
    }

    pub(super) async fn answer_questions(&mut self, id: &str, request: &str, answers: HashMap<String, String>) {
        let Some(pending) = self.questions.get(id).and_then(|list| list.iter().find(|p| p.request == request)) else {
            self.mailbox.notice(NoticeLevel::Info, "That question is no longer pending.");
            return;
        };
        let answer = answers
            .get(&pending.text)
            .or_else(|| (answers.len() == 1).then(|| answers.values().next()).flatten())
            .map(|answer| answer.trim().to_string())
            .filter(|answer| !answer.is_empty());
        let Some(answer) = answer else {
            self.mailbox.notice(NoticeLevel::Warning, "Choose an answer or type one first.");
            return;
        };
        let Some(pending) = self.take_question(id, request) else { return };
        match pending.reply {
            Reply::Plugin(reply) => {
                let note = note(&pending.plugin, &pending.text, &format!("You answered: {answer}"));
                let _ = reply.send(Outcome::Answered(answer));
                self.ask_notes.entry(id.to_string()).or_default().push(note);
                if !self.running(id) {
                    self.flush_ask_notes(id).await;
                }
            }
            Reply::Harness { base, index } => {
                let key = (id.to_string(), base);
                let Some(slots) = self.harness_answers.get_mut(&key) else { return };
                if let Some(slot) = slots.get_mut(index) {
                    *slot = Some(answer);
                }
                if slots.iter().all(Option::is_some) {
                    let answers: Vec<String> =
                        self.harness_answers.remove(&key).into_iter().flatten().flatten().collect();
                    self.cli_answer_questions(id, &key.1, &answers);
                }
            }
        }
    }

    pub(super) fn question_gone(&mut self, id: &str, request: &str) {
        self.take_question(id, request);
    }

    pub(super) fn cancel_questions(&mut self, id: &str, noted: bool) {
        let Some(list) = self.questions.remove(id) else { return };
        for pending in list {
            self.mailbox.push(Update::QuestionsSettled { thread: id.to_string(), request: pending.request.clone() });
            match pending.reply {
                Reply::Plugin(reply) => {
                    if noted {
                        let note = note(&pending.plugin, &pending.text, "You stopped the question (not confirmed).");
                        self.ask_notes.entry(id.to_string()).or_default().push(note);
                    }
                    let _ = reply.send(Outcome::Cancelled);
                }
                Reply::Harness { base, .. } => {
                    if self.harness_answers.remove(&(id.to_string(), base.clone())).is_some() {
                        self.cli_dismiss(id, &base);
                    }
                }
            }
        }
        self.dirty = true;
    }

    pub(super) fn question_since(&self, id: &str) -> Option<i64> {
        self.questions.get(id).and_then(|list| list.iter().map(|pending| pending.at).min())
    }

    pub(super) fn has_questions(&self, id: &str) -> bool {
        self.questions.get(id).is_some_and(|list| !list.is_empty())
    }

    pub(super) async fn flush_ask_notes(&mut self, id: &str) {
        let Some(active) = self.live_active(id).or_else(|| self.daemon_active(id)) else { return };
        let Some(notes) = self.ask_notes.remove(id) else { return };
        for note in notes {
            let command = DaemonCommand::AppendCustomMessage {
                id: None,
                active_session_id: active.clone(),
                message: json!({ "customType": ask::NOTE_TYPE, "content": note, "display": true }),
                rest: Map::default(),
            };
            if let Err(error) = self.call(command).await {
                self.mailbox.notice(NoticeLevel::Warning, format!("Could not record the plugin's question: {error}"));
            }
        }
    }

    /// Registers the thread's asking plugins, and every plugin server when the thread has a caller, with its
    /// session. Until this core has sent the session a list, the session may still hold one from before a
    /// restart, with a plugin whose grant is gone since, so an unknown list is always replaced, even by an empty one. Eukhe refuses a non-empty list mid-run but
    /// takes an empty one, so a busy session that may hold a server it should not is cleared and gets its
    /// list again on the next idle prompt.
    pub(super) async fn sync_plugin_servers(&mut self, id: &str, active: &str) {
        let caller = self.store.flags(id).caller;
        let scanned = if self.plugins_dir.is_some() { self.plugins() } else { Vec::new() };
        let grants = self.plugins_dir.as_deref().map(|dir| Grants::load(dir).unwrap_or_default()).unwrap_or_default();
        self.ask_tokens.retain(|plugin, _| scanned.iter().any(|known| known.id == plugin && ask::asks(known, &grants)));
        let mut servers = match (self.ask_socket.clone(), &caller) {
            (None, None) => Vec::new(),
            (socket, caller) => {
                let foreign = crate::daemon::agent_dir()
                    .and_then(|agent| Some(plugins::foreign_servers(self.plugins_dir.as_deref()?, &agent)))
                    .unwrap_or_default();
                let (servers, errors) = ask::acp_servers(
                    &scanned,
                    &grants,
                    socket.as_deref(),
                    id,
                    &self.ask_tokens,
                    &foreign,
                    caller.as_ref(),
                );
                if !errors.is_empty() {
                    self.mailbox.notice(NoticeLevel::Warning, format!("Plugins: {}", errors.join("; ")));
                }
                servers
            }
        };
        let before = self.ask_sent.get(active).and_then(Value::as_array);
        if before == Some(&servers) {
            return;
        }
        let busy = self.threads.get(id).and_then(|thread| thread.live.as_ref()).is_some_and(|live| live.state.busy);
        if busy {
            let stale = before.is_none_or(|sent| sent.iter().any(|server| !servers.contains(server)));
            if !stale {
                return;
            }
            servers.clear();
        }
        let servers = Value::Array(servers);
        let command = DaemonCommand::ReplaceAcpMcpServers {
            id: None,
            active_session_id: active.to_string(),
            owner_id: ask::NOTE_TYPE.to_string(),
            servers: servers.clone(),
            rest: Map::default(),
        };
        match self.call(command).await {
            Ok(_) => {
                self.ask_sent.insert(active.to_string(), servers);
            }
            Err(error) => {
                self.mailbox
                    .notice(NoticeLevel::Warning, format!("Plugin tool servers for this thread are off: {error}"));
            }
        }
    }

    pub(super) fn forget_ask_thread(&mut self, id: &str) {
        self.cancel_questions(id, false);
        self.ask_notes.remove(id);
        self.ask_tokens.retain(|_, thread| thread != id);
    }

    pub(super) async fn ask_socket_replaced(&mut self, error: Option<String>) {
        let path = self.ask_socket.as_ref().map(|path| path.display().to_string()).unwrap_or_default();
        let warning = match error {
            None => format!(
                "Another process replaced the plugin question socket {path}, so Local Studio made it again. A plugin answer from the replaced socket did not come from you."
            ),
            Some(error) => format!(
                "Another process replaced the plugin question socket {path}, and Local Studio could not make it again ({error}). Plugins cannot ask you questions until it can."
            ),
        };
        self.mailbox.notice(NoticeLevel::Warning, warning.clone());
        let asking: Vec<String> = self
            .threads
            .keys()
            .filter(|id| {
                self.live_active(id)
                    .and_then(|active| self.ask_sent.get(&active))
                    .and_then(Value::as_array)
                    .is_some_and(|servers| !servers.is_empty())
            })
            .cloned()
            .collect();
        for id in asking {
            self.ask_notes.entry(id.clone()).or_default().push(warning.clone());
            if !self.running(&id) {
                self.flush_ask_notes(&id).await;
            }
        }
    }
}

fn note(plugin: &str, text: &str, outcome: &str) -> String {
    let stop = if text.ends_with(['.', '?', '!']) { "" } else { "." };
    format!("{plugin} asked: {text}{stop} {outcome}")
}
