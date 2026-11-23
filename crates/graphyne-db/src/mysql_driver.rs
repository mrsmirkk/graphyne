//! MySQL and MariaDB over the network (the `mysql` crate; TLS through the system's SChannel).

use crate::config::{ServerConfig, SslMode};
use crate::{Column, Driver, EXACT_COUNT_BELOW, Interrupter, ResultSet, RowCount, Schema, StatementResult, Table, Value};
use mysql::consts::ColumnType;
use mysql::prelude::Queryable;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// The character set number MySQL gives binary data.
const BINARY_CHARSET: u16 = 63;

pub(crate) struct My {
    conn: mysql::Conn,
}

fn my_err(e: mysql::Error) -> String {
    match e {
        // "ERROR 1146 (42S02): Table 'shop.x' doesn't exist"
        mysql::Error::MySqlError(e) => format!("ERROR {} ({}): {}", e.code, e.state, e.message),
        mysql::Error::DriverError(e) => e.to_string(),
        mysql::Error::IoError(e) => e.to_string(),
        other => other.to_string(),
    }
}

/// A server that cannot be reached at all: trying again without TLS would only double the wait.
fn unreachable(e: &mysql::Error) -> bool {
    matches!(e, mysql::Error::IoError(_) | mysql::Error::DriverError(mysql::DriverError::CouldNotConnect(_) | mysql::DriverError::ConnectTimeout))
}

/// The addresses to try: "localhost" is IPv4 first, then IPv6 (the driver tries only one
/// address per name, and servers often listen on 127.0.0.1 only).
fn hosts(cfg: &ServerConfig) -> Vec<String> {
    if let Some(via) = cfg.via {
        return vec![via.ip().to_string()];
    }
    match cfg.host.trim() {
        "" | "localhost" => vec!["127.0.0.1".into(), "::1".into()],
        host => vec![host.to_string()],
    }
}

fn opts(cfg: &ServerConfig, host: &str, password: Option<String>, ssl: Option<mysql::SslOpts>) -> mysql::Opts {
    mysql::OptsBuilder::new()
        .ip_or_hostname(Some(host.to_string()))
        .tcp_port(cfg.via.map_or(cfg.port, |v| v.port()))
        .prefer_socket(false)
        .user((!cfg.user.is_empty()).then(|| cfg.user.clone()))
        .pass(password)
        .db_name((!cfg.database.is_empty()).then(|| cfg.database.clone()))
        .tcp_connect_timeout(Some(Duration::from_secs(10)))
        .ssl_opts(ssl)
        .into()
}

pub(crate) fn open(cfg: &ServerConfig, password: Option<String>) -> Result<(My, Interrupter), String> {
    // Like libpq: only "verify" checks the certificate.
    let verify = cfg.ssl == SslMode::VerifyFull;
    // Through an SSH tunnel the driver connects to 127.0.0.1, and it can only check the
    // certificate's name against the address it connects to: there, "verify" checks the
    // certificate but not its name.
    let tls = mysql::SslOpts::default().with_danger_accept_invalid_certs(!verify).with_danger_skip_domain_validation(!verify || cfg.via.is_some());
    let connect_to = |host: &str, ssl: Option<mysql::SslOpts>| {
        let o = opts(cfg, host, password.clone(), ssl);
        mysql::Conn::new(o.clone()).map(|c| (c, o))
    };
    let connect = |host: &str| match cfg.ssl {
        SslMode::Disable => connect_to(host, None),
        // Servers without TLS are common on a developer's machine.
        SslMode::Prefer => connect_to(host, Some(tls.clone())).or_else(|e| if unreachable(&e) { Err(e) } else { connect_to(host, None) }),
        SslMode::Require | SslMode::VerifyFull => connect_to(host, Some(tls.clone())),
    };
    let mut result = Err(String::new());
    for host in hosts(cfg) {
        result = connect(&host).map_err(my_err);
        if result.is_ok() {
            break;
        }
    }
    let (conn, used) = result?;
    // Stop: a second connection asks the server to end this one's statement.
    let id = conn.connection_id();
    let interrupt: Interrupter = Arc::new(move || {
        if let Ok(mut killer) = mysql::Conn::new(used.clone()) {
            let _ = killer.query_drop(format!("KILL QUERY {id}"));
        }
    });
    Ok((My { conn }, interrupt))
}

fn cell(value: mysql::Value, column: &mysql::Column) -> Value {
    use mysql::Value as V;
    match value {
        V::NULL => Value::Null,
        // The text protocol sends everything as text; the column type tells what it is.
        V::Bytes(bytes) => {
            let text = || String::from_utf8_lossy(&bytes).into_owned();
            match column.column_type() {
                ColumnType::MYSQL_TYPE_TINY | ColumnType::MYSQL_TYPE_SHORT | ColumnType::MYSQL_TYPE_LONG | ColumnType::MYSQL_TYPE_INT24 | ColumnType::MYSQL_TYPE_LONGLONG | ColumnType::MYSQL_TYPE_YEAR => {
                    // Unsigned BIGINT may not fit an i64: keep it exact.
                    text().parse::<i64>().map(Value::Int).unwrap_or_else(|_| Value::Num(text()))
                }
                ColumnType::MYSQL_TYPE_FLOAT | ColumnType::MYSQL_TYPE_DOUBLE => text().parse::<f64>().map(Value::Real).unwrap_or_else(|_| Value::Text(text())),
                ColumnType::MYSQL_TYPE_DECIMAL | ColumnType::MYSQL_TYPE_NEWDECIMAL => Value::Num(text()),
                ColumnType::MYSQL_TYPE_BIT => Value::Int(bytes.iter().fold(0u64, |n, b| (n << 8) | *b as u64) as i64),
                ColumnType::MYSQL_TYPE_TINY_BLOB
                | ColumnType::MYSQL_TYPE_MEDIUM_BLOB
                | ColumnType::MYSQL_TYPE_LONG_BLOB
                | ColumnType::MYSQL_TYPE_BLOB
                | ColumnType::MYSQL_TYPE_STRING
                | ColumnType::MYSQL_TYPE_VAR_STRING
                | ColumnType::MYSQL_TYPE_VARCHAR
                | ColumnType::MYSQL_TYPE_GEOMETRY
                    if column.character_set() == BINARY_CHARSET =>
                {
                    Value::blob(&bytes)
                }
                _ => Value::Text(text()),
            }
        }
        V::Int(i) => Value::Int(i),
        V::UInt(u) => i64::try_from(u).map(Value::Int).unwrap_or_else(|_| Value::Num(u.to_string())),
        V::Float(f) => Value::Real(f as f64),
        V::Double(d) => Value::Real(d),
        other => Value::Text(other.as_sql(true).trim_matches('\'').to_string()),
    }
}

impl Driver for My {
    fn schema(&mut self) -> Result<Schema, String> {
        // Without a database in the connection, every user database is listed.
        const WHERE: &str = "TABLE_SCHEMA NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys') AND (DATABASE() IS NULL OR TABLE_SCHEMA = DATABASE())";
        let qualify: bool = self.conn.query_first::<Option<String>, _>("SELECT DATABASE()").map_err(my_err)?.flatten().is_none();
        let tables: Vec<(String, String, String)> =
            self.conn.query(format!("SELECT TABLE_SCHEMA, TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES WHERE {WHERE} ORDER BY TABLE_SCHEMA, TABLE_NAME")).map_err(my_err)?;
        let columns: Vec<(String, String, String, String, String, String)> = self
            .conn
            .query(format!("SELECT TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY FROM information_schema.COLUMNS WHERE {WHERE} ORDER BY TABLE_SCHEMA, TABLE_NAME, ORDINAL_POSITION"))
            .map_err(my_err)?;
        let mut by_table: HashMap<(String, String), Vec<Column>> = HashMap::new();
        for (schema, table, name, type_name, nullable, key) in columns {
            by_table.entry((schema, table)).or_default().push(Column { name, type_name, not_null: nullable == "NO", primary_key: key == "PRI" });
        }
        let mut schema = Schema::default();
        for (s, name, kind) in tables {
            let columns = by_table.remove(&(s.clone(), name.clone())).unwrap_or_default();
            let table = Table { schema: qualify.then_some(s), name, columns };
            if kind == "VIEW" { schema.views.push(table) } else { schema.tables.push(table) }
        }
        Ok(schema)
    }

    fn run(&mut self, sql: &str, limit: usize) -> Result<StatementResult, String> {
        let mut result = self.conn.query_iter(sql).map_err(my_err)?;
        let columns: Vec<mysql::Column> = result.columns().as_ref().to_vec();
        if columns.is_empty() {
            let affected = result.affected_rows();
            return Ok(StatementResult::Done { affected });
        }
        let names = columns.iter().map(|c| c.name_str().into_owned()).collect();
        let mut rows = Vec::new();
        let mut truncated = false;
        if let Some(set) = result.iter() {
            for row in set {
                let row = row.map_err(my_err)?;
                if rows.len() == limit {
                    truncated = true;
                    break;
                }
                rows.push(row.unwrap().into_iter().zip(&columns).map(|(v, c)| cell(v, c)).collect());
            }
        }
        // Dropping the result reads what is left of it.
        drop(result);
        Ok(StatementResult::Rows(ResultSet { columns: names, rows, truncated }))
    }

    fn count_rows(&mut self, schema: Option<&str>, table: &str) -> Result<RowCount, String> {
        // InnoDB keeps an estimate; counting a huge table takes long.
        let estimate: Option<Option<u64>> = self
            .conn
            .exec_first("SELECT TABLE_ROWS FROM information_schema.TABLES WHERE TABLE_SCHEMA = COALESCE(?, DATABASE()) AND TABLE_NAME = ?", (schema, table))
            .map_err(my_err)?;
        if let Some(Some(n)) = estimate {
            if n >= EXACT_COUNT_BELOW {
                return Ok(RowCount { rows: n, exact: false });
            }
        }
        let rows: Option<u64> = self.conn.query_first(format!("SELECT COUNT(*) FROM {}", self.quote_table(schema, table))).map_err(my_err)?;
        Ok(RowCount { rows: rows.unwrap_or(0), exact: true })
    }

    fn quote_ident(&self, name: &str) -> String {
        format!("`{}`", name.replace('`', "``"))
    }

    fn version(&mut self) -> Result<String, String> {
        let v: Option<String> = self.conn.query_first("SELECT VERSION()").map_err(my_err)?;
        let v = v.unwrap_or_default();
        // "11.4.10-MariaDB" / "8.4.3"
        Ok(match v.split_once('-') {
            Some((number, rest)) if rest.starts_with("MariaDB") => format!("MariaDB {number}"),
            _ => format!("MySQL {v}"),
        })
    }
}
