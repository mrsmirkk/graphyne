//! Graphyne's own settings — fonts, autosave, terminal shell, startup — kept in
//! `<state dir>/settings.json` (see [`crate::session::state_dir`]) and edited in the
//! Settings tab. Native, not Lua: plugins add themes and languages, not core options.

use serde::{Deserialize, Deserializer, Serialize};
use std::ops::RangeInclusive;
use std::path::Path;

pub const FONT_SIZES: RangeInclusive<f32> = 9.0..=32.0;
pub const AUTOSAVE_DELAYS_MS: RangeInclusive<u64> = 100..=10_000;
pub const DEFAULT_CODE_FONT: &str = "JetBrains Mono";
pub const DEFAULT_UI_FONT: &str = "Inter";
pub const DEFAULT_THEME: &str = "Graphyne Dark";

/// A font name; `null` or a missing value means the default (filled in by `sanitized`).
fn font_name<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// "Graphyne Dark", "Graphyne Light" (built in) or a theme added by a plugin.
    #[serde(rename = "ui.theme", deserialize_with = "font_name")]
    pub theme: String,
    #[serde(rename = "editor.font_family", deserialize_with = "font_name")]
    pub editor_font: String,
    #[serde(rename = "editor.font_size")]
    pub editor_font_size: f32,
    /// `N usages` lines above functions and types.
    #[serde(rename = "editor.usage_hints")]
    pub usage_hints: bool,
    /// Type and parameter-name hints from the language server.
    #[serde(rename = "editor.inlay_hints")]
    pub inlay_hints: bool,
    #[serde(rename = "editor.autosave")]
    pub autosave: bool,
    #[serde(rename = "editor.autosave_delay_ms")]
    pub autosave_delay_ms: u64,
    #[serde(rename = "terminal.font_family", deserialize_with = "font_name")]
    pub terminal_font: String,
    #[serde(rename = "terminal.font_size")]
    pub terminal_font_size: f32,
    /// Font of menus, panels and tabs.
    #[serde(rename = "ui.font_family", deserialize_with = "font_name")]
    pub ui_font: String,
    /// "auto" or a shell id from `graphyne_terminal::available_shells`.
    #[serde(rename = "terminal.shell")]
    pub shell: String,
    /// Prepare Python projects for Run and Debug: `.venv` with debugpy, created when a
    /// Python file is added or run.
    #[serde(rename = "python.auto_environment")]
    pub python_auto_env: bool,
    /// Install Python packages and create environments with uv (downloaded the first time if
    /// it isn't installed); off: pip only.
    #[serde(rename = "python.use_uv")]
    pub python_uv: bool,
    /// Reopen the last session's projects on launch (off: always the Welcome screen).
    #[serde(rename = "startup.reopen_projects")]
    pub reopen_projects: bool,
    /// Shortcuts changed in Settings → Keymap: command id → its keystrokes ("ctrl-shift-k";
    /// an empty list leaves the command without one). Commands not listed keep their defaults.
    pub keymap: std::collections::BTreeMap<String, Vec<String>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: DEFAULT_THEME.into(),
            editor_font: DEFAULT_CODE_FONT.into(),
            editor_font_size: 13.5,
            usage_hints: true,
            inlay_hints: true,
            autosave: true,
            autosave_delay_ms: 500,
            terminal_font: DEFAULT_CODE_FONT.into(),
            terminal_font_size: 13.0,
            ui_font: DEFAULT_UI_FONT.into(),
            shell: "auto".into(),
            reopen_projects: true,
            python_auto_env: true,
            python_uv: true,
            keymap: Default::default(),
        }
    }
}

impl Settings {
    /// Values a hand-edited file may have pushed out of range are pulled back in.
    pub fn sanitized(mut self) -> Self {
        let size = |v: f32, default: f32| if v.is_finite() { (v * 2.).round() / 2. } else { default }.clamp(*FONT_SIZES.start(), *FONT_SIZES.end());
        self.editor_font_size = size(self.editor_font_size, 13.5);
        self.terminal_font_size = size(self.terminal_font_size, 13.0);
        self.autosave_delay_ms = self.autosave_delay_ms.clamp(*AUTOSAVE_DELAYS_MS.start(), *AUTOSAVE_DELAYS_MS.end());
        let font = |f: String, default: &str| if f.trim().is_empty() { default.to_string() } else { f.trim().to_string() };
        self.editor_font = font(self.editor_font, DEFAULT_CODE_FONT);
        self.terminal_font = font(self.terminal_font, DEFAULT_CODE_FONT);
        self.ui_font = font(self.ui_font, DEFAULT_UI_FONT);
        // "Fleet Dark" is the built-in dark theme's old name.
        self.theme = if self.theme.trim() == "Fleet Dark" { DEFAULT_THEME.to_string() } else { font(self.theme, DEFAULT_THEME) };
        if self.shell.trim().is_empty() {
            self.shell = "auto".into();
        }
        self
    }
}

pub fn load_from(dir: &Path) -> Settings {
    let path = dir.join("settings.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return Settings::default() };
    match serde_json::from_str::<Settings>(&text) {
        Ok(s) => s.sanitized(),
        Err(e) => {
            log::warn!("{}: {e}", path.display());
            Settings::default()
        }
    }
}

pub fn save_to(dir: &Path, settings: &Settings) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    crate::fs_util::atomic_write(&dir.join("settings.json"), serde_json::to_string_pretty(settings)?.as_bytes())
}

pub fn load() -> Settings {
    crate::session::state_dir().map(|d| load_from(&d)).unwrap_or_default()
}

pub fn save(settings: &Settings) -> anyhow::Result<()> {
    match crate::session::state_dir() {
        Some(dir) => save_to(&dir, settings),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_sanitize() {
        let dir = std::env::temp_dir().join(format!("graphyne-settings-test-{}", std::process::id()));
        assert_eq!(load_from(&dir), Settings::default());
        let s = Settings { editor_font: "Consolas".into(), ui_font: "Segoe UI".into(), editor_font_size: 16., autosave: false, shell: "cmd".into(), ..Default::default() };
        save_to(&dir, &s).unwrap();
        assert_eq!(load_from(&dir), s);
        // Hand edits: unknown keys ignored, missing keys default, out-of-range values clamped.
        std::fs::write(dir.join("settings.json"), r#"{ "editor.font_size": 100, "editor.autosave_delay_ms": 5, "x": 1 }"#).unwrap();
        let loaded = load_from(&dir);
        assert_eq!((loaded.editor_font_size, loaded.autosave_delay_ms, loaded.usage_hints), (32., 100, true));
        // The first version wrote `"editor.font_family": null` for "automatic".
        std::fs::write(dir.join("settings.json"), r#"{ "editor.font_family": null, "ui.font_family": " " }"#).unwrap();
        let loaded = load_from(&dir);
        assert_eq!((loaded.editor_font.as_str(), loaded.terminal_font.as_str(), loaded.ui_font.as_str()), (DEFAULT_CODE_FONT, DEFAULT_CODE_FONT, DEFAULT_UI_FONT));
        assert_eq!(loaded.theme, DEFAULT_THEME);
        std::fs::write(dir.join("settings.json"), r#"{ "ui.theme": "Fleet Dark" }"#).unwrap();
        assert_eq!(load_from(&dir).theme, DEFAULT_THEME);
        std::fs::remove_dir_all(&dir).ok();
    }
}
