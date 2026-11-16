//! File operations of the project tree: new file / folder, rename, delete to the Recycle Bin.
//! Blocking — call from background executors.

use std::path::{Path, PathBuf};

/// Characters Windows doesn't allow in file names (also refused elsewhere, for portability).
const FORBIDDEN: &[char] = &['<', '>', ':', '"', '|', '?', '*'];
/// Names Windows reserves for devices (`con`, `nul`, `com1`…), with or without extension.
const RESERVED: &[&str] = &["con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9"];

/// Check one path component typed by the user; `Err` says what is wrong.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("Enter a name".into());
    }
    if name == "." || name == ".." {
        return Err("\".\" and \"..\" can't be names".into());
    }
    if let Some(c) = name.chars().find(|c| FORBIDDEN.contains(c) || c.is_control()) {
        return Err(format!("A name can't contain {}", if c.is_control() { "control characters".to_string() } else { format!("\"{c}\"") }));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err("A name can't end with a dot or a space".into());
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        return Err(format!("\"{name}\" is reserved by Windows"));
    }
    Ok(())
}

/// The path a new item named `input` gets in `dir`. `input` may contain `/` or `\` to create
/// nested folders at once (`src/utils/io.py`), like JetBrains IDEs.
pub fn new_item_path(dir: &Path, input: &str) -> Result<PathBuf, String> {
    let parts: Vec<&str> = input.trim().split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err("Enter a name".into());
    }
    for part in &parts {
        validate_name(part)?;
    }
    Ok(parts.iter().fold(dir.to_path_buf(), |p, part| p.join(part)))
}

fn already_exists(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    format!("\"{name}\" already exists")
}

/// Create an empty file (and missing parent folders). Fails if it exists.
pub fn create_file(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Err(already_exists(path));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::OpenOptions::new().write(true).create_new(true).open(path).map(drop).map_err(|e| e.to_string())
}

/// Create a folder (and missing parents). Fails if it exists.
pub fn create_dir(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Err(already_exists(path));
    }
    std::fs::create_dir_all(path).map_err(|e| e.to_string())
}

/// Rename / move `from` to `to`. Fails if `to` is taken — except a change of letter case only
/// (Windows sees `a.py` and `A.py` as the same file).
pub fn rename(from: &Path, to: &Path) -> Result<(), String> {
    let case_only = from.to_string_lossy().to_lowercase() == to.to_string_lossy().to_lowercase();
    if to.exists() && !case_only {
        return Err(already_exists(to));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(from, to).map_err(|e| e.to_string())
}

/// Move a file or folder to the Recycle Bin (recoverable), not a permanent delete.
///
/// Runs on a thread of its own: `trash` sets COM up as a single-threaded apartment and
/// panics when the thread already uses COM in another mode — as the thread-pool threads of
/// GPUI's background executor do. A panic there would abort the whole app.
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    let path = path.to_path_buf();
    std::thread::spawn(move || trash::delete(&path).map_err(|e| e.to_string()))
        .join()
        .unwrap_or_else(|_| Err("the Recycle Bin is not available".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_checked() {
        assert!(validate_name("main.py").is_ok());
        assert!(validate_name(".gitignore").is_ok());
        assert!(validate_name("Найти пару.py").is_ok());
        for bad in ["", "  ", "..", "a:b", "what?", "trailing.", "trailing ", "con", "NUL.txt", "a|b"] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn create_rename_and_nested_paths() {
        let dir = std::env::temp_dir().join(format!("graphyne-fsops-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = new_item_path(&dir, "src/utils\\io.py").unwrap();
        assert_eq!(file, dir.join("src").join("utils").join("io.py"));
        create_file(&file).unwrap();
        assert!(file.is_file());
        assert!(create_file(&file).unwrap_err().contains("already exists"));
        create_dir(&dir.join("docs")).unwrap();
        assert!(create_dir(&dir.join("docs")).is_err());
        // Rename, refuse to overwrite, allow a case-only change.
        let renamed = dir.join("src").join("utils").join("files.py");
        rename(&file, &renamed).unwrap();
        assert!(renamed.is_file() && !file.exists());
        create_file(&file).unwrap();
        assert!(rename(&file, &renamed).is_err());
        rename(&renamed, &dir.join("src").join("utils").join("Files.py")).unwrap();
        assert!(new_item_path(&dir, "ok/bad?name").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
