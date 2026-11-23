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

/// `program` on PATH; when it isn't there, PATH is read again from the registry first: a
/// program installed after Graphyne started is found without a restart.
pub fn find(program: &str) -> Option<std::path::PathBuf> {
    which(program).or_else(|| {
        refresh_path();
        which(program)
    })
}

/// Add to Graphyne's own PATH the folders the system's PATH has gained since Graphyne
/// started (an installer added them). Windows only; `GRAPHYNE_KEEP_PATH` turns it off (tests
/// with a PATH that lacks some tools).
pub fn refresh_path() {
    #[cfg(windows)]
    {
        if std::env::var_os("GRAPHYNE_KEEP_PATH").is_some() {
            return;
        }
        let current: Vec<std::path::PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        let key = |p: &std::path::Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
        let known: std::collections::HashSet<String> = current.iter().map(|p| key(p)).collect();
        let mut added: Vec<std::path::PathBuf> = Vec::new();
        for dir in registry_path() {
            if !known.contains(&key(&dir)) && !added.iter().any(|a| key(a) == key(&dir)) {
                added.push(dir);
            }
        }
        if added.is_empty() {
            return;
        }
        log::info!("PATH gained {} folder(s) since start: {:?}", added.len(), added);
        if let Ok(path) = std::env::join_paths(current.into_iter().chain(added)) {
            // SAFETY: Windows guards its environment block itself, and Graphyne's own code
            // reads it through std, which takes the same lock.
            unsafe { std::env::set_var("PATH", path) };
        }
    }
}

/// The system's PATH as the registry has it now: the machine's, then the user's, expanded.
#[cfg(windows)]
fn registry_path() -> Vec<std::path::PathBuf> {
    use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
    use windows_sys::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW};
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    fn read(root: HKEY, key: &str) -> Option<String> {
        let (key, name) = (wide(key), wide("Path"));
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
        let mut size = 0u32;
        // SAFETY: plain Win32 calls with buffers sized as the API asks.
        unsafe {
            if RegGetValueW(root, key.as_ptr(), name.as_ptr(), flags, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) != 0 {
                return None;
            }
            let mut raw = vec![0u16; (size as usize).div_ceil(2) + 1];
            let mut size = (raw.len() * 2) as u32;
            if RegGetValueW(root, key.as_ptr(), name.as_ptr(), flags, std::ptr::null_mut(), raw.as_mut_ptr().cast(), &mut size) != 0 {
                return None;
            }
            let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
            raw.truncate(end);
            raw.push(0);
            let needed = ExpandEnvironmentStringsW(raw.as_ptr(), std::ptr::null_mut(), 0);
            if needed == 0 {
                return Some(String::from_utf16_lossy(&raw[..end]));
            }
            let mut out = vec![0u16; needed as usize];
            let written = ExpandEnvironmentStringsW(raw.as_ptr(), out.as_mut_ptr(), needed) as usize;
            Some(String::from_utf16_lossy(&out[..written.saturating_sub(1).min(out.len())]))
        }
    }
    [read(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"), read(HKEY_CURRENT_USER, "Environment")]
        .into_iter()
        .flatten()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .filter(|p| !p.as_os_str().is_empty())
        .collect()
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
    /// For Run. (Graphyne looks again once it's installed: no restart needed.)
    pub fn message(&self, lang: LanguageId) -> String {
        format!("{} isn't installed, so {} files can't run. {}", capitalize(self.what), lang.name(), self.hint)
    }

    /// For Debug.
    pub fn debug_message(&self, lang: LanguageId) -> String {
        format!("{} isn't installed, so {} can't be debugged. {}", capitalize(self.what), lang.name(), self.hint)
    }
}

/// `program` (one of those Run and Debug use) when it isn't installed: what it is and where
/// to get it.
pub fn missing(program: &'static str) -> Option<MissingTool> {
    if find(program).is_some() {
        return None;
    }
    let (what, hint) = describe(program)?;
    Some(MissingTool { program, what, hint })
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
        "python" => ("Python 3", "Install it from the Python menu in the status bar (Install Python), or from python.org."),
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
    if program == "lua" {
        // Without one on the computer, Run uses Graphyne's Lua.
        return None;
    }
    if program == "python" {
        // The project's .venv, or a real Python (not the Microsoft Store placeholder).
        if crate::python_env::interpreter(root).is_some() || crate::python_env::base_python().is_some() {
            return None;
        }
        let (what, hint) = describe(program)?;
        return Some(MissingTool { program, what, hint });
    }
    missing(program)
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
        assert_eq!(missing.message(LanguageId::Cpp), "A C++ compiler (g++) isn't installed, so C++ files can't run. Install it.");
        assert_eq!(missing.debug_message(LanguageId::Cpp), "A C++ compiler (g++) isn't installed, so C++ can't be debugged. Install it.");
        // Graphyne's own PATH holds at least what the system's PATH had at start.
        #[cfg(windows)]
        assert!(registry_path().iter().any(|d| d.join("cmd.exe").is_file()), "System32 is on the registry's PATH");
    }
}
