//! The Settings tab: Graphyne's own options (fonts, autosave, terminal, startup), applied
//! as you change them. Themes come from plugins and are picked here too.

use crate::app_settings;
use crate::assets::Icon;
use crate::components::icon;
use crate::lua_bridge;
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, FocusHandle, Focusable, SharedString, Window, div, px};
use graphyne_core::settings::{AUTOSAVE_DELAYS_MS, Settings};

pub struct SettingsPage {
    focus_handle: FocusHandle,
    /// Shells found on this machine (looked up once per page).
    shells: Vec<(String, String)>,
}

impl Focusable for SettingsPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SettingsPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self { focus_handle: cx.focus_handle(), shells: graphyne_terminal::available_shells() }
    }
}

fn section(label: &'static str) -> gpui::Div {
    div().pt(px(22.)).pb(px(8.)).text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child(label)
}

fn card() -> gpui::Div {
    div().flex().flex_col().rounded(px(8.)).border_1().border_color(color(c::BORDER)).bg(color(c::POPUP))
}

/// One setting: name and explanation on the left, its control on the right.
fn row(first: bool, label: &'static str, help: &'static str, control: AnyElement) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .px(px(16.))
        .py(px(12.))
        .when(!first, |d| d.border_t_1().border_color(color(c::BORDER)))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .gap(px(2.))
                .child(div().text_color(color(c::TEXT)).child(label))
                .child(div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child(help)),
        )
        .child(div().flex_none().child(control))
}

/// A setting whose choices are a row of chips under its name.
fn choice_row(first: bool, label: &'static str, help: &'static str, chips: Vec<AnyElement>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .px(px(16.))
        .py(px(12.))
        .when(!first, |d| d.border_t_1().border_color(color(c::BORDER)))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_color(color(c::TEXT)).child(label))
                .child(div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child(help)),
        )
        .child(div().flex().flex_wrap().gap(px(6.)).children(chips))
}

fn chip(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, selected: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id.into())
        .flex()
        .items_center()
        .h(px(28.))
        .px(px(11.))
        .rounded(px(6.))
        .border_1()
        .cursor_pointer()
        .border_color(color(if selected { c::ACCENT } else { c::POPUP_BORDER }))
        .when(selected, |d| d.bg(color_a(c::ACCENT, 0.14)).text_color(color(c::TEXT)))
        .when(!selected, |d| d.text_color(color(c::TEXT_MUTED)).hover(|s| s.bg(color(c::HOVER)).text_color(color(c::TEXT))))
        .child(label.into())
}

fn toggle(id: &'static str, on: bool, change: impl Fn(&mut Settings, bool) + 'static) -> AnyElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .w(px(36.))
        .h(px(20.))
        .px(px(3.))
        .rounded_full()
        .cursor_pointer()
        .bg(color(if on { c::ACCENT } else { c::PRESSED }))
        .when(on, |d| d.justify_end())
        .child(div().size(px(14.)).rounded_full().bg(color(if on { 0xFFFFFF } else { c::TEXT_MUTED })))
        .on_click(move |_, _, cx| app_settings::update(cx, |s| change(s, !on)))
        .into_any_element()
}

/// `−  value  +` for numbers.
fn stepper(id: &'static str, value: String, enabled: bool, step: impl Fn(&mut Settings, f32) + Clone + 'static) -> AnyElement {
    let button = |sub: &'static str, label: &'static str, delta: f32| {
        let step = step.clone();
        div()
            .id(SharedString::from(format!("{id}-{sub}")))
            .flex()
            .items_center()
            .justify_center()
            .size(px(26.))
            .rounded(px(6.))
            .border_1()
            .border_color(color(c::POPUP_BORDER))
            .text_color(color(c::TEXT_MUTED))
            .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(color(c::HOVER)).text_color(color(c::TEXT))).on_click(move |_, _, cx| app_settings::update(cx, |s| step(s, delta))))
            .child(label)
    };
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .when(!enabled, |d| d.opacity(0.45))
        .child(button("dec", "−", -1.))
        .child(div().w(px(72.)).flex().justify_center().text_color(color(c::TEXT)).child(value))
        .child(button("inc", "+", 1.))
        .into_any_element()
}

fn size_label(v: f32) -> String {
    if v.fract() == 0. { format!("{v:.0} px") } else { format!("{v:.1} px") }
}

impl Render for SettingsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = app_settings::get(cx);

        // Appearance
        let active_theme = theme::active_theme();
        let mut themes = vec![(None::<String>, "Fleet Dark".to_string())];
        themes.extend(theme::theme_list().into_iter().map(|(name, _)| (Some(name.clone()), name)));
        let theme_chips = themes
            .into_iter()
            .enumerate()
            .map(|(ix, (key, name))| {
                let selected = key == active_theme;
                chip(("settings-theme", ix), name, selected).on_click(move |_, _, cx| lua_bridge::choose_theme(key.clone(), cx)).into_any_element()
            })
            .collect();
        let current_font = s.editor_font.clone();
        let mut font_chips: Vec<AnyElement> = vec![chip("settings-font-auto", "Automatic", current_font.is_none())
            .on_click(|_, _, cx| app_settings::update(cx, |s| s.editor_font = None))
            .into_any_element()];
        font_chips.extend(theme::code_fonts().into_iter().enumerate().map(|(ix, family)| {
            let selected = current_font.as_deref().is_some_and(|f| f.eq_ignore_ascii_case(family));
            chip(("settings-font", ix), family, selected)
                .font_family(family)
                .on_click(move |_, _, cx| app_settings::update(cx, |s| s.editor_font = Some(family.to_string())))
                .into_any_element()
        }));
        let appearance = card()
            .child(choice_row(true, "Theme", "Colour themes come from plugins (Plugins page); Fleet Dark is built in.", theme_chips))
            .child(choice_row(false, "Editor font", "Used in the editor and the terminal. Automatic picks JetBrains Mono when installed.", font_chips))
            .child(row(
                false,
                "Editor font size",
                "Line height follows the size.",
                stepper("settings-editor-size", size_label(s.editor_font_size), true, |s, d| s.editor_font_size += d * 0.5),
            ));

        // Editor
        let editor = card()
            .child(row(true, "Usage hints", "“N usages” above functions and types.", toggle("settings-usages", s.usage_hints, |s, v| s.usage_hints = v)))
            .child(row(false, "Inlay hints", "Types and parameter names from the language server.", toggle("settings-inlays", s.inlay_hints, |s, v| s.inlay_hints = v)))
            .child(row(false, "Autosave", "Save files shortly after you stop typing. Ctrl+S always saves.", toggle("settings-autosave", s.autosave, |s, v| s.autosave = v)))
            .child(row(
                false,
                "Autosave delay",
                "How long to wait after the last keystroke.",
                stepper("settings-autosave-delay", format!("{} ms", s.autosave_delay_ms), s.autosave, |s, d| {
                    let step = if s.autosave_delay_ms >= 2000 { 500 } else { 100 };
                    s.autosave_delay_ms = (s.autosave_delay_ms as i64 + d as i64 * step).clamp(*AUTOSAVE_DELAYS_MS.start() as i64, *AUTOSAVE_DELAYS_MS.end() as i64) as u64;
                }),
            ));

        // Terminal
        let shell_chips = self
            .shells
            .iter()
            .enumerate()
            .map(|(ix, (id, label))| {
                let id = id.clone();
                chip(("settings-shell", ix), label.clone(), id == s.shell).on_click(move |_, _, cx| app_settings::update(cx, |s| s.shell = id.clone())).into_any_element()
            })
            .collect();
        let terminal = card()
            .child(row(
                true,
                "Terminal font size",
                "Applies to every terminal tab.",
                stepper("settings-term-size", size_label(s.terminal_font_size), true, |s, d| s.terminal_font_size += d * 0.5),
            ))
            .child(choice_row(false, "Shell", "Used for new terminal tabs. Automatic: PowerShell 7 if installed, else Windows PowerShell.", shell_chips));

        // Startup
        let startup = card().child(row(
            true,
            "Reopen projects",
            "Open the projects from the last session. Off: always start with the Welcome screen.",
            toggle("settings-reopen", s.reopen_projects, |s, v| s.reopen_projects = v),
        ));

        let reset = div()
            .id("settings-reset")
            .flex()
            .items_center()
            .h(px(26.))
            .px(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(color(c::POPUP_BORDER))
            .cursor_pointer()
            .text_size(px(theme::UI_SMALL))
            .text_color(color(c::TEXT_MUTED))
            .hover(|s| s.bg(color(c::HOVER)).text_color(color(c::TEXT)))
            .child("Reset to defaults")
            .on_click(|_, _, cx| app_settings::update(cx, |s| *s = Settings::default()));
        let file = graphyne_core::session::state_dir().map(|d| d.join("settings.json").display().to_string()).unwrap_or_default();

        div()
            .id("settings-page")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_y_scroll()
            .font_family(theme::fonts().ui)
            .text_size(px(theme::UI_TEXT))
            .child(
                div()
                    .max_w(px(860.))
                    .mx_auto()
                    .px(px(28.))
                    .py(px(24.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(icon(Icon::Settings, 22., color(c::ACCENT)))
                            .child(div().text_size(px(20.)).text_color(color(c::TEXT)).child("Settings"))
                            .child(div().w(px(8.)))
                            .child(reset),
                    )
                    .child(section("APPEARANCE"))
                    .child(appearance)
                    .child(section("EDITOR"))
                    .child(editor)
                    .child(section("TERMINAL"))
                    .child(terminal)
                    .child(section("STARTUP"))
                    .child(startup)
                    .child(div().pt(px(18.)).text_size(px(11.5)).text_color(color(c::TEXT_FAINT)).child(format!("Changes apply at once and are saved to {file}"))),
            )
    }
}
