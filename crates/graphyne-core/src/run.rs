//! Universal One-Click Run planner (spec §6.2).

use crate::language::LanguageId;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind {
    PowerShell,
    Posix,
}

impl ShellKind {
    pub fn native() -> Self {
        if cfg!(windows) { ShellKind::PowerShell } else { ShellKind::Posix }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunPlan {
    /// Execute `command` inside the active PTY after changing into `cwd`.
    Pty { label: String, cwd: PathBuf, command: String },
    /// Serve the folder with the live-reload preview server.
    WebPreview { root: PathBuf, entry: PathBuf },
    /// Execute the SQL buffer in the active database pool.
    Sql { file: PathBuf },
    /// Nothing sensible to run.
    Unsupported { reason: String },
}

pub fn quote(shell: ShellKind, p: &Path) -> String {
    let s = p.to_string_lossy();
    match shell {
        ShellKind::PowerShell => format!("'{}'", s.replace('\'', "''")),
        ShellKind::Posix => format!("'{}'", s.replace('\'', "'\\''")),
    }
}

/// Walk up from `start` looking for a marker file (e.g. `Cargo.toml`), bounded by `root`.
pub fn find_up(start: &Path, root: &Path, marker: &str) -> Option<PathBuf> {
    let mut dir = if start.is_dir() { Some(start) } else { start.parent() };
    while let Some(d) = dir {
        if d.join(marker).exists() {
            return Some(d.to_path_buf());
        }
        if d == root {
            break;
        }
        dir = d.parent();
    }
    None
}

/// The nearest project file with extension `ext` (`csproj`) in the folders from `start` up to
/// `root`.
pub fn find_project_file(start: &Path, root: &Path, ext: &str) -> Option<PathBuf> {
    let mut dir = if start.is_dir() { Some(start) } else { start.parent() };
    while let Some(d) = dir {
        if let Ok(entries) = std::fs::read_dir(d) {
            let mut found: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case(ext))).collect();
            found.sort();
            if let Some(p) = found.into_iter().next() {
                return Some(p);
            }
        }
        if d == root {
            break;
        }
        dir = d.parent();
    }
    None
}

/// Build the plan for running `file` inside `project_root`.
pub fn plan(file: &Path, project_root: &Path, shell: ShellKind) -> RunPlan {
    let lang = LanguageId::from_path(file);
    let dir = file.parent().unwrap_or(project_root).to_path_buf();
    let f = quote(shell, file);
    let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp_exe = match shell {
        ShellKind::PowerShell => "\"$env:TEMP\\graphyne-run.exe\"".to_string(),
        ShellKind::Posix => "/tmp/graphyne-run".to_string(),
    };
    let and_then = |build: String, run: String| match shell {
        ShellKind::PowerShell => format!("{build}; if ($?) {{ & {run} }}"),
        ShellKind::Posix => format!("{build} && {run}"),
    };
    let py = if cfg!(windows) { "python" } else { "python3" };
    let pty = |label: &str, cwd: PathBuf, command: String| RunPlan::Pty { label: label.to_string(), cwd, command };

    match lang {
        LanguageId::Rust => match find_up(file, project_root, "Cargo.toml") {
            Some(crate_dir) => pty("cargo run", crate_dir, "cargo run".into()),
            None => pty(&name, dir, and_then(format!("rustc {f} -o {tmp_exe}"), tmp_exe.clone())),
        },
        LanguageId::C => pty(&name, dir, and_then(format!("gcc {f} -o {tmp_exe}"), tmp_exe.clone())),
        LanguageId::Cpp => pty(&name, dir, and_then(format!("g++ -std=c++20 {f} -o {tmp_exe}"), tmp_exe.clone())),
        LanguageId::Python => match crate::python_env::interpreter(project_root) {
            // The project's .venv (PowerShell needs `&` to run a quoted path).
            Some(venv) => {
                let exe = quote(shell, &venv);
                pty(&name, dir, if shell == ShellKind::PowerShell { format!("& {exe} {f}") } else { format!("{exe} {f}") })
            }
            None => pty(&name, dir, format!("{py} {f}")),
        },
        LanguageId::Go => match find_up(file, project_root, "go.mod") {
            Some(m) => pty("go run .", m, "go run .".into()),
            None => pty(&name, dir, format!("go run {f}")),
        },
        LanguageId::TypeScript if crate::toolchain::typescript_runner() == "node" => pty(&name, dir, format!("node {f}")),
        LanguageId::TypeScript | LanguageId::Tsx => pty(&name, dir, format!("bun run {f}")),
        LanguageId::JavaScript => pty(&name, dir, format!("node {f}")),
        // A project, else the file alone (.NET 10 runs a single .cs file).
        LanguageId::CSharp => match find_project_file(file, project_root, "csproj") {
            Some(proj) => {
                let cwd = proj.parent().map(Path::to_path_buf).unwrap_or(dir);
                pty("dotnet run", cwd, format!("dotnet run --project {}", quote(shell, &proj)))
            }
            None => pty(&name, dir, format!("dotnet run {f}")),
        },
        LanguageId::Java => pty(&name, dir, format!("java {f}")),
        LanguageId::Kotlin => {
            let jar = match shell {
                ShellKind::PowerShell => "\"$env:TEMP\\graphyne-run.jar\"".to_string(),
                ShellKind::Posix => "/tmp/graphyne-run.jar".to_string(),
            };
            let build = format!("kotlinc {f} -include-runtime -d {jar}");
            let run = match shell {
                ShellKind::PowerShell => format!("{build}; if ($?) {{ java -jar {jar} }}"),
                ShellKind::Posix => format!("{build} && java -jar {jar}"),
            };
            pty(&name, dir, run)
        }
        LanguageId::Php => pty(&name, dir, format!("php {f}")),
        LanguageId::Dart => pty(&name, dir, format!("dart run {f}")),
        // The computer's Lua, else Graphyne's (see `prepare`).
        LanguageId::Lua => match crate::lua::run_lua() {
            Some(lua) => {
                let exe = quote(shell, &lua);
                pty(&name, dir, if shell == ShellKind::PowerShell { format!("& {exe} {f}") } else { format!("{exe} {f}") })
            }
            None => pty(&name, dir, format!("lua {f}")),
        },
        LanguageId::Ruby => pty(&name, dir, format!("ruby {f}")),
        LanguageId::Scala => pty("scala-cli run .", dir, "scala-cli run .".into()),
        LanguageId::Swift => pty(&name, dir, format!("swift {f}")),
        LanguageId::Shell => pty(&name, dir, format!("bash {f}")),
        LanguageId::PowerShell => pty(&name, dir, format!("& {f}")),
        LanguageId::Html | LanguageId::Css => RunPlan::WebPreview { root: dir, entry: file.to_path_buf() },
        LanguageId::Sql => RunPlan::Sql { file: file.to_path_buf() },
        LanguageId::Plugin(ix) => match crate::language::plugin_language(ix).and_then(|l| l.run) {
            Some(template) => {
                let d = quote(shell, &dir);
                let stem = file.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                pty(&name, dir, template.replace("{file}", &f).replace("{dir}", &d).replace("{name}", &stem))
            }
            None => RunPlan::Unsupported { reason: format!("The {} extension has no run command", lang.name()) },
        },
        _ => RunPlan::Unsupported { reason: format!("No run configuration for {}", lang.name()) },
    }
}

/// What Run fetches before it can run `lang` (Graphyne brings Lua itself): the note to show
/// while it does, or `None` when nothing has to be downloaded.
pub fn prepare_note(lang: LanguageId) -> Option<String> {
    match lang {
        LanguageId::Lua if crate::lua::run_lua().is_none() => {
            Some(format!("No Lua on this computer: downloading Graphyne's Lua 5.4 (with the debugger lua-debug {}, about 10 MB, once)…", crate::lua::LUA_DEBUG_VERSION))
        }
        _ => None,
    }
}

/// Fetch what Run needs for `lang` that Graphyne brings itself. Blocking.
pub fn prepare(lang: LanguageId) -> Result<(), String> {
    match lang {
        LanguageId::Lua => crate::lua::prepare_run(&|_| {}).map(|_| ()),
        _ => Ok(()),
    }
}

/// The text to type into the PTY for a plan (cd + command + newline).
pub fn pty_line(shell: ShellKind, cwd: &Path, command: &str) -> String {
    let cd = quote(shell, cwd);
    match shell {
        ShellKind::PowerShell => format!("Set-Location {cd}; {command}\r"),
        ShellKind::Posix => format!("cd {cd} && {command}\r"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn csharp_plans() {
        let tmp = std::env::temp_dir().join(format!("graphyne-cs-{}", std::process::id()));
        let app = tmp.join("App");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(tmp.join("script.cs"), "").unwrap();
        std::fs::write(app.join("Program.cs"), "").unwrap();
        std::fs::write(app.join("App.csproj"), "").unwrap();
        let cmd = |file: &Path| match plan(file, &tmp, ShellKind::PowerShell) {
            RunPlan::Pty { command, .. } => command,
            other => panic!("{other:?}"),
        };
        assert!(cmd(&app.join("Program.cs")).starts_with("dotnet run --project ") && cmd(&app.join("Program.cs")).ends_with("App.csproj'"));
        assert!(cmd(&tmp.join("script.cs")).starts_with("dotnet run '"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn python_plan_uses_the_project_venv() {
        let root = std::env::temp_dir().join(format!("graphyne-run-venv-{}", std::process::id()));
        let venv = root.join(".venv");
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), "").unwrap();
        let python = crate::python_env::venv_python(&venv);
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, "").unwrap();
        match plan(&root.join("main.py"), &root, ShellKind::PowerShell) {
            RunPlan::Pty { command, .. } => assert!(command.starts_with("& ") && command.contains(".venv"), "{command}"),
            _ => panic!("expected a terminal run"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn python_plan() {
        let root = Path::new("/p");
        match plan(Path::new("/p/src/main.py"), root, ShellKind::Posix) {
            RunPlan::Pty { command, .. } => assert!(command.ends_with("'/p/src/main.py'")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(plan(Path::new("/p/index.html"), root, ShellKind::Posix), RunPlan::WebPreview { .. }));
    }
}
