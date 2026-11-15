//! Material Icon Theme resolver (same rules as the VS Code theme):
//! exact file name → longest matching extension (`test.ts` before `ts`) → default.
//! Folders: by name (closed / expanded variants); the project root has its own icon.

use crate::file_icons_data::*;
use gpui::prelude::*;
use gpui::{Img, img, px};
use std::path::Path;

fn lookup<'a>(table: &'a [(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    table.binary_search_by(|(k, _)| (*k).cmp(key)).ok().map(|i| table[i].1)
}

/// Icon definition name for a file name.
pub fn for_file(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if let Some(icon) = lookup(FILE_NAMES, &lower) {
        return icon;
    }
    // Compound extensions first: for `a.test.ts` try `test.ts`, then `ts`.
    let mut rest = lower.as_str();
    while let Some(dot) = rest.find('.') {
        rest = &rest[dot + 1..];
        if let Some(icon) = lookup(FILE_EXTENSIONS, rest) {
            return icon;
        }
    }
    DEFAULT_FILE
}

/// Folder icons are hidden in the Fleet-style tree; kept for an optional setting.
#[allow(dead_code)]
pub fn for_folder(name: &str, expanded: bool, root: bool) -> &'static str {
    if root {
        return if expanded { DEFAULT_ROOT_FOLDER_EXPANDED } else { DEFAULT_ROOT_FOLDER };
    }
    let lower = name.to_lowercase();
    match FOLDER_NAMES.binary_search_by(|(k, _, _)| (*k).cmp(lower.as_str())) {
        Ok(i) => if expanded { FOLDER_NAMES[i].2 } else { FOLDER_NAMES[i].1 },
        Err(_) => if expanded { DEFAULT_FOLDER_EXPANDED } else { DEFAULT_FOLDER },
    }
}

/// SVG bytes for an asset path `file-icons/<definition>.svg`.
pub fn load(path: &str) -> Option<&'static [u8]> {
    let name = path.strip_prefix("file-icons/")?.strip_suffix(".svg")?;
    ICONS.binary_search_by(|(k, _)| (*k).cmp(name)).ok().map(|i| ICONS[i].1)
}

fn asset(def: &str) -> String {
    format!("file-icons/{def}.svg")
}

/// Full-colour icon for a file path.
pub fn file_icon(path: &Path) -> Img {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    img(asset(for_file(&name))).flex_none().size(px(16.))
}

#[allow(dead_code)]
pub fn folder_icon(name: &str, expanded: bool, root: bool) -> Img {
    img(asset(for_folder(name, expanded, root))).flex_none().size(px(16.))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_like_vscode() {
        assert_eq!(for_file("main.rs"), "rust");
        assert_eq!(for_file("App.TSX"), "react_ts");
        assert_eq!(for_file("schema.sql"), "database");
        assert_eq!(for_file("firmware.bin"), "hex");
        assert_eq!(for_file(".gitignore"), "git");
        assert_eq!(for_file("README.md"), "readme");
        assert_eq!(for_file("no_extension_at_all"), DEFAULT_FILE);
        assert_eq!(for_folder("src", false, false), "folder-src");
        assert_eq!(for_folder("SRC", true, false), "folder-src-open");
        assert_eq!(for_folder("whatever", false, false), DEFAULT_FOLDER);
        assert!(load("file-icons/rust.svg").is_some());
        assert!(load("file-icons/nope.svg").is_none());
        // Tables must be sorted for binary search.
        assert!(FILE_EXTENSIONS.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(FILE_NAMES.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(FOLDER_NAMES.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(ICONS.windows(2).all(|w| w[0].0 < w[1].0));
    }
}
