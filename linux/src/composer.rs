use std::cell::{Cell, RefCell};
use std::rc::Weak;

use adw::prelude::*;
use gtk::{gdk, glib};
use local_studio_core::{AccessMode, ApprovalAnswer, Connection, ModelKey, ModelOption, PromptDraft, SendShortcut};

use crate::app::{App, Dirty};
use crate::extras::Extras;
use crate::format;
use crate::icons;
use crate::picker::{Picker, Row, Spec};
use crate::state::State;

const PLACEHOLDER: &str = "Ask anything, @tag files/folders, or / for commands";
const ADD_PROVIDER: &str = "\u{1}add-provider";
const SHOW_HIDDEN: &str = "\u{1}show-hidden";
const HIDDEN_SECTION: &str = "hidden";

pub struct Composer {
    pub root: adw::Clamp,
    working: gtk::Label,
    queue: gtk::Box,
    approval: gtk::Box,
    approval_kind: gtk::Label,
    approval_count: gtk::Label,
    approval_command: gtk::Label,
    approval_effects: gtk::Label,
    approval_request: RefCell<Option<String>>,
    view: gtk::TextView,
    placeholder: gtk::Label,
    model: Picker,
    model_jumps: RefCell<Vec<ModelKey>>,
    model_providers: RefCell<Vec<(String, String)>>,
    focused_provider: RefCell<Option<String>>,
    show_hidden: Cell<bool>,
    thinking: Picker,
    access: Picker,
    stop: gtk::Button,
    run_started: Cell<Option<i64>>,
    shortcut: Cell<SendShortcut>,
    app: Weak<App>,
    pub extras: Extras,
    pub vim: crate::vim::Vim,
    pub tools: crate::tools::Tools,
    setup: crate::setup::SetupCard,
    pub options: crate::options::Options,
    markdown: Cell<bool>,
}

impl Composer {
    #[allow(clippy::too_many_lines)]
    pub fn new(app: Weak<App>) -> Self {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let working = gtk::Label::new(None);
        working.add_css_class("ls-working");
        working.set_xalign(0.0);
        working.set_margin_start(6);
        working.set_visible(false);
        let queue = gtk::Box::new(gtk::Orientation::Vertical, 4);
        queue.set_visible(false);

        let approval = gtk::Box::new(gtk::Orientation::Vertical, 6);
        approval.add_css_class("ls-approval");
        approval.set_visible(false);
        let approval_top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let shield = icons::styled(icons::APPROVAL, "status-approval");
        let approval_kind = gtk::Label::new(None);
        approval_kind.add_css_class("kind");
        approval_kind.set_xalign(0.0);
        approval_kind.set_hexpand(true);
        let approval_count = gtk::Label::new(None);
        approval_count.add_css_class("ls-meta");
        approval_top.append(&shield);
        approval_top.append(&approval_kind);
        approval_top.append(&approval_count);
        let approval_command = gtk::Label::new(None);
        approval_command.add_css_class("command");
        approval_command.set_xalign(0.0);
        approval_command.set_wrap(true);
        approval_command.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        approval_command.set_selectable(true);
        let approval_effects = gtk::Label::new(None);
        approval_effects.add_css_class("ls-meta");
        approval_effects.set_xalign(0.0);
        approval_effects.set_wrap(true);
        let approval_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        approval_buttons.set_halign(gtk::Align::End);
        let decline = gtk::Button::with_label("Decline");
        decline.add_css_class("flat");
        let approve = gtk::Button::with_label("Approve");
        approve.add_css_class("suggested-action");
        let always = gtk::Button::with_label("Always Allow");
        always.add_css_class("flat");
        always.set_tooltip_text(Some("Approve and allow these effects for the rest of this session"));
        approval_buttons.append(&decline);
        approval_buttons.append(&always);
        approval_buttons.append(&approve);
        approval.append(&approval_top);
        approval.append(&approval_command);
        approval.append(&approval_effects);
        approval.append(&approval_buttons);

        let view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .top_margin(6)
            .bottom_margin(6)
            .left_margin(6)
            .right_margin(6)
            .build();
        let vim = crate::vim::Vim::new(&view);
        let placeholder = gtk::Label::new(Some(PLACEHOLDER));
        placeholder.add_css_class("ls-placeholder");
        placeholder.set_halign(gtk::Align::Start);
        placeholder.set_valign(gtk::Align::Start);
        placeholder.set_margin_start(8);
        placeholder.set_margin_top(6);
        placeholder.set_can_target(false);
        placeholder.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .min_content_height(44)
            .max_content_height(240)
            .child(&view)
            .build();
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&scroller));
        overlay.add_overlay(&placeholder);

        let model = model_picker(&app);
        let thinking = thinking_picker(&app);
        let access = access_picker(&app);
        let stop = icons::button(icons::STOP, "Stop Generation");
        stop.add_css_class("ls-stop");
        stop.set_visible(false);
        let send = icons::button(icons::UP, "Send Message");
        send.add_css_class("ls-send");
        send.add_css_class("suggested-action");
        let extras = Extras::new(app.clone());
        let tools = crate::tools::Tools::new(&app);
        let setup = crate::setup::SetupCard::new(&app);
        let options = crate::options::Options::new(&app);
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        footer.append(&extras.attach);
        footer.append(&model.button);
        footer.append(&options.compare);
        footer.append(&thinking.button);
        footer.append(&access.button);
        footer.append(&options.tier.button);
        footer.append(&tools.meter);
        footer.append(&tools.stash.button);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        footer.append(&spacer);
        footer.append(&vim.label);
        footer.append(&stop);
        footer.append(&send);

        let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
        card.add_css_class("ls-composer");
        card.append(&extras.menu);
        card.append(&approval);
        card.append(&extras.chips);
        card.append(&overlay);
        card.append(&footer);
        column.append(&working);
        column.append(&options.goal);
        column.append(&setup.root);
        column.append(&queue);
        column.append(&card);
        let root = adw::Clamp::builder().maximum_size(820).tightening_threshold(640).child(&column).build();
        root.set_margin_start(20);
        root.set_margin_end(20);
        root.set_margin_bottom(16);
        root.set_margin_top(4);

        let buffer = view.buffer();
        let hint = placeholder.clone();
        let weak = app.clone();
        let watched = view.clone();
        buffer.connect_changed(move |buffer| {
            hint.set_visible(buffer.char_count() == 0);
            if let Some(app) = weak.upgrade() {
                app.composer.extras.changed(&app, &watched);
            }
        });
        let weak = app.clone();
        view.connect_paste_clipboard(move |view| {
            let clipboard = view.clipboard();
            if !clipboard.formats().contains_type(gdk::Texture::static_type()) {
                return;
            }
            view.stop_signal_emission_by_name("paste-clipboard");
            let weak = weak.clone();
            clipboard.read_texture_async(None::<&gtk::gio::Cancellable>, move |result| {
                if let (Some(app), Ok(Some(texture))) = (weak.upgrade(), result) {
                    app.composer.extras.attach_texture(&texture);
                }
            });
        });

        let weak = app.clone();
        send.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.composer.submit(&app);
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = app.clone();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(app) = weak.upgrade() else { return glib::Propagation::Proceed };
            let view = app.composer.view.clone();
            if app.composer.extras.key(&app, &view, key) == glib::Propagation::Stop {
                return glib::Propagation::Stop;
            }
            if app.composer.vim.key(&view, key, modifiers) == glib::Propagation::Stop {
                return glib::Propagation::Stop;
            }
            if key == gdk::Key::Escape && crate::keyboard::plain(modifiers) && crate::keyboard::composer_escape(&app) {
                return glib::Propagation::Stop;
            }
            let control = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
            if control && !shift && app.composer.markdown.get() {
                let marker = match key.to_lower() {
                    gdk::Key::b => Some("**"),
                    gdk::Key::i => Some("_"),
                    gdk::Key::e => Some("`"),
                    _ => None,
                };
                if let Some(marker) = marker {
                    app.composer.wrap_selection(marker);
                    return glib::Propagation::Stop;
                }
            }
            if !matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) {
                return glib::Propagation::Proceed;
            }
            let sends = match app.composer.shortcut.get() {
                SendShortcut::Enter => !shift && !control,
                SendShortcut::ModEnterMultiline | SendShortcut::ModEnter => control,
            };
            if sends {
                app.composer.submit(&app);
                return glib::Propagation::Stop;
            }
            if app.composer.shortcut.get() == SendShortcut::ModEnter && !shift {
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        view.add_controller(keys);
        let weak = app.clone();
        stop.connect_clicked(move |_| {
            let Some(app) = weak.upgrade() else { return };
            let thread = app.state.borrow().selected.clone();
            if let Some(thread) = thread {
                app.core.run(|studio| studio.abort(thread));
            }
        });
        for (button, answer) in [
            (&approve, ApprovalAnswer::Approve),
            (&always, ApprovalAnswer::AlwaysAllowSession),
            (&decline, ApprovalAnswer::Deny),
        ] {
            let weak = app.clone();
            button.connect_clicked(move |_| {
                let Some(app) = weak.upgrade() else { return };
                let thread = app.state.borrow().selected.clone();
                let request = app.composer.approval_request.borrow().clone();
                if let (Some(thread), Some(request)) = (thread, request) {
                    app.core.run(|studio| studio.answer_approval(thread, request, answer));
                }
            });
        }
        let weak = app.clone();
        let label = working.clone();
        glib::timeout_add_seconds_local(1, move || {
            let Some(app) = weak.upgrade() else { return glib::ControlFlow::Break };
            if let Some(started) = app.composer.run_started.get() {
                label.set_label(&format!(
                    "Working for {}",
                    format::duration(u64::try_from(format::now_ms() - started).unwrap_or(0))
                ));
            }
            glib::ControlFlow::Continue
        });

        Self {
            root,
            working,
            queue,
            approval,
            approval_kind,
            approval_count,
            approval_command,
            approval_effects,
            approval_request: RefCell::new(None),
            view,
            placeholder,
            model,
            model_jumps: RefCell::new(Vec::new()),
            model_providers: RefCell::new(Vec::new()),
            focused_provider: RefCell::new(None),
            show_hidden: Cell::new(false),
            thinking,
            access,
            stop,
            run_started: Cell::new(None),
            shortcut: Cell::new(SendShortcut::Enter),
            app,
            extras,
            vim,
            tools,
            setup,
            options,
            markdown: Cell::new(true),
        }
    }

    pub fn wrap_selection(&self, marker: &str) {
        let buffer = self.view.buffer();
        let (start, end) = buffer.selection_bounds().unwrap_or_else(|| {
            let cursor = buffer.iter_at_mark(&buffer.get_insert());
            (cursor, cursor)
        });
        let selected = buffer.text(&start, &end, false).to_string();
        let empty = selected.is_empty();
        let (mut start, mut end) = (start, end);
        buffer.begin_user_action();
        buffer.delete(&mut start, &mut end);
        buffer.insert(&mut start, &format!("{marker}{selected}{marker}"));
        if empty {
            let mut cursor = start;
            cursor.backward_chars(i32::try_from(marker.chars().count()).unwrap_or(0));
            buffer.place_cursor(&cursor);
        }
        buffer.end_user_action();
    }

    pub fn text(&self) -> String {
        let buffer = self.view.buffer();
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string()
    }

    pub fn set_text(&self, text: &str) {
        self.view.buffer().set_text(text);
    }

    pub fn view(&self) -> gtk::TextView {
        self.view.clone()
    }

    pub fn reset(&self) {
        self.set_text("");
        self.extras.reset();
        self.vim.reset(&self.view);
    }

    pub fn type_text(&self, app: &std::rc::Rc<App>, text: &str) {
        let buffer = self.view.buffer();
        buffer.set_text(text);
        buffer.place_cursor(&buffer.end_iter());
        self.extras.changed(app, &self.view);
    }

    pub fn open_picker(&self, which: &str) {
        match which {
            "model" => self.model.popup(),
            "thinking" => self.thinking.popup(),
            _ => self.access.popup(),
        }
    }

    pub fn model_picker_open(&self) -> bool {
        self.model.popover.is_visible()
    }

    fn choose_model(&self, app: &std::rc::Rc<App>, id: &str) {
        match id {
            ADD_PROVIDER => app.open_settings(Some("providers")),
            SHOW_HIDDEN => {
                self.show_hidden.set(!self.show_hidden.get());
                self.render_models(&app.state.borrow());
            }
            _ => {
                if let Some((_, key)) = model_key(id) {
                    select_model(app, key);
                }
            }
        }
    }

    pub fn reveal_hidden_models(&self, app: &std::rc::Rc<App>) {
        self.show_hidden.set(true);
        self.render_models(&app.state.borrow());
    }

    pub fn jump_model(&self, app: &std::rc::Rc<App>, index: usize) {
        let key = index.checked_sub(1).and_then(|index| self.model_jumps.borrow().get(index).cloned());
        if let Some(key) = key {
            self.model.popdown();
            select_model(app, key);
        }
    }

    pub fn step_provider(&self, state: &State, delta: isize) {
        let providers = self.model_providers.borrow();
        let Ok(count) = isize::try_from(providers.len()) else { return };
        if count < 2 {
            return;
        }
        let current = self.focused_provider.borrow().clone().or_else(|| state.active_model().map(|key| key.provider));
        let index = current
            .and_then(|current| providers.iter().position(|(id, _)| *id == current))
            .and_then(|index| isize::try_from(index).ok())
            .unwrap_or(if delta > 0 { -1 } else { count });
        let Some((id, title)) =
            usize::try_from((index + delta).rem_euclid(count)).ok().and_then(|next| providers.get(next))
        else {
            return;
        };
        self.focused_provider.replace(Some(id.clone()));
        self.model.reveal_section(title);
    }

    pub fn render_model_title(&self, state: &State) {
        let title = crate::options::fan_out_title(state).unwrap_or_else(|| {
            let thread = state.selected.as_ref().and(state.info.as_ref()).and_then(|info| info.model.as_ref());
            match (thread, state.active_model()) {
                (Some(model), _) => model.name.clone().unwrap_or_else(|| model.id.clone()),
                (None, Some(key)) => state.model_option(&key).map_or(key.model, |model| model_name(model).to_string()),
                (None, None) => "Model".to_string(),
            }
        });
        self.model.set_label(&title);
    }

    fn render_models(&self, state: &State) {
        let show_hidden = self.show_hidden.get();
        let sections = local_studio_core::model_sections(
            state.models.clone(),
            state.favorites.clone(),
            state.hidden.clone(),
            state.providers.clone(),
            self.model.search_text(),
            show_hidden,
            state.models_loaded,
        );
        let active = state.active_model();
        let mut rows = Vec::new();
        for section in &sections.sections {
            for entry in &section.models {
                let model = &entry.model;
                let id = format!("{}\u{1}{}\u{1}{}", section.id, model.provider, model.id);
                let checked =
                    active.as_ref().is_some_and(|key| key.provider == model.provider && key.model == model.id);
                let mut row =
                    Row::new(id, model_name(model)).section(&section.title).checked(checked).dimmed(entry.hidden);
                if model.context_window > 0 {
                    row = row.aside(format::tokens(model.context_window));
                }
                if let Some(jump) = entry.jump {
                    row = row.detail(format!("Ctrl+{jump}"));
                }
                rows.push(match (entry.hidden, entry.favorite) {
                    (true, _) => row.accessory(icons::WATCH, "Unhide model", true),
                    (false, true) => row.accessory(icons::STARRED, "Remove from favorites", true),
                    (false, false) => row.accessory(icons::STAR_ADD, "Add to favorites", false),
                });
            }
        }
        if sections.hidden > 0 || show_hidden {
            let title = if sections.hidden == 1 {
                "Show 1 hidden model".to_string()
            } else {
                format!("Show {} hidden models", sections.hidden)
            };
            rows.push(Row::new(SHOW_HIDDEN, title).switch(show_hidden));
        }
        rows.push(Row::new(ADD_PROVIDER, "Add Provider…").icon(icons::ADD));
        self.model.set_rows(rows);
        self.model.set_empty(sections.empty.as_deref());
        self.model.set_search_visible(sections.configured > 8);
        self.model_providers.replace(
            sections
                .sections
                .iter()
                .filter(|section| sections.providers.contains(&section.id))
                .map(|section| (section.id.clone(), section.title.clone()))
                .collect(),
        );
        self.model_jumps.replace(sections.jumps);
        self.render_model_title(state);
    }

    pub fn submit_steer(&self, app: &std::rc::Rc<App>) {
        app.force_steer.set(true);
        self.submit(app);
        app.force_steer.set(false);
    }

    pub fn focus(&self) {
        self.view.grab_focus();
    }

    pub fn submit(&self, app: &std::rc::Rc<App>) {
        if self.approval.is_visible() {
            return;
        }
        let text = self.text().trim().to_string();
        if text.is_empty() && !self.extras.has_attachments() {
            return;
        }
        let (images, files, contexts) = self.extras.take();
        self.extras.remember(&text);
        self.set_text("");
        let mut prompt: Vec<String> =
            contexts.iter().map(|(label, body)| format!("{label}:\n```\n{body}\n```")).collect();
        if !text.is_empty() {
            prompt.push(text.clone());
        }
        app.send(PromptDraft { text: prompt.join("\n\n"), images, files }, text);
    }

    #[allow(clippy::too_many_lines)]
    pub fn render(&self, state: &State) {
        self.shortcut.set(state.settings.send_shortcut);
        self.vim.render(&self.view, state);
        self.markdown.set(state.settings.rich_text_composer);
        self.tools.render(state);
        self.setup.render(state);
        self.options.render(state);
        let info = state.selected.as_ref().and(state.info.as_ref());
        let busy = info.is_some_and(|info| info.busy);
        self.stop.set_visible(busy);
        let status = info.and_then(|info| {
            if info.compacting {
                Some("Compacting…".to_string())
            } else if info.retry_attempt > 0 {
                Some(format!("Retrying (attempt {})…", info.retry_attempt))
            } else if info.busy {
                let started = info.run_started_at.unwrap_or_else(format::now_ms);
                Some(format!(
                    "Working for {}",
                    format::duration(u64::try_from(format::now_ms() - started).unwrap_or(0))
                ))
            } else {
                None
            }
        });
        self.run_started.set(
            info.filter(|info| info.busy && !info.compacting && info.retry_attempt == 0)
                .map(|info| info.run_started_at.unwrap_or_else(format::now_ms)),
        );
        self.working.set_visible(status.is_some());
        self.working.set_label(status.as_deref().unwrap_or_default());

        while let Some(child) = self.queue.first_child() {
            self.queue.remove(&child);
        }
        let sending: Vec<&str> = state
            .pending
            .iter()
            .filter(|pending| pending.queued && state.selected.as_deref() == Some(pending.thread.as_str()))
            .map(|pending| pending.text.as_str())
            .collect();
        self.queue.set_visible(!state.queue.is_empty() || !sending.is_empty());
        for queued in &state.queue {
            self.queue.append(&queued_row(&self.app, state, &queued.id, &queued.text, queued.sending, queued.steering));
        }
        for text in sending {
            self.queue.append(&sending_row(text));
        }

        let approval = state.approvals.first();
        self.approval.set_visible(approval.is_some());
        self.approval_request.replace(approval.map(|approval| approval.request_id.clone()));
        if let Some(approval) = approval {
            let files = approval.effects.iter().any(|effect| {
                let effect = effect.to_lowercase();
                effect.contains("write") || effect.contains("edit") || effect.contains("file")
            });
            let kind = if files { "File change approval" } else { "Command approval" };
            self.approval_kind.set_label(&match &approval.agent {
                Some(agent) if !agent.is_empty() => format!("{kind} · {agent}"),
                _ => kind.to_string(),
            });
            self.approval_count.set_label(&format!("1/{}", state.approvals.len()));
            self.approval_count.set_visible(state.approvals.len() > 1);
            self.approval_command.set_label(&approval.command);
            self.approval_effects.set_label(&approval.effects.join(" · "));
            self.approval_effects.set_visible(!approval.effects.is_empty());
        }
        self.view.set_editable(approval.is_none());
        let connected = matches!(state.connection, Some(Connection::Ready)) || state.connection.is_none();
        self.placeholder.set_label(if approval.is_some() {
            "Resolve this approval request to continue"
        } else if !connected {
            "Waiting for the Eukhe daemon…"
        } else if busy {
            "Queue a follow-up…"
        } else {
            PLACEHOLDER
        });

        self.render_models(state);

        let levels = state.thinking_levels();
        self.thinking.button.set_visible(!levels.is_empty());
        let current =
            state.active_thinking().filter(|level| levels.contains(level)).or_else(|| levels.first().cloned());
        let rows: Vec<Row> = levels
            .iter()
            .map(|level| Row::new(level, level_title(level)).checked(current.as_ref() == Some(level)))
            .collect();
        self.thinking.set_rows(rows);
        self.thinking.set_label(&current.as_deref().map(level_title).unwrap_or_default());

        let access: AccessMode = state.active_access();
        self.access.set_rows(access_rows(access));
        self.access.set_label(format::access_label(access));
        self.access.set_icon(Some(access_icon(access)));
        self.access.button.set_tooltip_text(Some(format::access_detail(access)));
    }
}

fn select_model(app: &std::rc::Rc<App>, key: ModelKey) {
    let thread = app.state.borrow().selected.clone();
    match thread {
        Some(thread) => app.core.run(|studio| studio.set_model(thread, key.provider, key.model)),
        None => app.state.borrow_mut().draft_settings.model = Some(key),
    }
    app.render(Dirty { composer: true, ..Dirty::default() });
}

fn model_picker(app: &Weak<App>) -> Picker {
    let weak = app.clone();
    let spec = Spec { tooltip: "Model", search: Some("Search models"), width: 320, ..Spec::default() };
    let picker = Picker::new(&spec, move |id| {
        if let Some(app) = weak.upgrade() {
            app.composer.choose_model(&app, id);
        }
    });
    let weak = app.clone();
    picker.connect_accessory(move |id| {
        let Some(app) = weak.upgrade() else { return };
        let Some((section, key)) = model_key(id) else { return };
        if section == HIDDEN_SECTION {
            app.core.run(|studio| studio.set_hidden_model(key.provider, key.model, false));
            return;
        }
        let favorite = app.state.borrow().favorites.contains(&key);
        app.core.run(|studio| studio.set_favorite_model(key.provider, key.model, !favorite));
    });
    let weak = app.clone();
    picker.connect_search(move || {
        if let Some(app) = weak.upgrade() {
            app.composer.render_models(&app.state.borrow());
        }
    });
    let weak = app.clone();
    picker.popover.connect_map(move |_| {
        let Some(app) = weak.upgrade() else { return };
        crate::bindings::model_picker_open(&app, true);
        if app.state.borrow().models.is_empty() {
            app.core.run(local_studio_core::Studio::load_models);
        }
    });
    let weak = app.clone();
    picker.popover.connect_closed(move |_| {
        let Some(app) = weak.upgrade() else { return };
        crate::bindings::model_picker_open(&app, false);
        app.composer.show_hidden.set(false);
        app.composer.focused_provider.replace(None);
        app.composer.model.set_search_text("");
        app.composer.render_models(&app.state.borrow());
    });
    picker
}

fn model_key(id: &str) -> Option<(&str, ModelKey)> {
    let mut parts = id.splitn(3, '\u{1}');
    let (section, provider, model) = (parts.next()?, parts.next()?, parts.next()?);
    Some((section, ModelKey { provider: provider.to_string(), model: model.to_string() }))
}

fn model_name(model: &ModelOption) -> &str {
    if model.name.is_empty() {
        &model.id
    } else {
        &model.name
    }
}

fn thinking_picker(app: &Weak<App>) -> Picker {
    let weak = app.clone();
    let spec = Spec { tooltip: "Thinking", title: Some("Thinking"), width: 220, ..Spec::default() };
    Picker::new(&spec, move |level| {
        let Some(app) = weak.upgrade() else { return };
        if app.syncing.get() {
            return;
        }
        let level = level.to_string();
        let thread = app.state.borrow().selected.clone();
        match thread {
            Some(thread) => app.core.run(|studio| studio.set_thinking_level(thread, level)),
            None => app.state.borrow_mut().draft_settings.thinking = Some(level),
        }
        app.render(Dirty { composer: true, ..Dirty::default() });
    })
}

fn access_picker(app: &Weak<App>) -> Picker {
    let weak = app.clone();
    let spec = Spec { tooltip: "Permissions", title: Some("Access"), width: 300, ..Spec::default() };
    Picker::new(&spec, move |id| {
        let Some(app) = weak.upgrade() else { return };
        if app.syncing.get() {
            return;
        }
        let Some(mode) = id.parse::<usize>().ok().and_then(|index| format::ACCESS_MODES.get(index).copied()) else {
            return;
        };
        let thread = app.state.borrow().selected.clone();
        match thread {
            Some(thread) => app.core.run(|studio| studio.set_access_mode(thread, mode)),
            None => app.state.borrow_mut().draft_settings.access = Some(mode),
        }
        app.render(Dirty { composer: true, ..Dirty::default() });
    })
}

fn access_rows(current: AccessMode) -> Vec<Row> {
    format::ACCESS_MODES
        .iter()
        .enumerate()
        .map(|(index, mode)| {
            Row::new(index.to_string(), format::access_label(*mode))
                .icon(access_icon(*mode))
                .subtitle(format::access_detail(*mode))
                .checked(*mode == current)
        })
        .collect()
}

fn access_icon(mode: AccessMode) -> &'static str {
    match mode {
        AccessMode::Supervised => icons::LOCKED,
        AccessMode::AutoAcceptEdits => icons::EDIT,
        AccessMode::Auto => icons::RUN,
        AccessMode::FullAccess => icons::UNLOCKED,
    }
}

fn level_title(level: &str) -> String {
    match level {
        "off" => "Thinking off".to_string(),
        "xhigh" => "Extra high".to_string(),
        other => format::capitalize(other),
    }
}

fn sending_row(text: &str) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("ls-bubble");
    row.add_css_class("queued");
    row.set_halign(gtk::Align::End);
    let icon = icons::styled(icons::TIME, "dim-label");
    row.append(&icon);
    let label = gtk::Label::new(Some(text));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(60);
    label.set_xalign(0.0);
    row.append(&label);
    let status = gtk::Label::new(Some("Sending"));
    status.add_css_class("ls-meta");
    row.append(&status);
    row.upcast()
}

fn queued_row(app: &Weak<App>, state: &State, id: &str, text: &str, sending: bool, steering: bool) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("ls-bubble");
    row.add_css_class("queued");
    row.set_halign(gtk::Align::End);
    let icon = icons::styled(icons::TIME, "dim-label");
    row.append(&icon);
    let label = gtk::Label::new(Some(text));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(60);
    label.set_xalign(0.0);
    row.append(&label);
    let status = gtk::Label::new(Some(if sending {
        "Sending"
    } else if steering {
        "Steering"
    } else {
        "Queued"
    }));
    status.add_css_class("ls-meta");
    row.append(&status);
    let thread = state.selected.clone().unwrap_or_default();
    for (icon, tooltip, now) in
        [(icons::UP, "Send Now", true), (icons::CLOSE, "Cancel and Return to the Composer", false)]
    {
        let button = icons::flat(icon, tooltip);
        button.add_css_class("circular");
        let thread = thread.clone();
        let id = id.to_string();
        let text = text.to_string();
        let weak = app.clone();
        button.connect_clicked(move |_| {
            let Some(app) = weak.upgrade() else { return };
            let (thread, id) = (thread.clone(), id.clone());
            if now {
                app.core.run(|studio| studio.send_queued_now(thread, id));
            } else {
                app.core.run(|studio| studio.remove_queued(thread, id));
                app.composer.set_text(&text);
            }
        });
        row.append(&button);
    }
    row.upcast()
}
