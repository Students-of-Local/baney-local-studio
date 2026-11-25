use sha2::{Digest, Sha256};

use super::{Actor, Command};
use crate::model::{Block, Item, ItemKind, NoticeLevel, PromptDraft, SkillDraft, SkillFile, Update};
use crate::skills::{self, Draft, CREATE};

const WORD: &str = "@create";

#[derive(Default)]
pub(super) struct Making {
    pub(super) asked: bool,
    offered: Option<SkillDraft>,
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

fn answers(items: &[Item]) -> impl Iterator<Item = &[Item]> {
    let asked = items.iter().enumerate().rev().filter(|(_, item)| asks(item));
    asked.map(|(at, _)| {
        let after = &items[at + 1..];
        let next = after.iter().position(|item| matches!(item.kind, ItemKind::User { .. }));
        &after[..next.unwrap_or(after.len())]
    })
}

fn parse(answer: &[Item]) -> Result<Draft, String> {
    let texts: Vec<String> = answer
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Assistant { blocks, error: None, .. } => Some(blocks),
            _ => None,
        })
        .map(|blocks| {
            let texts = blocks.iter().filter_map(|block| match block {
                Block::Text { text } => Some(text.as_str()),
                Block::Thinking { .. } => None,
            });
            texts.collect::<Vec<_>>().join("\n")
        })
        .collect();
    let whole = Draft::parse(&texts.join("\n"));
    whole.or_else(|reason| texts.iter().rev().find_map(|text| Draft::parse(text).ok()).ok_or(reason))
}

fn id(draft: &Draft) -> String {
    let mut hasher = Sha256::new();
    for part in draft.files.iter().flat_map(|(path, text)| [path, text]).chain([&draft.text]) {
        hasher.update(part.len().to_le_bytes());
        hasher.update(part);
    }
    hasher.finalize().iter().fold(String::new(), |id, byte| format!("{id}{byte:02x}"))
}

impl Actor {
    fn skill_answer(&self, thread: &str) -> Option<Draft> {
        let live = self.view(thread)?;
        let items = live.transcript.items();
        let running = live.state.busy && answering(items);
        answers(items).skip(usize::from(running)).find_map(|answer| parse(answer).ok())
    }

    fn skill_draft(&self, thread: &str) -> Option<SkillDraft> {
        let draft = self.skill_answer(thread)?;
        let id = id(&draft);
        if self.store.flags(thread).skills_done.contains(&id) {
            return None;
        }
        let replaces = draft.replaces(&self.data_dir);
        let files = draft.files.into_iter().map(|(path, text)| SkillFile { path, text }).collect();
        let Draft { name, description, folder, text, marked, left, .. } = draft;
        Some(SkillDraft { id, name, description, folder, text, files, marked, left, replaces })
    }

    fn push_skill_draft(&mut self, id: &str, draft: Option<SkillDraft>) {
        if let Some(thread) = self.threads.get_mut(id) {
            thread.skill.offered.clone_from(&draft);
        }
        self.mailbox.push(Update::SkillDraft { thread: id.to_string(), draft });
    }

    pub(super) fn emit_skill_draft(&mut self, id: &str) {
        let draft = self.skill_draft(id);
        if draft.is_some() || self.threads.get(id).is_some_and(|thread| thread.skill.offered.is_some()) {
            self.push_skill_draft(id, draft);
        }
    }

    pub(super) fn skill_turn_ended(&mut self, id: &str) {
        let Some(live) = self.view(id) else { return };
        let items = live.transcript.items();
        let Some(answer) = answers(items).next().filter(|_| !(live.state.busy && answering(items))) else { return };
        let failed = matches!(answer.last().map(|item| &item.kind), Some(ItemKind::Assistant { error: Some(_), .. }));
        let (parsed, ours) = (parse(answer), answering(items));
        let Some(thread) = self.threads.get_mut(id) else { return };
        let told = ours && thread.skill.asked && thread.stopping.is_none() && thread.failed_at.is_none();
        thread.skill.asked &= ours && failed && parsed.is_err();
        if let Some(again) = parsed.as_ref().ok().filter(|_| told).map(self::id) {
            if self.store.flags(id).skills_done.contains(&again) {
                self.store.update(id, |flags| flags.skills_done.retain(|done| *done != again));
                self.save();
            }
        }
        if let (Err(reason), true, false) = (&parsed, told, failed) {
            let text = format!("The reply holds no skill. Send @create again. {reason}");
            self.mailbox.notice(NoticeLevel::Warning, text);
        }
        let draft = self.skill_draft(id);
        if self.threads.get(id).is_some_and(|thread| thread.skill.offered != draft) {
            self.push_skill_draft(id, draft);
        }
    }

    pub(super) fn save_skill(&mut self, request: String, id: String, draft: &str, text: String) {
        if let Err(message) = self.start_skill_save(&request, id, draft, text) {
            self.mailbox.push(Update::RequestFailed { request, message });
        }
    }

    fn start_skill_save(&mut self, request: &str, thread: String, wanted: &str, text: String) -> Result<(), String> {
        let draft = self.skill_answer(&thread).ok_or("This thread holds no skill to save.")?;
        if id(&draft) != wanted {
            return Err("A newer reply holds another skill. Read it, then save.".to_string());
        }
        if let Some(plugin) = self.store.flags(&thread).local_only {
            return Err(format!(
                "This thread ran {plugin}, which runs only on local models, and any model can read a saved skill. It wasn't saved."
            ));
        }
        let name = draft.name.to_lowercase();
        let draft = draft.with_text(text)?;
        let same = draft.name.to_lowercase() == name;
        if draft.replaces(&self.data_dir) && !(same && self.skill_replaces(&thread, wanted)) {
            self.emit_skill_draft(&thread);
            let then = if same { "Save again to replace it" } else { "Give this one another name" };
            return Err(format!("You already have a skill named {}. {then}.", draft.name));
        }
        let making = self.threads.get_mut(&thread).ok_or_else(|| format!("Unknown thread {thread}."))?;
        if making.skill.saving {
            return Err("This thread's skill is being saved.".to_string());
        }
        making.skill.saving = true;
        let (request, saved, data) = (request.to_string(), wanted.to_string(), self.data_dir.clone());
        let (plugins, itself) = (self.plugins_dir.clone(), self.itself.clone());
        tokio::task::spawn_blocking(move || {
            let roots = skills::roots(&data, plugins.as_deref(), None);
            let store = roots.and_then(|roots| skills::Store::open(&data, roots));
            let outcome = store.and_then(|mut store| store.save(&draft));
            let outcome = outcome.map(|_| format!("Saved. Use @{} in any thread.", draft.name));
            itself.send(Command::SkillSaved { request, thread, draft: saved, outcome });
        });
        Ok(())
    }

    pub(super) fn skill_saved(&mut self, request: String, id: &str, draft: String, outcome: Result<String, String>) {
        if let Some(thread) = self.threads.get_mut(id) {
            thread.skill.saving = false;
        }
        match outcome {
            Ok(message) => {
                self.mailbox.push(Update::RequestDone { request, message });
                self.finish_skill(id, draft);
            }
            Err(message) => self.mailbox.push(Update::RequestFailed { request, message }),
        }
    }

    fn skill_replaces(&self, thread: &str, draft: &str) -> bool {
        let offered = self.threads.get(thread).and_then(|thread| thread.skill.offered.as_ref());
        offered.is_some_and(|offered| offered.id == draft && offered.replaces)
    }

    pub(super) fn discard_skill(&mut self, id: &str, draft: String) {
        let offered = self.threads.get(id).and_then(|thread| thread.skill.offered.as_ref());
        if offered.is_some_and(|offered| offered.id == draft) {
            self.finish_skill(id, draft);
        }
    }

    fn finish_skill(&mut self, id: &str, draft: String) {
        let Some(thread) = self.threads.get(id) else { return };
        let offered = thread.skill.offered.as_ref().is_some_and(|offered| offered.id == draft);
        self.store.update(id, |flags| {
            flags.skills_done.retain(|done| *done != draft);
            flags.skills_done.push(draft);
        });
        self.save();
        if offered {
            self.push_skill_draft(id, None);
        }
    }
}
