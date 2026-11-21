use std::sync::PoisonError;

use super::{Mailbox, Studio};
use crate::model::{NoticeLevel, Project, ThreadRow, Update};

#[derive(Default)]
pub(crate) struct Rows {
    threads: Option<Vec<ThreadRow>>,
    projects: Option<Vec<Project>>,
    remote: Vec<Remote>,
}

#[cfg_attr(not(feature = "link"), allow(dead_code))]
struct Remote {
    machine: String,
    threads: Vec<ThreadRow>,
    projects: Vec<Project>,
}

impl Rows {
    fn threads(&self) -> Vec<ThreadRow> {
        let local = self.threads.iter().flatten();
        local.chain(self.remote.iter().flat_map(|remote| &remote.threads)).cloned().collect()
    }

    fn projects(&self) -> Vec<Project> {
        let local = self.projects.iter().flatten();
        local.chain(self.remote.iter().flat_map(|remote| &remote.projects)).cloned().collect()
    }
}

#[cfg(feature = "link")]
impl Rows {
    fn remote(&mut self, machine: &str) -> &mut Remote {
        let at = self.remote.iter().position(|remote| remote.machine == machine).unwrap_or_else(|| {
            self.remote.push(Remote { machine: machine.to_string(), threads: Vec::new(), projects: Vec::new() });
            self.remote.len() - 1
        });
        &mut self.remote[at]
    }
}

pub(crate) fn split(id: &str) -> Option<(&str, &str)> {
    id.strip_prefix('@')?.split_once('/')
}

impl Mailbox {
    pub(super) fn local_projects(&self, projects: Vec<Project>) {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        rows.projects = Some(projects);
        self.enqueue(Update::Projects { projects: rows.projects() });
    }

    pub(super) fn local_threads(&self, threads: Vec<ThreadRow>) {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        rows.threads = Some(threads);
        self.queue_threads(rows.threads());
    }
}

#[cfg(feature = "link")]
impl Mailbox {
    pub(crate) fn remote_threads(&self, machine: &str, threads: Vec<ThreadRow>) {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        rows.remote(machine).threads = threads;
        self.queue_threads(rows.threads());
    }

    pub(crate) fn remote_projects(&self, machine: &str, projects: Vec<Project>) {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        rows.remote(machine).projects = projects;
        self.enqueue(Update::Projects { projects: rows.projects() });
    }

    pub(crate) fn forget_rows(&self, machine: &str) {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        let before = rows.remote.len();
        rows.remote.retain(|remote| remote.machine != machine);
        if rows.remote.len() != before {
            self.queue_threads(rows.threads());
            self.enqueue(Update::Projects { projects: rows.projects() });
        }
    }

    fn unread(&self, thread: &str) -> bool {
        let rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        rows.remote.iter().flat_map(|remote| &remote.threads).any(|row| row.id == thread && row.unread)
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
#[allow(clippy::needless_pass_by_value)]
impl Studio {
    pub fn pair_machine(&self, link: String) {
        #[cfg(feature = "link")]
        self.links.pair(&link);
        #[cfg(not(feature = "link"))]
        self.mailbox.notice(NoticeLevel::Error, format!("This build cannot link machines, so {link:?} was not used"));
    }

    pub fn forget_machine(&self, machine: String) {
        #[cfg(feature = "link")]
        self.links.forget(&machine);
        #[cfg(not(feature = "link"))]
        self.mailbox.notice(NoticeLevel::Error, format!("This build cannot link machines, so {machine} is not linked"));
    }

    pub fn open_pairing(&self, request: String) {
        #[cfg(feature = "link")]
        self.links.open_pairing(request, self.data_dir.clone());
        #[cfg(not(feature = "link"))]
        self.mailbox.push(Update::RequestFailed { request, message: "This build cannot pair devices".to_string() });
    }

    pub fn list_paired_devices(&self) {
        #[cfg(feature = "link")]
        self.publish_devices(crate::link::devices::Devices::new(&self.data_dir).list());
        #[cfg(not(feature = "link"))]
        self.mailbox.push(Update::PairedDevices { devices: Vec::new() });
    }

    #[cfg_attr(not(feature = "link"), allow(unused_variables))]
    pub fn revoke_paired_device(&self, device: String) {
        #[cfg(feature = "link")]
        {
            let devices = crate::link::devices::Devices::new(&self.data_dir);
            match devices.revoke(&device) {
                Ok(gone) if gone.is_empty() => {
                    self.mailbox.notice(NoticeLevel::Error, "That device was already revoked".to_string());
                }
                Ok(_) => {}
                Err(error) => {
                    self.mailbox.notice(NoticeLevel::Error, format!("Could not revoke that device: {error:#}"));
                }
            }
            self.publish_devices(devices.list());
        }
        #[cfg(not(feature = "link"))]
        self.mailbox.notice(NoticeLevel::Error, "This build cannot pair devices".to_string());
    }
}

#[cfg(feature = "link")]
impl Studio {
    fn publish_devices(&self, listed: anyhow::Result<Vec<crate::link::devices::Device>>) {
        match listed {
            Ok(listed) => {
                let mut devices: Vec<crate::model::PairedDevice> = listed
                    .into_iter()
                    .map(|device| crate::model::PairedDevice {
                        id: device.info.id,
                        name: device.info.name,
                        platform: device.info.platform,
                        app: device.info.app,
                        channel: device.info.channel,
                        first_seen: device.first_seen,
                        last_seen: device.last_seen,
                    })
                    .collect();
                devices.sort_by_key(|device| std::cmp::Reverse(device.last_seen));
                self.mailbox.push(Update::PairedDevices { devices });
            }
            Err(error) => self.mailbox.notice(NoticeLevel::Error, format!("Could not read paired devices: {error:#}")),
        }
    }
}

impl Studio {
    pub(crate) fn on_machine(&self, id: &str, what: &str, request: Option<String>) -> bool {
        let Some((machine, _)) = split(id) else { return false };
        #[cfg(feature = "link")]
        let machine = self.links.name(machine);
        let message = format!("{what} works only in Local Studio on {machine}, not over a link");
        match request {
            Some(request) => self.mailbox.push(Update::RequestFailed { request, message }),
            None => self.mailbox.notice(NoticeLevel::Info, message),
        }
        true
    }

    #[cfg(feature = "link")]
    pub(crate) fn forward(
        &self,
        op: &str,
        project: &str,
        mut args: serde_json::Value,
        request: Option<String>,
    ) -> bool {
        let Some((machine, id)) = split(project) else { return false };
        args["project"] = serde_json::Value::String(id.to_string());
        self.links.request(machine, op, args, None, request);
        true
    }
}

#[cfg(feature = "link")]
mod route {
    use serde_json::{json, Value};

    use super::{split, Studio};
    use crate::studio::Command;

    fn remote(id: &str) -> bool {
        split(id).is_some()
    }

    impl Studio {
        fn thread_op(&self, op: &str, thread: &str, mut args: Value) {
            let Some((machine, id)) = split(thread) else { return };
            args["thread"] = Value::String(id.to_string());
            self.links.request(machine, op, args, None, None);
        }

        #[allow(clippy::too_many_lines)]
        pub(in crate::studio) fn route(&self, command: Command) -> Option<Command> {
            match command {
                Command::SelectThread { thread } => {
                    let Some((machine, id)) = thread.as_deref().and_then(split) else {
                        self.links.select(None);
                        return Some(Command::SelectThread { thread });
                    };
                    self.links.select(Some((machine, id)));
                    if thread.as_deref().is_some_and(|thread| self.mailbox.unread(thread)) {
                        self.links.request(
                            machine,
                            "mark_unread",
                            json!({ "thread": id, "unread": false }),
                            None,
                            None,
                        );
                    }
                    Some(Command::SelectThread { thread: None })
                }
                Command::WatchThread { thread } if remote(&thread) => {
                    let (machine, id) = split(&thread)?;
                    self.links.watch(machine, id);
                    None
                }
                Command::UnwatchThread { thread } if remote(&thread) => {
                    let (machine, id) = split(&thread)?;
                    self.links.unwatch(machine, id);
                    None
                }
                Command::StartThread { draft, project: Some(project), prompt, settings } if remote(&project) => {
                    let (machine, id) = split(&project)?;
                    let args = json!({ "draft": draft, "project": id, "settings": settings });
                    self.links.request(machine, "start_thread", args, Some(prompt), Some(draft));
                    None
                }
                Command::StartThreads { draft, project: Some(project), .. } if remote(&project) => {
                    self.on_machine(&project, "Starting a thread on several models", Some(draft));
                    None
                }
                Command::Send { request, thread, prompt, delivery } if remote(&thread) => {
                    let (machine, id) = split(&thread)?;
                    let args = json!({ "request": request, "thread": id, "delivery": delivery });
                    self.links.request(machine, "send", args, Some(prompt), Some(request));
                    None
                }
                Command::Abort { thread } if remote(&thread) => {
                    self.thread_op("abort", &thread, json!({}));
                    None
                }
                Command::SendQueuedNow { thread, queued } if remote(&thread) => {
                    self.thread_op("send_queued_now", &thread, json!({ "queued": queued }));
                    None
                }
                Command::RemoveQueued { thread, queued } if remote(&thread) => {
                    self.thread_op("remove_queued", &thread, json!({ "queued": queued }));
                    None
                }
                Command::AnswerApproval { thread, request, answer } if remote(&thread) => {
                    self.thread_op("answer_approval", &thread, json!({ "request": request, "answer": answer }));
                    None
                }
                Command::RenameThread { thread, title } if remote(&thread) => {
                    self.thread_op("rename_thread", &thread, json!({ "title": title }));
                    None
                }
                Command::SetArchived { thread, archived } if remote(&thread) => {
                    self.thread_op("set_archived", &thread, json!({ "archived": archived }));
                    None
                }
                Command::SetPinned { thread, pinned } if remote(&thread) => {
                    self.thread_op("set_pinned", &thread, json!({ "pinned": pinned }));
                    None
                }
                Command::SetSettled { thread, settled } if remote(&thread) => {
                    self.thread_op("set_settled", &thread, json!({ "settled": settled }));
                    None
                }
                Command::Snooze { thread, until } if remote(&thread) => {
                    self.thread_op("snooze", &thread, json!({ "until": until }));
                    None
                }
                Command::MarkUnread { thread, unread } if remote(&thread) => {
                    self.thread_op("mark_unread", &thread, json!({ "unread": unread }));
                    None
                }
                Command::ForkThread { thread } if remote(&thread) => {
                    self.thread_op("fork_thread", &thread, json!({}));
                    None
                }
                Command::Compact { thread } if remote(&thread) => {
                    self.thread_op("compact", &thread, json!({}));
                    None
                }
                Command::RegenerateTitle { thread } if remote(&thread) => {
                    self.thread_op("regenerate_title", &thread, json!({}));
                    None
                }
                Command::SetModel { thread, provider, model } if remote(&thread) => {
                    self.thread_op("set_model", &thread, json!({ "provider": provider, "model": model }));
                    None
                }
                Command::SetThinkingLevel { thread, level } if remote(&thread) => {
                    self.thread_op("set_thinking_level", &thread, json!({ "level": level }));
                    None
                }
                Command::SetAccessMode { thread, mode } if remote(&thread) => {
                    self.thread_op("set_access_mode", &thread, json!({ "mode": mode }));
                    None
                }
                Command::CancelSubagent { thread, child } if remote(&thread) => {
                    self.thread_op("cancel_subagent", &thread, json!({ "child": child }));
                    None
                }
                Command::ListCommands { request, project: Some(project) } if remote(&project) => {
                    self.forward("list_commands", &project, json!({ "request": request }), Some(request.clone()));
                    None
                }
                Command::ListSkills { request, project: Some(project) } if remote(&project) => {
                    self.forward("list_skills", &project, json!({ "request": request }), Some(request.clone()));
                    None
                }
                Command::LoadMessageImage { request, thread, .. } if remote(&thread) => {
                    self.on_machine(&thread, "Opening images from a linked machine's thread", Some(request));
                    None
                }
                Command::DeleteThread { thread } if remote(&thread) => {
                    self.on_machine(&thread, "Deleting a thread", None);
                    None
                }
                Command::EditFromHere { thread, .. } if remote(&thread) => {
                    self.on_machine(&thread, "Editing from a message", None);
                    None
                }
                Command::SetAutoSettle { thread, .. } if remote(&thread) => {
                    self.on_machine(&thread, "Auto settle", None);
                    None
                }
                Command::SetServiceTier { thread, .. } if remote(&thread) => {
                    self.on_machine(&thread, "The service tier", None);
                    None
                }
                Command::RestartThread { thread } if remote(&thread) => {
                    self.on_machine(&thread, "Restarting a thread", None);
                    None
                }
                Command::RemoveProject { project } if remote(&project) => {
                    self.on_machine(&project, "Removing a project", None);
                    None
                }
                Command::RenameProject { project, .. } if remote(&project) => {
                    self.on_machine(&project, "Renaming a project", None);
                    None
                }
                Command::SetProjectIcon { project, .. } if remote(&project) => {
                    self.on_machine(&project, "A project's icon", None);
                    None
                }
                Command::ReorderProjects { projects } => {
                    Some(Command::ReorderProjects { projects: projects.into_iter().filter(|id| !remote(id)).collect() })
                }
                Command::ReorderPinned { threads } => {
                    Some(Command::ReorderPinned { threads: threads.into_iter().filter(|id| !remote(id)).collect() })
                }
                command => Some(command),
            }
        }
    }
}
