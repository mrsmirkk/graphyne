//! Toolchain version probing for the status bar (`Python 3.13.7`, `Rust 1.85.0`).
//! Spawns processes — background executors only.

use crate::language::LanguageId;
use regex::Regex;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

fn version_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d+\.\d+(?:\.\d+)?").expect("valid regex"))
}

/// Build a `Command` that never flashes a console window on Windows.
pub fn hidden_command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Returns e.g. `Some("Python 3.13.7")`, or `None` if the toolchain is missing.
pub fn probe(lang: LanguageId) -> Option<String> {
    let (program, args) = lang.info().toolchain?;
    let output = hidden_command(program).args(args).stdin(Stdio::null()).output().ok()?;
    let text = if output.stdout.is_empty() { output.stderr } else { output.stdout };
    let text = String::from_utf8_lossy(&text);
    let version = version_re().find(&text)?.as_str().to_string();
    let label = match lang {
        LanguageId::TypeScript | LanguageId::Tsx | LanguageId::JavaScript => "Node.js",
        LanguageId::CSharp => ".NET",
        other => other.name(),
    };
    Some(format!("{label} {version}"))
}
