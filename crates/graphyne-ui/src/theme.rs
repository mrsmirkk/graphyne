//! Graphyne Dark / Graphyne Light palettes, extension themes, fonts and metrics.

use gpui::{Hsla, Pixels, Rgba, px, rgb, rgba};
use graphyne_syntax::HighlightKind;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub mod c {
    //! Raw 0xRRGGBB constants.
    pub const CANVAS: u32 = 0x131416;
    pub const ISLAND: u32 = 0x191B1E;
    pub const BORDER: u32 = 0x26292E;
    pub const ACTIVE_LINE: u32 = 0x21262E;
    pub const POPUP: u32 = 0x1E2125;
    pub const POPUP_BORDER: u32 = 0x31353C;
    pub const HOVER: u32 = 0x25282D;
    pub const PRESSED: u32 = 0x2C3036;
    pub const TAB_ACTIVE: u32 = 0x262A30;
    pub const TAB_FOCUSED: u32 = 0x1B2638;
    pub const TAB_FOCUSED_BORDER: u32 = 0x2E5DAD;
    pub const TEXT: u32 = 0xD4D6DC;
    pub const TEXT_MUTED: u32 = 0x9A9EA7;
    pub const TEXT_DIM: u32 = 0x6A6F78;
    pub const TEXT_FAINT: u32 = 0x4A4E56;
    pub const ACCENT: u32 = 0x3F86F6;
    pub const ACCENT_SOFT: u32 = 0x22385E;
    pub const SELECTION: u32 = 0x2A3E5E;
    pub const SELECTION_INACTIVE: u32 = 0x2A2F38;
    pub const CURSOR: u32 = 0xE6E8EC;
    pub const MATCH: u32 = 0x6AA9FF;
    pub const GREEN: u32 = 0x5FB97C;
    pub const BLUE: u32 = 0x4F8DE8;
    pub const RED: u32 = 0xE5534B;
    pub const YELLOW: u32 = 0xE2B54A;
    pub const ORANGE: u32 = 0xE08A4A;
    pub const GIT_MODIFIED: u32 = 0x6CA2E8;
    pub const GIT_ADDED: u32 = 0x73C991;
    pub const GIT_UNTRACKED: u32 = 0xD68A6E;
    pub const GIT_DELETED: u32 = 0x7D828B;
    pub const CLOSE_HOVER: u32 = 0xC42B1C;
}

/// UI colour tokens an extension theme can set (`graphyne.themes.register{ ui = {…} }`).
pub const UI_TOKENS: &[(&str, u32)] = &[
    ("canvas", c::CANVAS),
    ("island", c::ISLAND),
    ("border", c::BORDER),
    ("active_line", c::ACTIVE_LINE),
    ("popup", c::POPUP),
    ("popup_border", c::POPUP_BORDER),
    ("hover", c::HOVER),
    ("pressed", c::PRESSED),
    ("tab_active", c::TAB_ACTIVE),
    ("tab_focused", c::TAB_FOCUSED),
    ("tab_focused_border", c::TAB_FOCUSED_BORDER),
    ("text", c::TEXT),
    ("text_muted", c::TEXT_MUTED),
    ("text_dim", c::TEXT_DIM),
    ("text_faint", c::TEXT_FAINT),
    ("accent", c::ACCENT),
    ("accent_soft", c::ACCENT_SOFT),
    ("selection", c::SELECTION),
    ("selection_inactive", c::SELECTION_INACTIVE),
    ("cursor", c::CURSOR),
    ("match", c::MATCH),
    ("green", c::GREEN),
    ("blue", c::BLUE),
    ("red", c::RED),
    ("yellow", c::YELLOW),
    ("orange", c::ORANGE),
    ("git_modified", c::GIT_MODIFIED),
    ("git_added", c::GIT_ADDED),
    ("git_untracked", c::GIT_UNTRACKED),
    ("git_deleted", c::GIT_DELETED),
];

/// Syntax tokens (`graphyne.themes.register{ syntax = {…} }`).
pub const SYNTAX_TOKENS: &[(&str, HighlightKind)] = &[
    ("plain", HighlightKind::Plain),
    ("keyword", HighlightKind::Keyword),
    ("string", HighlightKind::String),
    ("escape", HighlightKind::Escape),
    ("number", HighlightKind::Number),
    ("comment", HighlightKind::Comment),
    ("function", HighlightKind::Function),
    ("method", HighlightKind::Method),
    ("macro", HighlightKind::Macro),
    ("type", HighlightKind::Type),
    ("type_builtin", HighlightKind::TypeBuiltin),
    ("constructor", HighlightKind::Constructor),
    ("constant", HighlightKind::Constant),
    ("constant_builtin", HighlightKind::ConstantBuiltin),
    ("variable", HighlightKind::Variable),
    ("variable_builtin", HighlightKind::VariableBuiltin),
    ("parameter", HighlightKind::Parameter),
    ("property", HighlightKind::Property),
    ("attribute", HighlightKind::Attribute),
    ("label", HighlightKind::Label),
    ("operator", HighlightKind::Operator),
    ("punctuation", HighlightKind::Punctuation),
    ("tag", HighlightKind::Tag),
    ("module", HighlightKind::Module),
];

/// The two built-in themes. Graphyne Dark is the palette in [`c`]; other themes map those
/// colours to their own.
pub const GRAPHYNE_DARK: &str = "Graphyne Dark";
pub const GRAPHYNE_LIGHT: &str = "Graphyne Light";
pub const BUILTIN_THEMES: [&str; 2] = [GRAPHYNE_DARK, GRAPHYNE_LIGHT];

/// Graphyne Light: every Graphyne Dark colour (UI tokens and the few colours used directly
/// in views) → its light counterpart.
const LIGHT_UI: &[(u32, u32)] = &[
    (c::CANVAS, 0xE6E8EC),
    (c::ISLAND, 0xFFFFFF),
    (c::BORDER, 0xDDE0E5),
    (c::ACTIVE_LINE, 0xF3F6FB),
    (c::POPUP, 0xFFFFFF),
    (c::POPUP_BORDER, 0xD2D6DC),
    (c::HOVER, 0xEDEFF3),
    (c::PRESSED, 0xE1E4E9),
    (c::TAB_ACTIVE, 0xE9ECF1),
    (c::TAB_FOCUSED, 0xDDE8FC),
    (c::TAB_FOCUSED_BORDER, 0x7EA6F0),
    (c::TEXT, 0x1F2328),
    (c::TEXT_MUTED, 0x4E5560),
    (c::TEXT_DIM, 0x767D88),
    (c::TEXT_FAINT, 0xA3A9B2),
    (c::ACCENT, 0x2F6FE4),
    (c::ACCENT_SOFT, 0xD9E6FD),
    (c::SELECTION, 0xC6DAFB),
    (c::SELECTION_INACTIVE, 0xE0E4EA),
    (c::CURSOR, 0x1F2328),
    (c::MATCH, 0x2F6FE4),
    (c::GREEN, 0x2E9E5B),
    (c::BLUE, 0x2F6FE4),
    (c::RED, 0xD43B3B),
    (c::YELLOW, 0xB07D06),
    (c::ORANGE, 0xC4651F),
    (c::GIT_MODIFIED, 0x2F6FE4),
    (c::GIT_ADDED, 0x2E9E5B),
    (c::GIT_UNTRACKED, 0xB35A35),
    (c::GIT_DELETED, 0x8A9099),
    // Colours used directly by views (code blocks, debugger, search badges…).
    (0x16181B, 0xF3F4F6),
    (0xD6D6DD, 0x1F2328),
    (0xE3D7C2, 0x8A4B08),
    (0x7F93B5, 0x51698F),
    (0xEFB080, 0x00627A),
    (0xAA9BF5, 0x871094),
    (0xA8D8B0, 0x1E7B3A),
    (0xE0A3A3, 0xB3261E),
    (0xC9A3A3, 0xB3261E),
    (0xA8CC7C, 0x4F7A1F),
    (0x3A2F52, 0xEFE4FA),
    (0xC5A3F0, 0x7A3EB1),
    (0x2E3446, 0xE1E8F7),
    (0x9BB5E8, 0x2F5DB4),
    (0x5294F7, 0x2F6FE4),
];

/// Graphyne Light syntax colours (after IntelliJ Light).
fn light_syntax(kind: HighlightKind) -> u32 {
    use HighlightKind::*;
    match kind {
        Plain | Variable | Module => 0x1F2328,
        Keyword => 0x0033B3,
        String => 0x067D17,
        Escape => 0x0037A6,
        Number => 0x1750EB,
        Comment => 0x8C8C8C,
        Function | Method => 0x00627A,
        Macro | Attribute => 0x9E880D,
        Type | Constructor | Tag => 0x0E6E8C,
        TypeBuiltin | ConstantBuiltin => 0x0033B3,
        Constant | Property | VariableBuiltin => 0x871094,
        Parameter => 0x3E5D80,
        Label => 0x1750EB,
        Operator => 0x3B3F45,
        Punctuation => 0x5A606A,
    }
}

struct ActiveTheme {
    name: String,
    ui: std::collections::HashMap<u32, u32>,
    syntax: std::collections::HashMap<HighlightKind, u32>,
}

static ACTIVE_THEME: std::sync::RwLock<Option<ActiveTheme>> = std::sync::RwLock::new(None);
static THEMES: std::sync::RwLock<Vec<graphyne_lua::ThemeSpec>> = std::sync::RwLock::new(Vec::new());

/// Add or replace (by name) an extension theme.
pub fn register_theme(spec: graphyne_lua::ThemeSpec) {
    let mut themes = THEMES.write().unwrap_or_else(|e| e.into_inner());
    themes.retain(|t| t.name != spec.name);
    themes.push(spec);
}

/// Forget registered themes (a plugin reload re-registers them); the colours in use stay.
pub fn clear_themes() {
    THEMES.write().unwrap_or_else(|e| e.into_inner()).clear();
}

/// (name, extension id) of every theme added by an extension (the built-in ones are
/// [`BUILTIN_THEMES`]).
pub fn theme_list() -> Vec<(String, Option<String>)> {
    THEMES.read().unwrap_or_else(|e| e.into_inner()).iter().map(|t| (t.name.clone(), t.plugin.clone())).collect()
}

/// Name of the theme in use.
pub fn active_theme() -> String {
    ACTIVE_THEME.read().unwrap_or_else(|e| e.into_inner()).as_ref().map(|t| t.name.clone()).unwrap_or_else(|| GRAPHYNE_DARK.to_string())
}

/// Switch themes by name: a built-in one or one an extension registered. Returns false when
/// no such theme exists (yet) — Graphyne Dark is used then.
pub fn activate_theme(name: &str) -> bool {
    let active = if name == GRAPHYNE_DARK {
        None
    } else if name == GRAPHYNE_LIGHT {
        Some(ActiveTheme {
            name: name.to_string(),
            ui: LIGHT_UI.iter().copied().collect(),
            syntax: SYNTAX_TOKENS.iter().map(|(_, kind)| (*kind, light_syntax(*kind))).collect(),
        })
    } else {
        let themes = THEMES.read().unwrap_or_else(|e| e.into_inner());
        themes.iter().find(|t| t.name == name).map(|t| ActiveTheme {
            name: t.name.clone(),
            ui: t.ui.iter().filter_map(|(k, v)| UI_TOKENS.iter().find(|(n, _)| n == k).map(|(_, def)| (*def, *v))).collect(),
            syntax: t.syntax.iter().filter_map(|(k, v)| SYNTAX_TOKENS.iter().find(|(n, _)| n == k).map(|(_, kind)| (*kind, *v))).collect(),
        })
    };
    let found = active.is_some() || name == GRAPHYNE_DARK;
    *ACTIVE_THEME.write().unwrap_or_else(|e| e.into_inner()) = active;
    graphyne_terminal::set_light_palette(is_light());
    found
}

/// Whether the theme in use is light (by its editor background), for the terminal colours.
pub fn is_light() -> bool {
    let bg = themed(c::ISLAND);
    let (r, g, b) = ((bg >> 16) & 0xFF, (bg >> 8) & 0xFF, bg & 0xFF);
    (r * 299 + g * 587 + b * 114) / 1000 > 140
}

/// Built-in colour → active theme's colour.
fn themed(hex: u32) -> u32 {
    match ACTIVE_THEME.read() {
        Ok(guard) => guard.as_ref().and_then(|t| t.ui.get(&hex).copied()).unwrap_or(hex),
        Err(_) => hex,
    }
}

pub fn color(hex: u32) -> Hsla {
    rgb(themed(hex)).into()
}

pub fn color_a(hex: u32, alpha: f32) -> Hsla {
    let c: Hsla = rgb(themed(hex)).into();
    c.opacity(alpha)
}

pub fn rgba_u32(hex_rgba: u32) -> Rgba {
    rgba(hex_rgba)
}

/// Syntax colours (Graphyne Dark pastel accents unless the theme sets them).
pub fn syntax(kind: HighlightKind) -> Hsla {
    use HighlightKind::*;
    if let Some(v) = ACTIVE_THEME.read().ok().and_then(|g| g.as_ref().and_then(|t| t.syntax.get(&kind).copied())) {
        return rgb(v).into();
    }
    // Syntax defaults bypass the UI token mapping.
    let raw = |hex: u32| -> Hsla { rgb(hex).into() };
    raw(match kind {
        Plain | Variable | Module => 0xD6D6DD,
        Keyword => 0x83D6C5,
        String => 0xE394DC,
        Escape => 0xF8C762,
        Number => 0xEBC88D,
        Comment => 0x6D727C,
        Function | Method => 0xEFB080,
        Macro | Attribute => 0xA8CC7C,
        Type | Constructor | Tag => 0x87C3FF,
        TypeBuiltin | ConstantBuiltin => 0x82D2CE,
        Constant => 0xAA9BF5,
        VariableBuiltin => 0xC5A3F0,
        Parameter => 0xE3D7C2,
        Property => 0xAA9BF5,
        Label => 0xEBC88D,
        Operator => 0xC5C8CF,
        Punctuation => 0xA2A6AF,
    })
}

pub fn is_italic(kind: HighlightKind) -> bool {
    matches!(kind, HighlightKind::Comment)
}

#[derive(Clone, Copy)]
pub struct Fonts {
    /// Menus, panels, tabs.
    pub ui: &'static str,
    /// The editor.
    pub code: &'static str,
    /// The terminal.
    pub term: &'static str,
}

/// Fonts can change at run time (Settings), hence a lock, not a `OnceLock`.
static FONTS: std::sync::RwLock<Fonts> = std::sync::RwLock::new(Fonts { ui: "Inter", code: "JetBrains Mono", term: "JetBrains Mono" });
/// Installed families, sorted; chosen fonts borrow their names from here.
static INSTALLED_FONTS: OnceLock<Vec<String>> = OnceLock::new();
/// Used when a chosen font isn't installed on this machine.
static FALLBACK: OnceLock<(&'static str, &'static str)> = OnceLock::new();

/// Remember the installed font families and pick fallbacks (JetBrains Mono / Inter first).
pub fn init_fonts(available: &[String]) {
    let mut installed: Vec<String> = available.iter().filter(|f| !f.starts_with('.') && !f.trim().is_empty() && !is_symbol_font(f)).cloned().collect();
    installed.sort_by_key(|f| f.to_lowercase());
    installed.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    let installed = INSTALLED_FONTS.get_or_init(|| installed);
    let has = |name: &str| installed.iter().any(|f| f.eq_ignore_ascii_case(name));
    let pick = |prefs: &[&'static str], fallback: &'static str| prefs.iter().copied().find(|p| has(p)).unwrap_or(fallback);
    let code = pick(&["JetBrains Mono", "Cascadia Mono", "Cascadia Code", "Consolas", "Menlo", "DejaVu Sans Mono"], "monospace");
    let ui = pick(&["Inter", "Inter Variable", "Segoe UI Variable Text", "Segoe UI", "SF Pro Text", "Ubuntu", "Cantarell"], ".SystemUIFont");
    let _ = FALLBACK.set((code, ui));
    if let Ok(mut f) = FONTS.write() {
        *f = Fonts { ui, code, term: code };
    }
}

/// Icon and symbol fonts have no letters: chosen for the interface they turn every label
/// into boxes (and the Settings page with them), so they are not offered.
fn is_symbol_font(name: &str) -> bool {
    let n = name.to_lowercase();
    ["icons", "mdl2", "emoji", "wingdings", "webdings", "marlett", "dingbats", "bookshelf symbol"].iter().any(|p| n.contains(p)) || n == "symbol"
}

pub fn fonts() -> Fonts {
    FONTS.read().map(|f| *f).unwrap_or(Fonts { ui: "Inter", code: "JetBrains Mono", term: "JetBrains Mono" })
}

/// Font families installed on this computer, sorted by name.
pub fn installed_fonts() -> &'static [String] {
    INSTALLED_FONTS.get().map(Vec::as_slice).unwrap_or(&[])
}

/// The installed family called `name` (any case).
fn installed(name: &str) -> Option<&'static str> {
    installed_fonts().iter().find(|f| f.eq_ignore_ascii_case(name)).map(String::as_str)
}

pub fn is_installed(name: &str) -> bool {
    installed(name).is_some()
}

/// Use these families for the editor, terminal and interface; one that isn't installed
/// falls back to the best available. Returns whether anything changed.
pub fn set_fonts(code: &str, term: &str, ui: &str) -> bool {
    let (code_fallback, ui_fallback) = FALLBACK.get().copied().unwrap_or(("JetBrains Mono", "Inter"));
    let next = Fonts {
        ui: installed(ui).unwrap_or(ui_fallback),
        code: installed(code).unwrap_or(code_fallback),
        term: installed(term).unwrap_or(code_fallback),
    };
    match FONTS.write() {
        Ok(mut f) if (f.ui, f.code, f.term) != (next.ui, next.code, next.term) => {
            *f = next;
            true
        }
        _ => false,
    }
}

static USAGE_HINTS: AtomicBool = AtomicBool::new(true);
static INLAY_HINTS: AtomicBool = AtomicBool::new(true);
static AUTOSAVE_ON: AtomicBool = AtomicBool::new(true);

/// `N usages` lines above functions and types.
pub fn usage_hints() -> bool {
    USAGE_HINTS.load(Ordering::Relaxed)
}
/// Inlay type / parameter hints from the language server.
pub fn inlay_hints() -> bool {
    INLAY_HINTS.load(Ordering::Relaxed)
}
pub fn autosave_enabled() -> bool {
    AUTOSAVE_ON.load(Ordering::Relaxed)
}
pub fn set_flags(usage_hints: bool, inlay_hints: bool, autosave: bool) {
    USAGE_HINTS.store(usage_hints, Ordering::Relaxed);
    INLAY_HINTS.store(inlay_hints, Ordering::Relaxed);
    AUTOSAVE_ON.store(autosave, Ordering::Relaxed);
}

pub const UI_TEXT: f32 = 13.0;
pub const UI_SMALL: f32 = 12.0;
// Font sizes and autosave delay can be changed at run time from init.lua
// (`graphyne.settings.set`), hence atomics instead of constants.
static CODE_SIZE_BITS: AtomicU32 = AtomicU32::new(13.5f32.to_bits());
static TERM_SIZE_BITS: AtomicU32 = AtomicU32::new(13.0f32.to_bits());
static AUTOSAVE_MS: AtomicU64 = AtomicU64::new(500);

pub fn code_size() -> f32 {
    f32::from_bits(CODE_SIZE_BITS.load(Ordering::Relaxed))
}
/// Line height keeps the default 13.5 / 22 proportion.
pub fn code_line_height() -> f32 {
    (code_size() * 22.0 / 13.5).round()
}
pub fn term_size() -> f32 {
    f32::from_bits(TERM_SIZE_BITS.load(Ordering::Relaxed))
}
pub fn term_line_height() -> f32 {
    (term_size() * 19.0 / 13.0).round()
}
pub fn autosave_delay() -> std::time::Duration {
    std::time::Duration::from_millis(AUTOSAVE_MS.load(Ordering::Relaxed))
}

/// Apply a `graphyne.settings.set` value (already range-checked by graphyne-lua).
/// Returns whether something changed.
pub fn apply_setting(key: &str, value: &graphyne_lua::SettingValue) -> bool {
    let graphyne_lua::SettingValue::Number(n) = value else { return false };
    let n = *n;
    match key {
        "editor.font_size" => CODE_SIZE_BITS.swap((n as f32).to_bits(), Ordering::Relaxed) != (n as f32).to_bits(),
        "terminal.font_size" => TERM_SIZE_BITS.swap((n as f32).to_bits(), Ordering::Relaxed) != (n as f32).to_bits(),
        "editor.autosave_delay_ms" => AUTOSAVE_MS.swap(n as u64, Ordering::Relaxed) != n as u64,
        _ => false,
    }
}
pub const ISLAND_RADIUS: f32 = 8.0;
pub const GAP: f32 = 5.0;
pub const TITLE_BAR_HEIGHT: f32 = 40.0;
pub const STATUS_BAR_HEIGHT: f32 = 26.0;
pub const ISLAND_HEADER_HEIGHT: f32 = 36.0;

pub fn gap() -> Pixels {
    px(GAP)
}
