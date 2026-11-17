//! SQLite, built into Graphyne (the `bundled` engine — nothing to install).

use crate::{Column, Driver, ResultSet, Schema, StatementResult, Table, Value};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub(crate) struct Sqlite {
    conn: Connection,
}

fn err(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Open an existing database file. Never creates one: a connection whose file is gone must
/// say so rather than quietly start an empty database.
pub(crate) fn open(path: &Path) -> Result<(Sqlite, Arc<dyn Fn() + Send + Sync>), String> {
    if !path.is_file() {
        return Err(format!("{} not found", path.display()));
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags).map_err(err)?;
    conn.busy_timeout(Duration::from_secs(5)).map_err(err)?;
    let handle = conn.get_interrupt_handle();
    Ok((Sqlite { conn }, Arc::new(move || handle.interrupt())))
}

/// File > New SQLite database: an empty database (with its header written, so it is
/// recognised as SQLite right away).
pub fn create_sqlite(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    let conn = Connection::open(path).map_err(err)?;
    conn.execute_batch("CREATE TABLE graphyne_init(x); DROP TABLE graphyne_init;").map_err(err)
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn value(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Int(i),
        ValueRef::Real(r) => Value::Real(r),
        ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::blob(b),
    }
}

impl Sqlite {
    fn columns(&self, table: &str) -> Result<Vec<Column>, String> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({})", quote(table))).map_err(err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Column { name: r.get(1)?, type_name: r.get::<_, Option<String>>(2)?.unwrap_or_default(), not_null: r.get::<_, i64>(3)? != 0, primary_key: r.get::<_, i64>(5)? > 0 })
            })
            .map_err(err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(err)
    }
}

impl Driver for Sqlite {
    fn schema(&mut self) -> Result<Schema, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT type, name FROM sqlite_schema WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name COLLATE NOCASE")
            .map_err(err)?;
        let objects: Vec<(String, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(err)?.collect::<Result<_, _>>().map_err(err)?;
        let mut schema = Schema::default();
        for (kind, name) in objects {
            let table = Table { columns: self.columns(&name)?, name };
            if kind == "view" { schema.views.push(table) } else { schema.tables.push(table) }
        }
        Ok(schema)
    }

    fn run(&mut self, sql: &str, limit: usize) -> Result<StatementResult, String> {
        // SQLite repeats the statement in syntax errors; the console shows it already.
        let mut stmt = self.conn.prepare(sql).map_err(|e| err(e).replace(&format!(" in {sql}"), ""))?;
        let count = stmt.column_count();
        if count == 0 {
            let affected = stmt.execute([]).map_err(err)? as u64;
            return Ok(StatementResult::Done { affected });
        }
        let columns: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
        let mut rows = stmt.query([]).map_err(err)?;
        let mut out = Vec::new();
        let mut truncated = false;
        while let Some(row) = rows.next().map_err(err)? {
            if out.len() == limit {
                truncated = true;
                break;
            }
            let mut cells = Vec::with_capacity(count);
            for i in 0..count {
                cells.push(value(row.get_ref(i).map_err(err)?));
            }
            out.push(cells);
        }
        Ok(StatementResult::Rows(ResultSet { columns, rows: out, truncated }))
    }

    fn count_rows(&mut self, table: &str) -> Result<u64, String> {
        self.conn.query_row(&format!("SELECT COUNT(*) FROM {}", quote(table)), [], |r| r.get::<_, i64>(0)).map(|n| n as u64).map_err(err)
    }

    fn quote_ident(&self, name: &str) -> String {
        quote(name)
    }
}
