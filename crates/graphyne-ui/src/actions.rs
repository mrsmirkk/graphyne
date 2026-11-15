//! Action definitions and the default keymap.

use gpui::{App, KeyBinding, actions};

actions!(
    graphyne,
    [
        ToggleLeftDock,
        ToggleBottomDock,
        ToggleRightDock,
        SearchEverywhere,
        RunActive,
        ToggleDetachTerminal,
        NewTerminal,
        CloseActiveTab,
        SaveAll,
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

pub fn bind_keys(cx: &mut App) {
    let ws = Some("Workspace");
    let ed = Some("Editor");
    let ti = Some("TextInput");
    let term = Some("Terminal");
    cx.bind_keys([
        // Workspace (global)
        KeyBinding::new(&m("mod-k"), SearchEverywhere, ws),
        KeyBinding::new(&m("mod-shift-o"), SearchEverywhere, ws),
        KeyBinding::new(&m("mod-shift-f"), FindInFiles, ws),
        KeyBinding::new("shift-f10", RunActive, ws),
        KeyBinding::new(&m("mod-r"), RunActive, ed),
        KeyBinding::new("alt-shift-f12", ToggleDetachTerminal, ws),
        KeyBinding::new("alt-f12", FocusTerminal, ws),
        KeyBinding::new(&m("mod-1"), ToggleLeftDock, ws),
        KeyBinding::new(&m("mod-b"), editor::GoToDefinition, ed),
        KeyBinding::new("f12", editor::GoToDefinition, ed),
        KeyBinding::new(&m("mod-q"), editor::QuickDocumentation, ed),
        KeyBinding::new("alt-enter", editor::ShowCodeActions, ed),
        KeyBinding::new(&m("mod-p"), editor::ParameterInfo, ed),
        KeyBinding::new(&m("mod-j"), ToggleBottomDock, ws),
        KeyBinding::new(&m("mod-alt-b"), ToggleRightDock, ws),
        KeyBinding::new(&m("mod-shift-t"), NewTerminal, ws),
        KeyBinding::new(&m("mod-f4"), CloseActiveTab, ws),
        KeyBinding::new(&m("mod-tab"), NextTab, ws),
        KeyBinding::new(&m("mod-shift-tab"), PrevTab, ws),
        KeyBinding::new(&m("mod-alt-s"), SaveAll, ws),
        KeyBinding::new(&m("mod-q"), Quit, ws),
        KeyBinding::new("shift-f9", DebugActive, ws),
        KeyBinding::new("f9", DebugResume, ws),
        KeyBinding::new("f8", DebugStepOver, ws),
        KeyBinding::new("f7", DebugStepInto, ws),
        KeyBinding::new("shift-f8", DebugStepOut, ws),
        KeyBinding::new(&m("mod-f2"), DebugStop, ws),
        KeyBinding::new(&m("mod-f8"), editor::ToggleBreakpoint, ed),
        KeyBinding::new(&m("mod-shift-f8"), editor::EditBreakpoint, ed),
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
        KeyBinding::new(&m("mod-z"), editor::Undo, ed),
        KeyBinding::new(&m("mod-shift-z"), editor::Redo, ed),
        KeyBinding::new(&m("mod-y"), editor::Redo, ed),
        KeyBinding::new(&m("mod-d"), editor::DuplicateLine, ed),
        KeyBinding::new(&m("mod-shift-k"), editor::DeleteLine, ed),
        KeyBinding::new(&m("mod-/"), editor::ToggleComment, ed),
        KeyBinding::new(&m("mod-w"), editor::ExtendSelection, ed),
        KeyBinding::new(&m("mod-shift-w"), editor::ShrinkSelection, ed),
        KeyBinding::new("alt-shift-up", editor::MoveLineUp, ed),
        KeyBinding::new("alt-shift-down", editor::MoveLineDown, ed),
        KeyBinding::new("escape", editor::Escape, ed),
        KeyBinding::new(&m("mod-space"), editor::ShowCompletions, ed),
        KeyBinding::new(&m("mod-s"), editor::Save, ed),
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
        KeyBinding::new(&m("mod-shift-c"), terminal::Copy, term),
        KeyBinding::new(&m("mod-shift-v"), terminal::Paste, term),
        KeyBinding::new("shift-insert", terminal::Paste, term),
        KeyBinding::new("shift-pageup", terminal::ScrollPageUp, term),
        KeyBinding::new("shift-pagedown", terminal::ScrollPageDown, term),
    ]);
}
