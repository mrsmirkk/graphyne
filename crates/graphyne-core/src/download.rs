//! Pinned downloads of helper tools (the JavaScript debugger, uv): the file is checked
//! against a known SHA-256 and unpacked next to its final place, so a broken or tampered
//! download leaves nothing behind. Uses the system's own curl and tar (Windows 10+, which
//! unpack both .tar.gz and .zip). Blocking — call from background threads.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A program that ships with the OS (`%SystemRoot%\System32\<name>.exe` on Windows), so a
/// same-named file elsewhere on PATH is never picked up.
pub fn system_tool(name: &str) -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        root.join("System32").join(format!("{name}.exe"))
    } else {
        PathBuf::from(name)
    }
}

pub fn sha256_hex(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

fn hidden(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// Download `url` into `dir`, check its SHA-256 and unpack it into `dir/unpacking` (emptied
/// first). Returns that folder: the caller moves what it needs out of it and removes it.
pub fn fetch_unpacked(url: &str, sha256: &str, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let archive = dir.join("download.part");
    let out = hidden(&system_tool("curl"))
        .args(["-fsSL", "--retry", "2", "--connect-timeout", "15", "-o"])
        .arg(&archive)
        .arg(url)
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&archive);
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let hash = sha256_hex(&archive)?;
    if hash != sha256 {
        let _ = std::fs::remove_file(&archive);
        return Err(format!("the file's SHA-256 is {hash}, expected {sha256}"));
    }
    let staging = dir.join("unpacking");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let out = hidden(&system_tool("tar")).arg("-xf").arg(&archive).arg("-C").arg(&staging).output().map_err(|e| format!("tar: {e}"))?;
    let _ = std::fs::remove_file(&archive);
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("unpacking failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(staging)
}
