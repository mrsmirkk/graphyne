//! Graphyne's own settings at run time: loaded at start, changed from the Settings tab,
//! applied at once and saved to `settings.json` (see `graphyne_core::settings`).

use crate::theme;
use gpui::{App, AppContext, Global};
use graphyne_core::settings::Settings;
use graphyne_lua::SettingValue;

struct AppSettings(Settings);

impl Global for AppSettings {}

/// Load `settings.json` and apply it (before init.lua, which may still override sizes).
pub fn init(cx: &mut App) {
    let settings = graphyne_core::settings::load();
    apply(&settings);
    cx.set_global(AppSettings(settings));
}

pub fn get(cx: &App) -> Settings {
    cx.try_global::<AppSettings>().map(|s| s.0.clone()).unwrap_or_default()
}

/// Change settings: applied to every window immediately and saved in the background.
pub fn update(cx: &mut App, change: impl FnOnce(&mut Settings)) {
    let old = get(cx);
    let mut new = old.clone();
    change(&mut new);
    let new = new.sanitized();
    if new == old {
        return;
    }
    apply(&new);
    cx.set_global(AppSettings(new.clone()));
    if new.keymap != old.keymap {
        // Deferred: a keystroke being recorded may still be in dispatch.
        cx.defer(crate::lua_bridge::rebind);
    }
    if new.usage_hints != old.usage_hints {
        // Deferred: the Settings tab's own window is busy handling this click.
        cx.defer(|cx| {
            for editor in crate::all_editors(cx) {
                editor.update(cx, |e, cx| e.settings_changed(cx));
            }
        });
    }
    cx.background_spawn(async move {
        if let Err(e) = graphyne_core::settings::save(&new) {
            log::warn!("settings: {e:#}");
        }
    })
    .detach();
    cx.refresh_windows();
}

fn apply(s: &Settings) {
    // A plugin theme may not be registered yet at start-up; it is activated once it is.
    theme::activate_theme(&s.theme);
    theme::set_fonts(&s.editor_font, &s.terminal_font, &s.ui_font);
    theme::apply_setting("editor.font_size", &SettingValue::Number(s.editor_font_size as f64));
    theme::apply_setting("terminal.font_size", &SettingValue::Number(s.terminal_font_size as f64));
    theme::apply_setting("editor.autosave_delay_ms", &SettingValue::Number(s.autosave_delay_ms as f64));
    theme::set_flags(s.usage_hints, s.inlay_hints, s.autosave);
    graphyne_terminal::set_preferred_shell(Some(s.shell.clone()));
}
