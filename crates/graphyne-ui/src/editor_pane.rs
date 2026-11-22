//! Center-top island: open-file tabs + the active editor / hex view.

use crate::assets::Icon;
use crate::components::{icon, island, island_header, kbd, tooltip};
use crate::database::{ConsoleEvent, DbConsole, DbStore, DbTableEvent, DbTableView};
use crate::diff_view::{DiffEvent, DiffView, HunkSelection};
use crate::editor::{Editor, EditorEvent};
use crate::hex_view::HexView;
use crate::plugins_page::PluginsPage;
use crate::settings_page::SettingsPage;
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, ScrollHandle, SharedString, Subscription, Task, Window, div, px};
use graphyne_core::session::{TabKind, TabState};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub enum PaneItem {
    Editor(Entity<Editor>),
    Hex(Entity<HexView>),
    /// Side-by-side comparison with HEAD (opened from the Git tab).
    Diff(Entity<DiffView>),
    /// Installed extensions, permissions and theme.
    Plugins(Entity<PluginsPage>),
    /// Graphyne's own settings.
    Settings(Entity<SettingsPage>),
    /// A database connection's SQL console.
    DbConsole(Entity<DbConsole>),
    /// The rows of a database table.
    DbTable(Entity<DbTableView>),
}

impl PaneItem {
    fn path(&self, cx: &App) -> Option<PathBuf> {
        match self {
            PaneItem::Editor(e) => e.read(cx).path.clone(),
            PaneItem::Hex(h) => Some(h.read(cx).path.clone()),
            // A diff tab never stands in for the file itself.
            PaneItem::Diff(_) | PaneItem::Plugins(_) | PaneItem::Settings(_) | PaneItem::DbConsole(_) | PaneItem::DbTable(_) => None,
        }
    }
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            PaneItem::Editor(e) => e.focus_handle(cx),
            PaneItem::Hex(h) => h.focus_handle(cx),
            PaneItem::Diff(d) => d.focus_handle(cx),
            PaneItem::Plugins(p) => p.focus_handle(cx),
            PaneItem::Settings(p) => p.focus_handle(cx),
            PaneItem::DbConsole(c) => c.focus_handle(cx),
            PaneItem::DbTable(t) => t.focus_handle(cx),
        }
    }
}

pub enum PaneEvent {
    Saved,
    Changed,
    OpenLocation(PathBuf, usize, usize),
    ApplyEdits(Vec<graphyne_lsp::model::FileEdits>),
    Notify(String),
}

pub struct EditorPane {
    focus_handle: FocusHandle,
    items: Vec<PaneItem>,
    active: usize,
    root: PathBuf,
    lsp_store: Entity<crate::lsp_store::LspStore>,
    pub debugger: Option<Entity<crate::debugger::Debugger>>,
    /// Blocks left out of the next commit (shared with the Git tab).
    pub hunk_excluded: HunkSelection,
    subs: Vec<Subscription>,
    pub error: Option<String>,
    /// The tab strip scrolls sideways when the tabs don't fit.
    tab_scroll: ScrollHandle,
    /// (active tab, tab count) last scrolled into view.
    scrolled_to: Option<(usize, usize)>,
    /// The list of all open tabs (the `▾` at the right of the tab strip).
    tab_list_open: bool,
    /// When the list was closed by a click outside it (that click mustn't reopen it).
    tab_list_closed_at: Option<std::time::Instant>,
    /// Files being loaded → whether to focus them when they open (a double click in the
    /// tree arrives while the single click's load is still running: no second tab).
    opening: HashMap<PathBuf, bool>,
    /// The project's database connections (console and table tabs use them).
    pub db: Option<Entity<DbStore>>,
}

/// What a tab shows: icon kind, title, full path for the tooltip / list, unsaved changes.
struct TabInfo {
    title: String,
    kind: TabIcon,
    path: Option<PathBuf>,
    dirty: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum TabIcon {
    File,
    Diff,
    Plugins,
    Settings,
    Console,
    Table,
}

impl EventEmitter<PaneEvent> for EditorPane {}

impl Focusable for EditorPane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.items.get(self.active).map(|i| i.focus_handle(cx)).unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl EditorPane {
    pub fn new(root: PathBuf, lsp_store: Entity<crate::lsp_store::LspStore>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            items: Vec::new(),
            active: 0,
            root,
            lsp_store,
            debugger: None,
            hunk_excluded: HunkSelection::default(),
            subs: Vec::new(),
            error: None,
            tab_scroll: ScrollHandle::new(),
            scrolled_to: None,
            tab_list_open: false,
            tab_list_closed_at: None,
            opening: HashMap::new(),
            db: None,
        }
    }

    /// The open editor for `path`, if any.
    pub fn editor_for(&self, path: &Path, cx: &App) -> Option<Entity<Editor>> {
        let key = graphyne_lsp::convert::path_key(path);
        self.editors().find(|e| e.read(cx).path.as_deref().is_some_and(|p| graphyne_lsp::convert::path_key(p) == key)).cloned()
    }

    pub fn active_item(&self) -> Option<&PaneItem> {
        self.items.get(self.active)
    }

    pub fn active_editor(&self) -> Option<Entity<Editor>> {
        match self.items.get(self.active) {
            Some(PaneItem::Editor(e)) => Some(e.clone()),
            _ => None,
        }
    }

    pub fn editors(&self) -> impl Iterator<Item = &Entity<Editor>> {
        self.items.iter().filter_map(|i| match i {
            PaneItem::Editor(e) => Some(e),
            _ => None,
        })
    }

    /// Focus the active tab, or the pane itself when empty (keeps the Workspace key
    /// context alive so global shortcuts keep working).
    pub fn focus_active(&self, window: &mut Window, cx: &App) {
        match self.items.get(self.active) {
            Some(item) => window.focus(&item.focus_handle(cx)),
            None => window.focus(&self.focus_handle),
        }
    }

    /// Open (or re-activate and refresh) the side-by-side diff of `path` against HEAD.
    pub fn open_diff(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let key = dunce_like(&path);
        let existing = self.items.iter().position(|i| matches!(i, PaneItem::Diff(d) if dunce_like(&d.read(cx).path) == key));
        match existing {
            Some(ix) => {
                self.active = ix;
                if let PaneItem::Diff(d) = &self.items[ix] {
                    d.update(cx, |d, cx| d.refresh(cx));
                }
            }
            None => {
                let view = self.new_diff_view(path, cx);
                let at = (self.active + 1).min(self.items.len());
                self.items.insert(at, PaneItem::Diff(view));
                self.active = at;
            }
        }
        self.focus_active(window, cx);
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    /// Open (or focus) the Plugins page.
    pub fn open_plugins_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.items.iter().position(|i| matches!(i, PaneItem::Plugins(_))) {
            Some(ix) => self.active = ix,
            None => {
                let page = cx.new(PluginsPage::new);
                let at = (self.active + 1).min(self.items.len());
                self.items.insert(at, PaneItem::Plugins(page));
                self.active = at;
            }
        }
        self.focus_active(window, cx);
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    /// Open (or focus) the Settings page.
    pub fn open_settings_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.items.iter().position(|i| matches!(i, PaneItem::Settings(_))) {
            Some(ix) => self.active = ix,
            None => {
                let page = cx.new(SettingsPage::new);
                let at = (self.active + 1).min(self.items.len());
                self.items.insert(at, PaneItem::Settings(page));
                self.active = at;
            }
        }
        self.focus_active(window, cx);
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    /// Open (or focus) the SQL console of connection `db_id`; `sql` is added as its last line.
    pub fn open_db_console(&mut self, db_id: String, sql: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = self.db.clone() else { return };
        if let Some(ix) = self.items.iter().position(|i| matches!(i, PaneItem::DbConsole(c) if c.read(cx).db_id == db_id)) {
            self.active = ix;
            if let (Some(sql), PaneItem::DbConsole(console)) = (sql, &self.items[ix]) {
                let editor = console.read(cx).editor.clone();
                editor.update(cx, |e, cx| e.append_line(&sql, window, cx));
            } else {
                self.focus_active(window, cx);
            }
            cx.emit(PaneEvent::Changed);
            cx.notify();
            return;
        }
        let Some(path) = store.read(cx).console_file(&db_id) else { return };
        let name = store.read(cx).name(&db_id);
        cx.spawn_in(window, async move |this, cx| {
            let file = path.clone();
            let loaded = cx
                .background_spawn(async move {
                    if !file.exists() {
                        if let Some(dir) = file.parent() {
                            std::fs::create_dir_all(dir)?;
                        }
                        std::fs::write(&file, format!("-- {name}: Ctrl+Enter runs the statement under the cursor, or the selection.\n\n"))?;
                    }
                    load_file(&file)
                })
                .await;
            this.update_in(cx, |this, window, cx| match loaded {
                Ok(Some(buffer)) => {
                    let console = this.new_db_console(db_id, path, buffer, cx);
                    let at = (this.active + 1).min(this.items.len());
                    this.items.insert(at, PaneItem::DbConsole(console.clone()));
                    this.active = at;
                    let editor = console.read(cx).editor.clone();
                    match sql {
                        Some(sql) => editor.update(cx, |e, cx| e.append_line(&sql, window, cx)),
                        None => {
                            // Start below the last line, ready to type.
                            let end = editor.read(cx).buffer.len_lines().saturating_sub(1);
                            editor.update(cx, |e, cx| e.go_to(end, 0, window, cx));
                        }
                    }
                    cx.emit(PaneEvent::Changed);
                    cx.notify();
                }
                Ok(None) => this.error = Some(format!("{} is not a text file", path.display())),
                Err(e) => this.error = Some(format!("{e:#}")),
            })
            .ok();
        })
        .detach();
    }

    fn new_db_console(&mut self, db_id: String, path: PathBuf, buffer: graphyne_core::Buffer, cx: &mut Context<Self>) -> Entity<DbConsole> {
        let store = self.db.clone().expect("database store");
        // No language server for consoles: highlighting and word completion are enough here.
        let editor = cx.new(|cx| Editor::new(Some(path), buffer, self.root.clone(), None, None, cx));
        let console = cx.new(|cx| DbConsole::new(db_id, store, editor, cx));
        self.subs.push(cx.subscribe(&console, |_, _, ev: &ConsoleEvent, cx| match ev {
            ConsoleEvent::Notify(m) => cx.emit(PaneEvent::Notify(m.clone())),
        }));
        console
    }

    /// Connection `db_id` was removed: close its console and table tabs (the console text is
    /// saved first, as on any close).
    pub fn close_db_tabs(&mut self, db_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.items.get(self.active).map(|i| i.focus_handle(cx));
        let before = self.items.len();
        self.items.retain(|item| match item {
            PaneItem::DbConsole(c) => {
                if c.read(cx).db_id != db_id {
                    return true;
                }
                let editor = c.read(cx).editor.clone();
                if let Some(task) = editor.update(cx, |e, cx| e.save(cx)) {
                    task.detach();
                }
                false
            }
            PaneItem::DbTable(t) => t.read(cx).db_id != db_id,
            _ => true,
        });
        if self.items.len() == before {
            return;
        }
        self.active = active.and_then(|f| self.items.iter().position(|i| i.focus_handle(cx) == f)).unwrap_or(self.active.min(self.items.len().saturating_sub(1)));
        self.focus_active(window, cx);
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    /// Open (or focus and refresh) the rows of `table` of connection `db_id`.
    pub fn open_db_table(&mut self, db_id: String, schema: Option<String>, table: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = self.db.clone() else { return };
        match self.items.iter().position(|i| matches!(i, PaneItem::DbTable(t) if t.read(cx).db_id == db_id && t.read(cx).schema == schema && t.read(cx).table == table)) {
            Some(ix) => {
                self.active = ix;
                if let PaneItem::DbTable(t) = &self.items[ix] {
                    t.update(cx, |t, cx| t.load(cx));
                }
            }
            None => {
                let view = self.new_db_table(db_id, schema, table, store, window, cx);
                let at = (self.active + 1).min(self.items.len());
                self.items.insert(at, PaneItem::DbTable(view));
                self.active = at;
            }
        }
        self.focus_active(window, cx);
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    fn new_db_table(&mut self, db_id: String, schema: Option<String>, table: String, store: Entity<DbStore>, window: &mut Window, cx: &mut Context<Self>) -> Entity<DbTableView> {
        let view = cx.new(|cx| DbTableView::new(db_id, schema, table, store, cx));
        self.subs.push(cx.subscribe_in(&view, window, |this, _, ev: &DbTableEvent, window, cx| match ev {
            DbTableEvent::OpenConsole(db, sql) => this.open_db_console(db.clone(), Some(sql.clone()), window, cx),
            DbTableEvent::Notify(m) => cx.emit(PaneEvent::Notify(m.clone())),
        }));
        view
    }

    pub fn diff_views(&self) -> impl Iterator<Item = &Entity<DiffView>> {
        self.items.iter().filter_map(|i| match i {
            PaneItem::Diff(d) => Some(d),
            _ => None,
        })
    }

    /// Open (or activate) a file; `at` = (row, col) to reveal.
    pub fn open(&mut self, path: PathBuf, at: Option<(usize, usize)>, window: &mut Window, cx: &mut Context<Self>) {
        self.open_with_focus(path, at, true, window, cx);
    }

    /// [`Self::open`]; with `focus` false the keyboard stays where it is (a single click in
    /// the project tree previews the file, so F2 / Delete still go to the tree).
    pub fn open_with_focus(&mut self, path: PathBuf, at: Option<(usize, usize)>, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        let canonical = dunce_like(&path);
        if let Some(ix) = self.items.iter().position(|i| i.path(cx).as_deref().map(dunce_like) == Some(canonical.clone())) {
            self.active = ix;
            if let (Some((row, col)), PaneItem::Editor(e)) = (at, &self.items[ix]) {
                e.update(cx, |e, cx| e.go_to(row, col, window, cx));
            } else if focus {
                self.focus_active(window, cx);
            }
            cx.emit(PaneEvent::Changed);
            cx.notify();
            return;
        }
        if let Some(pending) = self.opening.get_mut(&canonical) {
            *pending |= focus;
            return;
        }
        self.opening.insert(canonical.clone(), focus);
        cx.spawn_in(window, async move |this, cx| {
            let p = path.clone();
            let loaded = cx.background_spawn(async move { load_file(&p) }).await;
            this.update_in(cx, |this, window, cx| {
                let focus = this.opening.remove(&canonical).unwrap_or(true);
                match loaded {
                    Ok(Some(buffer)) => {
                        let editor = this.new_editor(path.clone(), buffer, cx);
                        this.items.insert((this.active + 1).min(this.items.len()), PaneItem::Editor(editor.clone()));
                        this.active = (this.active + 1).min(this.items.len() - 1);
                        if this.items.len() == 1 {
                            this.active = 0;
                        }
                        match at {
                            Some((row, col)) => editor.update(cx, |e, cx| e.go_to(row, col, window, cx)),
                            None if focus => window.focus(&editor.focus_handle(cx)),
                            None => {}
                        }
                    }
                    Ok(None) => {
                        let hex = cx.new(|cx| HexView::new(path.clone(), cx));
                        this.items.insert((this.active + 1).min(this.items.len()), PaneItem::Hex(hex.clone()));
                        this.active = (this.active + 1).min(this.items.len() - 1);
                        if this.items.len() == 1 {
                            this.active = 0;
                        }
                        if focus {
                            window.focus(&hex.focus_handle(cx));
                        }
                    }
                    Err(err) => this.error = Some(format!("{err:#}")),
                }
                cx.emit(PaneEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn new_editor(&mut self, path: PathBuf, buffer: graphyne_core::Buffer, cx: &mut Context<Self>) -> Entity<Editor> {
        let (root, store, debugger) = (self.root.clone(), self.lsp_store.clone(), self.debugger.clone());
        let editor = cx.new(|cx| Editor::new(Some(path), buffer, root, Some(store), debugger, cx));
        self.subs.push(cx.subscribe(&editor, |_, _, ev, cx| match ev {
            EditorEvent::Saved => cx.emit(PaneEvent::Saved),
            EditorEvent::Edited => cx.emit(PaneEvent::Changed),
            EditorEvent::OpenLocation(p, l, c) => cx.emit(PaneEvent::OpenLocation(p.clone(), *l, *c)),
            EditorEvent::ApplyEdits(e) => cx.emit(PaneEvent::ApplyEdits(e.clone())),
            EditorEvent::Notify(m) => cx.emit(PaneEvent::Notify(m.clone())),
        }));
        editor
    }

    fn new_diff_view(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Entity<DiffView> {
        let excluded = self.hunk_excluded.clone();
        let view = cx.new(|cx| DiffView::new(path, excluded, cx));
        self.subs.push(cx.subscribe(&view, |_, _, ev, cx| match ev {
            DiffEvent::OpenFile(p) => cx.emit(PaneEvent::OpenLocation(p.clone(), 0, 0)),
            DiffEvent::SelectionChanged => cx.emit(PaneEvent::Changed),
        }));
        view
    }

    /// Open tabs for the session: each with its cursor and scroll, plus the active index.
    pub fn tab_states(&self, cx: &App) -> (Vec<TabState>, usize) {
        let tabs = self
            .items
            .iter()
            .map(|item| match item {
                PaneItem::Editor(e) => {
                    let e = e.read(cx);
                    let (cursor, anchor, scroll_line, scroll_x) = e.view_state();
                    TabState { kind: TabKind::File, path: e.path.clone(), cursor, anchor, scroll_line, scroll_x, folds: e.folded_rows(), ..Default::default() }
                }
                PaneItem::Hex(h) => TabState { kind: TabKind::File, path: Some(h.read(cx).path.clone()), ..Default::default() },
                PaneItem::Diff(d) => TabState { kind: TabKind::Diff, path: Some(d.read(cx).path.clone()), ..Default::default() },
                PaneItem::Plugins(_) => TabState { kind: TabKind::Plugins, ..Default::default() },
                PaneItem::Settings(_) => TabState { kind: TabKind::Settings, ..Default::default() },
                PaneItem::DbConsole(c) => {
                    let c = c.read(cx);
                    let e = c.editor.read(cx);
                    let (cursor, anchor, scroll_line, scroll_x) = e.view_state();
                    TabState { kind: TabKind::DbConsole, path: e.path.clone(), cursor, anchor, scroll_line, scroll_x, db: Some(c.db_id.clone()), ..Default::default() }
                }
                PaneItem::DbTable(t) => {
                    let t = t.read(cx);
                    TabState { kind: TabKind::DbTable, db: Some(t.db_id.clone()), schema: t.schema.clone(), table: Some(t.table.clone()), ..Default::default() }
                }
            })
            .collect();
        (tabs, self.active)
    }

    /// Reopen the tabs of the last session in their order, with cursors and scroll
    /// positions. Files are read off the UI thread; ones that are gone are skipped.
    pub fn restore(&mut self, tabs: Vec<TabState>, active: usize, window: &mut Window, cx: &mut Context<Self>) {
        if tabs.is_empty() {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let paths: Vec<Option<PathBuf>> = tabs.iter().map(|t| t.path.clone().filter(|_| matches!(t.kind, TabKind::File | TabKind::DbConsole))).collect();
            let loaded: Vec<Option<anyhow::Result<Option<graphyne_core::Buffer>>>> =
                cx.background_spawn(async move { paths.into_iter().map(|p| p.filter(|p| p.is_file()).map(|p| load_file(&p))).collect() }).await;
            this.update_in(cx, |this, window, cx| {
                let mut restored = Vec::new();
                let mut new_active = 0;
                for (ix, (tab, file)) in tabs.into_iter().zip(loaded).enumerate() {
                    // Database tabs come back only while their connection still exists.
                    let db = tab.db.clone().filter(|id| this.db.as_ref().is_some_and(|s| s.read(cx).entry(id).is_some()));
                    let table = tab.table.clone();
                    let schema = tab.schema.clone();
                    let item = match (tab.kind, tab.path, file) {
                        (TabKind::DbConsole, Some(path), Some(Ok(Some(buffer)))) if db.is_some() => {
                            let console = this.new_db_console(db.unwrap_or_default(), path, buffer, cx);
                            let editor = console.read(cx).editor.clone();
                            editor.update(cx, |e, cx| e.restore_view(tab.cursor, tab.anchor, tab.scroll_line, tab.scroll_x, cx));
                            PaneItem::DbConsole(console)
                        }
                        (TabKind::DbTable, _, _) if db.is_some() && table.is_some() => {
                            let store = this.db.clone().expect("checked above");
                            PaneItem::DbTable(this.new_db_table(db.unwrap_or_default(), schema, table.unwrap_or_default(), store, window, cx))
                        }
                        (TabKind::File, Some(path), Some(Ok(Some(buffer)))) => {
                            let editor = this.new_editor(path, buffer, cx);
                            editor.update(cx, |e, cx| {
                                e.set_folded_rows(tab.folds, cx);
                                e.restore_view(tab.cursor, tab.anchor, tab.scroll_line, tab.scroll_x, cx);
                            });
                            PaneItem::Editor(editor)
                        }
                        (TabKind::File, Some(path), Some(Ok(None))) => PaneItem::Hex(cx.new(|cx| HexView::new(path, cx))),
                        (TabKind::Diff, Some(path), _) if path.is_file() => PaneItem::Diff(this.new_diff_view(path, cx)),
                        (TabKind::Plugins, _, _) => PaneItem::Plugins(cx.new(PluginsPage::new)),
                        (TabKind::Settings, _, _) => PaneItem::Settings(cx.new(SettingsPage::new)),
                        _ => continue,
                    };
                    if ix <= active {
                        new_active = restored.len();
                    }
                    restored.push(item);
                }
                // Anything opened meanwhile (e.g. from the command line) stays after them.
                let opened_meanwhile = !this.items.is_empty();
                restored.append(&mut this.items);
                this.items = restored;
                if !opened_meanwhile {
                    this.active = new_active.min(this.items.len().saturating_sub(1));
                    this.focus_active(window, cx);
                }
                cx.emit(PaneEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.items.len() {
            return;
        }
        let editor = match &self.items[ix] {
            PaneItem::Editor(e) => Some(e.clone()),
            PaneItem::DbConsole(c) => Some(c.read(cx).editor.clone()),
            _ => None,
        };
        if let Some(task) = editor.and_then(|e| e.update(cx, |e, cx| e.save(cx))) {
            task.detach();
        }
        self.items.remove(ix);
        if self.active > ix || self.active >= self.items.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.focus_active(window, cx);
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    /// A file or folder was renamed / moved on disk: tabs of it (or of files inside it) follow.
    pub fn paths_moved(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        for item in &self.items {
            match item {
                PaneItem::Editor(e) => {
                    if let Some(new) = e.read(cx).path.as_deref().and_then(|p| moved_path(p, from, to)) {
                        e.update(cx, |e, cx| e.set_path(new, cx));
                    }
                }
                PaneItem::Hex(h) => {
                    if let Some(new) = moved_path(&h.read(cx).path, from, to) {
                        h.update(cx, |h, cx| {
                            h.path = new;
                            cx.notify();
                        });
                    }
                }
                PaneItem::Diff(d) => {
                    if let Some(new) = moved_path(&d.read(cx).path, from, to) {
                        d.update(cx, |d, cx| {
                            d.path = new;
                            cx.notify();
                        });
                    }
                }
                PaneItem::Plugins(_) | PaneItem::Settings(_) | PaneItem::DbConsole(_) | PaneItem::DbTable(_) => {}
            }
        }
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    /// A file or folder went to the Recycle Bin: close its tabs (and those of files inside
    /// it) without saving — saving would bring the file back.
    pub fn paths_deleted(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.items.len();
        let active = self.items.get(self.active).map(|i| i.focus_handle(cx));
        self.items.retain(|item| {
            let p = match item {
                PaneItem::Editor(e) => e.read(cx).path.clone(),
                PaneItem::Hex(h) => Some(h.read(cx).path.clone()),
                PaneItem::Diff(d) => Some(d.read(cx).path.clone()),
                PaneItem::Plugins(_) | PaneItem::Settings(_) | PaneItem::DbConsole(_) | PaneItem::DbTable(_) => None,
            };
            !p.is_some_and(|p| moved_path(&p, path, path).is_some())
        });
        if self.items.len() == before {
            return;
        }
        // Keep the same tab active when it survived, else the one now at its place.
        self.active = active
            .and_then(|f| self.items.iter().position(|i| i.focus_handle(cx) == f))
            .unwrap_or(self.active.min(self.items.len().saturating_sub(1)));
        self.focus_active(window, cx);
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    /// File > Close All Tabs (editors are saved first, as on a single close).
    pub fn close_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for item in &self.items {
            if let PaneItem::Editor(e) = item {
                if let Some(task) = e.update(cx, |e, cx| e.save(cx)) {
                    task.detach();
                }
            }
        }
        self.items.clear();
        self.active = 0;
        self.focus_active(window, cx);
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    /// After Save As onto a file that is open in another tab: that tab goes (unsaved — the
    /// file now holds this editor's text), `editor` stays active.
    pub fn close_duplicates(&mut self, editor: &Entity<Editor>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = editor.read(cx).path.clone() else { return };
        let key = dunce_like(&path).to_string_lossy().to_lowercase();
        let before = self.items.len();
        self.items.retain(|item| match item {
            PaneItem::Editor(e) if e == editor => true,
            _ => item.path(cx).is_none_or(|p| dunce_like(&p).to_string_lossy().to_lowercase() != key),
        });
        if self.items.len() != before {
            self.active = self.items.iter().position(|i| matches!(i, PaneItem::Editor(e) if e == editor)).unwrap_or(0);
            self.focus_active(window, cx);
        }
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    pub fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close(self.active, window, cx);
    }

    pub fn activate_delta(&mut self, d: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.items.is_empty() {
            return;
        }
        let n = self.items.len() as isize;
        self.active = ((self.active as isize + d).rem_euclid(n)) as usize;
        self.focus_active(window, cx);
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    pub fn save_all(&mut self, cx: &mut Context<Self>) -> Vec<Task<()>> {
        let editors: Vec<Entity<Editor>> = self.editors().cloned().collect();
        editors.into_iter().filter_map(|e| e.update(cx, |e, cx| e.save(cx))).collect()
    }

    fn tab_info(&self, item: &PaneItem, cx: &App) -> TabInfo {
        match item {
            PaneItem::Editor(e) => {
                let e = e.read(cx);
                TabInfo { title: e.title(), kind: TabIcon::File, path: e.path.clone(), dirty: e.is_dirty() }
            }
            PaneItem::Hex(h) => {
                let h = h.read(cx);
                TabInfo { title: h.title(), kind: TabIcon::File, path: Some(h.path.clone()), dirty: false }
            }
            PaneItem::Diff(d) => {
                let d = d.read(cx);
                TabInfo { title: d.title().to_string(), kind: TabIcon::Diff, path: Some(d.path.clone()), dirty: false }
            }
            PaneItem::Plugins(_) => TabInfo { title: "Plugins".into(), kind: TabIcon::Plugins, path: None, dirty: false },
            PaneItem::Settings(_) => TabInfo { title: "Settings".into(), kind: TabIcon::Settings, path: None, dirty: false },
            PaneItem::DbConsole(c) => TabInfo { title: c.read(cx).title(cx), kind: TabIcon::Console, path: None, dirty: false },
            PaneItem::DbTable(t) => TabInfo { title: t.read(cx).title(), kind: TabIcon::Table, path: None, dirty: false },
        }
    }

    /// A path for people: relative to the project when inside it.
    fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root).map(|r| r.display().to_string()).unwrap_or_else(|_| path.display().to_string())
    }

    /// How many tabs are (partly) outside the visible strip, by the last layout.
    fn hidden_tab_count(&self) -> usize {
        let visible = self.tab_scroll.bounds();
        if self.tab_scroll.max_offset().width <= px(0.5) || visible.size.width <= px(0.) {
            return 0;
        }
        let offset = self.tab_scroll.offset().x;
        (0..self.items.len())
            .filter(|ix| {
                self.tab_scroll
                    .bounds_for_item(*ix)
                    .is_some_and(|b| b.left() + offset < visible.left() - px(1.) || b.right() + offset > visible.right() + px(1.))
            })
            .count()
    }

    /// Every open tab with its folder; click to switch.
    fn render_tab_list(&self, infos: &[TabInfo], cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let rows: Vec<_> = infos
            .iter()
            .enumerate()
            .map(|(ix, info)| {
                let is_active = ix == self.active;
                let folder: SharedString = info
                    .path
                    .as_ref()
                    .and_then(|p| p.parent())
                    .map(|d| self.display_path(d))
                    .filter(|d| !d.is_empty())
                    .unwrap_or_default()
                    .into();
                let group = SharedString::from(format!("tab-list-{ix}"));
                div()
                    .id(("tab-list-item", ix))
                    .group(group.clone())
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(30.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(is_active, |d| d.bg(color(c::TAB_ACTIVE)))
                    .when(!is_active, |d| d.hover(|s| s.bg(color(c::HOVER))))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.active = ix;
                        this.tab_list_open = false;
                        this.focus_active(window, cx);
                        cx.emit(PaneEvent::Changed);
                        cx.notify();
                    }))
                    .child(tab_icon(info))
                    .child(div().flex_1().min_w(px(0.)).whitespace_nowrap().overflow_hidden().text_color(color(c::TEXT)).child(short_name(&info.title, 44)))
                    .child(div().flex_none().max_w(px(120.)).whitespace_nowrap().overflow_hidden().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child(short_name(&folder, 20)))
                    .when(info.dirty, |d| d.child(div().flex_none().size(px(6.)).rounded_full().bg(color(c::TEXT_MUTED))))
                    .child(
                        div()
                            .id(("tab-list-close", ix))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(18.))
                            .rounded(px(4.))
                            .invisible()
                            .group_hover(group, |s| s.visible())
                            .hover(|s| s.bg(color(c::PRESSED)))
                            .child(icon(Icon::Close, 11., color(c::TEXT_DIM)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close(ix, window, cx);
                                if this.items.is_empty() {
                                    this.tab_list_open = false;
                                }
                            })),
                    )
            })
            .collect();
        div()
            .id("tab-list")
            .occlude()
            .absolute()
            .top(px(theme::ISLAND_HEADER_HEIGHT - 2.))
            .right(px(6.))
            .w(px(460.))
            .max_h(px(460.))
            .overflow_y_scroll()
            .p(px(6.))
            .flex()
            .flex_col()
            .gap(px(1.))
            .rounded(px(10.))
            .bg(color(c::POPUP))
            .border_1()
            .border_color(color(c::POPUP_BORDER))
            .shadow_lg()
            .text_size(px(theme::UI_TEXT))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.tab_list_open = false;
                this.tab_list_closed_at = Some(std::time::Instant::now());
                cx.notify();
            }))
            .child(div().px(px(8.)).pt(px(2.)).pb(px(4.)).text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child(format!("OPEN TABS  {}", infos.len())))
            .children(rows)
    }

    fn render_problems(&self, cx: &App) -> Option<gpui::AnyElement> {
        let PaneItem::Editor(editor) = self.items.get(self.active)? else { return None };
        let (errors, warnings) = editor.read(cx).problem_count();
        let (bg, fg, ic, label) = if errors > 0 {
            (color_a(c::RED, 0.18), color(c::RED), Icon::Error, errors.to_string())
        } else if warnings > 0 {
            (color_a(c::YELLOW, 0.16), color(c::YELLOW), Icon::Shield, warnings.to_string())
        } else {
            (color_a(c::GREEN, 0.12), color(c::GREEN), Icon::Check, String::new())
        };
        let tip = if errors + warnings == 0 {
            "No problems found".to_string()
        } else {
            format!("{errors} error(s), {warnings} warning(s)")
        };
        Some(
            div()
                .id("problems")
                .absolute()
                .top(px(8.))
                .right(px(16.))
                .flex()
                .items_center()
                .gap(px(4.))
                .h(px(22.))
                .px(px(6.))
                .rounded(px(6.))
                .bg(bg)
                .text_color(fg)
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .tooltip(tooltip(tip, None))
                .child(icon(ic, 12., fg))
                .when(!label.is_empty(), |d| d.child(label))
                .into_any_element(),
        )
    }
}

/// Path identity without touching the filesystem (no blocking I/O on the UI thread).
fn dunce_like(p: &Path) -> PathBuf {
    PathBuf::from(p.to_string_lossy().replace('/', std::path::MAIN_SEPARATOR_STR))
}

/// Where `p` is after `from` moved to `to` (`p` is `from` or inside it); `None` if unaffected.
/// Letter case is ignored, as Windows does.
fn moved_path(p: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let key = |p: &Path| dunce_like(p).to_string_lossy().to_lowercase();
    let (pk, fk) = (key(p), key(from));
    let fk = fk.trim_end_matches(std::path::MAIN_SEPARATOR);
    if pk == fk {
        return Some(to.to_path_buf());
    }
    let rest = pk.strip_prefix(fk)?.strip_prefix(std::path::MAIN_SEPARATOR)?;
    // Take the original-case tail from `p` (same length as the lowercased one only for
    // ASCII; fall back to counting components).
    let depth = Path::new(rest).components().count();
    let comps: Vec<_> = p.components().collect();
    let tail: PathBuf = comps[comps.len().saturating_sub(depth)..].iter().collect();
    Some(to.join(tail))
}

/// A long file name with "…" in place of its end, keeping the extension:
/// `Найти непересекающуюся п….py`.
fn short_name(name: &str, max: usize) -> String {
    let count = name.chars().count();
    if count <= max {
        return name.to_string();
    }
    let ext = name.rfind('.').filter(|&i| i > 0 && name.len() - i <= 8).map(|i| &name[i..]).unwrap_or("");
    let keep = max.saturating_sub(ext.chars().count() + 1).max(4);
    let head: String = name.chars().take(keep).collect();
    format!("{}…{ext}", head.trim_end())
}

fn tab_icon(info: &TabInfo) -> gpui::AnyElement {
    match info.kind {
        TabIcon::Plugins => icon(Icon::Puzzle, 14., color(c::ACCENT)).into_any_element(),
        TabIcon::Settings => icon(Icon::Settings, 14., color(c::ACCENT)).into_any_element(),
        TabIcon::Console => icon(Icon::Database, 14., color(c::ACCENT)).into_any_element(),
        TabIcon::Table => icon(Icon::Table, 14., color(c::ACCENT)).into_any_element(),
        TabIcon::File | TabIcon::Diff => crate::file_icons::file_icon(Path::new(&info.title)).into_any_element(),
    }
}

fn empty_state(cx: &App) -> impl IntoElement {
    // Shortcuts as set in Settings → Keymap; a command without one is left out.
    let row = |label: &'static str, keys: Vec<String>| {
        (!keys.is_empty()).then(|| div()
            .flex()
            .items_center()
            .justify_between()
            .w(px(360.))
            .h(px(32.))
            .child(div().text_color(color(c::TEXT_MUTED)).child(label))
            .child(div().flex().gap(px(4.)).children(keys.into_iter().map(kbd))))
    };
    let shortcut = |label: &'static str, id: &str| row(label, crate::keymap::shortcut_parts(id, cx));
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(2.))
        .child(icon(Icon::Sparkle, 36., color(c::ACCENT)))
        .child(div().mt(px(10.)).mb(px(14.)).text_size(px(17.)).text_color(color(c::TEXT)).child("Graphyne"))
        .children(row("Search Everywhere", vec!["Shift".into(), "Shift".into()]))
        .children(shortcut("Go to File / Action", "search_everywhere"))
        .children(shortcut("Find in Files", "find_in_files"))
        .children(shortcut("Run Current File", "run"))
        .children(shortcut("Toggle Terminal", "toggle_terminal"))
        .children(shortcut("Detach Terminal", "detach_terminal"))
}

impl Render for EditorPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let island_focused = self.items.get(self.active).is_some_and(|i| i.focus_handle(cx).contains_focused(window, cx));
        let infos: Vec<TabInfo> = self.items.iter().map(|item| self.tab_info(item, cx)).collect();
        // A newly active / opened / closed tab is scrolled into view.
        if self.scrolled_to != Some((self.active, self.items.len())) {
            self.scrolled_to = Some((self.active, self.items.len()));
            self.tab_scroll.scroll_to_item(self.active);
        }
        let mut strip = div().id("editor-tab-strip").flex().flex_1().min_w(px(0.)).items_center().gap(px(2.)).overflow_x_scroll().track_scroll(&self.tab_scroll);
        for (ix, info) in infos.iter().enumerate() {
            let is_active = ix == self.active;
            let dirty = info.dirty;
            let tip = info.path.as_ref().map(|p| self.display_path(p));
            strip = strip.child(
                div()
                    .id(("tab", ix))
                    .group(gpui::SharedString::from(format!("tab-{ix}")))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .h(px(26.))
                    .pl(px(8.))
                    .pr(px(4.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .text_size(px(theme::UI_TEXT))
                    .cursor_pointer()
                    .when(is_active && island_focused, |d| d.bg(color(c::TAB_FOCUSED)).border_color(color(c::TAB_FOCUSED_BORDER)).text_color(color(c::TEXT)))
                    .when(is_active && !island_focused, |d| d.bg(color(c::TAB_ACTIVE)).text_color(color(c::TEXT)))
                    .when(!is_active, |d| d.text_color(color(c::TEXT_MUTED)).hover(|s| s.bg(color(c::HOVER)).text_color(color(c::TEXT))))
                    .when_some(tip, |d, tip| d.tooltip(tooltip(tip, None)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.active = ix;
                        this.focus_active(window, cx);
                        cx.emit(PaneEvent::Changed);
                        cx.notify();
                    }))
                    .on_mouse_down(gpui::MouseButton::Middle, cx.listener(move |this, _, window, cx| this.close(ix, window, cx)))
                    .child(tab_icon(info))
                    // Long names are shortened with "…" (extension kept); the tooltip has the full path.
                    .child(div().max_w(px(280.)).whitespace_nowrap().overflow_hidden().child(short_name(&info.title, 30)))
                    .when(info.kind == TabIcon::Diff, |d| {
                        d.child(div().px(px(4.)).rounded(px(3.)).bg(color_a(c::GIT_MODIFIED, 0.18)).text_size(px(10.5)).text_color(color(c::GIT_MODIFIED)).child("diff"))
                    })
                    .child(
                        div()
                            .id(("tab-close", ix))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(18.))
                            .rounded(px(4.))
                            .hover(|s| s.bg(color(c::PRESSED)))
                            .when(dirty, |d| d.child(div().size(px(6.)).rounded_full().bg(color(c::TEXT_MUTED))))
                            .when(!dirty, |d| {
                                d.child(
                                    div()
                                        .when(!is_active, |d| d.invisible().group_hover(gpui::SharedString::from(format!("tab-{ix}")), |s| s.visible()))
                                        .child(icon(Icon::Close, 11., color(c::TEXT_DIM))),
                                )
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close(ix, window, cx);
                            })),
                    ),
            );
        }
        // Tabs that don't fit (from the last layout): the `▾ N` button lists every tab.
        let hidden = self.hidden_tab_count();
        let list_button = (hidden > 0 || self.tab_list_open).then(|| {
            div()
                .id("tab-list-button")
                .flex()
                .flex_none()
                .items_center()
                .gap(px(3.))
                .h(px(26.))
                .px(px(6.))
                .ml(px(2.))
                .rounded(px(6.))
                .cursor_pointer()
                .text_size(px(theme::UI_SMALL))
                .text_color(color(c::TEXT_MUTED))
                .when(self.tab_list_open, |d| d.bg(color(c::HOVER)).text_color(color(c::TEXT)))
                .hover(|s| s.bg(color(c::HOVER)).text_color(color(c::TEXT)))
                .tooltip(tooltip("All open tabs", None))
                .child(icon(Icon::ChevronDown, 13., color(c::TEXT_MUTED)))
                .when(hidden > 0, |d| d.child(hidden.to_string()))
                .on_click(cx.listener(|this, _, _, cx| {
                    let just_closed = this.tab_list_closed_at.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(250));
                    this.tab_list_open = !this.tab_list_open && !just_closed;
                    cx.notify();
                }))
        });
        let tab_list = self.tab_list_open.then(|| self.render_tab_list(&infos, cx));
        let header = island_header().child(strip).children(list_button);
        let body: gpui::AnyElement = match self.items.get(self.active) {
            Some(PaneItem::Editor(e)) => e.clone().into_any_element(),
            Some(PaneItem::Hex(h)) => h.clone().into_any_element(),
            Some(PaneItem::Diff(d)) => d.clone().into_any_element(),
            Some(PaneItem::Plugins(p)) => p.clone().into_any_element(),
            Some(PaneItem::Settings(p)) => p.clone().into_any_element(),
            Some(PaneItem::DbConsole(c)) => c.clone().into_any_element(),
            Some(PaneItem::DbTable(t)) => t.clone().into_any_element(),
            None => empty_state(cx).into_any_element(),
        };
        let problems = self.render_problems(cx);
        island()
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .font_family(theme::fonts().ui)
            .child(header)
            .child(div().flex_1().min_h(px(0.)).relative().child(body).children(problems))
            .children(tab_list)
    }
}

/// Read a file for an editor tab: `Ok(None)` for binary files (shown in the hex view).
/// Streams straight into a rope: a 1 GB file takes seconds and must not be held twice.
fn load_file(path: &Path) -> anyhow::Result<Option<graphyne_core::Buffer>> {
    if graphyne_core::fs_util::is_probably_binary(path) {
        return Ok(None);
    }
    let file = std::fs::File::open(path)?;
    Ok(Some(graphyne_core::Buffer::from_reader(std::io::BufReader::with_capacity(1 << 20, file))?))
}

#[cfg(test)]
mod tests {
    use super::{moved_path, short_name};
    use std::path::PathBuf;

    #[test]
    fn long_tab_names_keep_their_extension() {
        assert_eq!(short_name("parser.py", 30), "parser.py");
        let s = short_name("Найти непересекающуюся пару с наибольшей суммой.py", 30);
        assert!(s.ends_with("….py") && s.chars().count() <= 30, "{s}");
        assert_eq!(short_name("longest_common_subsequence_without_extension", 20).chars().count(), 20);
    }

    #[test]
    fn tabs_follow_renamed_paths() {
        let p = PathBuf::from;
        let (src, lib) = (p(r"C:\proj\src"), p(r"C:\proj\lib"));
        // The file itself.
        assert_eq!(moved_path(&p(r"C:\proj\src\a.py"), &p(r"C:\proj\src\a.py"), &p(r"C:\proj\src\b.py")), Some(p(r"C:\proj\src\b.py")));
        // A file inside the renamed folder keeps the case of its own name.
        assert_eq!(moved_path(&p(r"C:\proj\src\app\User.py"), &src, &lib), Some(p(r"C:\proj\lib\app\User.py")));
        // Windows paths: another letter case or forward slashes are the same file.
        assert_eq!(moved_path(&p(r"c:\Proj\SRC\a.py"), &src, &lib), Some(p(r"C:\proj\lib\a.py")));
        assert_eq!(moved_path(&p("C:/proj/src/a.py"), &src, &lib), Some(p(r"C:\proj\lib\a.py")));
        // A sibling whose name only starts the same is not inside the folder.
        assert_eq!(moved_path(&p(r"C:\proj\src2\a.py"), &src, &lib), None);
        assert_eq!(moved_path(&p(r"C:\proj\main.py"), &src, &lib), None);
    }
}
