//! Fleet Dark palette, fonts and metrics.

use gpui::{Hsla, Pixels, Rgba, px, rgb, rgba};
use graphyne_syntax::HighlightKind;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

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

pub fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

pub fn color_a(hex: u32, alpha: f32) -> Hsla {
    let c: Hsla = rgb(hex).into();
    c.opacity(alpha)
}

pub fn rgba_u32(hex_rgba: u32) -> Rgba {
    rgba(hex_rgba)
}

/// Syntax colours ("Fleet Dark" pastel accents).
pub fn syntax(kind: HighlightKind) -> Hsla {
    use HighlightKind::*;
    color(match kind {
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

pub struct Fonts {
    pub ui: &'static str,
    pub code: &'static str,
}

static FONTS: OnceLock<Fonts> = OnceLock::new();

/// Resolve installed font families once (JetBrains Mono / Inter preferred).
pub fn init_fonts(available: &[String]) {
    let has = |name: &str| available.iter().any(|f| f.eq_ignore_ascii_case(name));
    let pick = |prefs: &[&'static str], fallback: &'static str| prefs.iter().copied().find(|p| has(p)).unwrap_or(fallback);
    let code = pick(&["JetBrains Mono", "Cascadia Mono", "Cascadia Code", "Consolas", "Menlo", "DejaVu Sans Mono"], "monospace");
    let ui = pick(&["Inter", "Inter Variable", "Segoe UI Variable Text", "Segoe UI", "SF Pro Text", "Ubuntu", "Cantarell"], ".SystemUIFont");
    let _ = FONTS.set(Fonts { ui, code });
}

pub fn fonts() -> &'static Fonts {
    FONTS.get_or_init(|| Fonts { ui: "Inter", code: "JetBrains Mono" })
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
