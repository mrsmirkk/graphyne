//! Center-top island: open-file tabs + the active editor / hex view.

use crate::assets::Icon;
use crate::components::{icon, island, island_header, kbd, tooltip};
use crate::diff_view::{DiffEvent, DiffView, HunkSelection};
use crate::editor::{Editor, EditorEvent};
use crate::hex_view::HexView;
use crate::plugins_page::PluginsPage;
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, Subscription, Task, Window, div, px};
use graphyne_core::LanguageId;
use graphyne_core::session::{TabKind, TabState};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub enum PaneItem {
    Editor(Entity<Editor>),
    Hex(Entity<HexView>),
    /// Side-by-side comparison with HEAD (opened from the Git tab).
    Diff(Entity<DiffView>),
    /// Installed extensions, permissions and theme.
    Plugins(Entity<PluginsPage>),
}

impl PaneItem {
    fn path(&self, cx: &App) -> Option<PathBuf> {
        match self {
            PaneItem::Editor(e) => e.read(cx).path.clone(),
            PaneItem::Hex(h) => Some(h.read(cx).path.clone()),
            // A diff tab never stands in for the file itself.
            PaneItem::Diff(_) | PaneItem::Plugins(_) => None,
        }
    }
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            PaneItem::Editor(e) => e.focus_handle(cx),
            PaneItem::Hex(h) => h.focus_handle(cx),
            PaneItem::Diff(d) => d.focus_handle(cx),
            PaneItem::Plugins(p) => p.focus_handle(cx),
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
}

impl EventEmitter<PaneEvent> for EditorPane {}

impl Focusable for EditorPane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.items.get(self.active).map(|i| i.focus_handle(cx)).unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl EditorPane {
    pub fn new(root: PathBuf, lsp_store: Entity<crate::lsp_store::LspStore>, cx: &mut Context<Self>) -> Self {
        Self { focus_handle: cx.focus_handle(), items: Vec::new(), active: 0, root, lsp_store, debugger: None, hunk_excluded: HunkSelection::default(), subs: Vec::new(), error: None }
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

    pub fn diff_views(&self) -> impl Iterator<Item = &Entity<DiffView>> {
        self.items.iter().filter_map(|i| match i {
            PaneItem::Diff(d) => Some(d),
            _ => None,
        })
    }

    /// Open (or activate) a file; `at` = (row, col) to reveal.
    pub fn open(&mut self, path: PathBuf, at: Option<(usize, usize)>, window: &mut Window, cx: &mut Context<Self>) {
        let canonical = dunce_like(&path);
        if let Some(ix) = self.items.iter().position(|i| i.path(cx).as_deref().map(dunce_like) == Some(canonical.clone())) {
            self.active = ix;
            if let (Some((row, col)), PaneItem::Editor(e)) = (at, &self.items[ix]) {
                e.update(cx, |e, cx| e.go_to(row, col, window, cx));
            } else {
                self.focus_active(window, cx);
            }
            cx.emit(PaneEvent::Changed);
            cx.notify();
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let p = path.clone();
            let loaded = cx.background_spawn(async move { load_file(&p) }).await;
            this.update_in(cx, |this, window, cx| {
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
                            None => window.focus(&editor.focus_handle(cx)),
                        }
                    }
                    Ok(None) => {
                        let hex = cx.new(|cx| HexView::new(path.clone(), cx));
                        this.items.insert((this.active + 1).min(this.items.len()), PaneItem::Hex(hex.clone()));
                        this.active = (this.active + 1).min(this.items.len() - 1);
                        if this.items.len() == 1 {
                            this.active = 0;
                        }
                        window.focus(&hex.focus_handle(cx));
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
                    TabState { kind: TabKind::File, path: e.path.clone(), cursor, anchor, scroll_line, scroll_x }
                }
                PaneItem::Hex(h) => TabState { kind: TabKind::File, path: Some(h.read(cx).path.clone()), ..Default::default() },
                PaneItem::Diff(d) => TabState { kind: TabKind::Diff, path: Some(d.read(cx).path.clone()), ..Default::default() },
                PaneItem::Plugins(_) => TabState { kind: TabKind::Plugins, ..Default::default() },
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
            let paths: Vec<Option<PathBuf>> = tabs.iter().map(|t| t.path.clone().filter(|_| t.kind == TabKind::File)).collect();
            let loaded: Vec<Option<anyhow::Result<Option<graphyne_core::Buffer>>>> =
                cx.background_spawn(async move { paths.into_iter().map(|p| p.filter(|p| p.is_file()).map(|p| load_file(&p))).collect() }).await;
            this.update_in(cx, |this, window, cx| {
                let mut restored = Vec::new();
                let mut new_active = 0;
                for (ix, (tab, file)) in tabs.into_iter().zip(loaded).enumerate() {
                    let item = match (tab.kind, tab.path, file) {
                        (TabKind::File, Some(path), Some(Ok(Some(buffer)))) => {
                            let editor = this.new_editor(path, buffer, cx);
                            editor.update(cx, |e, cx| e.restore_view(tab.cursor, tab.anchor, tab.scroll_line, tab.scroll_x, cx));
                            PaneItem::Editor(editor)
                        }
                        (TabKind::File, Some(path), Some(Ok(None))) => PaneItem::Hex(cx.new(|cx| HexView::new(path, cx))),
                        (TabKind::Diff, Some(path), _) if path.is_file() => PaneItem::Diff(this.new_diff_view(path, cx)),
                        (TabKind::Plugins, _, _) => PaneItem::Plugins(cx.new(PluginsPage::new)),
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
        if let PaneItem::Editor(e) = &self.items[ix] {
            if let Some(task) = e.update(cx, |e, cx| e.save(cx)) {
                task.detach();
            }
        }
        self.items.remove(ix);
        if self.active > ix || self.active >= self.items.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.focus_active(window, cx);
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

fn empty_state() -> impl IntoElement {
    let row = |label: &'static str, keys: &[&'static str]| {
        div()
            .flex()
            .items_center()
            .justify_between()
            .w(px(360.))
            .h(px(32.))
            .child(div().text_color(color(c::TEXT_MUTED)).child(label))
            .child(div().flex().gap(px(4.)).children(keys.iter().map(|k| kbd(*k))))
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(2.))
        .child(icon(Icon::Sparkle, 36., color(c::ACCENT)))
        .child(div().mt(px(10.)).mb(px(14.)).text_size(px(17.)).text_color(color(c::TEXT)).child("Graphyne"))
        .child(row("Search Everywhere", &["Shift", "Shift"]))
        .child(row("Go to File / Action", &["Ctrl", "K"]))
        .child(row("Find in Files", &["Ctrl", "Shift", "F"]))
        .child(row("Run Current File", &["Shift", "F10"]))
        .child(row("Toggle Terminal", &["Ctrl", "J"]))
        .child(row("Detach Terminal", &["Alt", "Shift", "F12"]))
}

impl Render for EditorPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let island_focused = self.items.get(self.active).is_some_and(|i| i.focus_handle(cx).contains_focused(window, cx));
        let mut header = island_header().id("editor-tabs").overflow_x_scroll();
        for (ix, item) in self.items.iter().enumerate() {
            let is_active = ix == self.active;
            let (title, _lang, dirty) = match item {
                PaneItem::Editor(e) => {
                    let e = e.read(cx);
                    (e.title(), e.language, e.is_dirty())
                }
                PaneItem::Hex(h) => (h.read(cx).title(), LanguageId::PlainText, false),
                PaneItem::Diff(d) => (d.read(cx).title().to_string(), LanguageId::PlainText, false),
                PaneItem::Plugins(_) => ("Plugins".to_string(), LanguageId::PlainText, false),
            };
            let is_diff = matches!(item, PaneItem::Diff(_));
            let is_plugins = matches!(item, PaneItem::Plugins(_));
            header = header.child(
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
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.active = ix;
                        this.focus_active(window, cx);
                        cx.emit(PaneEvent::Changed);
                        cx.notify();
                    }))
                    .on_mouse_down(gpui::MouseButton::Middle, cx.listener(move |this, _, window, cx| this.close(ix, window, cx)))
                    .when(is_plugins, |d| d.child(icon(Icon::Puzzle, 14., color(c::ACCENT))))
                    .when(!is_plugins, |d| d.child(crate::file_icons::file_icon(Path::new(&title))))
                    .child(title)
                    .when(is_diff, |d| {
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
        let body: gpui::AnyElement = match self.items.get(self.active) {
            Some(PaneItem::Editor(e)) => e.clone().into_any_element(),
            Some(PaneItem::Hex(h)) => h.clone().into_any_element(),
            Some(PaneItem::Diff(d)) => d.clone().into_any_element(),
            Some(PaneItem::Plugins(p)) => p.clone().into_any_element(),
            None => empty_state().into_any_element(),
        };
        let problems = self.render_problems(cx);
        island()
            .track_focus(&self.focus_handle)
            .size_full()
            .font_family(theme::fonts().ui)
            .child(header)
            .child(div().flex_1().min_h(px(0.)).relative().child(body).children(problems))
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
