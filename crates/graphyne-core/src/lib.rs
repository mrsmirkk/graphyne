//! Graphyne core: everything that is UI-agnostic and safe to run on any thread.
//!
//! * [`buffer`]    — `ropey`-backed text buffer with grouped undo/redo and tree-sitter edit deltas.
//! * [`language`]  — the language / toolchain matrix (badges, LSP, DAP, one-click run templates).
//! * [`project`]   — directory listing, `.gitignore`-aware project walk, find-in-files.
//! * [`fuzzy`]     — `nucleo` CamelHump / substring ranking used by Search Everywhere.
//! * [`secrets`]   — Secrets Guard scanner (AWS keys, private keys, tokens, JWT, entropy).
//! * [`fs_util`]   — atomic writes and binary sniffing.
//! * [`run`]       — One-Click Run planner.
//! * [`toolchain`] — background toolchain version probing.
//! * [`session`]   — what was open at exit (projects, tabs, cursors, panels, window).

pub mod buffer;
pub mod fs_util;
pub mod fuzzy;
pub mod language;
pub mod libraries;
pub mod live_server;
pub mod presets;
pub mod project;
pub mod recent;
pub mod run;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod toolchain;

pub use buffer::{Buffer, EditDelta, LineEnding, Selection, TextPoint};
pub use language::LanguageId;
