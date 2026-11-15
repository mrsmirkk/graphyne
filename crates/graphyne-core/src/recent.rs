//! Recently opened project folders (`<state dir>/recent.txt`, one path per line, newest
//! first; see [`crate::session::state_dir`]). Disk I/O — call from background executors.

use std::path::{Path, PathBuf};

const MAX_RECENT: usize = 15;

fn file() -> Option<PathBuf> {
    crate::session::state_dir().map(|d| d.join("recent.txt"))
}

use crate::session::same_path as same;

/// Recent projects that still exist, newest first.
pub fn load() -> Vec<PathBuf> {
    let Some(f) = file() else { return Vec::new() };
    std::fs::read_to_string(f)
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).filter(|p| p.is_dir()).collect())
        .unwrap_or_default()
}

/// Move `root` to the front of the list.
pub fn push(root: &Path) -> anyhow::Result<()> {
    let mut list = load();
    list.retain(|p| !same(p, root));
    list.insert(0, root.to_path_buf());
    list.truncate(MAX_RECENT);
    write(&list)
}

/// Forget `root` (the × on the Welcome screen).
pub fn remove(root: &Path) -> anyhow::Result<()> {
    let mut list = load();
    list.retain(|p| !same(p, root));
    write(&list)
}

fn write(list: &[PathBuf]) -> anyhow::Result<()> {
    let Some(f) = file() else { return Ok(()) };
    if let Some(dir) = f.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text: String = list.iter().map(|p| format!("{}\n", p.display())).collect();
    crate::fs_util::atomic_write(&f, text.as_bytes())
}
