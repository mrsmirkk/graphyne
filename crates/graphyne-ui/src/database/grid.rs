//! A virtualized table of query results: a header that stays put, row numbers, sorting by a
//! column, resizable columns, horizontal scrolling, cell selection with the keyboard, copy,
//! export — and, for table tabs, editing: changed cells, new and deleted rows wait as pending
//! edits until the tab submits them. Only visible rows — and only visible columns — are
//! built each frame, so hundreds of thousands of rows scroll as smoothly as ten.

use super::group_digits;
use crate::assets::Icon;
use crate::components::icon;
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{
    App, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle, DragMoveEvent, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, ScrollStrategy, ScrollWheelEvent, SharedString, Subscription, UTF16Selection, UniformListScrollHandle,
    Window, anchored, canvas, deferred, div, px, uniform_list,
};
use graphyne_db::{CellEdit, DbKind, ExportFormat, ResultSet, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

const ROW_H: f32 = 24.;
const HEADER_H: f32 = 28.;
/// Cells use the editor font at this size; its advance is close to `CHAR_W`.
const CELL_FONT: f32 = 12.5;
const CHAR_W: f32 = 7.6;
const CELL_PAD: f32 = 9.;
const MIN_W: f32 = 56.;
const MAX_W: f32 = 420.;
/// Rows looked at when sizing columns (spread over the whole result: ids grow).
const SAMPLE_ROWS: usize = 1000;

pub enum GridEvent {
    /// Pending edits changed (the table tab shows Submit / Revert).
    EditsChanged,
    /// A message for the notifications (exports).
    Notify(String),
}

/// Changes waiting for Submit.
#[derive(Clone, Default)]
pub struct Edits {
    /// Changed cells of loaded rows: (row index in the result, column) → new content.
    pub cells: BTreeMap<(usize, usize), CellEdit>,
    /// Loaded rows to delete.
    pub deleted: BTreeSet<usize>,
    /// New rows: column → content (columns left out get their default).
    pub added: Vec<BTreeMap<usize, CellEdit>>,
}

impl Edits {
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.deleted.is_empty() && self.added.is_empty()
    }

    /// Rows touched by the edits.
    pub fn row_count(&self) -> usize {
        let changed: BTreeSet<usize> = self.cells.keys().map(|(r, _)| *r).filter(|r| !self.deleted.contains(r)).collect();
        changed.len() + self.deleted.len() + self.added.len()
    }
}

/// A displayed row: one of the result, or one added in the grid (shown after them).
#[derive(Clone, Copy, PartialEq, Debug)]
enum RowRef {
    Loaded(usize),
    New(usize),
}

struct CellEditor {
    row: RowRef,
    col: usize,
    input: Entity<TextInput>,
    _subs: Vec<Subscription>,
}

struct GridMenu {
    position: Point<Pixels>,
    ix: usize,
    col: usize,
}

pub struct DataGrid {
    focus_handle: FocusHandle,
    columns: Vec<SharedString>,
    rows: Arc<Vec<Vec<Value>>>,
    /// Row indices in display order (sorting).
    order: Arc<Vec<usize>>,
    widths: Vec<f32>,
    /// (column, ascending)
    sort: Option<(usize, bool)>,
    /// (display row, column)
    selected: Option<(usize, usize)>,
    scroll: UniformListScrollHandle,
    scroll_x: f32,
    /// Width of the row-number column (grows with the row count).
    num_w: f32,
    /// Table tabs with a primary key can be edited; query results cannot.
    editable: bool,
    edits: Edits,
    editor: Option<CellEditor>,
    menu: Option<GridMenu>,
}

/// Dragging the right edge of a column header.
#[derive(Clone)]
struct ResizeColumn(usize);

/// Drags here show nothing under the cursor (the column itself follows the mouse).
pub(super) struct NoPreview;

impl Render for NoPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl EventEmitter<GridEvent> for DataGrid {}

impl Focusable for DataGrid {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// A click, not the end of a drag that started on the same element.
fn plain_click(ev: &ClickEvent) -> bool {
    match ev {
        ClickEvent::Mouse(m) => {
            let d = m.up.position - m.down.position;
            f32::from(d.x).abs() + f32::from(d.y).abs() < 4.
        }
        _ => true,
    }
}

impl DataGrid {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            columns: Vec::new(),
            rows: Arc::new(Vec::new()),
            order: Arc::new(Vec::new()),
            widths: Vec::new(),
            sort: None,
            selected: None,
            scroll: UniformListScrollHandle::new(),
            scroll_x: 0.,
            num_w: 40.,
            editable: false,
            edits: Edits::default(),
            editor: None,
            menu: None,
        }
    }

    /// Show `result` (columns sized to their content, back at the top-left, edits dropped).
    pub fn set_result(&mut self, result: ResultSet, cx: &mut Context<Self>) {
        self.widths = column_widths(&result);
        self.columns = result.columns.into_iter().map(SharedString::from).collect();
        self.order = Arc::new((0..result.rows.len()).collect());
        self.num_w = (result.rows.len().max(1).to_string().len() as f32 * CHAR_W + 2. * 8.).max(36.);
        self.rows = Arc::new(result.rows);
        self.sort = None;
        self.selected = None;
        self.scroll_x = 0.;
        self.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
        self.edits = Edits::default();
        self.editor = None;
        self.menu = None;
        cx.emit(GridEvent::EditsChanged);
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_result(ResultSet::default(), cx);
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn has_columns(&self) -> bool {
        !self.columns.is_empty()
    }

    pub fn set_editable(&mut self, editable: bool, cx: &mut Context<Self>) {
        self.editable = editable;
        cx.notify();
    }

    pub fn edits(&self) -> &Edits {
        &self.edits
    }

    pub fn rows(&self) -> Arc<Vec<Vec<Value>>> {
        self.rows.clone()
    }

    pub fn column_names(&self) -> Vec<String> {
        self.columns.iter().map(|c| c.to_string()).collect()
    }

    /// Drop all pending edits.
    pub fn revert(&mut self, cx: &mut Context<Self>) {
        self.edits = Edits::default();
        self.editor = None;
        self.selected = self.selected.filter(|(r, _)| *r < self.order.len());
        cx.emit(GridEvent::EditsChanged);
        cx.notify();
    }

    fn display_count(&self) -> usize {
        self.order.len() + self.edits.added.len()
    }

    fn row_ref(&self, ix: usize) -> Option<RowRef> {
        match self.order.get(ix) {
            Some(r) => Some(RowRef::Loaded(*r)),
            None => (ix - self.order.len() < self.edits.added.len()).then(|| RowRef::New(ix - self.order.len())),
        }
    }

    fn edit_at(&self, row: RowRef, col: usize) -> Option<&CellEdit> {
        match row {
            RowRef::Loaded(r) => self.edits.cells.get(&(r, col)),
            RowRef::New(i) => self.edits.added.get(i)?.get(&col),
        }
    }

    fn col_left(&self, col: usize) -> f32 {
        self.widths[..col.min(self.widths.len())].iter().sum()
    }

    fn total_w(&self) -> f32 {
        self.widths.iter().sum()
    }

    /// Width available to the cells (from the last layout; 0 before the first).
    fn viewport_w(&self) -> f32 {
        let width = f32::from(self.scroll.0.borrow().base_handle.bounds().size.width);
        (width - self.num_w).max(0.)
    }

    fn max_scroll_x(&self) -> f32 {
        (self.total_w() - self.viewport_w()).max(0.)
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let d = event.delta.pixel_delta(px(20.));
        // Shift + wheel scrolls sideways, as in the diff view.
        let dx = if event.modifiers.shift && f32::from(d.x) == 0. { f32::from(d.y) } else { f32::from(d.x) };
        if dx != 0. {
            self.scroll_x = (self.scroll_x - dx).clamp(0., self.max_scroll_x());
            cx.notify();
        }
    }

    /// Ascending → descending → as returned.
    fn toggle_sort(&mut self, col: usize, cx: &mut Context<Self>) {
        self.sort = match self.sort {
            Some((c, true)) if c == col => Some((col, false)),
            Some((c, false)) if c == col => None,
            _ => Some((col, true)),
        };
        let mut order: Vec<usize> = (0..self.rows.len()).collect();
        if let Some((col, ascending)) = self.sort {
            let rows = &self.rows;
            order.sort_by(|a, b| rows[*a][col].sort_cmp(&rows[*b][col]));
            if !ascending {
                order.reverse();
            }
        }
        self.order = Arc::new(order);
        self.selected = None;
        self.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn select(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        let count = self.display_count();
        if count == 0 || self.columns.is_empty() {
            return;
        }
        let (row, col) = (row.min(count - 1), col.min(self.columns.len() - 1));
        self.selected = Some((row, col));
        self.scroll.scroll_to_item(row, ScrollStrategy::Top);
        // Bring the column into view sideways.
        let (left, right) = (self.col_left(col), self.col_left(col) + self.widths[col]);
        let viewport = self.viewport_w();
        if left < self.scroll_x {
            self.scroll_x = left;
        } else if viewport > 0. && right > self.scroll_x + viewport {
            self.scroll_x = (right - viewport).min(left);
        }
        cx.notify();
    }

    /// The selected cell: its column name and what it holds (pending edits included).
    pub fn selected_value(&self) -> Option<(SharedString, Value)> {
        let (ix, col) = self.selected?;
        let row = self.row_ref(ix)?;
        let value = match self.edit_at(row, col) {
            Some(CellEdit::Text(t)) => Value::Text(t.clone()),
            Some(CellEdit::Null) => Value::Null,
            None => match row {
                RowRef::Loaded(r) => self.rows.get(r)?.get(col)?.clone(),
                RowRef::New(_) => Value::Null,
            },
        };
        Some((self.columns.get(col)?.clone(), value))
    }

    /// The displayed row as tab-separated text (Copy Row).
    fn row_text(&self, ix: usize) -> Option<String> {
        let row = self.row_ref(ix)?;
        let cells: Vec<String> = (0..self.columns.len())
            .map(|col| match self.edit_at(row, col) {
                Some(CellEdit::Text(t)) => t.clone(),
                Some(CellEdit::Null) => String::new(),
                None => match row {
                    RowRef::Loaded(r) => self.rows[r][col].export_text(),
                    RowRef::New(_) => String::new(),
                },
            })
            .collect();
        Some(cells.join("\t"))
    }

    // ------------------------------------------------------------------ editing

    /// Edit the cell at display row `ix`: in place, starting from `initial` (a typed
    /// character) or from what the cell holds.
    fn start_edit(&mut self, ix: usize, col: usize, initial: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let Some(row) = self.row_ref(ix).filter(|_| self.editable && col < self.columns.len()) else { return };
        let original = match row {
            RowRef::Loaded(r) if self.edits.deleted.contains(&r) => return,
            RowRef::Loaded(r) => Some(&self.rows[r][col]),
            RowRef::New(_) => None,
        };
        let current = match (self.edit_at(row, col), original) {
            (Some(CellEdit::Text(t)), _) => t.clone(),
            (Some(CellEdit::Null), _) | (None, None | Some(Value::Null)) => String::new(),
            (None, Some(Value::Blob { .. })) => {
                cx.emit(GridEvent::Notify("Binary values can't be edited here — use an UPDATE in the console.".into()));
                return;
            }
            (None, Some(v)) => v.to_text(),
        };
        let typed = initial.is_some();
        let input = cx.new(|cx| {
            let mut input = TextInput::new("", cx);
            input.set_text(initial.unwrap_or(current), cx);
            if !typed {
                input.select_all_text(cx);
            }
            input
        });
        let mut subs = vec![cx.subscribe_in(&input, window, |this, _, ev: &TextInputEvent, window, cx| match ev {
            TextInputEvent::Submit => this.commit_edit(window, cx),
            TextInputEvent::Cancel => this.cancel_edit(window, cx),
            _ => {}
        })];
        // Clicking elsewhere keeps what was typed.
        subs.push(cx.on_blur(&input.focus_handle(cx), window, |this, window, cx| this.commit_edit(window, cx)));
        window.focus(&input.focus_handle(cx));
        self.selected = Some((ix, col));
        self.editor = Some(CellEditor { row, col, input, _subs: subs });
        cx.notify();
    }

    fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.take() else { return };
        let text = editor.input.read(cx).text().to_string();
        self.apply(editor.row, editor.col, CellEdit::Text(text), cx);
        window.focus(&self.focus_handle);
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.take().is_some() {
            window.focus(&self.focus_handle);
            cx.notify();
        }
    }

    /// Record `edit` (or forget it when the cell is back to what was loaded).
    fn apply(&mut self, row: RowRef, col: usize, edit: CellEdit, cx: &mut Context<Self>) {
        // A longer value widens its column (within limits) instead of being cut.
        if let (CellEdit::Text(t), Some(width)) = (&edit, self.widths.get_mut(col)) {
            let needed = ((t.chars().count() + 1) as f32 * CHAR_W + 2. * CELL_PAD).min(MAX_W);
            *width = width.max(needed);
        }
        match row {
            RowRef::Loaded(r) => {
                let original = &self.rows[r][col];
                let unchanged = match (&edit, original) {
                    (CellEdit::Null, Value::Null) => true,
                    (CellEdit::Text(t), v) => !v.is_null() && *t == v.to_text(),
                    _ => false,
                };
                if unchanged {
                    self.edits.cells.remove(&(r, col));
                } else {
                    self.edits.cells.insert((r, col), edit);
                }
            }
            RowRef::New(i) => {
                if let Some(cells) = self.edits.added.get_mut(i) {
                    cells.insert(col, edit);
                }
            }
        }
        cx.emit(GridEvent::EditsChanged);
        cx.notify();
    }

    fn set_null(&mut self, ix: usize, col: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.row_ref(ix).filter(|_| self.editable) {
            self.apply(row, col, CellEdit::Null, cx);
        }
    }

    /// Add an empty row at the end (its cells get their defaults unless filled in).
    pub fn add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable || self.columns.is_empty() {
            return;
        }
        self.edits.added.push(BTreeMap::new());
        let ix = self.display_count() - 1;
        self.select(ix, 0, cx);
        window.focus(&self.focus_handle);
        cx.emit(GridEvent::EditsChanged);
    }

    /// Mark the selected row for deletion (again: keep it); a new row just goes away.
    pub fn delete_selected_row(&mut self, cx: &mut Context<Self>) {
        let Some((ix, col)) = self.selected.filter(|_| self.editable) else { return };
        match self.row_ref(ix) {
            Some(RowRef::Loaded(r)) => {
                if !self.edits.deleted.remove(&r) {
                    self.edits.deleted.insert(r);
                }
            }
            Some(RowRef::New(i)) => {
                self.edits.added.remove(i);
                self.selected = (self.display_count() > 0).then(|| (ix.min(self.display_count() - 1), col));
            }
            None => return,
        }
        cx.emit(GridEvent::EditsChanged);
        cx.notify();
    }

    /// Whether the selected row is marked for deletion (the toolbar says "Restore Row").
    pub fn selected_row_deleted(&self) -> bool {
        matches!(self.selected.and_then(|(ix, _)| self.row_ref(ix)), Some(RowRef::Loaded(r)) if self.edits.deleted.contains(&r))
    }

    pub fn has_selection(&self) -> bool {
        self.selected.is_some()
    }

    // ------------------------------------------------------------------ export

    /// Save the loaded rows (in their displayed order) as `format`; asks where.
    pub fn export(&mut self, format: ExportFormat, kind: DbKind, table_sql: String, file_stem: String, dir: PathBuf, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_new_path(&dir, Some(&format!("{file_stem}.{}", format.extension())));
        let (rows, order, columns) = (self.rows.clone(), self.order.clone(), self.column_names());
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let target = path.clone();
            let written = cx
                .background_spawn(async move {
                    let text = graphyne_db::export(format, &kind, &table_sql, &columns, order.iter().map(|&i| &rows[i]));
                    std::fs::write(&target, text).map(|()| order.len())
                })
                .await;
            let file = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string());
            let message = match written {
                Ok(n) => format!("Exported {} row{} to {file}", group_digits(n as u64), if n == 1 { "" } else { "s" }),
                Err(e) => format!("Could not export to {file}: {e}"),
            };
            this.update(cx, |_, cx| cx.emit(GridEvent::Notify(message))).ok();
        })
        .detach();
    }

    // ------------------------------------------------------------------ keys and menu

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = &self.editor {
            if editor.input.focus_handle(cx).is_focused(window) {
                return; // the cell's input has the keys
            }
            // A field that lost the focus without noticing: keep what it holds.
            self.commit_edit(window, cx);
        }
        let m = &ev.keystroke.modifiers;
        let key = ev.keystroke.key.as_str();
        if m.control && key == "c" {
            let text = if m.shift { self.selected.and_then(|(ix, _)| self.row_text(ix)) } else { self.selected_value().map(|(_, v)| v.export_text()) };
            if let Some(text) = text {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            cx.stop_propagation();
            return;
        }
        if key == "escape" && self.menu.is_some() {
            self.menu = None;
            cx.notify();
            cx.stop_propagation();
            return;
        }
        // Enter / F2 edit the cell. (Typing a character starts editing too: it arrives as
        // text input, see `EntityInputHandler` below.)
        if let Some((ix, col)) = self.selected.filter(|_| self.editable) {
            if matches!(key, "enter" | "f2") {
                self.start_edit(ix, col, None, window, cx);
                cx.stop_propagation();
                return;
            }
        }
        let Some((row, col)) = self.selected.or((self.display_count() > 0).then_some((0, 0))) else { return };
        let last_row = self.display_count().saturating_sub(1);
        let last_col = self.columns.len().saturating_sub(1);
        let target = match key {
            "up" => (row.saturating_sub(1), col),
            "down" => ((row + 1).min(last_row), col),
            "left" => (row, col.saturating_sub(1)),
            "right" | "tab" => (row, (col + 1).min(last_col)),
            "home" if m.control => (0, col),
            "end" if m.control => (last_row, col),
            "home" => (row, 0),
            "end" => (row, last_col),
            "pageup" => (row.saturating_sub(20), col),
            "pagedown" => ((row + 20).min(last_row), col),
            _ => return,
        };
        self.select(target.0, target.1, cx);
        cx.stop_propagation();
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let GridMenu { position, ix, col } = *self.menu.as_ref()?;
        let item = |id: &'static str, label: &'static str, shortcut: Option<&'static str>| {
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
                .child(div().flex_1().text_color(color(c::TEXT)).child(label))
                .when_some(shortcut, |d, s| d.child(div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child(s)))
        };
        let separator = || div().my(px(4.)).h(px(1.)).bg(color(c::BORDER));
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.menu = None;
            cx.notify();
        };
        let deleted = matches!(self.row_ref(ix), Some(RowRef::Loaded(r)) if self.edits.deleted.contains(&r));
        let menu = div()
            .id("grid-menu")
            .occlude()
            .w(px(220.))
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
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| close(this, cx)))
            .when(self.editable && !deleted, |m| {
                m.child(item("grid-edit", "Edit Value", Some("Enter")).on_click(cx.listener(move |this, _, window, cx| this.start_edit(ix, col, None, window, cx))))
                    .child(item("grid-null", "Set NULL", None).on_click(cx.listener(move |this, _, _, cx| {
                        close(this, cx);
                        this.set_null(ix, col, cx);
                    })))
                    .child(separator())
            })
            .child(item("grid-copy", "Copy Value", Some("Ctrl+C")).on_click(cx.listener(move |this, _, _, cx| {
                close(this, cx);
                if let Some((_, v)) = this.selected_value() {
                    cx.write_to_clipboard(ClipboardItem::new_string(v.export_text()));
                }
            })))
            .child(item("grid-copy-row", "Copy Row", Some("Ctrl+Shift+C")).on_click(cx.listener(move |this, _, _, cx| {
                close(this, cx);
                if let Some(text) = this.row_text(ix) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            })))
            .when(self.editable, |m| {
                m.child(separator())
                    .child(item("grid-add-row", "Add Row", None).on_click(cx.listener(move |this, _, window, cx| {
                        close(this, cx);
                        this.add_row(window, cx);
                    })))
                    .child(item("grid-delete-row", if deleted { "Restore Row" } else { "Delete Row" }, None).on_click(cx.listener(move |this, _, _, cx| {
                        close(this, cx);
                        this.delete_selected_row(cx);
                    })))
            });
        Some(deferred(anchored().position(position).snap_to_window_with_margin(px(8.)).child(menu)).with_priority(2).into_any_element())
    }

    /// Text typed while a cell is selected (any keyboard layout, IME too): edit the cell,
    /// starting with that text.
    fn type_into_cell(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if text.is_empty() || text.chars().all(char::is_control) {
            return;
        }
        // Fast typing: the field opened by the first key isn't drawn yet, so the next keys
        // still come here. Add them to it.
        if let Some(editor) = &self.editor {
            let input = editor.input.clone();
            input.update(cx, |i, cx| {
                let text = format!("{}{text}", i.text());
                i.set_text(text, cx);
            });
            return;
        }
        if let Some((ix, col)) = self.selected.filter(|_| self.editable) {
            self.start_edit(ix, col, Some(text.to_string()), window, cx);
        }
    }

    /// Columns `[first, last)` that intersect the visible width, and the x of `first`.
    fn visible_columns(&self) -> (usize, usize, f32) {
        let viewport = match self.viewport_w() {
            w if w > 0. => w,
            _ => 4000.,
        };
        let (mut first, mut x) = (0, 0.);
        while first < self.widths.len() && x + self.widths[first] < self.scroll_x {
            x += self.widths[first];
            first += 1;
        }
        let (mut last, mut end) = (first, x);
        while last < self.widths.len() && end < self.scroll_x + viewport {
            end += self.widths[last];
            last += 1;
        }
        (first, last, x)
    }
}

/// Wide enough for the name and for the values (a sample spread over all rows, plus the
/// first and last ones), within limits.
fn column_widths(result: &ResultSet) -> Vec<f32> {
    let n = result.rows.len();
    let step = (n / SAMPLE_ROWS).max(1);
    let sample: Vec<&Vec<Value>> = result.rows.iter().step_by(step).chain(result.rows.iter().take(50)).chain(result.rows.iter().skip(n.saturating_sub(50))).collect();
    (0..result.columns.len())
        .map(|col| {
            let name = result.columns[col].chars().count() + 2;
            let values = sample.iter().map(|r| r[col].display(60).chars().count()).max().unwrap_or(0);
            // One character of slack: the font's advance is not exactly CHAR_W.
            ((name.max(values) + 1) as f32 * CHAR_W + 2. * CELL_PAD).clamp(MIN_W, MAX_W)
        })
        .collect()
}

/// The grid takes text input while it has the focus, so typing into a selected cell starts
/// editing it.
impl EntityInputHandler for DataGrid {
    fn text_for_range(&mut self, _: std::ops::Range<usize>, _: &mut Option<std::ops::Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        None
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: 0..0, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<std::ops::Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    fn replace_text_in_range(&mut self, _: Option<std::ops::Range<usize>>, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.type_into_cell(text, window, cx);
    }

    fn replace_and_mark_text_in_range(&mut self, _: Option<std::ops::Range<usize>>, text: &str, _: Option<std::ops::Range<usize>>, window: &mut Window, cx: &mut Context<Self>) {
        self.type_into_cell(text, window, cx);
    }

    fn bounds_for_range(&mut self, _: std::ops::Range<usize>, bounds: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        Some(bounds)
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
    }
}

impl Render for DataGrid {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (first, last, first_x) = self.visible_columns();
        let scroll_x = self.scroll_x;
        let num_w = self.num_w;
        let widths = self.widths.clone();
        let sort = self.sort;
        let header_cells = (first..last).map(|ci| {
            let name = self.columns[ci].clone();
            let w = widths[ci];
            let arrow = sort.filter(|(c, _)| *c == ci).map(|(_, asc)| icon(if asc { Icon::ChevronUp } else { Icon::ChevronDown }, 12., color(c::ACCENT)));
            div()
                .id(("grid-header", ci))
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(4.))
                .w(px(w))
                .h_full()
                .px(px(CELL_PAD))
                .border_r_1()
                .border_color(color(c::BORDER))
                .cursor_pointer()
                .hover(|s| s.bg(color(c::HOVER)))
                .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                    if plain_click(ev) {
                        this.toggle_sort(ci, cx);
                    }
                }))
                .child(div().flex_1().min_w(px(0.)).overflow_hidden().whitespace_nowrap().font_family(theme::fonts().ui).text_size(px(theme::UI_SMALL)).font_weight(gpui::FontWeight::MEDIUM).child(name))
                .children(arrow)
                // The right edge resizes the column.
                .child(
                    div()
                        .id(("grid-resize", ci))
                        .absolute()
                        .top_0()
                        .right(px(-3.))
                        .w(px(6.))
                        .h_full()
                        .cursor(CursorStyle::ResizeLeftRight)
                        .on_drag(ResizeColumn(ci), |_, _, _, cx| cx.new(|_| NoPreview)),
                )
        });
        let header = div()
            .flex()
            .flex_none()
            .h(px(HEADER_H))
            .bg(color(c::CANVAS))
            .border_b_1()
            .border_color(color(c::BORDER))
            .text_color(color(c::TEXT))
            .child(div().flex_none().w(px(num_w)).h_full().border_r_1().border_color(color(c::BORDER)))
            .child(div().flex_1().min_w(px(0.)).h_full().overflow_hidden().child(div().flex().h_full().relative().left(px(first_x - scroll_x)).children(header_cells)));

        let selected = self.selected;
        let body = uniform_list("grid-rows", self.display_count(), cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
            range
                .map(|ix| {
                    let row = this.row_ref(ix).unwrap_or(RowRef::New(0));
                    let row_selected = selected.is_some_and(|(r, _)| r == ix);
                    let deleted = matches!(row, RowRef::Loaded(r) if this.edits.deleted.contains(&r));
                    let is_new = matches!(row, RowRef::New(_));
                    let cells = (first..last).map(|ci| {
                        let w = widths[ci];
                        let max_chars = (((w - 2. * CELL_PAD) / CHAR_W).floor() as usize).max(1);
                        let is_selected = selected == Some((ix, ci));
                        let edit = this.edit_at(row, ci).cloned();
                        let editing = this.editor.as_ref().filter(|e| e.row == row && e.col == ci).map(|e| e.input.clone());
                        let value = match row {
                            RowRef::Loaded(r) => this.rows[r][ci].clone(),
                            RowRef::New(_) => Value::Null,
                        };
                        // What the cell shows: the pending edit, the loaded value, or <default>.
                        let (text, tint, number) = match (&edit, row) {
                            (Some(CellEdit::Text(t)), _) => (Value::Text(t.clone()).display(max_chars), color(c::TEXT), false),
                            (Some(CellEdit::Null), _) => ("<null>".to_string(), color(c::TEXT_FAINT), false),
                            (None, RowRef::New(_)) => ("<default>".to_string(), color(c::TEXT_FAINT), false),
                            (None, RowRef::Loaded(_)) => match &value {
                                Value::Null => ("<null>".to_string(), color(c::TEXT_FAINT), false),
                                Value::Blob { .. } => (value.display(max_chars), color(c::TEXT_MUTED), false),
                                Value::Int(_) | Value::Real(_) | Value::Num(_) => (value.display(max_chars), color(c::BLUE), true),
                                Value::Text(_) => (value.display(max_chars), color(c::TEXT), false),
                            },
                        };
                        let cell = div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .when(number, |d| d.justify_end())
                            .w(px(w))
                            .h_full()
                            .px(px(CELL_PAD))
                            .border_r_1()
                            .border_color(color_a(c::BORDER, 0.6))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_color(tint)
                            .when(edit.is_some(), |d| d.bg(color_a(c::YELLOW, 0.16)))
                            .when(deleted, |d| d.line_through().text_color(color(c::TEXT_DIM)))
                            .when(is_selected, |d| d.bg(color(c::SELECTION)).border_1().border_color(color(c::ACCENT)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                                    if this.editor.as_ref().is_some_and(|e| e.row == row && e.col == ci) {
                                        return; // a click inside the field being edited
                                    }
                                    window.focus(&this.focus_handle);
                                    this.menu = None;
                                    this.select(ix, ci, cx);
                                    if ev.click_count >= 2 {
                                        this.start_edit(ix, ci, None, window, cx);
                                    }
                                    // The grid's own focus-on-click would take the focus back
                                    // from the field just opened.
                                    window.prevent_default();
                                }),
                            )
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                                    window.focus(&this.focus_handle);
                                    this.select(ix, ci, cx);
                                    this.menu = Some(GridMenu { position: ev.position, ix, col: ci });
                                    cx.stop_propagation();
                                }),
                            );
                        match editing {
                            Some(input) => cell.bg(color(c::ISLAND)).border_1().border_color(color(c::TAB_FOCUSED_BORDER)).px(px(4.)).child(input).into_any_element(),
                            None => cell.child(text).into_any_element(),
                        }
                    });
                    let (marker, marker_tint) = if is_new {
                        ("+".to_string(), color(c::GREEN))
                    } else if deleted {
                        ("−".to_string(), color(c::RED))
                    } else {
                        ((ix + 1).to_string(), color(c::TEXT_FAINT))
                    };
                    div()
                        .id(ix)
                        .flex()
                        .h(px(ROW_H))
                        .w_full()
                        .border_b_1()
                        .border_color(color_a(c::BORDER, 0.5))
                        .when(is_new, |d| d.bg(color_a(c::GREEN, 0.10)))
                        .when(deleted, |d| d.bg(color_a(c::RED, 0.10)))
                        .when(row_selected && !is_new && !deleted, |d| d.bg(color(c::ACTIVE_LINE)))
                        .when(!row_selected, |d| d.hover(|s| s.bg(color_a(c::HOVER, 0.6))))
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_end()
                                .w(px(num_w))
                                .h_full()
                                .pr(px(8.))
                                .border_r_1()
                                .border_color(color(c::BORDER))
                                .text_color(marker_tint)
                                .child(marker),
                        )
                        .child(div().flex_1().min_w(px(0.)).h_full().overflow_hidden().child(div().flex().h_full().relative().left(px(first_x - scroll_x)).children(cells)))
                })
                .collect()
        }))
        .track_scroll(self.scroll.clone())
        .flex_1()
        .min_h(px(0.));

        // The selected value in full (one line), like DataGrip's value viewer in short.
        let hint = if self.editable { "Enter edits · Ctrl+C copies" } else { "Ctrl+C copies" };
        let footer = self.selected_value().map(|(name, value)| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .h(px(24.))
                .px(px(10.))
                .border_t_1()
                .border_color(color(c::BORDER))
                .overflow_hidden()
                .whitespace_nowrap()
                .child(div().flex_none().font_family(theme::fonts().ui).text_size(px(theme::UI_SMALL)).text_color(color(c::TEXT_DIM)).child(name))
                .child(div().min_w(px(0.)).overflow_hidden().text_color(if value.is_null() { color(c::TEXT_FAINT) } else { color(c::TEXT) }).child(value.display(400)))
                .child(div().flex_1())
                .child(div().flex_none().font_family(theme::fonts().ui).text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child(hint))
        });
        let menu = self.render_menu(cx);
        // Registers the grid for text input while it is focused (see `EntityInputHandler`).
        let (focus, entity) = (self.focus_handle.clone(), cx.entity());
        let input_target = canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                if focus.is_focused(window) {
                    window.handle_input(&focus, ElementInputHandler::new(bounds, entity.clone()), cx);
                }
            },
        )
        .absolute()
        .size_full();

        div()
            .id("data-grid")
            .key_context("DataGrid")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_drag_move(cx.listener(|this, ev: &DragMoveEvent<ResizeColumn>, _, cx| {
                let col = ev.drag(cx).0;
                let left = f32::from(ev.bounds.left()) + this.num_w + this.col_left(col) - this.scroll_x;
                let width = (f32::from(ev.event.position.x) - left).clamp(40., 1200.);
                if (width - this.widths[col]).abs() >= 0.5 {
                    this.widths[col] = width;
                    this.scroll_x = this.scroll_x.min(this.max_scroll_x());
                    cx.notify();
                }
            }))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .font_family(theme::fonts().code)
            .text_size(px(CELL_FONT))
            .child(input_target)
            .child(header)
            .child(body)
            .children(footer)
            .children(menu)
    }
}
