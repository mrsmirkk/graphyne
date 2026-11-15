//! A project's Python environment: a `.venv` in the project folder with debugpy in it, so
//! Run and Debug work as soon as a Python file exists. Blocking — call from background
//! executors.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Folders recognised as the project's virtual environment (must contain `pyvenv.cfg`).
pub const VENV_DIRS: [&str; 3] = [".venv", "venv", "env"];

/// Files of tools that manage the environment themselves (Graphyne leaves those alone).
const OTHER_MANAGERS: [&str; 4] = ["poetry.lock", "Pipfile", "environment.yml", "environment.yaml"];

pub fn find_venv(root: &Path) -> Option<PathBuf> {
    VENV_DIRS.iter().map(|d| root.join(d)).find(|d| d.join("pyvenv.cfg").is_file())
}

pub fn venv_python(venv: &Path) -> PathBuf {
    if cfg!(windows) { venv.join("Scripts").join("python.exe") } else { venv.join("bin").join("python") }
}

/// The interpreter of the project's virtual environment, if it has one.
pub fn project_python(root: &Path) -> Option<PathBuf> {
    find_venv(root).map(|v| venv_python(&v)).filter(|p| p.is_file())
}

/// Poetry, Pipenv or conda project: its tool owns the environment.
pub fn managed_elsewhere(root: &Path) -> Option<&'static str> {
    OTHER_MANAGERS.iter().copied().find(|f| root.join(f).is_file())
}

fn command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// A real Python 3 installation: the `py` launcher or `python` / `python3` on PATH. Each is
/// asked for its own path, which also rules out the Microsoft Store placeholder `python.exe`
/// (it only prints how to install Python).
pub fn base_python() -> Option<PathBuf> {
    let candidates: &[(&str, &[&str])] = if cfg!(windows) { &[("py", &["-3"]), ("python", &[]), ("python3", &[])] } else { &[("python3", &[]), ("python", &[])] };
    candidates.iter().find_map(|(exe, pre)| {
        let out = command(Path::new(exe))
            .args(*pre)
            .args(["-c", "import sys, venv; print(sys.executable) if sys.version_info >= (3, 8) else exit(1)"])
            .output()
            .ok()?;
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !path.is_empty()).then(|| PathBuf::from(path)).filter(|p| p.is_file())
    })
}

pub fn has_module(python: &Path, module: &str) -> bool {
    command(python).args(["-c", &format!("import {module}")]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).output().map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("failed").trim().to_string();
    Err(format!("{what}: {last}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    CreatingVenv,
    InstallingDebugpy,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Environment ready; says what had to be done (nothing when it was already there).
    Ready { python: PathBuf, created_venv: bool, installed_debugpy: bool },
    /// Poetry / Pipenv / conda project — left to that tool.
    ManagedElsewhere(&'static str),
}

/// Make sure `root` has a `.venv` with debugpy: create and install what is missing.
pub fn ensure(root: &Path, progress: impl Fn(Step)) -> Result<Outcome, String> {
    let (python, created_venv) = match project_python(root) {
        Some(python) => (python, false),
        None => {
            if let Some(tool) = managed_elsewhere(root) {
                return Ok(Outcome::ManagedElsewhere(tool));
            }
            let base = base_python().ok_or("Python 3 is not installed. Get it from python.org (or `winget install Python.Python.3.13`), then run again.")?;
            progress(Step::CreatingVenv);
            run(command(&base).args(["-m", "venv", ".venv"]).current_dir(root), "creating .venv")?;
            let python = venv_python(&root.join(".venv"));
            if !python.is_file() {
                return Err("creating .venv: no interpreter in the new environment".into());
            }
            (python, true)
        }
    };
    let mut installed_debugpy = false;
    if !has_module(&python, "debugpy") {
        progress(Step::InstallingDebugpy);
        run(
            command(&python).args(["-m", "pip", "install", "--disable-pip-version-check", "--no-input", "--timeout", "30", "debugpy"]).current_dir(root),
            "installing debugpy",
        )?;
        installed_debugpy = true;
    }
    Ok(Outcome::Ready { python, created_venv, installed_debugpy })
}

/// What to do about the project's `requirements.txt`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Requirements {
    /// No `requirements.txt`, or no `.venv` to install into.
    NotApplicable,
    /// Installed (or declined) for this exact version of the file.
    UpToDate,
    /// Not installed yet: ask. `packages` = lines naming a package; `changed` = an earlier
    /// version of the file was installed.
    Ask { packages: usize, changed: bool },
}

/// `.venv/.graphyne-requirements`: hashes of the requirements.txt Graphyne installed or the
/// user declined, so the question comes back only when the file changes.
fn marker(venv: &Path) -> PathBuf {
    venv.join(".graphyne-requirements")
}

fn content_hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn read_marker(venv: &Path) -> serde_json::Value {
    std::fs::read_to_string(marker(venv)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(serde_json::Value::Null)
}

fn write_marker(venv: &Path, key: &str, hash: &str) {
    let mut m = read_marker(venv);
    if !m.is_object() {
        m = serde_json::json!({});
    }
    m[key] = serde_json::Value::String(hash.to_string());
    let _ = std::fs::write(marker(venv), m.to_string());
}

/// Lines of a requirements file that name a package (not comments or pip options).
pub fn package_lines(text: &str) -> usize {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-')).count()
}

pub fn requirements(root: &Path) -> Requirements {
    let (Some(venv), Ok(text)) = (find_venv(root), std::fs::read_to_string(root.join("requirements.txt"))) else {
        return Requirements::NotApplicable;
    };
    let packages = package_lines(&text);
    if packages == 0 {
        return Requirements::NotApplicable;
    }
    let hash = content_hash(&text);
    let m = read_marker(&venv);
    if m["installed"] == hash.as_str() || m["declined"] == hash.as_str() {
        return Requirements::UpToDate;
    }
    Requirements::Ask { packages, changed: m["installed"].is_string() }
}

/// `pip install -r requirements.txt` into the project's `.venv` (this runs the packages'
/// install code — only after the user agreed).
pub fn install_requirements(root: &Path) -> Result<(), String> {
    let venv = find_venv(root).ok_or("the project has no .venv")?;
    let python = venv_python(&venv);
    let text = std::fs::read_to_string(root.join("requirements.txt")).map_err(|e| format!("requirements.txt: {e}"))?;
    run(
        command(&python).args(["-m", "pip", "install", "--disable-pip-version-check", "--no-input", "--timeout", "60", "-r", "requirements.txt"]).current_dir(root),
        "installing requirements.txt",
    )?;
    write_marker(&venv, "installed", &content_hash(&text));
    Ok(())
}

/// "Don't ask again" — until requirements.txt changes.
pub fn decline_requirements(root: &Path) {
    if let (Some(venv), Ok(text)) = (find_venv(root), std::fs::read_to_string(root.join("requirements.txt"))) {
        write_marker(&venv, "declined", &content_hash(&text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_venv_by_its_config() {
        let root = std::env::temp_dir().join(format!("graphyne-pyenv-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("venv")).unwrap();
        assert_eq!(find_venv(&root), None, "a folder without pyvenv.cfg is not a venv");
        std::fs::write(root.join("venv").join("pyvenv.cfg"), "home = x\n").unwrap();
        assert_eq!(find_venv(&root), Some(root.join("venv")));
        assert_eq!(project_python(&root), None, "no interpreter inside yet");
        let python = venv_python(&root.join("venv"));
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, "").unwrap();
        assert_eq!(project_python(&root), Some(python));
        assert_eq!(managed_elsewhere(&root), None);
        std::fs::write(root.join("poetry.lock"), "").unwrap();
        assert_eq!(managed_elsewhere(&root), Some("poetry.lock"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn requirements_are_asked_once_per_version() {
        let root = std::env::temp_dir().join(format!("graphyne-req-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".venv")).unwrap();
        assert_eq!(requirements(&root), Requirements::NotApplicable, "no venv config, no file");
        std::fs::write(root.join(".venv").join("pyvenv.cfg"), "").unwrap();
        std::fs::write(root.join("requirements.txt"), "# deps
requests>=2
-r other.txt

flask
").unwrap();
        assert_eq!(requirements(&root), Requirements::Ask { packages: 2, changed: false });
        decline_requirements(&root);
        assert_eq!(requirements(&root), Requirements::UpToDate);
        std::fs::write(root.join("requirements.txt"), "requests>=2
flask
rich
").unwrap();
        assert_eq!(requirements(&root), Requirements::Ask { packages: 3, changed: false }, "asked again after a change");
        std::fs::remove_dir_all(&root).ok();
    }
}
