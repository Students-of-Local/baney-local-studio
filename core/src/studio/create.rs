use super::{Actor, Command};
use crate::model::{Block, Item, ItemKind, NoticeLevel, PromptDraft, SkillDraft, SkillFile, Update};
use crate::skills::{self, Draft, CREATE, SKILL};

const WORD: &str = "@create";

#[derive(Default)]
pub(super) struct Making {
    pub(super) asked: bool,
    offered: Option<SkillDraft>,
    done: Option<String>,
    saving: bool,
}

pub(super) fn asked(prompt: &PromptDraft) -> bool {
    let rest = prompt.text.trim_start().strip_prefix(WORD);
    rest.is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

pub(super) fn message(message: &str, expanded: &str) -> String {
    let skills = expanded.strip_suffix(message).unwrap_or_default();
    let rest = message.trim_start().strip_prefix(WORD).unwrap_or_default().trim_start();
    format!("{CREATE}\n\n{skills}{rest}").trim_end().to_string()
}

pub(super) fn answering(items: &[Item]) -> bool {
    let user = items.iter().rfind(|item| matches!(item.kind, ItemKind::User { .. }));
    user.is_some_and(asks)
}

fn asks(item: &Item) -> bool {
    let first = CREATE.lines().next().unwrap_or(CREATE);
    matches!(&item.kind, ItemKind::User { text, .. } if text.starts_with(first))
}

fn answer(items: &[Item]) -> Option<&[Item]> {
    let after = &items[items.iter().rposition(asks)? + 1..];
    let next = after.iter().position(|item| matches!(item.kind, ItemKind::User { .. }));
    Some(&after[..next.unwrap_or(after.len())])
}

fn parse(answer: &[Item]) -> Result<Draft, String> {
    let blocks = answer.iter().filter_map(|item| match &item.kind {
        ItemKind::Assistant { blocks, error: None, .. } => Some(blocks),
        _ => None,
    });
    let texts = blocks.flatten().filter_map(|block| match block {
        Block::Text { text } => Some(text.as_str()),
        Block::Thinking { .. } => None,
    });
    Draft::parse(&texts.collect::<Vec<_>>().join("\n"))
}

impl Actor {
    fn skill_answer(&self, id: &str) -> Option<&[Item]> {
        let live = self.view(id)?;
        let items = live.transcript.items();
        answer(items).filter(|_| !(live.state.busy && answering(items)))
    }

    fn skill_draft(&self, id: &str, draft: Draft) -> Option<SkillDraft> {
        let done = self.threads.get(id).and_then(|thread| thread.skill.done.as_ref()) == Some(&draft.text);
        let file = self.data_dir.join("skills").join(&draft.folder).join(SKILL);
        if done || std::fs::read_to_string(file).is_ok_and(|saved| saved == draft.text) {
            return None;
        }
        let replaces = draft.replaces(&self.data_dir);
        let files = draft.files.into_iter().map(|(path, text)| SkillFile { path, text }).collect();
        let Draft { name, description, folder, text, marked, left, .. } = draft;
        Some(SkillDraft { name, description, folder, text, files, marked, left, replaces })
    }

    fn push_skill_draft(&mut self, id: &str, draft: Option<SkillDraft>) {
        if let Some(thread) = self.threads.get_mut(id) {
            thread.skill.offered.clone_from(&draft);
        }
        self.mailbox.push(Update::SkillDraft { thread: id.to_string(), draft });
    }

    pub(super) fn emit_skill_draft(&mut self, id: &str) {
        let parsed = self.skill_answer(id).map(parse);
        if parsed.is_some() || self.threads.get(id).is_some_and(|thread| thread.skill.offered.is_some()) {
            let draft = parsed.and_then(Result::ok).and_then(|draft| self.skill_draft(id, draft));
            self.push_skill_draft(id, draft);
        }
    }

    pub(super) fn skill_turn_ended(&mut self, id: &str) {
        let Some(thread) = self.threads.get_mut(id) else { return };
        let asked = std::mem::take(&mut thread.skill.asked);
        let told = asked && thread.stopping.is_none() && thread.failed_at.is_none();
        let Some(answer) = self.skill_answer(id) else { return };
        let failed = matches!(answer.last().map(|item| &item.kind), Some(ItemKind::Assistant { error: Some(_), .. }));
        let parsed = parse(answer);
        if let (Err(reason), true, false) = (&parsed, told, failed) {
            let text = format!("The reply holds no skill. Send @create again. {reason}");
            self.mailbox.notice(NoticeLevel::Warning, text);
        }
        let draft = parsed.ok().and_then(|draft| self.skill_draft(id, draft));
        if self.threads.get(id).is_some_and(|thread| thread.skill.offered != draft) {
            self.push_skill_draft(id, draft);
        }
    }

    pub(super) fn save_skill(&mut self, request: String, id: String, text: String) {
        if let Err(message) = self.start_skill_save(&request, id, text) {
            self.mailbox.push(Update::RequestFailed { request, message });
        }
    }

    fn start_skill_save(&mut self, request: &str, id: String, text: String) -> Result<(), String> {
        let draft = parse(self.skill_answer(&id).ok_or("This thread holds no skill to save.")?)?;
        let thread = self.threads.get_mut(&id).ok_or_else(|| format!("Unknown thread {id}."))?;
        if thread.skill.saving {
            return Err("This thread's skill is being saved.".to_string());
        }
        let made = draft.text.clone();
        let draft = draft.with_text(text)?;
        thread.skill.saving = true;
        let (request, data, itself) = (request.to_string(), self.data_dir.clone(), self.itself.clone());
        tokio::task::spawn_blocking(move || {
            let store = skills::roots(&data, None, None).and_then(|roots| skills::Store::open(&data, roots));
            let outcome = store.and_then(|mut store| store.save(&draft));
            let outcome = outcome.map(|_| format!("Saved. Use @{} in any thread.", draft.name));
            itself.send(Command::SkillSaved { request, thread: id, text: made, outcome });
        });
        Ok(())
    }

    pub(super) fn skill_saved(&mut self, request: String, id: &str, text: String, outcome: Result<String, String>) {
        if let Some(thread) = self.threads.get_mut(id) {
            thread.skill.saving = false;
        }
        match outcome {
            Ok(message) => {
                self.mailbox.push(Update::RequestDone { request, message });
                self.finish_skill(id, text);
            }
            Err(message) => self.mailbox.push(Update::RequestFailed { request, message }),
        }
    }

    pub(super) fn discard_skill(&mut self, id: &str) {
        let draft = self.threads.get(id).and_then(|thread| thread.skill.offered.as_ref());
        if let Some(text) = draft.map(|draft| draft.text.clone()) {
            self.finish_skill(id, text);
        }
    }

    fn finish_skill(&mut self, id: &str, text: String) {
        let Some(thread) = self.threads.get_mut(id) else { return };
        let offered = thread.skill.offered.as_ref().is_some_and(|draft| draft.text == text);
        thread.skill.done = Some(text);
        if offered {
            self.push_skill_draft(id, None);
        }
    }
}
