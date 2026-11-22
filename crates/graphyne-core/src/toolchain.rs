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

/// What runs a `.ts` file: Bun when it is installed, else Node.js, which strips the types
/// itself since version 23.6 (`.tsx` still needs Bun).
pub fn typescript_runner() -> &'static str {
    if which("bun").is_some() || which("node").is_none() { "bun" } else { "node" }
}

/// A program on PATH (on Windows also `.exe` / `.cmd` / `.bat`), or in `~/.cargo/bin`.
pub fn which(program: &str) -> Option<std::path::PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".cargo").join("bin"));
    }
    dirs.iter().flat_map(|d| exts.iter().map(move |e| d.join(format!("{program}{e}")))).find(|p| p.is_file())
}

/// A compiler / runtime that Run needs but this computer doesn't have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingTool {
    /// The program Run calls (`gcc`, `cargo`, `java`…).
    pub program: &'static str,
    /// What it is, for messages: "a C compiler (gcc)".
    pub what: &'static str,
    /// Where to get it.
    pub hint: &'static str,
}

impl MissingTool {
    pub fn message(&self, lang: LanguageId) -> String {
        format!("{} isn't installed, so {} files can't run. {} Then restart Graphyne.", capitalize(self.what), lang.name(), self.hint)
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// The program Run uses for `file` (see `run::plan`).
pub fn run_program(lang: LanguageId, file: &std::path::Path, root: &std::path::Path) -> Option<&'static str> {
    Some(match lang {
        LanguageId::Rust => {
            let in_crate = file.ancestors().skip(1).take_while(|d| d.starts_with(root)).any(|d| d.join("Cargo.toml").is_file());
            if in_crate { "cargo" } else { "rustc" }
        }
        LanguageId::C => "gcc",
        LanguageId::Cpp => "g++",
        LanguageId::Python => "python",
        LanguageId::Go => "go",
        LanguageId::TypeScript => typescript_runner(),
        LanguageId::Tsx => "bun",
        LanguageId::JavaScript => "node",
        LanguageId::CSharp => "dotnet",
        LanguageId::Java => "java",
        LanguageId::Kotlin => "kotlinc",
        LanguageId::Php => "php",
        LanguageId::Dart => "dart",
        LanguageId::Lua => "lua",
        LanguageId::Ruby => "ruby",
        LanguageId::Scala => "scala-cli",
        LanguageId::Swift => "swift",
        LanguageId::Shell => "bash",
        _ => return None,
    })
}

fn describe(program: &'static str) -> Option<(&'static str, &'static str)> {
    Some(match program {
        "cargo" | "rustc" => ("Rust (rustc and cargo)", "Install it with rustup: https://rustup.rs (on Windows: winget install Rustlang.Rustup)."),
        "gcc" => ("a C compiler (gcc)", "Install MinGW-w64 (winget install BrechtSanders.WinLibs.POSIX.UCRT, or MSYS2 from msys2.org) and add its bin folder to PATH."),
        "g++" => ("a C++ compiler (g++)", "Install MinGW-w64 (winget install BrechtSanders.WinLibs.POSIX.UCRT, or MSYS2 from msys2.org) and add its bin folder to PATH."),
        "python" => ("Python 3", "Get it from python.org (on Windows: winget install Python.Python.3.13)."),
        "go" => ("Go", "Get it from go.dev/dl (on Windows: winget install GoLang.Go)."),
        "bun" => ("Bun (runs TypeScript)", "Get it from bun.sh (on Windows: winget install Oven-sh.Bun)."),
        "node" => ("Node.js", "Get it from nodejs.org (on Windows: winget install OpenJS.NodeJS.LTS)."),
        "dotnet" => ("the .NET SDK", "Get it from dotnet.microsoft.com/download (on Windows: winget install Microsoft.DotNet.SDK.8)."),
        "java" => ("a Java JDK", "Install one from adoptium.net (on Windows: winget install Microsoft.OpenJDK.21)."),
        "kotlinc" => ("the Kotlin compiler (kotlinc)", "Install it from kotlinlang.org/docs/command-line.html; it also needs a Java JDK."),
        "php" => ("PHP", "Get it from php.net/downloads."),
        "dart" => ("the Dart SDK", "Get it from dart.dev/get-dart."),
        "lua" => ("Lua", "Get it from lua.org/download.html."),
        "ruby" => ("Ruby", "Get it from rubyinstaller.org (Windows) or ruby-lang.org."),
        "scala-cli" => ("Scala CLI", "Get it from scala-cli.virtuslab.org."),
        "swift" => ("Swift", "Get it from swift.org/install."),
        "bash" => ("bash", "On Windows it comes with Git for Windows (git-scm.com)."),
        _ => return None,
    })
}

/// What Run needs for `file` that isn't installed, if anything. Blocking (may start Python).
pub fn missing_for_run(lang: LanguageId, file: &std::path::Path, root: &std::path::Path) -> Option<MissingTool> {
    let program = run_program(lang, file, root)?;
    let installed = match program {
        // The project's .venv, or a real Python (not the Microsoft Store placeholder).
        "python" => crate::python_env::project_python(root).is_some() || crate::python_env::base_python().is_some(),
        _ => which(program).is_some(),
    };
    if installed {
        return None;
    }
    let (what, hint) = describe(program)?;
    Some(MissingTool { program, what, hint })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_programs_and_missing_tools() {
        let root = std::path::Path::new("/nowhere");
        assert_eq!(run_program(LanguageId::Rust, &root.join("main.rs"), root), Some("rustc"));
        assert_eq!(run_program(LanguageId::Cpp, &root.join("a.cpp"), root), Some("g++"));
        assert_eq!(run_program(LanguageId::Markdown, &root.join("a.md"), root), None);
        // Every program Run may need has a description and an install hint.
        for lang in [LanguageId::Rust, LanguageId::C, LanguageId::Cpp, LanguageId::Python, LanguageId::Go, LanguageId::TypeScript, LanguageId::JavaScript, LanguageId::CSharp, LanguageId::Java, LanguageId::Kotlin, LanguageId::Php, LanguageId::Dart, LanguageId::Lua, LanguageId::Ruby, LanguageId::Scala, LanguageId::Swift, LanguageId::Shell] {
            let program = run_program(lang, &root.join("x"), root).unwrap();
            assert!(describe(program).is_some(), "{program}");
        }
        let missing = MissingTool { program: "g++", what: "a C++ compiler (g++)", hint: "Install it." };
        assert_eq!(missing.message(LanguageId::Cpp), "A C++ compiler (g++) isn't installed, so C++ files can't run. Install it. Then restart Graphyne.");
    }
}
