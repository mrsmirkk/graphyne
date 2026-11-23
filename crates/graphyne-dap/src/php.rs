//! PHP debugging setup: Xdebug for the PHP that runs the file. A PHP without Xdebug gets
//! Graphyne's copy, loaded with `-d zend_extension=` for the debug session only (php.ini is
//! left alone).

use graphyne_core::toolchain::hidden_command;
use std::path::{Path, PathBuf};

/// The Xdebug release Graphyne downloads, and its Windows builds with their SHA-256 (from
/// xdebug.org/download): PHP version, thread safety, compiler.
pub const XDEBUG_VERSION: &str = "3.5.3";
const XDEBUG_BUILDS: [(&str, bool, &str, &str); 12] = [
    ("8.0", false, "vs16", "a3ffd4d5b1f9ca0766b1f3ed321246808a51ce4ba49dc32ae195bfafc4d4d9a2"),
    ("8.0", true, "vs16", "48398f54d7bf311728eafeb9999a5a55be5b5feb83b9af8f3115fef4b71dd2f7"),
    ("8.1", false, "vs16", "15bb3abcd0755af8378550793bcf7603636add31dc4a04d24bfd2a1f764399b4"),
    ("8.1", true, "vs16", "8a90f1fd3e199a07e3039fc857330ca098a71bf9622e88a878efaa1b1b6fe358"),
    ("8.2", false, "vs16", "b449eda7eeefc80c651fac334582c5f0877cd72cfe55fc1c5f0f98bf6a4edb94"),
    ("8.2", true, "vs16", "56a62ebd342a143ced245f30de375f5b907ba50efaacf42801a5dd280006e127"),
    ("8.3", false, "vs16", "4c80487592484c36d21a7006a53738dfc07884fc6867720ddc5c11559371cdce"),
    ("8.3", true, "vs16", "c178f05068a6f0cef4a37eaca6003a56120004bf4ac20db6ff20f6f1ce0b439e"),
    ("8.4", false, "vs17", "a94d4fcfe4086a300b2decaf7c04e59f4981aa5e56992d48b55c5e49dfd9b9ee"),
    ("8.4", true, "vs17", "b9c79caec87f2b289df4b7bfa64b64f01c97956c9ac7ddb936302b79aec65cda"),
    ("8.5", false, "vs17", "0a0d52bf276dad7e5f90ac30a5110b3119d291b20c118be0cb7f5752064f1811"),
    ("8.5", true, "vs17", "c75623ba050ddef734ba28a957e92df9ce5b8dfdb1a0569e5071bd6b68a12292"),
];

/// What matters about a PHP for Xdebug.
#[derive(Debug, PartialEq)]
pub struct PhpInfo {
    /// `8.4`
    pub version: String,
    /// Thread-safe build.
    pub ts: bool,
    pub bits: u32,
    /// Xdebug is already loaded (php.ini).
    pub xdebug: bool,
}

fn parse_info(text: &str) -> Option<PhpInfo> {
    let mut parts = text.split_whitespace();
    let version = parts.next()?.to_string();
    let ts = parts.next()? == "ts";
    let bits = parts.next()?.parse().ok()?;
    let xdebug = parts.next()? == "xdebug";
    Some(PhpInfo { version, ts, bits, xdebug })
}

pub fn inspect(php: &Path) -> Result<PhpInfo, String> {
    let script = "echo PHP_MAJOR_VERSION, '.', PHP_MINOR_VERSION, ' ', PHP_ZTS ? 'ts' : 'nts', ' ', PHP_INT_SIZE * 8, ' ', extension_loaded('xdebug') ? 'xdebug' : '-';";
    let out = hidden_command(&php.to_string_lossy()).args(["-r", script]).stdin(std::process::Stdio::null()).output().map_err(|e| format!("{}: {e}", php.display()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    parse_info(&text).ok_or_else(|| format!("{} didn't run: {}", php.display(), String::from_utf8_lossy(&out.stderr).trim()))
}

/// Xdebug for that PHP: `GRAPHYNE_XDEBUG_PATH`, else Graphyne's copy (downloaded once, about
/// 1 MB, checked against its SHA-256).
pub fn xdebug_for(info: &PhpInfo, progress: &dyn Fn(&str)) -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("GRAPHYNE_XDEBUG_PATH").filter(|p| !p.is_empty()).map(PathBuf::from) {
        return if p.is_file() { Ok(p) } else { Err(format!("GRAPHYNE_XDEBUG_PATH: {} not found", p.display())) };
    }
    if !cfg!(windows) {
        return Err("Install Xdebug for your PHP (xdebug.org/docs/install) to debug PHP".into());
    }
    if info.bits != 64 {
        return Err("Graphyne has Xdebug for 64-bit PHP only: install Xdebug yourself (xdebug.org/wizard)".into());
    }
    let Some(&(version, ts, vs, sha)) = XDEBUG_BUILDS.iter().find(|(v, ts, _, _)| *v == info.version && *ts == info.ts) else {
        return Err(format!("Xdebug {XDEBUG_VERSION} has no build for PHP {}: install Xdebug yourself (xdebug.org/wizard)", info.version));
    };
    let file = format!("php_xdebug-{XDEBUG_VERSION}-{version}-{}-{vs}-x86_64.dll", if ts { "ts" } else { "nts" });
    let dest = graphyne_core::session::state_dir().ok_or("no folder for Graphyne's state")?.join("tools").join(format!("xdebug-{XDEBUG_VERSION}")).join(&file);
    if dest.is_file() {
        return Ok(dest);
    }
    progress(&format!("Downloading the PHP debugger (Xdebug {XDEBUG_VERSION} for PHP {version}, about 1 MB, once)…"));
    let url = format!("https://xdebug.org/files/{file}");
    graphyne_core::download::fetch_file(&url, sha, &dest).map_err(|e| format!("Could not download Xdebug from {url}: {e}. Check the connection"))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_info() {
        assert_eq!(parse_info("8.4 nts 64 -"), Some(PhpInfo { version: "8.4".into(), ts: false, bits: 64, xdebug: false }));
        assert_eq!(parse_info("8.2 ts 64 xdebug").map(|i| (i.ts, i.xdebug)), Some((true, true)));
        assert_eq!(parse_info("PHP Warning: …"), None);
        // Every build Graphyne knows is pinned.
        assert!(XDEBUG_BUILDS.iter().all(|(_, _, _, sha)| sha.len() == 64));
    }
}
