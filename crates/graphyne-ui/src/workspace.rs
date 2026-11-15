//! The root window view: Fleet island layout.
//!
//! ```text
//! title bar (CSD)
//! [left island] | [editor island / terminal island] | [right island]
//! status bar
//! ```

use crate::actions::*;
use crate::assets::Icon;
use crate::components::{header_tab, icon, icon_button, island, island_header, small_icon_button, tooltip, vsep};
use crate::editor_pane::{EditorPane, PaneEvent, PaneItem};
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::panels::{FindPanel, GitPanel, HistoryPanel, PanelEvent};
use crate::search_everywhere::{IdeAction, SearchEverywhere as SearchView, SearchEvent, SearchItem};
use crate::terminal_view::{DetachedTerminal, TerminalPanel, TerminalPanelEvent};
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, CursorStyle, Entity, FocusHandle, Focusable, ModifiersChangedEvent, MouseButton,
    MouseMoveEvent, Pixels, SharedString, Subscription, TitlebarOptions, Window, WindowBounds, WindowControlArea,
    WindowDecorations, WindowHandle, WindowOptions, div, point, px, size,
};
use graphyne_core::LanguageId;
use graphyne_core::session::{self, ProjectState, WindowState};
use graphyne_core::run::{RunPlan, ShellKind};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Title-bar dropdowns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu {
    Projects,
    Run,
    App,
    /// "+" in the left panel: init.lua, plugins folder, plugin tabs.
    Plugins,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LeftTab {
    Files,
    Search,
    Git,
    History,
    /// A tab registered by a Lua plugin (`graphyne.ui.register_tab`).
    Plugin(u64),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RightTab {
    Structure,
    Secrets,
    Plugin(u64),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BottomView {
    Terminal,
    Debug,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Splitter {
    Left,
    Right,
    Bottom,
}

#[derive(Clone)]
struct Notification {
    text: SharedString,
    error: bool,
}

pub struct Workspace {
    focus_handle: FocusHandle,
    root: PathBuf,
    project_name: SharedString,
    branch: Option<SharedString>,
    left: bool,
    right: bool,
    bottom: bool,
    left_tab: LeftTab,
    right_tab: RightTab,
    file_tree: Entity<FileTree>,
    find: Entity<FindPanel>,
    git: Entity<GitPanel>,
    history: Entity<HistoryPanel>,
    pane: Entity<EditorPane>,
    terminal: Entity<TerminalPanel>,
    lsp_store: Entity<crate::lsp_store::LspStore>,
    debugger: Entity<crate::debugger::Debugger>,
    debug_panel: Entity<crate::debug_panel::DebugPanel>,
    bottom_view: BottomView,
    detached: Option<WindowHandle<DetachedTerminal>>,
    search: Option<(Entity<SearchView>, Subscription)>,
    files: Arc<Vec<String>>,
    indexing: bool,
    toolchains: HashMap<LanguageId, Option<SharedString>>,
    probing: HashSet<LanguageId>,
    notifications: Vec<Notification>,
    unread: bool,
    show_notifications: bool,
    menu: Option<Menu>,
    recent: Vec<PathBuf>,
    /// Run-menu presets for this project, with "installed" flags.
    run_presets: Vec<(&'static graphyne_core::presets::Preset, bool)>,
    /// Live-reload preview server, started by the first Run on an HTML/CSS file.
    live_server: Option<Arc<graphyne_core::live_server::LiveServer>>,
    last_shift: Option<Instant>,
    shift_down: bool,
    left_width: Pixels,
    right_width: Pixels,
    bottom_height: Pixels,
    dragging: Option<Splitter>,
    _watcher: Option<notify::RecommendedWatcher>,
    _subs: Vec<Subscription>,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Workspace {
    /// `state`: this project's layout from the last session (tabs, panels, …), if any.
    pub fn new(root: PathBuf, state: Option<ProjectState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let file_tree = cx.new(|cx| FileTree::new(root.clone(), cx));
        let find = cx.new(|cx| FindPanel::new(root.clone(), cx));
        let git = cx.new(|cx| GitPanel::new(root.clone(), cx));
        let history = cx.new(|cx| HistoryPanel::new(root.clone(), cx));
        let lsp_store = cx.new(|_| crate::lsp_store::LspStore::new(root.clone()));
        let pane = cx.new(|cx| EditorPane::new(root.clone(), lsp_store.clone(), cx));
        let debugger = cx.new(|_| crate::debugger::Debugger::new(root.clone()));
        let debug_panel = cx.new(|cx| crate::debug_panel::DebugPanel::new(debugger.clone(), cx));
        pane.update(cx, |p, _| p.debugger = Some(debugger.clone()));
        git.update(cx, |g, _| g.pane = Some(pane.downgrade()));
        // Line-level staging: diff views tick blocks, the Git tab commits them.
        let hunk_excluded = crate::diff_view::HunkSelection::default();
        git.update(cx, |g, _| g.hunk_excluded = hunk_excluded.clone());
        pane.update(cx, |p, _| p.hunk_excluded = hunk_excluded);
        debugger.update(cx, |d, cx| d.load(cx));
        let terminal = cx.new(|cx| TerminalPanel::new(root.clone(), cx));

        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(&file_tree, window, |this, _, ev, window, cx| match ev {
            FileTreeEvent::OpenFile(path) => this.open_file(path.clone(), None, window, cx),
        }));
        let panel_handler = |this: &mut Self, ev: &PanelEvent, window: &mut Window, cx: &mut Context<Self>| match ev {
            PanelEvent::OpenFileAt(path, line, col) => this.open_file(path.clone(), Some((*line, *col)), window, cx),
            PanelEvent::OpenDiff(path) => {
                let path = path.clone();
                this.pane.update(cx, |p, cx| p.open_diff(path, window, cx));
            }
            PanelEvent::GitChanged(map, branch) => {
                this.branch = branch.clone().map(SharedString::from);
                let map = map.clone();
                this.file_tree.update(cx, |t, cx| t.set_git_status(map, cx));
                cx.notify();
            }
        };
        subs.push(cx.subscribe_in(&find, window, move |this, _, ev, window, cx| panel_handler(this, ev, window, cx)));
        subs.push(cx.subscribe_in(&git, window, move |this, _, ev, window, cx| panel_handler(this, ev, window, cx)));
        subs.push(cx.subscribe_in(&pane, window, |this, _, ev, window, cx| match ev {
            PaneEvent::Saved => {
                this.git.update(cx, |g, cx| g.refresh(cx));
                cx.notify();
            }
            PaneEvent::Changed => {
                this.probe_active_toolchain(cx);
                this.git.update(cx, |_, cx| cx.notify());
                cx.notify();
            }
            PaneEvent::OpenLocation(path, line, col) => this.open_file(path.clone(), Some((*line, *col)), window, cx),
            PaneEvent::ApplyEdits(files) => this.apply_file_edits(files.clone(), cx),
            PaneEvent::Notify(msg) => this.notify_user(msg.clone(), false, cx),
        }));
        subs.push(cx.subscribe(&lsp_store, |this, _, ev, cx| match ev {
            crate::lsp_store::LspStoreEvent::Notify(msg, err) => this.notify_user(msg.clone(), *err, cx),
            crate::lsp_store::LspStoreEvent::ApplyEdit { client, id, edits } => {
                this.apply_file_edits(edits.clone(), cx);
                client.respond(id.clone(), serde_json::json!({ "applied": true }));
            }
        }));
        subs.push(cx.observe(&lsp_store, |_, _, cx| cx.notify()));
        subs.push(cx.subscribe_in(&terminal, window, |this, _, ev, window, cx| match ev {
            TerminalPanelEvent::OpenFile(path, line, col) => this.open_file(path.clone(), Some((*line, *col)), window, cx),
            TerminalPanelEvent::ToggleDetach => this.toggle_detach(window, cx),
            TerminalPanelEvent::ShowDebug => {
                this.bottom_view = BottomView::Debug;
                cx.notify();
            }
        }));
        subs.push(cx.subscribe(&debug_panel, |this, _, ev, cx| match ev {
            crate::debug_panel::DebugPanelEvent::ShowTerminal => {
                this.bottom_view = BottomView::Terminal;
                cx.notify();
            }
        }));
        subs.push(cx.subscribe_in(&debugger, window, |this, _, ev, window, cx| match ev {
            crate::debugger::DebuggerEvent::Reveal(path, line) => {
                let path = path.clone();
                this.open_file(path, Some((*line, 0)), window, cx);
            }
            crate::debugger::DebuggerEvent::Notify(msg, err) => this.notify_user(msg.clone(), *err, cx),
            crate::debugger::DebuggerEvent::Started => {
                this.bottom = true;
                this.bottom_view = BottomView::Debug;
                cx.notify();
            }
            crate::debugger::DebuggerEvent::RunInTerminal(title, cwd, args, env) => {
                let (title, cwd, args, env) = (title.clone(), cwd.clone(), args.clone(), env.clone());
                this.terminal.update(cx, |t, cx| t.run_process_in_tab(&title, cwd, args, env, cx));
                cx.notify();
            }
        }));
        // FocusLost auto-save + refresh on re-activation.
        subs.push(cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                crate::lua_bridge::set_active(cx.entity().downgrade(), window.window_handle(), cx);
                this.refresh_project(cx);
            } else {
                let tasks = this.pane.update(cx, |p, cx| p.save_all(cx));
                for t in tasks {
                    t.detach();
                }
            }
        }));
        subs.push(cx.observe(&pane, |_, _, cx| cx.notify()));

        let mut ws = Self {
            focus_handle: cx.focus_handle(),
            project_name: graphyne_core::project::project_name(&root).into(),
            root,
            branch: None,
            left: true,
            right: false,
            bottom: true,
            left_tab: LeftTab::Files,
            right_tab: RightTab::Structure,
            file_tree,
            find,
            git,
            history,
            pane,
            terminal,
            lsp_store,
            debugger,
            debug_panel,
            bottom_view: BottomView::Terminal,
            detached: None,
            search: None,
            files: Arc::new(Vec::new()),
            indexing: true,
            toolchains: HashMap::new(),
            probing: HashSet::new(),
            notifications: Vec::new(),
            unread: false,
            show_notifications: false,
            menu: None,
            recent: Vec::new(),
            run_presets: Vec::new(),
            live_server: None,
            last_shift: None,
            shift_down: false,
            left_width: px(300.),
            right_width: px(300.),
            bottom_height: px(260.),
            dragging: None,
            _watcher: None,
            _subs: subs,
        };
        if let Some(state) = state {
            ws.apply_state(state, window, cx);
        }
        // Keep the session current even if Graphyne is killed: save when the window loses focus.
        ws._subs.push(cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                this.save_state(window, cx);
            }
        }));
        crate::lua_bridge::register(cx.entity().downgrade(), window.window_handle(), cx);
        ws.index_project(cx);
        ws.start_watcher(cx);
        ws.terminal.update(cx, |t, cx| t.ensure_terminal(cx));
        let dbg = ws.debugger.clone();
        ws.terminal.update(cx, |t, _| t.debugger = Some(dbg));
        window.focus(&ws.focus_handle);
        ws
    }

    // ---------------------------------------------------------------- session

    /// Open editors (for settings that change how they look).
    pub fn editors(&self, cx: &App) -> Vec<Entity<crate::editor::Editor>> {
        self.pane.read(cx).editors().cloned().collect()
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Put back the layout saved by [`Self::project_state`].
    fn apply_state(&mut self, s: ProjectState, window: &mut Window, cx: &mut Context<Self>) {
        self.left = s.left;
        self.right = s.right;
        self.bottom = s.bottom;
        let plugin_tab = |t: &str| t.strip_prefix("plugin:").and_then(|id| id.parse::<u64>().ok());
        self.left_tab = match s.left_tab.as_str() {
            "search" => LeftTab::Search,
            "git" => LeftTab::Git,
            "history" => LeftTab::History,
            t => plugin_tab(t).map(LeftTab::Plugin).unwrap_or(LeftTab::Files),
        };
        self.right_tab = match s.right_tab.as_str() {
            "secrets" => RightTab::Secrets,
            t => plugin_tab(t).map(RightTab::Plugin).unwrap_or(RightTab::Structure),
        };
        self.left_width = px(s.left_width.clamp(160., 1200.));
        self.right_width = px(s.right_width.clamp(180., 1200.));
        self.bottom_height = px(s.bottom_height.clamp(80., 1400.));
        self.file_tree.update(cx, |t, cx| t.restore_expanded(s.expanded, cx));
        self.pane.update(cx, |p, cx| p.restore(s.tabs, s.active_tab, window, cx));
    }

    /// Everything worth restoring next time: tabs with cursors, panels, window placement.
    pub fn project_state(&self, window: &Window, cx: &App) -> ProjectState {
        let (tabs, active_tab) = self.pane.read(cx).tab_states(cx);
        let (bounds, maximized) = match window.window_bounds() {
            WindowBounds::Windowed(b) => (b, false),
            WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => (b, true),
        };
        ProjectState {
            root: self.root.clone(),
            window: Some(WindowState {
                x: f32::from(bounds.origin.x),
                y: f32::from(bounds.origin.y),
                width: f32::from(bounds.size.width),
                height: f32::from(bounds.size.height),
                maximized,
            }),
            left: self.left,
            right: self.right,
            bottom: self.bottom,
            left_tab: match self.left_tab {
                LeftTab::Files => "files".into(),
                LeftTab::Search => "search".into(),
                LeftTab::Git => "git".into(),
                LeftTab::History => "history".into(),
                LeftTab::Plugin(id) => format!("plugin:{id}"),
            },
            right_tab: match self.right_tab {
                RightTab::Structure => "structure".into(),
                RightTab::Secrets => "secrets".into(),
                RightTab::Plugin(id) => format!("plugin:{id}"),
            },
            left_width: f32::from(self.left_width),
            right_width: f32::from(self.right_width),
            bottom_height: f32::from(self.bottom_height),
            tabs,
            active_tab,
            expanded: self.file_tree.read(cx).expanded_dirs(),
        }
    }

    /// Save this project's layout in the background.
    fn save_state(&self, window: &Window, cx: &mut Context<Self>) {
        let state = self.project_state(window, cx);
        cx.background_spawn(async move {
            if let Err(e) = session::save_project(&state) {
                log::warn!("session: {e:#}");
            }
        })
        .detach();
    }

    /// File › Close Project: save, close this window, and show the Welcome screen when no
    /// other project is open. A closed project is not reopened on the next launch.
    fn close_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tasks = self.pane.update(cx, |p, cx| p.save_all(cx));
        let state = self.project_state(window, cx);
        self.lsp_store.read(cx).shutdown_all();
        if let Some(detached) = self.detached.take() {
            detached.update(cx, |_, window, _| window.remove_window()).ok();
        }
        crate::project_closed(state, cx);
        let handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            for t in tasks {
                t.await;
            }
            cx.update(|cx| {
                handle.update(cx, |_, window, _| window.remove_window()).ok();
                crate::window_closed(cx);
            })
            .ok();
        })
        .detach();
    }

    // ---------------------------------------------------------------- project services

    fn index_project(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let lib_root = self.root.clone();
        self.indexing = true;
        cx.spawn(async move |this, cx| {
            let files = cx.background_spawn(async move { graphyne_core::project::walk_project_files(&root, 200_000) }).await;
            let mut langs: Vec<LanguageId> = Vec::new();
            for f in files.iter().take(5000) {
                let l = LanguageId::from_path(std::path::Path::new(f));
                if l.info().toolchain.is_some() && !langs.contains(&l) {
                    langs.push(l);
                }
            }
            let files = Arc::new(files);
            this.update(cx, |this, cx| {
                this.files = files.clone();
                this.indexing = false;
                if let Some((s, _)) = &this.search {
                    s.update(cx, |s, cx| s.set_files(files, cx));
                }
                cx.notify();
            })
            .ok();
            // External Libraries: detected SDKs / toolchains.
            let probes = cx
                .background_spawn(async move {
                    let mut seen = HashSet::new();
                    langs
                        .into_iter()
                        .filter_map(|l| graphyne_core::toolchain::probe(l).map(|v| (l, v)))
                        .filter(|(_, v)| seen.insert(v.clone()))
                        .map(|(l, v)| (l, v, graphyne_core::libraries::entries_for(l, &lib_root)))
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                let libs = probes.iter().map(|(_, v, entries)| (SharedString::from(format!("< {v} >")), entries.clone())).collect();
                for (l, v, _) in probes {
                    this.toolchains.insert(l, Some(v.into()));
                }
                this.file_tree.update(cx, |t, cx| t.set_libraries(libs, cx));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Recursive file watcher (ReadDirectoryChangesW / inotify / FSEvents) on a background
    /// thread; bursts are debounced and coalesced before touching the UI.
    fn start_watcher(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let (tx, rx) = flume::unbounded::<(bool, PathBuf)>();
        cx.spawn(async move |this, cx| {
            let watch_root = root.clone();
            let watcher = cx
                .background_spawn(async move {
                    use notify::Watcher;
                    let classify_root = watch_root.clone();
                    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                        let Ok(ev) = res else { return };
                        if matches!(ev.kind, notify::EventKind::Access(_)) {
                            return;
                        }
                        for p in ev.paths {
                            if let Some(git_only) = classify_fs_change(&classify_root, &p) {
                                let _ = tx.send((git_only, p));
                            }
                        }
                    })
                    .ok()?;
                    w.watch(&watch_root, notify::RecursiveMode::Recursive).ok()?;
                    Some(w)
                })
                .await;
            let Some(watcher) = watcher else { return };
            if this.update(cx, |this, _| this._watcher = Some(watcher)).is_err() {
                return;
            }
            while let Ok(first) = rx.recv_async().await {
                cx.background_executor().timer(Duration::from_millis(300)).await;
                let mut files: Vec<PathBuf> = Vec::new();
                let mut git = false;
                for (git_only, p) in std::iter::once(first).chain(rx.try_iter()) {
                    if git_only {
                        git = true;
                    } else if !files.contains(&p) {
                        files.push(p);
                    }
                }
                if this.update(cx, |this, cx| this.on_fs_change(files, git, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_fs_change(&mut self, files: Vec<PathBuf>, git_only: bool, cx: &mut Context<Self>) {
        if let (Some(server), false) = (&self.live_server, files.is_empty()) {
            server.reload();
        }
        // Open diffs follow saves, external edits and commits (HEAD moves).
        let diffs: Vec<_> = self.pane.read(cx).diff_views().cloned().collect();
        for d in diffs {
            if git_only || files.contains(&d.read(cx).path) {
                d.update(cx, |d, cx| d.refresh(cx));
            }
        }
        if !files.is_empty() {
            self.file_tree.update(cx, |t, cx| t.refresh(cx));
            self.index_files(cx);
            let editors: Vec<_> = self.pane.read(cx).editors().cloned().collect();
            for e in editors {
                let changed = e.read(cx).path.as_ref().is_some_and(|p| files.contains(p));
                if changed {
                    e.update(cx, |e, cx| e.reload_from_disk(cx));
                }
            }
        }
        if git_only || !files.is_empty() {
            self.git.update(cx, |g, cx| g.refresh(cx));
        }
        if git_only {
            self.history.update(cx, |h, cx| h.refresh(cx));
        }
    }

    /// Re-walk the project for Search Everywhere (background).
    fn index_files(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let files = Arc::new(cx.background_spawn(async move { graphyne_core::project::walk_project_files(&root, 200_000) }).await);
            this.update(cx, |this, cx| {
                this.files = files.clone();
                if let Some((s, _)) = &this.search {
                    s.update(cx, |s, cx| s.set_files(files, cx));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Apply language-server edits: open documents in place, others on disk (background).
    fn apply_file_edits(&mut self, files: Vec<graphyne_lsp::model::FileEdits>, cx: &mut Context<Self>) {
        for file in files {
            if let Some(editor) = self.pane.read(cx).editor_for(&file.path, cx) {
                editor.update(cx, |e, cx| e.apply_lsp_text_edits(&file.edits, cx));
                continue;
            }
            let path = file.path.clone();
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_spawn(async move {
                        use graphyne_lsp::convert::position_to_char;
                        let text = graphyne_core::fs_util::read_text_lossy(&file.path)?;
                        let mut rope = ropey::Rope::from_str(&text);
                        let mut edits: Vec<(usize, usize, String)> = file
                            .edits
                            .iter()
                            .map(|e| (position_to_char(&rope, e.range.start), position_to_char(&rope, e.range.end), e.new_text.clone()))
                            .collect();
                        edits.sort_by(|a, b| b.0.cmp(&a.0));
                        for (s, e, t) in edits {
                            rope.remove(s..e.max(s));
                            rope.insert(s, &t);
                        }
                        graphyne_core::fs_util::atomic_write(&file.path, rope.to_string().as_bytes())
                    })
                    .await;
                if let Err(err) = result {
                    this.update(cx, |this, cx| this.notify_user(format!("Could not apply edit to {}: {err:#}", path.display()), true, cx)).ok();
                }
            })
            .detach();
        }
    }

    fn refresh_project(&mut self, cx: &mut Context<Self>) {
        self.file_tree.update(cx, |t, cx| t.refresh(cx));
        self.git.update(cx, |g, cx| g.refresh(cx));
        self.history.update(cx, |h, cx| h.refresh(cx));
        self.index_files(cx);
    }

    fn probe_active_toolchain(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.pane.read(cx).active_editor() else { return };
        let lang = editor.read(cx).language;
        if lang.info().toolchain.is_none() || self.toolchains.contains_key(&lang) || !self.probing.insert(lang) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let v = cx.background_spawn(async move { graphyne_core::toolchain::probe(lang) }).await;
            this.update(cx, |this, cx| {
                this.toolchains.insert(lang, v.map(SharedString::from));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn notify_user(&mut self, text: impl Into<SharedString>, error: bool, cx: &mut Context<Self>) {
        self.notifications.insert(0, Notification { text: text.into(), error });
        self.notifications.truncate(30);
        self.unread = true;
        cx.notify();
    }

    pub fn open_file(&mut self, path: PathBuf, at: Option<(usize, usize)>, window: &mut Window, cx: &mut Context<Self>) {
        if !path.exists() {
            self.notify_user(format!("File not found: {}", path.display()), true, cx);
            return;
        }
        self.file_tree.update(cx, |t, cx| t.reveal(&path, cx));
        self.pane.update(cx, |p, cx| p.open(path, at, window, cx));
    }

    // ---------------------------------------------------------------- actions

    fn toggle_left(&mut self, _: &ToggleLeftDock, _: &mut Window, cx: &mut Context<Self>) {
        self.left = !self.left;
        cx.notify();
    }
    fn toggle_right(&mut self, _: &ToggleRightDock, _: &mut Window, cx: &mut Context<Self>) {
        self.right = !self.right;
        cx.notify();
    }
    fn toggle_bottom(&mut self, _: &ToggleBottomDock, window: &mut Window, cx: &mut Context<Self>) {
        if self.detached.is_some() {
            return;
        }
        self.bottom = !self.bottom;
        if self.bottom {
            self.terminal.update(cx, |t, cx| t.ensure_terminal(cx));
            self.terminal.read(cx).focus_active(window, cx);
        } else {
            self.pane.read(cx).focus_active(window, cx);
        }
        cx.notify();
    }
    fn focus_terminal(&mut self, _: &FocusTerminal, window: &mut Window, cx: &mut Context<Self>) {
        if self.detached.is_none() {
            self.bottom = true;
        }
        self.terminal.update(cx, |t, cx| t.ensure_terminal(cx));
        self.terminal.read(cx).focus_active(window, cx);
        cx.notify();
    }
    fn focus_editor(&mut self, _: &FocusEditor, window: &mut Window, cx: &mut Context<Self>) {
        self.pane.read(cx).focus_active(window, cx);
    }
    fn new_terminal(&mut self, _: &NewTerminal, window: &mut Window, cx: &mut Context<Self>) {
        if self.detached.is_none() {
            self.bottom = true;
        }
        self.terminal.update(cx, |t, cx| {
            t.new_terminal(cx);
        });
        self.terminal.read(cx).focus_active(window, cx);
        cx.notify();
    }
    fn close_tab(&mut self, _: &CloseActiveTab, window: &mut Window, cx: &mut Context<Self>) {
        self.pane.update(cx, |p, cx| p.close_active(window, cx));
    }
    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.pane.update(cx, |p, cx| p.activate_delta(1, window, cx));
    }
    fn prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.pane.update(cx, |p, cx| p.activate_delta(-1, window, cx));
    }
    fn save_all(&mut self, _: &SaveAll, _: &mut Window, cx: &mut Context<Self>) {
        for t in self.pane.update(cx, |p, cx| p.save_all(cx)) {
            t.detach();
        }
    }
    fn find_in_files(&mut self, _: &FindInFiles, window: &mut Window, cx: &mut Context<Self>) {
        self.left = true;
        self.left_tab = LeftTab::Search;
        window.focus(&self.find.focus_handle(cx));
        cx.notify();
    }
    fn quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        self.lsp_store.read(cx).shutdown_all();
        let tasks = self.pane.update(cx, |p, cx| p.save_all(cx));
        cx.spawn(async move |_, cx| {
            for t in tasks {
                t.await;
            }
            cx.update(crate::quit_app).ok();
        })
        .detach();
    }
    /// Shift+F9: save everything, then start a DAP session for the active file.
    fn debug_active(&mut self, _: &DebugActive, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.pane.read(cx).active_editor().and_then(|e| e.read(cx).path.clone()) else {
            self.notify_user("Open a file to debug it", true, cx);
            return;
        };
        let saves = self.pane.update(cx, |p, cx| p.save_all(cx));
        let debugger = self.debugger.clone();
        cx.spawn_in(window, async move |_, cx| {
            for s in saves {
                s.await;
            }
            debugger.update(cx, |d, cx| d.start(path, cx)).ok();
        })
        .detach();
    }
    fn debug_resume(&mut self, _: &DebugResume, _: &mut Window, cx: &mut Context<Self>) {
        self.debugger.update(cx, |d, cx| d.resume(cx));
    }
    fn debug_step_over(&mut self, _: &DebugStepOver, _: &mut Window, cx: &mut Context<Self>) {
        self.debugger.update(cx, |d, cx| d.step_over(cx));
    }
    fn debug_step_into(&mut self, _: &DebugStepInto, _: &mut Window, cx: &mut Context<Self>) {
        self.debugger.update(cx, |d, cx| d.step_into(cx));
    }
    fn debug_step_out(&mut self, _: &DebugStepOut, _: &mut Window, cx: &mut Context<Self>) {
        self.debugger.update(cx, |d, cx| d.step_out(cx));
    }
    fn debug_stop(&mut self, _: &DebugStop, _: &mut Window, cx: &mut Context<Self>) {
        self.debugger.update(cx, |d, cx| d.stop(cx));
    }

    fn search_action(&mut self, _: &SearchEverywhere, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_search(window, cx);
    }
    fn run_action(&mut self, _: &RunActive, window: &mut Window, cx: &mut Context<Self>) {
        self.run_active(window, cx);
    }
    fn detach_action(&mut self, _: &ToggleDetachTerminal, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_detach(window, cx);
    }

    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            self.pane.read(cx).focus_active(window, cx);
            cx.notify();
            return;
        }
        let symbols = self.pane.read(cx).active_editor().map(|e| e.read(cx).outline.clone()).unwrap_or_default();
        let files = self.files.clone();
        let view = cx.new(|cx| SearchView::new(files, symbols, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, ev, window, cx| match ev {
            SearchEvent::Dismiss => {
                this.search = None;
                this.pane.read(cx).focus_active(window, cx);
                cx.notify();
            }
            SearchEvent::Confirm(item) => {
                this.search = None;
                match item.clone() {
                    SearchItem::File(rel) => {
                        let path = this.root.join(rel);
                        this.open_file(path, None, window, cx);
                    }
                    SearchItem::Symbol { row } => {
                        if let Some(e) = this.pane.read(cx).active_editor() {
                            e.update(cx, |e, cx| e.go_to(row, 0, window, cx));
                        }
                    }
                    SearchItem::Action(a) => this.run_ide_action(a, window, cx),
                }
                cx.notify();
            }
        });
        window.focus(&view.focus_handle(cx));
        self.search = Some((view, sub));
        cx.notify();
    }

    fn run_ide_action(&mut self, a: IdeAction, window: &mut Window, cx: &mut Context<Self>) {
        match a {
            IdeAction::ToggleLeft => self.toggle_left(&ToggleLeftDock, window, cx),
            IdeAction::ToggleBottom => self.toggle_bottom(&ToggleBottomDock, window, cx),
            IdeAction::ToggleRight => self.toggle_right(&ToggleRightDock, window, cx),
            IdeAction::Run => self.run_active(window, cx),
            IdeAction::NewTerminal => self.new_terminal(&NewTerminal, window, cx),
            IdeAction::DetachTerminal => self.toggle_detach(window, cx),
            IdeAction::FindInFiles => self.find_in_files(&FindInFiles, window, cx),
            IdeAction::SaveAll => self.save_all(&SaveAll, window, cx),
            IdeAction::GitPanel => {
                self.left = true;
                self.left_tab = LeftTab::Git;
                window.focus(&self.git.focus_handle(cx));
            }
            IdeAction::History => {
                self.left = true;
                self.left_tab = LeftTab::History;
            }
            IdeAction::OpenFolder => crate::prompt_open_folder(cx),
            IdeAction::CloseProject => self.close_project(window, cx),
            IdeAction::Plugins => self.pane.update(cx, |p, cx| p.open_plugins_page(window, cx)),
            IdeAction::Settings => self.pane.update(cx, |p, cx| p.open_settings_page(window, cx)),
            IdeAction::Quit => self.quit(&Quit, window, cx),
        }
        cx.notify();
    }

    /// Universal One-Click Run: auto-save everything, then route by file type.
    /// Run-menu preset in its own terminal tab (detachable like any terminal). Commands that
    /// need a target are typed without Enter so the user can append host / URL / binary.
    fn run_preset(&mut self, preset: &'static graphyne_core::presets::Preset, window: &mut Window, cx: &mut Context<Self>) {
        let saves = self.pane.update(cx, |p, cx| p.save_all(cx));
        let line = if preset.needs_target { preset.command.to_string() } else { format!("{}\r", preset.command) };
        let cwd = self.root.clone();
        if self.detached.is_none() {
            self.bottom = true;
        }
        self.bottom_view = BottomView::Terminal;
        cx.spawn_in(window, async move |this, cx| {
            for s in saves {
                s.await;
            }
            this.update_in(cx, |this, window, cx| {
                this.terminal.update(cx, |t, cx| t.run_in_tab(preset.label, cwd, &line, cx));
                this.terminal.read(cx).focus_active(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn run_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.pane.read(cx).active_editor() else {
            self.notify_user("Open a file to run it", true, cx);
            return;
        };
        let Some(path) = editor.read(cx).path.clone() else { return };
        let saves = self.pane.update(cx, |p, cx| p.save_all(cx));
        let shell = ShellKind::native();
        match graphyne_core::run::plan(&path, &self.root, shell) {
            RunPlan::Pty { label, cwd, command } => {
                let line = graphyne_core::run::pty_line(shell, &cwd, &command);
                if self.detached.is_none() {
                    self.bottom = true;
                }
                cx.spawn_in(window, async move |this, cx| {
                    for s in saves {
                        s.await;
                    }
                    this.update_in(cx, |this, window, cx| {
                        this.terminal.update(cx, |t, cx| t.run_in_tab(&label, cwd, &line, cx));
                        this.terminal.read(cx).focus_active(window, cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            RunPlan::WebPreview { entry, .. } => {
                // Serve the project on 127.0.0.1 and open the page there; saves reload it.
                let existing = self.live_server.clone();
                let root = self.root.clone();
                cx.spawn(async move |this, cx| {
                    for s in saves {
                        s.await;
                    }
                    let started = cx
                        .background_spawn(async move {
                            let server = match existing {
                                Some(server) => server,
                                None => graphyne_core::live_server::LiveServer::start(root)?,
                            };
                            // A stylesheet previews through the page next to it.
                            let page = match entry.extension().and_then(|e| e.to_str()) {
                                Some("css" | "scss") => entry.parent().map(|d| d.join("index.html")).filter(|p| p.is_file()).unwrap_or(entry),
                                _ => entry,
                            };
                            let url = server.url_for(&page);
                            Ok::<_, std::io::Error>((server, url))
                        })
                        .await;
                    this.update(cx, |this, cx| match started {
                        Ok((server, url)) => {
                            this.live_server = Some(server);
                            match url {
                                Some(url) => {
                                    cx.open_url(&url);
                                    this.notify_user(format!("Live preview at {url}: the page reloads on every save"), false, cx);
                                }
                                None => this.notify_user("Live preview serves only files inside the project folder", true, cx),
                            }
                        }
                        Err(e) => this.notify_user(format!("Live preview server failed to start: {e}"), true, cx),
                    })
                    .ok();
                })
                .detach();
            }
            RunPlan::Sql { .. } => self.notify_user("SQL execution → DataGrid requires a database connection (planned: sqlx pools)", true, cx),
            RunPlan::Unsupported { reason } => self.notify_user(reason, true, cx),
        }
        cx.notify();
    }

    // ---------------------------------------------------------------- detachable terminal

    fn toggle_detach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = self.detached.take() {
            // The request may come from inside the detached window itself (its Attach button);
            // that window is borrowed until the current event finishes, so close it afterwards.
            cx.defer(move |cx| {
                handle.update(cx, |_, w, _| w.remove_window()).ok();
            });
            self.attach_terminal(window, cx);
            return;
        }
        let panel = self.terminal.clone();
        panel.update(cx, |p, cx| {
            p.detached = true;
            p.ensure_terminal(cx);
            cx.notify();
        });
        let weak = cx.entity().downgrade();
        let title: SharedString = format!("Terminal — {}", self.project_name).into();
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions { title: Some(title.clone()), appears_transparent: true, traffic_light_position: Some(point(px(12.), px(12.))) }),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(980.), px(600.)), cx))),
            window_decorations: Some(WindowDecorations::Client),
            window_min_size: Some(size(px(420.), px(240.))),
            app_id: Some("graphyne".into()),
            ..Default::default()
        };
        let result = cx.open_window(options, |window, cx| {
            let w = weak.clone();
            window.on_window_should_close(cx, move |_, cx| {
                w.update(cx, |ws, cx| {
                    ws.detached = None;
                    ws.terminal.update(cx, |p, cx| {
                        p.detached = false;
                        cx.notify();
                    });
                    ws.bottom = true;
                    cx.notify();
                })
                .ok();
                true
            });
            let view = cx.new(|_| DetachedTerminal { panel: panel.clone(), title: title.clone(), workspace: weak.clone() });
            panel.read(cx).focus_active(window, cx);
            view
        });
        match result {
            Ok(handle) => {
                self.detached = Some(handle);
                self.pane.read(cx).focus_active(window, cx);
            }
            Err(err) => {
                self.terminal.update(cx, |p, _| p.detached = false);
                self.notify_user(format!("Could not open terminal window: {err:#}"), true, cx);
            }
        }
        cx.notify();
    }

    fn attach_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal.update(cx, |p, cx| {
            p.detached = false;
            cx.notify();
        });
        self.bottom = true;
        self.terminal.read(cx).focus_active(window, cx);
        cx.notify();
    }

    // ---------------------------------------------------------------- input plumbing

    fn on_modifiers(&mut self, ev: &ModifiersChangedEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = ev.modifiers;
        let only_shift = m.shift && !m.control && !m.alt && !m.platform;
        if only_shift && !self.shift_down {
            let now = Instant::now();
            if self.last_shift.is_some_and(|t| now.duration_since(t) < Duration::from_millis(400)) {
                self.last_shift = None;
                self.shift_down = true;
                self.toggle_search(window, cx);
                return;
            }
            self.last_shift = Some(now);
        } else if !m.shift && (m.control || m.alt || m.platform) {
            self.last_shift = None;
        }
        self.shift_down = m.shift;
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(which) = self.dragging else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.dragging = None;
            return;
        }
        let vp = window.viewport_size();
        match which {
            Splitter::Left => self.left_width = (ev.position.x - theme::gap()).clamp(px(160.), vp.width * 0.5),
            Splitter::Right => self.right_width = (vp.width - ev.position.x - theme::gap()).clamp(px(180.), vp.width * 0.5),
            Splitter::Bottom => {
                let bottom_edge = vp.height - px(theme::STATUS_BAR_HEIGHT);
                self.bottom_height = (bottom_edge - ev.position.y).clamp(px(80.), vp.height - px(220.));
            }
        }
        cx.notify();
    }

    // ---------------------------------------------------------------- rendering

    fn splitter(&self, which: Splitter, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let vertical = which != Splitter::Bottom;
        div()
            .id(match which {
                Splitter::Left => "split-left",
                Splitter::Right => "split-right",
                Splitter::Bottom => "split-bottom",
            })
            .flex_none()
            .when(vertical, |d| d.w(theme::gap()).h_full().cursor(CursorStyle::ResizeLeftRight))
            .when(!vertical, |d| d.h(theme::gap()).w_full().cursor(CursorStyle::ResizeUpDown))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                this.dragging = Some(which);
                cx.stop_propagation();
            }))
    }

    fn render_title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tint = color(c::TEXT_MUTED);
        let left_cluster = div()
            .flex()
            .flex_1()
            .flex_basis(px(0.))
            .items_center()
            .gap(px(2.))
            .when(cfg!(target_os = "macos"), |d| d.pl(px(72.)))
            .child(
                title_button("toggle-left", if self.left { Icon::PanelLeftOn } else { Icon::PanelLeft }, if self.left { color(c::TEXT) } else { tint })
                    .tooltip(tooltip("Left Panel", Some("Ctrl+1")))
                    .on_click(cx.listener(|this, _, w, cx| this.toggle_left(&ToggleLeftDock, w, cx))),
            )
            .child(
                title_button("toggle-bottom", if self.bottom && self.detached.is_none() { Icon::PanelBottomOn } else { Icon::PanelBottom }, if self.bottom { color(c::TEXT) } else { tint })
                    .tooltip(tooltip("Bottom Panel", Some("Ctrl+J")))
                    .on_click(cx.listener(|this, _, w, cx| this.toggle_bottom(&ToggleBottomDock, w, cx))),
            )
            .child(
                title_button("toggle-right", if self.right { Icon::PanelRightOn } else { Icon::PanelRight }, if self.right { color(c::TEXT) } else { tint })
                    .tooltip(tooltip("Right Panel", Some("Ctrl+Alt+B")))
                    .on_click(cx.listener(|this, _, w, cx| this.toggle_right(&ToggleRightDock, w, cx))),
            )
            .child(vsep())
            .child(
                title_button("workspaces", Icon::Grid, if self.menu == Some(Menu::Projects) { color(c::TEXT) } else { tint })
                    .tooltip(tooltip("Projects", None))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Projects, cx))),
            );

        let branch = self.branch.clone();
        let center = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .text_size(px(theme::UI_TEXT))
            .child(div().text_color(color(c::TEXT)).font_weight(gpui::FontWeight::MEDIUM).child(self.project_name.clone()))
            .when_some(branch, |d, b| {
                d.child(
                    div()
                        .id("branch")
                        .occlude()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .px(px(6.))
                        .h(px(24.))
                        .rounded(px(5.))
                        .text_color(color(c::TEXT_MUTED))
                        .hover(|s| s.bg(color(c::HOVER)))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| this.run_ide_action(IdeAction::GitPanel, window, cx)))
                        .child(icon(Icon::Branch, 14., color(c::TEXT_MUTED)))
                        .child(b),
                )
            });

        let files_indexed = self.files.len();
        let indexing = self.indexing;
        let lsp_status = self.pane.read(cx).active_editor().map(|e| e.read(cx).language).map(|l| self.lsp_store.read(cx).status_for(l));
        let (bolt_color, bolt_tip) = {
            use crate::lsp_store::ServerStatus as S;
            match &lsp_status {
                Some(S::Ready { server }) => (color(c::ACCENT), format!("Smart Mode: {server} is ready ({files_indexed} files indexed)")),
                Some(S::Indexing { server, message }) => (color(c::YELLOW), format!("Smart Mode: {server} - {message}")),
                Some(S::Starting) => (color(c::YELLOW), "Smart Mode: starting language server...".to_string()),
                Some(S::Missing { program, hint }) => (color(c::TEXT_DIM), format!("No language server ({program} not found). Install: {hint}")),
                Some(S::Failed { server, error }) => (color(c::RED), format!("{server} failed: {error}")),
                Some(S::None) | None => (
                    if indexing { color(c::TEXT_DIM) } else { color(c::ACCENT) },
                    if indexing { "Smart Mode: indexing...".to_string() } else { format!("Smart Mode: {files_indexed} files indexed") },
                ),
            }
        };
        let right_cluster = div()
            .flex()
            .flex_1()
            .flex_basis(px(0.))
            .items_center()
            .justify_end()
            .gap(px(2.))
            .child(
                title_button("smart-mode", Icon::Bolt, bolt_color).tooltip(tooltip(bolt_tip, None)),
            )
            .child(
                title_button("run", Icon::Play, color(c::TEXT))
                    .tooltip(tooltip("Run", Some("Shift+F10")))
                    .on_click(cx.listener(|this, _, w, cx| this.run_active(w, cx))),
            )
            .child(
                div()
                    .id("run-menu")
                    .occlude()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .w(px(14.))
                    .h(px(28.))
                    .ml(px(-2.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .when(self.menu == Some(Menu::Run), |d| d.bg(color(c::HOVER)))
                    .hover(|s| s.bg(color(c::HOVER)))
                    .child(icon(Icon::ChevronDown, 12., tint))
                    .tooltip(tooltip("Run Presets", None))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Run, cx))),
            )
            .child(
                title_button("debug", Icon::Bug, if self.debugger.read(cx).is_live() { color(c::GREEN) } else { color(c::TEXT) })
                    .tooltip(tooltip("Debug", Some("Shift+F9")))
                    .on_click(cx.listener(|this, _, w, cx| this.debug_active(&DebugActive, w, cx))),
            )
            .child(
                title_button("search", Icon::Search, tint)
                    .tooltip(tooltip("Search Everywhere", Some("Double Shift / Ctrl+K")))
                    .on_click(cx.listener(|this, _, w, cx| this.toggle_search(w, cx))),
            )
            .child(
                div()
                    .relative()
                    .child(
                        title_button("bell", Icon::Bell, tint).tooltip(tooltip("Notifications", None)).on_click(cx.listener(|this, _, _, cx| {
                            this.show_notifications = !this.show_notifications;
                            this.unread = false;
                            cx.notify();
                        })),
                    )
                    .when(self.unread, |d| d.child(div().absolute().top(px(6.)).right(px(6.)).size(px(6.)).rounded_full().bg(color(c::ACCENT)))),
            )
            .child(
                title_button("settings", Icon::Settings, if self.menu == Some(Menu::App) { color(c::TEXT) } else { tint })
                    .tooltip(tooltip("Main Menu", None))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::App, cx))),
            )
            .when(!cfg!(target_os = "macos"), |d| d.child(div().w(px(8.))).child(caption_buttons(window.is_maximized(), cx)));

        div()
            .id("title-bar")
            // Keep presses on the bar away from the focusable root below: its focus-on-click
            // marks the press as handled, and then Windows never starts dragging the window.
            .occlude()
            .flex()
            .flex_none()
            .items_center()
            .h(px(theme::TITLE_BAR_HEIGHT))
            .pl(px(8.))
            .window_control_area(WindowControlArea::Drag)
            .child(left_cluster)
            .child(center)
            .child(right_cluster)
    }

    fn render_left(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let focused = self.file_tree.focus_handle(cx).contains_focused(window, cx)
            || self.find.focus_handle(cx).contains_focused(window, cx)
            || self.git.focus_handle(cx).contains_focused(window, cx);
        let tab = self.left_tab;
        let tabs = [(LeftTab::Files, "Files"), (LeftTab::Search, "Search"), (LeftTab::Git, "Git"), (LeftTab::History, "History")];
        // Tabs scroll sideways when plugins add more than fit; "+" stays visible.
        let mut header = div().id("left-tabs").flex().flex_1().min_w(px(0.)).items_center().gap(px(2.)).overflow_x_scroll();
        for (t, label) in tabs {
            header = header.child(header_tab(SharedString::from(format!("left-{label}")), label, t == tab, focused).on_click(cx.listener(move |this, _, window, cx| {
                this.left_tab = t;
                match t {
                    LeftTab::Search => window.focus(&this.find.focus_handle(cx)),
                    LeftTab::History => this.history.update(cx, |h, cx| h.refresh(cx)),
                    LeftTab::Git => this.git.update(cx, |g, cx| g.refresh(cx)),
                    LeftTab::Files | LeftTab::Plugin(_) => {}
                }
                cx.notify();
            })));
        }
        for tab in crate::lua_bridge::tabs(cx).into_iter().filter(|t| t.side == graphyne_lua::Side::Left) {
            let id = tab.id;
            header = header.child(header_tab(SharedString::from(format!("left-plugin-{id}")), tab.title.clone(), self.left_tab == LeftTab::Plugin(id), focused).on_click(cx.listener(move |this, _, _, cx| {
                this.left_tab = LeftTab::Plugin(id);
                crate::lua_bridge::send(graphyne_lua::ToLua::TabRefresh { tab: id }, cx);
                cx.notify();
            })));
        }
        let header = island_header().child(header).child(
            small_icon_button("left-add", Icon::Plus, if self.menu == Some(Menu::Plugins) { color(c::TEXT) } else { color(c::TEXT_DIM) })
                .tooltip(tooltip("Plugins & init.lua", None))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Plugins, cx))),
        );
        let body: AnyElement = match tab {
            LeftTab::Files => self.file_tree.clone().into_any_element(),
            LeftTab::Search => self.find.clone().into_any_element(),
            LeftTab::Git => self.git.clone().into_any_element(),
            LeftTab::History => self.history.clone().into_any_element(),
            LeftTab::Plugin(id) => self.render_plugin_tab(id, cx),
        };
        island().w(self.left_width).h_full().flex_none().child(header).child(div().flex_1().min_h(px(0.)).child(body))
    }

    fn render_right(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tab = self.right_tab;
        let editor = self.pane.read(cx).active_editor();
        let mut header = island_header();
        for (t, label) in [(RightTab::Structure, "Structure"), (RightTab::Secrets, "Secrets Guard")] {
            header = header.child(header_tab(SharedString::from(format!("right-{label}")), label, t == tab, false).on_click(cx.listener(move |this, _, _, cx| {
                this.right_tab = t;
                cx.notify();
            })));
        }
        for plugin in crate::lua_bridge::tabs(cx).into_iter().filter(|t| t.side == graphyne_lua::Side::Right) {
            let id = plugin.id;
            header = header.child(header_tab(SharedString::from(format!("right-plugin-{id}")), plugin.title.clone(), tab == RightTab::Plugin(id), false).on_click(cx.listener(move |this, _, _, cx| {
                this.right_tab = RightTab::Plugin(id);
                crate::lua_bridge::send(graphyne_lua::ToLua::TabRefresh { tab: id }, cx);
                cx.notify();
            })));
        }
        let body: AnyElement = match (tab, editor) {
            (RightTab::Plugin(id), _) => self.render_plugin_tab(id, cx),
            (_, None) => div().p(px(16.)).text_color(color(c::TEXT_DIM)).child("No file open").into_any_element(),
            (RightTab::Structure, Some(editor)) => {
                let outline = editor.read(cx).outline.clone();
                let cursor_row = editor.read(cx).cursor_row_col().0;
                let current = outline.iter().rposition(|i| i.byte_range.start <= editor.read(cx).buffer.char_to_byte(editor.read(cx).buffer.line_to_char(cursor_row)) && i.row <= cursor_row);
                div()
                    .id("structure")
                    .size_full()
                    .overflow_y_scroll()
                    .py(px(4.))
                    .children(outline.iter().enumerate().map(|(ix, item)| {
                        let row = item.row;
                        let editor = editor.clone();
                        div()
                            .id(ix)
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .h(px(24.))
                            .mx(px(4.))
                            .pl(px(8. + item.depth as f32 * 14.))
                            .rounded(px(5.))
                            .cursor_pointer()
                            .when(current == Some(ix), |d| d.bg(color(c::TAB_ACTIVE)))
                            .hover(|s| s.bg(color(c::HOVER)))
                            .on_click(move |_, window, cx| editor.update(cx, |e, cx| e.go_to(row, 0, window, cx)))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(16.))
                                    .rounded(px(4.))
                                    .bg(color(0x2E3446))
                                    .text_color(color(0x9BB5E8))
                                    .text_size(px(10.))
                                    .child(item.kind.badge()),
                            )
                            .child(div().text_size(px(theme::UI_TEXT)).text_color(color(c::TEXT)).child(item.name.clone()))
                            .child(div().text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child(format!(":{}", item.row + 1)))
                    }))
                    .into_any_element()
            }
            (RightTab::Secrets, Some(editor)) => {
                let secrets = editor.read(cx).secrets.clone();
                if secrets.is_empty() {
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(8.))
                        .pt(px(40.))
                        .text_color(color(c::TEXT_DIM))
                        .child(icon(Icon::Shield, 28., color(c::GREEN)))
                        .child("No leaked secrets detected in this file")
                        .into_any_element()
                } else {
                    div()
                        .flex()
                        .flex_col()
                        .p(px(8.))
                        .gap(px(4.))
                        .children(secrets.iter().map(|f| {
                            let (row, col) = (f.line, f.column);
                            let editor = editor.clone();
                            div()
                                .id(SharedString::from(format!("secret-{row}-{col}")))
                                .p(px(8.))
                                .rounded(px(6.))
                                .bg(color_a(c::YELLOW, 0.07))
                                .border_1()
                                .border_color(color_a(c::YELLOW, 0.25))
                                .cursor_pointer()
                                .hover(|s| s.bg(color_a(c::YELLOW, 0.12)))
                                .on_click(move |_, window, cx| editor.update(cx, |e, cx| e.go_to(row, col, window, cx)))
                                .child(div().flex().items_center().gap(px(6.)).text_color(color(c::YELLOW)).child(icon(Icon::Warning, 13., color(c::YELLOW))).child(f.kind))
                                .child(div().text_size(px(12.)).text_color(color(c::TEXT_MUTED)).child(format!("line {} · {}", row + 1, f.preview)))
                        }))
                        .into_any_element()
                }
            }
        };
        island()
            .w(self.right_width)
            .h_full()
            .flex_none()
            .font_family(theme::fonts().ui)
            .child(header)
            .child(div().flex_1().min_h(px(0.)).child(body))
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut crumbs: Vec<SharedString> = vec![self.project_name.clone()];
        let mut right: Vec<AnyElement> = Vec::new();
        let item = self.pane.read(cx).active_item().cloned();
        match item {
            Some(PaneItem::Editor(editor)) => {
                let e = editor.read(cx);
                if let Some(path) = &e.path {
                    if let Ok(rel) = path.strip_prefix(&self.root) {
                        for comp in rel.components() {
                            crumbs.push(comp.as_os_str().to_string_lossy().to_string().into());
                        }
                    }
                }
                for sym in e.breadcrumbs() {
                    crumbs.push(sym.into());
                }
                let (row, col) = e.cursor_row_col();
                let sel = e.selection_len();
                if let Some(err) = &e.save_error {
                    right.push(div().text_color(color(c::RED)).child(format!("Save failed: {err}")).into_any_element());
                }
                if !e.secrets.is_empty() {
                    right.push(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .text_color(color(c::YELLOW))
                            .child(icon(Icon::Shield, 12., color(c::YELLOW)))
                            .child(format!("{} secret(s)", e.secrets.len()))
                            .into_any_element(),
                    );
                }
                right.push(
                    div()
                        .child(if sel > 0 { format!("{}:{} ({sel} chars)", row + 1, col + 1) } else { format!("{}:{}", row + 1, col + 1) })
                        .into_any_element(),
                );
                if let crate::lsp_store::ServerStatus::Indexing { server, message } = self.lsp_store.read(cx).status_for(e.language) {
                    right.insert(
                        0,
                        div()
                            .max_w(px(420.))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(color(c::YELLOW))
                            .child(format!("{server}: {message}"))
                            .into_any_element(),
                    );
                }
                right.push(div().child(e.buffer.line_ending().label()).into_any_element());
                right.push(div().child("UTF-8").into_any_element());
                let lang = e.language;
                let toolchain = self.toolchains.get(&lang).cloned().flatten();
                right.push(div().child(toolchain.unwrap_or_else(|| lang.name().into())).into_any_element());
            }
            Some(PaneItem::Plugins(_)) => {
                crumbs.push("Plugins".into());
            }
            Some(PaneItem::Settings(_)) => {
                crumbs.push("Settings".into());
            }
            Some(PaneItem::Diff(d)) => {
                let d = d.read(cx);
                if let Ok(rel) = d.path.strip_prefix(&self.root) {
                    for comp in rel.components() {
                        crumbs.push(comp.as_os_str().to_string_lossy().to_string().into());
                    }
                }
                right.push(div().child("Diff with HEAD").into_any_element());
            }
            Some(PaneItem::Hex(h)) => {
                let h = h.read(cx);
                if let Ok(rel) = h.path.strip_prefix(&self.root) {
                    for comp in rel.components() {
                        crumbs.push(comp.as_os_str().to_string_lossy().to_string().into());
                    }
                }
                right.push(div().child(format!("0x{:08X}", h.cursor())).into_any_element());
                right.push(div().child(format!("{} bytes", h.len())).into_any_element());
                right.push(div().child("Binary").into_any_element());
            }
            None => {}
        }
        let n = crumbs.len();
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(theme::STATUS_BAR_HEIGHT))
            .px(px(12.))
            .gap(px(16.))
            .text_size(px(theme::UI_SMALL))
            .text_color(color(c::TEXT_DIM))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w(px(0.))
                    .overflow_hidden()
                    .children(crumbs.into_iter().enumerate().flat_map(|(i, s)| {
                        let mut v: Vec<AnyElement> = Vec::new();
                        if i > 0 {
                            v.push(div().text_color(color(c::TEXT_FAINT)).child("/").into_any_element());
                        }
                        v.push(div().whitespace_nowrap().when(i + 1 == n, |d| d.text_color(color(c::TEXT_MUTED))).child(s).into_any_element());
                        v
                    })),
            )
            .child(div().flex_1())
            .children(right)
    }

    // ---------------------------------------------------------------- Lua plugins

    /// Snapshot of the project and active editor for a Lua callback.
    fn lua_context(&self, cx: &App) -> graphyne_lua::EditorContext {
        let mut ctx = graphyne_lua::EditorContext { project_root: self.root.to_string_lossy().to_string(), ..Default::default() };
        if let Some(editor) = self.pane.read(cx).active_editor() {
            editor.read(cx).lua_context(&mut ctx);
        }
        ctx
    }

    fn lua_key(&mut self, action: &crate::lua_bridge::LuaKey, window: &mut Window, cx: &mut Context<Self>) {
        crate::lua_bridge::set_active(cx.entity().downgrade(), window.window_handle(), cx);
        let ctx = self.lua_context(cx);
        crate::lua_bridge::send(graphyne_lua::ToLua::Key { id: action.id, ctx }, cx);
    }

    pub fn redetect_languages(&mut self, cx: &mut Context<Self>) {
        let editors: Vec<_> = self.pane.read(cx).editors().cloned().collect();
        for e in editors {
            e.update(cx, |e, cx| e.redetect_language(cx));
        }
    }

    /// `graphyne.terminal.run`: the command runs in the named tab (or "Lua").
    pub fn run_lua_command(&mut self, command: &str, tab: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        if self.detached.is_none() {
            self.bottom = true;
        }
        self.bottom_view = BottomView::Terminal;
        let label = tab.unwrap_or("Lua").to_string();
        let line = format!("{command}\r");
        let cwd = self.root.clone();
        self.terminal.update(cx, |t, cx| t.run_in_tab(&label, cwd, &line, cx));
        self.terminal.read(cx).focus_active(window, cx);
        cx.notify();
    }

    pub fn apply_lua_edit(&mut self, edit: graphyne_lua::BufferEdit, cx: &mut Context<Self>) {
        match self.pane.read(cx).active_editor() {
            Some(editor) => editor.update(cx, |e, cx| e.lua_edit(edit, cx)),
            None => self.notify_user("Lua: no file is open to edit", true, cx),
        }
    }

    pub fn open_from_lua(&mut self, path: &str, line: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let p = PathBuf::from(path);
        let p = if p.is_absolute() { p } else { self.root.join(p) };
        self.open_file(p, line.map(|l| (l.saturating_sub(1), 0)), window, cx);
    }

    fn render_plugin_tab(&self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = crate::lua_bridge::tabs(cx).into_iter().find(|t| t.id == id) else {
            return div().p(px(16.)).text_color(color(c::TEXT_DIM)).child("This plugin tab is gone (init.lua was reloaded)").into_any_element();
        };
        div()
            .id(SharedString::from(format!("plugin-tab-{id}")))
            .size_full()
            .overflow_y_scroll()
            .py(px(4.))
            .when(tab.items.is_empty(), |d| d.child(div().p(px(12.)).text_color(color(c::TEXT_DIM)).child("Empty")))
            .children(tab.items.into_iter().enumerate().map(|(ix, item)| {
                let clickable = item.clickable;
                div()
                    .id(ix)
                    .flex()
                    .flex_col()
                    .justify_center()
                    .min_h(px(24.))
                    .py(px(3.))
                    .mx(px(4.))
                    .px(px(8.))
                    .rounded(px(5.))
                    .when(clickable, |d| {
                        d.cursor_pointer().hover(|s| s.bg(color(c::HOVER))).on_click(cx.listener(move |this, _, window, cx| {
                            crate::lua_bridge::set_active(cx.entity().downgrade(), window.window_handle(), cx);
                            let ctx = this.lua_context(cx);
                            crate::lua_bridge::send(graphyne_lua::ToLua::TabClick { tab: id, item: ix, ctx }, cx);
                        }))
                    })
                    .child(div().text_color(color(c::TEXT)).child(item.text))
                    .when_some(item.detail, |d, detail| d.child(div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child(detail)))
            }))
            .into_any_element()
    }

    fn toggle_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.show_notifications = false;
        self.menu = if self.menu == Some(menu) { None } else { Some(menu) };
        if self.menu == Some(Menu::Run) {
            let root = self.root.clone();
            cx.spawn(async move |this, cx| {
                let presets = cx
                    .background_spawn(async move {
                        graphyne_core::presets::applicable(&root)
                            .into_iter()
                            .map(|p| (p, graphyne_lsp::servers::which(p.program).is_some()))
                            .collect::<Vec<_>>()
                    })
                    .await;
                this.update(cx, |this, cx| {
                    this.run_presets = presets;
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        if self.menu == Some(Menu::Projects) {
            cx.spawn(async move |this, cx| {
                let recent = cx.background_spawn(async move { graphyne_core::recent::load() }).await;
                this.update(cx, |this, cx| {
                    this.recent = recent;
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    /// Projects menu (title-bar grid button) and main menu (gear).
    fn render_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let menu = self.menu?;
        let item = |id: &'static str, label: SharedString, detail: Option<SharedString>, shortcut: Option<&'static str>| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|s| s.bg(color(c::HOVER)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.))
                        .child(div().text_color(color(c::TEXT)).child(label))
                        .when_some(detail, |d, t| d.child(div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(t))),
                )
                .when_some(shortcut, |d, s| d.child(div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child(s)))
        };
        let section = |label: &'static str| div().px(px(10.)).pt(px(8.)).pb(px(2.)).text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child(label);
        let open_folder = item("menu-open", "Open Folder…".into(), None, None).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            crate::prompt_open_folder(cx);
            cx.notify();
        }));
        let close_project = item("menu-close-project", "Close Project".into(), Some("back to the Welcome screen".into()), None).on_click(cx.listener(|this, _, window, cx| {
            this.menu = None;
            this.close_project(window, cx);
        }));
        let panel = div()
            .id("menu")
            .occlude()
            .absolute()
            .top(px(theme::TITLE_BAR_HEIGHT - 2.))
            .w(px(320.))
            .max_h(px(460.))
            .overflow_y_scroll()
            .p(px(6.))
            .rounded(px(10.))
            .bg(color(c::POPUP))
            .border_1()
            .border_color(color(c::POPUP_BORDER))
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.menu = None;
                cx.notify();
            }));
        Some(match menu {
            Menu::Projects => {
                let current = self.root.clone();
                let recent: Vec<PathBuf> = self.recent.iter().filter(|p| **p != current).take(10).cloned().collect();
                panel
                    .left(px(110.))
                    .child(open_folder)
                    .child(close_project)
                    .child(section("RECENT PROJECTS"))
                    .when(recent.is_empty(), |d| d.child(div().px(px(10.)).py(px(6.)).text_color(color(c::TEXT_DIM)).child("No other recent projects")))
                    .children(recent.into_iter().enumerate().map(|(ix, path)| {
                        let name: SharedString = graphyne_core::project::project_name(&path).into();
                        let detail: SharedString = path.display().to_string().into();
                        item("menu-recent", name, Some(detail), None).id(("menu-recent", ix)).on_click(cx.listener(move |this, _, _, cx| {
                            this.menu = None;
                            crate::open_workspace(path.clone(), |_| {}, cx);
                            cx.notify();
                        }))
                    }))
            }
            Menu::Run => {
                let presets = self.run_presets.clone();
                panel
                    .right(px(214.))
                    .w(px(380.))
                    .child(item("menu-run-file", "Run Current File".into(), None, Some("Shift+F10")).on_click(cx.listener(|this, _, window, cx| {
                        this.menu = None;
                        this.run_active(window, cx);
                    })))
                    .child(section("AUDIT & REVERSE ENGINEERING"))
                    .when(presets.is_empty(), |d| d.child(div().px(px(10.)).py(px(6.)).text_color(color(c::TEXT_DIM)).child("Checking installed tools…")))
                    .children(presets.into_iter().enumerate().map(|(ix, (preset, installed))| {
                        let detail: SharedString = if installed {
                            if preset.needs_target { format!("{}<target>", preset.command).into() } else { preset.command.into() }
                        } else {
                            format!("not installed — {}", preset.install_hint).into()
                        };
                        let row = item("menu-preset", preset.label.into(), Some(detail), None).id(("menu-preset", ix));
                        if installed {
                            row.on_click(cx.listener(move |this, _, window, cx| {
                                this.menu = None;
                                this.run_preset(preset, window, cx);
                            }))
                        } else {
                            row.opacity(0.5).cursor_default()
                        }
                    }))
            }
            Menu::Plugins => {
                let tabs = crate::lua_bridge::tabs(cx);
                panel
                    .top(px(theme::TITLE_BAR_HEIGHT + theme::ISLAND_HEADER_HEIGHT))
                    .left(px(theme::GAP + 8.))
                    .child(item("menu-plugins-page", "Plugins…".into(), Some("installed extensions, permissions, theme".into()), None).on_click(cx.listener(|this, _, window, cx| {
                        this.menu = None;
                        this.pane.update(cx, |p, cx| p.open_plugins_page(window, cx));
                        cx.notify();
                    })))
                    .child(item("menu-init", "Open init.lua".into(), Some("~/.config/graphyne/init.lua — reloads on save".into()), None).on_click(cx.listener(|this, _, window, cx| {
                        this.menu = None;
                        match crate::lua_bridge::ensure_init_file(cx) {
                            Some(file) => this.open_file(file, None, window, cx),
                            None => this.notify_user("Could not create ~/.config/graphyne/init.lua", true, cx),
                        }
                        cx.notify();
                    })))
                    .child(item("menu-plugins-dir", "Open plugins folder".into(), None, None).on_click(cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        let dir = crate::lua_bridge::plugins_dir(cx);
                        cx.reveal_path(&dir);
                        cx.notify();
                    })))
                    .child(item("menu-lua-reload", "Reload plugins".into(), None, None).on_click(cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        crate::lua_bridge::send(graphyne_lua::ToLua::Reload, cx);
                        cx.notify();
                    })))
                    .child(section("PLUGIN TABS"))
                    .when(tabs.is_empty(), |d| d.child(div().px(px(10.)).py(px(6.)).text_color(color(c::TEXT_DIM)).child("None yet — graphyne.ui.register_tab in init.lua")))
                    .children(tabs.into_iter().enumerate().map(|(ix, t)| {
                        let side = if t.side == graphyne_lua::Side::Left { "left panel" } else { "right panel" };
                        let id = t.id;
                        let left = t.side == graphyne_lua::Side::Left;
                        item("menu-plugin-tab", t.title.clone(), Some(side.into()), None).id(("menu-plugin-tab", ix)).on_click(cx.listener(move |this, _, _, cx| {
                            this.menu = None;
                            if left {
                                this.left = true;
                                this.left_tab = LeftTab::Plugin(id);
                            } else {
                                this.right = true;
                                this.right_tab = RightTab::Plugin(id);
                            }
                            crate::lua_bridge::send(graphyne_lua::ToLua::TabRefresh { tab: id }, cx);
                            cx.notify();
                        }))
                    }))
            }
            Menu::App => panel
                .right(px(130.))
                .child(item("menu-settings", "Settings…".into(), None, Some("Ctrl+,")).on_click(cx.listener(|this, _, window, cx| {
                    this.menu = None;
                    this.pane.update(cx, |p, cx| p.open_settings_page(window, cx));
                    cx.notify();
                })))
                .child(open_folder)
                .child(close_project)
                .child(item("menu-plugins", "Plugins".into(), None, None).on_click(cx.listener(|this, _, window, cx| {
                    this.menu = None;
                    this.pane.update(cx, |p, cx| p.open_plugins_page(window, cx));
                    cx.notify();
                })))
                .child(item("menu-actions", "All Actions…".into(), None, Some("Ctrl+Shift+O")).on_click(cx.listener(|this, _, window, cx| {
                    this.menu = None;
                    this.toggle_search(window, cx);
                    cx.notify();
                })))
                .child(item("menu-about", "About Graphyne".into(), None, None).on_click(cx.listener(|this, _, _, cx| {
                    this.menu = None;
                    let msg = format!("Graphyne {} — Rust + GPUI. Extensions: Plugins page (main menu).", env!("CARGO_PKG_VERSION"));
                    this.notify_user(msg, false, cx);
                    this.show_notifications = true;
                    this.unread = false;
                    cx.notify();
                }))),
        })
    }

    fn render_notifications(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if !self.show_notifications {
            return None;
        }
        let items = self.notifications.clone();
        Some(
            div()
                .id("notifications")
                .occlude()
                .absolute()
                .top(px(theme::TITLE_BAR_HEIGHT))
                .right(px(60.))
                .w(px(360.))
                .max_h(px(420.))
                .overflow_y_scroll()
                .p(px(6.))
                .rounded(px(10.))
                .bg(color(c::POPUP))
                .border_1()
                .border_color(color(c::POPUP_BORDER))
                .shadow_lg()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.show_notifications = false;
                    cx.notify();
                }))
                .when(items.is_empty(), |d| d.child(div().p(px(12.)).text_color(color(c::TEXT_DIM)).child("No notifications")))
                .children(items.into_iter().map(|n| {
                    div()
                        .flex()
                        .gap(px(8.))
                        .p(px(8.))
                        .rounded(px(6.))
                        .hover(|s| s.bg(color(c::HOVER)))
                        .child(icon(if n.error { Icon::Warning } else { Icon::Bell }, 14., if n.error { color(c::YELLOW) } else { color(c::TEXT_MUTED) }))
                        .child(div().flex_1().text_color(color(c::TEXT)).child(n.text))
                })),
        )
    }
}

/// A title-bar button. It blocks the drag area under it, so pressing it is a click, not the
/// start of moving the window.
fn title_button(id: impl Into<gpui::ElementId>, ic: Icon, tint: gpui::Hsla) -> gpui::Stateful<gpui::Div> {
    icon_button(id, ic, tint).occlude()
}

/// Windows / Linux caption buttons (client-side decorations).
pub fn caption_buttons<V: 'static>(maximized: bool, _cx: &mut Context<V>) -> impl IntoElement + use<V> {
    let button = |id: &'static str, ic: Icon, area: WindowControlArea, close: bool| {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .w(px(46.))
            .h(px(theme::TITLE_BAR_HEIGHT))
            .window_control_area(area)
            // Block the parent Drag area's hitbox so the OS hit-test reports this button.
            .occlude()
            .when(close, |d| d.hover(|s| s.bg(color(c::CLOSE_HOVER))))
            .when(!close, |d| d.hover(|s| s.bg(color(c::HOVER))))
            .child(icon(ic, 16., color(c::TEXT_MUTED)))
            .when(!cfg!(windows), |d| {
                d.on_click(move |_, window, cx| match area {
                    WindowControlArea::Min => window.minimize_window(),
                    WindowControlArea::Max => window.zoom_window(),
                    WindowControlArea::Close => {
                        let _ = cx;
                        window.remove_window()
                    }
                    WindowControlArea::Drag => {}
                })
            })
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .child(button("win-min", Icon::WinMin, WindowControlArea::Min, false))
        .child(button("win-max", if maximized { Icon::WinRestore } else { Icon::WinMax }, WindowControlArea::Max, false))
        .child(button("win-close", Icon::WinClose, WindowControlArea::Close, true))
}

/// Minimal title bar used by secondary windows (detached terminal).
pub fn caption_bar<V: 'static>(title: SharedString, maximized: bool, cx: &mut Context<V>) -> impl IntoElement + use<V> {
    div()
        .id("caption-bar")
        .flex()
        .flex_none()
        .items_center()
        .h(px(theme::TITLE_BAR_HEIGHT))
        .pl(px(if cfg!(target_os = "macos") { 80. } else { 14. }))
        .occlude()
        .window_control_area(WindowControlArea::Drag)
        .child(icon(Icon::Terminal, 14., color(c::TEXT_MUTED)))
        .child(div().ml(px(8.)).text_size(px(theme::UI_TEXT)).text_color(color(c::TEXT_MUTED)).child(title))
        .child(div().flex_1())
        .when(!cfg!(target_os = "macos"), |d| d.child(caption_buttons(maximized, cx)))
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let debug_view = self.bottom_view == BottomView::Debug && self.debugger.read(cx).session.is_some();
        let show_bottom = self.bottom && (self.detached.is_none() || debug_view);
        let title_bar = self.render_title_bar(window, cx);
        let left = self.left.then(|| self.render_left(window, cx));
        let right = self.right.then(|| self.render_right(cx));
        let status = self.render_status_bar(cx);
        let notifications = self.render_notifications(cx);
        let menu = self.render_menu(cx);

        let center = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .child(div().flex_1().min_h(px(0.)).child(self.pane.clone()))
            .when(show_bottom, |d| {
                let body: AnyElement = if debug_view { self.debug_panel.clone().into_any_element() } else { self.terminal.clone().into_any_element() };
                d.child(self.splitter(Splitter::Bottom, cx)).child(div().flex_none().h(self.bottom_height).child(body))
            });

        let islands = div()
            .flex()
            .flex_1()
            .min_h(px(0.))
            .px(theme::gap())
            .when_some(left, |d, l| d.child(l).child(self.splitter(Splitter::Left, cx)))
            .child(center)
            .when_some(right, |d, r| d.child(self.splitter(Splitter::Right, cx)).child(r));

        let search_overlay = self.search.as_ref().map(|(view, _)| {
            div()
                .id("search-scrim")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(90.))
                .bg(gpui::hsla(0., 0., 0., 0.38))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                    this.search = None;
                    this.pane.read(cx).focus_active(window, cx);
                    cx.notify();
                }))
                .child(div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(view.clone()))
        });

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(color(c::CANVAS))
            .font_family(theme::fonts().ui)
            .text_size(px(theme::UI_TEXT))
            .text_color(color(c::TEXT))
            .on_action(cx.listener(Self::toggle_left))
            .on_action(cx.listener(Self::toggle_right))
            .on_action(cx.listener(Self::toggle_bottom))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.pane.update(cx, |p, cx| p.open_settings_page(window, cx))))
            .on_action(cx.listener(Self::focus_terminal))
            .on_action(cx.listener(Self::focus_editor))
            .on_action(cx.listener(Self::new_terminal))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::prev_tab))
            .on_action(cx.listener(Self::save_all))
            .on_action(cx.listener(Self::find_in_files))
            .on_action(cx.listener(Self::quit))
            .on_action(cx.listener(Self::search_action))
            .on_action(cx.listener(Self::run_action))
            .on_action(cx.listener(Self::detach_action))
            .on_action(cx.listener(Self::debug_active))
            .on_action(cx.listener(Self::debug_resume))
            .on_action(cx.listener(Self::debug_step_over))
            .on_action(cx.listener(Self::debug_step_into))
            .on_action(cx.listener(Self::debug_step_out))
            .on_action(cx.listener(Self::debug_stop))
            .on_action(cx.listener(Self::lua_key))
            .on_modifiers_changed(cx.listener(Self::on_modifiers))
            .capture_key_down(cx.listener(|this, _, _, _| this.last_shift = None))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.dragging = None))
            .child(title_bar)
            .child(islands)
            .child(status)
            .children(search_overlay)
            .children(notifications)
            .children(menu)
    }
}

/// `Some(true)` = git metadata changed (HEAD / index / refs), `Some(false)` = project file,
/// `None` = noise we ignore (build output, object store, our own atomic-save temp files).
fn classify_fs_change(root: &std::path::Path, path: &std::path::Path) -> Option<bool> {
    let rel = path.strip_prefix(root).ok()?;
    let mut comps = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string());
    let first = comps.next()?;
    if first == ".git" {
        return match comps.next().as_deref() {
            Some("HEAD") | Some("index") | Some("refs") | Some("packed-refs") => Some(true),
            _ => None,
        };
    }
    const NOISE: &[&str] = &["target", "node_modules", ".venv", "venv", "__pycache__", ".idea", ".gradle", "dist", "build"];
    if rel.components().any(|c| NOISE.contains(&c.as_os_str().to_string_lossy().as_ref())) {
        return None;
    }
    let name = path.file_name()?.to_string_lossy();
    if name.ends_with(".graphyne-tmp") {
        return None;
    }
    Some(false)
}
