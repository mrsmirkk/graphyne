//! The Database tool: the project's saved connections with their sessions and schemas
//! ([`DbStore`]), the Database panel, SQL consoles, table tabs and the data grid.
//!
//! Connections are kept per project in Graphyne's state folder (`databases.json`), consoles
//! next to them (`consoles/<connection>.sql`) — nothing is written into the project.

mod console;
mod form;
mod grid;
mod panel;
mod table;

pub use console::DbConsole;
pub use form::{ConnectionForm, Engine, FormEvent};
pub use panel::{DatabasePanel, DbPanelEvent};
pub use table::{DbTableEvent, DbTableView};

use gpui::{AppContext, Context, SharedString};
use graphyne_db::{DbConfig, DbKind, DbSession, Schema};
use std::path::{Path, PathBuf};
use std::sync::Arc;

gpui::actions!(database, [RunStatement, StopQuery]);

pub enum ConnState {
    Idle,
    Connecting,
    Ready(Arc<DbSession>),
    Failed(String),
}

pub struct DbEntry {
    pub config: DbConfig,
    pub conn: ConnState,
    /// Callers waiting for the connection being opened.
    waiters: Vec<flume::Sender<Result<Arc<DbSession>, String>>>,
    pub schema: Option<Arc<Schema>>,
    pub schema_error: Option<String>,
    pub loading_schema: bool,
}

impl DbEntry {
    fn new(config: DbConfig) -> Self {
        Self { config, conn: ConnState::Idle, waiters: Vec::new(), schema: None, schema_error: None, loading_schema: false }
    }
}

pub struct DbStore {
    root: PathBuf,
    pub entries: Vec<DbEntry>,
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().replace('/', "\\").eq_ignore_ascii_case(&b.to_string_lossy().replace('/', "\\"))
}

impl DbStore {
    pub fn new(root: PathBuf) -> Self {
        let entries = Self::config_file(&root).map(|f| graphyne_db::load_configs(&f)).unwrap_or_default().into_iter().map(DbEntry::new).collect();
        Self { root, entries }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn data_dir(root: &Path) -> Option<PathBuf> {
        graphyne_core::session::project_data_dir(root)
    }

    fn config_file(root: &Path) -> Option<PathBuf> {
        Self::data_dir(root).map(|d| d.join("databases.json"))
    }

    /// Where the SQL console of connection `id` keeps its text.
    pub fn console_file(&self, id: &str) -> Option<PathBuf> {
        Self::data_dir(&self.root).map(|d| d.join("consoles").join(format!("{id}.sql")))
    }

    fn save(&self, cx: &mut Context<Self>) {
        let Some(file) = Self::config_file(&self.root) else { return };
        let configs: Vec<DbConfig> = self.entries.iter().map(|e| e.config.clone()).collect();
        cx.background_spawn(async move {
            if let Err(e) = graphyne_db::save_configs(&file, &configs) {
                log::warn!("saving {}: {e}", file.display());
            }
        })
        .detach();
    }

    pub fn entry(&self, id: &str) -> Option<&DbEntry> {
        self.entries.iter().find(|e| e.config.id == id)
    }

    fn entry_mut(&mut self, id: &str) -> Option<&mut DbEntry> {
        self.entries.iter_mut().find(|e| e.config.id == id)
    }

    pub fn name(&self, id: &str) -> SharedString {
        self.entry(id).map(|e| e.config.name.clone()).unwrap_or_else(|| id.to_string()).into()
    }

    /// The connection to the SQLite file `path`, added if there is none yet; its id.
    pub fn add_sqlite(&mut self, path: &Path, cx: &mut Context<Self>) -> String {
        if let Some(e) = self.entries.iter().find(|e| e.config.file().is_some_and(|f| same_path(f, path))) {
            return e.config.id.clone();
        }
        let configs: Vec<DbConfig> = self.entries.iter().map(|e| e.config.clone()).collect();
        let config = DbConfig::sqlite(path, &configs);
        let id = config.id.clone();
        self.entries.push(DbEntry::new(config));
        self.save(cx);
        cx.notify();
        id
    }

    /// Add a server connection (`id` empty) or change one; the password goes to the
    /// Credential Manager (an empty one removes it). Returns the connection's id.
    pub fn save_server(&mut self, id: Option<&str>, name: &str, mut kind: DbKind, password: String, cx: &mut Context<Self>) -> String {
        let others: Vec<DbConfig> = self.entries.iter().filter(|e| Some(e.config.id.as_str()) != id).map(|e| e.config.clone()).collect();
        let existing = id.and_then(|id| self.entry(id)).map(|e| e.config.clone());
        // Keep the old password key so the saved password is replaced, not duplicated.
        let old_key = existing.as_ref().and_then(|c| c.kind.server()).and_then(|s| s.secret.clone());
        let config = match &existing {
            Some(old) => DbConfig { id: old.id.clone(), name: if name.trim().is_empty() { old.name.clone() } else { name.trim().to_string() }, kind: kind.clone() },
            None => DbConfig::server(name, kind.clone(), &others),
        };
        let key = old_key.unwrap_or_else(|| graphyne_db::secrets::new_key(&config.id));
        if let DbKind::Postgres(s) | DbKind::Mysql(s) = &mut kind {
            s.secret = (!password.is_empty()).then(|| key.clone());
        }
        let config = DbConfig { kind, ..config };
        let id = config.id.clone();
        // Written before anything connects (a quick system call): the panel opens the
        // connection right after Save and must find the password.
        if let Err(e) = graphyne_db::secrets::set_password(&key, &password) {
            log::warn!("{e}");
        }
        match self.entry_mut(&id) {
            Some(entry) => {
                entry.config = config;
                // New settings: reconnect on next use, reload the schema.
                entry.conn = ConnState::Idle;
                entry.schema = None;
                entry.schema_error = None;
            }
            None => self.entries.push(DbEntry::new(config)),
        }
        self.save(cx);
        cx.notify();
        id
    }

    /// Forget a connection (its open session closes, its saved password is removed). The
    /// database itself is not touched.
    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(key) = self.entry(id).and_then(|e| e.config.kind.server()).and_then(|s| s.secret.clone()) {
            cx.background_spawn(async move { drop(graphyne_db::secrets::delete_password(&key)) }).detach();
        }
        self.entries.retain(|e| e.config.id != id);
        self.save(cx);
        cx.notify();
    }

    /// The open session of `id`, connecting first if needed. Await the answer with
    /// `recv_async`.
    pub fn session(&mut self, id: &str, cx: &mut Context<Self>) -> flume::Receiver<Result<Arc<DbSession>, String>> {
        let (tx, rx) = flume::bounded(1);
        let Some(entry) = self.entry_mut(id) else {
            let _ = tx.send(Err(format!("no connection \u{201C}{id}\u{201D}")));
            return rx;
        };
        match &entry.conn {
            ConnState::Ready(session) => drop(tx.send(Ok(session.clone()))),
            ConnState::Connecting => entry.waiters.push(tx),
            ConnState::Idle | ConnState::Failed(_) => {
                entry.waiters.push(tx);
                entry.conn = ConnState::Connecting;
                let config = entry.config.clone();
                let id = id.to_string();
                cx.spawn(async move |this, cx| {
                    let result = cx.background_spawn(async move { DbSession::open(&config).map(Arc::new) }).await;
                    this.update(cx, |this, cx| {
                        if let Some(entry) = this.entry_mut(&id) {
                            entry.conn = match &result {
                                Ok(session) => ConnState::Ready(session.clone()),
                                Err(e) => ConnState::Failed(e.clone()),
                            };
                            for waiter in entry.waiters.drain(..) {
                                let _ = waiter.send(result.clone());
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
        rx
    }

    /// (Re)load the tables and views of `id`.
    pub fn load_schema(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.entry_mut(id) else { return };
        if entry.loading_schema {
            return;
        }
        entry.loading_schema = true;
        let waiting = self.session(id, cx);
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let schema = match waiting.recv_async().await {
                Ok(Ok(session)) => session.schema().recv_async().await.unwrap_or_else(|_| Err("the connection was closed".into())),
                Ok(Err(e)) => Err(e),
                Err(_) => Err("the connection was closed".into()),
            };
            this.update(cx, |this, cx| {
                if let Some(entry) = this.entry_mut(&id) {
                    entry.loading_schema = false;
                    match schema {
                        Ok(schema) => {
                            entry.schema = Some(Arc::new(schema));
                            entry.schema_error = None;
                        }
                        Err(e) => entry.schema_error = Some(e),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Drop the session (it reconnects on next use), e.g. after the file was replaced.
    pub fn disconnect(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(entry) = self.entry_mut(id) {
            if !matches!(entry.conn, ConnState::Connecting) {
                entry.conn = ConnState::Idle;
            }
        }
        cx.notify();
    }
}

/// `1234567` → `1,234,567`.
pub(crate) fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// `0.4 ms`, `12 ms`, `1.25 s`.
pub(crate) fn format_elapsed(d: std::time::Duration) -> String {
    let ms = d.as_secs_f64() * 1000.;
    if ms < 1. {
        format!("{ms:.1} ms")
    } else if ms < 1000. {
        format!("{ms:.0} ms")
    } else {
        format!("{:.2} s", ms / 1000.)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn numbers_read_well() {
        assert_eq!(super::group_digits(0), "0");
        assert_eq!(super::group_digits(1234), "1,234");
        assert_eq!(super::group_digits(1234567), "1,234,567");
        assert_eq!(super::format_elapsed(std::time::Duration::from_micros(400)), "0.4 ms");
        assert_eq!(super::format_elapsed(std::time::Duration::from_millis(1250)), "1.25 s");
    }
}
