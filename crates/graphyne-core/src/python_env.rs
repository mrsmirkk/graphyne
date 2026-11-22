//! A project's Python environment: a `.venv` in the project folder with debugpy in it, so
//! Run and Debug work as soon as a Python file exists — or the interpreter picked for the
//! project in the status bar. Blocking — call from background executors.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

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

/// Interpreters picked in the status bar, per project root. The UI keeps the choice in the
/// project's saved state and registers it here, so Run, Debug, the language server and the
/// terminals all use the same Python.
static CHOSEN: Mutex<Vec<(PathBuf, PathBuf)>> = Mutex::new(Vec::new());

pub fn set_chosen(root: &Path, python: Option<PathBuf>) {
    let mut chosen = CHOSEN.lock().unwrap_or_else(|e| e.into_inner());
    chosen.retain(|(r, _)| !crate::session::same_path(r, root));
    if let Some(python) = python {
        chosen.push((root.to_path_buf(), python));
    }
}

pub fn chosen(root: &Path) -> Option<PathBuf> {
    CHOSEN.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(r, _)| crate::session::same_path(r, root)).map(|(_, p)| p.clone())
}

/// The Python that Run, Debug, the language server and terminals use: the one picked for
/// the project if it still exists, else the project's virtual environment.
pub fn interpreter(root: &Path) -> Option<PathBuf> {
    chosen(root).filter(|p| p.is_file()).or_else(|| project_python(root))
}

/// The virtual environment an interpreter belongs to (`<venv>/Scripts/python.exe`).
pub fn venv_of(python: &Path) -> Option<PathBuf> {
    let venv = python.parent()?.parent()?;
    venv.join("pyvenv.cfg").is_file().then(|| venv.to_path_buf())
}

/// Variables and folders (to put first on PATH) for a terminal of this project, so `python`
/// and `pip` typed there are the project's interpreter — like an activated virtualenv.
pub fn terminal_env(root: &Path) -> (Vec<(String, String)>, Vec<PathBuf>) {
    let Some(python) = interpreter(root) else { return Default::default() };
    let Some(bin) = python.parent().map(Path::to_path_buf) else { return Default::default() };
    match venv_of(&python) {
        Some(venv) => {
            let name = venv.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            (vec![("VIRTUAL_ENV".into(), venv.to_string_lossy().to_string()), ("VIRTUAL_ENV_PROMPT".into(), name)], vec![bin])
        }
        None => {
            // A Python installation: pip and other tools live in its Scripts folder.
            let scripts = bin.join("Scripts");
            let mut dirs = vec![bin];
            if cfg!(windows) && scripts.is_dir() {
                dirs.push(scripts);
            }
            (Vec::new(), dirs)
        }
    }
}

/// A Python found on this computer.
#[derive(Clone, Debug, PartialEq)]
pub struct Interpreter {
    pub path: PathBuf,
    /// "3.12.4".
    pub version: String,
    /// Its virtual environment, if it is one.
    pub venv: Option<PathBuf>,
}

impl Interpreter {
    /// "Python 3.12.4 (.venv)" for an environment, "Python 3.12.4" for an installation.
    pub fn label(&self) -> String {
        match self.venv.as_ref().and_then(|v| v.file_name()) {
            Some(name) => format!("Python {} ({})", self.version, name.to_string_lossy()),
            None => format!("Python {}", self.version),
        }
    }
}

/// Run a Python and read back its real path and version (UTF-8 even for non-ASCII paths).
fn probe(mut cmd: Command) -> Option<Interpreter> {
    let out = cmd
        .args(["-c", "import sys; print(sys.executable); print('%d.%d.%d' % sys.version_info[:3])"])
        .env("PYTHONIOENCODING", "utf-8")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines().map(str::trim);
    let path = PathBuf::from(lines.next()?);
    let version = lines.next()?.to_string();
    (path.is_file() && version.split('.').count() == 3).then(|| Interpreter { venv: venv_of(&path), path, version })
}

/// What one interpreter is (None for something that isn't a working Python, such as the
/// Microsoft Store placeholder `python.exe`).
pub fn inspect(python: &Path) -> Option<Interpreter> {
    probe(command(python))
}

/// Tags of `py -0` lines: ` -V:3.12 *        Python 3.12 (64-bit)` → `-V:3.12` (older
/// launchers print `-3.12-64`).
fn py_launcher_tags(list: &str) -> Vec<String> {
    list.lines().filter_map(|l| l.split_whitespace().next()).filter(|t| t.starts_with('-') && t.len() > 1).map(str::to_string).collect()
}

/// Conda environments listed in `~/.conda/environments.txt`.
fn conda_envs() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(Path::new(&home).join(".conda").join("environments.txt")) else { return Vec::new() };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|env| if cfg!(windows) { Path::new(env).join("python.exe") } else { Path::new(env).join("bin").join("python") })
        .filter(|p| p.is_file())
        .collect()
}

/// `python` / `python3` in each PATH folder (not the Microsoft Store placeholders).
fn on_path() -> Vec<PathBuf> {
    let names: &[&str] = if cfg!(windows) { &["python.exe", "python3.exe"] } else { &["python3", "python"] };
    let dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.into_iter()
        .filter(|d| !d.to_string_lossy().to_lowercase().contains("windowsapps"))
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .filter(|p| p.is_file())
        .collect()
}

fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// Pythons this project could use: its virtual environments, the one picked before, the
/// installations the `py` launcher knows (Windows), conda environments and `python` on PATH.
/// Each is run once to learn its version, in parallel.
pub fn discover(root: &Path) -> Vec<Interpreter> {
    let mut jobs: Vec<Command> = Vec::new();
    let mut paths: Vec<PathBuf> = VENV_DIRS.iter().map(|d| venv_python(&root.join(d))).filter(|p| p.is_file()).collect();
    paths.extend(chosen(root).filter(|p| p.is_file()));
    if cfg!(windows) {
        if let Ok(out) = command(Path::new("py")).arg("-0").output() {
            for tag in py_launcher_tags(&String::from_utf8_lossy(&out.stdout)) {
                let mut cmd = command(Path::new("py"));
                cmd.arg(tag);
                jobs.push(cmd);
            }
        }
    }
    paths.extend(conda_envs());
    paths.extend(on_path());
    jobs.extend(paths.iter().map(|p| command(p)));
    let found: Vec<Option<Interpreter>> = std::thread::scope(|s| {
        let handles: Vec<_> = jobs.into_iter().map(|cmd| s.spawn(move || probe(cmd))).collect();
        handles.into_iter().map(|h| h.join().ok().flatten()).collect()
    });
    let mut seen = Vec::new();
    let mut out: Vec<Interpreter> = found.into_iter().flatten().filter(|i| {
        let key = path_key(&i.path);
        !seen.contains(&key) && {
            seen.push(key);
            true
        }
    })
    .collect();
    // The project's environments first, then installations from the newest version.
    let version_key = |i: &Interpreter| i.version.split('.').map(|n| n.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    out.sort_by(|a, b| {
        let in_project = |i: &Interpreter| i.venv.as_ref().is_some_and(|v| v.starts_with(root));
        in_project(b).cmp(&in_project(a)).then_with(|| a.venv.is_some().cmp(&b.venv.is_some()).reverse()).then_with(|| version_key(b).cmp(&version_key(a)))
    });
    out
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
    RemovingVenv,
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

/// Make sure `root` has a `.venv` with debugpy: create and install what is missing. An
/// installation picked in the status bar is used as it is — nothing is installed into it.
pub fn ensure(root: &Path, progress: impl Fn(Step)) -> Result<Outcome, String> {
    let picked = chosen(root).filter(|p| p.is_file());
    if let Some(python) = picked.as_ref().filter(|p| venv_of(p).is_none()) {
        return Ok(Outcome::Ready { python: python.clone(), created_venv: false, installed_debugpy: false });
    }
    let (python, created_venv) = match picked.or_else(|| project_python(root)) {
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

/// A new `.venv` for the project from `base` (a Python installation). The current `.venv`, if
/// any, goes to the Recycle Bin first. Returns the new environment's interpreter; debugpy is
/// added later by [`ensure`].
pub fn create_venv(root: &Path, base: &Path, progress: impl Fn(Step)) -> Result<PathBuf, String> {
    let dir = root.join(".venv");
    if dir.exists() {
        progress(Step::RemovingVenv);
        crate::fs_ops::move_to_trash(&dir).map_err(|e| format!("moving the old .venv to the Recycle Bin: {e} (is a program from it still running?)"))?;
    }
    progress(Step::CreatingVenv);
    run(command(base).args(["-m", "venv", ".venv"]).current_dir(root), "creating .venv")?;
    let python = venv_python(&dir);
    if !python.is_file() {
        return Err("creating .venv: no interpreter in the new environment".into());
    }
    Ok(python)
}

/// The top-level module of a missing import in a Pyright / Pylance diagnostic:
/// `Import "yaml.loader" could not be resolved (from source)` → `yaml`.
pub fn missing_import(message: &str) -> Option<String> {
    let rest = message.strip_prefix("Import \"")?;
    let (module, tail) = rest.split_once('"')?;
    if !tail.trim_start().starts_with("could not be resolved") {
        return None;
    }
    let top = module.split('.').next()?;
    (!top.is_empty() && top.chars().all(|c| c.is_alphanumeric() || c == '_')).then(|| top.to_string())
}

/// The pip package that provides a module, for the common cases where the names differ.
pub fn pip_package(module: &str) -> &str {
    match module {
        "cv2" => "opencv-python",
        "PIL" => "pillow",
        "sklearn" => "scikit-learn",
        "yaml" => "pyyaml",
        "bs4" => "beautifulsoup4",
        "dotenv" => "python-dotenv",
        "dateutil" => "python-dateutil",
        "jwt" => "pyjwt",
        "serial" => "pyserial",
        "usb" => "pyusb",
        "Crypto" => "pycryptodome",
        "OpenSSL" => "pyopenssl",
        "magic" => "python-magic",
        "docx" => "python-docx",
        "pptx" => "python-pptx",
        "telegram" => "python-telegram-bot",
        "win32api" | "win32con" | "win32gui" | "pythoncom" | "pywintypes" => "pywin32",
        "attr" => "attrs",
        "skimage" => "scikit-image",
        "fitz" => "pymupdf",
        "MySQLdb" => "mysqlclient",
        "psycopg2" => "psycopg2-binary",
        "zmq" => "pyzmq",
        "Levenshtein" => "python-Levenshtein",
        other => other,
    }
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

/// The virtual environment requirements.txt goes into: the one Run uses (never a Python
/// installation picked in the status bar).
fn active_venv(root: &Path) -> Option<PathBuf> {
    interpreter(root).and_then(|p| venv_of(&p))
}

pub fn requirements(root: &Path) -> Requirements {
    let (Some(venv), Ok(text)) = (active_venv(root), std::fs::read_to_string(root.join("requirements.txt"))) else {
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
    let venv = active_venv(root).ok_or("the project has no .venv")?;
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
    if let (Some(venv), Ok(text)) = (active_venv(root), std::fs::read_to_string(root.join("requirements.txt"))) {
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
    fn missing_imports_from_pyright_messages() {
        assert_eq!(missing_import("Import \"numpy\" could not be resolved").as_deref(), Some("numpy"));
        assert_eq!(missing_import("Import \"yaml.loader\" could not be resolved from source").as_deref(), Some("yaml"));
        assert_eq!(missing_import("\"foo\" is not defined"), None);
        assert_eq!(missing_import("Import \"a b\" could not be resolved"), None);
        assert_eq!(pip_package("cv2"), "opencv-python");
        assert_eq!(pip_package("requests"), "requests");
    }

    #[test]
    fn py_launcher_list_is_parsed() {
        let list = " -V:3.14 *        Python 3.14 (64-bit)\n -V:3.10          Python 3.10 (64-bit)\n";
        assert_eq!(py_launcher_tags(list), vec!["-V:3.14", "-V:3.10"]);
        assert_eq!(py_launcher_tags("Installed Pythons found by py Launcher for Windows\n -3.9-64 *\n"), vec!["-3.9-64"]);
    }

    #[test]
    fn the_picked_interpreter_wins_and_shapes_terminals() {
        let root = std::env::temp_dir().join(format!("graphyne-pick-test-{}", std::process::id()));
        let venv = root.join(".venv");
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), "").unwrap();
        let in_venv = venv_python(&venv);
        std::fs::create_dir_all(in_venv.parent().unwrap()).unwrap();
        std::fs::write(&in_venv, "").unwrap();
        assert_eq!(interpreter(&root), Some(in_venv.clone()));
        assert_eq!(venv_of(&in_venv), Some(venv.clone()));
        let (vars, dirs) = terminal_env(&root);
        assert!(vars.iter().any(|(k, v)| k == "VIRTUAL_ENV" && Path::new(v) == venv));
        assert_eq!(dirs, vec![in_venv.parent().unwrap().to_path_buf()]);

        // An installation elsewhere, picked for this project.
        let install = root.join("python-install");
        std::fs::create_dir_all(install.join("Scripts")).unwrap();
        let system = install.join(if cfg!(windows) { "python.exe" } else { "python" });
        std::fs::write(&system, "").unwrap();
        set_chosen(&root, Some(system.clone()));
        assert_eq!(interpreter(&root), Some(system.clone()));
        assert_eq!(venv_of(&system), None);
        let (vars, dirs) = terminal_env(&root);
        assert!(vars.is_empty());
        assert_eq!(dirs[0], install);
        assert_eq!(requirements(&root), Requirements::NotApplicable, "nothing is installed into a Python installation");
        // A picked interpreter that was deleted: back to the project's environment.
        std::fs::remove_file(&system).unwrap();
        assert_eq!(interpreter(&root), Some(in_venv));
        set_chosen(&root, None);
        assert_eq!(chosen(&root), None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn requirements_are_asked_once_per_version() {
        let root = std::env::temp_dir().join(format!("graphyne-req-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".venv")).unwrap();
        assert_eq!(requirements(&root), Requirements::NotApplicable, "no venv config, no file");
        std::fs::write(root.join(".venv").join("pyvenv.cfg"), "").unwrap();
        let python = venv_python(&root.join(".venv"));
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, "").unwrap();
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
