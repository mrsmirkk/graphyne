//! The Settings tab: Graphyne's own options (fonts, autosave, terminal, startup), applied
//! as you change them. Themes come from plugins and are picked here too.

use crate::app_settings;
use crate::assets::Icon;
use crate::components::icon;
use crate::keymap;
use crate::lua_bridge;
use crate::theme::{self, c, color, color_a};
use crate::text_input::{TextInput, TextInputEvent};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, Keystroke, SharedString, Subscription, UniformListScrollHandle, Window, anchored,
    deferred, div, px, uniform_list,
};
use graphyne_core::settings::{AUTOSAVE_DELAYS_MS, Settings};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
enum FontTarget {
    Editor,
    Terminal,
    Interface,
}

impl FontTarget {
    fn get(self, s: &Settings) -> &str {
        match self {
            FontTarget::Editor => &s.editor_font,
            FontTarget::Terminal => &s.terminal_font,
            FontTarget::Interface => &s.ui_font,
        }
    }
    fn set(self, s: &mut Settings, family: String) {
        match self {
            FontTarget::Editor => s.editor_font = family,
            FontTarget::Terminal => s.terminal_font = family,
            FontTarget::Interface => s.ui_font = family,
        }
    }
    fn id(self) -> &'static str {
        match self {
            FontTarget::Editor => "font-editor",
            FontTarget::Terminal => "font-terminal",
            FontTarget::Interface => "font-ui",
        }
    }
}

/// The open font dropdown: a search box over the installed fonts.
struct FontPicker {
    target: FontTarget,
    search: Entity<TextInput>,
    /// Installed fonts matching the search.
    matches: Vec<&'static str>,
    selected: usize,
    scroll: UniformListScrollHandle,
    _sub: Subscription,
}

pub struct SettingsPage {
    focus_handle: FocusHandle,
    /// Shells found on this machine (looked up once per page).
    shells: Vec<(String, String)>,
    picker: Option<FontPicker>,
    /// When a dropdown was closed by a click outside it (that click must not reopen it).
    picker_closed: Option<(FontTarget, Instant)>,
    /// Filters the Keymap section.
    keymap_query: Entity<TextInput>,
    /// The command whose new shortcut is being recorded, and which of its shortcuts it replaces.
    recording: Option<(&'static str, Option<usize>)>,
    /// Takes keystrokes before any shortcut acts on them while recording.
    intercept: Option<Subscription>,
    _query_sub: Subscription,
}

impl Focusable for SettingsPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SettingsPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let keymap_query = cx.new(|cx| TextInput::new("Search commands or shortcuts", cx));
        let query_sub = cx.subscribe(&keymap_query, |_, input, ev: &TextInputEvent, cx| match ev {
            TextInputEvent::Changed(_) => cx.notify(),
            TextInputEvent::Cancel => input.update(cx, |i, cx| i.set_text("", cx)),
            _ => {}
        });
        Self {
            focus_handle: cx.focus_handle(),
            shells: graphyne_terminal::available_shells(),
            picker: None,
            picker_closed: None,
            keymap_query,
            recording: None,
            intercept: None,
            _query_sub: query_sub,
        }
    }

    /// Record the next keystroke as a shortcut of `id` (replacing its shortcut `replace`).
    fn start_recording(&mut self, id: &'static str, replace: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_recording(cx);
        self.recording = Some((id, replace));
        window.focus(&self.focus_handle);
        let page = cx.entity().downgrade();
        self.intercept = Some(cx.intercept_keystrokes(move |ev, _, cx| {
            let Some(page) = page.upgrade() else { return };
            // No shortcut acts on this keystroke: it is the one being recorded.
            cx.stop_propagation();
            let keystroke = ev.keystroke.clone();
            page.update(cx, |p, cx| p.recorded(&keystroke, cx));
        }));
        cx.notify();
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        // The interceptor may be running right now: drop it once this keystroke is handled.
        if let Some(sub) = self.intercept.take() {
            cx.defer(move |_| drop(sub));
        }
        cx.notify();
    }

    fn recorded(&mut self, k: &Keystroke, cx: &mut Context<Self>) {
        let Some((id, replace)) = self.recording else { return };
        if k.key == "escape" && !k.modifiers.modified() {
            self.stop_recording(cx);
            return;
        }
        // A lone modifier: wait for the key that goes with it.
        let Some(keys) = keymap::from_keystroke(k) else { return };
        self.stop_recording(cx);
        let Some(cmd) = keymap::command(id) else { return };
        app_settings::update(cx, |s| {
            let mut all = cmd.keys(s);
            match replace {
                Some(i) if i < all.len() => all[i] = keys.clone(),
                _ => all.push(keys.clone()),
            }
            let mut seen = std::collections::HashSet::new();
            all.retain(|k| seen.insert(k.to_lowercase()));
            keymap::set_keys(s, id, all);
        });
    }

    fn remove_shortcut(id: &'static str, ix: usize, cx: &mut App) {
        let Some(cmd) = keymap::command(id) else { return };
        app_settings::update(cx, |s| {
            let mut all = cmd.keys(s);
            if ix < all.len() {
                all.remove(ix);
            }
            keymap::set_keys(s, id, all);
        });
    }

    /// Take `keys` away from the commands that share it with `id`.
    fn remove_elsewhere(id: &'static str, keys: String, cx: &mut App) {
        let Some(cmd) = keymap::command(id) else { return };
        app_settings::update(cx, |s| {
            let bare = keys.split('@').next().unwrap_or(&keys).to_lowercase();
            for other in keymap::conflicts(cmd, &keys, s) {
                let rest: Vec<String> = other.keys(s).into_iter().filter(|k| k.split('@').next().unwrap_or(k).to_lowercase() != bare).collect();
                keymap::set_keys(s, other.id, rest);
            }
        });
    }

    /// Settings → Keymap: every command with its shortcuts, filtered by the search box.
    fn keymap_section(&self, s: &Settings, cx: &mut Context<Self>) -> AnyElement {
        let query = self.keymap_query.read(cx).text().trim().to_lowercase();
        let key_query = query.replace('-', "+");
        let matches = |cmd: &keymap::Command| {
            query.is_empty() || cmd.label.to_lowercase().contains(&query) || cmd.keys(s).iter().any(|k| keymap::label(k).to_lowercase().contains(&key_query))
        };
        let changed = keymap::COMMANDS.iter().filter(|c| c.is_changed(s)).count();
        let search = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .w(px(320.))
            .h(px(30.))
            .px(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(color(c::POPUP_BORDER))
            .bg(color(c::POPUP))
            .child(icon(Icon::Search, 13., color(c::TEXT_DIM)))
            .child(div().flex_1().child(self.keymap_query.clone()));
        let header = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .pb(px(4.))
            .child(search)
            .child(div().flex_1())
            .when(changed > 0, |d| {
                d.child(div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child(format!("{changed} changed")))
                    .child(small_button("keymap-reset-all", "Reset shortcuts").on_click(|_, _, cx| app_settings::update(cx, |s| s.keymap.clear())))
            });
        let mut body = div().flex().flex_col().child(header);
        let mut any = false;
        for group in keymap::GROUPS {
            let rows: Vec<(usize, &'static keymap::Command)> = keymap::COMMANDS.iter().enumerate().filter(|(_, c)| c.group == *group && matches(c)).collect();
            if rows.is_empty() {
                continue;
            }
            any = true;
            let mut group_card = card();
            for (n, (ix, cmd)) in rows.into_iter().enumerate() {
                group_card = group_card.child(self.keymap_row(n == 0, ix, cmd, s, cx));
            }
            body = body.child(div().pt(px(12.)).pb(px(6.)).text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child(*group)).child(group_card);
        }
        if !any {
            body = body.child(div().py(px(16.)).text_color(color(c::TEXT_DIM)).child("No command or shortcut matches."));
        }
        // As tall as a screen however few commands match, so the page doesn't scroll away from
        // the search box while typing.
        body.min_h(px(1000.)).into_any_element()
    }

    fn keymap_row(&self, first: bool, ix: usize, cmd: &'static keymap::Command, s: &Settings, cx: &mut Context<Self>) -> AnyElement {
        let id = cmd.id;
        let keys = cmd.keys(s);
        let recording = self.recording.filter(|(r, _)| *r == id).map(|(_, replace)| replace);
        let mut chips = div().flex().flex_wrap().items_center().justify_end().gap(px(6.));
        for (k, key) in keys.iter().enumerate() {
            if recording == Some(Some(k)) {
                chips = chips.child(recording_chip(("km-rec", ix), cx));
                continue;
            }
            let chip = div()
                .id(("km-key", ix * 16 + k))
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(26.))
                .pl(px(9.))
                .pr(px(4.))
                .rounded(px(6.))
                .border_1()
                .border_color(color(c::POPUP_BORDER))
                .bg(color(c::ISLAND))
                .cursor_pointer()
                .hover(|d| d.border_color(color(c::ACCENT)))
                .child(div().text_size(px(12.)).text_color(color(c::TEXT)).child(keymap::label(key)))
                .child(
                    div()
                        .id(("km-del", ix * 16 + k))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(16.))
                        .rounded(px(4.))
                        .hover(|d| d.bg(color(c::HOVER)))
                        .child(icon(Icon::Close, 10., color(c::TEXT_DIM)))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            Self::remove_shortcut(id, k, cx);
                        }),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.start_recording(id, Some(k), window, cx)));
            chips = chips.child(chip);
        }
        if recording == Some(None) {
            chips = chips.child(recording_chip(("km-rec", ix), cx));
        }
        let add = div()
            .id(("km-add", ix))
            .flex()
            .items_center()
            .justify_center()
            .size(px(26.))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|d| d.bg(color(c::HOVER)))
            .child(icon(Icon::Plus, 13., color(c::TEXT_MUTED)))
            .on_click(cx.listener(move |this, _, window, cx| this.start_recording(id, None, window, cx)));
        let changed = cmd.is_changed(s);
        let reset = div()
            .id(("km-reset", ix))
            .flex()
            .items_center()
            .justify_center()
            .size(px(26.))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|d| d.bg(color(c::HOVER)))
            .child(icon(Icon::Refresh, 13., color(c::TEXT_MUTED)))
            .on_click(move |_, _, cx| {
                if let Some(cmd) = keymap::command(id) {
                    app_settings::update(cx, |s| keymap::set_keys(s, id, cmd.default_keys()));
                }
            });
        // Shortcuts another command uses where this one works too.
        let clashes: Vec<(String, Vec<&'static str>)> = keys
            .iter()
            .filter_map(|k| {
                let others = keymap::conflicts(cmd, k, s);
                (!others.is_empty()).then(|| (k.clone(), others.iter().map(|o| o.label).collect()))
            })
            .collect();
        let mut label = div().flex().flex_col().flex_1().min_w(px(0.)).gap(px(2.)).child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().text_color(color(c::TEXT)).child(cmd.label))
                .when(changed, |d| d.child(div().text_size(px(11.)).text_color(color(c::ACCENT)).child("changed"))),
        );
        for (n, (keys, others)) in clashes.into_iter().enumerate() {
            let take = keys.clone();
            label = label.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(11.5))
                    .child(div().text_color(color(c::YELLOW)).child(format!("{} is also used by {}", keymap::label(&keys), others.join(", "))))
                    .child(
                        div()
                            .id(("km-take", ix * 16 + n))
                            .cursor_pointer()
                            .text_color(color(c::ACCENT))
                            .hover(|d| d.underline())
                            .child("Remove there")
                            .on_click(move |_, _, cx| Self::remove_elsewhere(id, take.clone(), cx)),
                    ),
            );
        }
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(16.))
            .py(px(8.))
            .min_h(px(44.))
            .when(!first, |d| d.border_t_1().border_color(color(c::BORDER)))
            .child(label)
            .child(chips)
            .child(add)
            .child(div().size(px(26.)).when(changed, |d| d.child(reset)))
            .into_any_element()
    }

    fn toggle_picker(&mut self, target: FontTarget, window: &mut Window, cx: &mut Context<Self>) {
        let just_closed = self.picker_closed.is_some_and(|(t, at)| t == target && at.elapsed() < Duration::from_millis(250));
        if self.picker.as_ref().is_some_and(|p| p.target == target) || just_closed {
            self.close_picker(window, cx);
            return;
        }
        let search = cx.new(|cx| TextInput::new("Search fonts…", cx));
        let sub = cx.subscribe_in(&search, window, |this, _, ev: &TextInputEvent, window, cx| match ev {
            TextInputEvent::Changed(text) => {
                let text = text.clone();
                if let Some(p) = this.picker.as_mut() {
                    p.matches = font_matches(&text);
                    p.selected = 0;
                    p.scroll.scroll_to_item_strict(0, gpui::ScrollStrategy::Top);
                }
                cx.notify();
            }
            TextInputEvent::Up | TextInputEvent::Down => {
                if let Some(p) = this.picker.as_mut().filter(|p| !p.matches.is_empty()) {
                    let n = p.matches.len();
                    p.selected = if matches!(ev, TextInputEvent::Up) { (p.selected + n - 1) % n } else { (p.selected + 1) % n };
                    p.scroll.scroll_to_item(p.selected, gpui::ScrollStrategy::Top);
                }
                cx.notify();
            }
            TextInputEvent::Submit => {
                let chosen = this.picker.as_ref().and_then(|p| p.matches.get(p.selected).copied());
                if let Some(family) = chosen {
                    this.choose(family, window, cx);
                }
            }
            TextInputEvent::Cancel => this.close_picker(window, cx),
        });
        let current = target.get(&app_settings::get(cx)).to_string();
        let matches = font_matches("");
        let selected = matches.iter().position(|f| f.eq_ignore_ascii_case(&current)).unwrap_or(0);
        let scroll = UniformListScrollHandle::new();
        scroll.scroll_to_item_strict(selected.saturating_sub(4), gpui::ScrollStrategy::Top);
        window.focus(&search.focus_handle(cx));
        self.picker = Some(FontPicker { target, search, matches, selected, scroll, _sub: sub });
        cx.notify();
    }

    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.take().is_some() {
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    fn choose(&mut self, family: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = self.picker.as_ref().map(|p| p.target) {
            app_settings::update(cx, |s| target.set(s, family.to_string()));
        }
        self.close_picker(window, cx);
    }

    /// The dropdown button for one font setting, with its list when open.
    fn font_dropdown(&self, target: FontTarget, settings: &Settings, cx: &mut Context<Self>) -> AnyElement {
        let current = target.get(settings).to_string();
        let missing = !theme::is_installed(&current);
        let open = self.picker.as_ref().is_some_and(|p| p.target == target);
        let button = div()
            .id(target.id())
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(30.))
            .min_w(px(180.))
            .px(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(color(if open { c::TAB_FOCUSED_BORDER } else { c::POPUP_BORDER }))
            .bg(color(c::ISLAND))
            .cursor_pointer()
            .hover(|s| s.bg(color(c::HOVER)))
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.5))
                    .text_color(color(if open { c::ACCENT } else { c::TEXT }))
                    .when(!missing, |d| d.font_family(SharedString::from(current.clone())))
                    .child(current.clone()),
            )
            .when(missing, |d| d.child(div().text_size(px(11.)).text_color(color(c::YELLOW)).child("not available")))
            .child(icon(Icon::ChevronsUpDown, 14., color(c::TEXT_MUTED)))
            .on_click(cx.listener(move |this, _, window, cx| this.toggle_picker(target, window, cx)));
        let popup = self.picker.as_ref().filter(|p| p.target == target).map(|p| {
            let matches = p.matches.clone();
            let selected = p.selected;
            let current = current.clone();
            let list_height = px((matches.len() as f32 * 30.).min(320.));
            let list = if matches.is_empty() {
                div().p(px(12.)).text_color(color(c::TEXT_DIM)).child("No installed font matches").into_any_element()
            } else {
                uniform_list("font-list", matches.len(), cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                    range
                        .map(|ix| {
                            let family = matches[ix];
                            let is_current = family.eq_ignore_ascii_case(&current);
                            div()
                                .id(ix)
                                .flex()
                                .items_center()
                                .w_full()
                                .h(px(30.))
                                .px(px(10.))
                                .rounded(px(6.))
                                .cursor_pointer()
                                .font_family(family)
                                .text_size(px(14.))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_color(color(c::TEXT))
                                .when(ix == selected, |d| d.bg(color_a(c::ACCENT, 0.32)))
                                .when(ix != selected, |d| d.hover(|s| s.bg(color(c::HOVER))))
                                .when(is_current && ix != selected, |d| d.text_color(color(c::ACCENT)))
                                .child(family)
                                .on_click(cx.listener(move |this, _, window, cx| this.choose(family, window, cx)))
                        })
                        .collect()
                }))
                .track_scroll(p.scroll.clone())
                .h(list_height)
                .into_any_element()
            };
            deferred(
                anchored().anchor(gpui::Corner::TopRight).snap_to_window_with_margin(px(8.)).child(
                    div()
                        .id("font-popup")
                        .occlude()
                        .mt(px(4.))
                        .w(px(300.))
                        .p(px(6.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .rounded(px(8.))
                        .bg(color(c::POPUP))
                        .border_1()
                        .border_color(color(c::POPUP_BORDER))
                        .shadow_lg()
                        .font_family(theme::fonts().ui)
                        .text_size(px(theme::UI_TEXT))
                        .on_mouse_down_out(cx.listener(move |this, _, window, cx| {
                            this.picker_closed = Some((target, Instant::now()));
                            this.close_picker(window, cx);
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .h(px(32.))
                                .px(px(8.))
                                .rounded(px(6.))
                                .border_1()
                                .border_color(color(c::TAB_FOCUSED_BORDER))
                                .bg(color(c::ISLAND))
                                .child(p.search.clone()),
                        )
                        .child(list),
                ),
            )
            .with_priority(1)
        });
        div()
            .relative()
            .child(button)
            .when_some(popup, |d, popup| d.child(div().absolute().top(px(30.)).right_0().child(popup)))
            .into_any_element()
    }
}

/// Installed fonts whose name contains `query` (any case).
fn font_matches(query: &str) -> Vec<&'static str> {
    let q = query.trim().to_lowercase();
    theme::installed_fonts().iter().filter(|f| q.is_empty() || f.to_lowercase().contains(&q)).map(String::as_str).collect()
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

/// The chip that waits for a shortcut; a click elsewhere cancels.
fn recording_chip(id: impl Into<gpui::ElementId>, cx: &mut Context<SettingsPage>) -> AnyElement {
    div()
        .id(id.into())
        .flex()
        .items_center()
        .h(px(26.))
        .px(px(9.))
        .rounded(px(6.))
        .border_1()
        .border_color(color(c::ACCENT))
        .bg(color_a(c::ACCENT, 0.14))
        .text_size(px(12.))
        .text_color(color(c::TEXT))
        .child("Press a shortcut… (Esc cancels)")
        .on_mouse_down_out(cx.listener(|this, _, _, cx| this.stop_recording(cx)))
        .into_any_element()
}

fn small_button(id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
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
        .child(label)
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
        let mut themes: Vec<String> = theme::BUILTIN_THEMES.iter().map(|t| t.to_string()).collect();
        themes.extend(theme::theme_list().into_iter().map(|(name, _)| name));
        let theme_chips = themes
            .into_iter()
            .enumerate()
            .map(|(ix, name)| {
                let selected = name == active_theme;
                let key = name.clone();
                chip(("settings-theme", ix), name, selected).on_click(move |_, _, cx| lua_bridge::choose_theme(key.clone(), cx)).into_any_element()
            })
            .collect();
        let appearance = card()
            .child(choice_row(true, "Theme", "Graphyne Dark and Graphyne Light are built in; plugins can add more (Plugins page).", theme_chips))
            .child(row(false, "Interface font", "Menus, panels and tabs.", self.font_dropdown(FontTarget::Interface, &s, cx)))
            .child(row(false, "Editor font", "Any font installed on this computer; monospaced ones suit code best.", self.font_dropdown(FontTarget::Editor, &s, cx)))
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
            .child(row(true, "Terminal font", "Monospaced fonts keep columns aligned.", self.font_dropdown(FontTarget::Terminal, &s, cx)))
            .child(row(
                false,
                "Terminal font size",
                "Applies to every terminal tab.",
                stepper("settings-term-size", size_label(s.terminal_font_size), true, |s, d| s.terminal_font_size += d * 0.5),
            ))
            .child(choice_row(false, "Shell", "Used for new terminal tabs. Automatic: PowerShell 7 if installed, else Windows PowerShell.", shell_chips));

        // Run & Debug
        let run_debug = card()
            .child(row(
                true,
                "Prepare Python projects",
                "Create .venv with debugpy when a Python file is added or run, so Run and Debug just work.",
                toggle("settings-python-env", s.python_auto_env, |s, v| s.python_auto_env = v),
            ))
            .child(row(
                false,
                "Use uv for Python packages",
                "Much faster than pip. Downloaded the first time (about 15 MB) if it isn't installed; projects that need pip still get pip.",
                toggle("settings-python-uv", s.python_uv, |s, v| s.python_uv = v),
            ));

        // Startup
        let startup = card().child(row(
            true,
            "Reopen projects",
            "Open the projects from the last session. Off: always start with the Welcome screen.",
            toggle("settings-reopen", s.reopen_projects, |s, v| s.reopen_projects = v),
        ));

        let keymap = self.keymap_section(&s, cx);

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
                    .child(section("RUN & DEBUG"))
                    .child(run_debug)
                    .child(section("STARTUP"))
                    .child(startup)
                    .child(section("KEYMAP"))
                    .child(keymap)
                    .child(div().pt(px(18.)).text_size(px(11.5)).text_color(color(c::TEXT_FAINT)).child(format!("Changes apply at once and are saved to {file}"))),
            )
    }
}
