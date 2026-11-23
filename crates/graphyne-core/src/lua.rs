//! Lua: the interpreter Run uses and the debugger. The debugger is lua-debug (actboy168, MIT),
//! which Graphyne downloads on first use; its package carries Lua 5.1–5.5 and LuaJIT, so Lua
//! files run and debug on a computer without Lua too ("Graphyne's Lua").

use std::path::{Path, PathBuf};

/// The lua-debug release Graphyne downloads (from Open VSX) and its SHA-256 (as Open VSX lists it).
pub const LUA_DEBUG_VERSION: &str = "2.3.0";
const LUA_DEBUG_SHA256: &str = "f86710b01e755dd5d3d55c44441c7c0991dd47e515851c871801d6003f894d3d";

/// Lua interpreters on PATH, in the order Run and Debug look for them.
const NAMES: [&str; 8] = ["lua", "lua5.4", "lua54", "lua5.5", "lua55", "luajit", "lua5.3", "lua5.1"];

/// A Lua interpreter on this computer (installed since Graphyne started counts too).
pub fn system_lua() -> Option<PathBuf> {
    NAMES.iter().find_map(|n| crate::toolchain::which(n)).or_else(|| {
        crate::toolchain::refresh_path();
        NAMES.iter().find_map(|n| crate::toolchain::which(n))
    })
}

/// lua-debug's name for the runtime that matches `lua`'s version: `lua54`, `lua51`, `luajit`…
pub fn runtime_of(lua: &Path) -> Option<&'static str> {
    let out = crate::toolchain::hidden_command(&lua.to_string_lossy()).arg("-v").stdin(std::process::Stdio::null()).output().ok()?;
    // `lua -v` prints to stdout (5.4) or stderr (5.1).
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    runtime_for_banner(&text)
}

fn runtime_for_banner(text: &str) -> Option<&'static str> {
    if text.starts_with("LuaJIT") {
        return Some("luajit");
    }
    let version = text.strip_prefix("Lua ")?;
    Some(match version.get(..3)? {
        "5.1" => "lua51",
        "5.2" => "lua52",
        "5.3" => "lua53",
        "5.4" => "lua54",
        "5.5" => "lua55",
        _ => return None,
    })
}

fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("win32-x64"),
        _ => None,
    }
}

/// The lua-debug extension folder, when it is here: `GRAPHYNE_LUA_DEBUG_PATH`, else Graphyne's copy.
pub fn lua_debug_present() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GRAPHYNE_LUA_DEBUG_PATH").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p)).filter(|p| p.join("bin").is_dir());
    }
    let ext = own_dir()?.join("extension");
    ext.join("bin").join(if cfg!(windows) { "lua-debug.exe" } else { "lua-debug" }).is_file().then_some(ext)
}

fn own_dir() -> Option<PathBuf> {
    Some(crate::session::state_dir()?.join("adapters").join(format!("lua-debug-{LUA_DEBUG_VERSION}")))
}

/// The lua-debug extension folder, downloading it (about 10 MB) the first time.
pub fn lua_debug(progress: &dyn Fn(&str)) -> Result<PathBuf, String> {
    if let Some(ext) = lua_debug_present() {
        return Ok(ext);
    }
    if std::env::var_os("GRAPHYNE_LUA_DEBUG_PATH").is_some_and(|p| !p.is_empty()) {
        return Err("GRAPHYNE_LUA_DEBUG_PATH doesn't point to the lua-debug extension folder".into());
    }
    let plat = platform().ok_or("Lua debugging is available on Windows x64 for now")?;
    let dir = own_dir().ok_or("no folder for Graphyne's state")?;
    progress(&format!("Downloading the Lua debugger (lua-debug {LUA_DEBUG_VERSION}, about 10 MB, once)…"));
    let url = format!("https://open-vsx.org/api/actboy168/lua-debug/{plat}/{LUA_DEBUG_VERSION}/file/actboy168.lua-debug-{LUA_DEBUG_VERSION}@{plat}.vsix");
    let staging = crate::download::fetch_unpacked(&url, LUA_DEBUG_SHA256, &dir)
        .map_err(|e| format!("Could not download the Lua debugger (lua-debug {LUA_DEBUG_VERSION}) from {url}: {e}. Check the connection"))?;
    let moved = std::fs::rename(staging.join("extension"), dir.join("extension"));
    let _ = std::fs::remove_dir_all(&staging);
    moved.map_err(|e| format!("installing lua-debug: {e}"))?;
    lua_debug_present().ok_or_else(|| "the lua-debug package has no bin/lua-debug.exe".into())
}

/// `runtime` (`lua54`, `luajit`…) from lua-debug: Graphyne's Lua.
fn runtime_exe(ext: &Path, runtime: &str) -> Option<PathBuf> {
    let exe = ext.join("runtime").join(platform()?).join(runtime).join(if cfg!(windows) { "lua.exe" } else { "lua" });
    exe.is_file().then_some(exe)
}

/// The Lua Run uses: the computer's, else Graphyne's Lua 5.4 when it is already here.
pub fn run_lua() -> Option<PathBuf> {
    system_lua().or_else(|| runtime_exe(&lua_debug_present()?, "lua54"))
}

/// Make sure Run has a Lua: without one on the computer, Graphyne's Lua 5.4 (downloaded once
/// with the debugger, about 10 MB).
pub fn prepare_run(progress: &dyn Fn(&str)) -> Result<PathBuf, String> {
    if let Some(lua) = run_lua() {
        return Ok(lua);
    }
    let ext = lua_debug(progress)?;
    runtime_exe(&ext, "lua54").ok_or_else(|| "the lua-debug package has no Lua 5.4 runtime".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtimes_from_banners() {
        assert_eq!(runtime_for_banner("Lua 5.4.7  Copyright (C) 1994-2024 Lua.org, PUC-Rio\n"), Some("lua54"));
        assert_eq!(runtime_for_banner("Lua 5.1.5  Copyright (C) 1994-2012 Lua.org, PUC-Rio"), Some("lua51"));
        assert_eq!(runtime_for_banner("LuaJIT 2.1.1734355927 -- Copyright (C) 2005-2025 Mike Pall."), Some("luajit"));
        assert_eq!(runtime_for_banner("bash: lua: command not found"), None);
    }
}
