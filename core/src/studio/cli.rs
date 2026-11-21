use std::collections::HashMap;
use std::time::Duration;

use eukhe_types::daemon::SessionEvent;
use eukhe_types::session::AgentMessage;
use tokio::task::AbortHandle;
use tokio::time::Instant;

use super::{now_ms, Actor, Command, Live, Thread, BODY_LIMIT, STOP_GRACE};
use crate::harness::{self, Driver, Envelope, Event, Failure, Image, Launch, OpenReport, Sink};
use crate::images;
use crate::model::{
    AccessMode, Approval, ApprovalAnswer, Harness, ModelOption, ModelRef, NoticeLevel, NotificationKind, Update,
};
use crate::session::LiveState;
use crate::transcript::Transcript;

const IDLE_KILL: Duration = Duration::from_secs(600);
/// How long a CLI may sit idle with sends unanswered before Local Studio counts them as dropped.
const SETTLE: Duration = Duration::from_secs(3);

pub(super) struct Cli {
    driver: Driver,
    generation: u64,
    used: Instant,
    restart: bool,
    unanswered: usize,
    epoch: u64,
}

pub(super) struct Opening {
    generation: u64,
    launch: Launch,
    task: AbortHandle,
    waiting: Vec<Waiting>,
    early: Vec<(u64, Event)>,
    draft: Option<String>,
}

enum Waiting {
    Send { request: Option<String>, text: String, images: Vec<Image>, steer: bool },
    Compact,
}

impl Opening {
    fn owns(&self, generation: u64) -> bool {
        generation == self.generation || generation == self.generation + 1
    }

    fn cancel(&self) {
        self.task.abort();
    }
}

pub(super) fn name(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "Claude Code",
        Harness::Codex => "Codex",
        Harness::Eukhe => "Eukhe",
    }
}

fn session_gone(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    ["no rollout", "not found", "no such thread", "unknown thread"].iter().any(|words| lower.contains(words))
}

pub(super) fn unsupported(harness: Harness, what: &str) -> String {
    format!("{} threads cannot {what} in Local Studio.", name(harness))
}

impl Actor {
    pub(super) fn harness(&self, id: &str) -> Option<Harness> {
        self.store.threads.get(id).map(|flags| flags.harness).filter(|harness| *harness != Harness::Eukhe)
    }

    pub(super) fn all_models(&self) -> Vec<ModelOption> {
        let mut models = self.models.clone();
        models.extend(harness::models(Harness::ClaudeCode));
        models.extend(self.codex_models.iter().cloned());
        models
    }

    fn model_ref(&self, harness: Harness, model: &str) -> ModelRef {
        let provider = harness::provider_of(harness);
        let option = self.all_models().into_iter().find(|option| option.provider == provider && option.id == model);
        ModelRef {
            provider: provider.to_string(),
            id: model.to_string(),
            name: option.map(|option| option.name),
            reasoning: true,
        }
    }

    fn thinking_levels(&self, harness: Harness, model: Option<&str>) -> Vec<String> {
        let provider = harness::provider_of(harness);
        let models = self.all_models();
        models
            .iter()
            .find(|option| option.provider == provider && Some(option.id.as_str()) == model)
            .or_else(|| models.iter().find(|option| option.provider == provider))
            .map(|option| option.thinking_levels.clone())
            .unwrap_or_default()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn cli_create(
        &mut self,
        draft: &str,
        harness: Harness,
        cwd: String,
        project: Option<String>,
        access: AccessMode,
        model: Option<String>,
        thinking: Option<String>,
        fan_out: Option<String>,
        prompt: &str,
    ) -> Result<String, String> {
        let id = harness::new_session_id();
        let now = now_ms();
        self.store.update(&id, |flags| {
            flags.harness = harness;
            flags.native_session = (harness == Harness::ClaudeCode).then(|| id.clone());
            flags.cwd = Some(cwd.clone());
            flags.model = model;
            flags.thinking_level = thinking;
            flags.activity_at = Some(now);
            flags.project = project;
            flags.access = Some(access);
            flags.fan_out = fan_out;
        });
        self.save();
        self.threads
            .insert(id.clone(), Thread { cwd: Some(cwd), prompt: Some(prompt.to_string()), ..Thread::default() });
        self.cli_live(&id, &[]);
        self.cli_begin_open(&id, Vec::new(), Some(draft.to_string()))?;
        self.dirty = true;
        Ok(id)
    }

    fn cli_cwd(&self, id: &str) -> String {
        self.store
            .flags(id)
            .cwd
            .or_else(|| self.threads.get(id).and_then(|thread| thread.cwd.clone()))
            .unwrap_or_else(|| self.home.clone())
    }

    fn cli_live(&mut self, id: &str, history: &[AgentMessage]) {
        let Some(harness) = self.harness(id) else { return };
        let flags = self.store.flags(id);
        let cwd = self.cli_cwd(id);
        let model = flags.model.as_deref().map(|model| self.model_ref(harness, model));
        let thinking_levels = self.thinking_levels(harness, flags.model.as_deref());
        let state = LiveState {
            session_id: id.to_string(),
            cwd: cwd.clone(),
            model,
            thinking_level: flags.thinking_level.clone().unwrap_or_default(),
            thinking_levels,
            ..LiveState::default()
        };
        let mut transcript = Transcript::default();
        transcript.reset(history, false);
        transcript.take_settled();
        let thread = self.threads.entry(id.to_string()).or_default();
        thread.cwd = Some(cwd);
        thread.live = Some(Live {
            active: format!("{}:{id}", harness::provider_of(harness)),
            seen: 0,
            state,
            transcript,
            approvals: Vec::new(),
            sending: Vec::new(),
            shown: HashMap::new(),
        });
        let last_user_at = thread.live.as_ref().and_then(|live| live.transcript.last_user_at());
        self.note_user_at(id, last_user_at);
    }

    pub(super) fn cli_view(&mut self, id: &str) -> String {
        let has_live = self.threads.entry(id.to_string()).or_default().live.is_some();
        let busy = self.drivers.contains_key(id) || self.opening.contains_key(id) || self.loading.contains_key(id);
        if !has_live && !busy {
            if let Some(harness) = self.harness(id) {
                let cwd = self.cli_cwd(id);
                let native = self.store.flags(id).native_session.unwrap_or_default();
                self.generation += 1;
                let token = self.generation;
                self.loading.insert(id.to_string(), token);
                let (sender, thread) = (self.cli.clone(), id.to_string());
                tokio::spawn(async move {
                    let event = Event::Loaded(harness::history(harness, cwd, native).await);
                    sender.send(Envelope { thread, generation: token, event }).ok();
                });
            }
        }
        self.live_active(id).unwrap_or_default()
    }

    fn cli_loaded(&mut self, id: &str, token: u64, result: Result<Vec<AgentMessage>, String>) {
        if self.loading.get(id) != Some(&token) {
            return;
        }
        self.loading.remove(id);
        if self.threads.get(id).is_some_and(|thread| thread.live.is_some()) {
            return;
        }
        let history = result.unwrap_or_else(|error| {
            let title = self.title(id);
            self.mailbox.notice(NoticeLevel::Warning, format!("Could not read the history of “{title}”: {error}"));
            Vec::new()
        });
        self.cli_live(id, &history);
        if self.watched(id) {
            self.emit_focus(id);
        }
        self.dirty = true;
    }

    fn cli_begin_open(&mut self, id: &str, waiting: Vec<Waiting>, draft: Option<String>) -> Result<(), String> {
        let harness = self.harness(id).ok_or_else(|| format!("Unknown thread {id}."))?;
        if let Some(mut stale) = self.drivers.remove(id) {
            stale.driver.kill();
        }
        let flags = self.store.flags(id);
        let cwd = self.cli_cwd(id);
        let native = flags.native_session.clone().unwrap_or_default();
        let resume = match harness {
            Harness::ClaudeCode => harness::claude_session_exists(&cwd, &native),
            Harness::Codex => !native.is_empty(),
            Harness::Eukhe => false,
        };
        let launch = Launch {
            harness,
            native,
            resume,
            cwd,
            model: flags.model.clone(),
            effort: flags.thinking_level.clone(),
            access: flags.access.unwrap_or(AccessMode::FullAccess),
        };
        self.generation += 2;
        let generation = self.generation - 1;
        let (sender, thread, first) = (self.cli.clone(), id.to_string(), launch.clone());
        let task = tokio::spawn(async move {
            let opened = Driver::open(first.clone(), Sink::new(&thread, generation, sender.clone())).await;
            let (generation, result, lost) = match opened {
                Err(error) if first.harness == Harness::Codex && first.resume && session_gone(&error) => {
                    let fresh = Launch { resume: false, native: String::new(), ..first };
                    let sink = Sink::new(&thread, generation + 1, sender.clone());
                    (generation + 1, Driver::open(fresh, sink).await, Some(error))
                }
                other => (generation, other, None),
            };
            let event = Event::Opened(Box::new(OpenReport { result, lost }));
            sender.send(Envelope { thread, generation, event }).ok();
        });
        self.opening.insert(
            id.to_string(),
            Opening { generation, launch, task: task.abort_handle(), waiting, early: Vec::new(), draft },
        );
        Ok(())
    }

    async fn cli_opened(&mut self, id: &str, generation: u64, report: OpenReport) {
        let opening = match self.opening.remove(id) {
            Some(opening) if opening.owns(generation) => opening,
            other => {
                if let Some(newer) = other {
                    self.opening.insert(id.to_string(), newer);
                }
                if let Ok(mut opened) = report.result {
                    opened.driver.kill();
                }
                return;
            }
        };
        let opened = match report.result {
            Ok(opened) => opened,
            Err(error) => return self.cli_open_failed(id, opening, error).await,
        };
        if let Some(lost) = report.lost {
            self.mailbox.notice(
                NoticeLevel::Warning,
                format!("Codex could not resume this thread ({lost}); it continues in a new Codex session."),
            );
        }
        let Opening { launch, waiting, early, draft, .. } = opening;
        let flags = self.store.flags(id);
        if flags.native_session.as_deref() != Some(opened.native.as_str()) {
            let native = opened.native.clone();
            self.store.update(id, |flags| flags.native_session = Some(native));
            self.save();
        }
        if self.threads.get(id).is_none_or(|thread| thread.live.is_none()) || launch.resume {
            self.cli_live(id, &opened.history);
        }
        let mut cli =
            Cli { driver: opened.driver, generation, used: Instant::now(), restart: false, unanswered: 0, epoch: 0 };
        let access = flags.access.unwrap_or(AccessMode::FullAccess);
        if access != launch.access {
            if let Err(error) = cli.driver.set_access(access) {
                self.mailbox.notice(NoticeLevel::Error, error);
            }
        }
        if let Some(model) = flags.model.as_deref().filter(|model| launch.model.as_deref() != Some(*model)) {
            if let Err(error) = cli.driver.set_model(model) {
                self.mailbox.notice(NoticeLevel::Error, error);
            }
        }
        if let Some(level) = flags.thinking_level.as_deref().filter(|level| launch.effort.as_deref() != Some(*level)) {
            cli.restart = cli.driver.set_effort(level).is_err();
        }
        self.drivers.insert(id.to_string(), cli);
        self.loading.remove(id);
        if let Some(draft) = draft {
            self.mailbox.push(Update::ThreadStarted { draft, thread: id.to_string() });
        }
        if self.watched(id) {
            self.emit_focus(id);
        }
        self.dirty = true;
        for (from, event) in early {
            if from == generation {
                self.cli.send(Envelope { thread: id.to_string(), generation, event }).ok();
            }
        }
        for waiting in waiting {
            match waiting {
                Waiting::Send { request, text, images, steer } => {
                    if let Err(error) = self.cli_deliver(id, &text, &images, steer, request.clone()) {
                        self.cli_send_failed(id, request, error);
                    }
                }
                Waiting::Compact => self.cli_compact_now(id),
            }
        }
        self.cli_restart_due(id);
    }

    async fn cli_open_failed(&mut self, id: &str, opening: Opening, error: String) {
        if let Some(draft) = opening.draft {
            self.threads.remove(id);
            self.store.threads.remove(id);
            self.save();
            if self.focused.as_deref() == Some(id) {
                self.focused = None;
            }
            self.mailbox.push(Update::RequestFailed { request: draft, message: error });
            self.dirty = true;
            return;
        }
        let mut reported = false;
        for waiting in opening.waiting {
            if let Waiting::Send { request: Some(request), .. } = waiting {
                self.mailbox.push(Update::RequestFailed { request, message: error.clone() });
                reported = true;
            }
        }
        if !reported {
            self.mailbox.notice(NoticeLevel::Error, error);
        }
        self.cli_halt(id).await;
    }

    async fn cli_cancel(&mut self, id: &str, lead: Option<&str>) {
        let Some(opening) = self.opening.remove(id) else { return };
        opening.cancel();
        if let Some(draft) = opening.draft {
            self.mailbox.push(Update::ThreadStarted { draft, thread: id.to_string() });
        }
        if let Some(lead) = lead {
            for waiting in &opening.waiting {
                let Waiting::Send { text, .. } = waiting else { continue };
                let Some(message) = harness::notice(&format!("{lead}: {text}"), u64::try_from(now_ms()).unwrap_or(0))
                else {
                    continue;
                };
                self.apply_event(
                    id,
                    &SessionEvent::MessageStart { message: message.clone(), assistant_message_event: None },
                    0,
                )
                .await;
                self.apply_event(id, &SessionEvent::MessageEnd { message }, 0).await;
            }
        }
        self.cli_halt(id).await;
    }

    pub(super) fn cli_send(
        &mut self,
        id: &str,
        text: &str,
        images: Vec<Image>,
        steer: bool,
        request: Option<String>,
    ) -> Result<(), String> {
        let waiting = Waiting::Send { request: request.clone(), text: text.to_string(), images, steer };
        if let Some(opening) = self.opening.get_mut(id) {
            opening.waiting.push(waiting);
            return Ok(());
        }
        if !self.drivers.contains_key(id) {
            return self.cli_begin_open(id, vec![waiting], None);
        }
        let Waiting::Send { images, .. } = waiting else { return Ok(()) };
        self.cli_deliver(id, text, &images, steer, request)
    }

    fn cli_deliver(
        &mut self,
        id: &str,
        text: &str,
        images: &[Image],
        steer: bool,
        request: Option<String>,
    ) -> Result<(), String> {
        let cli = self.drivers.get_mut(id).ok_or_else(|| "The thread is not open.".to_string())?;
        cli.used = Instant::now();
        cli.unanswered += 1;
        cli.epoch += 1;
        let sent = cli.driver.send(text, images, steer, request);
        if sent.is_err() {
            self.answered(id);
        }
        sent
    }

    fn cli_send_failed(&mut self, id: &str, request: Option<String>, error: String) {
        let busy = self.threads.get(id).and_then(|thread| thread.live.as_ref()).is_some_and(|live| live.state.busy);
        if let Some(thread) = self.threads.get_mut(id).filter(|_| !busy) {
            thread.pending_run = false;
            thread.pending_at = None;
            thread.run_started_at = None;
        }
        self.emit_info(id);
        self.dirty = true;
        match request {
            Some(request) => self.mailbox.push(Update::RequestFailed { request, message: error }),
            None => self.mailbox.notice(NoticeLevel::Error, error),
        }
    }

    fn answered(&mut self, id: &str) {
        if let Some(cli) = self.drivers.get_mut(id) {
            cli.unanswered = cli.unanswered.saturating_sub(1);
        }
    }

    pub(super) fn cli_images(prompt: &crate::model::PromptDraft) -> Vec<Image> {
        prompt
            .images
            .iter()
            .map(|image| Image { mime_type: image.mime_type.clone(), data: images::encode_base64(&image.data) })
            .collect()
    }

    pub(super) async fn cli_stop(&mut self, id: &str) {
        if !self.running(id) {
            self.mailbox.notice(NoticeLevel::Info, "That thread is not running.");
            return;
        }
        if self.opening.contains_key(id) {
            let lead =
                format!("Stop dropped this message before {} started", name(self.harness(id).unwrap_or_default()));
            self.cli_cancel(id, Some(&lead)).await;
            return;
        }
        let Some(cli) = self.drivers.get_mut(id) else {
            self.forget_run(id);
            self.mailbox.notice(NoticeLevel::Info, "The thread had no running process; it no longer shows as working.");
            return;
        };
        let result = cli.driver.interrupt();
        self.threads.entry(id.to_string()).or_default().stopping = Some(Instant::now());
        match result {
            Ok(()) => self.later(STOP_GRACE, Command::StopCheck { thread: id.to_string() }),
            Err(error) => {
                self.mailbox.notice(NoticeLevel::Warning, format!("The stop request did not go through: {error}"));
                self.itself.send(Command::StopCheck { thread: id.to_string() });
            }
        }
    }

    pub(super) async fn cli_stop_check(&mut self, id: &str) {
        let harness = self.harness(id).unwrap_or_default();
        if let Some(mut cli) = self.drivers.remove(id) {
            cli.driver.kill();
        }
        let title = self.title(id);
        self.forget_run(id);
        self.mailbox.notice(
            NoticeLevel::Warning,
            format!(
                "“{title}” did not stop within {} s, so Local Studio stopped its {} process.",
                STOP_GRACE.as_secs(),
                name(harness)
            ),
        );
        self.reopen_focused(id).await;
    }

    pub(super) fn cli_answer(&mut self, id: &str, request: &str, answer: ApprovalAnswer) {
        let result = match self.drivers.get_mut(id) {
            Some(cli) => cli.driver.answer(request, answer),
            None => Err("That approval is no longer pending.".to_string()),
        };
        if let Err(error) = result {
            self.mailbox.notice(NoticeLevel::Warning, error);
        }
        self.settle_approval(id, request);
    }

    pub(super) fn cli_rename(&mut self, id: &str, title: &str) {
        self.store.update(id, |flags| flags.title = Some(title.to_string()));
        self.save();
        self.threads.entry(id.to_string()).or_default().title = Some(title.to_string());
        if let Some(cli) = self.drivers.get_mut(id) {
            cli.driver.rename(title);
        }
        self.emit_info(id);
        self.dirty = true;
    }

    pub(super) fn cli_delete(&mut self, id: &str) {
        if let Some(mut cli) = self.drivers.remove(id) {
            cli.driver.kill();
        }
        if let Some(opening) = self.opening.remove(id) {
            opening.cancel();
            if let Some(request) = opening.draft {
                let message = "The thread was deleted before it started.".to_string();
                self.mailbox.push(Update::RequestFailed { request, message });
            }
        }
        self.loading.remove(id);
        let cwd = self.threads.get(id).and_then(|thread| thread.cwd.clone()).or_else(|| self.store.flags(id).cwd);
        let worktree = cwd.and_then(|cwd| crate::threads::git_head(&cwd).worktree);
        self.workspace.thread_deleted(worktree);
        self.threads.remove(id);
        self.store.threads.remove(id);
        self.save();
        if self.focused.as_deref() == Some(id) {
            self.focused = None;
        }
        self.dirty = true;
    }

    pub(super) fn cli_compact(&mut self, id: &str) {
        if let Some(opening) = self.opening.get_mut(id) {
            opening.waiting.push(Waiting::Compact);
            return;
        }
        if self.drivers.contains_key(id) {
            self.cli_compact_now(id);
        } else if let Err(error) = self.cli_begin_open(id, vec![Waiting::Compact], None) {
            self.mailbox.notice(NoticeLevel::Error, error);
        }
    }

    fn cli_compact_now(&mut self, id: &str) {
        let Some(cli) = self.drivers.get_mut(id) else { return };
        cli.unanswered += 1;
        cli.epoch += 1;
        if let Err(error) = cli.driver.compact() {
            self.answered(id);
            self.mailbox.notice(NoticeLevel::Error, error);
        }
    }

    pub(super) fn cli_access(&mut self, id: &str, mode: AccessMode) {
        let Some(cli) = self.drivers.get_mut(id) else { return };
        if let Err(error) = cli.driver.set_access(mode) {
            self.mailbox.notice(NoticeLevel::Error, error);
        }
    }

    pub(super) fn cli_model(
        &mut self,
        id: &str,
        provider: Option<String>,
        model: Option<String>,
        level: Option<String>,
    ) -> Result<(), String> {
        let harness = self.harness(id).ok_or_else(|| format!("Unknown thread {id}."))?;
        if let (Some(provider), Some(model)) = (provider, model) {
            if harness::harness_of(&provider) != harness {
                return Err(format!(
                    "A {} thread keeps its harness; start a new thread to use a {provider} model.",
                    name(harness)
                ));
            }
            self.store.update(id, |flags| flags.model = Some(model.clone()));
            self.save();
            if let Some(cli) = self.drivers.get_mut(id) {
                cli.driver.set_model(&model)?;
            }
            let reference = self.model_ref(harness, &model);
            let levels = self.thinking_levels(harness, Some(&model));
            if let Some(live) = self.threads.get_mut(id).and_then(|thread| thread.live.as_mut()) {
                live.state.model = Some(reference);
                live.state.thinking_levels = levels;
            }
        }
        if let Some(level) = level {
            self.store.update(id, |flags| flags.thinking_level = Some(level.clone()));
            self.save();
            let running = self.running(id);
            if let Some(cli) = self.drivers.get_mut(id) {
                if cli.driver.set_effort(&level).is_err() {
                    cli.restart = true;
                    if running {
                        self.mailbox.notice(
                            NoticeLevel::Info,
                            format!("{} takes the new thinking level when the running turn ends.", name(harness)),
                        );
                    } else {
                        self.cli_restart_due(id);
                    }
                }
            }
            if let Some(live) = self.threads.get_mut(id).and_then(|thread| thread.live.as_mut()) {
                live.state.thinking_level = level;
            }
        }
        self.emit_info(id);
        self.dirty = true;
        Ok(())
    }

    pub(super) async fn cli_restart(&mut self, id: &str) -> Result<String, String> {
        if let Some(mut cli) = self.drivers.remove(id) {
            cli.driver.kill();
        }
        self.cli_cancel(id, Some("Restart dropped this message")).await;
        if let Some(thread) = self.threads.get_mut(id) {
            thread.pending_run = false;
            thread.pending_at = None;
            thread.run_started_at = None;
            if let Some(live) = thread.live.as_mut() {
                live.state.busy = false;
                live.approvals.clear();
            }
        }
        self.cli_begin_open(id, Vec::new(), None)?;
        Ok(self.live_active(id).unwrap_or_default())
    }

    pub(super) fn cli_idle(&mut self) {
        let idle: Vec<String> = self
            .drivers
            .iter()
            .filter(|(id, cli)| {
                cli.used.elapsed() >= IDLE_KILL
                    && !self.running(id)
                    && self
                        .threads
                        .get(id.as_str())
                        .and_then(|thread| thread.live.as_ref())
                        .is_none_or(|live| live.approvals.is_empty())
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in idle {
            if let Some(mut cli) = self.drivers.remove(&id) {
                cli.driver.kill();
            }
            if self.focused.as_deref() != Some(id.as_str()) {
                if let Some(thread) = self.threads.get_mut(&id) {
                    thread.live = None;
                }
            }
        }
    }

    pub(super) fn cli_shutdown(&mut self) {
        for (_, mut cli) in self.drivers.drain() {
            cli.driver.kill();
        }
        for (_, opening) in self.opening.drain() {
            opening.cancel();
        }
    }

    fn cli_early(&mut self, id: &str, generation: u64, event: Event) -> Option<Event> {
        if !matches!(event, Event::Exited(_) | Event::Stopped(_)) {
            return Some(event);
        }
        match self.opening.get_mut(id).filter(|opening| opening.owns(generation)) {
            Some(opening) => {
                opening.early.push((generation, event));
                None
            }
            None => Some(event),
        }
    }

    pub(super) async fn cli_event(&mut self, envelope: Envelope) {
        let Envelope { thread: id, generation, event } = envelope;
        let event = match event {
            Event::Opened(report) => return self.cli_opened(&id, generation, *report).await,
            Event::Loaded(result) => return self.cli_loaded(&id, generation, result),
            event => event,
        };
        let Some(event) = self.cli_early(&id, generation, event) else { return };
        let harness = self.harness(&id).unwrap_or_default();
        if let Event::Usage(spend) = &event {
            if let Err(error) = harness::record_usage(&self.data_dir, &id, harness, spend) {
                self.mailbox.notice(NoticeLevel::Warning, format!("Could not record usage: {error}"));
            }
            return;
        }
        let current = match self.drivers.get_mut(&id).filter(|cli| cli.generation == generation) {
            Some(cli) => {
                cli.used = Instant::now();
                true
            }
            None => false,
        };
        let retired = matches!(event, Event::Stopped(_))
            && self.threads.get(&id).is_some_and(|thread| thread.retired == Some(generation));
        if !current && !retired {
            return;
        }
        match event {
            Event::Session(event) => self.cli_session(&id, &event).await,
            Event::Idle => {
                self.answered(&id);
                let busy =
                    self.threads.get(&id).and_then(|thread| thread.live.as_ref()).is_some_and(|live| live.state.busy);
                if !busy {
                    if let Some(thread) = self.threads.get_mut(&id) {
                        thread.pending_run = false;
                        thread.pending_at = None;
                        thread.run_started_at = None;
                        thread.stopping = None;
                    }
                    self.emit_info(&id);
                    self.dirty = true;
                    self.cli_restart_due(&id);
                }
            }
            Event::Approval(approval) => self.add_approval(id, approval),
            Event::ApprovalSettled(request) => self.settle_approval(&id, &request),
            Event::Context(context) => {
                if let Some(live) = self.threads.get_mut(&id).and_then(|thread| thread.live.as_mut()) {
                    live.state.context = Some(context);
                }
                self.emit_info(&id);
            }
            Event::Model(model) => {
                let reference = self.model_ref(harness, &model);
                if let Some(live) = self.threads.get_mut(&id).and_then(|thread| thread.live.as_mut()) {
                    if live.state.model.as_ref().is_none_or(|known| known.id != model) {
                        live.state.model = Some(reference);
                    }
                }
                self.emit_info(&id);
                self.dirty = true;
            }
            Event::Usage(_) | Event::Opened(_) | Event::Loaded(_) => {}
            Event::Failed(failure, error) => self.cli_failed(&id, failure, error),
            Event::Stopped(reason) => {
                if current {
                    if let Some(mut cli) = self.drivers.remove(&id) {
                        cli.driver.kill();
                    }
                }
                if !self.drivers.contains_key(&id) {
                    self.cli_halt(&id).await;
                }
                let title = self.title(&id);
                let body = format!(
                    "{} stopped on an account problem: {reason}. Local Studio will not retry; check the sign-in or plan in a terminal, then send again.",
                    name(harness)
                );
                self.mailbox.notice(NoticeLevel::Error, format!("“{title}”: {body}"));
                self.mailbox.push(Update::Notification {
                    thread: id,
                    kind: NotificationKind::Failed,
                    title,
                    body: super::truncate(&body, BODY_LIMIT),
                });
            }
            Event::Exited(failure) => {
                self.drivers.remove(&id);
                let was = self.running(&id);
                self.cli_halt(&id).await;
                if was || failure.is_some() {
                    let reason = failure.unwrap_or_else(|| "it exited during the turn".to_string());
                    self.mailbox.notice(NoticeLevel::Warning, format!("{} stopped: {reason}", name(harness)));
                }
            }
        }
    }

    fn cli_failed(&mut self, id: &str, failure: Failure, error: String) {
        match failure {
            Failure::Send(request) => {
                self.answered(id);
                match request {
                    Some(request) => self.mailbox.push(Update::RequestFailed { request, message: error }),
                    None => self.mailbox.notice(NoticeLevel::Error, error),
                }
            }
            Failure::Interrupt => {
                self.mailbox.notice(NoticeLevel::Warning, format!("The stop request did not go through: {error}"));
                self.itself.send(Command::StopCheck { thread: id.to_string() });
            }
            Failure::Rename => self.mailbox.notice(NoticeLevel::Warning, error),
            Failure::Compact => {
                self.answered(id);
                self.mailbox.notice(NoticeLevel::Error, error);
            }
        }
    }

    async fn cli_session(&mut self, id: &str, event: &SessionEvent) {
        self.apply_event(id, event, 0).await;
        let ended = matches!(event, SessionEvent::AgentEnd { .. });
        if let Some(cli) = self.drivers.get_mut(id).filter(|_| ended || matches!(event, SessionEvent::AgentStart)) {
            cli.epoch += 1;
        }
        if !ended {
            return;
        }
        if let Some(thread) = self.threads.get_mut(id) {
            thread.stopping = None;
        }
        self.answered(id);
        self.cli_restart_due(id);
        if let Some(cli) = self.drivers.get(id).filter(|cli| cli.restart && cli.unanswered > 0) {
            self.later(SETTLE, Command::CliSettle { thread: id.to_string(), epoch: cli.epoch });
        }
    }

    /// A CLI can drop or merge input it held (on Stop, for one), so it answers fewer sends than it got. When
    /// nothing was sent, started or ended for `SETTLE` after a turn ended, count the thread as settled.
    pub(super) fn cli_settle(&mut self, id: &str, epoch: u64) {
        let running = self.running(id);
        let Some(cli) = self.drivers.get_mut(id).filter(|cli| cli.epoch == epoch && !running) else { return };
        cli.unanswered = 0;
        self.cli_restart_due(id);
    }

    fn cli_restart_due(&mut self, id: &str) {
        let due = self.drivers.get(id).is_some_and(|cli| cli.restart && cli.unanswered == 0)
            && !self.running(id)
            && self
                .threads
                .get(id)
                .and_then(|thread| thread.live.as_ref())
                .is_none_or(|live| live.approvals.is_empty());
        if !due {
            return;
        }
        if let Some(mut cli) = self.drivers.remove(id) {
            cli.driver.kill();
            self.threads.entry(id.to_string()).or_default().retired = Some(cli.generation);
        }
    }

    async fn cli_halt(&mut self, id: &str) {
        let busy = self.threads.get(id).and_then(|thread| thread.live.as_ref()).is_some_and(|live| live.state.busy);
        if busy {
            if let Some(thread) = self.threads.get_mut(id) {
                thread.failed_at = Some(now_ms());
            }
            self.apply_event(id, &SessionEvent::AgentEnd { messages: Vec::new() }, 0).await;
        }
        if let Some(thread) = self.threads.get_mut(id) {
            thread.pending_run = false;
            thread.pending_at = None;
            thread.stopping = None;
            thread.run_started_at = None;
            if let Some(live) = thread.live.as_mut() {
                live.approvals.clear();
            }
        }
        if self.focused.as_deref() == Some(id) {
            self.emit_approvals(id);
        }
        self.emit_info(id);
        self.dirty = true;
    }

    pub(super) fn add_approval(&mut self, id: String, approval: Approval) {
        let body = super::truncate(&approval.command, BODY_LIMIT);
        let Some(live) = self.threads.get_mut(&id).and_then(|thread| thread.live.as_mut()) else { return };
        live.approvals.retain(|(known, _)| known.request_id != approval.request_id);
        live.approvals.push((approval, now_ms()));
        self.emit_approvals(&id);
        let title = self.title(&id);
        self.mailbox.push(Update::Notification { thread: id, kind: NotificationKind::NeedsApproval, title, body });
        self.dirty = true;
    }

    pub(super) fn load_codex_models(&self) {
        let itself = self.itself.clone();
        tokio::spawn(async move {
            if let Ok(models) = harness::codex_models().await {
                itself.send(Command::HarnessModels { models });
            }
        });
    }
}
