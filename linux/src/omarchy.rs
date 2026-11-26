use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::colors::{Colors, Syntax};

pub const NAME: &str = "Omarchy";

thread_local! {
    static CACHE: RefCell<Option<(String, Colors)>> = const { RefCell::new(None) };
    static MONITORS: RefCell<Vec<gio::FileMonitor>> = const { RefCell::new(Vec::new()) };
}

fn current_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".local").join("state")));
    state
        .map(|state| state.join("omarchy").join("current"))
        .into_iter()
        .chain(home.map(|home| home.join(".config").join("omarchy").join("current")))
        .collect()
}

fn colors_file() -> Option<PathBuf> {
    current_dirs().into_iter().map(|dir| dir.join("theme").join("colors.toml")).find(|path| path.is_file())
}

pub fn present() -> bool {
    colors_file().is_some()
}

fn read() -> Option<String> {
    std::fs::read_to_string(colors_file()?).ok()
}

fn palette(text: &str) -> Option<BTreeMap<String, String>> {
    let table: toml::Table = text.parse().ok()?;
    let palette: BTreeMap<String, String> = table
        .into_iter()
        .filter_map(|(key, value)| value.as_str().map(|value| (key, value.trim().to_string())))
        .filter(|(key, value)| key == "mode" || gdk::RGBA::parse(value).is_ok())
        .collect();
    (palette.contains_key("background") && palette.contains_key("foreground")).then_some(palette)
}

fn rgba(hex: &str) -> gdk::RGBA {
    gdk::RGBA::parse(hex).unwrap_or(gdk::RGBA::BLACK)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn hex(color: &gdk::RGBA) -> String {
    format!("#{:02x}{:02x}{:02x}", byte(color.red()), byte(color.green()), byte(color.blue()))
}

fn mix(from: &str, to: &str, amount: f32) -> String {
    let (from, to) = (rgba(from), rgba(to));
    let blend = |a: f32, b: f32| a + (b - a) * amount;
    hex(&gdk::RGBA::new(
        blend(from.red(), to.red()),
        blend(from.green(), to.green()),
        blend(from.blue(), to.blue()),
        1.0,
    ))
}

fn luminance(color: &str) -> f32 {
    let color = rgba(color);
    0.2126 * color.red() + 0.7152 * color.green() + 0.0722 * color.blue()
}

fn leak(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

fn build(palette: &BTreeMap<String, String>) -> Colors {
    let get = |keys: &[&str], fallback: &str| {
        keys.iter().find_map(|key| palette.get(*key)).map_or_else(|| fallback.to_string(), |value| hex(&rgba(value)))
    };
    let background = get(&["background"], "#1a1b26");
    let foreground = get(&["foreground"], "#c0caf5");
    let dark = palette.get("mode").map_or_else(|| luminance(&background) < 0.5, |mode| mode != "light");
    let shade = if dark { "#000000" } else { "#ffffff" };
    let accent = get(&["accent", "blue", "color4"], &foreground);
    let red = get(&["red", "color1"], "#f7768e");
    let green = get(&["green", "color2"], "#9ece6a");
    let yellow = get(&["yellow", "color3"], "#e0af68");
    let blue = get(&["blue", "color4"], &accent);
    let magenta = get(&["magenta", "color5"], "#bb9af7");
    let cyan = get(&["cyan", "color6"], "#7dcfff");
    let orange = get(&["orange", "bright_yellow", "color11"], &yellow);
    let purple = get(&["purple", "bright_magenta", "color13"], &magenta);
    let muted = get(&["muted", "color8"], &mix(&foreground, &background, 0.45));
    let secondary = get(&["dark_foreground", "light_foreground"], &mix(&foreground, &background, 0.2));
    let selection = get(&["selection", "selection_background"], &mix(&background, &accent, 0.3));
    let ansi_default = [
        background.clone(),
        red.clone(),
        green.clone(),
        yellow.clone(),
        blue.clone(),
        magenta.clone(),
        cyan.clone(),
        foreground.clone(),
        muted.clone(),
        get(&["bright_red"], &red),
        get(&["bright_green"], &green),
        get(&["bright_yellow"], &yellow),
        get(&["bright_blue"], &blue),
        get(&["bright_magenta"], &magenta),
        get(&["bright_cyan"], &cyan),
        get(&["bright_foreground"], &foreground),
    ];
    let ansi: Vec<&'static str> = ansi_default
        .iter()
        .enumerate()
        .map(|(index, fallback)| leak(get(&[&format!("color{index}")], fallback)))
        .collect();
    let on_accent = if luminance(&accent) > 0.55 { mix(&background, "#000000", 0.2) } else { "#ffffff".to_string() };
    let alpha = |color: &str, amount: f32| {
        let color = rgba(color);
        format!("rgba({},{},{},{amount:.2})", byte(color.red()), byte(color.green()), byte(color.blue()))
    };
    Colors {
        dark,
        canvas: leak(background.clone()),
        sidebar: leak(mix(&background, shade, if dark { 0.22 } else { 0.04 })),
        card: leak(mix(&background, &foreground, 0.05)),
        raised: leak(mix(&background, &foreground, 0.08)),
        key: leak(mix(&background, &foreground, 0.12)),
        text: leak(foreground.clone()),
        secondary: leak(secondary),
        muted: leak(muted.clone()),
        hairline: leak(format!("alpha({foreground}, 0.08)")),
        accent: leak(accent.clone()),
        on_accent: leak(on_accent),
        ok: leak(green.clone()),
        warn: leak(yellow.clone()),
        error: leak(red.clone()),
        info: leak(blue.clone()),
        added: leak(green.clone()),
        removed: leak(red.clone()),
        added_bg: leak(alpha(&green, 0.12)),
        removed_bg: leak(alpha(&red, 0.12)),
        code_bg: leak(mix(&background, shade, if dark { 0.18 } else { 0.03 })),
        terminal_bg: leak(background.clone()),
        terminal_fg: leak(foreground.clone()),
        cursor: leak(get(&["cursor"], &foreground)),
        selection: leak(selection),
        syntax: Syntax {
            default: leak(foreground.clone()),
            keyword: leak(magenta.clone()),
            operator: leak(cyan.clone()),
            string: leak(green),
            function: leak(blue),
            types: leak(yellow),
            property: leak(foreground.clone()),
            constant: leak(orange.clone()),
            number: leak(orange),
            parameter: leak(foreground),
            builtin: leak(purple),
            comment: leak(muted),
            punctuation: leak(cyan),
        },
        ansi: std::array::from_fn(|index| ansi[index]),
    }
}

pub fn colors() -> Option<Colors> {
    let text = read()?;
    if let Some(colors) = CACHE
        .with(|cache| cache.borrow().as_ref().filter(|(known, _)| *known == text).map(|(_, colors)| colors.clone()))
    {
        return Some(colors);
    }
    let colors = build(&palette(&text)?);
    CACHE.with(|cache| cache.replace(Some((text, colors.clone()))));
    Some(colors)
}

pub fn theme() -> Option<crate::themes::Theme> {
    let colors = colors()?;
    Some(crate::themes::Theme {
        name: NAME.to_string(),
        dark: colors.dark,
        canvas: colors.canvas.to_string(),
        sidebar: colors.sidebar.to_string(),
        card: colors.card.to_string(),
        text: colors.text.to_string(),
        accent: colors.accent.to_string(),
    })
}

pub fn watch(changed: impl Fn() + Clone + 'static) {
    let pending = std::rc::Rc::new(std::cell::Cell::new(false));
    let mut monitors = Vec::new();
    for dir in current_dirs() {
        for (path, directory) in
            [(dir.clone(), true), (dir.join("theme"), true), (dir.join("theme").join("colors.toml"), false)]
        {
            let file = gio::File::for_path(&path);
            let monitor = if directory {
                file.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            } else {
                file.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            };
            let Ok(monitor) = monitor else { continue };
            let pending = pending.clone();
            let changed = changed.clone();
            monitor.connect_changed(move |_, _, _, _| {
                if pending.replace(true) {
                    return;
                }
                let pending = pending.clone();
                let changed = changed.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                    pending.set(false);
                    changed();
                    watch(changed);
                });
            });
            monitors.push(monitor);
        }
    }
    MONITORS.with(|known| known.replace(monitors));
}
