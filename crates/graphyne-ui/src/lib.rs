//! Graphyne GPUI front-end.

mod actions;
mod assets;
mod components;
mod debug_panel;
mod debugger;
mod editor;
mod editor_pane;
mod file_icons;
mod file_icons_data;
mod file_tree;
mod hex_view;
mod lsp_store;
mod panels;
mod search_everywhere;
mod terminal_view;
mod text_input;
mod theme;
mod workspace;

use gpui::{App, AppContext, Application, Bounds, PathPromptOptions, TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions, point, px, size};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Open project windows; the app quits when the last one closes.
static WORKSPACE_WINDOWS: AtomicUsize = AtomicUsize::new(0);

/// Boot the application. The first CLI argument is the project folder; without one a
/// native folder picker is shown (double-click launch).
pub fn run() {
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
        actions::bind_keys(cx);
        cx.on_action(|_: &actions::Quit, cx| cx.quit());
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
                let picked = cx.prompt_for_paths(PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some("Open Project".into()),
                });
                cx.spawn(async move |cx| {
                    let root = match picked.await {
                        Ok(Ok(Some(paths))) => paths.into_iter().next(),
                        _ => None,
                    };
                    cx.update(|cx| match root {
                        Some(root) => open_workspace(root, mark, cx),
                        None => cx.quit(),
                    })
                    .ok();
                })
                .detach();
            }
        }
    });
}

/// Open `root` in a new project window (also used by Open Folder / Recent Projects).
pub(crate) fn open_workspace(root: PathBuf, mark: impl Fn(&str) + Copy + 'static, cx: &mut App) {
    let root = std::fs::canonicalize(&root).map(strip_verbatim).unwrap_or(root);
    let root_for_recent = root.clone();
    let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
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
        WORKSPACE_WINDOWS.fetch_add(1, Ordering::SeqCst);
        window.on_window_should_close(cx, |_, cx| {
            if WORKSPACE_WINDOWS.fetch_sub(1, Ordering::SeqCst) <= 1 {
                cx.quit();
            }
            true
        });
        let ws = cx.new(|cx| workspace::Workspace::new(root, window, cx));
        mark("workspace built");
        ws
    });
    if let Err(err) = &window {
        log::error!("failed to open main window: {err:#}");
        if WORKSPACE_WINDOWS.load(Ordering::SeqCst) == 0 {
            cx.quit();
        }
        return;
    }
    // `open_window` returns after the first frame has been drawn.
    mark("first frame drawn");
    cx.activate(true);
    let recent = window.map(|_| ()).ok();
    if recent.is_some() {
        let root = root_for_recent;
        cx.background_spawn(async move {
            if let Err(e) = graphyne_core::recent::push(&root) {
                log::warn!("recent projects: {e}");
            }
        })
        .detach();
    }
}

/// "Open Folder…": native folder picker, then a new project window.
pub(crate) fn prompt_open_folder(cx: &mut App) {
    let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Open Project".into()) });
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
