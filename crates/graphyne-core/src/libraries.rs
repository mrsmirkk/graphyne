//! "External Libraries" of the Files tab: per detected toolchain, the standard library
//! sources and the project's third-party packages (Cargo dependencies, Python
//! site-packages). Spawns processes and touches the disk — background executors only.

use crate::language::LanguageId;
use crate::toolchain::hidden_command;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// One browsable entry: a directory (expandable) or a file (opens in the editor).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

fn run(program: &str, args: &[&str], cwd: &Path) -> Option<String> {
    let out = hidden_command(program).args(args).current_dir(cwd).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn dir_entry(name: impl Into<String>, path: PathBuf) -> Option<LibraryEntry> {
    path.is_dir().then(|| LibraryEntry { name: name.into(), path, is_dir: true })
}

/// Entries for `lang` in the project at `root` (empty when nothing is found).
pub fn entries_for(lang: LanguageId, root: &Path) -> Vec<LibraryEntry> {
    match lang {
        LanguageId::Rust => rust(root),
        LanguageId::Python => python(root),
        LanguageId::Go => run("go", &["env", "GOROOT"], root)
            .and_then(|r| dir_entry("std", PathBuf::from(r.trim()).join("src")))
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

/// `std` sources (rust-src component) + every non-workspace package of `cargo metadata`.
fn rust(root: &Path) -> Vec<LibraryEntry> {
    let mut out = Vec::new();
    if let Some(sysroot) = run("rustc", &["--print", "sysroot"], root) {
        out.extend(dir_entry("std", PathBuf::from(sysroot.trim()).join("lib/rustlib/src/rust/library")));
    }
    if !root.join("Cargo.toml").is_file() {
        return out;
    }
    let Some(json) = run("cargo", &["metadata", "--format-version", "1", "--offline"], root) else { return out };
    let Ok(meta) = serde_json::from_str::<serde_json::Value>(&json) else { return out };
    let mut deps: Vec<LibraryEntry> = meta["packages"]
        .as_array()
        .map(|pkgs| {
            pkgs.iter()
                .filter(|p| !p["source"].is_null())
                .filter_map(|p| {
                    let dir = Path::new(p["manifest_path"].as_str()?).parent()?.to_path_buf();
                    Some(LibraryEntry { name: format!("{} {}", p["name"].as_str()?, p["version"].as_str()?), path: dir, is_dir: true })
                })
                .collect()
        })
        .unwrap_or_default();
    deps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    deps.dedup_by(|a, b| a.path == b.path);
    out.extend(deps);
    out
}

/// The project's virtualenv (`.venv`, `venv`, `env`) if any, else `python` on PATH:
/// its stdlib and site-packages folders.
fn python(root: &Path) -> Vec<LibraryEntry> {
    let venv_python = [".venv", "venv", "env"].iter().map(|d| root.join(d)).find(|d| d.join("pyvenv.cfg").is_file()).map(|venv| {
        if cfg!(windows) { venv.join("Scripts").join("python.exe") } else { venv.join("bin").join("python") }
    });
    let program = venv_python.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| "python".to_string());
    let script = "import sysconfig as s; p = s.get_paths(); print(p['stdlib']); print(p['purelib'])";
    let Some(text) = run(&program, &["-c", script], root) else { return Vec::new() };
    let mut lines = text.lines().map(str::trim);
    let mut out = Vec::new();
    if let Some(stdlib) = lines.next() {
        out.extend(dir_entry("stdlib", PathBuf::from(stdlib)));
    }
    if let Some(site) = lines.next() {
        let label = if venv_python.is_some() { "site-packages (venv)" } else { "site-packages" };
        out.extend(dir_entry(label, PathBuf::from(site)));
    }
    out
}
