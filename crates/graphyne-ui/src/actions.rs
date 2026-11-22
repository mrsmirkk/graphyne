//! Action definitions and the default keymap.

use gpui::{App, KeyBinding, actions};

actions!(
    graphyne,
    [
        ToggleLeftDock,
        ToggleBottomDock,
        OpenSettings,
        ToggleRightDock,
        SearchEverywhere,
        RunActive,
        ToggleDetachTerminal,
        NewTerminal,
        CloseActiveTab,
        SaveAll,
        NewFile,
        NewFolder,
        OpenFile,
        SaveFile,
        SaveAs,
        CloseAllTabs,
        Quit,
        NextTab,
        PrevTab,
        FindInFiles,
        FocusTerminal,
        FocusEditor,
        DebugActive,
        DebugResume,
        DebugStepOver,
        DebugStepInto,
        DebugStepOut,
        DebugStop,
    ]
);

pub mod editor {
    use gpui::actions;
    actions!(
        editor,
        [
            MoveLeft,
            MoveRight,
            MoveUp,
            MoveDown,
            SelectLeft,
            SelectRight,
            SelectUp,
            SelectDown,
            MoveWordLeft,
            MoveWordRight,
            SelectWordLeft,
            SelectWordRight,
            MoveHome,
            MoveEnd,
            SelectHome,
            SelectEnd,
            MoveDocStart,
            MoveDocEnd,
            SelectDocStart,
            SelectDocEnd,
            PageUp,
            PageDown,
            Backspace,
            Delete,
            DeleteWordLeft,
            DeleteWordRight,
            Newline,
            NewlineBelow,
            Tab,
            Outdent,
            SelectAll,
            Copy,
            Cut,
            Paste,
            Undo,
            Redo,
            DuplicateLine,
            DeleteLine,
            ToggleComment,
            FoldRegion,
            UnfoldRegion,
            FoldAll,
            UnfoldAll,
            ExtendSelection,
            ShrinkSelection,
            MoveLineUp,
            MoveLineDown,
            Escape,
            ShowCompletions,
            Save,
            QuickDocumentation,
            GoToDefinition,
            ShowCodeActions,
            ParameterInfo,
            ToggleBreakpoint,
            EditBreakpoint,
        ]
    );
}

pub mod input {
    use gpui::actions;
    actions!(
        text_input,
        [Backspace, Delete, Left, Right, SelectLeft, SelectRight, SelectAll, Home, End, Paste, Cut, Copy, Enter, Escape, Up, Down, WordLeft, WordRight, DeleteWordLeft]
    );
}

pub mod terminal {
    use gpui::actions;
    actions!(terminal, [Copy, Paste, Clear, ScrollPageUp, ScrollPageDown]);
}

/// Platform "primary" modifier: cmd on macOS, ctrl elsewhere.
pub(crate) fn m(keys: &str) -> String {
    if cfg!(target_os = "macos") { keys.replace("mod-", "cmd-") } else { keys.replace("mod-", "ctrl-") }
}

/// The fixed bindings (cursor, selection, clipboard, text inputs) and every command's
/// shortcuts from [`crate::keymap`], with the user's changes.
pub fn bind_keys(cx: &mut App) {
    let ed = Some("Editor");
    let ti = Some("TextInput");
    let term = Some("Terminal");
    cx.bind_keys([
        // Editor
        KeyBinding::new("left", editor::MoveLeft, ed),
        KeyBinding::new("right", editor::MoveRight, ed),
        KeyBinding::new("up", editor::MoveUp, ed),
        KeyBinding::new("down", editor::MoveDown, ed),
        KeyBinding::new("shift-left", editor::SelectLeft, ed),
        KeyBinding::new("shift-right", editor::SelectRight, ed),
        KeyBinding::new("shift-up", editor::SelectUp, ed),
        KeyBinding::new("shift-down", editor::SelectDown, ed),
        KeyBinding::new(&m("mod-left"), editor::MoveWordLeft, ed),
        KeyBinding::new(&m("mod-right"), editor::MoveWordRight, ed),
        KeyBinding::new(&m("mod-shift-left"), editor::SelectWordLeft, ed),
        KeyBinding::new(&m("mod-shift-right"), editor::SelectWordRight, ed),
        KeyBinding::new("home", editor::MoveHome, ed),
        KeyBinding::new("end", editor::MoveEnd, ed),
        KeyBinding::new("shift-home", editor::SelectHome, ed),
        KeyBinding::new("shift-end", editor::SelectEnd, ed),
        KeyBinding::new(&m("mod-home"), editor::MoveDocStart, ed),
        KeyBinding::new(&m("mod-end"), editor::MoveDocEnd, ed),
        KeyBinding::new(&m("mod-shift-home"), editor::SelectDocStart, ed),
        KeyBinding::new(&m("mod-shift-end"), editor::SelectDocEnd, ed),
        KeyBinding::new("pageup", editor::PageUp, ed),
        KeyBinding::new("pagedown", editor::PageDown, ed),
        KeyBinding::new("backspace", editor::Backspace, ed),
        KeyBinding::new("shift-backspace", editor::Backspace, ed),
        KeyBinding::new("delete", editor::Delete, ed),
        KeyBinding::new(&m("mod-backspace"), editor::DeleteWordLeft, ed),
        KeyBinding::new(&m("mod-delete"), editor::DeleteWordRight, ed),
        KeyBinding::new("enter", editor::Newline, ed),
        KeyBinding::new("shift-enter", editor::NewlineBelow, ed),
        KeyBinding::new("tab", editor::Tab, ed),
        KeyBinding::new("shift-tab", editor::Outdent, ed),
        KeyBinding::new(&m("mod-a"), editor::SelectAll, ed),
        KeyBinding::new(&m("mod-c"), editor::Copy, ed),
        KeyBinding::new(&m("mod-x"), editor::Cut, ed),
        KeyBinding::new(&m("mod-v"), editor::Paste, ed),
        KeyBinding::new("escape", editor::Escape, ed),
        // Text inputs
        KeyBinding::new("backspace", input::Backspace, ti),
        KeyBinding::new("delete", input::Delete, ti),
        KeyBinding::new("left", input::Left, ti),
        KeyBinding::new("right", input::Right, ti),
        KeyBinding::new("shift-left", input::SelectLeft, ti),
        KeyBinding::new("shift-right", input::SelectRight, ti),
        KeyBinding::new(&m("mod-left"), input::WordLeft, ti),
        KeyBinding::new(&m("mod-right"), input::WordRight, ti),
        KeyBinding::new(&m("mod-backspace"), input::DeleteWordLeft, ti),
        KeyBinding::new(&m("mod-a"), input::SelectAll, ti),
        KeyBinding::new("home", input::Home, ti),
        KeyBinding::new("end", input::End, ti),
        KeyBinding::new(&m("mod-v"), input::Paste, ti),
        KeyBinding::new(&m("mod-c"), input::Copy, ti),
        KeyBinding::new(&m("mod-x"), input::Cut, ti),
        KeyBinding::new("enter", input::Enter, ti),
        KeyBinding::new("escape", input::Escape, ti),
        KeyBinding::new("up", input::Up, ti),
        KeyBinding::new("down", input::Down, ti),
        // Terminal
        KeyBinding::new("shift-pageup", terminal::ScrollPageUp, term),
        KeyBinding::new("shift-pagedown", terminal::ScrollPageDown, term),
    ]);
    let (bindings, invalid) = crate::keymap::bindings(&crate::app_settings::get(cx));
    cx.bind_keys(bindings);
    if !invalid.is_empty() {
        log::warn!("keymap: shortcuts that could not be read: {}", invalid.join(", "));
    }
}
