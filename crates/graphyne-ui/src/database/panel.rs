//! The Database panel (right island): connections → tables and views → columns.

use super::{ConnState, DbStore};
use crate::assets::Icon;
use crate::components::{icon, tooltip};
use crate::theme::{self, c, color};
use gpui::prelude::*;
use gpui::{
    App, ClickEvent, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseButton, MouseDownEvent, PathPromptOptions, Pixels, Point,
    SharedString, UniformListScrollHandle, Window, anchored, deferred, div, px, uniform_list,
};
use std::collections::HashSet;
use std::path::PathBuf;

pub enum DbPanelEvent {
    /// (connection, schema, table)
    OpenTable(String, Option<String>, String),
    /// Open the New Connection dialog (`true`: MySQL / MariaDB).
    NewServer(bool),
    EditConnection(String),
    /// A connection was removed: its console and table tabs close.
    Removed(String),
    /// Open the console of a connection, optionally adding a query to it.
    OpenConsole(String, Option<String>),
    Notify(String, bool),
}

#[derive(Clone, PartialEq)]
enum RowKind {
    Connection { location: SharedString, state: SharedString, failed: bool, engine: &'static str },
    Group { views: bool, count: usize },
    Table { view: bool, columns: usize },
    Column { type_name: SharedString, primary_key: bool, not_null: bool },
    Note { error: bool },
}

#[derive(Clone)]
struct Row {
    /// Expansion / selection key: `c/<id>`, `g/<id>/tables`, `t/<id>/<table>`…
    key: String,
    db: String,
    name: SharedString,
    depth: usize,
    expandable: bool,
    kind: RowKind,
    /// For table rows: (schema, table).
    table: Option<(Option<String>, String)>,
}

enum MenuTarget {
    Add,
    Connection(String),
    /// (connection, schema, table)
    Table(String, Option<String>, String),
}

pub struct DatabasePanel {
    focus_handle: FocusHandle,
    store: Entity<DbStore>,
    expanded: HashSet<String>,
    selected: Option<String>,
    scroll: UniformListScrollHandle,
    menu: Option<(MenuTarget, Point<Pixels>)>,
    _sub: gpui::Subscription,
}

impl EventEmitter<DbPanelEvent> for DatabasePanel {}

impl Focusable for DatabasePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl DatabasePanel {
    pub fn new(store: Entity<DbStore>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&store, |_, _, cx| cx.notify());
        Self { focus_handle: cx.focus_handle(), store, expanded: HashSet::new(), selected: None, scroll: UniformListScrollHandle::new(), menu: None, _sub: sub }
    }

    /// Show connection `id` opened up (after adding or opening a database file).
    pub fn reveal(&mut self, id: &str, cx: &mut Context<Self>) {
        self.expand_connection(id, cx);
        self.selected = Some(format!("c/{id}"));
        cx.notify();
    }

    fn expand_connection(&mut self, id: &str, cx: &mut Context<Self>) {
        self.expanded.insert(format!("c/{id}"));
        // Tables open by default the first time.
        self.expanded.insert(format!("g/{id}/tables"));
        let needs_schema = self.store.read(cx).entry(id).is_some_and(|e| e.schema.is_none() && !e.loading_schema);
        if needs_schema {
            self.store.update(cx, |s, cx| s.load_schema(id, cx));
        }
    }

    fn rows(&self, cx: &App) -> Vec<Row> {
        let mut rows = Vec::new();
        let store = self.store.read(cx);
        for entry in &store.entries {
            let id = entry.config.id.clone();
            let key = format!("c/{id}");
            let open = self.expanded.contains(&key);
            let (state, failed) = match &entry.conn {
                ConnState::Connecting => ("connecting…".into(), false),
                ConnState::Failed(_) => ("not connected".into(), true),
                _ if entry.loading_schema => ("loading…".into(), false),
                _ => (SharedString::default(), false),
            };
            rows.push(Row {
                key,
                db: id.clone(),
                name: entry.config.name.clone().into(),
                depth: 0,
                expandable: true,
                // A database inside the project shows its path from the project root.
                table: None,
                kind: RowKind::Connection {
                    engine: entry.config.kind.engine(),
                    location: entry.config.file().and_then(|f| f.strip_prefix(store.root()).ok()).map(|r| r.display().to_string()).unwrap_or_else(|| entry.config.location()).into(),
                    state,
                    failed,
                },
            });
            if !open {
                continue;
            }
            let note = |text: String, error: bool| Row { key: format!("n/{id}"), db: id.clone(), name: text.into(), depth: 1, expandable: false, kind: RowKind::Note { error }, table: None };
            if let ConnState::Failed(e) = &entry.conn {
                rows.push(note(e.clone(), true));
                continue;
            }
            if let Some(e) = &entry.schema_error {
                rows.push(note(e.clone(), true));
            }
            let Some(schema) = &entry.schema else {
                if entry.schema_error.is_none() {
                    rows.push(note("Loading…".into(), false));
                }
                continue;
            };
            if schema.tables.is_empty() && schema.views.is_empty() {
                rows.push(note("No tables yet".into(), false));
                continue;
            }
            for (views, list) in [(false, &schema.tables), (true, &schema.views)] {
                if list.is_empty() {
                    continue;
                }
                let group_key = format!("g/{id}/{}", if views { "views" } else { "tables" });
                let group_open = self.expanded.contains(&group_key);
                rows.push(Row { key: group_key, db: id.clone(), name: if views { "views" } else { "tables" }.into(), depth: 1, expandable: true, kind: RowKind::Group { views, count: list.len() }, table: None });
                if !group_open {
                    continue;
                }
                for table in list.iter() {
                    let shown = table.display_name();
                    let table_key = format!("t/{id}/{shown}");
                    let table_open = self.expanded.contains(&table_key);
                    rows.push(Row {
                        key: table_key,
                        db: id.clone(),
                        name: shown.clone().into(),
                        depth: 2,
                        expandable: !table.columns.is_empty(),
                        kind: RowKind::Table { view: views, columns: table.columns.len() },
                        table: Some((table.schema.clone(), table.name.clone())),
                    });
                    if table_open {
                        for col in &table.columns {
                            rows.push(Row {
                                key: format!("col/{id}/{shown}/{}", col.name),
                                table: None,
                                db: id.clone(),
                                name: col.name.clone().into(),
                                depth: 3,
                                expandable: false,
                                kind: RowKind::Column { type_name: col.type_name.clone().into(), primary_key: col.primary_key, not_null: col.not_null },
                            });
                        }
                    }
                }
            }
        }
        rows
    }

    fn toggle(&mut self, row: &Row, cx: &mut Context<Self>) {
        if self.expanded.remove(&row.key) {
            cx.notify();
            return;
        }
        if matches!(row.kind, RowKind::Connection { .. }) {
            // A connection that failed tries again when opened.
            let failed = self.store.read(cx).entry(&row.db).is_some_and(|e| matches!(e.conn, ConnState::Failed(_)) || e.schema_error.is_some());
            if failed {
                let id = row.db.clone();
                self.store.update(cx, |s, cx| {
                    s.disconnect(&id, cx);
                    s.load_schema(&id, cx);
                });
            }
            self.expand_connection(&row.db.clone(), cx);
        } else {
            self.expanded.insert(row.key.clone());
        }
        cx.notify();
    }

    fn click_row(&mut self, row: Row, ev: &ClickEvent, cx: &mut Context<Self>) {
        self.menu = None;
        self.selected = Some(row.key.clone());
        match &row.kind {
            RowKind::Table { .. } if ev.click_count() >= 2 => {
                if let Some((schema, table)) = row.table.clone() {
                    cx.emit(DbPanelEvent::OpenTable(row.db.clone(), schema, table));
                }
            }
            RowKind::Table { .. } | RowKind::Column { .. } | RowKind::Note { .. } => {}
            RowKind::Connection { .. } | RowKind::Group { .. } => self.toggle(&row, cx),
        }
        cx.notify();
    }

    pub fn add_existing(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        let root = self.store.read(cx).root().to_path_buf();
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Add".into()), directory: Some(root) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            this.update(cx, |this, cx| {
                for path in paths {
                    if graphyne_db::is_sqlite_file(&path) {
                        let id = this.store.update(cx, |s, cx| s.add_sqlite(&path, cx));
                        this.reveal(&id, cx);
                    } else {
                        cx.emit(DbPanelEvent::Notify(format!("{} is not an SQLite database", path.display()), true));
                    }
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn create_new(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        let root = self.store.read(cx).root().to_path_buf();
        let picked = cx.prompt_for_new_path(&root, Some("database.sqlite"));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let target: PathBuf = path.clone();
            let created = cx.background_spawn(async move { graphyne_db::create_sqlite(&target) }).await;
            this.update(cx, |this, cx| match created {
                Ok(()) => {
                    let id = this.store.update(cx, |s, cx| s.add_sqlite(&path, cx));
                    this.reveal(&id, cx);
                    cx.emit(DbPanelEvent::OpenConsole(id, None));
                }
                Err(e) => cx.emit(DbPanelEvent::Notify(format!("Could not create the database: {e}"), true)),
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn open_menu(&mut self, target: MenuTarget, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = Some((target, position));
        cx.notify();
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let (target, position) = self.menu.as_ref()?;
        let item = |id: &'static str, label: &'static str, ic: Icon| {
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
                .child(icon(ic, 14., color(c::TEXT_MUTED)))
                .child(div().text_color(color(c::TEXT)).child(label))
        };
        let separator = || div().my(px(4.)).h(px(1.)).bg(color(c::BORDER));
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.menu = None;
            cx.notify();
        };
        let mut menu = div()
            .id("db-menu")
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
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| close(this, cx)));
        match target {
            MenuTarget::Add => {
                menu = menu
                    .child(item("db-new-pg", "PostgreSQL…", Icon::Database).on_click(cx.listener(move |this, _, _, cx| {
                        close(this, cx);
                        cx.emit(DbPanelEvent::NewServer(false));
                    })))
                    .child(item("db-new-my", "MySQL / MariaDB…", Icon::Database).on_click(cx.listener(move |this, _, _, cx| {
                        close(this, cx);
                        cx.emit(DbPanelEvent::NewServer(true));
                    })))
                    .child(separator())
                    .child(item("db-add-file", "Add SQLite File…", Icon::FolderOpen).on_click(cx.listener(|this, _, _, cx| this.add_existing(cx))))
                    .child(item("db-new-file", "New SQLite Database…", Icon::FilePlus).on_click(cx.listener(|this, _, _, cx| this.create_new(cx))));
            }
            MenuTarget::Connection(id) => {
                let (console, refresh, remove, edit) = (id.clone(), id.clone(), id.clone(), id.clone());
                let file = self.store.read(cx).entry(id).and_then(|e| e.config.file().map(|p| p.to_path_buf()));
                let server = self.store.read(cx).entry(id).is_some_and(|e| e.config.kind.server().is_some());
                menu = menu
                    .child(item("db-console", "Open Console", Icon::Terminal).on_click(cx.listener(move |this, _, _, cx| {
                        close(this, cx);
                        cx.emit(DbPanelEvent::OpenConsole(console.clone(), None));
                    })))
                    .when(server, |m| {
                        m.child(item("db-edit", "Edit Connection…", Icon::Pencil).on_click(cx.listener(move |this, _, _, cx| {
                            close(this, cx);
                            cx.emit(DbPanelEvent::EditConnection(edit.clone()));
                        })))
                    })
                    .child(item("db-refresh", "Refresh", Icon::Refresh).on_click(cx.listener(move |this, _, _, cx| {
                        close(this, cx);
                        let id = refresh.clone();
                        this.store.update(cx, |s, cx| {
                            s.disconnect(&id, cx);
                            s.load_schema(&id, cx);
                        });
                        this.expand_connection(&id, cx);
                    })))
                    .child(separator());
                if let Some(file) = file {
                    let (reveal, copy) = (file.clone(), file);
                    menu = menu
                        .child(item("db-reveal", "Reveal in File Explorer", Icon::FolderOpen).on_click(cx.listener(move |this, _, _, cx| {
                            close(this, cx);
                            cx.reveal_path(&reveal);
                        })))
                        .child(item("db-copy-path", "Copy Path", Icon::Copy).on_click(cx.listener(move |this, _, _, cx| {
                            close(this, cx);
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.display().to_string()));
                        })))
                        .child(separator());
                }
                menu = menu.child(item("db-remove", "Remove Connection", Icon::Close).on_click(cx.listener(move |this, _, _, cx| {
                    close(this, cx);
                    let id = remove.clone();
                    this.store.update(cx, |s, cx| s.remove(&id, cx));
                    cx.emit(DbPanelEvent::Removed(id));
                    cx.emit(DbPanelEvent::Notify("Connection removed (the database itself was not touched).".into(), false));
                })));
            }
            MenuTarget::Table(id, schema, table) => {
                let (open_db, open_schema, open_table) = (id.clone(), schema.clone(), table.clone());
                let quoted = self.store.read(cx).entry(id).map(|e| e.config.kind.quote_table(schema.as_deref(), table)).unwrap_or_else(|| table.clone());
                let (console_db, console_sql) = (id.clone(), format!("SELECT * FROM {quoted};"));
                let name = match schema {
                    Some(s) => format!("{s}.{table}"),
                    None => table.clone(),
                };
                menu = menu
                    .child(item("db-open-table", "Open Data", Icon::Table).on_click(cx.listener(move |this, _, _, cx| {
                        close(this, cx);
                        cx.emit(DbPanelEvent::OpenTable(open_db.clone(), open_schema.clone(), open_table.clone()));
                    })))
                    .child(item("db-query-table", "Query in Console", Icon::Terminal).on_click(cx.listener(move |this, _, _, cx| {
                        close(this, cx);
                        cx.emit(DbPanelEvent::OpenConsole(console_db.clone(), Some(console_sql.clone())));
                    })))
                    .child(separator())
                    .child(item("db-copy-name", "Copy Name", Icon::Copy).on_click(cx.listener(move |this, _, _, cx| {
                        close(this, cx);
                        cx.write_to_clipboard(ClipboardItem::new_string(name.clone()));
                    })));
            }
        }
        Some(deferred(anchored().position(*position).snap_to_window_with_margin(px(8.)).child(menu)).with_priority(2).into_any_element())
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let button = |id: &'static str, label: &'static str, ic: Icon| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(30.))
                .px(px(12.))
                .rounded(px(6.))
                .border_1()
                .border_color(color(c::POPUP_BORDER))
                .cursor_pointer()
                .hover(|s| s.bg(color(c::HOVER)))
                .text_color(color(c::TEXT))
                .child(icon(ic, 14., color(c::TEXT_MUTED)))
                .child(label)
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(10.))
            .pt(px(36.))
            .px(px(16.))
            .text_size(px(theme::UI_TEXT))
            .child(icon(Icon::Database, 28., color(c::TEXT_DIM)))
            .child(div().text_color(color(c::TEXT)).child("No databases yet"))
            .child(button("db-empty-pg", "PostgreSQL…", Icon::Database).on_click(cx.listener(|_, _, _, cx| cx.emit(DbPanelEvent::NewServer(false)))))
            .child(button("db-empty-my", "MySQL / MariaDB…", Icon::Database).on_click(cx.listener(|_, _, _, cx| cx.emit(DbPanelEvent::NewServer(true)))))
            .child(button("db-empty-add", "Add SQLite File…", Icon::FolderOpen).on_click(cx.listener(|this, _, _, cx| this.add_existing(cx))))
            .child(button("db-empty-new", "New SQLite Database…", Icon::FilePlus).on_click(cx.listener(|this, _, _, cx| this.create_new(cx))))
            .child(div().pt(px(4.)).text_size(px(theme::UI_SMALL)).text_color(color(c::TEXT_DIM)).text_center().child("…or open a .db / .sqlite file from the project tree."))
    }
}

impl Render for DatabasePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows(cx);
        let empty = self.store.read(cx).entries.is_empty();
        let selected = self.selected.clone();
        let menu = self.render_menu(cx);
        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .h(px(30.))
            .px(px(6.))
            .child(
                div()
                    .id("db-add")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .hover(|s| s.bg(color(c::HOVER)))
                    .when(self.menu.is_none(), |d| d.tooltip(tooltip("Add a database", None)))
                    .child(icon(Icon::Plus, 14., color(c::TEXT_MUTED)))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.open_menu(MenuTarget::Add, ev.position, cx);
                    })),
            )
            .child(
                div()
                    .id("db-refresh-all")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .hover(|s| s.bg(color(c::HOVER)))
                    .tooltip(tooltip("Refresh the open connections", None))
                    .child(icon(Icon::Refresh, 14., color(c::TEXT_MUTED)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let open: Vec<String> = this.store.read(cx).entries.iter().map(|e| e.config.id.clone()).filter(|id| this.expanded.contains(&format!("c/{id}"))).collect();
                        this.store.update(cx, |s, cx| {
                            for id in &open {
                                s.disconnect(id, cx);
                                s.load_schema(id, cx);
                            }
                        });
                    })),
            );
        let list = uniform_list("db-tree", rows.len(), cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
            range
                .map(|ix| {
                    let row = rows[ix].clone();
                    let is_selected = selected.as_ref() == Some(&row.key);
                    let open = row.expandable && _this.expanded.contains(&row.key);
                    let chevron = div()
                        .id(("db-chevron", ix))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .w(px(16.))
                        .when(row.expandable, |d| {
                            let toggle_row = row.clone();
                            d.child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 13., color(c::TEXT_DIM))).on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle(&toggle_row, cx);
                            }))
                        });
                    let (lead, name_color, detail): (Option<gpui::AnyElement>, gpui::Hsla, Option<(SharedString, gpui::Hsla)>) = match &row.kind {
                        RowKind::Connection { location, state, failed, engine } => (
                            // The icon's colour tells the engine apart.
                            Some(
                                icon(
                                    Icon::Database,
                                    14.,
                                    if *failed {
                                        color(c::RED)
                                    } else {
                                        match *engine {
                                            "PostgreSQL" => color(c::BLUE),
                                            "MySQL" => color(c::ORANGE),
                                            _ => color(c::ACCENT),
                                        }
                                    },
                                )
                                .into_any_element(),
                            ),
                            color(c::TEXT),
                            Some(if state.is_empty() { (location.clone(), color(c::TEXT_FAINT)) } else { (state.clone(), if *failed { color(c::RED) } else { color(c::TEXT_DIM) }) }),
                        ),
                        RowKind::Group { count, .. } => (None, color(c::TEXT_MUTED), Some((count.to_string().into(), color(c::TEXT_FAINT)))),
                        RowKind::Table { view, .. } => (Some(icon(if *view { Icon::Eye } else { Icon::Table }, 13., color(c::TEXT_MUTED)).into_any_element()), color(c::TEXT), None),
                        RowKind::Column { type_name, primary_key, not_null } => (
                            Some(if *primary_key { icon(Icon::Key, 12., color(c::YELLOW)).into_any_element() } else { div().size(px(5.)).rounded_full().bg(color(c::TEXT_DIM)).into_any_element() }),
                            color(c::TEXT),
                            Some((format!("{}{}", if type_name.is_empty() { "any" } else { type_name.as_ref() }, if *not_null && !*primary_key { " · not null" } else { "" }).into(), color(c::TEXT_FAINT))),
                        ),
                        RowKind::Note { error } => (None, if *error { color(c::RED) } else { color(c::TEXT_DIM) }, None),
                    };
                    let menu_row = row.clone();
                    let click_row = row.clone();
                    // Notes (errors) can be longer than the panel is wide.
                    let note_tip = matches!(row.kind, RowKind::Note { .. }).then(|| row.name.clone());
                    div()
                        .id(ix)
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .h(px(24.))
                        .w_full()
                        .pl(px(6. + row.depth as f32 * 14.))
                        .pr(px(8.))
                        .rounded(px(5.))
                        .text_size(px(theme::UI_TEXT))
                        .cursor_pointer()
                        .when(is_selected, |d| d.bg(color(c::TAB_ACTIVE)))
                        .when(!is_selected, |d| d.hover(|s| s.bg(color(c::HOVER))))
                        .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                            window.focus(&this.focus_handle);
                            this.click_row(click_row.clone(), ev, cx);
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                this.selected = Some(menu_row.key.clone());
                                let target = match &menu_row.kind {
                                    RowKind::Connection { .. } => MenuTarget::Connection(menu_row.db.clone()),
                                    RowKind::Table { .. } => match menu_row.table.clone() {
                                        Some((schema, table)) => MenuTarget::Table(menu_row.db.clone(), schema, table),
                                        None => return,
                                    },
                                    _ => return,
                                };
                                this.open_menu(target, ev.position, cx);
                            }),
                        )
                        .when_some(note_tip, |d, text| d.tooltip(tooltip(text, None)))
                        .child(chevron)
                        .children(lead.map(|l| div().flex().flex_none().items_center().justify_center().w(px(14.)).child(l)))
                        .child(div().flex_none().max_w(px(220.)).overflow_hidden().whitespace_nowrap().text_color(name_color).child(row.name.clone()))
                        .children(detail.map(|(text, tint)| div().flex_1().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_size(px(11.5)).text_color(tint).child(text)))
                })
                .collect()
        }))
        .track_scroll(self.scroll.clone())
        .px(px(4.))
        .flex_1();
        div()
            .id("database-panel")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .font_family(theme::fonts().ui)
            .child(toolbar)
            .when(empty, |d| d.child(self.render_empty(cx)))
            .when(!empty, |d| d.child(list))
            .children(menu)
    }
}
