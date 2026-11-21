//! An open connection on a thread of its own. Requests are queued there and answered on
//! channels (await them with `recv_async`), so the UI never waits on the database.

use crate::config::{DbConfig, DbKind};
use crate::{Driver, QueryOutcome, ResultSet, RowCount, Schema, StatementResult, split};
use std::sync::Arc;
use std::time::Instant;

type Job = Box<dyn FnOnce(&mut dyn Driver) + Send>;

pub struct DbSession {
    jobs: flume::Sender<Job>,
    interrupt: Arc<dyn Fn() + Send + Sync>,
}

impl DbSession {
    /// Connect (blocking until the connection is open or has failed), with the password
    /// saved for it. The thread ends when the session is dropped.
    pub fn open(config: &DbConfig) -> Result<DbSession, String> {
        let password = match config.kind.server().and_then(|s| s.secret.as_deref()) {
            Some(key) => crate::secrets::get_password(key)?,
            None => None,
        };
        Self::open_with(config, password)
    }

    /// Connect with this `password` (the connection form's Test, before anything is saved).
    pub fn open_with(config: &DbConfig, password: Option<String>) -> Result<DbSession, String> {
        let (jobs, queue) = flume::unbounded::<Job>();
        let (ready_tx, ready) = flume::bounded(1);
        let kind = config.kind.clone();
        std::thread::Builder::new()
            .name(format!("db: {}", config.name))
            .spawn(move || {
                let opened = match &kind {
                    DbKind::Sqlite { path } => crate::sqlite::open(path).map(|(d, i)| (Box::new(d) as Box<dyn Driver>, i)),
                    DbKind::Postgres(server) => crate::pg::open(server, password).map(|(d, i)| (Box::new(d) as Box<dyn Driver>, i)),
                    DbKind::Mysql(server) => crate::mysql_driver::open(server, password).map(|(d, i)| (Box::new(d) as Box<dyn Driver>, i)),
                };
                match opened {
                    Err(e) => drop(ready_tx.send(Err(e))),
                    Ok((mut driver, interrupt)) => {
                        let _ = ready_tx.send(Ok(interrupt));
                        while let Ok(job) = queue.recv() {
                            job(driver.as_mut());
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let interrupt = ready.recv().map_err(|_| "the connection thread stopped".to_string())??;
        Ok(DbSession { jobs, interrupt })
    }

    fn call<T: Send + 'static>(&self, f: impl FnOnce(&mut dyn Driver) -> T + Send + 'static) -> flume::Receiver<T> {
        let (tx, rx) = flume::bounded(1);
        // If the thread is gone, `rx` reports a disconnect to the waiting side.
        let _ = self.jobs.send(Box::new(move |driver: &mut dyn Driver| drop(tx.send(f(driver)))));
        rx
    }

    pub fn schema(&self) -> flume::Receiver<Result<Schema, String>> {
        self.call(|d| d.schema())
    }

    /// Run a script statement by statement, stopping at the first error.
    pub fn run_script(&self, sql: String, limit: usize) -> flume::Receiver<Vec<QueryOutcome>> {
        self.call(move |d| {
            let mut out = Vec::new();
            for range in split::statements(&sql) {
                let statement = sql[range].to_string();
                let started = Instant::now();
                let result = d.run(&statement, limit);
                let failed = result.is_err();
                out.push(QueryOutcome { sql: statement, result, elapsed: started.elapsed() });
                if failed {
                    break;
                }
            }
            out
        })
    }

    /// The server's name and version ("PostgreSQL 17.10").
    pub fn version(&self) -> flume::Receiver<Result<String, String>> {
        self.call(|d| d.version())
    }

    /// A table's rows (at most `limit`) and how many it has in total.
    pub fn table_rows(&self, schema: Option<String>, table: String, limit: usize) -> flume::Receiver<(Result<ResultSet, String>, Option<RowCount>)> {
        self.call(move |d| {
            // LIMIT keeps a huge table from being read to the end.
            let sql = format!("SELECT * FROM {} LIMIT {}", d.quote_table(schema.as_deref(), &table), limit + 1);
            let rows = d.run(&sql, limit).map(|r| match r {
                StatementResult::Rows(rows) => rows,
                StatementResult::Done { .. } => ResultSet::default(),
            });
            let total = d.count_rows(schema.as_deref(), &table).ok();
            (rows, total)
        })
    }

    /// Stop the statement that is running now; it ends with an "interrupted" error.
    pub fn interrupt(&self) {
        (self.interrupt)()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DbKind, Value};

    /// A server for the integration tests from `var` = `host:port:user:password`; they are
    /// skipped when it is not set (no servers on a normal build machine).
    fn server(var: &str, mysql: bool) -> Option<(DbConfig, String)> {
        let spec = std::env::var(var).ok()?;
        let mut parts = spec.splitn(4, ':');
        let (host, port, user, password) = (parts.next()?, parts.next()?.parse().ok()?, parts.next()?, parts.next().unwrap_or(""));
        let server = crate::ServerConfig { host: host.into(), port, user: user.into(), ..Default::default() };
        let kind = if mysql { DbKind::Mysql(server) } else { DbKind::Postgres(server) };
        Some((DbConfig::server("test", kind, &[]), password.to_string()))
    }

    fn rows(outcome: &QueryOutcome) -> &ResultSet {
        match &outcome.result {
            Ok(StatementResult::Rows(r)) => r,
            other => panic!("{}: {other:?}", outcome.sql),
        }
    }

    #[test]
    fn postgres_server() {
        let Some((config, password)) = server("GRAPHYNE_TEST_PG", false) else { return };
        let db = DbSession::open_with(&config, Some(password)).unwrap();
        assert!(db.version().recv().unwrap().unwrap().starts_with("PostgreSQL "));
        let script = "DROP SCHEMA IF EXISTS graphyne_test CASCADE;
            CREATE SCHEMA graphyne_test;
            CREATE TABLE graphyne_test.items (id int PRIMARY KEY, price numeric(10,2), made date, at timestamp, tags text[], meta jsonb, uid uuid, ok boolean, wait interval, ip inet, raw bytea);
            INSERT INTO graphyne_test.items VALUES (1, 12.50, '2026-11-21', '2026-11-21 10:20:30.5', '{a,\"b c\"}', '{\"k\": [1, 2]}', '12345678-9abc-def0-0102-030405060708', true, '1 day 02:00:00', '10.1.2.3', '\\xcafe'), (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
            CREATE FUNCTION graphyne_test.twice(x int) RETURNS int AS $$ BEGIN RETURN x * 2; END; $$ LANGUAGE plpgsql;
            SELECT *, graphyne_test.twice(id) AS twice FROM graphyne_test.items ORDER BY id;";
        let out = db.run_script(script.into(), 100).recv().unwrap();
        let result = rows(out.last().unwrap());
        assert_eq!(result.columns.last().map(String::as_str), Some("twice"));
        let first: Vec<String> = result.rows[0].iter().map(Value::to_text).collect();
        assert_eq!(
            first,
            ["1", "12.50", "2026-11-21", "2026-11-21 10:20:30.5", "{a,\"b c\"}", "{\"k\": [1, 2]}", "12345678-9abc-def0-0102-030405060708", "true", "1 day 02:00:00", "10.1.2.3", "0xCAFE (2 bytes)", "2"]
        );
        assert!(result.rows[1][1..11].iter().all(Value::is_null));
        assert_eq!(result.rows[0][1], Value::Num("12.50".into()));

        let schema = db.schema().recv().unwrap().unwrap();
        let items = schema.tables.iter().find(|t| t.name == "items").expect("items listed");
        assert_eq!(items.schema.as_deref(), Some("graphyne_test"));
        assert!(items.columns[0].primary_key && items.columns[4].type_name == "text[]");
        let (page, total) = db.table_rows(Some("graphyne_test".into()), "items".into(), 1).recv().unwrap();
        assert!(page.unwrap().truncated);
        assert_eq!(total, Some(RowCount { rows: 2, exact: true }));

        let err = db.run_script("SELECT * FROM graphyne_test.missing".into(), 10).recv().unwrap();
        assert!(err[0].result.as_ref().unwrap_err().contains("does not exist"), "{:?}", err[0].result);
        let running = db.run_script("SELECT pg_sleep(30)".into(), 10);
        std::thread::sleep(std::time::Duration::from_millis(300));
        db.interrupt();
        let stopped = running.recv_timeout(std::time::Duration::from_secs(10)).expect("pg_sleep was cancelled");
        assert!(stopped[0].result.as_ref().unwrap_err().contains("cancel"), "{:?}", stopped[0].result);
        db.run_script("DROP SCHEMA graphyne_test CASCADE".into(), 10).recv().unwrap();
    }

    #[test]
    fn mysql_server() {
        let Some((config, password)) = server("GRAPHYNE_TEST_MYSQL", true) else { return };
        let db = DbSession::open_with(&config, Some(password)).unwrap();
        let version = db.version().recv().unwrap().unwrap();
        assert!(version.starts_with("MariaDB ") || version.starts_with("MySQL "), "{version}");
        let script = "DROP DATABASE IF EXISTS graphyne_test;
            CREATE DATABASE graphyne_test;
            CREATE TABLE graphyne_test.items (id INT PRIMARY KEY, price DECIMAL(10,2), big BIGINT UNSIGNED, raw VARBINARY(4), at DATETIME, note TEXT, f DOUBLE);
            INSERT INTO graphyne_test.items VALUES (1, 12.50, 18446744073709551615, x'CAFE', '2026-11-21 10:20:30', 'a;b', 0.25), (2, NULL, NULL, NULL, NULL, NULL, NULL);
            SELECT * FROM graphyne_test.items ORDER BY id;";
        let out = db.run_script(script.into(), 100).recv().unwrap();
        assert_eq!(out[3].result, Ok(StatementResult::Done { affected: 2 }));
        let result = rows(out.last().unwrap());
        assert_eq!(
            result.rows[0],
            vec![Value::Int(1), Value::Num("12.50".into()), Value::Num("18446744073709551615".into()), Value::blob(&[0xCA, 0xFE]), Value::Text("2026-11-21 10:20:30".into()), Value::Text("a;b".into()), Value::Real(0.25)]
        );
        assert!(result.rows[1][1..].iter().all(Value::is_null));

        // Without a database in the connection, tables come with their database.
        let schema = db.schema().recv().unwrap().unwrap();
        let items = schema.tables.iter().find(|t| t.name == "items" && t.schema.as_deref() == Some("graphyne_test")).expect("items listed");
        assert!(items.columns[0].primary_key && items.columns[1].type_name == "decimal(10,2)");
        let (page, total) = db.table_rows(Some("graphyne_test".into()), "items".into(), 1).recv().unwrap();
        assert!(page.unwrap().truncated);
        assert_eq!(total, Some(RowCount { rows: 2, exact: true }));

        let err = db.run_script("SELECT * FROM graphyne_test.missing".into(), 10).recv().unwrap();
        assert!(err[0].result.as_ref().unwrap_err().contains("doesn't exist"), "{:?}", err[0].result);
        let running = db.run_script("SELECT COUNT(*) FROM information_schema.COLUMNS a, information_schema.COLUMNS b, information_schema.COLUMNS c".into(), 10);
        std::thread::sleep(std::time::Duration::from_millis(300));
        db.interrupt();
        let stopped = running.recv_timeout(std::time::Duration::from_secs(10)).expect("the query was killed");
        assert!(stopped[0].result.as_ref().unwrap_err().contains("interrupted"), "{:?}", stopped[0].result);
        db.run_script("DROP DATABASE graphyne_test".into(), 10).recv().unwrap();
    }

    fn temp_db(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("graphyne-db-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.sqlite");
        let _ = std::fs::remove_file(&path);
        crate::create_sqlite(&path).unwrap();
        path
    }

    #[test]
    fn scripts_schema_and_tables() {
        let path = temp_db("script");
        assert!(crate::is_sqlite_file(&path));
        let db = DbSession::open(&DbConfig::sqlite(&path, &[])).unwrap();
        let script = "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL, avatar BLOB);
            INSERT INTO users (name, score, avatar) VALUES ('Ann', 9.5, x'CAFE'), ('Bob', NULL, NULL);
            CREATE VIEW good AS SELECT name FROM users WHERE score > 5;
            SELECT id, name, score, avatar FROM users ORDER BY id;"
            .to_string();
        let out = db.run_script(script, 100).recv().unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(out[1].result, Ok(StatementResult::Done { affected: 2 }));
        let Ok(StatementResult::Rows(rows)) = &out[3].result else { panic!("{:?}", out[3].result) };
        assert_eq!(rows.columns, ["id", "name", "score", "avatar"]);
        assert_eq!(rows.rows[0], vec![Value::Int(1), Value::Text("Ann".into()), Value::Real(9.5), Value::blob(&[0xCA, 0xFE])]);
        assert!(rows.rows[1][2].is_null());

        let schema = db.schema().recv().unwrap().unwrap();
        assert_eq!(schema.tables.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["users"]);
        assert_eq!(schema.views[0].name, "good");
        let id = &schema.tables[0].columns[0];
        assert!(id.primary_key && id.type_name == "INTEGER");
        assert!(schema.tables[0].columns[1].not_null);

        // An error stops the script; earlier statements stay done.
        let out = db.run_script("INSERT INTO users (name) VALUES ('Cy'); SELEC 1; DELETE FROM users;".into(), 100).recv().unwrap();
        assert_eq!(out.len(), 2);
        assert!(out[1].result.as_ref().unwrap_err().contains("syntax error"));

        let (rows, total) = db.table_rows(None, "users".into(), 2).recv().unwrap();
        let rows = rows.unwrap();
        assert_eq!((rows.rows.len(), rows.truncated, total), (2, true, Some(RowCount { rows: 3, exact: true })));
        assert!(db.version().recv().unwrap().unwrap().starts_with("SQLite 3."));
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn missing_files_are_not_created_and_queries_can_be_stopped() {
        let missing = std::env::temp_dir().join("graphyne-db-surely-missing.sqlite");
        let Err(error) = DbSession::open(&DbConfig::sqlite(&missing, &[])) else { panic!("opened a missing file") };
        assert!(error.contains("not found"), "{error}");
        assert!(!missing.exists());

        let path = temp_db("interrupt");
        let db = DbSession::open(&DbConfig::sqlite(&path, &[])).unwrap();
        let endless = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n".to_string();
        let running = db.run_script(endless, 10);
        std::thread::sleep(std::time::Duration::from_millis(200));
        db.interrupt();
        let out = running.recv_timeout(std::time::Duration::from_secs(10)).expect("the query stopped");
        assert!(out[0].result.as_ref().unwrap_err().contains("interrupt"));
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
