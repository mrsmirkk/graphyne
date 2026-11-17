//! Databases for Graphyne: connection settings, schema browsing and SQL execution — no UI.
//!
//! Every open connection lives on a thread of its own ([`DbSession`]): requests go there and
//! answers come back on channels, so a slow query never blocks the editor and can be
//! cancelled. SQLite is built in (nothing to install); other engines plug in behind the same
//! [`Driver`] interface.

mod config;
mod session;
pub mod split;
mod sqlite;
mod value;

pub use config::{DbConfig, DbKind, SQLITE_EXTENSIONS, is_sqlite_file, load_configs, save_configs};
pub use session::DbSession;
pub use sqlite::create_sqlite;
pub use value::Value;

use std::time::Duration;

/// Rows a result keeps at most; the grid says when there were more.
pub const ROW_LIMIT: usize = 200_000;

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
    pub name: String,
    pub columns: Vec<Column>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub name: String,
    /// As declared (`INTEGER`, `VARCHAR(20)`…); may be empty in SQLite.
    pub type_name: String,
    pub not_null: bool,
    pub primary_key: bool,
}

/// What a database engine provides. Used only on its session thread.
pub(crate) trait Driver: Send {
    fn schema(&mut self) -> Result<Schema, String>;
    /// Run one statement, keeping at most `limit` rows.
    fn run(&mut self, sql: &str, limit: usize) -> Result<StatementResult, String>;
    fn count_rows(&mut self, table: &str) -> Result<u64, String>;
    fn quote_ident(&self, name: &str) -> String;
}
