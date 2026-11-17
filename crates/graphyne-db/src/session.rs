//! An open connection on a thread of its own. Requests are queued there and answered on
//! channels (await them with `recv_async`), so the UI never waits on the database.

use crate::config::{DbConfig, DbKind};
use crate::{Driver, QueryOutcome, ResultSet, Schema, StatementResult, split};
use std::sync::Arc;
use std::time::Instant;

type Job = Box<dyn FnOnce(&mut dyn Driver) + Send>;

pub struct DbSession {
    jobs: flume::Sender<Job>,
    interrupt: Arc<dyn Fn() + Send + Sync>,
}

impl DbSession {
    /// Connect (blocking until the connection is open or has failed). The thread ends when
    /// the session is dropped.
    pub fn open(config: &DbConfig) -> Result<DbSession, String> {
        let (jobs, queue) = flume::unbounded::<Job>();
        let (ready_tx, ready) = flume::bounded(1);
        let kind = config.kind.clone();
        std::thread::Builder::new()
            .name(format!("db: {}", config.name))
            .spawn(move || {
                let opened = match &kind {
                    DbKind::Sqlite { path } => crate::sqlite::open(path).map(|(d, i)| (Box::new(d) as Box<dyn Driver>, i)),
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

    /// A table's rows (at most `limit`) and how many it has in total.
    pub fn table_rows(&self, table: String, limit: usize) -> flume::Receiver<(Result<ResultSet, String>, Option<u64>)> {
        self.call(move |d| {
            let sql = format!("SELECT * FROM {}", d.quote_ident(&table));
            let rows = d.run(&sql, limit).map(|r| match r {
                StatementResult::Rows(rows) => rows,
                StatementResult::Done { .. } => ResultSet::default(),
            });
            let total = d.count_rows(&table).ok();
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
    use crate::Value;

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

        let (rows, total) = db.table_rows("users".into(), 2).recv().unwrap();
        let rows = rows.unwrap();
        assert_eq!((rows.rows.len(), rows.truncated, total), (2, true, Some(3)));
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
