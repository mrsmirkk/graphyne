//! Python projects whose environment belongs to another tool — Poetry, PDM, Pipenv or conda.
//! Graphyne runs that tool to install the packages (through uv when the tool itself isn't
//! installed; conda has to be) and then uses the environment the tool made. Blocking — call
//! from background threads.

use crate::python_env::{Step, command, error_line, get_uv, venv_python};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Poetry,
    Pdm,
    Pipenv,
    Conda,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Poetry => "Poetry",
            Tool::Pdm => "PDM",
            Tool::Pipenv => "Pipenv",
            Tool::Conda => "conda",
        }
    }

    /// The tool's command.
    fn exe(self) -> &'static str {
        match self {
            Tool::Poetry => "poetry",
            Tool::Pdm => "pdm",
            Tool::Pipenv => "pipenv",
            Tool::Conda => "conda",
        }
    }

    /// The PyPI package with the command, for `uv tool run` (conda isn't one).
    fn package(self) -> Option<&'static str> {
        match self {
            Tool::Poetry => Some("poetry"),
            Tool::Pdm => Some("pdm"),
            Tool::Pipenv => Some("pipenv"),
            Tool::Conda => None,
        }
    }

    /// What installing the packages runs, for messages (`file` = the project's marker file).
    pub fn install_text(self, root: &Path) -> &'static str {
        match self {
            Tool::Poetry => "poetry install",
            Tool::Pdm if root.join("pdm.lock").is_file() => "pdm sync",
            Tool::Pdm => "pdm install",
            Tool::Pipenv if root.join("Pipfile.lock").is_file() => "pipenv sync --dev",
            Tool::Pipenv => "pipenv install --dev",
            Tool::Conda => "conda env update",
        }
    }
}

/// The tool that owns the project's environment, and the file that says so. A uv project
/// (`uv.lock`) is uv's, handled with the other installers.
pub fn project_tool(root: &Path) -> Option<(Tool, &'static str)> {
    if root.join("uv.lock").is_file() {
        return None;
    }
    let files = [
        ("poetry.lock", Tool::Poetry),
        ("pdm.lock", Tool::Pdm),
        ("Pipfile.lock", Tool::Pipenv),
        ("Pipfile", Tool::Pipenv),
        ("environment.yml", Tool::Conda),
        ("environment.yaml", Tool::Conda),
    ];
    if let Some((file, tool)) = files.into_iter().find(|(f, _)| root.join(f).is_file()) {
        return Some((tool, file));
    }
    // A Poetry project that hasn't been locked yet.
    let text = std::fs::read_to_string(root.join("pyproject.toml")).ok()?;
    let doc: toml::Table = toml::from_str(&text).ok()?;
    doc.get("tool").and_then(|t| t.get("poetry")).and_then(|p| p.get("dependencies")).is_some().then_some((Tool::Poetry, "pyproject.toml"))
}

/// The tool is installed (else Poetry, PDM and Pipenv run through uv).
pub fn installed(tool: Tool) -> bool {
    match tool {
        Tool::Conda => conda().is_some(),
        _ => crate::toolchain::which(tool.exe()).is_some(),
    }
}

/// conda, mamba (same commands) or micromamba (its own).
#[derive(Clone, Debug)]
enum Conda {
    Conda(PathBuf),
    Micromamba(PathBuf),
}

fn conda() -> Option<Conda> {
    let which = crate::toolchain::which;
    which("mamba")
        .or_else(|| which("conda"))
        .or_else(|| std::env::var_os("CONDA_EXE").map(PathBuf::from).filter(|p| p.is_file()))
        .map(Conda::Conda)
        .or_else(|| which("micromamba").map(Conda::Micromamba))
}

/// The tool's command: its own if installed, else `uv tool run --from <package> <command>`
/// (uv keeps the tool in its cache; downloaded the first time).
fn tool_command(tool: Tool, progress: &dyn Fn(Step)) -> Result<Command, String> {
    if let Some(exe) = crate::toolchain::which(tool.exe()) {
        return Ok(command(&exe));
    }
    let package = tool.package().ok_or("conda isn't installed: get Miniforge (conda-forge.org/download) or micromamba, then try again")?;
    let uv = get_uv(progress).map_err(|e| format!("{} isn't installed, and uv to run it isn't either: {e}", tool.name()))?;
    let mut cmd = command(&uv);
    cmd.args(["tool", "run", "--from", package, tool.exe()]);
    Ok(cmd)
}

/// Run and read stdout (trimmed); on failure, the line of the error that says the most.
fn output(cmd: &mut Command, what: &str) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let mut err = String::from_utf8_lossy(&out.stderr).to_string();
        if err.trim().is_empty() {
            err = String::from_utf8_lossy(&out.stdout).to_string();
        }
        Err(format!("{what}: {}", error_line(&err)))
    }
}

/// `name:` of an environment.yml.
pub fn conda_env_name(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("name:"))
        .map(|v| v.split('#').next().unwrap_or("").trim().trim_matches(['"', '\'']).to_string())
        .filter(|n| !n.is_empty())
}

/// Where a named conda environment lives, if it exists.
fn conda_env_prefix(cmd: Command, name: &str) -> Option<PathBuf> {
    let mut cmd = cmd;
    let text = output(cmd.args(["env", "list", "--json"]), "conda env list").ok()?;
    let list: serde_json::Value = serde_json::from_str(&text).ok()?;
    list["envs"].as_array()?.iter().filter_map(|p| p.as_str()).map(PathBuf::from).find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(name)))
}

fn conda_python(prefix: &Path) -> PathBuf {
    if cfg!(windows) { prefix.join("python.exe") } else { prefix.join("bin").join("python") }
}

/// Install the project's packages with its tool; returns the interpreter of the environment
/// the tool made (or already had).
pub fn install(root: &Path, tool: Tool, file: &str, progress: &dyn Fn(Step)) -> Result<PathBuf, String> {
    let at = |mut cmd: Command| {
        cmd.current_dir(root);
        cmd
    };
    let python = match tool {
        Tool::Poetry => {
            output(at(tool_command(tool, progress)?).args(["install", "--no-interaction"]), "poetry install")?;
            let env = output(at(tool_command(tool, progress)?).args(["env", "info", "--path"]), "poetry env info")?;
            venv_python(Path::new(&env))
        }
        Tool::Pdm => {
            let verb = if root.join("pdm.lock").is_file() { "sync" } else { "install" };
            output(at(tool_command(tool, progress)?).arg(verb), &format!("pdm {verb}"))?;
            PathBuf::from(output(at(tool_command(tool, progress)?).args(["info", "--python"]), "pdm info")?)
        }
        Tool::Pipenv => {
            let quiet = |mut cmd: Command| {
                cmd.env("PIPENV_YES", "1").env("PIPENV_NOSPIN", "1").env("PIPENV_VERBOSITY", "-1");
                cmd
            };
            let verb = if root.join("Pipfile.lock").is_file() { "sync" } else { "install" };
            output(quiet(at(tool_command(tool, progress)?)).args([verb, "--dev"]), &format!("pipenv {verb}"))?;
            PathBuf::from(output(quiet(at(tool_command(tool, progress)?)).arg("--py"), "pipenv --py")?)
        }
        Tool::Conda => {
            let conda = conda().ok_or("conda isn't installed: get Miniforge (conda-forge.org/download) or micromamba, then try again")?;
            let text = std::fs::read_to_string(root.join(file)).map_err(|e| format!("{file}: {e}"))?;
            let name = conda_env_name(&text).ok_or_else(|| format!("{file} has no `name:` for the environment"))?;
            let exe = match &conda {
                Conda::Conda(p) | Conda::Micromamba(p) => p.clone(),
            };
            let exists = conda_env_prefix(command(&exe), &name).is_some();
            let args: Vec<&str> = match (&conda, exists) {
                (Conda::Conda(_), true) => vec!["env", "update", "-n", name.as_str(), "-f", file, "--prune"],
                (Conda::Conda(_), false) => vec!["env", "create", "-f", file],
                (Conda::Micromamba(_), true) => vec!["install", "-y", "-n", name.as_str(), "-f", file],
                (Conda::Micromamba(_), false) => vec!["create", "-y", "-n", name.as_str(), "-f", file],
            };
            output(at(command(&exe)).args(&args), &format!("{} {}", exe.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(), args[..2].join(" ")))?;
            let prefix = conda_env_prefix(command(&exe), &name).ok_or_else(|| format!("the conda environment {name} wasn't created"))?;
            conda_python(&prefix)
        }
    };
    if python.is_file() { Ok(python) } else { Err(format!("{} made no Python at {}", tool.name(), python.display())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_are_found_by_their_files() {
        let root = std::env::temp_dir().join(format!("graphyne-tools-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(project_tool(&root), None);
        std::fs::write(root.join("pyproject.toml"), "[tool.poetry]\nname = \"x\"\n[tool.poetry.dependencies]\npython = \"^3.10\"\n").unwrap();
        assert_eq!(project_tool(&root), Some((Tool::Poetry, "pyproject.toml")));
        std::fs::write(root.join("Pipfile"), "").unwrap();
        assert_eq!(project_tool(&root), Some((Tool::Pipenv, "Pipfile")));
        std::fs::write(root.join("poetry.lock"), "").unwrap();
        assert_eq!(project_tool(&root), Some((Tool::Poetry, "poetry.lock")));
        std::fs::write(root.join("uv.lock"), "").unwrap();
        assert_eq!(project_tool(&root), None, "uv projects are uv's");
        std::fs::remove_dir_all(&root).ok();
        assert_eq!(conda_env_name("name: data-lab  # the env\nchannels:\n  - conda-forge\n").as_deref(), Some("data-lab"));
        assert_eq!(conda_env_name("channels:\n  - defaults\n"), None);
    }
}
