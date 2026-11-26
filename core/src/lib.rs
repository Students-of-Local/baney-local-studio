#![cfg_attr(feature = "ffi", allow(unsafe_code))]

mod access;
mod daemon;
pub mod fleet;
pub mod frontmatter;
mod harness;
mod highlight;
mod images;
#[cfg(feature = "link")]
pub mod link;
mod links;
mod model;
mod oneshot;
mod platform;
pub mod plugins;
mod scratch;
mod session;
pub mod skills;
mod store;
mod studio;
mod threads;
mod tools;
mod transcript;
pub mod tunnel;
pub mod update;
#[cfg(feature = "wire")]
mod wire;
mod workspace;

pub use local_studio_version::{CHANNEL, VERSION};

pub use access::{default_access, model_tier, ModelTier};
pub use highlight::{highlight, highlight_lines, CodeSegment, SyntaxKind};
#[doc(hidden)]
pub use links::open::{link_command, LinkEnv, Platform};
pub use links::refused_link;
pub use links::studio::{link_words, outside_link, studio_link, LinkWords, StudioLink};
pub use model::{
    AccessMode, Approval, ApprovalAnswer, Block, CatalogPlugin, CommandKind, Connection, ConsentAnswer, ConsentNeed,
    ConsentRequest, ConsentRow, ContextUsage, DaemonMismatch, Delivery, FileChange, Goal, Harness, ImageAttachment,
    ImageThumbnail, Item, ItemKind, Machine, MachineState, Mention, MentionKind, ModelKey, ModelOption, ModelRef,
    NoticeLevel, NotificationKind, PairedDevice, PluginEntry, PluginGrant, PluginRow, PluginState, PluginUpdate,
    Project, PromptDraft, Question, QuestionOption, QueuedMessage, Shelf, SkillDraft, SkillEntry, SkillFile,
    SkillSource, SlashCommand, Subagent, ThreadInfo, ThreadRow, ThreadSearchHit, ThreadSettings, ThreadStatus,
    ToolKind, ToolState, TranscriptOp, Update, WorktreeChoice,
};
pub use scratch::{check_scratch_agent, stored_keys_empty};
pub use store::{data_dir, harness_threads, ListedThread};
pub use studio::{
    process_name, serve_holds_lock, DaemonLaunch, Studio, StudioConfig, StudioError, Subscription, Waker,
};
pub use workspace::model::*;
pub use workspace::t3::import_from_t3;
pub use workspace::{endpoint_presets, terminal_preview};

#[cfg_attr(feature = "ffi", uniffi::export)]
#[must_use]
pub fn build_version() -> String {
    VERSION.to_string()
}

#[cfg_attr(feature = "ffi", uniffi::export)]
#[must_use]
pub fn build_channel() -> String {
    CHANNEL.to_string()
}

#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!("local_studio_core");
