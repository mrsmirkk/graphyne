//! Left-island tool tabs: Search (find in files), Git (changes + commit with Secrets Guard),
//! History (commit log).

use crate::assets::Icon;
use crate::components::{icon, small_icon_button, tooltip};
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, PathBuilder, SharedString, Subscription, Task, WeakEntity, Window, canvas, div, point, px, uniform_list};
use graphyne_core::project::SearchHit;
use graphyne_core::secrets::SecretFinding;
use graphyne_git::{ChangeKind, CommitInfo, EdgePart, FileChange, GraphRow};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

pub enum PanelEvent {
    OpenFileAt(PathBuf, usize, usize),
    /// Show the side-by-side diff of a changed file.
    OpenDiff(PathBuf),
    GitChanged(HashMap<String, ChangeKind>, Option<String>),
}

fn search_field(input: &Entity<TextInput>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(30.))
        .mx(px(8.))
        .mt(px(8.))
        .px(px(8.))
        .rounded(px(6.))
        .bg(color(c::CANVAS))
        .border_1()
        .border_color(color(c::BORDER))
        .child(icon(Icon::Search, 13., color(c::TEXT_DIM)))
        .child(input.clone())
}

// ------------------------------------------------------------------------------------ Find

pub struct FindPanel {
    root: PathBuf,
    input: Entity<TextInput>,
    hits: Vec<SearchHit>,
    searching: bool,
    task: Option<Task<()>>,
    _sub: Subscription,
}

impl EventEmitter<PanelEvent> for FindPanel {}

impl Focusable for FindPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl FindPanel {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("Find in files…", cx));
        let sub = cx.subscribe(&input, |this, _, ev, cx| match ev {
            TextInputEvent::Changed(q) => this.search(q.clone(), cx),
            TextInputEvent::Submit => {
                if let Some(h) = this.hits.first() {
                    cx.emit(PanelEvent::OpenFileAt(this.root.join(&h.rel_path), h.line, h.column));
                }
            }
            _ => {}
        });
        Self { root, input, hits: Vec::new(), searching: false, task: None, _sub: sub }
    }

    fn search(&mut self, query: String, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self.searching = !query.is_empty();
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(180)).await;
            let hits = cx.background_spawn(async move { graphyne_core::project::find_in_files(&root, &query, 2000) }).await;
            this.update(cx, |this, cx| {
                this.hits = hits;
                this.searching = false;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for FindPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hits = self.hits.clone();
        let root = self.root.clone();
        let summary = if self.searching {
            "Searching…".to_string()
        } else if self.input.read(cx).text().is_empty() {
            "Type to search across the project".to_string()
        } else {
            format!("{} matches", hits.len())
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(search_field(&self.input))
            .child(div().px(px(12.)).py(px(6.)).text_size(px(theme::UI_SMALL)).text_color(color(c::TEXT_DIM)).child(summary))
            .child(
                uniform_list("find-hits", hits.len(), cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                    range
                        .map(|ix| {
                            let h = &hits[ix];
                            let path = root.join(&h.rel_path);
                            let (line, col) = (h.line, h.column);
                            let file = h.rel_path.rsplit('/').next().unwrap_or(&h.rel_path).to_string();
                            div()
                                .id(ix)
                                .flex()
                                .flex_col()
                                .justify_center()
                                .h(px(40.))
                                .w_full()
                                .px(px(8.))
                                .rounded(px(5.))
                                .cursor_pointer()
                                .hover(|s| s.bg(color(c::HOVER)))
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(PanelEvent::OpenFileAt(path.clone(), line, col))))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .text_size(px(theme::UI_SMALL))
                                        .text_color(color(c::TEXT_MUTED))
                                        .child(crate::file_icons::file_icon(std::path::Path::new(&file)))
                                        .child(format!("{file}:{}", line + 1)),
                                )
                                .child(
                                    div()
                                        .whitespace_nowrap()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .font_family(theme::fonts().code)
                                        .text_size(px(12.))
                                        .text_color(color(c::TEXT))
                                        .child(h.preview.clone()),
                                )
                        })
                        .collect()
                }))
                .px(px(4.))
                .flex_1(),
            )
    }
}

// ------------------------------------------------------------------------------------- Git

pub struct GitPanel {
    root: PathBuf,
    repo_root: Option<PathBuf>,
    pub branch: Option<String>,
    changes: Vec<FileChange>,
    unchecked: HashSet<String>,
    message: Entity<TextInput>,
    status: Option<(SharedString, bool)>,
    blocked: Vec<(String, SecretFinding)>,
    busy: bool,
    /// Open editors, flushed to disk before every commit.
    pub pane: Option<WeakEntity<crate::editor_pane::EditorPane>>,
    /// Blocks deselected in diff views: those files are committed partially.
    pub hunk_excluded: crate::diff_view::HunkSelection,
    _sub: Subscription,
}

impl EventEmitter<PanelEvent> for GitPanel {}

impl Focusable for GitPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.message.focus_handle(cx)
    }
}

impl GitPanel {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| TextInput::new("Commit message", cx));
        let sub = cx.subscribe(&message, |this, _, ev, cx| {
            if let TextInputEvent::Submit = ev {
                this.commit(false, cx);
            }
        });
        let mut panel = Self {
            root,
            repo_root: None,
            branch: None,
            changes: Vec::new(),
            unchecked: HashSet::new(),
            message,
            status: None,
            blocked: Vec::new(),
            busy: false,
            pane: None,
            hunk_excluded: Default::default(),
            _sub: sub,
        };
        panel.refresh(cx);
        panel
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let info = graphyne_git::discover(&root)?;
                    let changes = graphyne_git::status(&info.root).unwrap_or_default();
                    Some((info, changes))
                })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Some((info, changes)) => {
                        let map = changes.iter().map(|c| (c.rel_path.clone(), c.kind)).collect();
                        this.repo_root = Some(info.root);
                        this.branch = Some(info.branch.clone());
                        this.changes = changes;
                        cx.emit(PanelEvent::GitChanged(map, Some(info.branch)));
                    }
                    None => {
                        this.repo_root = None;
                        this.branch = None;
                        this.changes.clear();
                        cx.emit(PanelEvent::GitChanged(HashMap::new(), None));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn commit(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(repo_root) = self.repo_root.clone() else { return };
        if self.busy {
            return;
        }
        let message = self.message.read(cx).text().to_string();
        let unchecked = self.unchecked.clone();
        let hunk_excluded = self.hunk_excluded.clone();
        // Auto-save contract: unsaved buffers reach disk before git looks at the tree.
        let saves = self.pane.as_ref().and_then(|p| p.update(cx, |p, cx| p.save_all(cx)).ok()).unwrap_or_default();
        self.busy = true;
        self.status = Some(("Running Secrets Guard…".into(), false));
        cx.notify();
        cx.spawn(async move |this, cx| {
            for save in saves {
                save.await;
            }
            // Re-read status: saving may have changed which files differ from HEAD.
            let status_root = repo_root.clone();
            let changes = cx.background_spawn(async move { graphyne_git::status(&status_root).unwrap_or_default() }).await;
            let paths: Vec<String> = changes.into_iter().map(|c| c.rel_path).filter(|p| !unchecked.contains(p)).collect();
            // Files with blocks left out in the diff view: stage only the selected blocks.
            let excluded: HashMap<String, HashSet<u64>> = {
                let map = hunk_excluded.lock().unwrap_or_else(|e| e.into_inner());
                paths.iter().filter_map(|rel| map.get(&repo_root.join(rel)).filter(|s| !s.is_empty()).map(|s| (rel.clone(), s.clone()))).collect()
            };
            let split_root = repo_root.clone();
            let (paths, partial): (Vec<String>, Vec<(String, Vec<u8>)>) = cx
                .background_spawn(async move {
                    let (mut full, mut partial) = (Vec::new(), Vec::new());
                    for rel in paths {
                        let Some(set) = excluded.get(&rel) else {
                            full.push(rel);
                            continue;
                        };
                        let raw_base = graphyne_git::head_text(&split_root, &rel).unwrap_or_default();
                        let Ok(raw_current) = graphyne_core::fs_util::read_text_lossy(&split_root.join(&rel)) else {
                            full.push(rel);
                            continue;
                        };
                        let (text, applied, total) =
                            graphyne_git::apply_selected_hunks(&raw_base.replace("\r\n", "\n"), &raw_current.replace("\r\n", "\n"), set);
                        if applied == 0 {
                            continue;
                        }
                        if applied == total {
                            full.push(rel);
                            continue;
                        }
                        // Keep the repository's line endings.
                        let text = if raw_base.contains("\r\n") { text.replace('\n', "\r\n") } else { text };
                        partial.push((rel, text.into_bytes()));
                    }
                    (full, partial)
                })
                .await;
            if paths.is_empty() && partial.is_empty() {
                this.update(cx, |this, cx| {
                    this.busy = false;
                    this.status = Some(("Nothing to commit".into(), true));
                    this.refresh(cx);
                })
                .ok();
                return;
            }
            let scan_root = repo_root.clone();
            let scan_paths = paths.clone();
            let scan_partial: Vec<(String, String)> = partial.iter().map(|(r, d)| (r.clone(), String::from_utf8_lossy(d).into_owned())).collect();
            // Pre-commit gate: scan exactly what would be committed.
            let findings = if force {
                Vec::new()
            } else {
                cx.background_spawn(async move {
                    let mut out = Vec::new();
                    for rel in &scan_paths {
                        let p = scan_root.join(rel);
                        if graphyne_core::fs_util::is_probably_binary(&p) {
                            continue;
                        }
                        if let Ok(text) = std::fs::read_to_string(&p) {
                            for f in graphyne_core::secrets::scan(&text) {
                                out.push((rel.clone(), f));
                            }
                        }
                    }
                    // Partially committed files: scan what is actually staged.
                    for (rel, text) in &scan_partial {
                        for f in graphyne_core::secrets::scan(text) {
                            out.push((rel.clone(), f));
                        }
                    }
                    out
                })
                .await
            };
            if !findings.is_empty() {
                this.update(cx, |this, cx| {
                    this.busy = false;
                    this.status = Some((format!("Commit blocked: {} potential secret(s)", findings.len()).into(), true));
                    this.blocked = findings;
                    cx.notify();
                })
                .ok();
                return;
            }
            let result = cx.background_spawn(async move { graphyne_git::commit_selection(&repo_root, &paths, &partial, &message) }).await;
            this.update(cx, |this, cx| {
                this.busy = false;
                this.blocked.clear();
                match result {
                    Ok(id) => {
                        this.status = Some((format!("Committed {id}").into(), false));
                        this.message.update(cx, |m, cx| m.set_text("", cx));
                    }
                    Err(e) => this.status = Some((format!("{e:#}").into(), true)),
                }
                this.refresh(cx);
            })
            .ok();
        })
        .detach();
    }
}

fn kind_color(kind: ChangeKind) -> gpui::Hsla {
    color(match kind {
        ChangeKind::Modified | ChangeKind::Renamed => c::GIT_MODIFIED,
        ChangeKind::Added => c::GIT_ADDED,
        ChangeKind::Untracked => c::GIT_UNTRACKED,
        ChangeKind::Deleted => c::GIT_DELETED,
        ChangeKind::Conflicted => c::RED,
    })
}

impl Render for GitPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.repo_root.is_none() {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .text_color(color(c::TEXT_DIM))
                .child(icon(Icon::Branch, 28., color(c::TEXT_FAINT)))
                .child("This folder is not a Git repository")
                .into_any_element();
        }
        let root = self.repo_root.clone().unwrap_or_default();
        let checked_count = self.changes.iter().filter(|c| !self.unchecked.contains(&c.rel_path)).count();
        let mut list = div().flex().flex_col().px(px(4.));
        for (ix, ch) in self.changes.iter().enumerate() {
            let checked = !self.unchecked.contains(&ch.rel_path);
            let partial = checked && self.hunk_excluded.lock().unwrap_or_else(|e| e.into_inner()).get(&root.join(&ch.rel_path)).is_some_and(|s| !s.is_empty());
            let abs = root.join(&ch.rel_path);
            let rel = ch.rel_path.clone();
            let rel2 = ch.rel_path.clone();
            let path = root.join(&ch.rel_path);
            let file = ch.rel_path.rsplit('/').next().unwrap_or(&ch.rel_path).to_string();
            let dir = ch.rel_path.strip_suffix(&file).unwrap_or("").trim_end_matches('/').to_string();
            list = list.child(
                div()
                    .id(("change", ix))
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .h(px(26.))
                    .px(px(6.))
                    .rounded(px(5.))
                    .hover(|s| s.bg(color(c::HOVER)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(PanelEvent::OpenDiff(path.clone()))))
                    .child(
                        div()
                            .id(("check", ix))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(14.))
                            .rounded(px(3.))
                            .border_1()
                            .border_color(if checked { color(c::ACCENT) } else { color(c::TEXT_FAINT) })
                            .when(checked && !partial, |d| d.bg(color(c::ACCENT)).child(icon(Icon::Check, 10., color(0xFFFFFF))))
                            // Some blocks left out in the diff view: "partly selected" dash.
                            .when(partial, |d| d.bg(color(c::ACCENT)).child(div().w(px(8.)).h(px(2.)).rounded(px(1.)).bg(color(0xFFFFFF))))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if partial {
                                    // Partly selected → whole file selected again.
                                    this.hunk_excluded.lock().unwrap_or_else(|e| e.into_inner()).remove(&abs);
                                } else if !this.unchecked.remove(&rel) {
                                    this.unchecked.insert(rel.clone());
                                }
                                cx.notify();
                            })),
                    )
                    .child(crate::file_icons::file_icon(std::path::Path::new(&file)))
                    .child(div().text_color(kind_color(ch.kind)).whitespace_nowrap().child(file))
                    .child(div().flex_1().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(color(c::TEXT_DIM)).text_size(px(theme::UI_SMALL)).child(dir))
                    .child(div().text_size(px(11.)).text_color(kind_color(ch.kind)).child(ch.kind.letter()))
                    .tooltip(tooltip(rel2, None)),
            );
        }
        let blocked = self.blocked.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(32.))
                    .px(px(12.))
                    .text_size(px(theme::UI_SMALL))
                    .text_color(color(c::TEXT_MUTED))
                    .child(icon(Icon::Branch, 13., color(c::TEXT_MUTED)))
                    .child(self.branch.clone().unwrap_or_default())
                    .child(div().flex_1())
                    .child(format!("{} changes", self.changes.len()))
                    .child(small_icon_button("git-refresh", Icon::Refresh, color(c::TEXT_MUTED)).on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))),
            )
            .child(div().id("changes").flex_1().min_h(px(0.)).overflow_y_scroll().child(list))
            .when(!blocked.is_empty(), |d| {
                d.child(
                    div()
                        .mx(px(8.))
                        .p(px(8.))
                        .rounded(px(6.))
                        .bg(color_a(c::RED, 0.08))
                        .border_1()
                        .border_color(color_a(c::RED, 0.35))
                        .text_size(px(theme::UI_SMALL))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .text_color(color(c::RED))
                                .child(icon(Icon::Shield, 13., color(c::RED)))
                                .child("Secrets Guard blocked this commit"),
                        )
                        .children(blocked.iter().take(6).map(|(file, f)| {
                            div().text_color(color(c::TEXT_MUTED)).child(format!("{file}:{} — {} ({})", f.line + 1, f.kind, f.preview))
                        }))
                        .child(
                            div()
                                .id("commit-anyway")
                                .mt(px(6.))
                                .text_color(color(c::TEXT))
                                .cursor_pointer()
                                .hover(|s| s.text_color(color(c::RED)))
                                .child("Commit anyway")
                                .on_click(cx.listener(|this, _, _, cx| this.commit(true, cx))),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .p(px(8.))
                    .border_t_1()
                    .border_color(color(c::BORDER))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .h(px(32.))
                            .px(px(8.))
                            .rounded(px(6.))
                            .bg(color(c::CANVAS))
                            .border_1()
                            .border_color(color(c::BORDER))
                            .child(self.message.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .id("commit")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(6.))
                                    .h(px(28.))
                                    .px(px(14.))
                                    .rounded(px(6.))
                                    .bg(color(c::ACCENT))
                                    .text_color(color(0xFFFFFF))
                                    .text_size(px(theme::UI_TEXT))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(color(0x5294F7)))
                                    .child(icon(Icon::Commit, 13., color(0xFFFFFF)))
                                    .child(format!("Commit {checked_count}"))
                                    .on_click(cx.listener(|this, _, _, cx| this.commit(false, cx))),
                            )
                            .children(self.status.clone().map(|(s, err)| {
                                div()
                                    .flex_1()
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_size(px(theme::UI_SMALL))
                                    .text_color(if err { color(c::RED) } else { color(c::TEXT_MUTED) })
                                    .child(s)
                            })),
                    ),
            )
            .into_any_element()
    }
}

// --------------------------------------------------------------------------------- History

pub struct HistoryPanel {
    root: PathBuf,
    commits: Rc<Vec<CommitInfo>>,
    graph: Rc<Vec<GraphRow>>,
    loaded: bool,
}

impl EventEmitter<PanelEvent> for HistoryPanel {}

/// Branch colours of the history graph, cycled by lane.
const LANE_COLORS: [u32; 6] = [c::BLUE, c::GREEN, c::ORANGE, 0xB07FE0, 0x4FC1C9, 0xE07AA6];
const LANE_W: f32 = 12.;
const MAX_LANES: usize = 12;
const HISTORY_ROW_H: f32 = 44.;

impl HistoryPanel {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut p = Self { root, commits: Rc::default(), graph: Rc::default(), loaded: false };
        p.refresh(cx);
        p
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let (commits, graph) = cx
                .background_spawn(async move {
                    let commits = graphyne_git::discover(&root).and_then(|info| graphyne_git::log(&info.root, 2000).ok()).unwrap_or_default();
                    let graph = graphyne_git::graph(&commits);
                    (commits, graph)
                })
                .await;
            this.update(cx, |this, cx| {
                this.commits = Rc::new(commits);
                this.graph = Rc::new(graph);
                this.loaded = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// Draw one row's slice of the commit graph: straight lines for lanes that stay put,
/// S-curves where a line changes lane.
fn paint_graph_row(row: &GraphRow, bounds: gpui::Bounds<gpui::Pixels>, window: &mut Window) {
    let x = |lane: usize| bounds.origin.x + px(1. + lane as f32 * LANE_W + LANE_W / 2.);
    let top = bounds.origin.y;
    let mid = top + px(HISTORY_ROW_H / 2.);
    let bottom = top + bounds.size.height;
    for e in &row.edges {
        if e.from >= MAX_LANES || e.to >= MAX_LANES {
            continue;
        }
        let (a, b, lane) = match e.part {
            EdgePart::Through => (point(x(e.from), top), point(x(e.to), bottom), e.from),
            EdgePart::In => (point(x(e.from), top), point(x(e.to), mid), e.from),
            EdgePart::Out => (point(x(e.from), mid), point(x(e.to), bottom), e.to),
        };
        let mut path = PathBuilder::stroke(px(1.5));
        path.move_to(a);
        if a.x == b.x {
            path.line_to(b);
        } else {
            let my = px((f32::from(a.y) + f32::from(b.y)) * 0.5);
            path.cubic_bezier_to(b, point(a.x, my), point(b.x, my));
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color(LANE_COLORS[lane % LANE_COLORS.len()]));
        }
    }
}

fn relative_time(secs: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(secs);
    let d = (now - secs).max(0);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        86_400..=2_591_999 => format!("{} d ago", d / 86_400),
        _ => format!("{} mo ago", d / 2_592_000),
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.loaded && self.commits.is_empty() {
            return div().size_full().flex().items_center().justify_center().text_color(color(c::TEXT_DIM)).child("No commits yet").into_any_element();
        }
        let commits = self.commits.clone();
        let graph = self.graph.clone();
        let count = commits.len();
        let lanes = graph.iter().map(|r| r.width).max().unwrap_or(1).clamp(1, MAX_LANES);
        let graph_w = px(lanes as f32 * LANE_W + 2.);
        div()
            .size_full()
            .pt(px(4.))
            .child(
                uniform_list("history", count, cx.processor(move |_, range: std::ops::Range<usize>, _, _| {
                    range
                        .map(|ix| {
                            let cm = &commits[ix];
                            let row = graph.get(ix).cloned().unwrap_or_default();
                            let node_lane = row.lane.min(MAX_LANES - 1);
                            let node_color = LANE_COLORS[row.lane % LANE_COLORS.len()];
                            let merge = cm.parent_ids.len() > 1;
                            div()
                                .id(ix)
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .h(px(HISTORY_ROW_H))
                                .w_full()
                                .px(px(8.))
                                .rounded(px(5.))
                                .hover(|s| s.bg(color(c::HOVER)))
                                .child(
                                    div()
                                        .relative()
                                        .flex_none()
                                        .w(graph_w)
                                        .h_full()
                                        .overflow_hidden()
                                        .child(canvas(|_, _, _| (), move |bounds, _, window, _| paint_graph_row(&row, bounds, window)).size_full())
                                        .child(
                                            div()
                                                .absolute()
                                                .left(px(1. + node_lane as f32 * LANE_W + LANE_W / 2. - 5.))
                                                .top(px(HISTORY_ROW_H / 2. - 5.))
                                                .size(px(10.))
                                                .rounded_full()
                                                .border_2()
                                                .border_color(color(node_color))
                                                .bg(color(if merge { node_color } else { c::ISLAND })),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(6.))
                                                .children(cm.refs.iter().take(3).map(|r| {
                                                    div()
                                                        .px(px(5.))
                                                        .rounded(px(4.))
                                                        .bg(color_a(c::GREEN, 0.15))
                                                        .text_color(color(c::GREEN))
                                                        .text_size(px(10.5))
                                                        .child(r.clone())
                                                }))
                                                .child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(color(c::TEXT)).child(cm.summary.clone())),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(11.5))
                                                .text_color(color(c::TEXT_DIM))
                                                .child(format!("{} · {} · {}", cm.short_id, cm.author, relative_time(cm.time))),
                                        ),
                                )
                        })
                        .collect()
                }))
                .px(px(4.))
                .size_full(),
            )
            .into_any_element()
    }
}
