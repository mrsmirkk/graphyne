//! Side-by-side diff of a file against `HEAD` (spec §7): one virtualised list of aligned
//! rows, so both columns always scroll together; changed blocks are joined by smooth
//! connector shapes in the middle gutter, IntelliJ style.

use crate::assets::Icon;
use crate::components::{small_icon_button, tooltip};
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, EventEmitter, FocusHandle, Focusable, HighlightStyle, Hsla, Pixels, SharedString, StyledText,
    UniformListScrollHandle, Window, canvas, div, point, px, uniform_list,
};
use graphyne_core::LanguageId;
use graphyne_git::{SideKind, SideRow};
use graphyne_syntax::HighlightKind;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ROW_H: f32 = 22.;
const GUTTER_W: f32 = 30.;
const NUMBER_W: f32 = 44.;

pub enum DiffEvent {
    OpenFile(PathBuf),
}

type LineSpans = Vec<Vec<(Range<usize>, HighlightKind)>>;

struct Side {
    lines: Vec<SharedString>,
    spans: LineSpans,
}

struct Loaded {
    left: Side,
    right: Side,
    rows: Vec<SideRow>,
    added: usize,
    removed: usize,
}

pub struct DiffView {
    pub path: PathBuf,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    data: Option<Arc<Loaded>>,
    error: Option<String>,
    /// Row of the changed block the ↑/↓ buttons moved to last.
    last_jump: Option<usize>,
}

impl EventEmitter<DiffEvent> for DiffView {}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl DiffView {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut view = Self { path, focus_handle: cx.focus_handle(), scroll: UniformListScrollHandle::new(), data: None, error: None, last_jump: None };
        view.refresh(cx);
        view
    }

    pub fn title(&self) -> SharedString {
        self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default().into()
    }

    /// Reload HEAD and working-tree text (after saves, external edits, commits).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        cx.spawn(async move |this, cx| {
            let loaded = cx.background_spawn(async move { load(&path) }).await;
            this.update(cx, |this, cx| {
                match loaded {
                    Ok(data) => {
                        this.data = Some(Arc::new(data));
                        this.error = None;
                        this.last_jump = None;
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Scroll to the next (`dir` = 1) or previous (-1) changed block.
    fn jump(&mut self, dir: i32, cx: &mut Context<Self>) {
        let Some(data) = &self.data else { return };
        let starts: Vec<usize> = data.rows.iter().enumerate().filter(|(_, r)| r.hunk.is_some_and(|h| h.index == 0)).map(|(i, _)| i).collect();
        if starts.is_empty() {
            return;
        }
        // Step from the block we jumped to last; before the first jump, from the top row.
        let reference = self.last_jump.map(|j| j as isize).unwrap_or_else(|| {
            let top = self.scroll.0.borrow().base_handle.offset().y;
            ((-f32::from(top)) / ROW_H).round() as isize - 1
        });
        let target = if dir > 0 {
            starts.iter().copied().find(|&s| s as isize > reference).unwrap_or(starts[0])
        } else {
            starts.iter().rev().copied().find(|&s| (s as isize) < reference).unwrap_or(*starts.last().unwrap())
        };
        self.last_jump = Some(target);
        // Strict: also scroll when the block is already on screen; 3 rows of context above.
        self.scroll.scroll_to_item_strict(target.saturating_sub(3), gpui::ScrollStrategy::Top);
        cx.notify();
    }
}

fn load(path: &Path) -> Result<Loaded, String> {
    let info = graphyne_git::discover(path).ok_or("The file is not in a git repository")?;
    let rel = graphyne_git::rel_path(&info.root, path).ok_or("The file is outside the repository")?;
    let base = graphyne_git::head_text(&info.root, &rel).unwrap_or_default().replace("\r\n", "\n");
    let current = if path.exists() {
        graphyne_core::fs_util::read_text_lossy(path).map_err(|e| format!("{e:#}"))?.replace("\r\n", "\n")
    } else {
        String::new()
    };
    let rows = graphyne_git::side_by_side(&base, &current);
    let added = rows.iter().filter(|r| r.kind != SideKind::Same && r.right.is_some()).count();
    let removed = rows.iter().filter(|r| r.kind != SideKind::Same && r.left.is_some()).count();
    let lang = LanguageId::from_path(path);
    Ok(Loaded { left: side(&base, lang), right: side(&current, lang), rows, added, removed })
}

/// Lines plus per-line syntax spans (skipped for very large files).
fn side(text: &str, lang: LanguageId) -> Side {
    let lines: Vec<SharedString> = text.lines().map(|l| SharedString::from(l.to_string())).collect();
    let mut spans: LineSpans = vec![Vec::new(); lines.len()];
    if text.len() < 2 * 1024 * 1024 && graphyne_syntax::supports(lang) {
        let rope = ropey::Rope::from_str(text);
        if let Some(tree) = graphyne_syntax::parse(lang, &rope, None) {
            let mut starts = Vec::with_capacity(lines.len());
            let mut off = 0;
            for l in text.split_inclusive('\n') {
                starts.push(off);
                off += l.len();
            }
            for s in graphyne_syntax::highlight(lang, &tree, &rope, 0..text.len()) {
                let mut line = starts.partition_point(|&st| st <= s.range.start).saturating_sub(1);
                let mut start = s.range.start;
                while line < lines.len() && start < s.range.end {
                    let line_start = starts[line];
                    let line_len = lines[line].len();
                    let end = s.range.end.min(line_start + line_len);
                    if end > start {
                        spans[line].push((start - line_start..end - line_start, s.kind));
                    }
                    line += 1;
                    start = starts.get(line).copied().unwrap_or(usize::MAX);
                }
            }
        }
    }
    Side { lines, spans }
}

fn kind_color(kind: SideKind) -> u32 {
    match kind {
        SideKind::Same => c::TEXT,
        SideKind::Modified => c::GIT_MODIFIED,
        SideKind::Added => c::GIT_ADDED,
        SideKind::Deleted => c::RED,
    }
}

/// Connector slice for one row: the block's shape is the area between the block top and a
/// smooth curve from the left extent (old lines) to the right extent (new lines).
fn paint_connector(row: &SideRow, bounds: Bounds<Pixels>, window: &mut Window) {
    let Some(h) = row.hunk else { return };
    let w = f32::from(bounds.size.width);
    let row_h = f32::from(bounds.size.height);
    let smooth = |t: f32| t * t * (3. - 2. * t);
    let bottom_at = |x: f32| {
        let t = smooth(x / w);
        ((h.old_len as f32 + (h.new_len as f32 - h.old_len as f32) * t) - h.index as f32) * row_h
    };
    let mut path = gpui::PathBuilder::fill();
    path.move_to(bounds.origin);
    path.line_to(point(bounds.origin.x + px(w), bounds.origin.y));
    const STEPS: usize = 16;
    for i in (0..=STEPS).rev() {
        let x = w * i as f32 / STEPS as f32;
        let y = bottom_at(x).clamp(0., row_h);
        path.line_to(point(bounds.origin.x + px(x), bounds.origin.y + px(y)));
    }
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color_a(kind_color(row.kind), 0.30));
    }
}

fn line_cell(side: &Side, line: Option<u32>, kind: SideKind, is_left: bool) -> impl IntoElement {
    let changed = kind != SideKind::Same;
    let bg: Option<Hsla> = match (line, changed) {
        (Some(_), true) => Some(color_a(kind_color(kind), 0.14)),
        (None, _) => Some(color_a(c::TEXT_FAINT, 0.07)),
        _ => None,
    };
    let number: SharedString = line.map(|l| (l + 1).to_string()).unwrap_or_default().into();
    let text = line.and_then(|l| side.lines.get(l as usize)).cloned().unwrap_or_default();
    let highlights: Vec<(Range<usize>, HighlightStyle)> = line
        .and_then(|l| side.spans.get(l as usize))
        .map(|spans| {
            spans
                .iter()
                .map(|(r, k)| {
                    let style = HighlightStyle {
                        color: Some(theme::syntax(*k)),
                        font_style: theme::is_italic(*k).then_some(gpui::FontStyle::Italic),
                        ..Default::default()
                    };
                    (r.clone(), style)
                })
                .collect()
        })
        .unwrap_or_default();
    div()
        .flex()
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .items_center()
        .when_some(bg, |d, bg| d.bg(bg))
        .when(changed && line.is_some(), |d| {
            // Thin colour bar on the outer edge, like the editor's gutter markers.
            let bar = div().w(px(3.)).h_full().bg(color(kind_color(kind)));
            if is_left { d.child(bar) } else { d }
        })
        .child(div().flex_none().w(px(NUMBER_W)).pr(px(10.)).text_right().text_color(color(c::TEXT_FAINT)).child(number))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_color(theme::syntax(HighlightKind::Plain))
                .child(StyledText::new(text).with_highlights(highlights)),
        )
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header_label = |t: &'static str| div().flex_1().pl(px(NUMBER_W)).text_color(color(c::TEXT_DIM)).child(t);
        let (added, removed) = self.data.as_ref().map(|d| (d.added, d.removed)).unwrap_or_default();
        let path = self.path.clone();
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(32.))
            .px(px(6.))
            .gap(px(4.))
            .border_b_1()
            .border_color(color(c::BORDER))
            .text_size(px(theme::UI_SMALL))
            .child(header_label("HEAD"))
            .child(div().w(px(GUTTER_W)))
            .child(header_label("Working tree"))
            .child(div().text_color(color(c::GIT_ADDED)).child(format!("+{added}")))
            .child(div().text_color(color(c::RED)).child(format!("−{removed}")))
            .child(small_icon_button("diff-prev", Icon::ChevronUp, color(c::TEXT_MUTED)).tooltip(tooltip("Previous change", None)).on_click(cx.listener(|this, _, _, cx| this.jump(-1, cx))))
            .child(small_icon_button("diff-next", Icon::ChevronDown, color(c::TEXT_MUTED)).tooltip(tooltip("Next change", None)).on_click(cx.listener(|this, _, _, cx| this.jump(1, cx))))
            .child(
                small_icon_button("diff-open", Icon::File, color(c::TEXT_MUTED))
                    .tooltip(tooltip("Open file", None))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(DiffEvent::OpenFile(path.clone())))),
            );
        let body = match (&self.data, &self.error) {
            (_, Some(err)) => div().flex_1().flex().items_center().justify_center().text_color(color(c::TEXT_DIM)).child(err.clone()).into_any_element(),
            (None, None) => div().flex_1().into_any_element(),
            (Some(data), None) if data.rows.iter().all(|r| r.kind == SideKind::Same) => {
                div().flex_1().flex().items_center().justify_center().text_color(color(c::TEXT_DIM)).child("No changes against HEAD").into_any_element()
            }
            (Some(data), None) => {
                let data = data.clone();
                uniform_list("diff-rows", data.rows.len(), cx.processor(move |_, range: Range<usize>, _, _| {
                    range
                        .map(|ix| {
                            let row = data.rows[ix];
                            div()
                                .id(ix)
                                .flex()
                                .w_full()
                                .h(px(ROW_H))
                                .child(line_cell(&data.left, row.left, row.kind, true))
                                .child(div().flex_none().w(px(GUTTER_W)).h_full().child(canvas(|_, _, _| (), move |bounds, _, window, _| paint_connector(&row, bounds, window)).size_full()))
                                .child(line_cell(&data.right, row.right, row.kind, false))
                        })
                        .collect()
                }))
                .track_scroll(self.scroll.clone())
                .flex_1()
                .into_any_element()
            }
        };
        div()
            .id("diff-view")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .font_family(theme::fonts().code)
            .text_size(px(theme::CODE_SIZE))
            .child(header.font_family(theme::fonts().ui))
            .child(body)
    }
}
