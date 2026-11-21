use std::collections::HashMap;

use eukhe_types::daemon::DaemonCommand;
use serde_json::{json, Map, Value};
use tokio::sync::oneshot;

use super::{now_ms, truncate, Actor, BODY_LIMIT};
use crate::model::{NoticeLevel, NotificationKind, Question, QuestionOption, Update};
use crate::plugins::ask::{self, Ask, Outcome};
use crate::plugins::{self, Grants};

pub(super) struct Pending {
    pub(super) request: String,
    pub(super) plugin: String,
    pub(super) text: String,
    pub(super) choices: Vec<String>,
    pub(super) at: i64,
    reply: oneshot::Sender<Outcome>,
}

impl Pending {
    fn update(&self, thread: &str) -> Update {
        Update::Questions {
            thread: thread.to_string(),
            request: self.request.clone(),
            source: format!("plugin:{}", self.plugin),
            questions: vec![Question {
                header: self.plugin.clone(),
                question: self.text.clone(),
                options: self
                    .choices
                    .iter()
                    .map(|label| QuestionOption { label: label.clone(), description: String::new() })
                    .collect(),
                multi_select: false,
            }],
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
        let pending = Pending { request, plugin, text, choices, at: now_ms(), reply };
        let update = pending.update(&thread);
        let body = truncate(&format!("{} asks: {}", pending.plugin, pending.text), BODY_LIMIT);
        self.questions.entry(thread.clone()).or_default().push(pending);
        self.mailbox.push(update);
        let title = self.title(&thread);
        self.mailbox.push(Update::Notification { thread, kind: NotificationKind::NeedsAnswer, title, body });
        self.dirty = true;
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
        let note = note(&pending.plugin, &pending.text, &format!("You answered: {answer}"));
        let _ = pending.reply.send(Outcome::Answered(answer));
        self.ask_notes.entry(id.to_string()).or_default().push(note);
        if !self.running(id) {
            self.flush_ask_notes(id).await;
        }
    }

    pub(super) fn question_gone(&mut self, id: &str, request: &str) {
        self.take_question(id, request);
    }

    pub(super) fn cancel_questions(&mut self, id: &str, noted: bool) {
        let Some(list) = self.questions.remove(id) else { return };
        for pending in list {
            self.mailbox.push(Update::QuestionsSettled { thread: id.to_string(), request: pending.request.clone() });
            if noted {
                let note = note(&pending.plugin, &pending.text, "You stopped the question (not confirmed).");
                self.ask_notes.entry(id.to_string()).or_default().push(note);
            }
            let _ = pending.reply.send(Outcome::Cancelled);
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

    pub(super) async fn sync_ask_servers(&mut self, id: &str, active: &str) {
        let Some(socket) = self.ask_socket.clone() else { return };
        if self.threads.get(id).and_then(|thread| thread.live.as_ref()).is_some_and(|live| live.state.busy) {
            return;
        }
        let (asking, grants) = self.granted_askers();
        self.ask_tokens.retain(|plugin, _| asking.iter().any(|known| known.id == plugin));
        let foreign = crate::daemon::agent_dir()
            .and_then(|agent| Some(plugins::foreign_servers(self.plugins_dir.as_deref()?, &agent)))
            .unwrap_or_default();
        let (servers, errors) = ask::acp_servers(&asking, &grants, &socket, id, &self.ask_tokens, &foreign);
        if !errors.is_empty() {
            self.mailbox.notice(NoticeLevel::Warning, format!("Plugins: {}", errors.join("; ")));
        }
        let servers = Value::Array(servers);
        let before = self.ask_sent.get(active);
        if before == Some(&servers) || (before.is_none() && servers.as_array().is_some_and(Vec::is_empty)) {
            return;
        }
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
                self.mailbox.notice(NoticeLevel::Warning, format!("Plugins that ask you questions are off: {error}"));
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
