//! The commands a shortcut can be given to, with their default shortcuts; the user's changes
//! (`keymap` in settings.json, edited in Settings → Keymap); how shortcuts are shown.
//!
//! Cursor movement, text editing inside inputs and the clipboard stay fixed (see
//! [`crate::actions::bind_keys`]); everything here can be changed.

use crate::actions::{self, editor, terminal};
use gpui::{Action, App, DummyKeyboardMapper, KeyBinding, KeyBindingContextPredicate, Keystroke};
use graphyne_core::settings::Settings;
use std::rc::Rc;

pub struct Command {
    /// Stable name, the key in settings.json.
    pub id: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    /// The key context the shortcuts work in ("Workspace": everywhere in a project window).
    pub context: &'static str,
    /// Default shortcuts; `mod-` is Ctrl (Cmd on macOS), `keys@Context` works in another context.
    pub defaults: &'static [&'static str],
    action: fn() -> Box<dyn Action>,
}

macro_rules! cmd {
    ($id:literal, $label:literal, $group:literal, $context:literal, [$($key:literal),*], $action:expr) => {
        Command { id: $id, label: $label, group: $group, context: $context, defaults: &[$($key),*], action: || Box::new($action) }
    };
}

pub static GROUPS: &[&str] = &["General", "Files and tabs", "Panels", "Run and debug", "Editor", "Terminal", "Database"];

pub static COMMANDS: &[Command] = &[
    cmd!("search_everywhere", "Search Everywhere (files, symbols, actions)", "General", "Workspace", ["mod-k", "mod-shift-o"], actions::SearchEverywhere),
    cmd!("find_in_files", "Find in Files", "General", "Workspace", ["mod-shift-f"], actions::FindInFiles),
    cmd!("settings", "Settings", "General", "Workspace", ["mod-,"], actions::OpenSettings),
    cmd!("exit", "Exit", "General", "Workspace", ["mod-q"], actions::Quit),
    cmd!("new_file", "New File", "Files and tabs", "Workspace", ["alt-insert"], actions::NewFile),
    cmd!("new_folder", "New Folder", "Files and tabs", "Workspace", [], actions::NewFolder),
    cmd!("open_file", "Open File", "Files and tabs", "Workspace", ["mod-o"], actions::OpenFile),
    cmd!("save", "Save", "Files and tabs", "Editor", ["mod-s"], editor::Save),
    cmd!("save_as", "Save As", "Files and tabs", "Workspace", ["mod-shift-s"], actions::SaveAs),
    cmd!("save_all", "Save All", "Files and tabs", "Workspace", ["mod-alt-s"], actions::SaveAll),
    cmd!("close_tab", "Close Tab", "Files and tabs", "Workspace", ["mod-f4"], actions::CloseActiveTab),
    cmd!("close_all_tabs", "Close All Tabs", "Files and tabs", "Workspace", [], actions::CloseAllTabs),
    cmd!("next_tab", "Next Tab", "Files and tabs", "Workspace", ["mod-tab"], actions::NextTab),
    cmd!("previous_tab", "Previous Tab", "Files and tabs", "Workspace", ["mod-shift-tab"], actions::PrevTab),
    cmd!("toggle_left_panel", "Toggle Left Panel", "Panels", "Workspace", ["mod-1"], actions::ToggleLeftDock),
    cmd!("toggle_right_panel", "Toggle Right Panel", "Panels", "Workspace", ["mod-alt-b"], actions::ToggleRightDock),
    cmd!("toggle_terminal", "Toggle Terminal", "Panels", "Workspace", ["mod-j"], actions::ToggleBottomDock),
    cmd!("focus_terminal", "Focus Terminal", "Panels", "Workspace", ["alt-f12"], actions::FocusTerminal),
    cmd!("focus_editor", "Focus Editor", "Panels", "Workspace", [], actions::FocusEditor),
    cmd!("new_terminal", "New Terminal", "Panels", "Workspace", ["mod-shift-t"], actions::NewTerminal),
    cmd!("detach_terminal", "Detach / Attach Terminal", "Panels", "Workspace", ["alt-shift-f12"], actions::ToggleDetachTerminal),
    cmd!("run", "Run Current File", "Run and debug", "Workspace", ["shift-f10", "mod-r@Editor"], actions::RunActive),
    cmd!("debug", "Debug Current File", "Run and debug", "Workspace", ["shift-f9"], actions::DebugActive),
    cmd!("debug_resume", "Resume", "Run and debug", "Workspace", ["f9"], actions::DebugResume),
    cmd!("debug_step_over", "Step Over", "Run and debug", "Workspace", ["f8"], actions::DebugStepOver),
    cmd!("debug_step_into", "Step Into", "Run and debug", "Workspace", ["f7"], actions::DebugStepInto),
    cmd!("debug_step_out", "Step Out", "Run and debug", "Workspace", ["shift-f8"], actions::DebugStepOut),
    cmd!("debug_stop", "Stop", "Run and debug", "Workspace", ["mod-f2"], actions::DebugStop),
    cmd!("toggle_breakpoint", "Toggle Breakpoint", "Run and debug", "Editor", ["mod-f8"], editor::ToggleBreakpoint),
    cmd!("edit_breakpoint", "Edit Breakpoint (condition, log message)", "Run and debug", "Editor", ["mod-shift-f8"], editor::EditBreakpoint),
    cmd!("go_to_definition", "Go to Definition", "Editor", "Editor", ["mod-b", "f12"], editor::GoToDefinition),
    cmd!("quick_documentation", "Quick Documentation", "Editor", "Editor", ["mod-q"], editor::QuickDocumentation),
    cmd!("parameter_info", "Parameter Info", "Editor", "Editor", ["mod-p"], editor::ParameterInfo),
    cmd!("code_actions", "Show Code Actions", "Editor", "Editor", ["alt-enter"], editor::ShowCodeActions),
    cmd!("completion", "Show Completions", "Editor", "Editor", ["mod-space"], editor::ShowCompletions),
    cmd!("undo", "Undo", "Editor", "Editor", ["mod-z"], editor::Undo),
    cmd!("redo", "Redo", "Editor", "Editor", ["mod-shift-z", "mod-y"], editor::Redo),
    cmd!("duplicate_line", "Duplicate Line", "Editor", "Editor", ["mod-d"], editor::DuplicateLine),
    cmd!("delete_line", "Delete Line", "Editor", "Editor", ["mod-shift-k"], editor::DeleteLine),
    cmd!("move_line_up", "Move Line Up", "Editor", "Editor", ["alt-shift-up"], editor::MoveLineUp),
    cmd!("move_line_down", "Move Line Down", "Editor", "Editor", ["alt-shift-down"], editor::MoveLineDown),
    cmd!("toggle_comment", "Comment / Uncomment", "Editor", "Editor", ["mod-/"], editor::ToggleComment),
    cmd!("extend_selection", "Extend Selection", "Editor", "Editor", ["mod-w"], editor::ExtendSelection),
    cmd!("shrink_selection", "Shrink Selection", "Editor", "Editor", ["mod-shift-w"], editor::ShrinkSelection),
    // Shift+- arrives as "_" and Shift+= as "+": both spellings are bound.
    cmd!("fold", "Fold", "Editor", "Editor", ["mod--"], editor::FoldRegion),
    cmd!("unfold", "Unfold", "Editor", "Editor", ["mod-="], editor::UnfoldRegion),
    cmd!("fold_all", "Fold All", "Editor", "Editor", ["mod-shift--", "mod-_"], editor::FoldAll),
    cmd!("unfold_all", "Unfold All", "Editor", "Editor", ["mod-shift-=", "mod-+"], editor::UnfoldAll),
    cmd!("terminal_copy", "Copy", "Terminal", "Terminal", ["mod-shift-c"], terminal::Copy),
    cmd!("terminal_paste", "Paste", "Terminal", "Terminal", ["mod-shift-v", "shift-insert"], terminal::Paste),
    cmd!("sql_run", "Run Statement in SQL Console", "Database", "DbConsole", ["mod-enter"], crate::database::RunStatement),
];

pub fn command(id: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.id == id)
}

impl Command {
    /// The default shortcuts with the platform's modifier filled in.
    pub fn default_keys(&self) -> Vec<String> {
        self.defaults.iter().map(|k| actions::m(k)).collect()
    }

    /// The shortcuts in use: the user's, else the defaults.
    pub fn keys(&self, settings: &Settings) -> Vec<String> {
        settings.keymap.get(self.id).cloned().unwrap_or_else(|| self.default_keys())
    }

    pub fn is_changed(&self, settings: &Settings) -> bool {
        settings.keymap.get(self.id).is_some_and(|keys| *keys != self.default_keys())
    }
}

/// `keys` without its `@Context`, and the context it works in.
fn split<'a>(keys: &'a str, cmd: &Command) -> (&'a str, &'a str) {
    match keys.split_once('@') {
        Some((k, context)) => (k, context),
        None => (keys, cmd.context),
    }
}

/// Bindings for every command; shortcuts that don't parse (a hand-edited file) are returned
/// separately.
pub fn bindings(settings: &Settings) -> (Vec<KeyBinding>, Vec<String>) {
    let (mut out, mut invalid) = (Vec::new(), Vec::new());
    for cmd in COMMANDS {
        for keys in cmd.keys(settings) {
            let (k, context) = split(&keys, cmd);
            let predicate = KeyBindingContextPredicate::parse(context).ok().map(Rc::new);
            match KeyBinding::load(k, (cmd.action)(), predicate, false, None, &DummyKeyboardMapper) {
                Ok(binding) if !k.trim().is_empty() => out.push(binding),
                _ => invalid.push(format!("{} ({keys})", cmd.label)),
            }
        }
    }
    (out, invalid)
}

/// One keystroke's keycaps: "ctrl-shift-k" → ["Ctrl", "Shift", "K"].
fn stroke_parts(stroke: &str) -> Vec<String> {
    // "ctrl--" is Ctrl and "-": the key itself may be a dash.
    let (mods, key) = match stroke.rsplit_once('-') {
        Some((m, "")) => (m.strip_suffix('-').unwrap_or(m), "-"),
        Some((m, k)) => (m, k),
        None => ("", stroke),
    };
    let mut out: Vec<String> = mods
        .split('-')
        .filter(|m| !m.is_empty())
        .map(|m| match m {
            "ctrl" | "control" => "Ctrl".to_string(),
            "alt" => "Alt".into(),
            "shift" => "Shift".into(),
            "cmd" | "platform" | "super" | "win" => if cfg!(target_os = "macos") { "Cmd" } else { "Win" }.into(),
            "fn" | "function" => "Fn".into(),
            other => other.to_string(),
        })
        .collect();
    out.push(key_name(key));
    out
}

/// The keycaps of a shortcut (a chord's keystrokes one after another).
pub fn parts(keys: &str) -> Vec<String> {
    let keys = keys.split('@').next().unwrap_or(keys);
    keys.split_whitespace().flat_map(stroke_parts).collect()
}

fn key_name(key: &str) -> String {
    match key {
        "pageup" => "Page Up".into(),
        "pagedown" => "Page Down".into(),
        "backspace" => "Backspace".into(),
        "escape" => "Esc".into(),
        "up" => "↑".into(),
        "down" => "↓".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        k if k.len() > 1 => {
            let mut c = k.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
        k => k.to_uppercase(),
    }
}

/// "ctrl-shift-k" → "Ctrl+Shift+K".
pub fn label(keys: &str) -> String {
    let keys = keys.split('@').next().unwrap_or(keys);
    keys.split_whitespace().map(|s| stroke_parts(s).join("+")).collect::<Vec<_>>().join(" ")
}

/// The first shortcut of command `id`, as shown in menus ("Ctrl+K").
pub fn shortcut(id: &str, cx: &App) -> Option<String> {
    let cmd = command(id)?;
    cmd.keys(&crate::app_settings::get(cx)).first().map(|k| label(k))
}

/// The same shortcut as `parts`, for keycaps.
pub fn shortcut_parts(id: &str, cx: &App) -> Vec<String> {
    command(id).and_then(|cmd| cmd.keys(&crate::app_settings::get(cx)).first().map(|k| parts(k))).unwrap_or_default()
}

/// What a recorded keystroke is stored as: "ctrl-shift-k". `None` for a lone modifier.
pub fn from_keystroke(k: &Keystroke) -> Option<String> {
    if matches!(k.key.as_str(), "shift" | "control" | "alt" | "platform" | "function" | "") {
        return None;
    }
    Some(k.unparse())
}

/// Whether two contexts can both see a key: the same one, or Workspace (every project window).
fn overlap(a: &str, b: &str) -> bool {
    a == b || a == "Workspace" || b == "Workspace"
}

/// Whether `keys` is one of `cmd`'s default shortcuts.
fn is_default(cmd: &Command, keys: &str) -> bool {
    cmd.default_keys().iter().any(|d| split(d, cmd).0.eq_ignore_ascii_case(keys))
}

/// Other commands that use `keys` where `cmd`'s shortcut works too. Shortcuts both commands
/// have by default are meant to share (Ctrl+Q: documentation in the editor, Exit elsewhere).
pub fn conflicts(cmd: &Command, keys: &str, settings: &Settings) -> Vec<&'static Command> {
    let (k, context) = split(keys, cmd);
    COMMANDS
        .iter()
        .filter(|other| other.id != cmd.id)
        .filter(|other| !(is_default(cmd, k) && is_default(other, k)))
        .filter(|other| {
            other.keys(settings).iter().any(|ok| {
                let (ok, oc) = split(ok, other);
                ok.eq_ignore_ascii_case(k) && overlap(oc, context)
            })
        })
        .collect()
}

/// Store `keys` as command `id`'s shortcuts (the defaults again: the change is forgotten).
pub fn set_keys(settings: &mut Settings, id: &str, keys: Vec<String>) {
    let Some(cmd) = command(id) else { return };
    if keys == cmd.default_keys() {
        settings.keymap.remove(id);
    } else {
        settings.keymap.insert(id.to_string(), keys);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_read_well() {
        assert_eq!(label("ctrl-shift-k"), "Ctrl+Shift+K");
        assert_eq!(label("shift-f10"), "Shift+F10");
        assert_eq!(label("ctrl--"), "Ctrl+-");
        assert_eq!(label("alt-insert"), "Alt+Insert");
        assert_eq!(label("ctrl-r@Editor"), "Ctrl+R");
        assert_eq!(parts("ctrl-,"), ["Ctrl", ","]);
        assert_eq!(label("alt-shift-up"), "Alt+Shift+↑");
        assert_eq!(label("ctrl-+"), "Ctrl++");
        assert_eq!(parts("ctrl-shift--"), ["Ctrl", "Shift", "-"]);
        assert_eq!(label("ctrl-k ctrl-s"), "Ctrl+K Ctrl+S");
    }

    #[test]
    fn every_default_parses_and_ids_are_unique() {
        let settings = Settings::default();
        let (bindings, invalid) = bindings(&settings);
        assert!(invalid.is_empty(), "{invalid:?}");
        assert_eq!(bindings.len(), COMMANDS.iter().map(|c| c.defaults.len()).sum::<usize>());
        let mut ids: Vec<_> = COMMANDS.iter().map(|c| c.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), COMMANDS.len());
        assert!(COMMANDS.iter().all(|c| GROUPS.contains(&c.group)));
        // Out of the box nothing is reported as a conflict.
        for cmd in COMMANDS {
            for keys in cmd.keys(&settings) {
                assert!(conflicts(cmd, &keys, &settings).is_empty(), "{} {keys}", cmd.id);
            }
        }
    }

    #[test]
    fn changes_and_conflicts() {
        let mut s = Settings::default();
        let dup = command("duplicate_line").unwrap();
        let search = command("search_everywhere").unwrap();
        // Ctrl+D for a Workspace command collides with Duplicate Line in the editor.
        assert_eq!(conflicts(search, "ctrl-d", &s).iter().map(|c| c.id).collect::<Vec<_>>(), ["duplicate_line"]);
        // Editor and Terminal shortcuts never meet.
        assert!(conflicts(command("terminal_copy").unwrap(), "ctrl-d", &s).is_empty());
        set_keys(&mut s, "duplicate_line", vec!["ctrl-alt-d".into()]);
        assert!(dup.is_changed(&s));
        assert!(conflicts(search, "ctrl-d", &s).is_empty());
        set_keys(&mut s, "duplicate_line", dup.default_keys());
        assert!(s.keymap.is_empty());
    }
}
