//! The Plugins page (an editor-area tab, like JetBrains' Settings › Plugins): installed
//! extensions with what they add, enable / disable, permissions, the colour theme — and
//! installing one from a .zip or a folder, after a look at what it is and asks for.

use crate::assets::Icon;
use crate::components::{icon, tooltip};
use crate::lua_bridge;
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use crate::plugin_install::{self, Package};
use gpui::{App, Context, FocusHandle, Focusable, PathPromptOptions, SharedString, Window, div, px};
use graphyne_lua::PluginInfo;
use std::collections::BTreeSet;

/// An extension picked for installing, waiting for Install / Cancel.
struct Pending {
    pkg: Package,
    /// Permissions ticked in the confirmation (off unless the user ticks them).
    allow: BTreeSet<String>,
}

pub struct PluginsPage {
    focus_handle: FocusHandle,
    pending: Option<Pending>,
    busy: bool,
    /// Result of the last install (text, error?).
    status: Option<(SharedString, bool)>,
}

impl Focusable for PluginsPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl PluginsPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self { focus_handle: cx.focus_handle(), pending: None, busy: false, status: None }
    }

    /// "Install from file…" (a .zip) / "From folder…": pick it, then show what it is.
    fn pick(&mut self, folder: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let plugins_dir = lua_bridge::plugins_dir(cx);
        let picked = cx.prompt_for_paths(PathPromptOptions { files: !folder, directories: folder, multiple: false, prompt: Some("Install".into()), directory: None });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update(cx, |this, cx| {
                this.busy = true;
                this.status = Some(("Reading the extension…".into(), false));
                cx.notify();
            })
            .ok();
            let result = cx.background_spawn(async move { plugin_install::inspect(&path, &plugins_dir) }).await;
            this.update(cx, |this, cx| {
                this.busy = false;
                this.status = None;
                match result {
                    Ok(pkg) => {
                        // An update keeps what was allowed for the installed version.
                        let granted: BTreeSet<String> =
                            lua_bridge::snapshot(cx).plugins.into_iter().find(|p| p.id == pkg.id).map(|p| p.granted.into_iter().collect()).unwrap_or_default();
                        let allow = pkg.requested.iter().filter(|p| granted.contains(*p)).cloned().collect();
                        if let Some(old) = this.pending.take() {
                            plugin_install::discard(&old.pkg);
                        }
                        this.pending = Some(Pending { pkg, allow });
                    }
                    Err(e) => this.status = Some((format!("Can't install it: {e}").into(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.pending.take() {
            cx.background_spawn(async move { plugin_install::discard(&p.pkg) }).detach();
        }
        cx.notify();
    }

    /// Install the pending extension: copy it in, enable it, allow what was ticked.
    fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(Pending { pkg, allow }) = self.pending.take() else { return };
        let plugins_dir = lua_bridge::plugins_dir(cx);
        self.busy = true;
        self.status = Some((format!("Installing {}…", pkg.name).into(), false));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let job = pkg.clone();
            let result = cx.background_spawn(async move { plugin_install::install(&job, &plugins_dir) }).await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(_) => {
                        lua_bridge::set_enabled(&pkg.id, true, cx);
                        for perm in &pkg.requested {
                            lua_bridge::set_permission(&pkg.id, perm, allow.contains(perm), cx);
                        }
                        let what = if pkg.replaces.is_some() { "Updated" } else { "Installed" };
                        let version = pkg.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
                        this.status = Some((format!("{what} {}{version}.", pkg.name).into(), false));
                    }
                    Err(e) => this.status = Some((format!("Not installed: {e}").into(), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The confirmation: what the extension is, what it asks for, Install / Cancel.
    fn render_pending(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let Pending { pkg, allow } = self.pending.as_ref()?;
        let verb = if pkg.replaces.is_some() { "Update" } else { "Install" };
        let version = match (&pkg.replaces, &pkg.version) {
            (Some(Some(old)), Some(new)) if old != new => format!("{old} → {new}"),
            (_, Some(v)) => v.clone(),
            _ => String::new(),
        };
        let place = match &pkg.replaces {
            Some(_) => format!("Replaces the installed plugins\\{} (the old one goes to the Recycle Bin); what you allowed it stays.", pkg.id),
            None => format!("Goes into the plugins folder as {}, enabled.", pkg.id),
        };
        let perms = pkg.requested.iter().enumerate().map(|(ix, perm)| {
            let on = allow.contains(perm);
            let name = perm.clone();
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .py(px(2.))
                .child(
                    div()
                        .id(("install-perm", ix))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(14.))
                        .rounded(px(3.))
                        .border_1()
                        .cursor_pointer()
                        .when(on, |d| d.bg(color(c::ACCENT)).border_color(color(c::ACCENT)).child(icon(Icon::Check, 10., color(0xFFFFFF))))
                        .when(!on, |d| d.border_color(color(c::TEXT_FAINT)).hover(|s| s.border_color(color(c::TEXT_MUTED))))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(p) = this.pending.as_mut() {
                                if !p.allow.remove(&name) {
                                    p.allow.insert(name.clone());
                                }
                            }
                            cx.notify();
                        })),
                )
                .child(div().text_color(color(c::TEXT)).child(perm.clone()))
                .child(div().text_color(color(c::TEXT_DIM)).child(permission_help(perm)))
        });
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .mt(px(14.))
                .p(px(16.))
                .rounded(px(8.))
                .border_1()
                .border_color(color(c::ACCENT))
                .bg(color_a(c::ACCENT, 0.06))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(icon(Icon::Puzzle, 16., color(c::ACCENT)))
                        .child(div().text_size(px(theme::UI_TEXT + 1.)).text_color(color(c::TEXT)).font_weight(gpui::FontWeight::MEDIUM).child(format!("{verb} {}?", pkg.name)))
                        .when(!version.is_empty(), |d| d.child(div().text_color(color(c::TEXT_DIM)).child(version)))
                        .when_some(pkg.author.clone(), |d, a| d.child(div().text_color(color(c::TEXT_DIM)).child(format!("· {a}")))),
                )
                .when_some(pkg.description.clone(), |d, desc| d.child(div().text_color(color(c::TEXT_MUTED)).child(desc)))
                .child(div().text_size(px(theme::UI_SMALL)).text_color(color(c::TEXT_DIM)).child(place))
                .child(div().pt(px(4.)).text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child(if pkg.requested.is_empty() {
                    "IT ASKS FOR NO PERMISSIONS — it runs inside Graphyne's Lua sandbox"
                } else {
                    "IT ASKS FOR — off unless you tick them (you can change them later)"
                }))
                .children(perms)
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .pt(px(6.))
                        .child(button("install-confirm", verb, true).on_click(cx.listener(|this, _, _, cx| this.confirm(cx))))
                        .child(button("install-cancel", "Cancel", false).on_click(cx.listener(|this, _, _, cx| this.cancel(cx)))),
                ),
        )
    }
}

/// What each permission unlocks, shown next to its toggle.
fn permission_help(p: &str) -> &'static str {
    match p {
        "os" => "run programs and delete / rename files (os.*)",
        "io" => "run shell commands through io.popen",
        "ffi" => "call C code through the LuaJIT FFI",
        "native" => "load its grammar library — native machine code, outside the Lua sandbox",
        _ => "unknown permission",
    }
}

fn button(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, primary: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id.into())
        .flex()
        .items_center()
        .h(px(26.))
        .px(px(10.))
        .rounded(px(6.))
        .border_1()
        .cursor_pointer()
        .text_size(px(theme::UI_SMALL))
        .when(primary, |d| d.bg(color(c::ACCENT)).border_color(color(c::ACCENT)).text_color(color(0xFFFFFF)))
        .when(!primary, |d| d.border_color(color(c::POPUP_BORDER)).text_color(color(c::TEXT)).hover(|s| s.bg(color(c::HOVER))))
        .child(label.into())
}

fn section(label: &'static str) -> gpui::Div {
    div().pt(px(18.)).pb(px(8.)).text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child(label)
}

impl PluginsPage {
    fn plugin_card(&self, ix: usize, p: PluginInfo, notes: Vec<String>, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let id = p.id.clone();
        let enabled = p.enabled;
        let (status, status_color) = match (&p.error, p.enabled, p.loaded) {
            (Some(e), _, _) => (format!("Error: {e}"), c::RED),
            (None, false, _) => ("Disabled".to_string(), c::TEXT_DIM),
            (None, true, true) => ("Active".to_string(), c::GREEN),
            (None, true, false) => ("Not loaded".to_string(), c::TEXT_DIM),
        };
        let title = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(icon(Icon::Puzzle, 16., color(if enabled { c::ACCENT } else { c::TEXT_DIM })))
            .child(div().text_size(px(theme::UI_TEXT + 1.)).text_color(color(c::TEXT)).font_weight(gpui::FontWeight::MEDIUM).child(p.name.clone()))
            .when_some(p.version.clone(), |d, v| d.child(div().text_color(color(c::TEXT_DIM)).child(v)))
            .when_some(p.author.clone(), |d, a| d.child(div().text_color(color(c::TEXT_DIM)).child(format!("· {a}"))))
            .child(div().flex_1())
            .child({
                let id = id.clone();
                button(("plugin-toggle", ix), if enabled { "Disable" } else { "Enable" }, !enabled)
                    .on_click(cx.listener(move |_, _, _, cx| lua_bridge::set_enabled(&id, !enabled, cx)))
            });
        let permissions = p.requested.iter().enumerate().map(|(pi, perm)| {
            let granted = p.granted.contains(perm);
            let (id, perm_name) = (id.clone(), perm.clone());
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .py(px(2.))
                .child(
                    div()
                        .id(("plugin-perm", ix * 16 + pi))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(14.))
                        .rounded(px(3.))
                        .border_1()
                        .cursor_pointer()
                        .when(granted, |d| d.bg(color(c::ACCENT)).border_color(color(c::ACCENT)).child(icon(Icon::Check, 10., color(0xFFFFFF))))
                        .when(!granted, |d| d.border_color(color(c::TEXT_FAINT)).hover(|s| s.border_color(color(c::TEXT_MUTED))))
                        .tooltip(tooltip(if granted { "Allowed — click to revoke" } else { "Not allowed — click to allow" }, None))
                        .on_click(cx.listener(move |_, _, _, cx| lua_bridge::set_permission(&id, &perm_name, !granted, cx))),
                )
                .child(div().text_color(color(c::TEXT)).child(perm.clone()))
                .child(div().text_color(color(c::TEXT_DIM)).child(permission_help(perm)))
        });
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(14.))
            .mb(px(10.))
            .rounded(px(8.))
            .border_1()
            .border_color(color(c::BORDER))
            .bg(color(c::POPUP))
            .when(!enabled, |d| d.opacity(0.75))
            .child(title)
            .when_some(p.description.clone(), |d, desc| d.child(div().text_color(color(c::TEXT_MUTED)).child(desc)))
            .when(!p.provides.is_empty(), |d| {
                d.child(div().flex().flex_wrap().gap(px(6.)).children(p.provides.iter().map(|what| {
                    div().px(px(7.)).py(px(1.)).rounded(px(4.)).bg(color_a(c::ACCENT, 0.14)).text_size(px(11.5)).text_color(color(c::GIT_MODIFIED)).child(what.clone())
                })))
            })
            .when(!p.requested.is_empty(), |d| d.child(div().pt(px(4.)).text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child("PERMISSIONS")).children(permissions))
            .children(notes.into_iter().map(|n| div().flex().gap(px(6.)).text_color(color(c::YELLOW)).child(icon(Icon::Warning, 13., color(c::YELLOW))).child(n)))
            .child(div().text_size(px(11.5)).text_color(color(status_color)).child(status))
            .child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child(p.folder.display().to_string()))
    }
}

impl Render for PluginsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = lua_bridge::snapshot(cx);
        let active_theme = theme::active_theme();
        let mut themes: Vec<(String, String, String)> = theme::BUILTIN_THEMES.iter().map(|t| (t.to_string(), t.to_string(), "built-in".to_string())).collect();
        themes.extend(theme::theme_list().into_iter().map(|(name, plugin)| (name.clone(), name, plugin.map(|p| format!("from {p}")).unwrap_or_else(|| "from init.lua".into()))));
        let plugins_dir = snapshot.plugins_dir.display().to_string();
        let count = snapshot.plugins.len();
        let header = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(icon(Icon::Puzzle, 22., color(c::ACCENT)))
            .child(div().text_size(px(20.)).text_color(color(c::TEXT)).child("Plugins"))
            // Buttons sit right after the title; the folder itself opens in Explorer.
            .child(div().w(px(8.)))
            .child(button("plugins-open-folder", "Open folder", false).on_click(cx.listener(|_, _, _, cx| {
                let dir = lua_bridge::plugins_dir(cx);
                cx.reveal_path(&dir);
            })))
            .child(button("plugins-reload", "Reload", false).on_click(cx.listener(|_, _, _, cx| lua_bridge::send(graphyne_lua::ToLua::Reload, cx))))
            .child(div().flex_1())
            .child(
                button("plugins-install-file", "Install from file…", true)
                    .tooltip(tooltip("Install an extension from a .zip", None))
                    .on_click(cx.listener(|this, _, _, cx| this.pick(false, cx))),
            )
            .child(
                button("plugins-install-folder", "From folder…", false)
                    .tooltip(tooltip("Install an extension from a folder (it is copied)", None))
                    .on_click(cx.listener(|this, _, _, cx| this.pick(true, cx))),
            );
        let status = self.status.clone().map(|(text, error)| {
            div()
                .mt(px(10.))
                .flex()
                .items_center()
                .gap(px(6.))
                .text_color(color(if error { c::RED } else { c::TEXT_MUTED }))
                .when(error, |d| d.child(icon(Icon::Error, 13., color(c::RED))))
                .child(text)
        });
        let pending = self.render_pending(cx);
        let theme_row = div().flex().flex_wrap().gap(px(8.)).children(themes.into_iter().enumerate().map(|(ix, (key, name, origin))| {
            let selected = key == active_theme;
            div()
                .id(("theme", ix))
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(8.))
                .border_1()
                .cursor_pointer()
                .border_color(color(if selected { c::ACCENT } else { c::BORDER }))
                .when(selected, |d| d.bg(color_a(c::ACCENT, 0.12)))
                .when(!selected, |d| d.hover(|s| s.bg(color(c::HOVER))))
                .child(icon(Icon::Palette, 15., color(if selected { c::ACCENT } else { c::TEXT_MUTED })))
                .child(div().flex().flex_col().child(div().text_color(color(c::TEXT)).child(name)).child(div().text_size(px(11.)).text_color(color(c::TEXT_DIM)).child(origin)))
                .on_click(cx.listener(move |_, _, _, cx| lua_bridge::choose_theme(key.clone(), cx)))
        }));
        let cards: Vec<_> = snapshot
            .plugins
            .into_iter()
            .enumerate()
            .map(|(ix, p)| {
                let notes = snapshot.notes.get(&p.id).cloned().unwrap_or_default();
                self.plugin_card(ix, p, notes, cx).into_any_element()
            })
            .collect();
        let init = snapshot.init_file.display().to_string();
        div()
            .id("plugins-page")
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
                    .child(header)
                    .children(status)
                    .children(pending)
                    .child(section("THEME"))
                    .child(theme_row)
                    .child(section("INSTALLED"))
                    .when(count == 0, |d| {
                        d.child(
                            div()
                                .p(px(18.))
                                .rounded(px(8.))
                                .border_1()
                                .border_color(color(c::BORDER))
                                .text_color(color(c::TEXT_DIM))
                                .child(format!("No plugins yet. Install one from a .zip or a folder (buttons above), or put a plugin folder (with init.lua and plugin.json) into {plugins_dir} — it is picked up immediately.")),
                        )
                    })
                    .children(cards)
                    .child(section("YOUR SETTINGS"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(div().flex_1().min_w(px(0.)).whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(color(c::TEXT_MUTED)).child(format!("init.lua: {init}")))
                            .child(button("plugins-open-init", "Open init.lua", false).on_click(cx.listener(|_, _, _, cx| lua_bridge::request_open_init(cx)))),
                    ),
            )
    }
}
