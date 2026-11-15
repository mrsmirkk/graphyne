//! The code editor view.
//!
//! State (`Buffer`, selection, syntax tree, analysis results) lives in [`Editor`]; drawing is
//! done by a `canvas` element that only shapes the visible rows. All I/O (save, git base,
//! parsing, diff, secrets scan) runs on background executors.

use crate::actions::editor::*;
use crate::assets::Icon;
use crate::components::{highlighted_label, icon};
use crate::debugger::{BpKind, BpOpts};
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementInputHandler, Entity, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, FontStyle, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollWheelEvent, ShapedLine, SharedString, Task, TextRun, UTF16Selection,
    UnderlineStyle, Window, anchored, canvas, deferred, div, fill, point, px, size,
};
use graphyne_core::buffer::is_word_char;
use graphyne_core::fuzzy::fuzzy_filter;
use graphyne_core::secrets::SecretFinding;
use graphyne_core::{Buffer, EditDelta, LanguageId, Selection};
use graphyne_git::{DiffHunk, LineChange};
use graphyne_syntax::{HighlightKind, OutlineItem, SymbolKind, Tree};
use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

mod lsp;
mod markdown;

use lsp::LspState;

/// Inline breakpoint editor (right click on a line number / Ctrl+Shift+F8).
struct BpEdit {
    line: usize,
    anchor: Point<Pixels>,
    condition: Entity<TextInput>,
    log: Entity<TextInput>,
    _subs: Vec<gpui::Subscription>,
}

const TAB_WIDTH: usize = 4;
const MAX_RENDER_COLS: usize = 4000;
/// Above this size a buffer is edited as plain text: no syntax tree, git gutter diff or
/// full-text analysis (each would re-scan the whole file after every keystroke).
const LARGE_FILE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub enum EditorEvent {
    Saved,
    Edited,
    /// Go to definition result (path, line, UTF-16 column).
    OpenLocation(PathBuf, usize, usize),
    /// Code-action edits that touch other files.
    ApplyEdits(Vec<graphyne_lsp::model::FileEdits>),
    Notify(String),
}

#[derive(Clone)]
enum RowKind {
    /// `col_to_byte`: caret byte per column (before inlay hints); `char_start`: where each
    /// char's glyph begins (after inlay hints).
    Line { row: usize, line: ShapedLine, col_to_byte: Arc<Vec<usize>>, char_start: Arc<Vec<usize>> },
    Lens { label: SharedString, indent_x: Pixels },
}

#[derive(Clone)]
struct RowLayout {
    y: Pixels,
    kind: RowKind,
}

#[derive(Clone)]
struct LayoutCache {
    bounds: Bounds<Pixels>,
    text_x: Pixels,
    gutter_width: Pixels,
    line_height: Pixels,
    char_width: Pixels,
    rows: Vec<RowLayout>,
    viewport_rows: usize,
}

#[derive(Clone)]
struct CompletionEntry {
    label: String,
    indices: Vec<u32>,
    detail: Option<String>,
    kind: u8,
    lsp: Option<graphyne_lsp::model::Completion>,
}

struct Completion {
    word_start: usize,
    items: Vec<CompletionEntry>,
    selected: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragMode {
    Char,
    Word,
    Line,
}

pub struct Editor {
    focus_handle: FocusHandle,
    pub path: Option<PathBuf>,
    pub project_root: PathBuf,
    pub language: LanguageId,
    pub buffer: Buffer,
    sel: Selection,
    goal_col: Option<usize>,
    scroll_y: Pixels,
    scroll_x: Pixels,
    autoscroll: bool,
    center_on_cursor: bool,
    /// Session restore: the buffer line to keep at the top until the user scrolls, types
    /// or moves the cursor. `N usages` rows appear later and would push the text down.
    restore_top: Option<f32>,
    /// Foldable regions from the syntax tree: (first row, last row hidden when folded).
    fold_ranges: Arc<Vec<(usize, usize)>>,
    /// First rows of the folded regions.
    folded: BTreeSet<usize>,
    /// Rows actually hidden: the outermost folded regions, (header row, last hidden row).
    hidden: Vec<(usize, usize)>,
    /// Fold arrows for unfolded regions show while the mouse is over the gutter.
    mouse_in_gutter: bool,
    tree: Option<Tree>,
    reparse_task: Option<Task<()>>,
    analysis_task: Option<Task<()>>,
    pub outline: Arc<Vec<OutlineItem>>,
    lenses: Arc<Vec<(usize, SharedString)>>,
    /// Same-file identifier counts: the `N usages` fallback without a language server.
    ident_counts: Arc<HashMap<String, u32>>,
    words: Arc<Vec<String>>,
    pub syntax_errors: Arc<Vec<Range<usize>>>,
    base_text: Option<Arc<String>>,
    pub hunks: Arc<Vec<DiffHunk>>,
    diff_popup: Option<usize>,
    bp_edit: Option<BpEdit>,
    pub secrets: Arc<Vec<SecretFinding>>,
    expand_stack: Vec<Selection>,
    completion: Option<Completion>,
    autosave_task: Option<Task<()>>,
    pub save_error: Option<String>,
    layout: Option<LayoutCache>,
    marked_range: Option<Range<usize>>,
    drag: Option<(DragMode, usize)>,
    cursor_visible: bool,
    blink_epoch: usize,
    _blink_task: Option<Task<()>>,
    lsp: LspState,
    debugger: Option<Entity<crate::debugger::Debugger>>,
    _debug_sub: Option<gpui::Subscription>,
}

impl EventEmitter<EditorEvent> for Editor {}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Editor {
    pub fn new(
        path: Option<PathBuf>,
        buffer: Buffer,
        project_root: PathBuf,
        lsp_store: Option<Entity<crate::lsp_store::LspStore>>,
        debugger: Option<Entity<crate::debugger::Debugger>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let debug_sub = debugger.as_ref().map(|d| cx.observe(d, |_, _, cx| cx.notify()));
        let language = path.as_deref().map(LanguageId::from_path).unwrap_or(LanguageId::PlainText);
        let mut editor = Self {
            focus_handle: cx.focus_handle(),
            path,
            project_root,
            language,
            buffer,
            sel: Selection::caret(0),
            goal_col: None,
            scroll_y: px(0.),
            scroll_x: px(0.),
            autoscroll: false,
            center_on_cursor: false,
            restore_top: None,
            fold_ranges: Arc::new(Vec::new()),
            folded: BTreeSet::new(),
            hidden: Vec::new(),
            mouse_in_gutter: false,
            tree: None,
            reparse_task: None,
            analysis_task: None,
            outline: Arc::new(Vec::new()),
            lenses: Arc::new(Vec::new()),
            ident_counts: Arc::new(HashMap::new()),
            words: Arc::new(Vec::new()),
            syntax_errors: Arc::new(Vec::new()),
            base_text: None,
            hunks: Arc::new(Vec::new()),
            diff_popup: None,
            bp_edit: None,
            secrets: Arc::new(Vec::new()),
            expand_stack: Vec::new(),
            completion: None,
            autosave_task: None,
            save_error: None,
            layout: None,
            marked_range: None,
            drag: None,
            cursor_visible: true,
            blink_epoch: 0,
            _blink_task: None,
            lsp: LspState::new(lsp_store),
            debugger,
            _debug_sub: debug_sub,
        };
        editor.lsp_init(cx);
        editor.schedule_reparse(cx);
        editor.load_git_base(cx);
        editor.schedule_analysis(Duration::from_millis(50), cx);
        editor
    }

    // ----- public accessors (status bar, structure view, workspace) -----

    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled".into())
    }

    pub fn cursor_row_col(&self) -> (usize, usize) {
        self.buffer.row_col(self.sel.head)
    }

    pub fn selection_len(&self) -> usize {
        self.sel.range().len()
    }

    pub fn is_dirty(&self) -> bool {
        self.buffer.is_dirty()
    }

    /// (errors, warnings): language-server diagnostics when attached, tree-sitter syntax
    /// errors otherwise; Secrets Guard findings count as warnings.
    pub fn problem_count(&self) -> (usize, usize) {
        match self.lsp_problem_count() {
            Some((e, w)) => (e, w + self.secrets.len()),
            None => (self.syntax_errors.len(), self.secrets.len()),
        }
    }

    pub fn breadcrumbs(&self) -> Vec<String> {
        match &self.tree {
            Some(tree) => {
                let byte = self.buffer.char_to_byte(self.sel.head);
                graphyne_syntax::enclosing_symbols(tree, self.buffer.rope(), byte)
            }
            None => Vec::new(),
        }
    }

    /// For the session: (cursor, selection anchor, first visible line, horizontal scroll).
    /// Lines are buffer lines, so the `N usages` rows above functions don't skew them.
    pub fn view_state(&self) -> ((usize, usize), (usize, usize), f32, f32) {
        let line_height = self.layout.as_ref().map(|l| f32::from(l.line_height)).unwrap_or_else(theme::code_line_height).max(1.);
        let display = f32::from(self.scroll_y) / line_height;
        let target = display.floor() as usize;
        let (mut lo, mut hi) = (0usize, self.buffer.len_lines());
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.display_row(mid) <= target { lo = mid + 1 } else { hi = mid }
        }
        let row = lo.saturating_sub(1);
        let top = row as f32 + (display - self.display_row(row) as f32).clamp(0., 0.99);
        (self.buffer.row_col(self.sel.head), self.buffer.row_col(self.sel.anchor), top, f32::from(self.scroll_x))
    }

    /// Put back what [`Self::view_state`] saved; positions past the end are clamped.
    pub fn restore_view(&mut self, cursor: (usize, usize), anchor: (usize, usize), top_line: f32, scroll_x: f32, cx: &mut Context<Self>) {
        self.sel = Selection { anchor: self.buffer.char_at(anchor.0, anchor.1), head: self.buffer.char_at(cursor.0, cursor.1) };
        self.goal_col = None;
        self.restore_top = Some(top_line.max(0.));
        self.apply_restore_top();
        self.scroll_x = px(scroll_x.max(0.));
        self.autoscroll = false;
        self.center_on_cursor = false;
        cx.notify();
    }

    fn apply_restore_top(&mut self) {
        if let Some(top) = self.restore_top {
            let display = self.display_row(top.floor() as usize) as f32 + top.fract();
            self.scroll_y = px(display * theme::code_line_height());
        }
    }

    pub fn go_to(&mut self, row: usize, col: usize, window: &mut Window, cx: &mut Context<Self>) {
        let pos = self.buffer.char_at(row, col);
        self.sel = Selection::caret(pos);
        self.goal_col = None;
        self.center_on_cursor = true;
        self.autoscroll = true;
        window.focus(&self.focus_handle);
        cx.notify();
    }

    /// Reload from disk after an external change (only when there are no unsaved edits).
    pub fn reload_from_disk(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else { return };
        if self.buffer.is_dirty() {
            return;
        }
        let version = self.buffer.version();
        cx.spawn(async move |this, cx| {
            let Ok(text) = cx.background_spawn(async move { graphyne_core::fs_util::read_text_lossy(&path) }).await else { return };
            this.update(cx, |this, cx| {
                if this.buffer.version() != version || this.buffer.is_dirty() || this.buffer.text_for_save() == text {
                    return;
                }
                this.buffer.replace_all(&text);
                let v = this.buffer.version();
                this.buffer.mark_saved(v);
                this.sel = Selection::caret(this.sel.head.min(this.buffer.len_chars()));
                this.tree = None;
                this.lsp.hints = Arc::new(Vec::new());
                this.schedule_reparse(cx);
                this.schedule_analysis(Duration::ZERO, cx);
                this.after_lsp_edit(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ----- background work -----

    fn schedule_reparse(&mut self, cx: &mut Context<Self>) {
        if !graphyne_syntax::supports(self.language) || self.is_large() {
            return;
        }
        let rope = self.buffer.rope().clone();
        let version = self.buffer.version();
        let old = self.tree.clone();
        let lang = self.language;
        let usages = self.lsp.usages.clone();
        self.reparse_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let tree = graphyne_syntax::parse(lang, &rope, old.as_ref())?;
                    let outline = graphyne_syntax::outline(&tree, &rope);
                    let counts = if rope.len_bytes() < 8 * 1024 * 1024 {
                        graphyne_syntax::identifier_counts(&tree, &rope)
                    } else {
                        HashMap::new()
                    };
                    let errors = collect_errors(&tree);
                    let lenses = build_lenses(&outline, &counts, &usages);
                    let folds = graphyne_syntax::fold_ranges(&tree, &rope);
                    Some((tree, outline, errors, lenses, counts, folds))
                })
                .await;
            let Some((tree, outline, errors, lenses, counts, folds)) = result else { return };
            this.update(cx, |this, cx| {
                if this.buffer.version() == version {
                    this.tree = Some(tree);
                    this.outline = Arc::new(outline);
                    this.syntax_errors = Arc::new(errors);
                    this.lenses = Arc::new(lenses);
                    this.ident_counts = Arc::new(counts);
                    this.fold_ranges = Arc::new(folds);
                    this.rebuild_hidden();
                    this.apply_restore_top();
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn load_git_base(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else { return };
        if self.is_large() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let base = cx
                .background_spawn(async move {
                    let info = graphyne_git::discover(&path)?;
                    let rel = graphyne_git::rel_path(&info.root, &path)?;
                    graphyne_git::head_text(&info.root, &rel)
                })
                .await;
            this.update(cx, |this, cx| {
                this.base_text = base.map(Arc::new);
                this.schedule_analysis(Duration::ZERO, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Git line diff + Secrets Guard + completion word index, debounced.
    fn schedule_analysis(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let rope = self.buffer.rope().clone();
        let base = self.base_text.clone();
        let version = self.buffer.version();
        self.analysis_task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let (hunks, secrets, words) = cx
                .background_spawn(async move {
                    // Big files: only the head is scanned and there is no gutter diff.
                    let large = rope.len_bytes() > LARGE_FILE_BYTES;
                    let text = if large { rope.slice(..rope.byte_to_char(2 * 1024 * 1024)).to_string() } else { rope.to_string() };
                    let hunks = if large { Vec::new() } else { base.map(|b| graphyne_git::line_diff(&b, &text)).unwrap_or_default() };
                    let scan_slice = &text[..floor_char_boundary(&text, 2 * 1024 * 1024)];
                    let secrets = graphyne_core::secrets::scan(scan_slice);
                    let words = collect_words(scan_slice);
                    (hunks, secrets, words)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.buffer.version() == version {
                    this.hunks = Arc::new(hunks);
                    this.secrets = Arc::new(secrets);
                    this.words = Arc::new(words);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        if self.path.is_none() {
            return;
        }
        self.autosave_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(theme::autosave_delay()).await;
            // Detach: the write must outlive this debounce task (which a newer edit may cancel).
            if let Ok(Some(task)) = this.update(cx, |this, cx| this.write_to_disk(cx)) {
                task.detach();
            }
        }));
    }

    /// Atomic save on a background thread (cancels a pending debounce). `None` = nothing to do.
    pub fn save(&mut self, cx: &mut Context<Self>) -> Option<Task<()>> {
        self.autosave_task = None;
        self.write_to_disk(cx)
    }

    fn write_to_disk(&mut self, cx: &mut Context<Self>) -> Option<Task<()>> {
        let path = self.path.clone()?;
        if !self.buffer.is_dirty() {
            return None;
        }
        // Rope snapshots are O(1); the (possibly huge) string is built off the UI thread.
        let rope = self.buffer.rope().clone();
        let line_ending = self.buffer.line_ending();
        let version = self.buffer.version();
        Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let text = Buffer::render_for_save(&rope, line_ending);
                    graphyne_core::fs_util::atomic_write(&path, text.as_bytes())
                })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.buffer.mark_saved(version);
                        this.save_error = None;
                        this.lsp_did_save();
                        cx.emit(EditorEvent::Saved);
                    }
                    Err(err) => this.save_error = Some(format!("{err:#}")),
                }
                cx.notify();
            })
            .ok();
        }))
    }

    fn pause_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = true;
        self.blink_epoch += 1;
        let epoch = self.blink_epoch;
        self._blink_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(600)).await;
            loop {
                let keep = this
                    .update(cx, |this, cx| {
                        if this.blink_epoch != epoch {
                            return false;
                        }
                        this.cursor_visible = !this.cursor_visible;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(530)).await;
            }
        }));
    }

    // ----- edit plumbing -----

    fn after_edit(&mut self, delta: EditDelta, cx: &mut Context<Self>) {
        self.after_edits(&[delta], cx);
    }

    fn after_edits(&mut self, deltas: &[EditDelta], cx: &mut Context<Self>) {
        if let (Some(debugger), Some(path)) = (self.debugger.clone(), self.path.clone()) {
            for d in deltas {
                let (start, old_end, new_end) = (d.start_point.row, d.old_end_point.row, d.new_end_point.row);
                if old_end != new_end || old_end != start {
                    debugger.update(cx, |dbg, cx| dbg.shift_breakpoints(&path, start, old_end, new_end, cx));
                }
            }
        }
        if let Some(tree) = self.tree.as_mut() {
            for delta in deltas {
                graphyne_syntax::apply_edit(tree, delta);
            }
        }
        for d in deltas {
            self.shift_folds(d.start_point.row, d.old_end_point.row, d.new_end_point.row);
        }
        self.after_lsp_edit(cx);
        self.expand_stack.clear();
        self.diff_popup = None;
        self.autoscroll = true;
        self.schedule_reparse(cx);
        self.schedule_analysis(Duration::from_millis(250), cx);
        self.schedule_autosave(cx);
        self.pause_blink(cx);
        cx.emit(EditorEvent::Edited);
        cx.notify();
    }

    fn replace(&mut self, range: Range<usize>, text: &str, new_sel: Selection, cx: &mut Context<Self>) {
        self.shift_lsp_positions(range.start, range.len(), text.chars().count());
        self.lsp_record_changes(&[(range.clone(), text)]);
        let before = self.sel;
        let delta = self.buffer.edit(range, text, before, new_sel);
        self.lsp_recorded();
        self.sel = new_sel;
        self.goal_col = None;
        self.after_edit(delta, cx);
    }

    /// Several replacements as one undo step (completion + auto-import, code actions).
    fn apply_batch(&mut self, edits: Vec<(Range<usize>, String)>, sel: Selection, cx: &mut Context<Self>) {
        let mut ordered: Vec<&(Range<usize>, String)> = edits.iter().collect();
        ordered.sort_by(|a, b| b.0.start.cmp(&a.0.start).then(b.0.end.cmp(&a.0.end)));
        for (r, t) in &ordered {
            self.shift_lsp_positions(r.start, r.len(), t.chars().count());
        }
        // `edit_many` applies descending, so earlier ranges stay valid: record in that order.
        let changes: Vec<(Range<usize>, &str)> = ordered.iter().map(|(r, t)| (r.clone(), t.as_str())).collect();
        self.lsp_record_changes(&changes);
        let before = self.sel;
        let deltas = self.buffer.edit_many(edits, before, sel);
        self.lsp_recorded();
        let len = self.buffer.len_chars();
        self.sel = Selection::new(sel.anchor.min(len), sel.head.min(len));
        self.goal_col = None;
        self.after_edits(&deltas, cx);
    }

    fn word_start_before(&self, pos: usize) -> usize {
        let mut start = pos.min(self.buffer.len_chars());
        while start > 0 && is_word_char(self.buffer.rope().char(start - 1)) {
            start -= 1;
        }
        start
    }

    fn insert_at_selection(&mut self, text: &str, cx: &mut Context<Self>) {
        let range = self.sel.range();
        let end = range.start + text.chars().count();
        self.replace(range, text, Selection::caret(end), cx);
    }

    fn set_selection(&mut self, sel: Selection, keep_goal: bool, cx: &mut Context<Self>) {
        self.sel = sel;
        if !keep_goal {
            self.goal_col = None;
        }
        self.completion = None;
        self.diff_popup = None;
        self.autoscroll = true;
        self.pause_blink(cx);
        cx.notify();
    }

    fn move_to(&mut self, pos: usize, select: bool, cx: &mut Context<Self>) {
        let sel = if select { Selection::new(self.sel.anchor, pos) } else { Selection::caret(pos) };
        self.expand_stack.clear();
        self.set_selection(sel, false, cx);
    }

    fn vertical(&mut self, delta: isize, select: bool, cx: &mut Context<Self>) {
        let (row, col) = self.buffer.row_col(self.sel.head);
        let goal = self.goal_col.unwrap_or(col);
        let last = self.buffer.len_lines().saturating_sub(1) as isize;
        let mut target = row as isize + delta;
        // Step over folded regions: down lands after them, up on their header line.
        if target >= 0 && target <= last {
            if let Some((header, last_hidden)) = self.hidden_span(target as usize) {
                target = if delta > 0 { last_hidden as isize + 1 } else { header as isize };
            }
        }
        let pos = if target < 0 {
            0
        } else if target > last {
            self.buffer.len_chars()
        } else {
            self.buffer.char_at(target as usize, goal)
        };
        let sel = if select { Selection::new(self.sel.anchor, pos) } else { Selection::caret(pos) };
        self.expand_stack.clear();
        self.goal_col = Some(goal);
        self.set_selection(sel, true, cx);
    }

    fn line_range_chars(&self, first_row: usize, last_row: usize) -> Range<usize> {
        let start = self.buffer.line_to_char(first_row);
        let end = if last_row + 1 >= self.buffer.len_lines() {
            self.buffer.len_chars()
        } else {
            self.buffer.line_to_char(last_row + 1)
        };
        start..end
    }

    fn selected_rows(&self) -> (usize, usize) {
        let r = self.sel.range();
        let first = self.buffer.char_to_line(r.start);
        let mut last = self.buffer.char_to_line(r.end);
        if last > first && r.end == self.buffer.line_to_char(last) {
            last -= 1;
        }
        (first, last)
    }

    fn char_before(&self, pos: usize) -> Option<char> {
        (pos > 0).then(|| self.buffer.rope().char(pos - 1))
    }
    fn char_after(&self, pos: usize) -> Option<char> {
        (pos < self.buffer.len_chars()).then(|| self.buffer.rope().char(pos))
    }

    // ----- actions: movement -----

    fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        let r = self.sel.range();
        let pos = if r.is_empty() { r.start.saturating_sub(1) } else { r.start };
        self.move_to(pos, false, cx);
    }
    fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        let r = self.sel.range();
        let pos = if r.is_empty() { (r.end + 1).min(self.buffer.len_chars()) } else { r.end };
        self.move_to(pos, false, cx);
    }
    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.lsp.actions.is_some() {
            self.move_action_selection(-1, cx);
            return;
        }
        if let Some(comp) = self.completion.as_mut() {
            comp.selected = comp.selected.checked_sub(1).unwrap_or(comp.items.len() - 1);
            cx.notify();
            return;
        }
        self.vertical(-1, false, cx);
    }
    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.lsp.actions.is_some() {
            self.move_action_selection(1, cx);
            return;
        }
        if let Some(comp) = self.completion.as_mut() {
            comp.selected = (comp.selected + 1) % comp.items.len();
            cx.notify();
            return;
        }
        self.vertical(1, false, cx);
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.sel.head.saturating_sub(1), true, cx);
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to((self.sel.head + 1).min(self.buffer.len_chars()), true, cx);
    }
    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1, true, cx);
    }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1, true, cx);
    }
    fn move_word_left(&mut self, _: &MoveWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.buffer.prev_word_start(self.sel.head), false, cx);
    }
    fn move_word_right(&mut self, _: &MoveWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.buffer.next_word_end(self.sel.head), false, cx);
    }
    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.buffer.prev_word_start(self.sel.head), true, cx);
    }
    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.buffer.next_word_end(self.sel.head), true, cx);
    }
    fn smart_home(&self) -> usize {
        let (row, col) = self.buffer.row_col(self.sel.head);
        let indent = self.buffer.indentation(row).chars().count();
        let start = self.buffer.line_to_char(row);
        if col == indent { start } else { start + indent }
    }
    fn move_home(&mut self, _: &MoveHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.smart_home(), false, cx);
    }
    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.smart_home(), true, cx);
    }
    fn move_end(&mut self, _: &MoveEnd, _: &mut Window, cx: &mut Context<Self>) {
        let row = self.buffer.char_to_line(self.sel.head);
        self.move_to(self.buffer.char_at(row, usize::MAX), false, cx);
    }
    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        let row = self.buffer.char_to_line(self.sel.head);
        self.move_to(self.buffer.char_at(row, usize::MAX), true, cx);
    }
    fn move_doc_start(&mut self, _: &MoveDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, false, cx);
    }
    fn move_doc_end(&mut self, _: &MoveDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.buffer.len_chars(), false, cx);
    }
    fn select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, true, cx);
    }
    fn select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.buffer.len_chars(), true, cx);
    }
    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        let rows = self.layout.as_ref().map(|l| l.viewport_rows).unwrap_or(30).max(2) - 1;
        if let Some(l) = &self.layout {
            self.scroll_y = (self.scroll_y - l.line_height * rows as f32).max(px(0.));
        }
        self.vertical(-(rows as isize), false, cx);
    }
    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        let rows = self.layout.as_ref().map(|l| l.viewport_rows).unwrap_or(30).max(2) - 1;
        if let Some(l) = &self.layout {
            self.scroll_y += l.line_height * rows as f32;
        }
        self.vertical(rows as isize, false, cx);
    }
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = Selection::new(0, self.buffer.len_chars());
        self.completion = None;
        cx.notify();
    }
    fn escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        let lsp_popup = self.lsp.actions.take().is_some() | self.lsp.hover.take().is_some() | self.lsp.signature.take().is_some();
        if self.completion.take().is_some() | self.diff_popup.take().is_some() | self.bp_edit.take().is_some() | lsp_popup {
            cx.notify();
            return;
        }
        if !self.sel.is_empty() {
            self.set_selection(Selection::caret(self.sel.head), false, cx);
        }
    }

    // ----- actions: editing -----

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        let r = self.sel.range();
        if !r.is_empty() {
            self.replace(r.clone(), "", Selection::caret(r.start), cx);
            return;
        }
        let pos = r.start;
        if pos == 0 {
            return;
        }
        let (row, col) = self.buffer.row_col(pos);
        let line = self.buffer.line_text(row);
        let before: String = line.chars().take(col).collect();
        // Smart unindent inside leading whitespace.
        if col > 0 && before.chars().all(|c| c == ' ') {
            let remove = if col % TAB_WIDTH == 0 { TAB_WIDTH } else { col % TAB_WIDTH };
            self.replace(pos - remove..pos, "", Selection::caret(pos - remove), cx);
            self.update_completion(cx);
            return;
        }
        // Delete an empty auto-pair.
        let pair = (self.char_before(pos), self.char_after(pos));
        let end = if matches!(pair, (Some('('), Some(')')) | (Some('['), Some(']')) | (Some('{'), Some('}')) | (Some('"'), Some('"')) | (Some('\''), Some('\''))) {
            pos + 1
        } else {
            pos
        };
        self.replace(pos - 1..end, "", Selection::caret(pos - 1), cx);
        self.update_completion(cx);
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        let r = self.sel.range();
        if !r.is_empty() {
            self.replace(r.clone(), "", Selection::caret(r.start), cx);
        } else if r.start < self.buffer.len_chars() {
            self.replace(r.start..r.start + 1, "", Selection::caret(r.start), cx);
        }
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let head = self.sel.head;
        let start = if self.sel.is_empty() { self.buffer.prev_word_start(head) } else { self.sel.range().start };
        let end = if self.sel.is_empty() { head } else { self.sel.range().end };
        self.replace(start..end, "", Selection::caret(start), cx);
    }

    fn delete_word_right(&mut self, _: &DeleteWordRight, _: &mut Window, cx: &mut Context<Self>) {
        let head = self.sel.head;
        let (start, end) = if self.sel.is_empty() { (head, self.buffer.next_word_end(head)) } else { (self.sel.range().start, self.sel.range().end) };
        self.replace(start..end, "", Selection::caret(start), cx);
    }

    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        if self.lsp.actions.is_some() {
            self.apply_selected_action(cx);
            return;
        }
        if self.completion.is_some() {
            self.accept_completion(cx);
            return;
        }
        let r = self.sel.range();
        let row = self.buffer.char_to_line(r.start);
        let indent = self.buffer.indentation(row);
        let prev = self.buffer.slice_to_string(self.buffer.line_to_char(row)..r.start);
        let trimmed = prev.trim_end();
        let opens = trimmed.ends_with(['{', '[', '('])
            || (matches!(self.language, LanguageId::Python) && trimmed.ends_with(':'));
        let next = self.char_after(r.end);
        let unit = " ".repeat(TAB_WIDTH);
        if opens && matches!(next, Some('}') | Some(']') | Some(')')) {
            let text = format!("\n{indent}{unit}\n{indent}");
            let caret = r.start + 1 + indent.chars().count() + TAB_WIDTH;
            self.replace(r, &text, Selection::caret(caret), cx);
        } else if opens {
            let text = format!("\n{indent}{unit}");
            let caret = r.start + text.chars().count();
            self.replace(r, &text, Selection::caret(caret), cx);
        } else {
            let text = format!("\n{indent}");
            let caret = r.start + text.chars().count();
            self.replace(r, &text, Selection::caret(caret), cx);
        }
    }

    fn newline_below(&mut self, _: &NewlineBelow, _: &mut Window, cx: &mut Context<Self>) {
        let row = self.buffer.char_to_line(self.sel.head);
        let end = self.buffer.char_at(row, usize::MAX);
        let indent = self.buffer.indentation(row);
        let text = format!("\n{indent}");
        self.replace(end..end, &text, Selection::caret(end + text.chars().count()), cx);
    }

    fn tab(&mut self, _: &Tab, _: &mut Window, cx: &mut Context<Self>) {
        if self.lsp.actions.is_some() {
            self.apply_selected_action(cx);
            return;
        }
        // Postfix templates (`items.for` + Tab) win over the completion popup.
        if self.sel.is_empty() && self.try_postfix(cx) {
            self.completion = None;
            return;
        }
        if self.completion.is_some() {
            self.accept_completion(cx);
            return;
        }
        let (first, last) = self.selected_rows();
        if !self.sel.is_empty() && (first != last || self.sel.range().len() >= self.buffer.line_len(first)) {
            self.shift_lines(first, last, true, cx);
            return;
        }
        let col = self.buffer.row_col(self.sel.range().start).1;
        let n = TAB_WIDTH - col % TAB_WIDTH;
        self.insert_at_selection(&" ".repeat(n), cx);
    }

    fn outdent(&mut self, _: &Outdent, _: &mut Window, cx: &mut Context<Self>) {
        let (first, last) = self.selected_rows();
        self.shift_lines(first, last, false, cx);
    }

    fn shift_lines(&mut self, first: usize, last: usize, indent: bool, cx: &mut Context<Self>) {
        let range = self.line_range_chars(first, last);
        let text = self.buffer.slice_to_string(range.clone());
        let unit = " ".repeat(TAB_WIDTH);
        let mut out = String::new();
        for line in text.split_inclusive('\n') {
            if indent {
                if line.trim().is_empty() {
                    out.push_str(line);
                } else {
                    out.push_str(&unit);
                    out.push_str(line);
                }
            } else {
                let strip = line.chars().take(TAB_WIDTH).take_while(|c| *c == ' ').count();
                let strip = if strip == 0 && line.starts_with('\t') { 1 } else { strip };
                out.push_str(&line[strip..]);
            }
        }
        let new_end = range.start + out.chars().count();
        let trailing_nl = out.ends_with('\n');
        let sel = Selection::new(range.start, if trailing_nl { new_end - 1 } else { new_end });
        self.replace(range, &out, sel, cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let text = if self.sel.is_empty() {
            let row = self.buffer.char_to_line(self.sel.head);
            format!("{}\n", self.buffer.line_text(row))
        } else {
            self.buffer.slice_to_string(self.sel.range())
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if self.sel.is_empty() {
            let row = self.buffer.char_to_line(self.sel.head);
            cx.write_to_clipboard(ClipboardItem::new_string(format!("{}\n", self.buffer.line_text(row))));
            let range = self.line_range_chars(row, row);
            self.replace(range.clone(), "", Selection::caret(range.start), cx);
        } else {
            cx.write_to_clipboard(ClipboardItem::new_string(self.buffer.slice_to_string(self.sel.range())));
            let r = self.sel.range();
            self.replace(r.clone(), "", Selection::caret(r.start), cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace("\r\n", "\n");
            self.insert_at_selection(&text, cx);
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((deltas, sel)) = self.buffer.undo() {
            self.sel = sel;
            self.completion = None;
            self.lsp.hints = Arc::new(Vec::new());
            self.after_edits(&deltas, cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((deltas, sel)) = self.buffer.redo() {
            self.sel = sel;
            self.completion = None;
            self.lsp.hints = Arc::new(Vec::new());
            self.after_edits(&deltas, cx);
        }
    }

    fn duplicate_line(&mut self, _: &DuplicateLine, _: &mut Window, cx: &mut Context<Self>) {
        if !self.sel.is_empty() {
            let r = self.sel.range();
            let text = self.buffer.slice_to_string(r.clone());
            let len = text.chars().count();
            self.replace(r.end..r.end, &text, Selection::new(r.end, r.end + len), cx);
            return;
        }
        let (row, col) = self.buffer.row_col(self.sel.head);
        let line = self.buffer.line_text(row);
        let end = self.buffer.char_at(row, usize::MAX);
        let insert = format!("\n{line}");
        let caret = self.buffer.line_to_char(row) + line.chars().count() + 1 + col;
        self.replace(end..end, &insert, Selection::caret(caret), cx);
    }

    fn delete_line(&mut self, _: &DeleteLine, _: &mut Window, cx: &mut Context<Self>) {
        let (first, last) = self.selected_rows();
        let mut range = self.line_range_chars(first, last);
        if last + 1 >= self.buffer.len_lines() && first > 0 {
            range.start -= 1; // also remove the preceding newline on the last line
        }
        let caret = range.start.min(self.buffer.len_chars());
        self.replace(range, "", Selection::caret(caret), cx);
    }

    fn move_line(&mut self, up: bool, cx: &mut Context<Self>) {
        let (first, last) = self.selected_rows();
        let lines = self.buffer.len_lines();
        if (up && first == 0) || (!up && last + 1 >= lines) {
            return;
        }
        let (a, b) = if up { (first - 1, last) } else { (first, last + 1) };
        let block: Vec<String> = (a..=b).map(|r| self.buffer.line_text(r)).collect();
        let mut reordered = block.clone();
        if up {
            let moved = reordered.remove(0);
            reordered.push(moved);
        } else {
            let moved = reordered.pop().unwrap_or_default();
            reordered.insert(0, moved);
        }
        let start = self.buffer.line_to_char(a);
        let end = self.buffer.char_at(b, usize::MAX);
        let text = reordered.join("\n");
        let shift: isize = if up {
            -((self.buffer.line_len(a) + 1) as isize)
        } else {
            (self.buffer.line_len(b) + 1) as isize
        };
        let s = Selection::new((self.sel.anchor as isize + shift) as usize, (self.sel.head as isize + shift) as usize);
        self.replace(start..end, &text, s, cx);
    }

    fn move_line_up(&mut self, _: &MoveLineUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_line(true, cx);
    }
    fn move_line_down(&mut self, _: &MoveLineDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_line(false, cx);
    }

    fn toggle_comment(&mut self, _: &ToggleComment, _: &mut Window, cx: &mut Context<Self>) {
        let Some(token) = self.language.info().line_comment else { return };
        let (first, last) = self.selected_rows();
        let lines: Vec<String> = (first..=last).map(|r| self.buffer.line_text(r)).collect();
        let non_empty: Vec<&String> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
        if non_empty.is_empty() {
            return;
        }
        let all_commented = non_empty.iter().all(|l| l.trim_start().starts_with(token));
        let min_indent = non_empty.iter().map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').count()).min().unwrap_or(0);
        let out: Vec<String> = lines
            .iter()
            .map(|l| {
                if l.trim().is_empty() {
                    return l.clone();
                }
                if all_commented {
                    let ix = l.find(token).unwrap_or(0);
                    let mut rest = &l[ix + token.len()..];
                    if rest.starts_with(' ') {
                        rest = &rest[1..];
                    }
                    format!("{}{}", &l[..ix], rest)
                } else {
                    let (indent, rest) = split_at_char(l, min_indent);
                    format!("{indent}{token} {rest}")
                }
            })
            .collect();
        let start = self.buffer.line_to_char(first);
        let end = self.buffer.char_at(last, usize::MAX);
        let text = out.join("\n");
        let new_end = start + text.chars().count();
        let sel = if self.sel.is_empty() {
            let (row, col) = self.buffer.row_col(self.sel.head);
            let delta = out[row - first].chars().count() as isize - lines[row - first].chars().count() as isize;
            Selection::caret((start + (row - first) + out[..row - first].iter().map(|s| s.chars().count()).sum::<usize>() + (col as isize + delta).max(0) as usize).min(new_end))
        } else {
            Selection::new(start, new_end)
        };
        self.replace(start..end, &text, sel, cx);
    }

    fn extend_selection(&mut self, _: &ExtendSelection, _: &mut Window, cx: &mut Context<Self>) {
        let r = self.sel.range();
        let next = if r.is_empty() && self.buffer.word_range_at(r.start).len() > 0 {
            Some(self.buffer.word_range_at(r.start))
        } else if let Some(tree) = &self.tree {
            let br = self.buffer.char_to_byte(r.start)..self.buffer.char_to_byte(r.end);
            graphyne_syntax::expand_selection(tree, br).map(|b| self.buffer.byte_to_char(b.start)..self.buffer.byte_to_char(b.end))
        } else {
            Some(0..self.buffer.len_chars())
        };
        if let Some(n) = next {
            if n != r {
                self.expand_stack.push(self.sel);
                self.sel = Selection::new(n.start, n.end);
                cx.notify();
            }
        }
    }

    fn shrink_selection(&mut self, _: &ShrinkSelection, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(prev) = self.expand_stack.pop() {
            self.sel = prev;
            cx.notify();
        }
    }

    fn toggle_breakpoint(&mut self, _: &ToggleBreakpoint, _: &mut Window, cx: &mut Context<Self>) {
        let row = self.buffer.char_to_line(self.sel.head);
        if let (Some(debugger), Some(path)) = (self.debugger.clone(), self.path.clone()) {
            debugger.update(cx, |d, cx| d.toggle_breakpoint(&path, row, cx));
        }
    }

    /// Ctrl+Shift+F8: condition / log message of the breakpoint on the caret line.
    fn edit_breakpoint(&mut self, _: &EditBreakpoint, window: &mut Window, cx: &mut Context<Self>) {
        let row = self.buffer.char_to_line(self.sel.head);
        let anchor = self.caret_point().unwrap_or_default();
        self.open_bp_editor(row, anchor, window, cx);
    }

    fn open_bp_editor(&mut self, line: usize, anchor: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(debugger), Some(path)) = (self.debugger.clone(), self.path.clone()) else { return };
        let opts = debugger.read(cx).breakpoint(&path, line).unwrap_or_default();
        let condition = cx.new(|cx| {
            let mut t = TextInput::new("e.g. total > 100", cx);
            t.set_text(opts.condition.clone().unwrap_or_default(), cx);
            t
        });
        let log = cx.new(|cx| {
            let mut t = TextInput::new("e.g. total = {total}", cx);
            t.set_text(opts.log_message.clone().unwrap_or_default(), cx);
            t
        });
        let mut subs = Vec::new();
        for (input, other) in [(condition.clone(), log.clone()), (log.clone(), condition.clone())] {
            subs.push(cx.subscribe_in(&input, window, move |this, _, ev: &TextInputEvent, window, cx| match ev {
                TextInputEvent::Submit => this.close_bp_editor(true, window, cx),
                TextInputEvent::Cancel => this.close_bp_editor(false, window, cx),
                TextInputEvent::Up | TextInputEvent::Down => window.focus(&other.focus_handle(cx)),
                TextInputEvent::Changed(_) => {}
            }));
        }
        window.focus(&condition.focus_handle(cx));
        self.bp_edit = Some(BpEdit { line, anchor, condition, log, _subs: subs });
        cx.notify();
    }

    /// Close the breakpoint editor; `save` writes condition / log message (creating the
    /// breakpoint if the line had none).
    fn close_bp_editor(&mut self, save: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.bp_edit.take() else { return };
        if let (true, Some(debugger), Some(path)) = (save, self.debugger.clone(), self.path.clone()) {
            let field = |e: &Entity<TextInput>| Some(e.read(cx).text().trim().to_string()).filter(|t| !t.is_empty());
            let opts = BpOpts { condition: field(&edit.condition), log_message: field(&edit.log) };
            debugger.update(cx, |d, cx| d.set_breakpoint(&path, edit.line, Some(opts), cx));
        }
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn remove_breakpoint_from_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.bp_edit.take() else { return };
        if let (Some(debugger), Some(path)) = (self.debugger.clone(), self.path.clone()) {
            debugger.update(cx, |d, cx| d.set_breakpoint(&path, edit.line, None, cx));
        }
        window.focus(&self.focus_handle);
        cx.notify();
    }

    /// Right click on the line-number area: edit that line's breakpoint.
    fn on_right_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = self.layout.as_ref() else { return };
        if event.position.x >= layout.bounds.left() + layout.gutter_width - px(16. + FOLD_W) {
            return;
        }
        let row = layout.rows.iter().find_map(|r| match &r.kind {
            RowKind::Line { row, .. } if event.position.y >= r.y && event.position.y < r.y + layout.line_height => Some(*row),
            _ => None,
        });
        if let Some(row) = row {
            cx.stop_propagation();
            self.open_bp_editor(row, event.position, window, cx);
        }
    }

    fn render_bp_editor(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let edit = self.bp_edit.as_ref()?;
        let entity = cx.entity();
        let field = |label: &'static str, hint: &'static str, input: Entity<TextInput>| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().flex().gap(px(6.)).child(div().text_color(color(c::TEXT_MUTED)).child(label)).child(div().text_color(color(c::TEXT_FAINT)).child(hint)))
                .child(
                    div()
                        .h(px(28.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .bg(color(c::ISLAND))
                        .border_1()
                        .border_color(color(c::POPUP_BORDER))
                        .font_family(theme::fonts().code)
                        .child(input),
                )
        };
        Some(
            deferred(
                anchored().position(edit.anchor + point(px(6.), px(10.))).snap_to_window_with_margin(px(8.)).child(
                    div()
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w(px(360.))
                        .p(px(12.))
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .bg(color(c::POPUP))
                        .border_1()
                        .border_color(color(c::POPUP_BORDER))
                        .rounded(px(8.))
                        .shadow_lg()
                        .font_family(theme::fonts().ui)
                        .text_size(px(theme::UI_SMALL))
                        .text_color(color(c::TEXT))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().text_size(px(theme::UI_TEXT)).child(format!("Breakpoint · line {}", edit.line + 1)))
                                .child(
                                    div()
                                        .id("bp-remove")
                                        .px(px(6.))
                                        .rounded(px(4.))
                                        .cursor_pointer()
                                        .text_color(color(c::RED))
                                        .hover(|s| s.bg(color(c::HOVER)))
                                        .child("Remove")
                                        .on_click(move |_, window, cx| entity.update(cx, |e, cx| e.remove_breakpoint_from_editor(window, cx))),
                                ),
                        )
                        .child(field("Condition", "pause only when true", edit.condition.clone()))
                        .child(field("Log message", "print {expr} and continue", edit.log.clone()))
                        .child(div().text_color(color(c::TEXT_DIM)).child("Enter — save · ↑↓ — switch field · Esc — cancel")),
                ),
            )
            .with_priority(2),
        )
    }

    fn save_action(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(task) = self.save(cx) {
            task.detach();
        }
    }

    fn show_completions(&mut self, _: &ShowCompletions, _: &mut Window, cx: &mut Context<Self>) {
        self.update_completion_inner(true, cx);
    }

    // ----- postfix templates (IntelliJ-style) -----

    fn try_postfix(&mut self, cx: &mut Context<Self>) -> bool {
        let pos = self.sel.head;
        let (row, col) = self.buffer.row_col(pos);
        let line = self.buffer.line_text(row);
        let before: String = line.chars().take(col).collect();
        let Some(dot) = before.rfind('.') else { return false };
        let key = &before[dot + 1..];
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphabetic()) {
            return false;
        }
        let lang = self.language;
        let unit = " ".repeat(TAB_WIDTH);
        if postfix_template(lang, key, "x", "x", "", &unit).is_none() {
            return false;
        }
        let line_start = self.buffer.line_to_char(row);
        let dot_char = line_start + before[..dot].chars().count();
        // Receiver: the syntax tree knows chains over several lines, strings with spaces and
        // `a + b`; the text heuristic covers languages without a grammar.
        let range = match self.postfix_receiver(dot_char, pos) {
            Some(r) => r,
            None => {
                let head = &before[..dot];
                let mut depth = 0i32;
                let mut start = head.len();
                for (i, ch) in head.char_indices().rev() {
                    match ch {
                        ')' | ']' => depth += 1,
                        '(' | '[' if depth > 0 => depth -= 1,
                        c if depth > 0 || is_word_char(c) || c == '.' || c == '"' || c == '\'' || c == ':' || c == '&' || c == '*' => {}
                        _ => break,
                    }
                    start = i;
                }
                let lead = head[start..].len() - head[start..].trim_start().len();
                line_start + head[..start + lead].chars().count()..dot_char
            }
        };
        let expr = self.buffer.slice_to_string(range.clone());
        if expr.trim().is_empty() {
            return false;
        }
        let indent = self.buffer.indentation(self.buffer.row_col(range.start).0);
        let item = singular(&expr);
        // Prefix forms need parentheses around a compound receiver: `!(a == b)`.
        let compound = expr.contains(char::is_whitespace) && !(expr.starts_with('(') && expr.ends_with(')'));
        let receiver = if compound && matches!(key, "not" | "ref" | "await") { format!("({expr})") } else { expr.clone() };
        let Some(template) = postfix_template(lang, key, &receiver, &item, &indent, &unit) else { return false };
        let caret_ix = template.find("$0").unwrap_or(template.len());
        let caret_chars = template[..caret_ix].chars().count();
        let text = template.replace("$0", "");
        self.replace(range.start..pos, &text, Selection::caret(range.start + caret_chars), cx);
        true
    }

    /// Char range of the expression a postfix template applies to, from a parse of the
    /// buffer with `.key` removed (incremental on the current tree).
    fn postfix_receiver(&self, dot_char: usize, key_end: usize) -> Option<Range<usize>> {
        if !graphyne_syntax::supports(self.language) || self.buffer.len_bytes() > 4 * 1024 * 1024 {
            return None;
        }
        let current = self.buffer.rope();
        let (dot_b, end_b) = (current.char_to_byte(dot_char), current.char_to_byte(key_end));
        let mut rope = current.clone();
        rope.remove(dot_char..key_end);
        let old = self.tree.clone().map(|mut t| {
            let at = self.buffer.byte_point(dot_b);
            graphyne_syntax::apply_edit(
                &mut t,
                &graphyne_core::EditDelta {
                    start_byte: dot_b,
                    old_end_byte: end_b,
                    new_end_byte: dot_b,
                    start_point: at,
                    old_end_point: self.buffer.byte_point(end_b),
                    new_end_point: at,
                },
            );
            t
        });
        let tree = graphyne_syntax::parse(self.language, &rope, old.as_ref())?;
        let r = graphyne_syntax::expression_ending_at(&tree, dot_b)?;
        Some(rope.byte_to_char(r.start)..rope.byte_to_char(r.end))
    }

    // ----- completion: language server first, buffer words / keywords as fallback -----

    fn update_completion(&mut self, cx: &mut Context<Self>) {
        if self.completion.is_some() {
            self.update_completion_inner(false, cx);
        }
    }

    fn update_completion_inner(&mut self, explicit: bool, cx: &mut Context<Self>) {
        let pos = self.sel.head;
        let start = self.word_start_before(pos);
        let prefix = self.buffer.slice_to_string(start..pos);
        if self.lsp_completion_ready() {
            let cache_valid = self.lsp.completions.as_ref().is_some_and(|c| c.word_start == start && !c.incomplete);
            if !cache_valid && (explicit || !prefix.is_empty()) {
                self.request_lsp_completion(None, cx);
            }
            if prefix.is_empty() && !explicit && !self.lsp.completions.as_ref().is_some_and(|c| c.word_start == start) {
                self.completion = None;
                return;
            }
        } else if prefix.is_empty() && !explicit {
            self.completion = None;
            return;
        }
        self.rebuild_completion(explicit, cx);
    }

    fn rebuild_completion(&mut self, explicit: bool, cx: &mut Context<Self>) {
        let pos = self.sel.head;
        let start = self.word_start_before(pos);
        let prefix = self.buffer.slice_to_string(start..pos);
        let items = match self.lsp_completion_entries(start, &prefix) {
            Some(items) => items,
            // While a server request is in flight keep the current list (no flicker).
            None if self.lsp_completion_ready() => {
                cx.notify();
                return;
            }
            None => {
                if !explicit && (prefix.chars().count() < 2 || prefix.chars().next().is_some_and(|c| c.is_ascii_digit())) {
                    self.completion = None;
                    cx.notify();
                    return;
                }
                let mut candidates: Vec<String> = keywords(self.language).iter().map(|s| s.to_string()).collect();
                candidates.extend(self.words.iter().filter(|w| **w != prefix).cloned());
                candidates.sort();
                candidates.dedup();
                fuzzy_filter(&candidates, &prefix, 12, false)
                    .into_iter()
                    .filter(|m| candidates[m.index] != prefix)
                    .map(|m| {
                        let label = candidates[m.index].clone();
                        let kind = if keywords_static_contains(&label) { 14 } else { 1 };
                        CompletionEntry { label, indices: m.indices, detail: None, kind, lsp: None }
                    })
                    .collect()
            }
        };
        self.completion = if items.is_empty() { None } else { Some(Completion { word_start: start, items, selected: 0 }) };
        cx.notify();
    }

    fn accept_completion(&mut self, cx: &mut Context<Self>) {
        let Some(comp) = self.completion.take() else { return };
        let Some(entry) = comp.items.get(comp.selected).cloned() else { return };
        if let Some(item) = entry.lsp {
            self.accept_lsp_completion(item, comp.word_start, cx);
            return;
        }
        let word = entry.label;
        let pos = self.sel.head;
        let mut end = pos;
        while end < self.buffer.len_chars() && is_word_char(self.buffer.rope().char(end)) {
            end += 1;
        }
        let caret = comp.word_start + word.chars().count();
        self.replace(comp.word_start..end, &word, Selection::caret(caret), cx);
    }

    // ----- git gutter -----

    fn rollback_hunk(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(hunk) = self.hunks.get(ix).cloned() else { return };
        let Some(base) = self.base_text.clone() else { return };
        let old: String = base.split_inclusive('\n').skip(hunk.old.start as usize).take((hunk.old.end - hunk.old.start) as usize).collect();
        let start = self.buffer.line_to_char(hunk.new.start as usize);
        let end = if hunk.new.end as usize >= self.buffer.len_lines() { self.buffer.len_chars() } else { self.buffer.line_to_char(hunk.new.end as usize) };
        self.diff_popup = None;
        self.replace(start..end, &old, Selection::caret(start), cx);
    }

    // ----- mouse -----

    fn hit_test(&self, position: Point<Pixels>) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let lh = layout.line_height;
        let mut row_layout: Option<&RowLayout> = None;
        for r in &layout.rows {
            if position.y >= r.y && position.y < r.y + lh {
                row_layout = Some(r);
                break;
            }
        }
        let line_rows: Vec<&RowLayout> = layout.rows.iter().filter(|r| matches!(r.kind, RowKind::Line { .. })).collect();
        let row_layout = match row_layout {
            Some(r) if matches!(r.kind, RowKind::Line { .. }) => r,
            Some(r) => line_rows.iter().find(|l| l.y > r.y).copied().or(line_rows.last().copied())?,
            None if position.y < layout.bounds.top() => line_rows.first().copied()?,
            None => {
                if position.y >= layout.bounds.bottom() || layout.rows.last().map(|r| position.y >= r.y + lh).unwrap_or(true) {
                    if let Some(last) = line_rows.last() {
                        if position.y >= layout.bounds.bottom() {
                            return Some(self.after_row_end(last));
                        }
                        return Some(self.buffer.len_chars().min(self.after_row_end(last)));
                    }
                }
                line_rows.first().copied()?
            }
        };
        let RowKind::Line { row, line, char_start, .. } = &row_layout.kind else { return None };
        let x = position.x - layout.text_x;
        let byte = if x <= px(0.) { 0 } else { line.closest_index_for_x(x) };
        let col = char_start.partition_point(|b| *b < byte).min(self.buffer.line_len(*row));
        Some(self.buffer.line_to_char(*row) + col)
    }

    fn after_row_end(&self, row: &RowLayout) -> usize {
        match &row.kind {
            RowKind::Line { row, .. } => self.buffer.char_at(*row, usize::MAX),
            _ => self.buffer.len_chars(),
        }
    }

    fn gutter_hunk_at(&self, position: Point<Pixels>) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        if position.x > layout.bounds.left() + layout.gutter_width - px(FOLD_W) || position.x < layout.bounds.left() + layout.gutter_width - px(16. + FOLD_W) {
            return None;
        }
        let row = layout.rows.iter().find_map(|r| match &r.kind {
            RowKind::Line { row, .. } if position.y >= r.y && position.y < r.y + layout.line_height => Some(*row as u32),
            _ => None,
        })?;
        self.hunks.iter().position(|h| match h.kind {
            LineChange::Deleted => h.new.start == row || h.new.start == row + 1,
            _ => h.new.contains(&row),
        })
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.restore_top = None;
        window.focus(&self.focus_handle);
        self.completion = None;
        self.lsp.hover = None;
        self.lsp.actions = None;
        if let Some(ix) = self.gutter_hunk_at(event.position) {
            self.diff_popup = if self.diff_popup == Some(ix) { None } else { Some(ix) };
            cx.notify();
            return;
        }
        self.diff_popup = None;
        if let Some(row) = self.fold_click_row(event.position) {
            self.toggle_fold(row, cx);
            return;
        }
        // Click on the line-number area toggles a breakpoint (JetBrains style).
        if let Some(layout) = self.layout.as_ref() {
            if event.position.x < layout.bounds.left() + layout.gutter_width - px(16. + FOLD_W) {
                let row = layout.rows.iter().find_map(|r| match &r.kind {
                    RowKind::Line { row, .. } if event.position.y >= r.y && event.position.y < r.y + layout.line_height => Some(*row),
                    _ => None,
                });
                if let (Some(row), Some(debugger), Some(path)) = (row, self.debugger.clone(), self.path.clone()) {
                    debugger.update(cx, |d, cx| d.toggle_breakpoint(&path, row, cx));
                    return;
                }
            }
        }
        let Some(pos) = self.hit_test(event.position) else { return };
        self.expand_stack.clear();
        match event.click_count {
            2 => {
                let w = self.buffer.word_range_at(pos);
                self.sel = Selection::new(w.start, w.end);
                self.drag = Some((DragMode::Word, w.start));
            }
            n if n >= 3 => {
                let row = self.buffer.char_to_line(pos);
                let r = self.line_range_chars(row, row);
                self.sel = Selection::new(r.start, r.end);
                self.drag = Some((DragMode::Line, r.start));
            }
            _ => {
                if event.modifiers.shift {
                    self.sel = Selection::new(self.sel.anchor, pos);
                } else {
                    self.sel = Selection::caret(pos);
                }
                if event.modifiers.control || event.modifiers.platform {
                    // Ctrl+Click: go to declaration.
                    self.request_definition(pos, cx);
                } else {
                    self.drag = Some((DragMode::Char, self.sel.anchor));
                }
            }
        }
        self.goal_col = None;
        self.pause_blink(cx);
        cx.notify();
    }

    /// The fold header row under `position` when it is on a fold arrow or a `⋯` marker.
    fn fold_click_row(&self, position: Point<Pixels>) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let (row, line) = layout.rows.iter().find_map(|r| match &r.kind {
            RowKind::Line { row, line, .. } if position.y >= r.y && position.y < r.y + layout.line_height => Some((*row, line)),
            _ => None,
        })?;
        let left = layout.bounds.left();
        let on_arrow = position.x >= left + layout.gutter_width - px(FOLD_W) && position.x < left + layout.gutter_width;
        let marker_x = layout.text_x + line.width + layout.char_width;
        let on_marker = self.folded.contains(&row) && position.x >= marker_x && position.x < marker_x + layout.char_width * 3. + px(6.);
        ((on_arrow && self.fold_range_at(row).is_some()) || on_marker).then_some(row)
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(layout) = self.layout.as_ref() {
            let in_gutter = event.position.x >= layout.bounds.left() && event.position.x < layout.bounds.left() + layout.gutter_width;
            if in_gutter != self.mouse_in_gutter {
                self.mouse_in_gutter = in_gutter;
                cx.notify();
            }
        }
        let Some((mode, anchor)) = self.drag else {
            if event.pressed_button.is_none() {
                self.on_hover_move(event.position, cx);
            }
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let Some(pos) = self.hit_test(event.position) else { return };
        self.sel = match mode {
            DragMode::Char => Selection::new(anchor, pos),
            DragMode::Word => {
                let w = self.buffer.word_range_at(pos);
                if pos < anchor { Selection::new(self.buffer.word_range_at(anchor).end, w.start) } else { Selection::new(anchor, w.end) }
            }
            DragMode::Line => {
                let row = self.buffer.char_to_line(pos);
                let r = self.line_range_chars(row, row);
                if pos < anchor {
                    let ar = self.buffer.char_to_line(anchor);
                    Selection::new(self.line_range_chars(ar, ar).end, r.start)
                } else {
                    Selection::new(anchor, r.end)
                }
            }
        };
        self.autoscroll = true;
        cx.notify();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.drag = None;
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let lh = px(theme::code_line_height());
        let delta = event.delta.pixel_delta(lh);
        let (dx, dy) = if event.modifiers.shift && delta.x == px(0.) { (delta.y, px(0.)) } else { (delta.x, delta.y) };
        self.restore_top = None;
        self.scroll_y = (self.scroll_y - dy).max(px(0.));
        self.scroll_x = (self.scroll_x - dx).max(px(0.));
        self.clamp_scroll();
        cx.notify();
    }

    fn total_display_rows(&self) -> usize {
        let hidden: usize = self.hidden.iter().map(|(s, e)| e - s).sum();
        self.buffer.len_lines() - hidden.min(self.buffer.len_lines()) + self.lenses.len() - self.hidden_lenses_before(usize::MAX)
    }

    /// An extension registered or changed languages: switch this file's language if needed.
    pub fn redetect_language(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else { return };
        let lang = LanguageId::from_path(&path);
        // Same id but a grammar may have arrived: reparse extension languages anyway.
        if lang == self.language && !matches!(lang, LanguageId::Plugin(_)) {
            return;
        }
        self.language = lang;
        self.tree = None;
        self.lsp_init(cx);
        self.schedule_reparse(cx);
        cx.notify();
    }

    /// Snapshot for `graphyne.buffer.*` (text omitted for very large files).
    pub fn lua_context(&self, ctx: &mut graphyne_lua::EditorContext) {
        let (row, col) = self.buffer.row_col(self.sel.head);
        ctx.path = self.path.as_ref().map(|p| p.to_string_lossy().to_string());
        ctx.language = Some(self.language.name().to_string());
        ctx.text = (self.buffer.len_bytes() <= 8 * 1024 * 1024).then(|| self.buffer.rope().to_string());
        ctx.line = row + 1;
        ctx.column = col + 1;
        ctx.selection = self.buffer.slice_to_string(self.sel.range());
        if let Some((kind, range)) = self.tree.as_ref().and_then(|t| graphyne_syntax::node_at(t, self.buffer.char_to_byte(self.sel.head))) {
            let rope = self.buffer.rope();
            let (s, e) = (rope.byte_to_char(range.start.min(rope.len_bytes())), rope.byte_to_char(range.end.min(rope.len_bytes())));
            ctx.node_kind = Some(kind);
            ctx.node_text = Some(self.buffer.slice_to_string(s..e.min(s + 4000)));
        }
    }

    /// `graphyne.buffer.insert / replace_selection / set_text`.
    pub fn lua_edit(&mut self, edit: graphyne_lua::BufferEdit, cx: &mut Context<Self>) {
        match edit {
            graphyne_lua::BufferEdit::Insert(t) | graphyne_lua::BufferEdit::ReplaceSelection(t) => self.insert_at_selection(&t, cx),
            graphyne_lua::BufferEdit::SetText(t) => {
                let len = self.buffer.len_chars();
                let caret = self.sel.head.min(t.chars().count());
                self.replace(0..len, &t, Selection::caret(caret), cx);
            }
        }
    }

    fn is_large(&self) -> bool {
        self.buffer.len_bytes() > LARGE_FILE_BYTES
    }

    fn rebuild_lenses(&mut self) {
        self.lenses = Arc::new(build_lenses(&self.outline, &self.ident_counts, &self.lsp.usages));
        self.apply_restore_top();
    }

    fn display_row(&self, row: usize) -> usize {
        // A hidden row shows where its fold header is.
        let row = self.hidden_span(row).map_or(row, |(header, _)| header);
        let hidden_rows: usize = self.hidden.iter().take_while(|(_, e)| *e < row).map(|(s, e)| e - s).sum();
        row - hidden_rows + self.lenses.partition_point(|(r, _)| *r <= row) - self.hidden_lenses_before(row)
    }

    // ----- code folding -----

    /// The folded region hiding `row`, if any: (header row, last hidden row).
    fn hidden_span(&self, row: usize) -> Option<(usize, usize)> {
        let ix = self.hidden.partition_point(|(s, _)| *s < row);
        ix.checked_sub(1).map(|i| self.hidden[i]).filter(|(_, e)| row <= *e)
    }

    /// `N usages` rows that sit on hidden lines before `row`.
    fn hidden_lenses_before(&self, row: usize) -> usize {
        self.hidden
            .iter()
            .take_while(|(_, e)| *e < row)
            .map(|(s, e)| self.lenses.partition_point(|(r, _)| *r <= *e) - self.lenses.partition_point(|(r, _)| *r <= *s))
            .sum()
    }

    fn fold_range_at(&self, row: usize) -> Option<(usize, usize)> {
        self.fold_ranges.binary_search_by_key(&row, |(s, _)| *s).ok().map(|ix| self.fold_ranges[ix])
    }

    /// Recompute the hidden rows from the folded headers and the current fold regions.
    fn rebuild_hidden(&mut self) {
        if self.tree.is_none() {
            // Not parsed yet (e.g. restored folds): they apply once the regions are known.
            self.hidden.clear();
            return;
        }
        let ranges = self.fold_ranges.clone();
        // A fold whose region is gone (edited away) is dropped.
        self.folded.retain(|s| ranges.binary_search_by_key(s, |(r, _)| *r).is_ok());
        let mut hidden: Vec<(usize, usize)> = Vec::new();
        for s in &self.folded {
            let Ok(ix) = ranges.binary_search_by_key(s, |(r, _)| *r) else { continue };
            if hidden.last().is_some_and(|(_, e)| *s <= *e) {
                continue; // inside an outer folded region
            }
            hidden.push(ranges[ix]);
        }
        self.hidden = hidden;
    }

    /// Lines were replaced (`start..=old_end` became `start..=new_end`): move the folds below.
    fn shift_folds(&mut self, start: usize, old_end: usize, new_end: usize) {
        if old_end == new_end || (self.folded.is_empty() && self.fold_ranges.is_empty()) {
            return;
        }
        let shift = |r: usize| -> Option<usize> {
            if r <= start {
                Some(r)
            } else if r <= old_end {
                None
            } else {
                Some(r - old_end + new_end)
            }
        };
        self.folded = self.folded.iter().filter_map(|r| shift(*r)).collect();
        let ranges: Vec<(usize, usize)> = self
            .fold_ranges
            .iter()
            .filter_map(|(s, e)| {
                let s2 = shift(*s)?;
                let e2 = shift(*e).unwrap_or(new_end).max(s2);
                (e2 > s2).then_some((s2, e2))
            })
            .collect();
        self.fold_ranges = Arc::new(ranges);
        self.rebuild_hidden();
    }

    /// Fold or unfold the region that starts on `row` (gutter arrow, `⋯` marker).
    fn toggle_fold(&mut self, row: usize, cx: &mut Context<Self>) {
        if !self.folded.remove(&row) {
            if self.fold_range_at(row).is_none() {
                return;
            }
            self.folded.insert(row);
        }
        self.after_fold_change(cx);
    }

    fn after_fold_change(&mut self, cx: &mut Context<Self>) {
        self.rebuild_hidden();
        // The caret never stays on a hidden line: it goes to the end of the fold's header.
        let row = self.buffer.char_to_line(self.sel.head);
        if let Some((header, _)) = self.hidden_span(row) {
            let pos = self.buffer.char_at(header, usize::MAX);
            self.sel = Selection::caret(pos);
        }
        self.restore_top = None;
        self.clamp_scroll();
        cx.notify();
    }

    /// The caret moved onto a hidden line (search, go to, undo…): unfold around it.
    fn reveal_caret(&mut self) {
        let row = self.buffer.char_to_line(self.sel.head);
        if self.hidden_span(row).is_none() {
            return;
        }
        let ranges = self.fold_ranges.clone();
        self.folded.retain(|s| !ranges.binary_search_by_key(s, |(r, _)| *r).is_ok_and(|ix| *s < row && row <= ranges[ix].1));
        self.rebuild_hidden();
    }

    /// Folded headers, for the session.
    pub fn folded_rows(&self) -> Vec<usize> {
        self.folded.iter().copied().collect()
    }

    /// Restore folds from the session (applied once the file is parsed).
    pub fn set_folded_rows(&mut self, rows: Vec<usize>, cx: &mut Context<Self>) {
        self.folded = rows.into_iter().collect();
        self.rebuild_hidden();
        cx.notify();
    }

    /// Ctrl+-: fold the innermost unfolded region around the caret; again folds the next one out.
    fn fold_region(&mut self, _: &FoldRegion, _: &mut Window, cx: &mut Context<Self>) {
        let row = self.buffer.char_to_line(self.sel.head);
        let target = self.fold_ranges.iter().filter(|(s, e)| *s <= row && row <= *e + 1 && !self.folded.contains(s)).map(|(s, _)| *s).max();
        if let Some(s) = target {
            self.folded.insert(s);
            self.after_fold_change(cx);
        }
    }

    /// Ctrl+=: unfold the region on the caret's line, or the innermost folded one around it.
    fn unfold_region(&mut self, _: &UnfoldRegion, _: &mut Window, cx: &mut Context<Self>) {
        let row = self.buffer.char_to_line(self.sel.head);
        let target = if self.folded.contains(&row) {
            Some(row)
        } else {
            self.fold_ranges.iter().filter(|(s, e)| *s <= row && row <= *e + 1 && self.folded.contains(s)).map(|(s, _)| *s).max()
        };
        if let Some(s) = target {
            self.folded.remove(&s);
            self.after_fold_change(cx);
        }
    }

    /// Ctrl+Shift+-: fold everything.
    fn fold_all(&mut self, _: &FoldAll, _: &mut Window, cx: &mut Context<Self>) {
        self.folded = self.fold_ranges.iter().map(|(s, _)| *s).collect();
        self.after_fold_change(cx);
    }

    /// Ctrl+Shift+=: unfold everything.
    fn unfold_all(&mut self, _: &UnfoldAll, _: &mut Window, cx: &mut Context<Self>) {
        self.folded.clear();
        self.after_fold_change(cx);
    }

    fn clamp_scroll(&mut self) {
        if let Some(layout) = &self.layout {
            let total = self.total_display_rows() as f32 * layout.line_height;
            let max = (total - layout.bounds.size.height * 0.5).max(px(0.));
            self.scroll_y = self.scroll_y.min(max);
        }
    }
}

// ---------- free helpers ----------

fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn split_at_char(s: &str, n: usize) -> (&str, &str) {
    let byte = s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len());
    s.split_at(byte)
}

/// IntelliJ-style postfix template for `expr.key`; `$0` marks the caret.
fn postfix_template(lang: LanguageId, key: &str, expr: &str, item: &str, indent: &str, unit: &str) -> Option<String> {
    match (lang, key) {
        (LanguageId::Rust, "for") => Some(format!("for {item} in {expr} {{\n{indent}{unit}$0\n{indent}}}")),
        (LanguageId::Rust, "if") => Some(format!("if {expr} {{\n{indent}{unit}$0\n{indent}}}")),
        (LanguageId::Rust, "match") => Some(format!("match {expr} {{\n{indent}{unit}$0\n{indent}}}")),
        (LanguageId::Rust, "some") => Some(format!("Some({expr})$0")),
        (LanguageId::Rust, "ok") => Some(format!("Ok({expr})$0")),
        (LanguageId::Rust, "err") => Some(format!("Err({expr})$0")),
        (LanguageId::Rust, "dbg") => Some(format!("dbg!({expr})$0")),
        (LanguageId::Rust, "let") => Some(format!("let $0 = {expr};")),
        (LanguageId::Rust, "sout" | "println") => Some(format!("println!(\"{{:?}}\", {expr});$0")),
        (LanguageId::Rust, "ref") => Some(format!("&{expr}$0")),
        (LanguageId::Python, "for") => Some(format!("for {item} in {expr}:\n{indent}{unit}$0")),
        (LanguageId::Python, "if") => Some(format!("if {expr}:\n{indent}{unit}$0")),
        (LanguageId::Python, "sout" | "print") => Some(format!("print({expr})$0")),
        (LanguageId::Python, "len") => Some(format!("len({expr})$0")),
        (LanguageId::Python, "not") => Some(format!("not {expr}$0")),
        (LanguageId::TypeScript | LanguageId::Tsx | LanguageId::JavaScript, "for") => {
            Some(format!("for (const {item} of {expr}) {{\n{indent}{unit}$0\n{indent}}}"))
        }
        (LanguageId::TypeScript | LanguageId::Tsx | LanguageId::JavaScript, "if") => Some(format!("if ({expr}) {{\n{indent}{unit}$0\n{indent}}}")),
        (LanguageId::TypeScript | LanguageId::Tsx | LanguageId::JavaScript, "log" | "sout") => Some(format!("console.log({expr});$0")),
        (LanguageId::TypeScript | LanguageId::Tsx | LanguageId::JavaScript, "const") => Some(format!("const $0 = {expr};")),
        (LanguageId::TypeScript | LanguageId::Tsx | LanguageId::JavaScript, "await") => Some(format!("await {expr}$0")),
        (LanguageId::Go, "for") => Some(format!("for _, {item} := range {expr} {{\n{indent}{unit}$0\n{indent}}}")),
        (LanguageId::Go, "if") => Some(format!("if {expr} {{\n{indent}{unit}$0\n{indent}}}")),
        (LanguageId::Go, "sout" | "print") => Some(format!("fmt.Println({expr})$0")),
        (LanguageId::C | LanguageId::Cpp | LanguageId::Java | LanguageId::CSharp | LanguageId::Kotlin, "if") => {
            Some(format!("if ({expr}) {{\n{indent}{unit}$0\n{indent}}}"))
        }
        (LanguageId::Cpp, "for") => Some(format!("for (auto& {item} : {expr}) {{\n{indent}{unit}$0\n{indent}}}")),
        (LanguageId::Java, "for") => Some(format!("for (var {item} : {expr}) {{\n{indent}{unit}$0\n{indent}}}")),
        (LanguageId::Java, "sout") => Some(format!("System.out.println({expr});$0")),
        (LanguageId::CSharp, "for") => Some(format!("foreach (var {item} in {expr}) {{\n{indent}{unit}$0\n{indent}}}")),
        (_, "not") => Some(format!("!{expr}$0")),
        (_, "par") => Some(format!("({expr})$0")),
        _ => None,
}
}

fn singular(expr: &str) -> String {
    let base: String = expr.rsplit(['.', ':']).next().unwrap_or(expr).chars().filter(|c| is_word_char(*c)).collect();
    if base.len() > 1 && base.ends_with('s') {
        base[..base.len() - 1].to_string()
    } else if base.is_empty() {
        "item".into()
    } else {
        "item".into()
    }
}

fn collect_errors(tree: &Tree) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if out.len() >= 100 {
            break;
        }
        if node.is_error() || node.is_missing() {
            let r = node.byte_range();
            out.push(r.start..r.end.max(r.start + 1));
            continue;
        }
        if node.has_error() {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
    }
    out.sort_by_key(|r| r.start);
    out
}

/// Outline items that get an `N usages` lens, with a key stable across edits:
/// (name, ordinal among same-named items) — e.g. several `fn new` in one file.
pub(crate) fn lens_items(outline: &[OutlineItem]) -> Vec<(&OutlineItem, (String, usize))> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    outline
        .iter()
        .filter(|i| matches!(i.kind, SymbolKind::Function | SymbolKind::Method | SymbolKind::Struct | SymbolKind::Class | SymbolKind::Enum | SymbolKind::Trait | SymbolKind::Interface))
        .map(|i| {
            let n = seen.entry(i.name.as_str()).or_insert(0);
            *n += 1;
            (i, (i.name.clone(), *n - 1))
        })
        .collect()
}

/// `N usages` lenses: language-server reference counts when known, else same-file
/// identifier occurrences minus the declaration.
fn build_lenses(outline: &[OutlineItem], counts: &HashMap<String, u32>, usages: &HashMap<(String, usize), usize>) -> Vec<(usize, SharedString)> {
    let mut lenses: Vec<(usize, SharedString)> = lens_items(outline)
        .into_iter()
        .map(|(i, key)| {
            let uses = match usages.get(&key) {
                Some(&n) => n as u32,
                None => counts.get(&i.name).copied().unwrap_or(1).saturating_sub(1),
            };
            let label = match uses {
                0 => "no usages".to_string(),
                1 => "1 usage".to_string(),
                n => format!("{n} usages"),
            };
            (i.row, SharedString::from(label))
        })
        .collect();
    lenses.sort_by_key(|(r, _)| *r);
    lenses.dedup_by_key(|(r, _)| *r);
    lenses
}

fn collect_words(text: &str) -> Vec<String> {
    let mut words = std::collections::HashSet::new();
    let mut current = String::new();
    for ch in text.chars() {
        if is_word_char(ch) {
            current.push(ch);
        } else {
            if current.len() >= 3 && !current.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                words.insert(std::mem::take(&mut current));
            }
            current.clear();
        }
        if words.len() > 20_000 {
            break;
        }
    }
    words.into_iter().collect()
}

fn keywords(lang: LanguageId) -> &'static [&'static str] {
    match lang {
        LanguageId::Rust => &[
            "as", "async", "await", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "for", "impl", "in", "let",
            "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true",
            "type", "unsafe", "use", "where", "while", "Option", "Result", "Some", "None", "Ok", "Err", "String", "Vec", "Box", "println",
        ],
        LanguageId::Python => &[
            "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else",
            "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise",
            "return", "try", "while", "with", "yield", "print", "len", "range", "self",
        ],
        LanguageId::TypeScript | LanguageId::Tsx | LanguageId::JavaScript => &[
            "async", "await", "break", "case", "catch", "class", "const", "continue", "default", "delete", "else", "export", "extends",
            "false", "finally", "for", "function", "if", "import", "in", "instanceof", "interface", "let", "new", "null", "return",
            "switch", "this", "throw", "true", "try", "type", "typeof", "undefined", "var", "while", "console", "Promise",
        ],
        LanguageId::Go => &[
            "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for", "func", "go", "goto", "if",
            "import", "interface", "map", "package", "range", "return", "select", "struct", "switch", "type", "var", "nil", "true", "false",
        ],
        LanguageId::C | LanguageId::Cpp => &[
            "auto", "break", "case", "char", "class", "const", "continue", "default", "double", "else", "enum", "float", "for", "if",
            "include", "int", "long", "namespace", "nullptr", "return", "short", "sizeof", "static", "struct", "switch", "template",
            "typedef", "unsigned", "using", "void", "while", "std", "printf",
        ],
        _ => &[],
    }
}

/// Expand tabs, splice inlay hints in, and remember per column:
/// * `caret[col]`  — display byte where the caret sits (before any hint at `col`),
/// * `start[col]`  — display byte where the char's glyph begins (after hints).
fn display_line(text: &str, inlays: &[(usize, String)]) -> (String, Vec<usize>, Vec<usize>) {
    let mut out = String::with_capacity(text.len() + 16);
    let mut caret = Vec::with_capacity(text.len() + 1);
    let mut start = Vec::with_capacity(text.len() + 1);
    let mut col = 0usize;
    let mut hint = 0usize;
    let chars: Vec<char> = text.chars().take(MAX_RENDER_COLS).collect();
    for (ix, ch) in chars.iter().enumerate() {
        caret.push(out.len());
        while hint < inlays.len() && inlays[hint].0 == ix {
            out.push_str(&inlays[hint].1);
            hint += 1;
        }
        start.push(out.len());
        if *ch == '\t' {
            let n = TAB_WIDTH - col % TAB_WIDTH;
            for _ in 0..n {
                out.push(' ');
            }
            col += n;
        } else if ch.is_control() {
            out.push('\u{2400}');
            col += 1;
        } else {
            out.push(*ch);
            col += 1;
        }
    }
    caret.push(out.len());
    while hint < inlays.len() {
        if inlays[hint].0 >= chars.len() {
            out.push_str(&inlays[hint].1);
        }
        hint += 1;
    }
    start.push(out.len());
    (out, caret, start)
}

struct PaintRow {
    y: Pixels,
    kind: RowKind,
    row: Option<usize>,
    /// Debugger inline values painted after the line end.
    inline: Option<ShapedLine>,
    /// Fold arrow: Some(true) folded (shows `⋯` after the text), Some(false) foldable.
    fold: Option<bool>,
}

struct PaintState {
    /// Prepaint start, for `GRAPHYNE_FRAME_TRACE`.
    frame_start: std::time::Instant,
    rows: Vec<PaintRow>,
    gutter_width: Pixels,
    text_x: Pixels,
    line_height: Pixels,
    char_width: Pixels,
    cursor: Option<(usize, usize)>,
    selection: Range<usize>,
    focused: bool,
    show_cursor: bool,
    hunks: Arc<Vec<DiffHunk>>,
    cursor_row: usize,
    scrollbar: Option<Bounds<Pixels>>,
    number_runs_font: gpui::Font,
    breakpoints: Vec<(usize, BpKind)>,
    exec_line: Option<usize>,
    show_fold_arrows: bool,
}

fn editor_element(editor: Entity<Editor>) -> impl IntoElement {
    let prepaint_editor = editor.clone();
    canvas(
        move |bounds, window, cx| {
            let frame_start = std::time::Instant::now();
            let code_font = gpui::font(theme::fonts().code);
            let font_size = px(theme::code_size());
            let line_height = px(theme::code_line_height());
            let text_system = window.text_system().clone();
            let font_id = text_system.resolve_font(&code_font);
            let char_width = text_system.advance(font_id, font_size, 'm').map(|s| s.width).unwrap_or(px(8.));

            let focused = prepaint_editor.read(cx).focus_handle.is_focused(window);
            // Autoscroll before computing the viewport.
            prepaint_editor.update(cx, |e, _| {
                let digits = (e.buffer.len_lines().max(1) as f32).log10().floor() as usize + 1;
                let gutter = px(18.) + char_width * digits.max(3) as f32 + px(22. + FOLD_W);
                e.reveal_caret();
                if e.autoscroll || e.center_on_cursor {
                    e.restore_top = None;
                    let (row, col) = e.buffer.row_col(e.sel.head);
                    let drow = e.display_row(row) as f32;
                    let top = line_height * drow;
                    let view_h = bounds.size.height;
                    if e.center_on_cursor {
                        e.scroll_y = (top - view_h * 0.35).max(px(0.));
                    } else if top < e.scroll_y + line_height {
                        e.scroll_y = (top - line_height).max(px(0.));
                    } else if top + line_height * 2. > e.scroll_y + view_h {
                        e.scroll_y = top + line_height * 2. - view_h;
                    }
                    let cx_pos = char_width * col as f32;
                    let text_w = bounds.size.width - gutter - px(20.);
                    if cx_pos < e.scroll_x {
                        e.scroll_x = (cx_pos - char_width * 4.).max(px(0.));
                    } else if cx_pos > e.scroll_x + text_w {
                        e.scroll_x = cx_pos - text_w + char_width * 4.;
                    }
                    e.autoscroll = false;
                    e.center_on_cursor = false;
                }
                e.scroll_y = e.scroll_y.max(px(0.));
            });

            let e = prepaint_editor.read(cx);
            let (breakpoints, exec_line, locals) = match (&e.debugger, &e.path) {
                (Some(d), Some(path)) => {
                    let d = d.read(cx);
                    let exec = d.execution_line(path);
                    (d.breakpoints_for(path), exec, if exec.is_some() { d.inline_locals() } else { Vec::new() })
                }
                _ => (Vec::new(), None, Vec::new()),
            };
            // Innermost symbol containing the execution line bounds the inline-values region.
            let func_start = exec_line.map(|exec| {
                e.outline
                    .iter()
                    .filter(|i| i.row <= exec && e.buffer.rope().byte_to_line(i.byte_range.end.min(e.buffer.len_bytes())) >= exec)
                    .map(|i| i.row)
                    .max()
                    .unwrap_or(exec.saturating_sub(30))
            });
            let digits = (e.buffer.len_lines().max(1) as f32).log10().floor() as usize + 1;
            let gutter_width = px(18.) + char_width * digits.max(3) as f32 + px(22. + FOLD_W);
            let text_x = bounds.left() + gutter_width - e.scroll_x;
            let total_rows = e.total_display_rows();
            let first_display = (e.scroll_y / line_height).floor().max(0.) as usize;
            let visible = (bounds.size.height / line_height).ceil() as usize + 1;
            let y_offset = bounds.top() - (e.scroll_y - line_height * first_display as f32);

            // Map display rows -> buffer rows (lenses interleaved).
            let len_lines = e.buffer.len_lines();
            let mut lo = 0usize;
            let mut hi = len_lines;
            while lo < hi {
                let mid = (lo + hi) / 2;
                if e.display_row(mid) < first_display { lo = mid + 1 } else { hi = mid }
            }
            let mut items: Vec<(usize, Option<usize>, Option<SharedString>)> = Vec::new(); // (display_row, buffer_row, lens)
            let mut r = lo;
            while r < len_lines && items.len() < visible + 1 {
                if let Some((_, last_hidden)) = e.hidden_span(r) {
                    r = last_hidden + 1;
                    continue;
                }
                let d = e.display_row(r);
                if let Ok(ix) = e.lenses.binary_search_by_key(&r, |(lr, _)| *lr) {
                    if d >= 1 && d - 1 >= first_display {
                        items.push((d - 1, None, Some(e.lenses[ix].1.clone())));
                    }
                }
                if d >= first_display {
                    items.push((d, Some(r), None));
                }
                if d > first_display + visible {
                    break;
                }
                r += 1;
            }
            let _ = total_rows;

            // Highlights for the visible byte range.
            let first_row = items.iter().find_map(|i| i.1).unwrap_or(0);
            let last_row = items.iter().rev().find_map(|i| i.1).unwrap_or(0);
            let rope = e.buffer.rope();
            let start_byte = rope.line_to_byte(first_row.min(len_lines));
            let end_byte = if last_row + 1 >= len_lines { rope.len_bytes() } else { rope.line_to_byte(last_row + 1) };
            let spans = match &e.tree {
                Some(tree) => graphyne_syntax::highlight(e.language, tree, rope, start_byte..end_byte),
                None => Vec::new(),
            };
            let errors = e.syntax_errors.clone();
            let secrets = e.secrets.clone();
            let lsp_diags = e.lsp.diags.clone();
            let use_lsp_diags = e.lsp.client.is_some();

            let mut rows = Vec::new();
            let mut span_ix = 0;
            for (d, buffer_row, lens) in items {
                let y = y_offset + line_height * (d as f32 - first_display as f32);
                if let Some(label) = lens {
                    // Anchor the lens at the indentation of the following line.
                    let next_row = buffer_row.unwrap_or_else(|| {
                        e.lenses.iter().find(|(lr, l)| *l == label && e.display_row(*lr) == d + 1).map(|(lr, _)| *lr).unwrap_or(0)
                    });
                    let indent = e.buffer.indentation(next_row).chars().count();
                    rows.push(PaintRow { y, kind: RowKind::Lens { label, indent_x: char_width * indent as f32 }, row: None, inline: None, fold: None });
                    continue;
                }
                let Some(row) = buffer_row else { continue };
                let text = e.buffer.line_text(row);
                let inlays = e.hints_for_row(row);
                let (display, col_to_byte, char_start) = display_line(&text, &inlays);
                let line_start_byte = rope.line_to_byte(row);
                let line_start_char = rope.line_to_char(row);
                let row_diags: Vec<lsp::EditorDiag> = if use_lsp_diags {
                    let line_end_char = line_start_char + text.chars().count() + 1;
                    lsp_diags.iter().filter(|d| d.range.start < line_end_char && d.range.end > line_start_char).cloned().collect()
                } else {
                    Vec::new()
                };
                // Per-char highlight kind.
                let mut kinds: Vec<HighlightKind> = Vec::with_capacity(col_to_byte.len());
                let mut underline: Vec<Option<gpui::Hsla>> = Vec::with_capacity(col_to_byte.len());
                let mut byte = line_start_byte;
                let secret_cols: Vec<(usize, usize)> = secrets.iter().filter(|s| s.line == row).map(|s| (s.column, s.column + 8)).collect();
                for (col, ch) in text.chars().take(MAX_RENDER_COLS).enumerate() {
                    while span_ix < spans.len() && spans[span_ix].range.end <= byte {
                        span_ix += 1;
                    }
                    let kind = spans
                        .get(span_ix)
                        .filter(|s| s.range.start <= byte && byte < s.range.end)
                        .map(|s| s.kind)
                        .unwrap_or(HighlightKind::Plain);
                    kinds.push(kind);
                    let diag = if use_lsp_diags {
                        Editor::lsp_underline(&row_diags, line_start_char + col)
                    } else {
                        errors.iter().any(|r| r.start <= byte && byte < r.end).then(|| color(c::RED))
                    };
                    let sec = secret_cols.iter().any(|(s, e)| col >= *s && col < *e);
                    underline.push(diag.or_else(|| sec.then(|| color(c::YELLOW))));
                    byte += ch.len_utf8();
                }
                // Build runs over the display string.
                let mut runs: Vec<TextRun> = Vec::new();
                let hint_run = |len: usize| TextRun {
                    len,
                    font: code_font.clone(),
                    color: color(c::TEXT_DIM),
                    background_color: Some(color_a(0xFFFFFF, 0.055)),
                    underline: None,
                    strikethrough: None,
                };
                let mut last_was_hint = false;
                for (col, kind) in kinds.iter().enumerate() {
                    let hint_len = char_start[col] - col_to_byte[col];
                    if hint_len > 0 {
                        runs.push(hint_run(hint_len));
                        last_was_hint = true;
                    }
                    let len = col_to_byte[col + 1] - char_start[col];
                    let ul = underline[col];
                    if let Some(last) = runs.last_mut().filter(|_| !last_was_hint) {
                        if last.color == theme::syntax(*kind) && last.underline.as_ref().and_then(|u| u.color) == ul {
                            last.len += len;
                            continue;
                        }
                    }
                    let mut f = code_font.clone();
                    if theme::is_italic(*kind) {
                        f.style = FontStyle::Italic;
                    }
                    runs.push(TextRun {
                        len,
                        font: f,
                        color: theme::syntax(*kind),
                        background_color: None,
                        underline: ul.map(|c| UnderlineStyle { color: Some(c), thickness: px(1.), wavy: true }),
                        strikethrough: None,
                    });
                    last_was_hint = false;
                }
                let tail = char_start.last().copied().unwrap_or(0) - col_to_byte.last().copied().unwrap_or(0);
                if tail > 0 {
                    runs.push(hint_run(tail));
                }
                let shaped = text_system.shape_line(display.into(), font_size, &runs, None);
                // Inline debugger values ("name = value") for lines of the paused function.
                let inline = match (exec_line, func_start) {
                    (Some(exec), Some(start)) if row >= start && row <= exec && !locals.is_empty() => {
                        let mut seen: Vec<&str> = Vec::new();
                        for word in text.split(|ch: char| !is_word_char(ch)).filter(|w| !w.is_empty()) {
                            if !seen.contains(&word) && locals.iter().any(|(n, _)| n == word) {
                                seen.push(word);
                            }
                        }
                        let parts: Vec<String> = seen
                            .iter()
                            .filter_map(|w| locals.iter().find(|(n, _)| n == w))
                            .map(|(n, v)| {
                                let v: String = if v.chars().count() > 40 { format!("{}…", v.chars().take(40).collect::<String>()) } else { v.clone() };
                                format!("{n} = {v}")
                            })
                            .collect();
                        (!parts.is_empty()).then(|| {
                            let label: SharedString = parts.join(", ").into();
                            let mut f = code_font.clone();
                            f.style = FontStyle::Italic;
                            let run = TextRun { len: label.len(), font: f, color: color(0x7F93B5), background_color: None, underline: None, strikethrough: None };
                            text_system.shape_line(label, font_size, &[run], None)
                        })
                    }
                    _ => None,
                };
                rows.push(PaintRow {
                    y,
                    kind: RowKind::Line { row, line: shaped, col_to_byte: Arc::new(col_to_byte), char_start: Arc::new(char_start) },
                    row: Some(row),
                    inline,
                    fold: e.fold_range_at(row).map(|_| e.folded.contains(&row)),
                });
            }

            let sel = e.sel.range();
            let (cursor_row, cursor_col) = e.buffer.row_col(e.sel.head);
            let total_h = line_height * total_rows as f32;
            let scrollbar = (total_h > bounds.size.height).then(|| {
                let ratio = bounds.size.height / total_h;
                let thumb_h = (bounds.size.height * ratio).max(px(24.));
                let top = bounds.top() + (bounds.size.height - thumb_h) * (e.scroll_y / (total_h - bounds.size.height).max(px(1.))).min(1.);
                Bounds::new(point(bounds.right() - px(9.), top), size(px(6.), thumb_h))
            });
            PaintState {
                frame_start,
                rows,
                gutter_width,
                text_x,
                line_height,
                char_width,
                cursor: Some((cursor_row, cursor_col)),
                selection: sel,
                focused,
                show_cursor: e.cursor_visible,
                show_fold_arrows: e.mouse_in_gutter,
                hunks: e.hunks.clone(),
                cursor_row,
                scrollbar,
                number_runs_font: code_font,
                breakpoints,
                exec_line,
            }
        },
        move |bounds, state, window, cx| {
            let focus_handle = editor.read(cx).focus_handle.clone();
            window.handle_input(&focus_handle, ElementInputHandler::new(bounds, editor.clone()), cx);
            let lh = state.line_height;
            let text_area = Bounds::from_corners(point(bounds.left() + state.gutter_width, bounds.top()), bounds.bottom_right());

            // Active line, breakpoint lines, execution line.
            for r in &state.rows {
                let Some(row) = r.row else { continue };
                let full = Bounds::new(point(bounds.left(), r.y), size(bounds.size.width, lh));
                if state.exec_line == Some(row) {
                    window.paint_quad(fill(full, color_a(c::ACCENT, 0.26)));
                } else if let Some((_, kind)) = state.breakpoints.iter().find(|(r, _)| *r == row) {
                    let tint = if *kind == BpKind::Log { BP_LOG } else { BP_RED };
                    window.paint_quad(fill(full, color_a(tint, 0.10)));
                } else if row == state.cursor_row && state.selection.is_empty() {
                    window.paint_quad(fill(full, color(c::ACTIVE_LINE)));
                }
            }

            let row_spans: Vec<Option<(usize, usize)>> = {
                let e = editor.read(cx);
                state.rows.iter().map(|r| r.row.map(|row| (e.buffer.line_to_char(row), e.buffer.line_len(row)))).collect()
            };
            let sel_color = if state.focused { color(c::SELECTION) } else { color(c::SELECTION_INACTIVE) };
            window.with_content_mask(Some(gpui::ContentMask { bounds: text_area }), |window| {
                // Selection.
                if !state.selection.is_empty() {
                    for (ri, r) in state.rows.iter().enumerate() {
                        let RowKind::Line { line, col_to_byte, .. } = &r.kind else { continue };
                        let Some((line_start, line_len)) = row_spans[ri] else { continue };
                        let s = state.selection.start.max(line_start);
                        let en = state.selection.end.min(line_start + line_len + 1);
                        if s >= en && !(state.selection.start <= line_start && state.selection.end > line_start + line_len) {
                            continue;
                        }
                        let sc = s.saturating_sub(line_start).min(line_len);
                        let ec = en.saturating_sub(line_start).min(line_len);
                        let x0 = line.x_for_index(col_to_byte[sc.min(col_to_byte.len() - 1)]);
                        let mut x1 = line.x_for_index(col_to_byte[ec.min(col_to_byte.len() - 1)]);
                        if state.selection.end > line_start + line_len {
                            x1 += state.char_width * 0.6;
                        }
                        if x1 > x0 {
                            window.paint_quad(
                                fill(Bounds::from_corners(point(state.text_x + x0, r.y), point(state.text_x + x1, r.y + lh)), sel_color)
                                    .corner_radii(px(3.)),
                            );
                        }
                    }
                }
                // Text + lenses.
                for r in &state.rows {
                    match &r.kind {
                        RowKind::Line { line, .. } => {
                            let _ = line.paint(point(state.text_x, r.y), lh, window, cx);
                            let mut after = state.text_x + line.width + state.char_width * 3.;
                            if r.fold == Some(true) {
                                // `⋯` marker for the hidden lines (click to unfold).
                                let x = state.text_x + line.width + state.char_width;
                                let w = state.char_width * 3. + px(6.);
                                window.paint_quad(fill(Bounds::new(point(x, r.y + px(3.)), size(w, lh - px(6.))), color_a(c::TEXT_DIM, 0.28)).corner_radii(px(4.)));
                                let run = TextRun { len: "⋯".len(), font: state.number_runs_font.clone(), color: color(c::TEXT_MUTED), background_color: None, underline: None, strikethrough: None };
                                let dots = window.text_system().shape_line("⋯".into(), px(theme::code_size()), &[run], None);
                                let _ = dots.paint(point(x + (w - dots.width) / 2., r.y), lh, window, cx);
                                after = x + w + state.char_width * 2.;
                            }
                            if let Some(inline) = &r.inline {
                                let _ = inline.paint(point(after, r.y), lh, window, cx);
                            }
                        }
                        RowKind::Lens { label, indent_x } => {
                            let run = TextRun {
                                len: label.len(),
                                font: gpui::font(theme::fonts().ui),
                                color: color(c::TEXT_DIM),
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            let shaped = window.text_system().shape_line(label.clone(), px(11.5), &[run], None);
                            let _ = shaped.paint(point(state.text_x + *indent_x, r.y + px(3.)), lh, window, cx);
                        }
                    }
                }
                // Cursor.
                if state.focused && state.show_cursor {
                    if let Some((crow, ccol)) = state.cursor {
                        for r in &state.rows {
                            if let RowKind::Line { row, line, col_to_byte, .. } = &r.kind {
                                if *row == crow {
                                    let x = line.x_for_index(col_to_byte[ccol.min(col_to_byte.len() - 1)]);
                                    window.paint_quad(fill(Bounds::new(point(state.text_x + x - px(0.5), r.y + px(2.)), size(px(2.), lh - px(4.))), color(c::CURSOR)));
                                }
                            }
                        }
                    }
                }
            });

            // Gutter: breakpoints, execution arrow, numbers, git markers.
            let num_size = px(theme::code_size() - 1.0);
            for r in &state.rows {
                let Some(row) = r.row else { continue };
                // Gutter markers share one 12 px slot left of the line numbers:
                // ● breakpoint, ● with "?" conditional, ◆ logpoint, ➜ execution point.
                // Stopped on a breakpoint line: the marker stays and gets a white arrow inside.
                let center = point(bounds.left() + px(11.), r.y + lh / 2.);
                let bp = state.breakpoints.iter().find(|(r, _)| *r == row).map(|(_, k)| *k);
                let on_exec = state.exec_line == Some(row);
                if let Some(kind) = bp {
                    let d = px(12.);
                    let disc = Bounds::new(point(center.x - d / 2., center.y - d / 2.), size(d, d));
                    match kind {
                        BpKind::Plain | BpKind::Conditional => window.paint_quad(fill(disc, color(BP_RED)).corner_radii(d / 2.)),
                        BpKind::Log => paint_polygon(
                            window,
                            &[point(center.x, center.y - px(6.5)), point(center.x + px(6.5), center.y), point(center.x, center.y + px(6.5)), point(center.x - px(6.5), center.y)],
                            color(BP_LOG),
                        ),
                    }
                    if kind == BpKind::Conditional && !on_exec {
                        let run = TextRun { len: 1, font: gpui::Font { weight: gpui::FontWeight::BOLD, ..state.number_runs_font.clone() }, color: color(0xFFFFFF), background_color: None, underline: None, strikethrough: None };
                        let q = window.text_system().shape_line("?".into(), px(9.5), &[run], None);
                        let _ = q.paint(point(center.x - q.width / 2., center.y - px(6.)), px(12.), window, cx);
                    }
                    if on_exec {
                        paint_polygon(window, &exec_arrow(center, 0.6), color(0xFFFFFF));
                    }
                } else if on_exec {
                    paint_polygon(window, &exec_arrow(center, 1.0), color(c::YELLOW));
                }
                let label: SharedString = (row + 1).to_string().into();
                let col = if row == state.cursor_row { color(c::TEXT_MUTED) } else { color(c::TEXT_FAINT) };
                let run = TextRun { len: label.len(), font: state.number_runs_font.clone(), color: col, background_color: None, underline: None, strikethrough: None };
                let shaped = window.text_system().shape_line(label, num_size, &[run], None);
                let x = bounds.left() + state.gutter_width - px(20. + FOLD_W) - shaped.width;
                let _ = shaped.paint(point(x, r.y + px(1.)), lh, window, cx);
                if let Some(folded) = r.fold {
                    if folded || state.show_fold_arrows {
                        let c = point(bounds.left() + state.gutter_width - px(FOLD_W / 2. + 2.), r.y + lh / 2.);
                        let p = |x: f32, y: f32| point(c.x + px(x), c.y + px(y));
                        if folded {
                            paint_polygon(window, &[p(-2.5, -4.), p(3., 0.), p(-2.5, 4.)], color(c::TEXT_MUTED));
                        } else {
                            paint_polygon(window, &[p(-4., -2.5), p(4., -2.5), p(0., 3.)], color(c::TEXT_FAINT));
                        }
                    }
                }
                for h in state.hunks.iter() {
                    let marker_x = bounds.left() + state.gutter_width - px(12. + FOLD_W);
                    match h.kind {
                        LineChange::Added | LineChange::Modified if h.new.contains(&(row as u32)) => {
                            let col = if h.kind == LineChange::Added { color(c::GIT_ADDED) } else { color(c::GIT_MODIFIED) };
                            window.paint_quad(fill(Bounds::new(point(marker_x, r.y), size(px(3.), lh)), col).corner_radii(px(1.5)));
                        }
                        LineChange::Deleted if h.new.start == row as u32 + 1 || (h.new.start == 0 && row == 0) => {
                            let y = if h.new.start == 0 { r.y } else { r.y + lh - px(3.) };
                            window.paint_quad(fill(Bounds::new(point(marker_x - px(1.), y), size(px(6.), px(5.))), color(c::GIT_DELETED)).corner_radii(px(2.)));
                        }
                        _ => {}
                    }
                }
            }

            if let Some(thumb) = state.scrollbar {
                window.paint_quad(fill(thumb, color_a(0xFFFFFF, 0.10)).corner_radii(px(3.)));
            }

            let rows: Vec<RowLayout> = state.rows.iter().map(|r| RowLayout { y: r.y, kind: r.kind.clone() }).collect();
            let viewport_rows = (bounds.size.height / lh).floor() as usize;
            editor.update(cx, |e, _| {
                e.layout = Some(LayoutCache {
                    bounds,
                    text_x: state.text_x,
                    gutter_width: state.gutter_width,
                    line_height: lh,
                    char_width: state.char_width,
                    rows,
                    viewport_rows,
                });
            });
            frame_trace::record(state.frame_start.elapsed());
        },
    )
    .size_full()
}

const BP_RED: u32 = 0xE5534B;
const BP_LOG: u32 = 0xE08A4A;

/// Execution-point arrow (shaft + head) centred on `c`; `scale` 1.0 = 13 × 11 px.
/// Width of the fold-arrow column at the right edge of the gutter.
const FOLD_W: f32 = 14.;

fn exec_arrow(c: Point<Pixels>, scale: f32) -> [Point<Pixels>; 7] {
    let p = |x: f32, y: f32| point(c.x + px(x * scale), c.y + px(y * scale));
    [p(-6.5, -2.5), p(0.5, -2.5), p(0.5, -5.5), p(6.5, 0.), p(0.5, 5.5), p(0.5, 2.5), p(-6.5, 2.5)]
}

fn paint_polygon(window: &mut Window, points: &[Point<Pixels>], fill_color: gpui::Hsla) {
    let Some((first, rest)) = points.split_first() else { return };
    let mut path = gpui::PathBuilder::fill();
    path.move_to(*first);
    for p in rest {
        path.line_to(*p);
    }
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, fill_color);
    }
}

/// `GRAPHYNE_FRAME_TRACE=1`: every 2 s print how many editor frames were drawn and how long
/// our prepaint + paint took (CPU side; presentation is vsync-paced by gpui).
mod frame_trace {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    struct Stats {
        since: Instant,
        frames: u32,
        total: Duration,
        max: Duration,
    }

    static STATS: Mutex<Option<Stats>> = Mutex::new(None);

    pub fn record(spent: Duration) {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ENABLED.get_or_init(|| std::env::var_os("GRAPHYNE_FRAME_TRACE").is_some()) {
            return;
        }
        let mut guard = STATS.lock().unwrap_or_else(|e| e.into_inner());
        let st = guard.get_or_insert_with(|| Stats { since: Instant::now(), frames: 0, total: Duration::ZERO, max: Duration::ZERO });
        st.frames += 1;
        st.total += spent;
        st.max = st.max.max(spent);
        let window = st.since.elapsed();
        if window >= Duration::from_secs(2) {
            eprintln!(
                "frames: {:>4} in {:.1} s = {:>5.1} fps | editor prepaint+paint avg {:.2} ms, max {:.2} ms",
                st.frames,
                window.as_secs_f64(),
                st.frames as f64 / window.as_secs_f64(),
                st.total.as_secs_f64() * 1000.0 / st.frames as f64,
                st.max.as_secs_f64() * 1000.0
            );
            *guard = None;
        }
    }
}

impl Editor {
    /// Window-space position of the caret (for popups).
    fn caret_point(&self) -> Option<Point<Pixels>> {
        let layout = self.layout.as_ref()?;
        let (crow, ccol) = self.buffer.row_col(self.sel.head);
        layout.rows.iter().find_map(|r| match &r.kind {
            RowKind::Line { row, line, col_to_byte, .. } if *row == crow => {
                Some(point(layout.text_x + line.x_for_index(col_to_byte[ccol.min(col_to_byte.len() - 1)]), r.y + layout.line_height))
            }
            _ => None,
        })
    }

    fn render_completion(&self, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if !self.focus_handle.is_focused(window) {
            return None;
        }
        let comp = self.completion.as_ref()?;
        let at = self.caret_point()?;
        let items = comp.items.clone();
        let selected = comp.selected;
        let word_len = self.sel.head - comp.word_start;
        let char_w = self.layout.as_ref().map(|l| l.char_width).unwrap_or(px(8.));
        let origin = point(at.x - char_w * word_len as f32 - px(30.), at.y + px(2.));
        let entity = cx.entity();
        Some(deferred(
            anchored().position(origin).snap_to_window_with_margin(px(8.)).child(
                div()
                    .id("completion")
                    .occlude()
                    .min_w(px(320.))
                    .max_w(px(620.))
                    .max_h(px(330.))
                    .overflow_y_scroll()
                    .p(px(4.))
                    .rounded(px(8.))
                    .bg(color(c::POPUP))
                    .border_1()
                    .border_color(color(c::POPUP_BORDER))
                    .shadow_lg()
                    .font_family(theme::fonts().code)
                    .text_size(px(theme::code_size() - 0.5))
                    .children(items.into_iter().enumerate().map(move |(ix, entry)| {
                        let entity = entity.clone();
                        let (badge, badge_color) = if entry.lsp.is_some() || entry.kind == 14 {
                            graphyne_lsp::model::kind_badge(entry.kind)
                        } else {
                            ("abc", c::TEXT_DIM)
                        };
                        let (label, indices, detail) = (entry.label, entry.indices, entry.detail);
                        div()
                            .id(ix)
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .h(px(26.))
                            .px(px(8.))
                            .rounded(px(5.))
                            .when(ix == selected, |d| d.bg(color(c::ACCENT_SOFT)))
                            .hover(|d| d.bg(color(c::HOVER)))
                            .cursor_pointer()
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |e, cx| {
                                    if let Some(comp) = e.completion.as_mut() {
                                        comp.selected = ix;
                                    }
                                    e.accept_completion(cx);
                                })
                            })
                            .child(
                                div()
                                    .w(px(22.))
                                    .flex_none()
                                    .text_size(px(11.))
                                    .text_color(color(badge_color))
                                    .font_family(theme::fonts().ui)
                                    .child(badge),
                            )
                            .child(highlighted_label(&label, &indices, color(c::TEXT), color(c::MATCH)))
                            .child(div().flex_1())
                            .children(detail.map(|d| {
                                div()
                                    .max_w(px(220.))
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_size(px(11.5))
                                    .text_color(color(c::TEXT_DIM))
                                    .font_family(theme::fonts().ui)
                                    .child(d)
                            }))
                    })),
            ),
        ))
    }

    fn render_diff_popup(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let ix = self.diff_popup?;
        let hunk = self.hunks.get(ix)?.clone();
        let layout = self.layout.as_ref()?;
        let row_y = layout.rows.iter().find_map(|r| match &r.kind {
            RowKind::Line { row, .. } if *row as u32 == hunk.new.start.saturating_sub((hunk.kind == LineChange::Deleted) as u32) || *row as u32 == hunk.new.start => Some(r.y),
            _ => None,
        })?;
        let base = self.base_text.clone()?;
        let old: Vec<String> = base.lines().skip(hunk.old.start as usize).take(((hunk.old.end - hunk.old.start) as usize).min(10)).map(|s| s.to_string()).collect();
        let entity = cx.entity();
        let title = match hunk.kind {
            LineChange::Added => format!("{} added line(s)", hunk.new.end - hunk.new.start),
            LineChange::Modified => "Modified lines — HEAD version:".to_string(),
            LineChange::Deleted => format!("{} deleted line(s):", hunk.old.end - hunk.old.start),
        };
        Some(deferred(
            anchored().position(point(layout.bounds.left() + layout.gutter_width + px(4.), row_y + layout.line_height)).snap_to_window_with_margin(px(8.)).child(
                div()
                    .id("diff-popup")
                    .occlude()
                    .w(px(460.))
                    .rounded(px(8.))
                    .bg(color(c::POPUP))
                    .border_1()
                    .border_color(color(c::POPUP_BORDER))
                    .shadow_lg()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px(px(10.))
                            .h(px(32.))
                            .border_b_1()
                            .border_color(color(c::BORDER))
                            .font_family(theme::fonts().ui)
                            .text_size(px(theme::UI_SMALL))
                            .text_color(color(c::TEXT_MUTED))
                            .child(title)
                            .child(
                                div()
                                    .id("rollback")
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .px(px(8.))
                                    .h(px(24.))
                                    .rounded(px(5.))
                                    .cursor_pointer()
                                    .text_color(color(c::TEXT))
                                    .hover(|d| d.bg(color(c::HOVER)))
                                    .child(icon(Icon::Undo, 13., color(c::TEXT_MUTED)))
                                    .child("Rollback Lines")
                                    .on_click(move |_, _, cx| entity.update(cx, |e, cx| e.rollback_hunk(ix, cx))),
                            ),
                    )
                    .when(!old.is_empty(), |d| {
                        d.child(
                            div()
                                .p(px(8.))
                                .font_family(theme::fonts().code)
                                .text_size(px(12.5))
                                .text_color(color(0xC9A3A3))
                                .bg(color_a(c::RED, 0.06))
                                .children(old.into_iter().map(|l| div().whitespace_nowrap().child(if l.is_empty() { " ".to_string() } else { l }))),
                        )
                    }),
            ),
        ))
    }
}

fn keywords_static_contains(word: &str) -> bool {
    [LanguageId::Rust, LanguageId::Python, LanguageId::TypeScript, LanguageId::Go, LanguageId::C]
        .iter()
        .any(|l| keywords(*l).contains(&word))
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let completion = self.render_completion(window, cx);
        let diff_popup = self.render_diff_popup(cx);
        let bp_editor = self.render_bp_editor(cx);
        let focused = self.focus_handle.is_focused(window);
        let hover = self.render_hover(cx);
        let signature = if focused { self.render_signature() } else { None };
        let actions = if focused { self.render_actions(cx) } else { None };
        div()
            .id("editor")
            .key_context("Editor")
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .overflow_hidden()
            .cursor(CursorStyle::IBeam)
            .font_family(theme::fonts().code)
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::move_word_left))
            .on_action(cx.listener(Self::move_word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::move_home))
            .on_action(cx.listener(Self::move_end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::move_doc_start))
            .on_action(cx.listener(Self::move_doc_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::newline_below))
            .on_action(cx.listener(Self::tab))
            .on_action(cx.listener(Self::outdent))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::duplicate_line))
            .on_action(cx.listener(Self::delete_line))
            .on_action(cx.listener(Self::toggle_comment))
            .on_action(cx.listener(Self::fold_region))
            .on_action(cx.listener(Self::unfold_region))
            .on_action(cx.listener(Self::fold_all))
            .on_action(cx.listener(Self::unfold_all))
            .on_action(cx.listener(Self::extend_selection))
            .on_action(cx.listener(Self::shrink_selection))
            .on_action(cx.listener(Self::move_line_up))
            .on_action(cx.listener(Self::move_line_down))
            .on_action(cx.listener(Self::escape))
            .on_action(cx.listener(Self::show_completions))
            .on_action(cx.listener(Self::save_action))
            .on_action(cx.listener(Self::quick_documentation))
            .on_action(cx.listener(Self::go_to_definition))
            .on_action(cx.listener(Self::show_code_actions))
            .on_action(cx.listener(Self::parameter_info))
            .on_action(cx.listener(Self::toggle_breakpoint))
            .on_action(cx.listener(Self::edit_breakpoint))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(editor_element(cx.entity()))
            .children(completion)
            .children(diff_popup)
            .children(bp_editor)
            .children(hover)
            .children(signature)
            .children(actions)
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(&mut self, range_utf16: Range<usize>, actual: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let rope = self.buffer.rope();
        let max = rope.len_utf16_cu();
        let s = rope.utf16_cu_to_char(range_utf16.start.min(max));
        let e = rope.utf16_cu_to_char(range_utf16.end.min(max));
        actual.replace(rope.char_to_utf16_cu(s)..rope.char_to_utf16_cu(e));
        Some(rope.slice(s..e).to_string())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let rope = self.buffer.rope();
        let r = self.sel.range();
        Some(UTF16Selection { range: rope.char_to_utf16_cu(r.start)..rope.char_to_utf16_cu(r.end), reversed: self.sel.head < self.sel.anchor })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let rope = self.buffer.rope();
        self.marked_range.as_ref().map(|r| rope.char_to_utf16_cu(r.start)..rope.char_to_utf16_cu(r.end))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        let rope = self.buffer.rope();
        let range = range_utf16
            .map(|r| rope.utf16_cu_to_char(r.start)..rope.utf16_cu_to_char(r.end))
            .or(self.marked_range.take())
            .unwrap_or(self.sel.range());
        self.marked_range = None;
        if text.is_empty() && range.is_empty() {
            return;
        }
        let mut chars = text.chars();
        let single = match (chars.next(), chars.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        };
        if let Some(ch) = single {
            if range.is_empty() {
                // Type-over closing pairs.
                if matches!(ch, ')' | ']' | '}' | '"' | '\'') && self.char_after(range.start) == Some(ch) {
                    self.set_selection(Selection::caret(range.start + 1), false, cx);
                    self.on_char_typed(ch, cx);
                    return;
                }
                let next_is_word = self.char_after(range.start).is_some_and(is_word_char);
                let prev_is_word = self.char_before(range.start).is_some_and(is_word_char);
                let close = match ch {
                    '(' => Some(')'),
                    '[' => Some(']'),
                    '{' => Some('}'),
                    '"' if !prev_is_word => Some('"'),
                    '\'' if !prev_is_word && !matches!(self.language, LanguageId::Rust) => Some('\''),
                    _ => None,
                };
                if let Some(close) = close.filter(|_| !next_is_word) {
                    let s = format!("{ch}{close}");
                    self.replace(range.clone(), &s, Selection::caret(range.start + 1), cx);
                    self.completion = None;
                    self.on_char_typed(ch, cx);
                    return;
                }
            } else if matches!(ch, '(' | '[' | '{' | '"') {
                // Wrap the selection.
                let close = match ch {
                    '(' => ')',
                    '[' => ']',
                    '{' => '}',
                    _ => '"',
                };
                let inner = self.buffer.slice_to_string(range.clone());
                let n = inner.chars().count();
                self.replace(range.clone(), &format!("{ch}{inner}{close}"), Selection::new(range.start + 1, range.start + 1 + n), cx);
                return;
            }
        }
        let end = range.start + text.chars().count();
        self.replace(range, text, Selection::caret(end), cx);
        if single.is_some_and(is_word_char) {
            self.update_completion_inner(false, cx);
        } else {
            self.completion = None;
            self.lsp.completions = None;
        }
        if let Some(ch) = single {
            self.on_char_typed(ch, cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rope = self.buffer.rope();
        let range = range_utf16
            .map(|r| rope.utf16_cu_to_char(r.start)..rope.utf16_cu_to_char(r.end))
            .or(self.marked_range.clone())
            .unwrap_or(self.sel.range());
        let n = text.chars().count();
        let caret = new_selected_range_utf16.map(|r| range.start + r.end).unwrap_or(range.start + n);
        self.replace(range.clone(), text, Selection::caret(caret.min(range.start + n)), cx);
        self.marked_range = if text.is_empty() { None } else { Some(range.start..range.start + n) };
    }

    fn bounds_for_range(&mut self, range_utf16: Range<usize>, _: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let rope = self.buffer.rope();
        let start = rope.utf16_cu_to_char(range_utf16.start.min(rope.len_utf16_cu()));
        let (row, col) = self.buffer.row_col(start);
        layout.rows.iter().find_map(|r| match &r.kind {
            RowKind::Line { row: rr, line, col_to_byte, .. } if *rr == row => {
                let x = layout.text_x + line.x_for_index(col_to_byte[col.min(col_to_byte.len() - 1)]);
                Some(Bounds::new(point(x, r.y), size(layout.char_width, layout.line_height)))
            }
            _ => None,
        })
    }

    fn character_index_for_point(&mut self, p: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let pos = self.hit_test(p)?;
        Some(self.buffer.rope().char_to_utf16_cu(pos))
    }
}

