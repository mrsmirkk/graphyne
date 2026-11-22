//! Installing an extension from a .zip or a folder (Plugins page → "Install from file…"): the
//! package is unpacked and checked (it needs an `init.lua`), the user sees what it is and which
//! permissions it asks for, and only then it is copied into the plugins folder. Blocking —
//! background threads only.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// An extension about to be installed.
#[derive(Clone, Debug)]
pub struct Package {
    /// Its folder name in `plugins/` (the key plugins.json uses).
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    /// From plugin.json "permissions".
    pub requested: Vec<String>,
    /// The folder with init.lua (inside `staging` for a zip).
    pub root: PathBuf,
    /// The unpacked zip, removed after installing or cancelling.
    pub staging: Option<PathBuf>,
    /// Already installed under the same id: its version (an update).
    pub replaces: Option<Option<String>>,
}

/// "Nord Theme" → "nord-theme": the folder name of an extension.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "plugin".into() } else { out }
}

/// The folder of the package that has init.lua: the top, or the only folder that has one
/// (a zip of a folder, a GitHub download `name-main/`).
fn find_root(dir: &Path) -> Result<PathBuf, String> {
    if dir.join("init.lua").is_file() {
        return Ok(dir.to_path_buf());
    }
    let mut found = Vec::new();
    let mut look = |d: &Path, found: &mut Vec<PathBuf>| {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() && p.join("init.lua").is_file() {
                found.push(p);
            }
        }
    };
    look(dir, &mut found);
    if found.is_empty() {
        // One level deeper (a folder inside a folder).
        let subdirs: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        if subdirs.len() == 1 {
            look(&subdirs[0], &mut found);
        }
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err("there is no init.lua in it — a Graphyne extension is a folder with init.lua (and plugin.json)".into()),
        _ => Err("it holds several extensions; install them one by one".into()),
    }
}

fn manifest(dir: &Path) -> serde_json::Value {
    std::fs::read_to_string(dir.join("plugin.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

static STAGING: AtomicUsize = AtomicUsize::new(0);

/// Unpack (a zip) and read the package at `path` for the confirmation.
pub fn inspect(path: &Path, plugins_dir: &Path) -> Result<Package, String> {
    let (root, staging) = if path.is_dir() {
        if path.starts_with(plugins_dir) {
            return Err("this folder is already in the plugins folder".into());
        }
        (find_root(path)?, None)
    } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
        let staging = std::env::temp_dir().join(format!("graphyne-plugin-{}-{}", std::process::id(), STAGING.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
        // The system's tar (bsdtar) unpacks zips and refuses entries outside the folder.
        let out = std::process::Command::new(graphyne_core::download::system_tool("tar"))
            .arg("-xf")
            .arg(path)
            .arg("-C")
            .arg(&staging)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("tar: {e}"))?;
        if !out.status.success() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(format!("could not unpack it: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        match find_root(&staging) {
            Ok(root) => (root, Some(staging)),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(e);
            }
        }
    } else {
        return Err("pick a .zip file or a folder".into());
    };
    let m = manifest(&root);
    let text = |k: &str| m[k].as_str().map(str::to_string).filter(|s| !s.trim().is_empty());
    // The folder name: from the extension's name, else from the folder / zip it came in
    // (without GitHub's "-main" / "-master").
    let fallback = if staging.as_ref().is_some_and(|s| *s == root) { path.file_stem() } else { root.file_name() };
    let fallback = fallback.map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let fallback = fallback.trim_end_matches("-main").trim_end_matches("-master").to_string();
    let id = slug(&text("name").unwrap_or(fallback));
    let existing = plugins_dir.join(&id);
    let replaces = existing.join("init.lua").is_file().then(|| manifest(&existing)["version"].as_str().map(str::to_string));
    Ok(Package {
        name: text("name").unwrap_or_else(|| id.clone()),
        version: text("version"),
        author: text("author"),
        description: text("description"),
        requested: m["permissions"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        id,
        root,
        staging,
        replaces,
    })
}

/// Copy the package into `plugins/<id>`. An installed version goes to the Recycle Bin first.
pub fn install(pkg: &Package, plugins_dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(plugins_dir).map_err(|e| format!("{}: {e}", plugins_dir.display()))?;
    let dest = plugins_dir.join(&pkg.id);
    if dest.exists() {
        graphyne_core::fs_ops::move_to_trash(&dest).map_err(|e| format!("moving the installed version to the Recycle Bin: {e}"))?;
    }
    let copied = graphyne_core::fs_ops::copy_dir(&pkg.root, &dest);
    discard(pkg);
    copied.map_err(|e| format!("copying into {}: {e}", dest.display()))?;
    Ok(dest)
}

/// Remove the unpacked zip (after installing or cancelling).
pub fn discard(pkg: &Package) {
    if let Some(staging) = &pkg.staging {
        let _ = std::fs::remove_dir_all(staging);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_become_folder_names() {
        assert_eq!(slug("Nord Theme"), "nord-theme");
        assert_eq!(slug("  Zig  "), "zig");
        assert_eq!(slug("C++ Tools!"), "c-tools");
        assert_eq!(slug("Тема"), "plugin");
    }

    #[test]
    fn folders_are_inspected_and_installed() {
        let base = std::env::temp_dir().join(format!("graphyne-install-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("download").join("nord-main");
        std::fs::create_dir_all(src.join("themes")).unwrap();
        std::fs::write(src.join("init.lua"), "-- theme").unwrap();
        std::fs::write(src.join("themes").join("x.json"), "{}").unwrap();
        std::fs::write(src.join("plugin.json"), r#"{ "name": "Nord Theme", "version": "1.1.0", "permissions": ["io"] }"#).unwrap();
        let plugins = base.join("plugins");

        // The download folder holds one extension folder: found one level down.
        let pkg = inspect(&base.join("download"), &plugins).unwrap();
        assert_eq!((pkg.id.as_str(), pkg.version.as_deref(), pkg.replaces.clone()), ("nord-theme", Some("1.1.0"), None));
        assert_eq!(pkg.requested, vec!["io"]);
        let dest = install(&pkg, &plugins).unwrap();
        assert!(dest.join("themes").join("x.json").is_file());
        assert!(src.join("init.lua").is_file(), "the source folder is left as it was");

        // Again: an update of the installed version.
        let again = inspect(&src, &plugins).unwrap();
        assert_eq!(again.replaces, Some(Some("1.1.0".into())));
        assert!(inspect(&dest, &plugins).is_err(), "already in the plugins folder");
        std::fs::remove_file(src.join("init.lua")).unwrap();
        assert!(inspect(&src, &plugins).unwrap_err().contains("init.lua"));
        std::fs::remove_dir_all(&base).ok();
    }
}
