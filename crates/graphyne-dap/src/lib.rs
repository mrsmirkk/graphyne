//! Graphyne Debug Adapter Protocol client.
//!
//! * [`client`]   — DAP over stdio or TCP: framed reader/writer threads, `seq`/`request_seq`
//!   correlation via `flume` one-shots, answers to reverse requests, an event stream.
//! * [`adapters`] — the language → debug-adapter matrix (debugpy, lldb-dap / CodeLLDB,
//!   Delve, netcoredbg) and per-language `launch` configurations.
//! * [`model`]    — threads, stack frames, scopes and variables.
//!
//! UI-agnostic: no GPUI types.

pub mod adapters;
pub mod bridge;
pub mod client;
pub mod model;
pub mod php;

pub use client::{DapClient, DapEvent, Transport};
