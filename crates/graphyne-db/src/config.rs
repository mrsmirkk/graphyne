//! Saved connections of a project (`databases.json` in Graphyne's state folder for it).

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
}

impl DbConfig {
    /// A connection to the SQLite file `path`, named after it, with an id not in `existing`.
    pub fn sqlite(path: &Path, existing: &[DbConfig]) -> DbConfig {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "database".into());
        let stem = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        DbConfig { id: unique_id(&stem, existing), name, kind: DbKind::Sqlite { path: path.to_path_buf() } }
    }

    /// The database file, for file-based engines.
    pub fn file(&self) -> Option<&Path> {
        match &self.kind {
            DbKind::Sqlite { path } => Some(path),
        }
    }

    /// One line under the name: where the database is.
    pub fn location(&self) -> String {
        match &self.kind {
            DbKind::Sqlite { path } => path.display().to_string(),
        }
    }
}

/// `shop`, then `shop-2`, `shop-3`… — lowercase letters, digits and `-` only.
fn unique_id(base: &str, existing: &[DbConfig]) -> String {
    let mut slug: String = base.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
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
        let file = std::env::temp_dir().join(format!("graphyne-dbconf-{}", std::process::id())).join("databases.json");
        save_configs(&file, &[a.clone(), b.clone()]).unwrap();
        assert_eq!(load_configs(&file), vec![a, b]);
        assert!(std::fs::read_to_string(&file).unwrap().contains("\"driver\": \"sqlite\""));
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
        assert!(load_configs(Path::new("Z:/nowhere/databases.json")).is_empty());
    }
}
