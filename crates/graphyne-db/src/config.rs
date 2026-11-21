//! Saved connections of a project (`databases.json` in Graphyne's state folder for it).
//! Passwords are never written here: they live in the Windows Credential Manager
//! ([`crate::secrets`]), under the connection's `secret` key.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// File extensions opened as SQLite databases.
pub const SQLITE_EXTENSIONS: &[&str] = &["db", "sqlite", "sqlite3", "db3", "s3db", "sl3"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DbConfig {
    /// Stable key (consoles and tabs refer to it).
    pub id: String,
    /// Shown in the Database panel.
    pub name: String,
    #[serde(flatten)]
    pub kind: DbKind,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "driver", rename_all = "lowercase")]
pub enum DbKind {
    Sqlite { path: PathBuf },
    Postgres(ServerConfig),
    Mysql(ServerConfig),
}

/// Where a database server is and who connects.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// Empty: the server's default (PostgreSQL: the user's database; MySQL: none, all
    /// databases are shown).
    pub database: String,
    pub user: String,
    pub ssl: SslMode,
    /// Credential Manager key of the password; `None` when there is no password.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

/// Encryption of the connection, as in libpq's `sslmode`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SslMode {
    /// Plain connection.
    Disable,
    /// Encrypt when the server can; certificates are not checked.
    #[default]
    Prefer,
    /// Always encrypt; certificates are not checked.
    Require,
    /// Always encrypt and check the certificate and host name.
    VerifyFull,
}

impl SslMode {
    pub const ALL: [SslMode; 4] = [SslMode::Prefer, SslMode::Require, SslMode::VerifyFull, SslMode::Disable];

    pub fn label(self) -> &'static str {
        match self {
            SslMode::Disable => "Off",
            SslMode::Prefer => "Prefer",
            SslMode::Require => "Require",
            SslMode::VerifyFull => "Verify",
        }
    }
}

impl DbKind {
    /// "SQLite", "PostgreSQL", "MySQL".
    pub fn engine(&self) -> &'static str {
        match self {
            DbKind::Sqlite { .. } => "SQLite",
            DbKind::Postgres(_) => "PostgreSQL",
            DbKind::Mysql(_) => "MySQL",
        }
    }

    pub fn server(&self) -> Option<&ServerConfig> {
        match self {
            DbKind::Sqlite { .. } => None,
            DbKind::Postgres(s) | DbKind::Mysql(s) => Some(s),
        }
    }

    pub fn default_port(engine_is_mysql: bool) -> u16 {
        if engine_is_mysql { 3306 } else { 5432 }
    }

    /// A name quoted for this engine's SQL (`"a"` / `` `a` ``).
    pub fn quote_ident(&self, name: &str) -> String {
        match self {
            DbKind::Mysql(_) => format!("`{}`", name.replace('`', "``")),
            _ => format!("\"{}\"", name.replace('"', "\"\"")),
        }
    }

    /// `schema.table` quoted for this engine (just the table in the default schema).
    pub fn quote_table(&self, schema: Option<&str>, table: &str) -> String {
        match schema {
            Some(s) => format!("{}.{}", self.quote_ident(s), self.quote_ident(table)),
            None => self.quote_ident(table),
        }
    }
}

impl DbConfig {
    /// A connection to the SQLite file `path`, named after it, with an id not in `existing`.
    pub fn sqlite(path: &Path, existing: &[DbConfig]) -> DbConfig {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "database".into());
        let stem = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        DbConfig { id: unique_id(&stem, existing), name, kind: DbKind::Sqlite { path: path.to_path_buf() } }
    }

    /// A server connection; `name` empty → `database@host`.
    pub fn server(name: &str, kind: DbKind, existing: &[DbConfig]) -> DbConfig {
        let server = kind.server().cloned().unwrap_or_default();
        let fallback = if server.database.is_empty() { server.host.clone() } else { format!("{}@{}", server.database, server.host) };
        let name = if name.trim().is_empty() { fallback } else { name.trim().to_string() };
        DbConfig { id: unique_id(&name, existing), name, kind }
    }

    /// The database file, for file-based engines.
    pub fn file(&self) -> Option<&Path> {
        match &self.kind {
            DbKind::Sqlite { path } => Some(path),
            _ => None,
        }
    }

    /// One line under the name: where the database is.
    pub fn location(&self) -> String {
        match &self.kind {
            DbKind::Sqlite { path } => path.display().to_string(),
            DbKind::Postgres(s) | DbKind::Mysql(s) => {
                let user = if s.user.is_empty() { String::new() } else { format!("{}@", s.user) };
                let db = if s.database.is_empty() { String::new() } else { format!("/{}", s.database) };
                format!("{user}{}:{}{db}", s.host, s.port)
            }
        }
    }
}

/// `shop`, then `shop-2`, `shop-3`… — lowercase letters, digits and `-` only.
pub(crate) fn unique_id(base: &str, existing: &[DbConfig]) -> String {
    let mut slug: String = base.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        slug = "db".into();
    }
    let taken = |id: &str| existing.iter().any(|c| c.id == id);
    if !taken(&slug) {
        return slug;
    }
    (2..).map(|n| format!("{slug}-{n}")).find(|id| !taken(id)).unwrap()
}

#[derive(Serialize, Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    connections: Vec<DbConfig>,
}

pub fn load_configs(file: &Path) -> Vec<DbConfig> {
    let Ok(text) = std::fs::read_to_string(file) else { return Vec::new() };
    match serde_json::from_str::<ConfigFile>(&text) {
        Ok(f) => f.connections,
        Err(e) => {
            log::warn!("{}: {e}", file.display());
            Vec::new()
        }
    }
}

pub fn save_configs(file: &Path, connections: &[DbConfig]) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(&ConfigFile { connections: connections.to_vec() }).map_err(std::io::Error::other)?;
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, file)
}

/// Whether `path` is an SQLite database: its header says so, or it is an empty file with a
/// database extension (SQLite treats an empty file as an empty database).
pub fn is_sqlite_file(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let mut header = [0u8; 16];
    match file.read(&mut header) {
        Ok(16) => &header == b"SQLite format 3\0",
        Ok(0) => path.extension().and_then(|e| e.to_str()).is_some_and(|e| SQLITE_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x))),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_round_trip() {
        let a = DbConfig::sqlite(Path::new("C:/p/Shop Data.sqlite"), &[]);
        assert_eq!((a.id.as_str(), a.name.as_str()), ("shop-data", "Shop Data.sqlite"));
        let b = DbConfig::sqlite(Path::new("C:/q/shop data.db"), std::slice::from_ref(&a));
        assert_eq!(b.id, "shop-data-2");
        let server = ServerConfig { host: "db.example.com".into(), port: 5432, database: "shop".into(), user: "app".into(), ssl: SslMode::Require, secret: Some("db/x".into()) };
        let c = DbConfig::server("", DbKind::Postgres(server.clone()), &[a.clone(), b.clone()]);
        assert_eq!((c.name.as_str(), c.id.as_str()), ("shop@db.example.com", "shop-db-example-com"));
        assert_eq!(c.location(), "app@db.example.com:5432/shop");
        let d = DbConfig::server("Prod", DbKind::Mysql(ServerConfig { database: String::new(), secret: None, ..server }), &[]);
        assert_eq!(d.kind.quote_table(Some("shop"), "my`table"), "`shop`.`my``table`");
        assert_eq!(c.kind.quote_table(None, "a\"b"), "\"a\"\"b\"");

        let file = std::env::temp_dir().join(format!("graphyne-dbconf-{}", std::process::id())).join("databases.json");
        save_configs(&file, &[a.clone(), b.clone(), c.clone(), d.clone()]).unwrap();
        assert_eq!(load_configs(&file), vec![a, b, c, d]);
        let json = std::fs::read_to_string(&file).unwrap();
        assert!(json.contains("\"driver\": \"sqlite\"") && json.contains("\"driver\": \"postgres\"") && json.contains("\"ssl\": \"require\""));
        assert!(!json.contains("password"));
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
        assert!(load_configs(Path::new("Z:/nowhere/databases.json")).is_empty());
    }
}
