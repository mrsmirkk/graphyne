//! Databases for Graphyne: connection settings, schema browsing and SQL execution — no UI.
//!
//! Every open connection lives on a thread of its own ([`DbSession`]): requests go there and
//! answers come back on channels, so a slow query never blocks the editor and can be
//! cancelled. SQLite is built in (nothing to install); PostgreSQL and MySQL / MariaDB are
//! reached over the network. All engines sit behind the same [`Driver`] interface.

mod config;
pub mod edit;
pub mod export;
pub mod import;
mod mysql_driver;
mod pg;
pub mod secrets;
pub mod select;
pub mod ssh;
mod session;
pub mod split;
mod sqlite;
mod value;

pub use edit::{CellEdit, EditStatement};
pub use export::{ExportFormat, export};
pub use import::{ColumnType, Csv, decode_text, match_columns, parse_csv};
pub use select::{SelectSource, single_table_select};
pub use config::{DbConfig, DbKind, SQLITE_EXTENSIONS, ServerConfig, SshConfig, SslMode, is_sqlite_file, load_configs, save_configs};
pub use session::DbSession;
pub use sqlite::create_sqlite;
pub use value::Value;

use std::time::Duration;

/// Rows a result keeps at most; the grid says when there were more.
pub const ROW_LIMIT: usize = 200_000;

/// Tables up to this size are counted exactly; bigger ones show the server's estimate.
pub(crate) const EXACT_COUNT_BELOW: u64 = 1_000_000;

/// Rows returned by a query.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResultSet {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// More rows were available than the limit.
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StatementResult {
    Rows(ResultSet),
    /// A statement without a result set (INSERT, CREATE…): rows it changed.
    Done { affected: u64 },
}

/// One statement of a script: its text, what it gave and how long it took.
#[derive(Clone, Debug)]
pub struct QueryOutcome {
    pub sql: String,
    pub result: Result<StatementResult, String>,
    pub elapsed: Duration,
}

/// Tables and views of a database.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schema {
    pub tables: Vec<Table>,
    pub views: Vec<Table>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    /// The schema (PostgreSQL) or database (MySQL) holding it, when not the default one
    /// (`public`, the connection's database).
    pub schema: Option<String>,
    pub name: String,
    pub columns: Vec<Column>,
}

impl Table {
    /// `schema.name`, or just the name in the default schema.
    pub fn display_name(&self) -> String {
        match &self.schema {
            Some(s) => format!("{s}.{}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub name: String,
    /// As declared (`INTEGER`, `VARCHAR(20)`…); may be empty in SQLite.
    pub type_name: String,
    pub not_null: bool,
    pub primary_key: bool,
}

/// How many rows a table has: counted, or the server's estimate for big tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowCount {
    pub rows: u64,
    pub exact: bool,
}

/// What a database engine provides. Used only on its session thread.
pub(crate) trait Driver: Send {
    fn schema(&mut self) -> Result<Schema, String>;
    /// Run one statement, keeping at most `limit` rows.
    fn run(&mut self, sql: &str, limit: usize) -> Result<StatementResult, String>;
    fn count_rows(&mut self, schema: Option<&str>, table: &str) -> Result<RowCount, String>;
    fn quote_ident(&self, name: &str) -> String;
    /// "PostgreSQL 17.10", "MariaDB 11.4.10", "SQLite 3.50.2".
    fn version(&mut self) -> Result<String, String>;

    fn quote_table(&self, schema: Option<&str>, table: &str) -> String {
        match schema {
            Some(s) => format!("{}.{}", self.quote_ident(s), self.quote_ident(table)),
            None => self.quote_ident(table),
        }
    }
}

/// The stop button of a connection: interrupts whatever statement runs on it.
pub(crate) type Interrupter = std::sync::Arc<dyn Fn() + Send + Sync>;
