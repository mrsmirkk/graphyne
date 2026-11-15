//! Side-by-side diff of a file against `HEAD` (spec §7): one virtualised list of aligned
//! rows, so both columns always scroll together; changed blocks are joined by smooth
//! connector shapes in the middle gutter, IntelliJ style. Each block has a checkbox that
//! decides whether it goes into the next commit (line-level staging), and changed words
//! inside modified lines are highlighted.

use crate::assets::Icon;
use crate::components::{icon, small_icon_button, tooltip};
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, EventEmitter, FocusHandle, Focusable, HighlightStyle, Hsla, Pixels, ScrollWheelEvent, SharedString,
    StyledText, UniformListScrollHandle, Window, canvas, div, point, px, uniform_list,
};
use graphyne_core::LanguageId;
use graphyne_git::{SideKind, SideRow};
use graphyne_syntax::HighlightKind;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const ROW_H: f32 = 22.;
const GUTTER_W: f32 = 30.;
const NUMBER_W: f32 = 44.;
/// Column at the start of the right side that holds the "include in commit" checkboxes.
const CHECK_W: f32 = 24.;

/// Changed blocks left out of the next commit, per file (by block signature). Shared by
/// the diff views (checkboxes) and the Git tab (commit).
pub type HunkSelection = Arc<Mutex<HashMap<PathBuf, HashSet<u64>>>>;

pub enum DiffEvent {
    OpenFile(PathBuf),
    SelectionChanged,
}

type LineSpans = Vec<Vec<(Range<usize>, HighlightKind)>>;
/// Changed byte ranges of a modified line pair (left, right).
type WordRanges = (Vec<Range<usize>>, Vec<Range<usize>>);

struct Side {
    lines: Vec<SharedString>,
    spans: LineSpans,
}

struct Loaded {
    left: Side,
    right: Side,
    rows: Vec<SideRow>,
    /// Per row: which changed block it belongs to.
    hunk_of_row: Vec<Option<usize>>,
    /// Per block: its signature (stable across unrelated edits).
    hunk_sigs: Vec<u64>,
    words: Vec<Option<WordRanges>>,
    added: usize,
    removed: usize,
    max_cols: usize,
}

pub struct DiffView {
    pub path: PathBuf,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    data: Option<Arc<Loaded>>,
    error: Option<String>,
    /// Row of the changed block the ↑/↓ buttons moved to last.
    last_jump: Option<usize>,
    excluded: HunkSelection,
    /// Shared horizontal offset of both columns.
    scroll_x: f32,
    char_w: f32,
}

impl EventEmitter<DiffEvent> for DiffView {}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl DiffView {
    pub fn new(path: PathBuf, excluded: HunkSelection, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            path,
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            data: None,
            error: None,
            last_jump: None,
            excluded,
            scroll_x: 0.,
            char_w: 8.,
        };
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

    fn excluded_set(&self) -> HashSet<u64> {
        self.excluded.lock().unwrap_or_else(|e| e.into_inner()).get(&self.path).cloned().unwrap_or_default()
    }

    fn toggle_hunk(&mut self, sig: u64, cx: &mut Context<Self>) {
        {
            let mut map = self.excluded.lock().unwrap_or_else(|e| e.into_inner());
            let set = map.entry(self.path.clone()).or_default();
            if !set.remove(&sig) {
                set.insert(sig);
            }
            if set.is_empty() {
                map.remove(&self.path);
            }
        }
        cx.emit(DiffEvent::SelectionChanged);
        cx.notify();
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

    /// Shift+wheel or a horizontal swipe moves both columns sideways.
    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let d = event.delta.pixel_delta(px(ROW_H));
        let dx = if event.modifiers.shift && f32::from(d.x) == 0. { f32::from(d.y) } else { f32::from(d.x) };
        if dx == 0. {
            return;
        }
        let max = self.data.as_ref().map(|d| d.max_cols as f32 * self.char_w - 240.).unwrap_or(0.).max(0.);
        self.scroll_x = (self.scroll_x - dx).clamp(0., max);
        cx.stop_propagation();
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
    let hunk_sigs = graphyne_git::hunk_signatures(&base, &current);
    let mut hunk_of_row = Vec::with_capacity(rows.len());
    let mut block: Option<usize> = None;
    for r in &rows {
        hunk_of_row.push(match r.hunk {
            Some(h) => {
                if h.index == 0 {
                    block = Some(block.map_or(0, |b| b + 1));
                }
                block
            }
            None => None,
        });
    }
    let added = rows.iter().filter(|r| r.kind != SideKind::Same && r.right.is_some()).count();
    let removed = rows.iter().filter(|r| r.kind != SideKind::Same && r.left.is_some()).count();
    let lang = LanguageId::from_path(path);
    let (left, right) = (side(&base, lang), side(&current, lang));
    let words = rows
        .iter()
        .map(|r| match (r.kind, r.left, r.right) {
            (SideKind::Modified, Some(l), Some(n)) => {
                let (a, b) = (left.lines.get(l as usize), right.lines.get(n as usize));
                a.zip(b).map(|(a, b)| graphyne_git::word_diff(a, b))
            }
            _ => None,
        })
        .collect();
    let max_cols = left.lines.iter().chain(right.lines.iter()).map(|l| l.chars().count()).max().unwrap_or(0);
    Ok(Loaded { left, right, rows, hunk_of_row, hunk_sigs, words, added, removed, max_cols })
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

/// Syntax colours and changed-word backgrounds merged into non-overlapping runs.
fn merge_styles(len: usize, syntax: &[(Range<usize>, HighlightKind)], words: &[Range<usize>], word_bg: Hsla) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut cuts: Vec<usize> = vec![0, len];
    for (r, _) in syntax {
        cuts.extend([r.start.min(len), r.end.min(len)]);
    }
    for r in words {
        cuts.extend([r.start.min(len), r.end.min(len)]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a >= b {
            continue;
        }
        let kind = syntax.iter().find(|(r, _)| r.start <= a && a < r.end).map(|(_, k)| *k);
        let in_word = words.iter().any(|r| r.start <= a && a < r.end);
        if kind.is_none() && !in_word {
            continue;
        }
        out.push((
            a..b,
            HighlightStyle {
                color: kind.map(theme::syntax),
                font_style: kind.filter(|k| theme::is_italic(*k)).map(|_| gpui::FontStyle::Italic),
                background_color: in_word.then_some(word_bg),
                ..Default::default()
            },
        ));
    }
    out
}

/// Connector slice for one row: the block's shape is the area between the block top and a
/// smooth curve from the left extent (old lines) to the right extent (new lines).
fn paint_connector(row: &SideRow, fill: Hsla, bounds: Bounds<Pixels>, window: &mut Window) {
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
        window.paint_path(path, fill);
    }
}

struct Cell<'a> {
    side: &'a Side,
    line: Option<u32>,
    kind: SideKind,
    words: &'a [Range<usize>],
    is_left: bool,
    excluded: bool,
    scroll_x: f32,
    /// Right side only: the checkbox column (with a checkbox on a block's first row).
    check: Option<Option<gpui::AnyElement>>,
}

fn line_cell(cell: Cell<'_>) -> impl IntoElement {
    let Cell { side, line, kind, words, is_left, excluded, scroll_x, check } = cell;
    let changed = kind != SideKind::Same;
    // A block left out of the commit is drawn grey.
    let tint = if excluded { c::TEXT_FAINT } else { kind_color(kind) };
    let bg: Option<Hsla> = match (line, changed) {
        (Some(_), true) => Some(color_a(tint, 0.14)),
        (None, _) => Some(color_a(c::TEXT_FAINT, 0.07)),
        _ => None,
    };
    let number: SharedString = line.map(|l| (l + 1).to_string()).unwrap_or_default().into();
    let text = line.and_then(|l| side.lines.get(l as usize)).cloned().unwrap_or_default();
    let syntax: &[(Range<usize>, HighlightKind)] = line.and_then(|l| side.spans.get(l as usize)).map(Vec::as_slice).unwrap_or(&[]);
    let highlights = merge_styles(text.len(), syntax, words, color_a(tint, 0.38));
    div()
        .flex()
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .items_center()
        .when_some(bg, |d, bg| d.bg(bg))
        .when(changed && line.is_some() && is_left, |d| d.child(div().w(px(3.)).h_full().bg(color(tint))))
        .when_some(check, |d, checkbox| d.child(div().flex_none().w(px(CHECK_W)).h_full().flex().items_center().justify_center().children(checkbox)))
        .child(div().flex_none().w(px(NUMBER_W)).pr(px(10.)).text_right().text_color(color(c::TEXT_FAINT)).child(number))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .flex()
                .items_center()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_color(theme::syntax(HighlightKind::Plain))
                .when(excluded, |d| d.opacity(0.55))
                .child(div().relative().left(px(-scroll_x)).child(StyledText::new(text).with_highlights(highlights))),
        )
}

impl Render for DiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_id = window.text_system().resolve_font(&gpui::font(theme::fonts().code));
        self.char_w = window.text_system().advance(font_id, px(theme::code_size()), 'm').map(|s| f32::from(s.width)).unwrap_or(8.);
        let header_label = |t: &'static str, indent: f32| div().flex_1().pl(px(indent)).text_color(color(c::TEXT_DIM)).child(t);
        let (added, removed) = self.data.as_ref().map(|d| (d.added, d.removed)).unwrap_or_default();
        let excluded = self.excluded_set();
        let total_blocks = self.data.as_ref().map(|d| d.hunk_sigs.len()).unwrap_or(0);
        let skipped = self.data.as_ref().map(|d| d.hunk_sigs.iter().filter(|s| excluded.contains(s)).count()).unwrap_or(0);
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
            .child(header_label("HEAD", NUMBER_W))
            .child(div().w(px(GUTTER_W)))
            .child(header_label("Working tree", NUMBER_W + CHECK_W))
            .when(skipped > 0, |d| d.child(div().text_color(color(c::TEXT_DIM)).child(format!("{} of {total_blocks} changes in commit", total_blocks - skipped))))
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
                let scroll_x = self.scroll_x;
                uniform_list("diff-rows", data.rows.len(), cx.processor(move |_, range: Range<usize>, _, cx| {
                    range
                        .map(|ix| {
                            let row = data.rows[ix];
                            let block = data.hunk_of_row[ix];
                            let sig = block.and_then(|b| data.hunk_sigs.get(b).copied());
                            let is_excluded = sig.is_some_and(|s| excluded.contains(&s));
                            let fill = color_a(if is_excluded { c::TEXT_FAINT } else { kind_color(row.kind) }, 0.30);
                            let (wl, wr): (&[Range<usize>], &[Range<usize>]) = match &data.words[ix] {
                                Some((l, r)) => (l, r),
                                None => (&[], &[]),
                            };
                            // Checkbox on the first row of each block: include it in the next
                            // commit. Same look as the Git tab's file checkboxes.
                            let checkbox = (row.hunk.is_some_and(|h| h.index == 0)).then_some(sig).flatten().map(|sig| {
                                div()
                                    .id(("hunk-check", ix))
                                    .size(px(14.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(3.))
                                    .border_1()
                                    .cursor_pointer()
                                    .when(!is_excluded, |d| d.bg(color(c::ACCENT)).border_color(color(c::ACCENT)).child(icon(Icon::Check, 10., color(0xFFFFFF))))
                                    .when(is_excluded, |d| d.border_color(color(c::TEXT_FAINT)).hover(|s| s.border_color(color(c::TEXT_MUTED))))
                                    .tooltip(tooltip(if is_excluded { "Not in the next commit — click to include" } else { "In the next commit — click to leave out" }, None))
                                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_hunk(sig, cx)))
                                    .into_any_element()
                            });
                            div()
                                .id(ix)
                                .flex()
                                .w_full()
                                .h(px(ROW_H))
                                .child(line_cell(Cell { side: &data.left, line: row.left, kind: row.kind, words: wl, is_left: true, excluded: is_excluded, scroll_x, check: None }))
                                .child(
                                    div()
                                        .flex_none()
                                        .w(px(GUTTER_W))
                                        .h_full()
                                        .child(canvas(|_, _, _| (), move |bounds, _, window, _| paint_connector(&row, fill, bounds, window)).size_full()),
                                )
                                .child(line_cell(Cell {
                                    side: &data.right,
                                    line: row.right,
                                    kind: row.kind,
                                    words: wr,
                                    is_left: false,
                                    excluded: is_excluded,
                                    scroll_x,
                                    check: Some(checkbox),
                                }))
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
            .text_size(px(theme::code_size()))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(header.font_family(theme::fonts().ui))
            .child(body)
    }
}
