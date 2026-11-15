//! `Files` tab of the left island: lazily-loaded, virtualised project tree.

use crate::assets::Icon;
use crate::components::icon;
use crate::theme::{self, c, color};
use gpui::prelude::*;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, SharedString, UniformListScrollHandle, Window, div, px, uniform_list,
};
use graphyne_core::libraries::LibraryEntry;
use graphyne_core::project::{DirEntryInfo, read_dir_sorted};
use graphyne_git::ChangeKind;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub enum FileTreeEvent {
    OpenFile(PathBuf),
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

    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix).cloned() else { return };
        match row.kind {
            // The root is a label, as in Fleet: it never collapses.
            RowKind::Root => {}
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
                cx.emit(FileTreeEvent::OpenFile(row.path.clone()));
            }
        }
        if row.kind != RowKind::Library {
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
}

impl Render for FileTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.rows.is_empty() {
            self.rebuild();
        }
        let rows = self.rows.clone();
        let selected = self.selected.clone();
        let colors: Vec<Option<gpui::Hsla>> = rows.iter().map(|r| self.git_color(&r.path)).collect();
        div()
            .id("file-tree")
            .track_focus(&self.focus_handle)
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
                                _ if row.is_dir => Some(slot(
                                    icon(if row.expanded { Icon::ChevronDown } else { Icon::ChevronRight }, 13., color(c::TEXT_DIM)).into_any_element(),
                                )),
                                _ => Some(slot(crate::file_icons::file_icon(&row.path).into_any_element())),
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
                                .when(is_selected, |d| d.bg(color(c::TAB_ACTIVE)))
                                .when(!is_selected, |d| d.hover(|s| s.bg(color(c::HOVER))))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| this.toggle(ix, cx)))
                                .children(leading)
                                .child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().child(row.name.clone()))
                                .when(row.kind == RowKind::Root, |d| {
                                    d.font_weight(gpui::FontWeight::MEDIUM)
                                })
                        })
                        .collect()
                }))
                .track_scroll(self.scroll.clone())
                .px(px(4.))
                .size_full(),
            )
    }
}
