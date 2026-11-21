//! A virtualized table of query results: a header that stays put, row numbers, sorting by a
//! column, resizable columns, horizontal scrolling, cell selection with the keyboard, copy.
//! Only visible rows — and only visible columns — are built each frame, so hundreds of
//! thousands of rows scroll as smoothly as ten.

use crate::assets::Icon;
use crate::components::icon;
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{
    App, ClickEvent, ClipboardItem, Context, CursorStyle, DragMoveEvent, FocusHandle, Focusable, KeyDownEvent, MouseButton, MouseDownEvent, ScrollStrategy,
    ScrollWheelEvent, SharedString, UniformListScrollHandle, Window, div, px, uniform_list,
};
use graphyne_db::{ResultSet, Value};
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
        }
    }

    /// Show `result` (columns sized to their content, back at the top-left).
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
        if self.order.is_empty() || self.columns.is_empty() {
            return;
        }
        let (row, col) = (row.min(self.order.len() - 1), col.min(self.columns.len() - 1));
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

    /// The selected cell: its column name and value.
    pub fn selected_value(&self) -> Option<(SharedString, Value)> {
        let (row, col) = self.selected?;
        let value = self.rows.get(*self.order.get(row)?)?.get(col)?.clone();
        Some((self.columns[col].clone(), value))
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        let key = ev.keystroke.key.as_str();
        if m.control && key == "c" {
            if let Some((_, value)) = self.selected_value() {
                cx.write_to_clipboard(ClipboardItem::new_string(value.to_text()));
            }
            cx.stop_propagation();
            return;
        }
        let Some((row, col)) = self.selected.or((!self.order.is_empty()).then_some((0, 0))) else { return };
        let last_row = self.order.len().saturating_sub(1);
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

        let rows = self.rows.clone();
        let order = self.order.clone();
        let selected = self.selected;
        let body = uniform_list("grid-rows", order.len(), cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
            range
                .map(|ix| {
                    let row = &rows[order[ix]];
                    let row_selected = selected.is_some_and(|(r, _)| r == ix);
                    let cells = (first..last).map(|ci| {
                        let w = widths[ci];
                        let value = &row[ci];
                        let max_chars = (((w - 2. * CELL_PAD) / CHAR_W).floor() as usize).max(1);
                        let is_selected = selected == Some((ix, ci));
                        let (text, tint) = match value {
                            Value::Null => ("<null>".to_string(), color(c::TEXT_FAINT)),
                            Value::Blob { .. } => (value.display(max_chars), color(c::TEXT_MUTED)),
                            Value::Int(_) | Value::Real(_) | Value::Num(_) => (value.display(max_chars), color(c::BLUE)),
                            Value::Text(_) => (value.display(max_chars), color(c::TEXT)),
                        };
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .when(value.is_number(), |d| d.justify_end())
                            .w(px(w))
                            .h_full()
                            .px(px(CELL_PAD))
                            .border_r_1()
                            .border_color(color_a(c::BORDER, 0.6))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_color(tint)
                            .when(is_selected, |d| d.bg(color(c::SELECTION)).border_1().border_color(color(c::ACCENT)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                    window.focus(&this.focus_handle);
                                    this.select(ix, ci, cx);
                                }),
                            )
                            .child(text)
                    });
                    div()
                        .id(ix)
                        .flex()
                        .h(px(ROW_H))
                        .w_full()
                        .border_b_1()
                        .border_color(color_a(c::BORDER, 0.5))
                        .when(row_selected, |d| d.bg(color(c::ACTIVE_LINE)))
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
                                .text_color(color(c::TEXT_FAINT))
                                .child((ix + 1).to_string()),
                        )
                        .child(div().flex_1().min_w(px(0.)).h_full().overflow_hidden().child(div().flex().h_full().relative().left(px(first_x - scroll_x)).children(cells)))
                })
                .collect()
        }))
        .track_scroll(self.scroll.clone())
        .flex_1()
        .min_h(px(0.));

        // The selected value in full (one line), like DataGrip's value viewer in short.
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
                .child(div().flex_none().font_family(theme::fonts().ui).text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child("Ctrl+C copies"))
        });

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
            .flex()
            .flex_col()
            .font_family(theme::fonts().code)
            .text_size(px(CELL_FONT))
            .child(header)
            .child(body)
            .children(footer)
    }
}
