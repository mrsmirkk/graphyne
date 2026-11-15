//! Recently opened project folders (`<config dir>/Graphyne/recent.txt`, one path per line,
//! newest first). Disk I/O — call from background executors.

use std::path::{Path, PathBuf};

const MAX_RECENT: usize = 15;

fn file() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("Graphyne").join("recent.txt"))
}

fn same(a: &Path, b: &Path) -> bool {
    if cfg!(windows) { a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()) } else { a == b }
}

/// Recent projects that still exist, newest first.
pub fn load() -> Vec<PathBuf> {
    let Some(f) = file() else { return Vec::new() };
    std::fs::read_to_string(f)
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).filter(|p| p.is_dir()).collect())
        .unwrap_or_default()
}

/// Move `root` to the front of the list.
pub fn push(root: &Path) -> anyhow::Result<()> {
    let Some(f) = file() else { return Ok(()) };
    let mut list = load();
    list.retain(|p| !same(p, root));
    list.insert(0, root.to_path_buf());
    list.truncate(MAX_RECENT);
    if let Some(dir) = f.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text: String = list.iter().map(|p| format!("{}\n", p.display())).collect();
    crate::fs_util::atomic_write(&f, text.as_bytes())
}
