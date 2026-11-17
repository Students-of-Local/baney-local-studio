use std::rc::Rc;

use adw::prelude::*;
use local_studio_core::{Branch, GitStatus, ModelKey, ModelOption, Update};

use crate::app::{App, Dirty};
use crate::fixtures::{base, conversation, project_id, providers, FOCUSED};

pub const SCENES: [&str; 8] = [
    "picker-access",
    "picker-thinking",
    "picker-branches",
    "picker-project",
    "picker-stash",
    "picker-model",
    "picker-model-hidden",
    "picker-model-all-hidden",
];

fn key(provider: &str, model: &str) -> ModelKey {
    ModelKey { provider: provider.to_string(), model: model.to_string() }
}

pub fn models(favorites: Vec<ModelKey>, hidden: Vec<ModelKey>) -> Update {
    Update::Models { models: catalog(), favorites, hidden }
}

fn catalog() -> Vec<ModelOption> {
    [
        ("anthropic", "claude-sonnet-4-5", "Claude Sonnet 4.5", 200_000, true),
        ("anthropic", "claude-opus-4-1", "Claude Opus 4.1", 200_000, true),
        ("anthropic", "claude-haiku-4-5", "Claude Haiku 4.5", 200_000, true),
        ("openai", "gpt-5-codex", "GPT-5 Codex", 400_000, true),
        ("openai", "gpt-5", "GPT-5", 400_000, true),
        ("openai", "gpt-5-mini", "GPT-5 mini", 400_000, true),
        ("google", "gemini-2.5-pro", "Gemini 2.5 Pro", 1_048_576, false),
        ("local", "qwen3-coder-30b", "Qwen3 Coder 30B", 131_072, true),
        ("local", "gpt-oss-120b", "gpt-oss-120b", 131_072, true),
        ("local", "llama-3.3-70b", "Llama 3.3 70B", 0, true),
    ]
    .into_iter()
    .map(|(provider, id, name, context_window, configured)| ModelOption {
        provider: provider.to_string(),
        id: id.to_string(),
        name: name.to_string(),
        reasoning: false,
        thinking_levels: Vec::new(),
        context_window,
        configured,
        service_tiers: Vec::new(),
    })
    .collect()
}

pub fn hidden() -> Vec<ModelKey> {
    vec![key("openai", "gpt-5-mini"), key("local", "llama-3.3-70b")]
}

fn model_picker(app: &Rc<App>, hidden: Vec<ModelKey>, reveal: bool) {
    conversation(app, false);
    app.apply(vec![
        Update::Providers { providers: providers(), engines: Vec::new(), errors: Vec::new() },
        models(vec![key("anthropic", "claude-sonnet-4-5"), key("local", "qwen3-coder-30b")], hidden),
    ]);
    app.composer.open_picker("model");
    if reveal {
        app.composer.reveal_hidden_models(app);
    }
}

pub fn stage(app: &Rc<App>, scene: &str) {
    match scene {
        "picker-access" => {
            conversation(app, false);
            app.composer.open_picker("access");
        }
        "picker-thinking" => {
            conversation(app, false);
            let info = app.state.borrow().info.clone();
            if let Some(mut info) = info {
                info.thinking_level = "medium".to_string();
                app.apply(vec![Update::Thread { thread: FOCUSED.to_string(), info }]);
            }
            app.composer.open_picker("thinking");
        }
        "picker-branches" => branches(app),
        "picker-project" => {
            draft(app);
            app.hero.project.popup();
        }
        "picker-model" => model_picker(app, Vec::new(), false),
        "picker-model-hidden" => model_picker(app, hidden(), true),
        "picker-model-all-hidden" => {
            let all = catalog().iter().map(|model| key(&model.provider, &model.id)).collect();
            model_picker(app, all, false);
        }
        "picker-stash" => {
            conversation(app, false);
            app.composer.tools.set_fixture(&[
                "Refactor the drawer so splits remember their sizes",
                "Write the release notes for 0.5\n\n- drawer\n- launcher",
                "Check the GPU bars against the shared palette",
            ]);
            app.composer.tools.stash.popup();
        }
        _ => crate::dashboard_scenes::stage(app, scene),
    }
}

pub fn reset(app: &Rc<App>) {
    close(app.window.upcast_ref());
}

fn close(widget: &gtk::Widget) {
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(popover) = current.downcast_ref::<gtk::Popover>() {
            popover.popdown();
        }
        close(&current);
        child = current.next_sibling();
    }
}

fn draft(app: &Rc<App>) {
    app.apply(base());
    app.new_thread(Some(project_id("local-studio")));
    app.apply(vec![Update::GitStatus {
        project: project_id("local-studio"),
        status: GitStatus {
            branch: Some("linux/gtk".to_string()),
            upstream: Some("origin/linux/gtk".to_string()),
            ahead: 0,
            behind: 0,
            dirty: false,
            default_branch: Some("main".to_string()),
            pull_request: None,
            is_repo: true,
            changed_files: 0,
            has_remote: true,
        },
    }]);
    app.composer.set_text("");
}

fn branches(app: &Rc<App>) {
    draft(app);
    app.branches.preset(true, None);
    app.render(Dirty::all());
    app.branches.open_branch();
    let branch = |name: &str, current: bool, remote: bool, worktree: Option<&str>| Branch {
        name: name.to_string(),
        remote,
        current,
        worktree: worktree.map(str::to_string),
    };
    app.branches.loaded(
        &[
            branch("linux/gtk", true, false, None),
            branch("main", false, false, None),
            branch("feat/gtk-panels", false, false, Some("/home/gil/code/local-studio-worktrees/gtk-panels")),
            branch("swift/k", false, false, None),
            branch("origin/main", false, true, None),
            branch("origin/linux/gtk", false, true, None),
        ],
        24,
    );
}
