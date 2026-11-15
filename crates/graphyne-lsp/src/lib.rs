//! Graphyne LSP client.
//!
//! * [`client`]   — JSON-RPC 2.0 over stdio: framed reader/writer threads, request/response
//!   correlation through `flume` one-shot channels (awaitable from any executor), automatic
//!   replies to server→client requests, and a stream of [`ServerEvent`]s.
//! * [`servers`]  — the language → language-server matrix and executable discovery.
//! * [`convert`]  — `file://` URIs ⇄ paths, LSP positions (UTF-16) ⇄ rope char indices.
//! * [`model`]    — lenient parsing of server responses into small, UI-friendly types.
//! * [`snippet`]  — LSP snippet syntax → plain text + caret / placeholder selection.
//!
//! The crate is UI-agnostic: no GPUI types, no async runtime requirement.

pub mod client;
pub mod convert;
pub mod model;
pub mod servers;
pub mod snippet;

pub use client::{LspClient, ProgressKind, ServerEvent};
pub use lsp_types;
