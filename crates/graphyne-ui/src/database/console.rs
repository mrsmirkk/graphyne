//! A SQL console tab: an editor for the connection's queries (kept between sessions) above
//! the results. Ctrl+Enter runs the statement under the cursor, or the selection.

use super::grid::{DataGrid, GridEvent, NoPreview};
use super::{DbStore, RunStatement, StopQuery, format_elapsed, group_digits};
use crate::assets::Icon;
use crate::components::{icon, kbd, tooltip};
use crate::editor::Editor;
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{App, Context, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, MouseButton, MouseDownEvent, Pixels, Point, SharedString, Window, div, px, relative};
use graphyne_db::ExportFormat;
use graphyne_db::{DbSession, QueryOutcome, ROW_LIMIT, StatementResult};
use std::sync::Arc;

const TOOLBAR_H: f32 = 36.;

#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Info,
    Ok,
    Error,
}

/// Dragging the bar between the editor and the results.
#[derive(Clone)]
struct ResizeResults;

pub enum ConsoleEvent {
    /// A message for the notifications (exports).
    Notify(String),
}

pub struct DbConsole {
    pub db_id: String,
    store: Entity<DbStore>,
    pub editor: Entity<Editor>,
    grid: Entity<DataGrid>,
    /// The session running a statement now (Stop interrupts it).
    running: Option<Arc<DbSession>>,
    busy: bool,
    status: Option<(SharedString, Tone)>,
    /// The statement that failed, shown under the error.
    failed_sql: Option<SharedString>,
    /// Share of the height given to the results.
    results_ratio: f32,
    _store_sub: gpui::Subscription,
    _grid_sub: gpui::Subscription,
    export_menu: Option<Point<Pixels>>,
}

impl EventEmitter<ConsoleEvent> for DbConsole {}

impl Focusable for DbConsole {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

/// Statements that change the schema (the Database panel reloads after them).
fn changes_schema(sql: &str) -> bool {
    let first = sql.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    matches!(first.as_str(), "create" | "drop" | "alter")
}

impl DbConsole {
    pub fn new(db_id: String, store: Entity<DbStore>, editor: Entity<Editor>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&store, |_, _, cx| cx.notify());
        let grid = cx.new(DataGrid::new);
        let grid_sub = cx.subscribe(&grid, |_, _, ev: &GridEvent, cx| match ev {
            GridEvent::Notify(m) => cx.emit(ConsoleEvent::Notify(m.clone())),
            GridEvent::EditsChanged => {}
        });
        Self {
            db_id,
            store,
            editor,
            grid,
            running: None,
            busy: false,
            status: None,
            failed_sql: None,
            results_ratio: 0.5,
            _store_sub: sub,
            _grid_sub: grid_sub,
            export_menu: None,
        }
    }

    pub fn title(&self, cx: &App) -> String {
        self.store.read(cx).name(&self.db_id).to_string()
    }

    /// The SQL to run: the selection, else the statement around the cursor.
    fn sql_to_run(&self, cx: &App) -> Option<String> {
        let editor = self.editor.read(cx);
        let selection = editor.selection();
        let text = editor.buffer.rope().to_string();
        let sql = if selection.is_empty() {
            let offset = editor.buffer.char_to_byte(selection.start);
            graphyne_db::split::statement_at(&text, offset).map(|r| text[r].to_string())
        } else {
            Some(editor.buffer.rope().slice(selection).to_string())
        };
        sql.filter(|s| !s.trim().is_empty())
    }

    /// Run (▷ in the title bar, Shift+F10) while this console is the active tab.
    pub fn run_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run(&RunStatement, window, cx);
    }

    fn run(&mut self, _: &RunStatement, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(sql) = self.sql_to_run(cx) else {
            self.status = Some(("Nothing to run: put the cursor on a statement or select some SQL".into(), Tone::Info));
            cx.notify();
            return;
        };
        self.busy = true;
        self.failed_sql = None;
        self.status = Some(("Running…".into(), Tone::Info));
        let waiting = self.store.update(cx, |s, cx| s.session(&self.db_id, cx));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let session = match waiting.recv_async().await {
                Ok(Ok(session)) => session,
                Ok(Err(e)) => {
                    this.update(cx, |this, cx| this.finish_with_error(format!("Could not connect: {e}"), None, cx)).ok();
                    return;
                }
                Err(_) => return,
            };
            this.update(cx, |this, cx| {
                this.running = Some(session.clone());
                cx.notify();
            })
            .ok();
            let outcomes = session.run_script(sql, ROW_LIMIT).recv_async().await.unwrap_or_default();
            this.update(cx, |this, cx| this.show(outcomes, cx)).ok();
        })
        .detach();
    }

    fn stop(&mut self, _: &StopQuery, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = &self.running {
            session.interrupt();
            self.status = Some(("Stopping…".into(), Tone::Info));
            cx.notify();
        }
    }

    fn finish_with_error(&mut self, error: String, sql: Option<String>, cx: &mut Context<Self>) {
        self.busy = false;
        self.running = None;
        self.status = Some((error.into(), Tone::Error));
        self.failed_sql = sql.map(SharedString::from);
        self.grid.update(cx, |g, cx| g.clear(cx));
        cx.notify();
    }

    fn show(&mut self, outcomes: Vec<QueryOutcome>, cx: &mut Context<Self>) {
        self.busy = false;
        self.running = None;
        if outcomes.iter().any(|o| o.result.is_ok() && changes_schema(&o.sql)) {
            let id = self.db_id.clone();
            self.store.update(cx, |s, cx| s.load_schema(&id, cx));
        }
        let count = outcomes.len();
        let elapsed = format_elapsed(outcomes.iter().map(|o| o.elapsed).sum());
        let statements = if count > 1 { format!("{count} statements · ") } else { String::new() };
        let mut affected = 0u64;
        let mut rows = None;
        let mut error = None;
        for outcome in outcomes {
            match outcome.result {
                Ok(StatementResult::Rows(r)) => rows = Some(r),
                Ok(StatementResult::Done { affected: n }) => affected += n,
                Err(e) => error = Some((e, outcome.sql)),
            }
        }
        if let Some((e, sql)) = error {
            // Stopped on purpose: not an error.
            if e == "interrupted" {
                self.status = Some(("Stopped".into(), Tone::Info));
                self.grid.update(cx, |g, cx| g.clear(cx));
                cx.notify();
                return;
            }
            let done = if count > 1 { format!(" (statement {count} of the script; the ones before it ran)") } else { String::new() };
            return self.finish_with_error(format!("{e}{done}"), Some(sql), cx);
        }
        let status = match rows {
            Some(r) => {
                let n = r.rows.len() as u64;
                let line = if r.truncated {
                    format!("{statements}the first {} rows (there are more) · {elapsed}", group_digits(n))
                } else {
                    format!("{statements}{} row{} · {elapsed}", group_digits(n), if n == 1 { "" } else { "s" })
                };
                self.grid.update(cx, |g, cx| g.set_result(r, cx));
                line
            }
            None => {
                self.grid.update(cx, |g, cx| g.clear(cx));
                if count == 0 { "Nothing to run".to_string() } else { format!("{statements}Done · {} row{} affected · {elapsed}", group_digits(affected), if affected == 1 { "" } else { "s" }) }
            }
        };
        self.status = Some((status.into(), Tone::Ok));
        cx.notify();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let busy = self.busy;
        let name = self.store.read(cx).name(&self.db_id);
        let button = |id: &'static str, ic: Icon, tint: gpui::Hsla, label: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(26.))
                .px(px(8.))
                .rounded(px(6.))
                .text_size(px(theme::UI_SMALL))
                .child(icon(ic, 14., tint))
                .child(label)
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .h(px(TOOLBAR_H))
            .px(px(8.))
            .border_b_1()
            .border_color(color(c::BORDER))
            .font_family(theme::fonts().ui)
            .text_color(color(c::TEXT))
            // Running is the title bar's ▷ (and Ctrl+Enter); the console only adds Stop.
            .child(
                button("console-stop", Icon::Stop, if busy { color(c::RED) } else { color(c::TEXT_FAINT) }, "Stop")
                    .when(busy, |b| b.cursor_pointer().hover(|s| s.bg(color(c::HOVER))))
                    .when(!busy, |b| b.text_color(color(c::TEXT_FAINT)))
                    .tooltip(tooltip("Stop the running query", None))
                    .on_click(cx.listener(|this, _, window, cx| this.stop(&StopQuery, window, cx))),
            )
            .child(div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(color(c::BORDER)))
            .child(icon(Icon::Database, 14., color(c::TEXT_MUTED)))
            .child(div().text_size(px(theme::UI_SMALL)).text_color(color(c::TEXT_MUTED)).child(name))
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_size(px(11.))
                    .text_color(color(c::TEXT_FAINT))
                    .child(icon(Icon::Play, 11., color(c::TEXT_DIM)))
                    .child("Run or")
                    .child(kbd("Ctrl"))
                    .child(kbd("Enter"))
                    .child("runs the statement under the cursor"),
            )
    }
}

impl Render for DbConsole {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_rows = self.grid.read(cx).has_columns();
        let menu = self.export_menu.map(|position| {
            super::table::export_menu(position, cx, |this, format, cx| this.export(format, cx), |this, cx| {
                this.export_menu = None;
                cx.notify();
            })
        });
        let (status, tone) = self.status.clone().map(|(s, t)| (Some(s), t)).unwrap_or((None, Tone::Info));
        let status_color = match tone {
            Tone::Info => color(c::TEXT_MUTED),
            Tone::Ok => color(c::GREEN),
            Tone::Error => color(c::RED),
        };
        let results_header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(28.))
            .px(px(10.))
            .border_b_1()
            .border_color(color(c::BORDER))
            .font_family(theme::fonts().ui)
            .text_size(px(theme::UI_SMALL))
            .child(div().text_color(color(c::TEXT)).font_weight(gpui::FontWeight::MEDIUM).child("Result"))
            .when_some(status.clone().filter(|_| tone != Tone::Error), |d, s| d.child(div().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_color(status_color).child(s)))
            .child(div().flex_1())
            .when(has_rows && tone != Tone::Error, |d| {
                d.child(
                    div()
                        .id("console-export")
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .h(px(22.))
                        .px(px(8.))
                        .rounded(px(5.))
                        .cursor_pointer()
                        .hover(|s| s.bg(color(c::HOVER)))
                        .text_color(color(c::TEXT))
                        .child(icon(Icon::FolderOpen, 13., color(c::TEXT_MUTED)))
                        .child("Export")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                this.export_menu = Some(ev.position);
                                cx.notify();
                            }),
                        ),
                )
            });
        let body: gpui::AnyElement = if tone == Tone::Error {
            let failed = self.failed_sql.clone();
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .m(px(10.))
                .p(px(12.))
                .rounded(px(8.))
                .bg(color_a(c::RED, 0.08))
                .border_1()
                .border_color(color_a(c::RED, 0.35))
                .child(div().flex().items_start().gap(px(8.)).font_family(theme::fonts().ui).text_size(px(theme::UI_TEXT)).text_color(color(c::RED)).child(icon(Icon::Error, 15., color(c::RED))).child(status.unwrap_or_default()))
                .when_some(failed, |d, sql| d.child(div().font_family(theme::fonts().code).text_size(px(12.5)).text_color(color(c::TEXT_MUTED)).child(sql)))
                .into_any_element()
        } else if has_rows {
            self.grid.clone().into_any_element()
        } else {
            let hint = if self.busy { "Running…" } else if self.status.is_some() { "No rows to show" } else { "Run a query to see its result here" };
            div().flex().flex_1().items_center().justify_center().font_family(theme::fonts().ui).text_size(px(theme::UI_TEXT)).text_color(color(c::TEXT_DIM)).child(hint).into_any_element()
        };
        let ratio = self.results_ratio;
        div()
            .id("db-console")
            .key_context("DbConsole")
            .on_action(cx.listener(Self::run))
            .on_action(cx.listener(Self::stop))
            .on_drag_move(cx.listener(|this, ev: &DragMoveEvent<ResizeResults>, _, cx| {
                let top = f32::from(ev.bounds.top()) + TOOLBAR_H;
                let height = (f32::from(ev.bounds.size.height) - TOOLBAR_H).max(1.);
                let ratio = (1. - (f32::from(ev.event.position.y) - top) / height).clamp(0.12, 0.88);
                if (ratio - this.results_ratio).abs() > 0.002 {
                    this.results_ratio = ratio;
                    cx.notify();
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .child(div().h(relative(1. - ratio)).min_h(px(60.)).child(self.editor.clone()))
            // The bar between editor and results: drag to resize.
            .child(
                div()
                    .id("console-split")
                    .flex_none()
                    .h(px(5.))
                    .border_t_1()
                    .border_color(color(c::BORDER))
                    .cursor(gpui::CursorStyle::ResizeUpDown)
                    .hover(|s| s.bg(color_a(c::ACCENT, 0.35)))
                    .on_drag(ResizeResults, |_, _, _, cx| cx.new(|_| NoPreview)),
            )
            .child(div().flex().flex_col().flex_1().min_h(px(0.)).child(results_header).child(div().flex().flex_col().flex_1().min_h(px(0.)).child(body)))
            .children(menu)
    }
}

impl DbConsole {
    /// Save the result as a file (INSERTs go into a table called `result`).
    fn export(&mut self, format: ExportFormat, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        let Some(kind) = store.entry(&self.db_id).map(|e| e.config.kind.clone()) else { return };
        let dir = store.root().to_path_buf();
        let table = kind.quote_ident("result");
        self.grid.update(cx, |g, cx| g.export(format, kind, table, "result".into(), dir, cx));
    }
}
