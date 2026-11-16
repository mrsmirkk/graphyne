//! Embedded assets (icons). Everything is compiled into the binary: zero disk I/O at boot.

use anyhow::Result;
use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$(
            (concat!("icons/", $name, ".svg"), include_bytes!(concat!("../../../assets/icons/", $name, ".svg")) as &[u8]),
        )*]
    };
}

static ICONS: &[(&str, &[u8])] = icons![
    "panel-left", "panel-bottom", "panel-right", "panel-left-on", "panel-bottom-on", "panel-right-on",
    "grid", "user-plus", "bolt", "play", "stop", "search", "bell", "settings", "branch",
    "chevron-right", "chevron-down", "chevron-up", "chevrons-up-down", "puzzle", "palette", "folder", "folder-fill", "file", "close", "plus", "detach", "attach",
    "win-min", "win-max", "win-restore", "win-close", "history", "commit", "refresh", "check", "warning",
    "error", "library", "structure", "shield", "terminal", "binary", "undo", "dot", "sparkle", "database",
    "bug", "pause", "resume", "step-over", "step-into", "step-out", "restart",
    "file-plus", "folder-plus", "pencil", "trash", "copy", "folder-open",
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = crate::file_icons::load(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        Ok(ICONS.iter().find(|(p, _)| *p == path).map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS.iter().filter(|(p, _)| p.starts_with(path)).map(|(p, _)| SharedString::from(*p)).collect())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    PanelLeft,
    PanelBottom,
    PanelRight,
    PanelLeftOn,
    PanelBottomOn,
    PanelRightOn,
    Grid,
    UserPlus,
    Bolt,
    Play,
    Stop,
    Search,
    Bell,
    Settings,
    Branch,
    ChevronRight,
    ChevronDown,
    ChevronUp,
    ChevronsUpDown,
    Puzzle,
    Palette,
    Folder,
    FolderFill,
    File,
    Close,
    Plus,
    Detach,
    Attach,
    WinMin,
    WinMax,
    WinRestore,
    WinClose,
    History,
    Commit,
    Refresh,
    Check,
    Warning,
    Error,
    Library,
    Structure,
    Shield,
    Terminal,
    Binary,
    Undo,
    Dot,
    Sparkle,
    Database,
    Bug,
    Pause,
    Resume,
    StepOver,
    StepInto,
    StepOut,
    Restart,
    FilePlus,
    FolderPlus,
    Pencil,
    Trash,
    Copy,
    FolderOpen,
}

impl Icon {
    pub fn path(self) -> &'static str {
        use Icon::*;
        match self {
            PanelLeft => "icons/panel-left.svg",
            PanelBottom => "icons/panel-bottom.svg",
            PanelRight => "icons/panel-right.svg",
            PanelLeftOn => "icons/panel-left-on.svg",
            PanelBottomOn => "icons/panel-bottom-on.svg",
            PanelRightOn => "icons/panel-right-on.svg",
            Grid => "icons/grid.svg",
            UserPlus => "icons/user-plus.svg",
            Bolt => "icons/bolt.svg",
            Play => "icons/play.svg",
            Stop => "icons/stop.svg",
            Search => "icons/search.svg",
            Bell => "icons/bell.svg",
            Settings => "icons/settings.svg",
            Branch => "icons/branch.svg",
            ChevronRight => "icons/chevron-right.svg",
            ChevronDown => "icons/chevron-down.svg",
            ChevronUp => "icons/chevron-up.svg",
            ChevronsUpDown => "icons/chevrons-up-down.svg",
            Puzzle => "icons/puzzle.svg",
            Palette => "icons/palette.svg",
            Folder => "icons/folder.svg",
            FolderFill => "icons/folder-fill.svg",
            File => "icons/file.svg",
            Close => "icons/close.svg",
            Plus => "icons/plus.svg",
            Detach => "icons/detach.svg",
            Attach => "icons/attach.svg",
            WinMin => "icons/win-min.svg",
            WinMax => "icons/win-max.svg",
            WinRestore => "icons/win-restore.svg",
            WinClose => "icons/win-close.svg",
            History => "icons/history.svg",
            Commit => "icons/commit.svg",
            Refresh => "icons/refresh.svg",
            Check => "icons/check.svg",
            Warning => "icons/warning.svg",
            Error => "icons/error.svg",
            Library => "icons/library.svg",
            Structure => "icons/structure.svg",
            Shield => "icons/shield.svg",
            Terminal => "icons/terminal.svg",
            Binary => "icons/binary.svg",
            Undo => "icons/undo.svg",
            Dot => "icons/dot.svg",
            Sparkle => "icons/sparkle.svg",
            Database => "icons/database.svg",
            Bug => "icons/bug.svg",
            Pause => "icons/pause.svg",
            Resume => "icons/resume.svg",
            StepOver => "icons/step-over.svg",
            StepInto => "icons/step-into.svg",
            StepOut => "icons/step-out.svg",
            Restart => "icons/restart.svg",
            FilePlus => "icons/file-plus.svg",
            FolderPlus => "icons/folder-plus.svg",
            Pencil => "icons/pencil.svg",
            Trash => "icons/trash.svg",
            Copy => "icons/copy.svg",
            FolderOpen => "icons/folder-open.svg",
        }
    }
}
