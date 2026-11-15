//! Language → debug adapter matrix and `launch` configurations (spec §6.3, DAP column).
//! Everything here may block (PATH probing, `cargo build`): background executors only.

use crate::client::Transport;
use graphyne_core::LanguageId;
use graphyne_core::toolchain::hidden_command;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// A ready-to-start adapter plus the `launch` request arguments.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub name: &'static str,
    pub adapter_id: &'static str,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub transport: Transport,
    pub cwd: PathBuf,
    pub launch: Value,
}

/// Locate an executable on PATH (Windows extensions honoured).
pub fn which(program: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".cargo").join("bin"));
    }
    if cfg!(windows) {
        dirs.push(PathBuf::from(r"C:\Program Files\LLVM\bin"));
    }
    dirs.into_iter().flat_map(|d| exts.iter().map(move |e| d.join(format!("{program}{e}")))).find(|c| c.is_file())
}

fn free_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?.local_addr().ok().map(|a| a.port())
}

/// Resolve the adapter and launch configuration for debugging `file`.
pub fn resolve(file: &Path, root: &Path) -> Result<Resolved, String> {
    let lang = LanguageId::from_path(file);
    let dir = file.parent().unwrap_or(root).to_path_buf();
    let title = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    match lang {
        LanguageId::Python => python(file, &dir, &title, root),
        LanguageId::Rust => rust(file, root, &title),
        LanguageId::C | LanguageId::Cpp => native(file, &dir, &title, lang),
        LanguageId::Go => go(file, &dir, &title),
        other => Err(format!("No debugger configured for {} yet", other.name())),
    }
}

fn python(file: &Path, dir: &Path, title: &str, root: &Path) -> Result<Resolved, String> {
    // The project's .venv first (Graphyne sets it up with debugpy), then the system Python.
    let python = graphyne_core::python_env::project_python(root)
        .or_else(|| which(if cfg!(windows) { "python" } else { "python3" }))
        .or_else(|| which("python"))
        .ok_or("Python interpreter not found on PATH")?;
    let mut env = Vec::new();
    // Explicit location (e.g. an isolated `pip install --target` folder) wins.
    if let Some(extra) = std::env::var_os("GRAPHYNE_DEBUGPY_PATH") {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let mut pp = extra.to_string_lossy().to_string();
        if let Some(existing) = std::env::var_os("PYTHONPATH") {
            pp = format!("{pp}{sep}{}", existing.to_string_lossy());
        }
        env.push(("PYTHONPATH".to_string(), pp));
    } else {
        let ok = hidden_command(&python.to_string_lossy())
            .args(["-c", "import debugpy"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return Err("debugpy is not installed for this Python. Install it with `python -m pip install debugpy` or set GRAPHYNE_DEBUGPY_PATH".into());
        }
    }
    Ok(Resolved {
        name: "debugpy",
        adapter_id: "debugpy",
        program: python,
        args: vec!["-m".into(), "debugpy.adapter".into()],
        env,
        transport: Transport::Stdio,
        cwd: dir.to_path_buf(),
        launch: json!({
            "type": "python", "request": "launch", "name": format!("Graphyne: {title}"),
            "program": file, "cwd": dir, "console": "integratedTerminal",
            // Program I/O lives in the terminal tab (runInTerminal); don't echo it to the console.
            "justMyCode": true, "redirectOutput": false, "showReturnValue": true
        }),
    })
}

/// lldb-dap (stdio) or CodeLLDB (TCP).
fn lldb_adapter() -> Option<(&'static str, PathBuf, Vec<String>, Transport)> {
    if let Some(p) = which("lldb-dap").or_else(|| which("lldb-vscode")) {
        return Some(("lldb-dap", p, Vec::new(), Transport::Stdio));
    }
    let p = which("codelldb")?;
    let port = free_port()?;
    Some(("codelldb", p, vec!["--port".into(), port.to_string()], Transport::Tcp { port }))
}

/// `cargo build --message-format=json` and pick the produced binary.
fn cargo_build(crate_dir: &Path) -> Result<PathBuf, String> {
    let out = hidden_command("cargo")
        .args(["build", "--message-format=json-render-diagnostics"])
        .current_dir(crate_dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cargo: {e}"))?;
    let mut exe = None;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["reason"] == "compiler-artifact" {
            if let Some(path) = v["executable"].as_str() {
                exe = Some(PathBuf::from(path));
            }
        }
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("cargo build failed:\n{}", err.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")));
    }
    exe.ok_or_else(|| "cargo build produced no executable (library crate?)".to_string())
}

fn rust(file: &Path, root: &Path, title: &str) -> Result<Resolved, String> {
    let (name, program, args, transport) = lldb_adapter().ok_or("No LLDB debug adapter found (install LLVM for lldb-dap, or CodeLLDB)")?;
    let crate_dir = graphyne_core::run::find_up(file, root, "Cargo.toml").ok_or("Cargo.toml not found for this file")?;
    let exe = cargo_build(&crate_dir)?;
    Ok(Resolved {
        name,
        adapter_id: "lldb",
        program,
        args,
        env: Vec::new(),
        transport,
        cwd: crate_dir.clone(),
        launch: json!({ "type": "lldb", "request": "launch", "name": format!("Graphyne: {title}"), "program": exe, "args": [], "cwd": crate_dir, "stopOnEntry": false }),
    })
}

fn native(file: &Path, dir: &Path, title: &str, lang: LanguageId) -> Result<Resolved, String> {
    let exe = std::env::temp_dir().join(if cfg!(windows) { "graphyne-debug.exe" } else { "graphyne-debug" });
    let compiler = if lang == LanguageId::Cpp { "g++" } else { "gcc" };
    let out = hidden_command(compiler)
        .args(["-g", "-O0"])
        .arg(file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("{compiler}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{compiler} failed:\n{}", String::from_utf8_lossy(&out.stderr)));
    }
    // GDB ≥ 14 speaks DAP natively; otherwise LLDB.
    if let Some(gdb) = which("gdb") {
        return Ok(Resolved {
            name: "gdb",
            adapter_id: "gdb",
            program: gdb,
            args: vec!["-i".into(), "dap".into()],
            env: Vec::new(),
            transport: Transport::Stdio,
            cwd: dir.to_path_buf(),
            launch: json!({ "request": "launch", "name": format!("Graphyne: {title}"), "program": exe, "cwd": dir, "stopAtBeginningOfMainSubprogram": false }),
        });
    }
    let (name, program, args, transport) = lldb_adapter().ok_or("No native debugger found (gdb ≥ 14 or lldb-dap)")?;
    Ok(Resolved {
        name,
        adapter_id: "lldb",
        program,
        args,
        env: Vec::new(),
        transport,
        cwd: dir.to_path_buf(),
        launch: json!({ "type": "lldb", "request": "launch", "name": format!("Graphyne: {title}"), "program": exe, "cwd": dir, "stopOnEntry": false }),
    })
}

fn go(file: &Path, dir: &Path, title: &str) -> Result<Resolved, String> {
    let dlv = which("dlv").ok_or("Delve not found (go install github.com/go-delve/delve/cmd/dlv@latest)")?;
    let port = free_port().ok_or("no free TCP port")?;
    let program = graphyne_core::run::find_up(file, dir, "go.mod").unwrap_or_else(|| file.to_path_buf());
    Ok(Resolved {
        name: "delve",
        adapter_id: "go",
        program: dlv,
        args: vec!["dap".into(), format!("--listen=127.0.0.1:{port}")],
        env: Vec::new(),
        transport: Transport::Tcp { port },
        cwd: dir.to_path_buf(),
        launch: json!({ "request": "launch", "name": format!("Graphyne: {title}"), "mode": "debug", "program": program, "cwd": dir }),
    })
}
