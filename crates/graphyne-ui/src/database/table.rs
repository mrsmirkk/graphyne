//! A table (or view) tab: its rows in the data grid, with the total count. Tables with a
//! primary key can be edited: changed cells, new and deleted rows are saved together by
//! Submit, in one transaction. Rows can be exported to CSV, JSON or SQL.

use super::grid::{DataGrid, GridEvent};
use super::{DbStore, format_elapsed, group_digits};
use crate::assets::Icon;
use crate::components::{icon, tooltip};
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{
    App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseButton, MouseDownEvent, Pixels, Point, SharedString, Subscription, Window, anchored, deferred, div, px,
};
use graphyne_db::{CellEdit, ExportFormat, ROW_LIMIT, Value};
use std::collections::BTreeMap;
use std::time::Instant;

pub enum DbTableEvent {
    /// Open the connection's console with this query added.
    OpenConsole(String, String),
    /// A message for the notifications.
    Notify(String),
}

pub struct DbTableView {
    pub db_id: String,
    /// PostgreSQL schema / MySQL database, when not the default one.
    pub schema: Option<String>,
    pub table: String,
    store: Entity<DbStore>,
    grid: Entity<DataGrid>,
    loading: bool,
    saving: bool,
    status: Option<SharedString>,
    error: Option<SharedString>,
    /// Why Submit failed, and the statement that failed.
    save_error: Option<(SharedString, Option<SharedString>)>,
    export_menu: Option<Point<Pixels>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<DbTableEvent> for DbTableView {}

impl Focusable for DbTableView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.focus_handle(cx)
    }
}

/// Export formats as a small menu at `position`; `pick` gets the chosen one.
pub(super) fn export_menu<V: 'static>(position: Point<Pixels>, cx: &mut Context<V>, pick: fn(&mut V, ExportFormat, &mut Context<V>), close: fn(&mut V, &mut Context<V>)) -> gpui::AnyElement {
    let menu = div()
        .id("export-menu")
        .occlude()
        .w(px(230.))
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
        .child(div().px(px(10.)).pt(px(4.)).pb(px(2.)).text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child("EXPORT LOADED ROWS"))
        .children(ExportFormat::ALL.map(|format| {
            div()
                .id(SharedString::from(format!("export-{}", format.extension())))
                .flex()
                .items_center()
                .gap(px(10.))
                .h(px(28.))
                .px(px(10.))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|s| s.bg(color(c::HOVER)))
                .child(div().flex_1().text_color(color(c::TEXT)).child(format.label()))
                .child(div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child(format!(".{}", format.extension())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    close(this, cx);
                    pick(this, format, cx);
                }))
        }));
    deferred(anchored().position(position).snap_to_window_with_margin(px(8.)).child(menu)).with_priority(2).into_any_element()
}

impl DbTableView {
    pub fn new(db_id: String, schema: Option<String>, table: String, store: Entity<DbStore>, cx: &mut Context<Self>) -> Self {
        let grid = cx.new(DataGrid::new);
        let subs = vec![
            cx.subscribe(&grid, |_, _, ev: &GridEvent, cx| match ev {
                GridEvent::EditsChanged => cx.notify(),
                GridEvent::Notify(m) => cx.emit(DbTableEvent::Notify(m.clone())),
            }),
            // The schema (primary keys) may arrive after the rows.
            cx.observe(&store, |this, _, cx| this.update_editable(cx)),
        ];
        let mut view = Self { db_id, schema, table, store, grid, loading: false, saving: false, status: None, error: None, save_error: None, export_menu: None, _subs: subs };
        let missing_schema = view.store.read(cx).entry(&view.db_id).is_some_and(|e| e.schema.is_none() && !e.loading_schema);
        if missing_schema {
            let id = view.db_id.clone();
            view.store.update(cx, |s, cx| s.load_schema(&id, cx));
        }
        view.load(cx);
        view
    }

    /// `schema.table`, or the table alone in the default schema.
    pub fn title(&self) -> String {
        match &self.schema {
            Some(s) => format!("{s}.{}", self.table),
            None => self.table.clone(),
        }
    }

    /// The primary key's columns, or why the rows can't be edited here.
    fn key_columns(&self, cx: &App) -> Result<Vec<String>, &'static str> {
        let store = self.store.read(cx);
        let entry = store.entry(&self.db_id).ok_or("the connection was removed")?;
        let schema = entry.schema.as_ref().ok_or("reading the table's structure…")?;
        let find = |list: &[graphyne_db::Table]| list.iter().any(|t| t.schema == self.schema && t.name == self.table);
        if find(&schema.views) {
            return Err("a view: its rows are read-only");
        }
        let table = schema.tables.iter().find(|t| t.schema == self.schema && t.name == self.table).ok_or("not in the schema any more: refresh the connection")?;
        let keys: Vec<String> = table.columns.iter().filter(|c| c.primary_key).map(|c| c.name.clone()).collect();
        if keys.is_empty() { Err("no primary key: change rows with UPDATE in the console") } else { Ok(keys) }
    }

    fn update_editable(&mut self, cx: &mut Context<Self>) {
        let editable = self.key_columns(cx).is_ok();
        self.grid.update(cx, |g, cx| g.set_editable(editable, cx));
        cx.notify();
    }

    pub fn load(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.error = None;
        self.save_error = None;
        let waiting = self.store.update(cx, |s, cx| s.session(&self.db_id, cx));
        let (schema, table) = (self.schema.clone(), self.table.clone());
        cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let loaded = match waiting.recv_async().await {
                Ok(Ok(session)) => session.table_rows(schema, table, ROW_LIMIT).recv_async().await.map_err(|_| "the connection was closed".to_string()),
                Ok(Err(e)) => Err(format!("Could not connect: {e}")),
                Err(_) => Err("the connection was closed".into()),
            };
            this.update(cx, |this, cx| {
                this.loading = false;
                match loaded {
                    Ok((Ok(rows), total)) => {
                        let shown = rows.rows.len() as u64;
                        let elapsed = format_elapsed(started.elapsed());
                        this.status = Some(
                            match total {
                                // Big server tables report the planner's estimate.
                                Some(total) if !total.exact => format!("the first {} of about {} rows · {elapsed}", group_digits(shown), group_digits(total.rows)),
                                Some(total) if total.rows > shown => format!("the first {} of {} rows · {elapsed}", group_digits(shown), group_digits(total.rows)),
                                _ => format!("{} row{} · {elapsed}", group_digits(shown), if shown == 1 { "" } else { "s" }),
                            }
                            .into(),
                        );
                        this.grid.update(cx, |g, cx| g.set_result(rows, cx));
                    }
                    Ok((Err(e), _)) | Err(e) => {
                        this.status = None;
                        this.error = Some(e.into());
                        this.grid.update(cx, |g, cx| g.clear(cx));
                    }
                }
                this.update_editable(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Save the pending edits: DELETEs, UPDATEs, then INSERTs, in one transaction.
    fn submit(&mut self, cx: &mut Context<Self>) {
        let Ok(keys) = self.key_columns(cx) else { return };
        let Some(kind) = self.store.read(cx).entry(&self.db_id).map(|e| e.config.kind.clone()) else { return };
        let grid = self.grid.read(cx);
        let (edits, rows, columns) = (grid.edits().clone(), grid.rows(), grid.column_names());
        if edits.is_empty() || self.saving {
            return;
        }
        let key_at: Vec<usize> = keys.iter().filter_map(|k| columns.iter().position(|c| c == k)).collect();
        if key_at.len() != keys.len() {
            self.save_error = Some(("Nothing was saved: the primary key's columns are not in the loaded rows.".into(), None));
            cx.notify();
            return;
        }
        let table = kind.quote_table(self.schema.as_deref(), &self.table);
        let key_of = |r: usize| -> Vec<(&str, &Value)> { key_at.iter().map(|&i| (columns[i].as_str(), &rows[r][i])).collect() };
        let mut statements = Vec::new();
        for &r in &edits.deleted {
            statements.push(kind.delete_sql(&table, &key_of(r)));
        }
        let mut by_row: BTreeMap<usize, Vec<(&str, &CellEdit)>> = BTreeMap::new();
        for ((r, col), edit) in &edits.cells {
            if !edits.deleted.contains(r) {
                by_row.entry(*r).or_default().push((columns[*col].as_str(), edit));
            }
        }
        for (r, set) in &by_row {
            statements.push(kind.update_sql(&table, set, &key_of(*r)));
        }
        for added in &edits.added {
            let values: Vec<(&str, &CellEdit)> = added.iter().map(|(col, edit)| (columns[*col].as_str(), edit)).collect();
            statements.push(kind.insert_sql(&table, &values));
        }
        let sql: Vec<String> = statements.iter().map(|s| s.sql.clone()).collect();
        let begin = kind.begin_sql();
        let title = self.title();
        self.saving = true;
        self.save_error = None;
        let waiting = self.store.update(cx, |s, cx| s.session(&self.db_id, cx));
        cx.spawn(async move |this, cx| {
            let result = match waiting.recv_async().await {
                Ok(Ok(session)) => session.run_transaction(begin, statements).recv_async().await.unwrap_or_else(|_| Err((0, "the connection was closed".into()))),
                Ok(Err(e)) => Err((usize::MAX, format!("could not connect: {e}"))),
                Err(_) => Err((usize::MAX, "the connection was closed".into())),
            };
            this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(changed) => {
                        cx.emit(DbTableEvent::Notify(format!("{title}: saved, {} row{} changed.", group_digits(changed), if changed == 1 { "" } else { "s" })));
                        this.load(cx);
                    }
                    Err((index, e)) => this.save_error = Some((format!("Nothing was saved: {e}").into(), sql.get(index).map(|s| s.clone().into()))),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn export(&mut self, format: ExportFormat, cx: &mut Context<Self>) {
        let Some(kind) = self.store.read(cx).entry(&self.db_id).map(|e| e.config.kind.clone()) else { return };
        let table = kind.quote_table(self.schema.as_deref(), &self.table);
        let dir = self.store.read(cx).root().to_path_buf();
        let stem = self.table.clone();
        self.grid.update(cx, |g, cx| g.export(format, kind, table, stem, dir, cx));
    }
}

impl Render for DbTableView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.store.read(cx).name(&self.db_id);
        let read_only = self.key_columns(cx).err();
        let grid = self.grid.read(cx);
        let pending = grid.edits().row_count();
        let can_delete = grid.has_selection();
        let restore = grid.selected_row_deleted();
        let has_rows = grid.has_columns();
        let tool = |id: &'static str, ic: Icon, label: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(26.))
                .px(px(8.))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|s| s.bg(color(c::HOVER)))
                .text_size(px(theme::UI_SMALL))
                .child(icon(ic, 14., color(c::TEXT_MUTED)))
                .child(label)
        };
        let separator = || div().w(px(1.)).h(px(16.)).mx(px(2.)).bg(color(c::BORDER));
        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(36.))
            .px(px(10.))
            .border_b_1()
            .border_color(color(c::BORDER))
            .font_family(theme::fonts().ui)
            .text_color(color(c::TEXT))
            .child(icon(Icon::Table, 15., color(c::ACCENT)))
            .child(div().ml(px(2.)).font_weight(gpui::FontWeight::MEDIUM).text_size(px(theme::UI_TEXT)).child(self.title()))
            .child(div().ml(px(4.)).text_size(px(theme::UI_SMALL)).text_color(color(c::TEXT_DIM)).child(name))
            .child(separator())
            .child(tool("table-refresh", Icon::Refresh, "Refresh").tooltip(tooltip("Read the rows again (pending edits are dropped)", None)).on_click(cx.listener(|this, _, _, cx| this.load(cx))))
            .child(tool("table-console", Icon::Terminal, "Query").tooltip(tooltip("Open the SQL console with a SELECT for this table", None)).on_click(cx.listener(|this, _, _, cx| {
                let quoted = this.store.read(cx).entry(&this.db_id).map(|e| e.config.kind.quote_table(this.schema.as_deref(), &this.table)).unwrap_or_else(|| this.table.clone());
                cx.emit(DbTableEvent::OpenConsole(this.db_id.clone(), format!("SELECT * FROM {quoted};")));
            })))
            .child(separator())
            .when(read_only.is_none() && has_rows, |d| {
                d.child(tool("table-add-row", Icon::Plus, "Row").tooltip(tooltip("Add a row (filled in the grid, saved by Submit)", None)).on_click(cx.listener(|this, _, window, cx| {
                    this.grid.update(cx, |g, cx| g.add_row(window, cx));
                })))
                .when(can_delete, |d| {
                    d.child(tool("table-delete-row", Icon::Trash, if restore { "Restore" } else { "Delete" }).tooltip(tooltip("Mark the selected row for deletion (saved by Submit)", None)).on_click(cx.listener(|this, _, _, cx| {
                        this.grid.update(cx, |g, cx| g.delete_selected_row(cx));
                    })))
                })
            })
            .when_some(read_only.filter(|_| has_rows), |d, why| {
                d.child(div().px(px(6.)).text_size(px(theme::UI_SMALL)).text_color(color(c::TEXT_DIM)).child(format!("read-only — {why}")))
            })
            .when(pending > 0, |d| {
                d.child(separator())
                    .child(div().px(px(6.)).text_size(px(theme::UI_SMALL)).text_color(color(c::YELLOW)).child(format!("{pending} row{} changed", if pending == 1 { "" } else { "s" })))
                    .child(tool("table-revert", Icon::Undo, "Revert").tooltip(tooltip("Drop the pending edits", None)).on_click(cx.listener(|this, _, _, cx| {
                        this.save_error = None;
                        this.grid.update(cx, |g, cx| g.revert(cx));
                    })))
                    .child(
                        div()
                            .id("table-submit")
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .h(px(26.))
                            .px(px(10.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .bg(color(c::ACCENT))
                            .hover(|s| s.bg(color_a(c::ACCENT, 0.85)))
                            .text_size(px(theme::UI_SMALL))
                            .text_color(color(0xFFFFFF))
                            .child(icon(Icon::Check, 14., color(0xFFFFFF)))
                            .child(if self.saving { "Saving…" } else { "Submit" })
                            .tooltip(tooltip("Save all changes in one transaction", None))
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    )
            })
            .child(div().flex_1())
            .when_some(self.status.clone(), |d, s| d.child(div().px(px(6.)).text_size(px(theme::UI_SMALL)).text_color(color(c::TEXT_MUTED)).child(s)))
            .when(self.loading, |d| d.child(div().px(px(6.)).text_size(px(theme::UI_SMALL)).text_color(color(c::TEXT_DIM)).child("Loading…")))
            .when(has_rows, |d| {
                d.child(
                    tool("table-export", Icon::FolderOpen, "Export").when(self.export_menu.is_none(), |b| b.tooltip(tooltip("Save the loaded rows as CSV, JSON or SQL", None))).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.export_menu = Some(ev.position);
                            cx.notify();
                        }),
                    ),
                )
            });
        let save_error = self.save_error.clone().map(|(message, sql)| {
            div()
                .flex()
                .flex_none()
                .items_start()
                .gap(px(8.))
                .mx(px(10.))
                .mt(px(8.))
                .p(px(10.))
                .rounded(px(8.))
                .bg(color_a(c::RED, 0.08))
                .border_1()
                .border_color(color_a(c::RED, 0.35))
                .font_family(theme::fonts().ui)
                .text_size(px(theme::UI_SMALL))
                .child(icon(Icon::Error, 14., color(c::RED)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .gap(px(4.))
                        .child(div().text_color(color(c::RED)).child(message))
                        .when_some(sql, |d, sql| d.child(div().font_family(theme::fonts().code).text_color(color(c::TEXT_MUTED)).child(sql))),
                )
                .child(div().id("save-error-close").cursor_pointer().child(icon(Icon::Close, 13., color(c::TEXT_DIM))).on_click(cx.listener(|this, _, _, cx| {
                    this.save_error = None;
                    cx.notify();
                })))
        });
        let body: gpui::AnyElement = match self.error.clone() {
            Some(e) => div()
                .m(px(10.))
                .p(px(12.))
                .rounded(px(8.))
                .bg(color_a(c::RED, 0.08))
                .border_1()
                .border_color(color_a(c::RED, 0.35))
                .font_family(theme::fonts().ui)
                .text_size(px(theme::UI_TEXT))
                .text_color(color(c::RED))
                .child(e)
                .into_any_element(),
            None => self.grid.clone().into_any_element(),
        };
        let menu = self.export_menu.map(|position| {
            export_menu(position, cx, |this, format, cx| this.export(format, cx), |this, cx| {
                this.export_menu = None;
                cx.notify();
            })
        });
        div().size_full().flex().flex_col().child(toolbar).children(save_error).child(div().flex().flex_col().flex_1().min_h(px(0.)).child(body)).children(menu)
    }
}
