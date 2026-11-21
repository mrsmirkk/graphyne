//! Picking up where you left off: which project windows were open when Graphyne closed,
//! and for each project its layout — open tabs with cursors and scroll positions, panels,
//! expanded folders, window position.
//!
//! Stored in the state folder (`%APPDATA%\Graphyne` on Windows; override with
//! `GRAPHYNE_STATE_DIR`): `session.json` lists the open projects, `projects/<hash>.json`
//! holds one project's layout. Nothing is written into the project itself.
//! Disk I/O — small files, fine on any thread.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where Graphyne keeps its own state (recent projects, session).
pub fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("GRAPHYNE_STATE_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    dirs::config_dir().map(|d| d.join("Graphyne"))
}

/// Project folders that were open when the app last closed, in window order.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Session {
    pub open: Vec<PathBuf>,
}

/// Window placement in logical pixels; `maximized` restores maximized over these bounds.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct WindowState {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TabKind {
    /// A file in the editor (or the hex view, chosen again on open).
    #[default]
    File,
    /// Side-by-side comparison with HEAD.
    Diff,
    /// The Plugins page.
    Plugins,
    /// The Settings page.
    Settings,
    /// The SQL console of a database connection (`db`); `path` is its text.
    DbConsole,
    /// The rows of table `table` of connection `db`.
    DbTable,
}

/// One editor tab. Positions are (line, column) from 0, so they survive outside edits.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct TabState {
    pub kind: TabKind,
    pub path: Option<PathBuf>,
    pub cursor: (usize, usize),
    /// Other end of the selection; equal to `cursor` when nothing is selected.
    pub anchor: (usize, usize),
    /// First visible line (fractional) and horizontal scroll in pixels.
    pub scroll_line: f32,
    pub scroll_x: f32,
    /// First lines of the folded code regions.
    pub folds: Vec<usize>,
    /// Database connection id (console and table tabs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db: Option<String>,
    /// Table name (table tabs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    /// The table's schema, when not the default one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

/// Everything about one project's window worth restoring.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ProjectState {
    pub root: PathBuf,
    pub window: Option<WindowState>,
    pub left: bool,
    pub right: bool,
    pub bottom: bool,
    /// "files", "search", "git", "history"; "structure", "secrets".
    pub left_tab: String,
    pub right_tab: String,
    pub left_width: f32,
    pub right_width: f32,
    pub bottom_height: f32,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    /// Expanded folders of the file tree (besides the project root).
    pub expanded: Vec<PathBuf>,
    /// Expanded nodes of the Database panel: connections, groups, tables.
    pub db_expanded: Vec<String>,
}

impl Default for ProjectState {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            window: None,
            left: true,
            right: false,
            bottom: true,
            left_tab: "files".into(),
            right_tab: "structure".into(),
            left_width: 300.,
            right_width: 300.,
            bottom_height: 260.,
            tabs: Vec::new(),
            active_tab: 0,
            expanded: Vec::new(),
            db_expanded: Vec::new(),
        }
    }
}

pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) { a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()) } else { a == b }
}

/// File name for a project's state: a stable hash of its path (case-insensitive on Windows).
fn project_file(dir: &Path, root: &Path) -> PathBuf {
    let key = root.to_string_lossy();
    let key = if cfg!(windows) { key.to_lowercase() } else { key.into_owned() };
    // FNV-1a: stable across Rust versions, unlike `DefaultHasher`.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let name: String = root.file_name().map(|n| n.to_string_lossy().chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').take(32).collect()).unwrap_or_default();
    dir.join("projects").join(format!("{name}-{h:016x}.json"))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            log::warn!("{}: {e}", path.display());
            None
        }
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::fs_util::atomic_write(path, serde_json::to_string_pretty(value)?.as_bytes())
}

pub fn load_session() -> Session {
    state_dir().and_then(|d| read_json(&d.join("session.json"))).unwrap_or_default()
}

pub fn save_session(session: &Session) -> anyhow::Result<()> {
    let Some(dir) = state_dir() else { return Ok(()) };
    write_json(&dir.join("session.json"), session)
}

/// Graphyne's own data for project `root` that is not layout (database connections, SQL
/// consoles): a folder next to its state file, `projects/<name>-<hash>/`.
pub fn project_data_dir(root: &Path) -> Option<PathBuf> {
    let dir = state_dir()?;
    Some(project_file(&dir, root).with_extension(""))
}

/// The saved layout of `root`, if it was ever open.
pub fn load_project(root: &Path) -> Option<ProjectState> {
    let dir = state_dir()?;
    read_json::<ProjectState>(&project_file(&dir, root)).filter(|s| same_path(&s.root, root))
}

pub fn save_project(state: &ProjectState) -> anyhow::Result<()> {
    let Some(dir) = state_dir() else { return Ok(()) };
    write_json(&project_file(&dir, &state.root), state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_and_project_state_round_trip() {
        let dir = std::env::temp_dir().join(format!("graphyne-session-test-{}", std::process::id()));
        // SAFETY: tests in this module are the only readers of this variable.
        unsafe { std::env::set_var("GRAPHYNE_STATE_DIR", &dir) };
        assert_eq!(load_session(), Session::default());

        let session = Session { open: vec![PathBuf::from("C:/work/app"), PathBuf::from("C:/work/lib")] };
        save_session(&session).unwrap();
        assert_eq!(load_session(), session);

        let root = PathBuf::from("C:/work/app");
        assert!(load_project(&root).is_none());
        let state = ProjectState {
            root: root.clone(),
            window: Some(WindowState { x: 10., y: 20., width: 1200., height: 800., maximized: true }),
            right: true,
            left_tab: "git".into(),
            tabs: vec![
                TabState { path: Some(root.join("main.rs")), cursor: (41, 7), anchor: (40, 0), scroll_line: 30.5, ..Default::default() },
                TabState { kind: TabKind::Plugins, ..Default::default() },
            ],
            active_tab: 1,
            expanded: vec![root.join("src")],
            db_expanded: vec!["c/shop".into(), "g/shop/tables".into()],
            ..Default::default()
        };
        save_project(&state).unwrap();
        assert_eq!(load_project(&root), Some(state.clone()));
        if cfg!(windows) {
            assert_eq!(load_project(Path::new("c:/WORK/app")).map(|s| s.active_tab), Some(1));
        }
        assert!(load_project(Path::new("C:/work/lib")).is_none());

        // Older or hand-edited files: missing fields take their defaults.
        let file = project_file(&dir, Path::new("C:/old"));
        std::fs::write(&file, r#"{ "root": "C:/old", "bottom": false }"#).unwrap();
        let old = load_project(Path::new("C:/old")).unwrap();
        assert!(!old.bottom && old.left && old.left_width == 300.);

        std::fs::remove_dir_all(&dir).ok();
        unsafe { std::env::remove_var("GRAPHYNE_STATE_DIR") };
    }
}
