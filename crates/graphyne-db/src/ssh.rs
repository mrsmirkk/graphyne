//! SSH tunnels to database servers that are reachable only through SSH (a bastion / jump
//! host). The system's OpenSSH client (`ssh`, part of Windows 10 and 11) forwards a free local
//! port to the database, so the user's `~/.ssh/config`, keys and ssh-agent work as in a
//! terminal. A password or a key's passphrase reaches ssh through `SSH_ASKPASS`: Graphyne
//! itself answers the prompt ([`answer_askpass`]). New host keys are remembered
//! (`StrictHostKeyChecking=accept-new`); a changed one stops the connection.

use crate::config::SshConfig;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Set for the ssh process: when ssh starts Graphyne as its askpass helper, this tells it so.
const ASKPASS_ENV: &str = "GRAPHYNE_SSH_ASKPASS";
/// The password / passphrase the askpass helper answers with (only in ssh's environment).
const SECRET_ENV: &str = "GRAPHYNE_SSH_SECRET";
/// An ssh config file to use instead of `~/.ssh/config` (`-F`; tests, special setups).
const CONFIG_ENV: &str = "GRAPHYNE_SSH_CONFIG";

/// Graphyne was started by ssh to answer a prompt (`SSH_ASKPASS`): print the answer. Returns
/// true when that was the case — the caller exits right after.
pub fn answer_askpass() -> bool {
    if std::env::var_os(ASKPASS_ENV).is_none() {
        return false;
    }
    let prompt = std::env::args().nth(1).unwrap_or_default();
    // Host-key questions are settled by StrictHostKeyChecking (accept-new): never "yes" here.
    let answer = if prompt.contains("(yes/no") { "no".to_string() } else { std::env::var(SECRET_ENV).unwrap_or_default() };
    println!("{answer}");
    true
}

/// A running tunnel: `127.0.0.1:port` leads to the database. Closing it ends ssh.
pub struct Tunnel {
    child: Child,
    pub port: u16,
    /// The database as the SSH host sees it (`host:port`).
    target: String,
    /// ssh's latest messages.
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl Tunnel {
    /// The database driver could not connect through the tunnel: its `error`, explained
    /// when the cause is the way from the SSH host to the database.
    pub fn explain(&self, error: String) -> String {
        // ssh reports a failed forward right after closing the forwarded connection.
        std::thread::sleep(Duration::from_millis(100));
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        forward_failure(&self.target, error, &lines)
    }
}

/// `error` from the database driver, with what ssh said about the forward to `target`.
fn forward_failure(target: &str, error: String, ssh_lines: &VecDeque<String>) -> String {
    // A Linux sshd tells why it could not connect ("channel 2: open failed: connect failed:
    // Connection refused").
    if let Some(line) = ssh_lines.iter().rev().find(|l| l.contains("open failed")) {
        let why = line.rsplit_once("open failed: ").map(|(_, why)| why.trim()).unwrap_or(line.trim());
        return format!("SSH: the SSH host could not reach the database at {target} ({why})");
    }
    // Otherwise the driver only saw the tunnel close.
    let lower = error.to_lowercase();
    if ["eof", "communicating", "closed", "reset", "lost connection", "broken pipe"].iter().any(|w| lower.contains(w)) {
        return format!("{error}. Through the SSH tunnel: can the SSH host reach the database at {target}?");
    }
    error
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Windows' own OpenSSH client first (`System32\OpenSSH`), else `ssh` on PATH.
fn ssh_exe() -> Result<PathBuf, String> {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let own = root.join("System32").join("OpenSSH").join("ssh.exe");
        if own.is_file() {
            return Ok(own);
        }
    }
    let names: &[&str] = if cfg!(windows) { &["ssh.exe"] } else { &["ssh"] };
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
        .ok_or_else(|| "the OpenSSH client (ssh) isn't installed: add it in Windows Settings → System → Optional features → OpenSSH Client".to_string())
}

/// ssh's arguments for forwarding `127.0.0.1:local` to `db_host:db_port` through `ssh`.
pub fn arguments(ssh: &SshConfig, local: u16, db_host: &str, db_port: u16) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if let Some(config) = std::env::var_os(CONFIG_ENV).filter(|c| !c.is_empty()) {
        args.extend(["-F".into(), config.to_string_lossy().to_string()]);
    }
    args.extend(["-N".into(), "-T".into(), "-L".into(), format!("127.0.0.1:{local}:{db_host}:{db_port}")]);
    for option in ["ExitOnForwardFailure=yes", "StrictHostKeyChecking=accept-new", "ServerAliveInterval=30", "ConnectTimeout=15", "NumberOfPasswordPrompts=1"] {
        args.extend(["-o".into(), option.into()]);
    }
    if let Some(key) = ssh.key_file.as_deref().filter(|k| !k.trim().is_empty()) {
        args.extend(["-i".into(), key.trim().into(), "-o".into(), "IdentitiesOnly=yes".into()]);
    }
    if !ssh.user.trim().is_empty() {
        args.extend(["-l".into(), ssh.user.trim().into()]);
    }
    args.extend(["-p".into(), ssh.port.to_string(), ssh.host.trim().into()]);
    args
}

fn free_port() -> Result<u16, String> {
    std::net::TcpListener::bind(("127.0.0.1", 0)).and_then(|l| l.local_addr()).map(|a| a.port()).map_err(|e| format!("no free local port: {e}"))
}

/// What ssh said last that matters (its own errors; the forwarding's notices are skipped).
fn last_error(lines: &VecDeque<String>) -> String {
    lines
        .iter()
        .rev()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("Warning: Permanently added"))
        .unwrap_or("ssh stopped")
        .to_string()
}

/// ssh's message in plain words where it is cryptic, with what to check.
fn explain(error: &str, ssh: &SshConfig) -> String {
    let at = format!("{} port {}", ssh.host.trim(), ssh.port);
    if error.contains("Connection refused") {
        format!("nothing answers at {at} (connection refused)")
    } else if error.contains("timed out") {
        format!("{at} did not answer (timed out)")
    } else if error.contains("Permission denied") {
        format!("{error} Check the SSH user, the key file and the password or passphrase.")
    } else if error.contains("Host key verification failed") {
        format!("{error} The SSH host's key differs from the one remembered in ~/.ssh/known_hosts.")
    } else {
        error.to_string()
    }
}

/// Start ssh and wait until the forward is open (or ssh gave up). `secret` answers a password
/// or passphrase prompt.
pub fn open(ssh: &SshConfig, db_host: &str, db_port: u16, secret: Option<&str>) -> Result<Tunnel, String> {
    if ssh.host.trim().is_empty() {
        return Err("SSH: the SSH host is empty".into());
    }
    let exe = ssh_exe()?;
    let port = free_port()?;
    let mut cmd = Command::new(&exe);
    cmd.args(arguments(ssh, port, db_host, db_port)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    if let Ok(me) = std::env::current_exe() {
        cmd.env("SSH_ASKPASS", me).env("SSH_ASKPASS_REQUIRE", "force").env(ASKPASS_ENV, "1").env(SECRET_ENV, secret.unwrap_or_default());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("SSH: {}: {e}", exe.display()))?;
    graphyne_core::process::end_with_graphyne(&child);
    // ssh's messages, read all along (a full pipe would stall it); the last ones explain a failure.
    let lines = Arc::new(Mutex::new(VecDeque::<String>::new()));
    if let Some(stderr) = child.stderr.take() {
        let lines = lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut l = lines.lock().unwrap_or_else(|e| e.into_inner());
                l.push_back(line);
                if l.len() > 30 {
                    l.pop_front();
                }
            }
        });
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            // Let the reader catch ssh's last words.
            std::thread::sleep(Duration::from_millis(150));
            let error = last_error(&lines.lock().unwrap_or_else(|e| e.into_inner()));
            return Err(format!("SSH: {}", explain(&error, ssh)));
        }
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            return Ok(Tunnel { child, port, target: format!("{db_host}:{db_port}"), lines });
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err(format!("SSH: no tunnel to {} after 30 s", ssh.host));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_arguments() {
        let ssh = SshConfig { host: "bastion.example.com".into(), port: 2222, user: "deploy".into(), key_file: Some("C:/keys/id_ed25519".into()), secret: None };
        let args = arguments(&ssh, 50123, "db.internal", 5432).join(" ");
        assert!(args.contains("-N -T -L 127.0.0.1:50123:db.internal:5432"), "{args}");
        assert!(args.contains("-o ExitOnForwardFailure=yes") && args.contains("-o StrictHostKeyChecking=accept-new"), "{args}");
        // The same answer every time: one try is enough.
        assert!(args.contains("-o NumberOfPasswordPrompts=1"), "{args}");
        assert!(args.contains("-i C:/keys/id_ed25519 -o IdentitiesOnly=yes"), "{args}");
        assert!(args.ends_with("-l deploy -p 2222 bastion.example.com"), "{args}");
        let plain = SshConfig { host: "h".into(), port: 22, ..Default::default() };
        assert!(arguments(&plain, 1, "localhost", 3306).join(" ").ends_with("-p 22 h"));
        let mut lines = VecDeque::new();
        lines.push_back("Warning: Permanently added 'h' (ED25519) to the list of known hosts.".to_string());
        lines.push_back("deploy@h: Permission denied (publickey,password).".to_string());
        assert_eq!(last_error(&lines), "deploy@h: Permission denied (publickey,password).");
        // Windows' OpenSSH reports a closed port like this.
        let refused = explain("banner exchange: Connection to UNKNOWN port -1: Connection refused", &ssh);
        assert_eq!(refused, "nothing answers at bastion.example.com port 2222 (connection refused)");
        assert!(explain("deploy@h: Permission denied (publickey).", &ssh).contains("Check the SSH user"));
        // The database behind the SSH host is down.
        let mut said = VecDeque::new();
        said.push_back("channel 2: open failed: connect failed: Connection refused".to_string());
        assert_eq!(forward_failure("db:5432", "early eof".into(), &said), "SSH: the SSH host could not reach the database at db:5432 (connect failed: Connection refused)");
        let quiet = VecDeque::new();
        assert!(forward_failure("db:5432", "error communicating with the server: early eof".into(), &quiet).ends_with("can the SSH host reach the database at db:5432?"));
        let auth = "password authentication failed for user \"app\"".to_string();
        assert_eq!(forward_failure("db:5432", auth.clone(), &quiet), auth);
    }
}
