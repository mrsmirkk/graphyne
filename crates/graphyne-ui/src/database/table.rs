//! A table (or view) tab: its rows in the data grid, with the total count.

use super::grid::DataGrid;
use super::{DbStore, format_elapsed, group_digits};
use crate::assets::Icon;
use crate::components::{icon, tooltip};
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Window, div, px};
use graphyne_db::ROW_LIMIT;
use std::time::Instant;

pub enum DbTableEvent {
    /// Open the connection's console with this query added.
    OpenConsole(String, String),
}

pub struct DbTableView {
    pub db_id: String,
    pub table: String,
    store: Entity<DbStore>,
    grid: Entity<DataGrid>,
    loading: bool,
    status: Option<SharedString>,
    error: Option<SharedString>,
}

impl EventEmitter<DbTableEvent> for DbTableView {}

impl Focusable for DbTableView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.focus_handle(cx)
    }
}

impl DbTableView {
    pub fn new(db_id: String, table: String, store: Entity<DbStore>, cx: &mut Context<Self>) -> Self {
        let mut view = Self { db_id, table, store, grid: cx.new(DataGrid::new), loading: false, status: None, error: None };
        view.load(cx);
        view
    }

    pub fn load(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.error = None;
        let waiting = self.store.update(cx, |s, cx| s.session(&self.db_id, cx));
        let table = self.table.clone();
        cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let loaded = match waiting.recv_async().await {
                Ok(Ok(session)) => session.table_rows(table, ROW_LIMIT).recv_async().await.map_err(|_| "the connection was closed".to_string()),
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
                                Some(total) if total > shown => format!("the first {} of {} rows · {elapsed}", group_digits(shown), group_digits(total)),
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
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

impl Render for DbTableView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.store.read(cx).name(&self.db_id);
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
        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .h(px(36.))
            .px(px(10.))
            .border_b_1()
            .border_color(color(c::BORDER))
            .font_family(theme::fonts().ui)
            .text_color(color(c::TEXT))
            .child(icon(Icon::Table, 15., color(c::ACCENT)))
            .child(div().font_weight(gpui::FontWeight::MEDIUM).text_size(px(theme::UI_TEXT)).child(self.table.clone()))
            .child(div().text_size(px(theme::UI_SMALL)).text_color(color(c::TEXT_DIM)).child(name))
            .child(div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(color(c::BORDER)))
            .child(tool("table-refresh", Icon::Refresh, "Refresh").tooltip(tooltip("Read the rows again", None)).on_click(cx.listener(|this, _, _, cx| this.load(cx))))
            .child(tool("table-console", Icon::Terminal, "Query in Console").tooltip(tooltip("Open the SQL console with a SELECT for this table", None)).on_click(cx.listener(|this, _, _, cx| {
                let sql = format!("SELECT * FROM \"{}\";", this.table.replace('"', "\"\""));
                cx.emit(DbTableEvent::OpenConsole(this.db_id.clone(), sql));
            })))
            .child(div().flex_1())
            .when_some(self.status.clone(), |d, s| d.child(div().text_size(px(theme::UI_SMALL)).text_color(color(c::TEXT_MUTED)).child(s)))
            .when(self.loading, |d| d.child(div().text_size(px(theme::UI_SMALL)).text_color(color(c::TEXT_DIM)).child("Loading…")));
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
        div().size_full().flex().flex_col().child(toolbar).child(div().flex().flex_col().flex_1().min_h(px(0.)).child(body))
    }
}
