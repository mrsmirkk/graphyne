//! Graphyne GPUI front-end.

mod actions;
mod app_settings;
mod assets;
mod components;
mod database;
mod debug_panel;
mod debugger;
mod diff_view;
mod editor;
mod editor_pane;
mod file_icons;
mod file_icons_data;
mod file_tree;
mod hex_view;
mod keymap;
mod lsp_store;
mod lua_bridge;
mod panels;
mod plugin_install;
mod plugins_page;
mod search_everywhere;
mod settings_page;
mod terminal_view;
mod text_input;
mod theme;
mod welcome;
mod window_fit;
mod workspace;

use gpui::{App, AppContext, Application, Bounds, PathPromptOptions, TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions, point, px, size};
use graphyne_core::session::{self, ProjectState, Session};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Project and Welcome windows; the app quits when the last one closes.
static APP_WINDOWS: AtomicUsize = AtomicUsize::new(0);
/// Project folders open right now, in the order they were opened: the session that the
/// next launch reopens.
static OPEN_PROJECTS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Boot the application. The first CLI argument is the project folder. Without one the
/// projects that were open at the last exit come back; with none, the Welcome window.
pub fn run() {
    // ssh started Graphyne to answer a password / passphrase prompt (an SSH tunnel to a
    // database): answer and leave.
    if graphyne_db::ssh::answer_askpass() {
        return;
    }
    let started = std::time::Instant::now();
    let trace = std::env::var_os("GRAPHYNE_STARTUP_TRACE").is_some();
    let mark = move |stage: &str| {
        if trace {
            eprintln!("graphyne: {stage:<28} {:>7.1} ms", started.elapsed().as_secs_f64() * 1000.0);
        }
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let root_arg = std::env::args().nth(1).map(PathBuf::from);

    let app = Application::new().with_assets(assets::Assets);
    mark("platform init");
    app.run(move |cx: &mut App| {
        mark("run callback");
        theme::init_fonts(&cx.text_system().all_font_names());
        mark("font enumeration");
        app_settings::init(cx);
        actions::bind_keys(cx);
        // LuaJIT plugins: the VM loads init.lua on its own thread.
        lua_bridge::init(cx);
        cx.on_action(|_: &actions::Quit, cx| quit_app(cx));
        cx.observe_keystrokes(|ev, _, _| {
            log::trace!(
                "keystroke {:?} char {:?} mods {:?} -> action {:?} ctx {:?}",
                ev.keystroke.key,
                ev.keystroke.key_char,
                ev.keystroke.modifiers,
                ev.action.as_ref().map(|a| a.name()),
                ev.context_stack.iter().map(|c| format!("{c:?}")).collect::<Vec<_>>()
            );
        })
        .detach();
        mark("keymap");

        match root_arg.clone() {
            Some(root) => open_workspace(root, mark, cx),
            None => {
                let reopen = app_settings::get(cx).reopen_projects;
                let last: Vec<PathBuf> = if reopen { session::load_session().open.into_iter().filter(|p| p.is_dir()).collect() } else { Vec::new() };
                mark("session loaded");
                if last.is_empty() {
                    welcome::open_welcome(cx);
                }
                for root in last {
                    open_workspace(root, mark, cx);
                }
            }
        }
    });
}

/// Every open editor in every project window (for settings that change how they look).
pub(crate) fn all_editors(cx: &App) -> Vec<gpui::Entity<editor::Editor>> {
    cx.windows()
        .into_iter()
        .filter_map(|w| w.downcast::<workspace::Workspace>())
        .filter_map(|w| w.read(cx).ok().map(|ws| ws.editors(cx)))
        .flatten()
        .collect()
}

pub(crate) fn window_opened() {
    APP_WINDOWS.fetch_add(1, Ordering::SeqCst);
}

/// A project or Welcome window went away; the app quits with the last one.
pub(crate) fn window_closed(cx: &mut App) {
    if APP_WINDOWS.fetch_sub(1, Ordering::SeqCst) <= 1 {
        cx.quit();
    }
}

fn write_session() {
    let open = OPEN_PROJECTS.lock().map(|l| l.clone()).unwrap_or_default();
    if let Err(e) = session::save_session(&Session { open }) {
        log::warn!("session: {e:#}");
    }
}

fn save_project_now(state: &ProjectState) {
    if let Err(e) = session::save_project(state) {
        log::warn!("session: {e:#}");
    }
}

/// `root` stops being part of the session (closed while other windows stay open).
fn forget_open(root: &Path) {
    if let Ok(mut open) = OPEN_PROJECTS.lock() {
        open.retain(|p| !session_same(p, root));
    }
    write_session();
}

/// Close Project: keep its layout for next time, drop it from the session, and show the
/// Welcome screen when it was the only project window.
pub(crate) fn project_closed(state: ProjectState, cx: &mut App) {
    save_project_now(&state);
    forget_open(&state.root);
    let projects = cx.windows().into_iter().filter(|w| w.downcast::<workspace::Workspace>().is_some()).count();
    if projects <= 1 {
        welcome::open_welcome(cx);
    }
}

/// Quit (Ctrl+Q / Exit): remember every open project's layout, then quit. The session
/// keeps all of them, so the next launch reopens them all.
pub(crate) fn quit_app(cx: &mut App) {
    // Deferred so that no window is mid-update and every one can be read.
    cx.defer(|cx| {
        for handle in cx.windows() {
            if let Some(ws) = handle.downcast::<workspace::Workspace>() {
                ws.update(cx, |ws, window, cx| save_project_now(&ws.project_state(window, cx))).ok();
            }
        }
        write_session();
        cx.quit();
    });
}

/// Where a project window goes: its saved place if that is still on a connected display —
/// shrunk and moved to fit that screen (it may have been saved on a bigger one). Returns the
/// display too: GPUI converts the bounds with that display's scale factor.
fn saved_bounds(state: Option<&ProjectState>, cx: &App) -> Option<(WindowBounds, gpui::DisplayId)> {
    let w = state?.window?;
    if w.width < 400. || w.height < 300. {
        return None;
    }
    let bounds = Bounds::new(point(px(w.x), px(w.y)), size(px(w.width), px(w.height)));
    let displays = cx.displays();
    let primary = cx.primary_display().and_then(|p| displays.iter().position(|d| d.id() == p.id()));
    let ix = window_fit::pick_display(&displays.iter().map(|d| d.bounds()).collect::<Vec<_>>(), primary, bounds)?;
    let bounds = window_fit::fit_saved(displays[ix].bounds(), bounds);
    Some((if w.maximized { WindowBounds::Maximized(bounds) } else { WindowBounds::Windowed(bounds) }, displays[ix].id()))
}

/// Open `root` in a project window with its layout from last time (also used by Open
/// Folder and the recent-project lists). A project that is already open is brought forward.
pub(crate) fn open_workspace(root: PathBuf, mark: impl Fn(&str) + Copy + 'static, cx: &mut App) {
    let root = std::fs::canonicalize(&root).map(strip_verbatim).unwrap_or(root);
    let already = cx.windows().into_iter().filter_map(|w| w.downcast::<workspace::Workspace>()).find(|w| w.read(cx).is_ok_and(|ws| session_same(ws.root(), &root)));
    if let Some(window) = already {
        window.update(cx, |_, window, _| window.activate_window()).ok();
        welcome::close_welcome(cx);
        return;
    }
    let root_for_recent = root.clone();
    let state = session::load_project(&root);
    let (bounds, display_id) = match saved_bounds(state.as_ref(), cx) {
        Some((bounds, display)) => (bounds, Some(display)),
        None => (window_fit::new_window(window_fit::PROJECT, cx), None),
    };
    let options = WindowOptions {
        window_bounds: Some(bounds),
        display_id,
        titlebar: Some(TitlebarOptions {
            title: Some(format!("{} \u{2014} Graphyne", graphyne_core::project::project_name(&root)).into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(13.))),
        }),
        window_decorations: Some(WindowDecorations::Client),
        window_min_size: Some(size(px(760.), px(480.))),
        app_id: Some("graphyne".into()),
        ..Default::default()
    };
    let window = cx.open_window(options, |window, cx| {
        mark("window created");
        window_opened();
        // The window's ×: remember its layout. Closing the last window quits, and the
        // project stays in the session; closing one of several drops it from the session.
        window.on_window_should_close(cx, |window, cx| {
            if let Some(Some(ws)) = window.root::<workspace::Workspace>() {
                let state = ws.read(cx).project_state(window, cx);
                save_project_now(&state);
                if APP_WINDOWS.load(Ordering::SeqCst) > 1 {
                    forget_open(&state.root);
                }
            }
            window_closed(cx);
            true
        });
        let ws = cx.new(|cx| workspace::Workspace::new(root, state, window, cx));
        mark("workspace built");
        ws
    });
    if let Err(err) = &window {
        log::error!("failed to open main window: {err:#}");
        if APP_WINDOWS.load(Ordering::SeqCst) == 0 {
            welcome::open_welcome(cx);
        }
        return;
    }
    // `open_window` returns after the first frame has been drawn.
    mark("first frame drawn");
    cx.activate(true);
    welcome::close_welcome(cx);
    if let Ok(mut open) = OPEN_PROJECTS.lock() {
        if !open.iter().any(|p| session_same(p, &root_for_recent)) {
            open.push(root_for_recent.clone());
        }
    }
    let root = root_for_recent;
    cx.background_spawn(async move {
        write_session();
        if let Err(e) = graphyne_core::recent::push(&root) {
            log::warn!("recent projects: {e}");
        }
    })
    .detach();
}

fn session_same(a: &Path, b: &Path) -> bool {
    if cfg!(windows) { a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()) } else { a == b }
}

/// "Open Folder…": native folder picker, then a new project window.
pub(crate) fn prompt_open_folder(cx: &mut App) {
    let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Open Project".into()), directory: None });
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(paths))) = picked.await {
            if let Some(root) = paths.into_iter().next() {
                cx.update(|cx| open_workspace(root, |_| {}, cx)).ok();
            }
        }
    })
    .detach();
}

/// `canonicalize` on Windows yields `\\?\C:\...`; strip it for display and child processes.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}
