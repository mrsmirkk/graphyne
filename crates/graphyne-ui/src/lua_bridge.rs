//! Glue between the LuaJIT plugin engine (`graphyne-lua`) and the windows: one VM for the
//! whole app, its key bindings, plugin tabs and settings live in a gpui `Global`; events
//! that need a window go to the most recently active workspace.

use crate::workspace::Workspace;
use gpui::{AnyWindowHandle, App, Global, KeyBinding, Keystroke, SharedString, WeakEntity};
use graphyne_lua::{FromLua, KeyContext, LuaDirs, LuaHost, PluginInfo, PluginState, Side, TabItem, ToLua};
use std::collections::HashMap;
use std::path::PathBuf;

/// Key binding registered by `graphyne.keymap.set`.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = graphyne_lua, no_json)]
pub struct LuaKey {
    pub id: u64,
}

#[derive(Clone)]
pub struct PluginTab {
    pub id: u64,
    pub title: SharedString,
    pub side: Side,
    pub items: Vec<TabItem>,
}

pub struct LuaState {
    pub host: LuaHost,
    keys: Vec<(u64, String, KeyContext)>,
    pub tabs: Vec<PluginTab>,
    workspaces: Vec<(WeakEntity<Workspace>, AnyWindowHandle)>,
    active: Option<(WeakEntity<Workspace>, AnyWindowHandle)>,
    loads: usize,
    /// Extensions found by the last load (Plugins page).
    plugins: Vec<PluginInfo>,
    /// plugins.json: disabled, permissions, theme.
    state: PluginState,
    /// Per-extension warnings (grammar problems…) shown on the Plugins page.
    notes: HashMap<String, Vec<String>>,
    /// Grammar library already loaded per language (Windows keeps DLLs locked).
    grammars: HashMap<u16, PathBuf>,
}

/// What the Plugins page shows.
pub struct Snapshot {
    pub plugins: Vec<PluginInfo>,
    pub notes: HashMap<String, Vec<String>>,
    pub plugins_dir: PathBuf,
    pub init_file: PathBuf,
}

pub fn snapshot(cx: &App) -> Snapshot {
    let st = cx.global::<LuaState>();
    Snapshot { plugins: st.plugins.clone(), notes: st.notes.clone(), plugins_dir: st.host.plugins_dir(), init_file: st.host.init_file() }
}

fn save_state(cx: &mut App) {
    let st = cx.global::<LuaState>();
    if let Err(e) = st.state.save(&st.host.dirs().config) {
        notify(format!("Could not save plugins.json: {e}"), true, cx);
    }
}

pub fn set_enabled(id: &str, enabled: bool, cx: &mut App) {
    let st = cx.global_mut::<LuaState>();
    if enabled {
        st.state.disabled.remove(id);
    } else {
        st.state.disabled.insert(id.to_string());
    }
    save_state(cx);
    send(ToLua::Reload, cx);
}

pub fn set_permission(id: &str, perm: &str, granted: bool, cx: &mut App) {
    let st = cx.global_mut::<LuaState>();
    let perms = st.state.permissions.entry(id.to_string()).or_default();
    if granted {
        perms.insert(perm.to_string());
    } else {
        perms.remove(perm);
    }
    if perms.is_empty() {
        st.state.permissions.remove(id);
    }
    save_state(cx);
    send(ToLua::Reload, cx);
}

/// Pick the colour theme (`None` = built-in) and remember it.
/// Use theme `name` (built in or from a plugin); remembered in settings.json.
pub fn choose_theme(name: String, cx: &mut App) {
    crate::app_settings::update(cx, |s| s.theme = name);
}

pub fn request_open_init(cx: &mut App) {
    with_active(cx, |ws, window, cx| match ensure_init_file(cx) {
        Some(file) => ws.open_file(file, None, window, cx),
        None => ws.notify_user("Could not create init.lua", true, cx),
    });
}

/// Re-detect every open editor's language (an extension may have added one).
fn redetect_languages(cx: &mut App) {
    let targets = cx.global::<LuaState>().workspaces.clone();
    for (ws, window) in targets {
        let _ = window.update(cx, |_, _, cx| {
            if let Some(ws) = ws.upgrade() {
                ws.update(cx, |ws, cx| ws.redetect_languages(cx));
            }
        });
    }
}

impl Global for LuaState {}

/// Start the VM (its own thread; nothing here blocks start-up) and pump its events.
pub fn init(cx: &mut App) {
    let dirs = LuaDirs::default_dirs();
    let state = PluginState::load(&dirs.config);
    let host = LuaHost::start(dirs);
    let events = host.events();
    cx.set_global(LuaState {
        host,
        keys: Vec::new(),
        tabs: Vec::new(),
        workspaces: Vec::new(),
        active: None,
        loads: 0,
        plugins: Vec::new(),
        state,
        notes: HashMap::new(),
        grammars: HashMap::new(),
    });
    // The theme used to be remembered in plugins.json; it now lives in settings.json.
    if let Some(theme) = cx.global_mut::<LuaState>().state.theme.take() {
        if crate::app_settings::get(cx).theme == graphyne_core::settings::DEFAULT_THEME {
            crate::app_settings::update(cx, |s| s.theme = theme);
        }
        save_state(cx);
    }
    cx.spawn(async move |cx| {
        while let Ok(ev) = events.recv_async().await {
            if cx.update(|cx| handle(ev, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
}

pub fn register(ws: WeakEntity<Workspace>, window: AnyWindowHandle, cx: &mut App) {
    let st = cx.global_mut::<LuaState>();
    st.workspaces.retain(|(w, _)| w.upgrade().is_some());
    st.workspaces.push((ws.clone(), window));
    st.active = Some((ws, window));
}

pub fn set_active(ws: WeakEntity<Workspace>, window: AnyWindowHandle, cx: &mut App) {
    if cx.has_global::<LuaState>() {
        cx.global_mut::<LuaState>().active = Some((ws, window));
    }
}

pub fn send(msg: ToLua, cx: &App) {
    if cx.has_global::<LuaState>() {
        cx.global::<LuaState>().host.send(msg);
    }
}

pub fn tabs(cx: &App) -> Vec<PluginTab> {
    if cx.has_global::<LuaState>() { cx.global::<LuaState>().tabs.clone() } else { Vec::new() }
}

/// Run `f` on the active workspace (falling back to any open one).
fn with_active(cx: &mut App, f: impl FnOnce(&mut Workspace, &mut gpui::Window, &mut gpui::Context<Workspace>) + 'static) {
    let st = cx.global::<LuaState>();
    let target = st
        .active
        .clone()
        .filter(|(w, _)| w.upgrade().is_some())
        .or_else(|| st.workspaces.iter().find(|(w, _)| w.upgrade().is_some()).cloned());
    let Some((ws, window)) = target else { return };
    let _ = window.update(cx, |_, window, cx| {
        if let Some(ws) = ws.upgrade() {
            ws.update(cx, |ws, cx| f(ws, window, cx));
        }
    });
}

fn notify(message: String, error: bool, cx: &mut App) {
    with_active(cx, move |ws, _, cx| ws.notify_user(message, error, cx));
}

/// Built-in bindings plus every `graphyne.keymap.set` binding (re-run on each change).
pub(crate) fn rebind(cx: &mut App) {
    cx.clear_key_bindings();
    crate::actions::bind_keys(cx);
    let keys = cx.global::<LuaState>().keys.clone();
    let mut bindings = Vec::new();
    let mut invalid = Vec::new();
    for (id, keys, context) in keys {
        let spec = crate::actions::m(&keys);
        if spec.split_whitespace().all(|k| Keystroke::parse(k).is_ok()) && !spec.trim().is_empty() {
            let context = match context {
                KeyContext::Editor => "Editor",
                KeyContext::Global => "Workspace",
            };
            bindings.push(KeyBinding::new(&spec, LuaKey { id }, Some(context)));
        } else {
            invalid.push(keys);
        }
    }
    cx.bind_keys(bindings);
    if !invalid.is_empty() {
        notify(format!("Lua: invalid key binding(s): {} (use e.g. \"ctrl-alt-k\")", invalid.join(", ")), true, cx);
    }
}

fn handle(ev: FromLua, cx: &mut App) {
    match ev {
        FromLua::Reset => {
            let st = cx.global_mut::<LuaState>();
            st.keys.clear();
            st.tabs.clear();
            st.notes.clear();
            crate::theme::clear_themes();
            rebind(cx);
            cx.refresh_windows();
        }
        FromLua::Loaded { files, errors } => {
            let st = cx.global_mut::<LuaState>();
            st.loads += 1;
            // Start-up is silent; a hot reload confirms what was loaded.
            if st.loads > 1 && errors.is_empty() {
                let what = if files.is_empty() { "nothing to load".to_string() } else { files.join(", ") };
                notify(format!("Lua reloaded: {what}"), false, cx);
            }
        }
        FromLua::Notify { message, error } => notify(message, error, cx),
        FromLua::Keymap { id, keys, context } => {
            cx.global_mut::<LuaState>().keys.push((id, keys, context));
            rebind(cx);
        }
        FromLua::RegisterTab { id, title, side } => {
            cx.global_mut::<LuaState>().tabs.push(PluginTab { id, title: title.into(), side, items: Vec::new() });
            send(ToLua::TabRefresh { tab: id }, cx);
            cx.refresh_windows();
        }
        FromLua::TabItems { tab, items } => {
            if let Some(t) = cx.global_mut::<LuaState>().tabs.iter_mut().find(|t| t.id == tab) {
                t.items = items;
            }
            cx.refresh_windows();
        }
        FromLua::Terminal { command, tab } => with_active(cx, move |ws, window, cx| ws.run_lua_command(&command, tab.as_deref(), window, cx)),
        FromLua::Edit(edit) => with_active(cx, move |ws, _, cx| ws.apply_lua_edit(edit, cx)),
        FromLua::OpenFile { path, line } => with_active(cx, move |ws, window, cx| ws.open_from_lua(&path, line, window, cx)),
        FromLua::Setting { key, value } => {
            if crate::theme::apply_setting(&key, &value) {
                cx.refresh_windows();
            }
        }
        FromLua::Plugins(list) => {
            let st = cx.global_mut::<LuaState>();
            st.plugins = list;
            // The chosen theme's extension is gone or disabled: Graphyne Dark until it is back.
            crate::theme::activate_theme(&crate::app_settings::get(cx).theme);
            cx.refresh_windows();
        }
        FromLua::Theme(spec) => {
            let name = spec.name.clone();
            crate::theme::register_theme(spec);
            if crate::app_settings::get(cx).theme == name {
                crate::theme::activate_theme(&name);
            }
            cx.refresh_windows();
        }
        FromLua::Language(spec) => {
            let id = graphyne_core::language::register_plugin_language(&spec.name, &spec.extensions, &spec.file_names, spec.line_comment.as_deref(), spec.run.as_deref());
            if let Some((command, args)) = &spec.lsp {
                graphyne_lsp::servers::register_plugin_server(id, command, args);
            }
            let mut note = spec.grammar_error.clone();
            if let (Some(g), graphyne_core::LanguageId::Plugin(ix)) = (&spec.grammar, id) {
                let already = cx.global::<LuaState>().grammars.get(&ix) == Some(&g.library);
                if !already {
                    match graphyne_syntax::load_plugin_grammar(id, &g.library, &g.symbol, &g.highlights) {
                        Ok(warning) => {
                            cx.global_mut::<LuaState>().grammars.insert(ix, g.library.clone());
                            note = warning;
                        }
                        Err(e) => note = Some(format!("grammar: {e}")),
                    }
                }
            }
            if let (Some(n), Some(plugin)) = (note, spec.plugin.clone()) {
                cx.global_mut::<LuaState>().notes.entry(plugin).or_default().push(format!("{}: {n}", spec.name));
            }
            redetect_languages(cx);
            cx.refresh_windows();
        }
    }
}

/// Starter init.lua (written only when the user picks "Open init.lua" and none exists).
pub const INIT_TEMPLATE: &str = r#"-- Graphyne init.lua — runs at start-up and again every time you save it (hot reload).
-- API: graphyne.keymap / buffer / terminal / ui / settings / workspace / plugins.

-- Settings
-- graphyne.settings.set("editor.font_size", 14)
-- graphyne.settings.set("editor.autosave_delay_ms", 800)
-- graphyne.settings.set("terminal.font_size", 13)

-- Insert today's date at the cursor
-- graphyne.keymap.set("editor", "ctrl-alt-d", function()
--   graphyne.buffer.insert(os.date("%Y-%m-%d"))
-- end)

-- Run a command in its own terminal tab, from anywhere in the window
-- graphyne.keymap.set("global", "ctrl-alt-g", function()
--   graphyne.terminal.run("git status", { tab = "git status" })
-- end)

-- A tab in the left panel
-- graphyne.ui.register_tab({ title = "Notes", side = "left", items = function()
--   return { "first item", { text = "open README", on_click = function() graphyne.workspace.open("README.md") end } }
-- end })

-- Extensions live in the plugins folder next to graphyne.exe; manage them on the
-- Plugins page ("+" in the left panel, or the main menu). They run sandboxed.
"#;

/// "Open init.lua": create the config folder and a starter file if needed, then watch it.
pub fn ensure_init_file(cx: &mut App) -> Option<std::path::PathBuf> {
    let st = cx.global_mut::<LuaState>();
    let file = st.host.init_file();
    if !file.exists() {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).ok()?;
        }
        std::fs::write(&file, INIT_TEMPLATE).ok()?;
    }
    st.host.watch();
    Some(file)
}

pub fn plugins_dir(cx: &mut App) -> std::path::PathBuf {
    let st = cx.global_mut::<LuaState>();
    let dir = st.host.plugins_dir();
    let _ = std::fs::create_dir_all(&dir);
    st.host.watch();
    dir
}
