//! `Files` tab of the left island: lazily-loaded, virtualised project tree, with file
//! operations — new file / folder, rename, delete to the Recycle Bin, copy path, reveal.

use crate::assets::Icon;
use crate::components::icon;
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent, MouseButton, MouseDownEvent, Pixels,
    Point, SharedString, Subscription, UniformListScrollHandle, Window, anchored, deferred, div, px, uniform_list,
};
use graphyne_core::libraries::LibraryEntry;
use graphyne_core::project::{DirEntryInfo, read_dir_sorted};
use graphyne_git::ChangeKind;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub enum FileTreeEvent {
    /// Open and focus the file (double click, a new file).
    OpenFile(PathBuf),
    /// Open the file but keep the keyboard in the tree (single click).
    PreviewFile(PathBuf),
    /// A file or folder was renamed / moved: open tabs follow it.
    Renamed(PathBuf, PathBuf),
    /// A file or folder went to the Recycle Bin: its tabs close.
    Deleted(PathBuf),
    /// A file operation failed (shown as a notification).
    Error(String),
}

#[derive(Clone)]
struct Row {
    path: PathBuf,
    name: SharedString,
    depth: usize,
    is_dir: bool,
    expanded: bool,
    kind: RowKind,
}

#[derive(Clone, Copy, PartialEq)]
enum RowKind {
    Entry,
    Root,
    LibrariesHeader,
    Library,
    /// The inline name field of a new file / folder.
    NewItem,
}

/// The right-click menu.
struct TreeMenu {
    path: PathBuf,
    is_dir: bool,
    kind: RowKind,
    position: Point<Pixels>,
}

#[derive(Clone, Copy, PartialEq)]
enum EditKind {
    NewFile,
    NewFolder,
    Rename,
}

/// A name being typed in the tree: a new item in `dir`, or renaming `target`.
struct TreeEdit {
    kind: EditKind,
    dir: PathBuf,
    target: Option<PathBuf>,
    input: Entity<TextInput>,
    error: Option<SharedString>,
    /// The file operation is running (Enter, then a click elsewhere: don't run it twice).
    busy: bool,
    _subs: Vec<Subscription>,
}

pub struct FileTree {
    focus_handle: FocusHandle,
    root: PathBuf,
    children: HashMap<PathBuf, Vec<DirEntryInfo>>,
    expanded: HashSet<PathBuf>,
    loading: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    git: HashMap<String, ChangeKind>,
    /// (toolchain label, its std / package folders).
    libraries: Vec<(SharedString, Vec<LibraryEntry>)>,
    libraries_expanded: bool,
    rows: Vec<Row>,
    scroll: UniformListScrollHandle,
    menu: Option<TreeMenu>,
    edit: Option<TreeEdit>,
    /// "Delete …?" waiting for an answer, shown where the menu was.
    confirm_delete: Option<(PathBuf, bool, Point<Pixels>)>,
}

impl EventEmitter<FileTreeEvent> for FileTree {}

impl Focusable for FileTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl FileTree {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut tree = Self {
            focus_handle: cx.focus_handle(),
            root: root.clone(),
            children: HashMap::new(),
            expanded: HashSet::from([root.clone()]),
            loading: HashSet::new(),
            selected: None,
            git: HashMap::new(),
            libraries: Vec::new(),
            libraries_expanded: false,
            rows: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            menu: None,
            edit: None,
            confirm_delete: None,
        };
        tree.load_dir(root, cx);
        tree
    }

    fn load_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if !self.loading.insert(dir.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let d = dir.clone();
            let entries = cx.background_spawn(async move { read_dir_sorted(&d) }).await;
            this.update(cx, |this, cx| {
                this.loading.remove(&dir);
                this.children.insert(dir, entries);
                this.rebuild();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Expanded folders inside the project (for the session), parents first.
    pub fn expanded_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self.expanded.iter().filter(|d| **d != self.root && d.starts_with(&self.root)).cloned().collect();
        dirs.sort();
        dirs
    }

    /// Re-expand folders saved by [`Self::expanded_dirs`] (missing ones are skipped).
    pub fn restore_expanded(&mut self, dirs: Vec<PathBuf>, cx: &mut Context<Self>) {
        let root = self.root.clone();
        for d in dirs.into_iter().filter(|d| d.starts_with(&root) && d.is_dir()) {
            if self.expanded.insert(d.clone()) {
                self.load_dir(d, cx);
            }
        }
        self.rebuild();
        cx.notify();
    }

    /// Re-read every expanded directory (after saves / window activation).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let dirs: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        for d in dirs {
            self.load_dir(d, cx);
        }
    }

    pub fn set_git_status(&mut self, status: HashMap<String, ChangeKind>, cx: &mut Context<Self>) {
        self.git = status;
        cx.notify();
    }

    pub fn set_libraries(&mut self, libs: Vec<(SharedString, Vec<LibraryEntry>)>, cx: &mut Context<Self>) {
        self.libraries = libs;
        self.rebuild();
        cx.notify();
    }

    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.selected = Some(path.to_path_buf());
        let mut dir = path.parent();
        while let Some(d) = dir {
            if !d.starts_with(&self.root) {
                break;
            }
            if self.expanded.insert(d.to_path_buf()) && !self.children.contains_key(d) {
                self.load_dir(d.to_path_buf(), cx);
            }
            dir = d.parent();
        }
        self.rebuild();
        if let Some(ix) = self.rows.iter().position(|r| r.path == path) {
            self.scroll.scroll_to_item(ix, gpui::ScrollStrategy::Center);
        }
        cx.notify();
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        let root_name: SharedString = graphyne_core::project::project_name(&self.root).into();
        let root_expanded = self.expanded.contains(&self.root);
        rows.push(Row { path: self.root.clone(), name: root_name, depth: 0, is_dir: true, expanded: root_expanded, kind: RowKind::Root });
        if root_expanded {
            self.push_children(&self.root.clone(), 1, &mut rows);
        }
        rows.push(Row {
            path: PathBuf::from("::external-libraries"),
            name: "External Libraries".into(),
            depth: 0,
            is_dir: true,
            expanded: self.libraries_expanded,
            kind: RowKind::LibrariesHeader,
        });
        if self.libraries_expanded {
            for (label, entries) in &self.libraries {
                let key = PathBuf::from(format!("::lib::{label}"));
                let open = self.expanded.contains(&key);
                rows.push(Row { path: key, name: label.clone(), depth: 1, is_dir: !entries.is_empty(), expanded: open, kind: RowKind::Library });
                if !open {
                    continue;
                }
                for e in entries {
                    let expanded = e.is_dir && self.expanded.contains(&e.path);
                    rows.push(Row { path: e.path.clone(), name: e.name.clone().into(), depth: 2, is_dir: e.is_dir, expanded, kind: RowKind::Entry });
                    if expanded {
                        self.push_children(&e.path, 3, &mut rows);
                    }
                }
            }
        }
        // The name field of a new item sits first inside its folder.
        if let Some(edit) = self.edit.as_ref().filter(|e| e.kind != EditKind::Rename) {
            if let Some(ix) = rows.iter().position(|r| r.path == edit.dir && matches!(r.kind, RowKind::Root | RowKind::Entry)) {
                let depth = rows[ix].depth + 1;
                rows.insert(ix + 1, Row { path: edit.dir.join("::new"), name: SharedString::default(), depth, is_dir: edit.kind == EditKind::NewFolder, expanded: false, kind: RowKind::NewItem });
            }
        }
        self.rows = rows;
    }

    fn push_children(&self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        let Some(entries) = self.children.get(dir) else { return };
        for e in entries {
            let expanded = e.is_dir && self.expanded.contains(&e.path);
            rows.push(Row { path: e.path.clone(), name: e.name.clone().into(), depth, is_dir: e.is_dir, expanded, kind: RowKind::Entry });
            if expanded {
                self.push_children(&e.path, depth + 1, rows);
            }
        }
    }

    fn toggle(&mut self, ix: usize, clicks: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix).cloned() else { return };
        self.menu = None;
        match row.kind {
            // The root is a label, as in Fleet: it never collapses.
            RowKind::Root | RowKind::NewItem => {}
            RowKind::LibrariesHeader => self.libraries_expanded = !self.libraries_expanded,
            RowKind::Library => {
                if row.is_dir && !self.expanded.remove(&row.path) {
                    self.expanded.insert(row.path.clone());
                }
            }
            _ if row.is_dir => {
                if !self.expanded.remove(&row.path) {
                    self.expanded.insert(row.path.clone());
                    if !self.children.contains_key(&row.path) {
                        self.load_dir(row.path.clone(), cx);
                    }
                }
            }
            _ => {
                self.selected = Some(row.path.clone());
                cx.emit(if clicks >= 2 { FileTreeEvent::OpenFile(row.path.clone()) } else { FileTreeEvent::PreviewFile(row.path.clone()) });
            }
        }
        if !matches!(row.kind, RowKind::Library | RowKind::NewItem) {
            self.selected = Some(row.path);
        }
        self.rebuild();
        cx.notify();
    }

    fn git_color(&self, path: &Path) -> Option<gpui::Hsla> {
        let rel = path.strip_prefix(&self.root).ok()?.to_string_lossy().replace('\\', "/");
        if let Some(kind) = self.git.get(&rel) {
            return Some(color(match kind {
                ChangeKind::Modified | ChangeKind::Renamed => c::GIT_MODIFIED,
                ChangeKind::Added => c::GIT_ADDED,
                ChangeKind::Untracked => c::GIT_UNTRACKED,
                ChangeKind::Deleted => c::GIT_DELETED,
                ChangeKind::Conflicted => c::RED,
            }));
        }
        // Directories containing changes get a softer tint.
        let prefix = format!("{rel}/");
        self.git.keys().any(|k| k.starts_with(&prefix)).then(|| color(c::GIT_MODIFIED).opacity(0.8))
    }

    // ------------------------------------------------------------------ file operations

    /// Whether `path` is inside the project (file operations stay there; libraries are read-only).
    fn in_project(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
    }

    fn open_menu(&mut self, ix: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix).cloned() else { return };
        if matches!(row.kind, RowKind::LibrariesHeader | RowKind::NewItem) || (row.kind == RowKind::Library && !row.path.exists()) {
            return;
        }
        self.confirm_delete = None;
        if row.kind != RowKind::Library {
            self.selected = Some(row.path.clone());
        }
        self.menu = Some(TreeMenu { path: row.path, is_dir: row.is_dir, kind: row.kind, position });
        cx.notify();
    }

    /// The folder a new item goes into when started from `path` (a file's parent).
    fn target_dir(&self, path: &Path, is_dir: bool) -> PathBuf {
        if is_dir { path.to_path_buf() } else { path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone()) }
    }

    fn begin_edit(&mut self, kind: EditKind, dir: PathBuf, target: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.confirm_delete = None;
        if kind != EditKind::Rename && self.expanded.insert(dir.clone()) && !self.children.contains_key(&dir) {
            self.load_dir(dir.clone(), cx);
        }
        let placeholder = match kind {
            EditKind::NewFile => "File name, e.g. main.py or src/app.py",
            EditKind::NewFolder => "Folder name",
            EditKind::Rename => "New name",
        };
        let input = cx.new(|cx| {
            let mut input = TextInput::new(placeholder, cx);
            if let Some(name) = target.as_ref().and_then(|t| t.file_name()).map(|n| n.to_string_lossy().to_string()) {
                // Like VS Code: the name without its extension is selected.
                let stem_end = name.rfind('.').filter(|&i| i > 0).unwrap_or(name.len());
                input.set_text(name, cx);
                input.select_range(0..stem_end, cx);
            }
            input
        });
        let mut subs = vec![cx.subscribe_in(&input, window, |this, _, ev: &TextInputEvent, window, cx| match ev {
            TextInputEvent::Submit => this.commit_edit(false, window, cx),
            TextInputEvent::Cancel => this.cancel_edit(true, window, cx),
            TextInputEvent::Changed(_) => {
                if let Some(edit) = this.edit.as_mut() {
                    edit.error = None;
                }
                cx.notify();
            }
            _ => {}
        })];
        // Clicking elsewhere ends the edit: kept if the name is fine, dropped otherwise, and
        // the keyboard stays where the user clicked.
        subs.push(cx.on_blur(&input.focus_handle(cx), window, |this, window, cx| {
            if this.edit.is_some() {
                let text = this.edit.as_ref().map(|e| e.input.read(cx).text().trim().to_string()).unwrap_or_default();
                if text.is_empty() { this.cancel_edit(false, window, cx) } else { this.commit_edit(true, window, cx) }
            }
        }));
        window.focus(&input.focus_handle(cx));
        self.edit = Some(TreeEdit { kind, dir, target, input, error: None, busy: false, _subs: subs });
        self.rebuild();
        if let Some(ix) = self.rows.iter().position(|r| r.kind == RowKind::NewItem || Some(&r.path) == self.edit.as_ref().and_then(|e| e.target.as_ref())) {
            self.scroll.scroll_to_item(ix, gpui::ScrollStrategy::Center);
        }
        cx.notify();
    }

    /// Drop the name field; `refocus` gives the keyboard back to the tree (not after a click
    /// elsewhere).
    fn cancel_edit(&mut self, refocus: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.take().is_some() {
            self.rebuild();
            if refocus {
                window.focus(&self.focus_handle);
            }
            cx.notify();
        }
    }

    fn fail_edit(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(edit) = self.edit.as_mut() {
            edit.error = Some(error.into());
        }
        cx.notify();
    }

    /// Create / rename with the typed name. `blur`: the user clicked elsewhere — a bad name
    /// then drops the edit (with a notification) instead of waiting for a correction.
    fn commit_edit(&mut self, blur: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.edit.as_ref().filter(|e| !e.busy) else { return };
        let text = edit.input.read(cx).text().trim().to_string();
        let (kind, dir, target) = (edit.kind, edit.dir.clone(), edit.target.clone());
        let destination = match kind {
            EditKind::NewFile | EditKind::NewFolder => graphyne_core::fs_ops::new_item_path(&dir, &text),
            EditKind::Rename => graphyne_core::fs_ops::validate_name(&text).map(|()| dir.join(&text)),
        };
        let destination = match destination {
            Ok(path) => path,
            Err(_) if blur => return self.cancel_edit(false, window, cx),
            Err(e) => return self.fail_edit(e, cx),
        };
        if target.as_ref() == Some(&destination) {
            return self.cancel_edit(!blur, window, cx);
        }
        if let Some(edit) = self.edit.as_mut() {
            edit.busy = true;
        }
        let to = destination.clone();
        let from = target.clone();
        cx.spawn_in(window, async move |this, cx| {
            let done = cx
                .background_spawn(async move {
                    match (kind, &from) {
                        (EditKind::NewFile, _) => graphyne_core::fs_ops::create_file(&to),
                        (EditKind::NewFolder, _) => graphyne_core::fs_ops::create_dir(&to),
                        (EditKind::Rename, Some(from)) => graphyne_core::fs_ops::rename(from, &to),
                        (EditKind::Rename, None) => Ok(()),
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| match done {
                Err(e) if blur => {
                    this.cancel_edit(false, window, cx);
                    cx.emit(FileTreeEvent::Error(e));
                }
                Err(e) => {
                    if let Some(edit) = this.edit.as_mut() {
                        edit.busy = false;
                    }
                    this.fail_edit(e, cx)
                }
                Ok(()) => {
                    this.edit = None;
                    if !blur {
                        window.focus(&this.focus_handle);
                    }
                    if let Some(old) = &target {
                        this.paths_moved(old, &destination);
                        cx.emit(FileTreeEvent::Renamed(old.clone(), destination.clone()));
                    }
                    for d in [destination.parent().map(Path::to_path_buf), target.as_ref().and_then(|t| t.parent()).map(Path::to_path_buf)].into_iter().flatten() {
                        if this.expanded.contains(&d) {
                            this.loading.remove(&d);
                            this.load_dir(d, cx);
                        }
                    }
                    this.reveal(&destination, cx);
                    if kind == EditKind::NewFile {
                        // Enter: start typing in the new file. Click elsewhere: just show it.
                        cx.emit(if blur { FileTreeEvent::PreviewFile(destination.clone()) } else { FileTreeEvent::OpenFile(destination.clone()) });
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Expanded / selected paths follow a rename.
    fn paths_moved(&mut self, from: &Path, to: &Path) {
        // (`to.join("")` would add a trailing separator.)
        let moved = |p: &PathBuf| p.strip_prefix(from).ok().map(|rest| if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) });
        self.expanded = self.expanded.iter().map(|p| moved(p).unwrap_or_else(|| p.clone())).collect();
        self.children.retain(|k, _| !k.starts_with(from));
        if let Some(sel) = self.selected.as_ref().and_then(moved) {
            self.selected = Some(sel);
        }
    }

    fn ask_delete(&mut self, path: PathBuf, is_dir: bool, position: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.in_project(&path) || path == self.root {
            return;
        }
        self.menu = None;
        self.confirm_delete = Some((path, is_dir, position));
        cx.notify();
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        let Some((path, _, _)) = self.confirm_delete.take() else { return };
        let target = path.clone();
        cx.spawn(async move |this, cx| {
            let done = cx.background_spawn(async move { graphyne_core::fs_ops::move_to_trash(&target) }).await;
            this.update(cx, |this, cx| {
                match done {
                    Ok(()) => {
                        this.expanded.retain(|p| !p.starts_with(&path));
                        this.children.retain(|k, _| !k.starts_with(&path));
                        if this.selected.as_ref().is_some_and(|s| s.starts_with(&path)) {
                            this.selected = path.parent().map(Path::to_path_buf);
                        }
                        if let Some(parent) = path.parent().map(Path::to_path_buf) {
                            this.loading.remove(&parent);
                            this.load_dir(parent, cx);
                        }
                        cx.emit(FileTreeEvent::Deleted(path));
                    }
                    Err(e) => cx.emit(FileTreeEvent::Error(format!("Could not delete {}: {e}", path.display()))),
                }
                this.rebuild();
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.is_some() {
            return; // the name field handles its own keys
        }
        let key = ev.keystroke.key.as_str();
        let plain = !ev.keystroke.modifiers.control && !ev.keystroke.modifiers.alt;
        match key {
            "escape" if self.menu.is_some() || self.confirm_delete.is_some() => {
                self.menu = None;
                self.confirm_delete = None;
                cx.notify();
            }
            "enter" if self.confirm_delete.is_some() => self.delete(cx),
            "f2" if plain => {
                if let Some(sel) = self.selected.clone().filter(|s| self.in_project(s) && *s != self.root) {
                    let dir = sel.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone());
                    self.begin_edit(EditKind::Rename, dir, Some(sel), window, cx);
                }
            }
            "delete" if plain => {
                if let Some(sel) = self.selected.clone() {
                    let is_dir = sel.is_dir();
                    let position = self.row_anchor(&sel);
                    self.ask_delete(sel, is_dir, position, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// Where to show a popup for `path` when there was no click (keyboard): near its row.
    fn row_anchor(&self, path: &Path) -> Point<Pixels> {
        let bounds = self.scroll.0.borrow().base_handle.bounds();
        let ix = self.rows.iter().position(|r| r.path == path).unwrap_or(0) as f32;
        let offset = f32::from(self.scroll.0.borrow().base_handle.offset().y);
        gpui::point(bounds.left() + px(40.), bounds.top() + px(4. + ix * 24. + 24.) + px(offset))
    }

    fn render_menu(&self, menu: &TreeMenu, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let in_project = self.in_project(&menu.path);
        let can_create = in_project && menu.kind != RowKind::Library;
        let can_change = in_project && menu.kind == RowKind::Entry;
        let dir = self.target_dir(&menu.path, menu.is_dir);
        let (path, is_dir, position) = (menu.path.clone(), menu.is_dir, menu.position);
        let rel = path.strip_prefix(&self.root).map(|r| r.display().to_string()).unwrap_or_else(|_| path.display().to_string());
        let item = |id: &'static str, label: &'static str, shortcut: Option<&'static str>, ic: Icon| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(10.))
                .h(px(28.))
                .px(px(10.))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|s| s.bg(color(c::HOVER)))
                .child(icon(ic, 14., color(c::TEXT_MUTED)))
                .child(div().flex_1().text_color(color(c::TEXT)).child(label))
                .when_some(shortcut, |d, s| d.child(div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child(s)))
        };
        let separator = || div().my(px(4.)).h(px(1.)).bg(color(c::BORDER));
        let menu_box = div()
            .id("tree-menu")
            .occlude()
            .w(px(240.))
            .p(px(5.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .bg(color(c::POPUP))
            .border_1()
            .border_color(color(c::POPUP_BORDER))
            .shadow_lg()
            .font_family(theme::fonts().ui)
            .text_size(px(theme::UI_TEXT))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.menu = None;
                cx.notify();
            }))
            .when(can_create, |d| {
                let (dir_file, dir_folder) = (dir.clone(), dir.clone());
                d.child(item("tree-new-file", "New File", None, Icon::FilePlus).on_click(cx.listener(move |this, _, window, cx| this.begin_edit(EditKind::NewFile, dir_file.clone(), None, window, cx))))
                    .child(item("tree-new-folder", "New Folder", None, Icon::FolderPlus).on_click(cx.listener(move |this, _, window, cx| this.begin_edit(EditKind::NewFolder, dir_folder.clone(), None, window, cx))))
                    .child(separator())
            })
            .when(can_change, |d| {
                let (rename_path, delete_path) = (path.clone(), path.clone());
                d.child(item("tree-rename", "Rename…", Some("F2"), Icon::Pencil).on_click(cx.listener(move |this, _, window, cx| {
                    let parent = rename_path.parent().map(Path::to_path_buf).unwrap_or_else(|| this.root.clone());
                    this.begin_edit(EditKind::Rename, parent, Some(rename_path.clone()), window, cx);
                })))
                .child(item("tree-delete", "Delete", Some("Delete"), Icon::Trash).on_click(cx.listener(move |this, _, _, cx| this.ask_delete(delete_path.clone(), is_dir, position, cx))))
                .child(separator())
            })
            .child({
                let p = path.display().to_string();
                item("tree-copy-path", "Copy Path", None, Icon::Copy).on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(p.clone()));
                    this.menu = None;
                    cx.notify();
                }))
            })
            .when(in_project, |d| {
                d.child(item("tree-copy-rel", "Copy Relative Path", None, Icon::Copy).on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(rel.replace('\\', "/")));
                    this.menu = None;
                    cx.notify();
                })))
            })
            .child(item("tree-reveal", "Reveal in File Explorer", None, Icon::FolderOpen).on_click(cx.listener(move |this, _, _, cx| {
                cx.reveal_path(&path);
                this.menu = None;
                cx.notify();
            })));
        deferred(anchored().position(position).snap_to_window_with_margin(px(8.)).child(menu_box)).with_priority(2)
    }

    fn render_confirm(&self, path: &Path, is_dir: bool, position: Point<Pixels>, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let button = |id: &'static str, label: &'static str, danger: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .h(px(28.))
                .px(px(12.))
                .rounded(px(6.))
                .border_1()
                .cursor_pointer()
                .text_size(px(theme::UI_SMALL))
                .when(danger, |d| d.bg(color(c::RED)).border_color(color(c::RED)).text_color(color(0xFFFFFF)).hover(|s| s.bg(color_a(c::RED, 0.85))))
                .when(!danger, |d| d.border_color(color(c::POPUP_BORDER)).text_color(color(c::TEXT)).hover(|s| s.bg(color(c::HOVER))))
                .child(label)
        };
        let popup = div()
            .id("tree-confirm-delete")
            .occlude()
            .w(px(300.))
            .p(px(14.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(10.))
            .bg(color(c::POPUP))
            .border_1()
            .border_color(color(c::POPUP_BORDER))
            .shadow_lg()
            .font_family(theme::fonts().ui)
            .text_size(px(theme::UI_TEXT))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.confirm_delete = None;
                cx.notify();
            }))
            .child(div().text_color(color(c::TEXT)).font_weight(gpui::FontWeight::MEDIUM).child(format!("Delete \u{201C}{name}\u{201D}?")))
            .child(div().text_color(color(c::TEXT_MUTED)).child(if is_dir {
                "The folder and everything in it go to the Recycle Bin."
            } else {
                "The file goes to the Recycle Bin."
            }))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .pt(px(4.))
                    .child(button("tree-delete-yes", "Delete", true).on_click(cx.listener(|this, _, _, cx| this.delete(cx))))
                    .child(button("tree-delete-no", "Cancel", false).on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_delete = None;
                        cx.notify();
                    }))),
            );
        deferred(anchored().position(position).snap_to_window_with_margin(px(8.)).child(popup)).with_priority(2)
    }
}

impl Render for FileTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.rows.is_empty() {
            self.rebuild();
        }
        let rows = self.rows.clone();
        let selected = self.selected.clone();
        let colors: Vec<Option<gpui::Hsla>> = rows.iter().map(|r| self.git_color(&r.path)).collect();
        let edit = self.edit.as_ref().map(|e| (e.kind, e.target.clone(), e.input.clone(), e.error.clone()));
        let menu = self.menu.as_ref().map(|m| self.render_menu(m, cx).into_any_element());
        let confirm = self.confirm_delete.clone().map(|(p, is_dir, pos)| self.render_confirm(&p, is_dir, pos, cx).into_any_element());
        div()
            .id("file-tree")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .pt(px(4.))
            .child(
                uniform_list("file-tree-list", rows.len(), cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                    range
                        .map(|ix| {
                            let row = &rows[ix];
                            let is_selected = selected.as_ref() == Some(&row.path);
                            let text_color = colors[ix].unwrap_or(if row.kind == RowKind::Library { color(c::TEXT_MUTED) } else { color(c::TEXT) });
                            // Fleet-style minimal tree: folders are chevron + name, files put their
                            // icon in the chevron's slot so names line up, the root is a plain label.
                            let slot = |child: gpui::AnyElement| div().flex().flex_none().items_center().justify_center().w(px(16.)).child(child).into_any_element();
                            let leading = match row.kind {
                                RowKind::Root => None,
                                RowKind::Library if !row.is_dir => Some(slot(icon(Icon::Binary, 13., color(c::TEXT_DIM)).into_any_element())),
                                RowKind::NewItem if !row.is_dir => Some(slot(icon(Icon::File, 13., color(c::TEXT_DIM)).into_any_element())),
                                _ if row.is_dir => Some(slot(
                                    icon(if row.expanded { Icon::ChevronDown } else { Icon::ChevronRight }, 13., color(c::TEXT_DIM)).into_any_element(),
                                )),
                                _ => Some(slot(crate::file_icons::file_icon(&row.path).into_any_element())),
                            };
                            // The name field replaces the label of the row being created / renamed.
                            let editing = edit.as_ref().filter(|(kind, target, _, _)| match kind {
                                EditKind::Rename => target.as_ref() == Some(&row.path),
                                _ => row.kind == RowKind::NewItem,
                            });
                            let label: gpui::AnyElement = match editing {
                                Some((_, _, input, error)) => div()
                                    .flex()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .items_center()
                                    .gap(px(6.))
                                    .h(px(22.))
                                    .px(px(4.))
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_color(color(if error.is_some() { c::RED } else { c::TAB_FOCUSED_BORDER }))
                                    .bg(color(c::ISLAND))
                                    .child(div().flex_1().min_w(px(0.)).child(input.clone()))
                                    .when_some(error.clone(), |d, e| d.child(div().flex_none().max_w(px(150.)).whitespace_nowrap().overflow_hidden().text_size(px(11.)).text_color(color(c::RED)).child(e)))
                                    .into_any_element(),
                                None => div().whitespace_nowrap().overflow_hidden().text_ellipsis().child(row.name.clone()).into_any_element(),
                            };
                            div()
                                .id(ix)
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .h(px(24.))
                                .w_full()
                                .pl(px(8. + row.depth as f32 * 14.))
                                .pr(px(8.))
                                .rounded(px(5.))
                                .text_size(px(theme::UI_TEXT))
                                .text_color(text_color)
                                .when(is_selected && editing.is_none(), |d| d.bg(color(c::TAB_ACTIVE)))
                                .when(!is_selected && editing.is_none(), |d| d.hover(|s| s.bg(color(c::HOVER))))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                                    window.focus(&this.focus_handle);
                                    this.toggle(ix, ev.click_count(), cx)
                                }))
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.open_menu(ix, ev.position, cx);
                                    }),
                                )
                                .children(leading)
                                .child(label)
                                .when(row.kind == RowKind::Root, |d| d.font_weight(gpui::FontWeight::MEDIUM))
                        })
                        .collect()
                }))
                .track_scroll(self.scroll.clone())
                .px(px(4.))
                .size_full(),
            )
            .children(menu)
            .children(confirm)
    }
}
