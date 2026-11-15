//! Project model helpers. Every function here does blocking I/O and must only be
//! called from background executors, never from a render / event handler.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct DirEntryInfo {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

const ALWAYS_HIDDEN: &[&str] = &[".git", ".hg", ".svn", ".DS_Store", "Thumbs.db"];
const HEAVY_DIRS: &[&str] = &["target", "node_modules", ".venv", "venv", "__pycache__", "dist", "build", ".gradle", ".idea"];

/// Sorted listing of one directory: folders first, then files, case-insensitive.
pub fn read_dir_sorted(path: &Path) -> Vec<DirEntryInfo> {
    let Ok(read) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut entries: Vec<DirEntryInfo> = read
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if ALWAYS_HIDDEN.contains(&name.as_str()) {
                return None;
            }
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            Some(DirEntryInfo { path: e.path(), name, is_dir })
        })
        .collect();
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    entries
}

/// `.gitignore`-aware walk of all project files. Returns paths relative to `root`
/// using `/` separators (stable for fuzzy matching on every OS).
pub fn walk_project_files(root: &Path, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .parents(true)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !(ALWAYS_HIDDEN.contains(&name.as_ref()) || (e.depth() > 0 && HEAVY_DIRS.contains(&name.as_ref())))
        })
        .build();
    for entry in walker.flatten() {
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            if let Ok(rel) = entry.path().strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
                if out.len() >= limit {
                    break;
                }
            }
        }
    }
    out.sort();
    out
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub rel_path: String,
    pub line: usize,
    pub column: usize,
    pub preview: String,
}

/// Plain-text find in files (case-insensitive unless the query has uppercase letters).
pub fn find_in_files(root: &Path, query: &str, limit: usize) -> Vec<SearchHit> {
    if query.is_empty() {
        return Vec::new();
    }
    let case_sensitive = query.chars().any(|c| c.is_uppercase());
    let needle = if case_sensitive { query.to_string() } else { query.to_lowercase() };
    let mut hits = Vec::new();
    for rel in walk_project_files(root, 50_000) {
        let path = root.join(&rel);
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.len() > 4 * 1024 * 1024 {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for (line_ix, line) in text.lines().enumerate() {
            let hay = if case_sensitive { line.to_string() } else { line.to_lowercase() };
            if let Some(col) = hay.find(&needle) {
                hits.push(SearchHit {
                    rel_path: rel.clone(),
                    line: line_ix,
                    column: line[..col.min(line.len())].chars().count(),
                    preview: line.trim().chars().take(160).collect(),
                });
                if hits.len() >= limit {
                    return hits;
                }
            }
        }
    }
    hits
}

/// Display name for a project root.
pub fn project_name(root: &Path) -> String {
    root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| root.to_string_lossy().to_string())
}
