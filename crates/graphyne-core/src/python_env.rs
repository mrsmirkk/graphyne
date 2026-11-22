//! A project's Python environment: a `.venv` in the project folder with debugpy in it, so
//! Run and Debug work as soon as a Python file exists — or the interpreter picked for the
//! project in the status bar. Blocking — call from background executors.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Folders recognised as the project's virtual environment (must contain `pyvenv.cfg`).
pub const VENV_DIRS: [&str; 3] = [".venv", "venv", "env"];


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
    let (vars, mut dirs) = match interpreter(root) {
        Some(python) => {
            let bin = python.parent().map(Path::to_path_buf).unwrap_or_default();
            match venv_of(&python) {
                Some(venv) => {
                    let name = venv.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    (vec![("VIRTUAL_ENV".into(), venv.to_string_lossy().to_string()), ("VIRTUAL_ENV_PROMPT".into(), name)], vec![bin])
                }
                None if bin.join("conda-meta").is_dir() => {
                    // A conda environment: what `conda activate` puts on PATH (its DLLs too).
                    let name = bin.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let dirs: Vec<PathBuf> = if cfg!(windows) {
                        [bin.clone(), bin.join("Library").join("mingw-w64").join("bin"), bin.join("Library").join("usr").join("bin"), bin.join("Library").join("bin"), bin.join("Scripts"), bin.join("bin")]
                            .into_iter()
                            .filter(|d| d.is_dir())
                            .collect()
                    } else {
                        vec![bin.clone()]
                    };
                    (vec![("CONDA_PREFIX".into(), bin.to_string_lossy().to_string()), ("CONDA_DEFAULT_ENV".into(), name)], dirs)
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
        None => Default::default(),
    };
    // The uv Graphyne uses, when it isn't on PATH anyway (its own copy): `uv` works there too.
    if let Some(dir) = private_uv().and_then(|p| p.parent().map(Path::to_path_buf)) {
        dirs.push(dir);
    }
    (vars, dirs)
}

// ---------------------------------------------------------------- uv and pip

/// The uv Graphyne downloads when none is installed (official release, checked by SHA-256).
pub const UV_VERSION: &str = "0.13.9";
const UV_SHA256: &str = "edd61f7f5728a56cc62455386661860bdfc7bab65872f39b0c25bdcc1d8e1beb";

static UV_ENABLED: AtomicBool = AtomicBool::new(true);

/// Settings → "Use uv". Off: pip everywhere, and uv is never downloaded.
pub fn set_uv_enabled(on: bool) {
    UV_ENABLED.store(on, Ordering::Relaxed);
}

pub fn uv_enabled() -> bool {
    UV_ENABLED.load(Ordering::Relaxed)
}

fn own_uv_dir() -> Option<PathBuf> {
    Some(crate::session::state_dir()?.join("tools").join(format!("uv-{UV_VERSION}")))
}

/// Graphyne's copy of uv, when downloaded.
fn own_uv() -> Option<PathBuf> {
    let p = own_uv_dir()?.join(if cfg!(windows) { "uv.exe" } else { "uv" });
    p.is_file().then_some(p)
}

/// uv that isn't on PATH: `GRAPHYNE_UV_PATH` or Graphyne's copy.
fn private_uv() -> Option<PathBuf> {
    match std::env::var_os("GRAPHYNE_UV_PATH").filter(|p| !p.is_empty()) {
        Some(p) => Some(PathBuf::from(p)).filter(|p| p.is_file()),
        None => own_uv().filter(|_| crate::toolchain::which("uv").is_none()),
    }
}

/// uv on this computer, without downloading anything: `GRAPHYNE_UV_PATH`, `uv` on PATH or
/// Graphyne's copy.
pub fn find_uv() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GRAPHYNE_UV_PATH").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p)).filter(|p| p.is_file());
    }
    crate::toolchain::which("uv").or_else(own_uv)
}

/// uv, downloading Graphyne's copy (about 15 MB) the first time it's needed.
pub fn get_uv(progress: &dyn Fn(Step)) -> Result<PathBuf, String> {
    if let Some(uv) = find_uv() {
        return Ok(uv);
    }
    if std::env::var_os("GRAPHYNE_UV_PATH").is_some_and(|p| !p.is_empty()) {
        return Err("GRAPHYNE_UV_PATH doesn't point to uv".into());
    }
    if !(cfg!(windows) && cfg!(target_arch = "x86_64")) {
        return Err("uv is not installed (docs.astral.sh/uv)".into());
    }
    let dir = own_uv_dir().ok_or("no folder for Graphyne's state")?;
    progress(Step::DownloadingUv);
    let url = format!("https://github.com/astral-sh/uv/releases/download/{UV_VERSION}/uv-x86_64-pc-windows-msvc.zip");
    let staging = crate::download::fetch_unpacked(&url, UV_SHA256, &dir).map_err(|e| format!("could not download uv {UV_VERSION} ({e})"))?;
    for name in ["uv.exe", "uvx.exe", "uvw.exe"] {
        let _ = std::fs::rename(staging.join(name), dir.join(name));
    }
    let _ = std::fs::remove_dir_all(&staging);
    own_uv().ok_or_else(|| "the uv download has no uv.exe".into())
}

/// The tool that installs a project's packages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Installer {
    Uv,
    Pip,
}

impl Installer {
    pub fn name(self) -> &'static str {
        match self {
            Installer::Uv => "uv",
            Installer::Pip => "pip",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "uv" => Some(Installer::Uv),
            "pip" => Some(Installer::Pip),
            _ => None,
        }
    }
}

/// Installers picked in the status-bar menu, per project root (kept in its saved state).
static INSTALLERS: Mutex<Vec<(PathBuf, Installer)>> = Mutex::new(Vec::new());

pub fn set_installer_choice(root: &Path, choice: Option<Installer>) {
    let mut all = INSTALLERS.lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|(r, _)| !crate::session::same_path(r, root));
    if let Some(choice) = choice {
        all.push((root.to_path_buf(), choice));
    }
}

pub fn installer_choice(root: &Path) -> Option<Installer> {
    INSTALLERS.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(r, _)| crate::session::same_path(r, root)).map(|(_, i)| *i)
}

/// Does a pip config file point pip to a package source of its own?
pub fn pip_config_sets_index(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#') && !l.starts_with(';'))
        .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim().to_ascii_lowercase()))
        .any(|k| matches!(k.as_str(), "index-url" | "extra-index-url" | "find-links" | "no-index"))
}

/// pip is set up with its own package source (a company index, a local folder) that uv
/// doesn't read — and uv has none of its own. Says where.
fn pip_own_index() -> Option<&'static str> {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    if ["UV_INDEX_URL", "UV_DEFAULT_INDEX", "UV_INDEX", "UV_EXTRA_INDEX_URL", "UV_FIND_LINKS", "UV_NO_INDEX"].iter().any(|k| set(k)) {
        return None;
    }
    if ["PIP_INDEX_URL", "PIP_EXTRA_INDEX_URL", "PIP_FIND_LINKS", "PIP_NO_INDEX"].iter().any(|k| set(k)) {
        return Some("environment");
    }
    let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let files: Vec<PathBuf> = if cfg!(windows) {
        [var("PIP_CONFIG_FILE"), var("APPDATA").map(|d| d.join("pip").join("pip.ini")), var("USERPROFILE").map(|d| d.join("pip").join("pip.ini")), var("PROGRAMDATA").map(|d| d.join("pip").join("pip.ini"))]
            .into_iter()
            .flatten()
            .collect()
    } else {
        [var("PIP_CONFIG_FILE"), var("HOME").map(|d| d.join(".config").join("pip").join("pip.conf")), var("HOME").map(|d| d.join(".pip").join("pip.conf")), Some(PathBuf::from("/etc/pip.conf"))]
            .into_iter()
            .flatten()
            .collect()
    };
    files.iter().any(|f| std::fs::read_to_string(f).is_ok_and(|t| pip_config_sets_index(&t))).then_some("pip.ini")
}

/// What installs packages when nothing was picked for the project, and why.
pub fn auto_installer() -> (Installer, &'static str) {
    if !uv_enabled() {
        return (Installer::Pip, "uv is turned off in Settings");
    }
    match pip_own_index() {
        Some("environment") => (Installer::Pip, "PIP_INDEX_URL / PIP_FIND_LINKS give pip its own package source, which uv doesn't read"),
        Some(_) => (Installer::Pip, "pip.ini gives pip its own package source, which uv doesn't read"),
        None => (Installer::Uv, "much faster than pip; pip is used when uv can't be"),
    }
}

/// The project's installer: picked in the menu, else the automatic one.
pub fn installer(root: &Path) -> Installer {
    installer_choice(root).unwrap_or_else(|| auto_installer().0)
}

/// uv for this project if it uses uv (downloaded when needed). None: use pip — with the reason
/// uv couldn't be had, when it was wanted.
fn uv_for(root: &Path, progress: &dyn Fn(Step)) -> (Option<PathBuf>, Option<String>) {
    if installer(root) != Installer::Uv {
        return (None, None);
    }
    match get_uv(progress) {
        Ok(uv) => (Some(uv), None),
        Err(e) => (None, Some(e)),
    }
}

/// `uv pip install --python <python>` or `<python> -m pip install`, ready for package arguments.
fn install_command(uv: Option<&Path>, python: &Path) -> Command {
    match uv {
        Some(uv) => {
            let mut cmd = command(uv);
            cmd.args(["pip", "install", "--python"]).arg(python);
            cmd
        }
        None => {
            let mut cmd = command(python);
            cmd.args(["-m", "pip", "install", "--disable-pip-version-check", "--no-input", "--timeout", "60"]);
            cmd
        }
    }
}

/// The program and arguments the "Install package" quick fix runs in a terminal tab (the
/// package name goes last): uv when the project uses it and it's here, else pip.
pub fn install_package_command(root: &Path, python: &Path) -> (PathBuf, Vec<String>) {
    match find_uv().filter(|_| installer(root) == Installer::Uv) {
        Some(uv) => (uv, vec!["pip".into(), "install".into(), "--python".into(), python.to_string_lossy().to_string()]),
        None => (python.to_path_buf(), vec!["-m".into(), "pip".into(), "install".into()]),
    }
}

/// Python versions offered for "Install Python 3.x" (uv downloads them).
pub const PYTHON_VERSIONS: [&str; 4] = ["3.13", "3.12", "3.11", "3.10"];

/// Install a Python version with uv, into uv's own folder only: no `python3.x.exe` on PATH
/// (`--no-bin`) and no Windows registry entry (`--no-registry`) — Graphyne finds it through uv.
pub fn install_python(version: &str, progress: &dyn Fn(Step)) -> Result<(), String> {
    let uv = get_uv(progress)?;
    progress(Step::InstallingPython);
    run(command(&uv).args(["python", "install", "--no-bin", "--no-registry", version]), &format!("installing Python {version}"))
}

/// Pythons uv installed (`uv python install`), for the interpreter list.
fn uv_pythons() -> Vec<PathBuf> {
    let Some(uv) = find_uv() else { return Vec::new() };
    let Ok(out) = command(&uv).args(["python", "list", "--only-installed", "--output-format", "json"]).output() else { return Vec::new() };
    let Ok(list) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return Vec::new() };
    list.as_array().into_iter().flatten().filter_map(|p| p["path"].as_str()).map(PathBuf::from).filter(|p| p.is_file()).collect()
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
    /// "Python 3.12.4 (.venv)" for an environment, "Python 3.12.4 (conda: data)" for a conda
    /// environment, "Python 3.12.4" for an installation.
    pub fn label(&self) -> String {
        if let Some(name) = self.venv.as_ref().and_then(|v| v.file_name()) {
            return format!("Python {} ({})", self.version, name.to_string_lossy());
        }
        let home = self.path.parent().map(|d| if cfg!(windows) { d.to_path_buf() } else { d.parent().unwrap_or(d).to_path_buf() });
        match home.filter(|h| h.join("conda-meta").is_dir()).as_ref().and_then(|h| h.file_name()) {
            Some(env) => format!("Python {} (conda: {})", self.version, env.to_string_lossy()),
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
    paths.extend(uv_pythons());
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

/// Poetry, PDM, Pipenv or conda project: its tool owns the environment (the file that says
/// so). Graphyne installs the packages with that tool, after asking.
pub fn managed_elsewhere(root: &Path) -> Option<&'static str> {
    crate::python_tools::project_tool(root).map(|(_, file)| file)
}

pub(crate) fn command(program: &Path) -> Command {
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

/// A package that has to be compiled here, without a C/C++ compiler: what to do about it.
pub fn build_tools_hint(output: &str) -> Option<&'static str> {
    let s = output.to_ascii_lowercase();
    (s.contains("microsoft visual c++") || s.contains("vcvarsall") || s.contains("unable to find a compiler") || s.contains("link.exe") && s.contains("not found"))
        .then_some("A package has to be compiled on this computer and needs Microsoft C++ Build Tools (visualstudio.microsoft.com/visual-cpp-build-tools). Or create .venv with an older Python the package has ready-made builds for (status bar → Python).")
}

/// The line of an installer's error output that says what went wrong. uv: `error: …` /
/// `cause: Because …` / `hint: …` (on a console `╰─▶ Because …`) — the cause says the most.
/// pip: its last line.
pub fn error_line(stderr: &str) -> String {
    let clean = |l: &str| l.trim().trim_start_matches(['×', '╰', '─', '▶', '│', ' ']).trim().to_string();
    let lines: Vec<String> = stderr.lines().map(clean).filter(|l| !l.is_empty()).collect();
    lines
        .iter()
        .rev()
        .find(|l| l.starts_with("cause:") || l.starts_with("Because "))
        .or_else(|| lines.iter().rev().find(|l| !l.starts_with("hint:") && !l.starts_with("help:")))
        .map(|l| l.trim_start_matches("cause:").trim().to_string())
        .unwrap_or_else(|| "failed".into())
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).output().map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let last = error_line(&err);
    match build_tools_hint(&err) {
        Some(hint) => Err(format!("{what}: {last} {hint}")),
        None => Err(format!("{what}: {last}")),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    DownloadingUv,
    RemovingVenv,
    CreatingVenv,
    InstallingDebugpy,
    InstallingPackages,
    InstallingPython,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Environment ready; says what had to be done (nothing when it was already there), and
    /// why uv couldn't be used when it was wanted (pip was).
    Ready { python: PathBuf, created_venv: bool, installed_debugpy: bool, uv_note: Option<String> },
    /// Poetry / Pipenv / conda project — left to that tool.
    ManagedElsewhere(&'static str),
}

/// Make sure `root` has a `.venv` with debugpy: create and install what is missing. An
/// installation picked in the status bar is used as it is — nothing is installed into it.
pub fn ensure(root: &Path, progress: impl Fn(Step)) -> Result<Outcome, String> {
    let picked = chosen(root).filter(|p| p.is_file());
    if let Some(python) = picked.as_ref().filter(|p| venv_of(p).is_none()) {
        return Ok(Outcome::Ready { python: python.clone(), created_venv: false, installed_debugpy: false, uv_note: None });
    }
    let existing = picked.or_else(|| project_python(root));
    let needs_debugpy = existing.as_ref().is_none_or(|p| !has_module(p, "debugpy"));
    if existing.is_none() {
        if let Some(tool) = managed_elsewhere(root) {
            return Ok(Outcome::ManagedElsewhere(tool));
        }
    }
    // uv only when there is something to do (it may have to be downloaded).
    let (uv, uv_note) = if existing.is_none() || needs_debugpy { uv_for(root, &progress) } else { (None, None) };
    let (python, created_venv) = match existing {
        Some(python) => (python, false),
        None => {
            let base = base_python().ok_or("Python 3 is not installed. Get it from python.org (or `winget install Python.Python.3.13`), then run again.")?;
            progress(Step::CreatingVenv);
            (new_venv(root, &base, uv.as_deref())?, true)
        }
    };
    let mut installed_debugpy = false;
    if needs_debugpy {
        progress(Step::InstallingDebugpy);
        let with_uv = run(install_command(uv.as_deref(), &python).arg("debugpy").current_dir(root), "installing debugpy");
        // uv couldn't (e.g. a source only pip knows about): pip, if the environment has it.
        if with_uv.is_err() && uv.is_some() && has_module(&python, "pip") {
            run(install_command(None, &python).arg("debugpy").current_dir(root), "installing debugpy")?;
        } else {
            with_uv?;
        }
        installed_debugpy = true;
    }
    Ok(Outcome::Ready { python, created_venv, installed_debugpy, uv_note })
}

/// `.venv` in `root` from the installation `base`: with uv (a fraction of a second; `--seed`
/// puts pip in, so `pip` in a terminal works), else — or when uv fails, e.g. offline —
/// with `python -m venv`.
fn new_venv(root: &Path, base: &Path, uv: Option<&Path>) -> Result<PathBuf, String> {
    let dir = root.join(".venv");
    let existed = dir.exists();
    let by_uv = uv.is_some_and(|uv| run(command(uv).args(["venv", "--seed", "--python"]).arg(base).arg(".venv").current_dir(root), "uv venv").is_ok());
    if !by_uv {
        if uv.is_some() && !existed {
            // What a failed `uv venv` left behind.
            let _ = std::fs::remove_dir_all(&dir);
        }
        run(command(base).args(["-m", "venv", ".venv"]).current_dir(root), "creating .venv")?;
    }
    let python = venv_python(&dir);
    if !python.is_file() {
        return Err("creating .venv: no interpreter in the new environment".into());
    }
    Ok(python)
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
    let (uv, _) = uv_for(root, &progress);
    progress(Step::CreatingVenv);
    new_venv(root, base, uv.as_deref())
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

/// Where a project lists the packages it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepsFile {
    Requirements,
    Pyproject,
    /// A Poetry / PDM / Pipenv / conda project, and the file that says so.
    Tool(crate::python_tools::Tool, &'static str),
}

impl DepsFile {
    pub fn name(self) -> &'static str {
        match self {
            DepsFile::Requirements => "requirements.txt",
            DepsFile::Pyproject => "pyproject.toml",
            DepsFile::Tool(_, file) => file,
        }
    }
}

/// What to do about the packages the project lists (`requirements.txt`, else the
/// dependencies in `pyproject.toml`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Requirements {
    /// Nothing listed, or no virtualenv to install into.
    NotApplicable,
    /// Installed (or declined) for this exact list.
    UpToDate,
    /// Not installed yet: ask. `packages` = how many are listed (0: a tool's project);
    /// `changed` = an earlier version of the list was installed; `uv` = installed with
    /// `uv sync` (uv.lock) — for a tool's project: the tool runs through uv (not installed).
    Ask { packages: usize, changed: bool, file: DepsFile, uv: bool },
}

/// `.venv/.graphyne-requirements`: hashes of the package lists Graphyne installed or the
/// user declined, so the question comes back only when the list changes.
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
    read_marker_at(&marker(venv))
}

fn write_marker(venv: &Path, key: &str, hash: &str) {
    write_marker_at(&marker(venv), key, hash);
}

fn read_marker_at(file: &Path) -> serde_json::Value {
    std::fs::read_to_string(file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(serde_json::Value::Null)
}

fn write_marker_at(file: &Path, key: &str, hash: &str) {
    let mut m = read_marker_at(file);
    if !m.is_object() {
        m = serde_json::json!({});
    }
    m[key] = serde_json::Value::String(hash.to_string());
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(file, m.to_string());
}

/// A tool's project: its environment may live anywhere, so the marker is kept in Graphyne's
/// folder for the project. The hash is of the file that lists the packages.
fn tool_marker(root: &Path, tool: crate::python_tools::Tool, file: &str) -> Option<(PathBuf, String)> {
    let text = std::fs::read_to_string(root.join(file)).ok()?;
    Some((crate::session::project_data_dir(root)?.join("python-packages.json"), content_hash(&format!("{}\n{text}", tool.name()))))
}

/// Lines of a requirements file that name a package (not comments or pip options).
pub fn package_lines(text: &str) -> usize {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-')).count()
}

/// The packages a `pyproject.toml` needs (PEP 508 strings as written): `[project]
/// dependencies` and the `dev` group of `[dependency-groups]`, which uv installs by default.
/// None when the file can't be read as TOML.
pub fn pyproject_dependencies(text: &str) -> Option<Vec<String>> {
    fn group(groups: &toml::Table, name: &str, out: &mut Vec<String>, depth: usize) {
        let Some(items) = groups.get(name).and_then(|g| g.as_array()).filter(|_| depth < 8) else { return };
        for item in items {
            match item {
                toml::Value::String(dep) => out.push(dep.clone()),
                toml::Value::Table(t) => {
                    if let Some(other) = t.get("include-group").and_then(|g| g.as_str()) {
                        group(groups, other, out, depth + 1);
                    }
                }
                _ => {}
            }
        }
    }
    let doc: toml::Table = toml::from_str(text).ok()?;
    let mut deps: Vec<String> = Vec::new();
    if let Some(list) = doc.get("project").and_then(|p| p.get("dependencies")).and_then(|d| d.as_array()) {
        deps.extend(list.iter().filter_map(|v| v.as_str()).map(str::to_string));
    }
    if let Some(groups) = doc.get("dependency-groups").and_then(|g| g.as_table()) {
        group(groups, "dev", &mut deps, 0);
    }
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for d in deps {
        let d = d.trim().to_string();
        if !d.is_empty() && !seen.contains(&d) {
            seen.push(d.clone());
            out.push(d);
        }
    }
    Some(out)
}

/// The project's package list: requirements.txt if it names packages, else pyproject.toml.
struct Deps {
    file: DepsFile,
    /// pyproject.toml's packages (requirements.txt goes to pip as a file).
    packages: Vec<String>,
    count: usize,
    /// Of the list itself (a new version number in pyproject.toml doesn't ask again).
    hash: String,
}

fn project_deps(root: &Path) -> Option<Deps> {
    if let Ok(text) = std::fs::read_to_string(root.join("requirements.txt")) {
        let count = package_lines(&text);
        if count > 0 {
            return Some(Deps { file: DepsFile::Requirements, packages: Vec::new(), count, hash: content_hash(&text) });
        }
    }
    let text = std::fs::read_to_string(root.join("pyproject.toml")).ok()?;
    let packages = pyproject_dependencies(&text)?;
    if packages.is_empty() {
        return None;
    }
    let hash = content_hash(&format!("pyproject\n{}", packages.join("\n")));
    Some(Deps { file: DepsFile::Pyproject, count: packages.len(), packages, hash })
}

/// The virtual environment the packages go into: the one Run uses (never a Python
/// installation picked in the status bar).
fn active_venv(root: &Path) -> Option<PathBuf> {
    interpreter(root).and_then(|p| venv_of(&p))
}

/// A uv project (uv.lock) whose environment is the project's `.venv`: `uv sync` installs
/// exactly the locked versions — unless pip was picked for the project or uv is turned off.
fn uses_uv_sync(root: &Path, venv: &Path) -> bool {
    root.join("uv.lock").is_file()
        && crate::session::same_path(venv, &root.join(".venv"))
        && match installer_choice(root) {
            Some(i) => i == Installer::Uv,
            None => uv_enabled(),
        }
}

pub fn requirements(root: &Path) -> Requirements {
    if let Some((tool, file)) = crate::python_tools::project_tool(root) {
        let Some((marker, hash)) = tool_marker(root, tool, file) else { return Requirements::NotApplicable };
        let m = read_marker_at(&marker);
        if m["installed"] == hash.as_str() || m["declined"] == hash.as_str() {
            return Requirements::UpToDate;
        }
        let through_uv = !crate::python_tools::installed(tool);
        return Requirements::Ask { packages: 0, changed: m["installed"].is_string(), file: DepsFile::Tool(tool, file), uv: through_uv };
    }
    let (Some(venv), Some(deps)) = (active_venv(root), project_deps(root)) else {
        return Requirements::NotApplicable;
    };
    let m = read_marker(&venv);
    if m["installed"] == deps.hash.as_str() || m["declined"] == deps.hash.as_str() {
        return Requirements::UpToDate;
    }
    let uv = deps.file == DepsFile::Pyproject && uses_uv_sync(root, &venv);
    Requirements::Ask { packages: deps.count, changed: m["installed"].is_string(), file: deps.file, uv }
}

/// Install the project's packages into its virtualenv: `uv sync` for a uv project (uv.lock),
/// else requirements.txt / pyproject.toml's dependencies with the project's installer (uv or
/// pip). This runs the packages' install code — only after the user agreed. The error comes
/// with the installer that failed (after uv, pip may be worth a try). A Poetry / PDM / Pipenv /
/// conda project is installed by its tool, and the interpreter of the tool's environment comes
/// back for the project to use.
pub fn install_requirements(root: &Path, progress: impl Fn(Step)) -> Result<(DepsFile, Installer, Option<PathBuf>), (String, Installer)> {
    if let Some((tool, file)) = crate::python_tools::project_tool(root) {
        progress(Step::InstallingPackages);
        // Not an installer of Graphyne's: no "Use pip" offer for it (Installer::Pip).
        let python = crate::python_tools::install(root, tool, file, &progress).map_err(|e| (e, Installer::Pip))?;
        // The tool may have written its lock file (`poetry.lock`, `Pipfile.lock`): mark what
        // is there now, or the next check would take the new file for a change.
        let (tool, file) = crate::python_tools::project_tool(root).unwrap_or((tool, file));
        if let Some((marker, hash)) = tool_marker(root, tool, file) {
            write_marker_at(&marker, "installed", &hash);
        }
        return Ok((DepsFile::Tool(tool, file), Installer::Pip, Some(python)));
    }
    let venv = active_venv(root).ok_or_else(|| ("the project has no .venv".to_string(), Installer::Pip))?;
    let deps = project_deps(root).ok_or_else(|| ("the project lists no packages".to_string(), Installer::Pip))?;
    let python = venv_python(&venv);
    let sync = deps.file == DepsFile::Pyproject && uses_uv_sync(root, &venv);
    let uv = if sync {
        Some(get_uv(&progress).map_err(|e| (e, Installer::Uv))?)
    } else {
        uv_for(root, &progress).0
    };
    let used = if uv.is_some() { Installer::Uv } else { Installer::Pip };
    progress(Step::InstallingPackages);
    let result = match (&uv, sync) {
        // --inexact: keep what isn't in the lock (debugpy).
        (Some(uv), true) => run(command(uv).args(["sync", "--inexact"]).current_dir(root), "uv sync"),
        _ => {
            let mut cmd = install_command(uv.as_deref(), &python);
            match deps.file {
                DepsFile::Requirements => cmd.args(["-r", "requirements.txt"]),
                // (Tool projects returned above; project_deps never gives one.)
                DepsFile::Pyproject | DepsFile::Tool(..) => cmd.args(&deps.packages),
            };
            run(cmd.current_dir(root), &format!("installing the packages from {} with {}", deps.file.name(), used.name()))
        }
    };
    result.map_err(|e| (e, used))?;
    write_marker(&venv, "installed", &deps.hash);
    Ok((deps.file, used, None))
}

/// "Don't ask again" — until the list changes.
pub fn decline_requirements(root: &Path) {
    if let Some((tool, file)) = crate::python_tools::project_tool(root) {
        if let Some((marker, hash)) = tool_marker(root, tool, file) {
            write_marker_at(&marker, "declined", &hash);
        }
        return;
    }
    if let (Some(venv), Some(deps)) = (active_venv(root), project_deps(root)) {
        write_marker(&venv, "declined", &deps.hash);
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
    fn pyproject_dependencies_are_read() {
        let text = r#"
[project]
name = "app"
version = "0.1.0"
dependencies = ["requests>=2.31", "rich", "numpy; python_version >= '3.10'"]

[project.optional-dependencies]
docs = ["mkdocs"]

[dependency-groups]
dev = ["pytest>=8", { include-group = "lint" }, "rich"]
lint = ["ruff"]
"#;
        assert_eq!(
            pyproject_dependencies(text).unwrap(),
            vec!["requests>=2.31", "rich", "numpy; python_version >= '3.10'", "pytest>=8", "ruff"]
        );
        assert_eq!(pyproject_dependencies("[tool.black]\nline-length = 100\n").unwrap(), Vec::<String>::new());
        assert_eq!(pyproject_dependencies("not = [toml"), None);

        // Asked for when there is no requirements.txt; a new version number doesn't ask again.
        let root = std::env::temp_dir().join(format!("graphyne-pyproject-test-{}", std::process::id()));
        let python = venv_python(&root.join(".venv"));
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(root.join(".venv").join("pyvenv.cfg"), "").unwrap();
        std::fs::write(&python, "").unwrap();
        std::fs::write(root.join("pyproject.toml"), text).unwrap();
        assert!(matches!(requirements(&root), Requirements::Ask { packages: 5, file: DepsFile::Pyproject, uv: false, .. }));
        decline_requirements(&root);
        std::fs::write(root.join("pyproject.toml"), text.replace("0.1.0", "0.2.0")).unwrap();
        assert_eq!(requirements(&root), Requirements::UpToDate);
        std::fs::write(root.join("pyproject.toml"), text.replace("\"rich\", \"numpy", "\"rich\", \"httpx\", \"numpy")).unwrap();
        assert!(matches!(requirements(&root), Requirements::Ask { packages: 6, .. }));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn pip_sources_and_errors() {
        assert!(pip_config_sets_index("[global]\nindex-url = https://pypi.example.com/simple\n"));
        assert!(pip_config_sets_index("[install]\nfind-links=C:\\wheels\n"));
        assert!(!pip_config_sets_index("[global]\n# index-url = x\ntimeout = 60\n"));
        assert!(build_tools_hint("error: Microsoft Visual C++ 14.0 or greater is required.").is_some());
        assert!(build_tools_hint("No matching distribution found for foo").is_none());
        let uv_err = "error: No solution found when resolving dependencies\n  cause: Because demo-missing was not found in the cache and you require demo-missing, we can conclude that your requirements are unsatisfiable.\nhint: Packages were unavailable because the network was disabled.\n";
        assert!(error_line(uv_err).starts_with("Because demo-missing was not found"));
        assert_eq!(error_line("Collecting x\nERROR: No matching distribution found for x\n"), "ERROR: No matching distribution found for x");
        assert_eq!(Installer::from_name(Installer::Uv.name()), Some(Installer::Uv));
        let root = Path::new("C:/some/project");
        assert_eq!(installer_choice(root), None);
        set_installer_choice(root, Some(Installer::Pip));
        assert_eq!(installer(root), Installer::Pip);
        set_installer_choice(root, None);
        assert_eq!(installer_choice(root), None);
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
        assert_eq!(requirements(&root), Requirements::Ask { packages: 2, changed: false, file: DepsFile::Requirements, uv: false });
        decline_requirements(&root);
        assert_eq!(requirements(&root), Requirements::UpToDate);
        std::fs::write(root.join("requirements.txt"), "requests>=2
flask
rich
").unwrap();
        assert_eq!(requirements(&root), Requirements::Ask { packages: 3, changed: false, file: DepsFile::Requirements, uv: false }, "asked again after a change");
        std::fs::remove_dir_all(&root).ok();
    }
}
