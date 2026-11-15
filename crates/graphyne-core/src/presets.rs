//! Run-menu presets for audit and reverse-engineering tools (spec §9.3). Each runs in its
//! own terminal tab, so the output can be detached into a separate window.

use std::path::Path;

pub struct Preset {
    pub label: &'static str,
    /// Executable that must be on PATH (shown as "not installed" otherwise).
    pub program: &'static str,
    /// Command line typed into the terminal tab.
    pub command: &'static str,
    /// Needs a target (host, URL, binary): the command is typed but not run, so the user
    /// can append the target and press Enter.
    pub needs_target: bool,
    /// valgrind / strace exist only on Linux-like systems.
    pub unix_only: bool,
    /// Shown only for projects with this file at the root.
    pub requires: Option<&'static str>,
    pub install_hint: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        label: "cargo audit",
        program: "cargo-audit",
        command: "cargo audit",
        needs_target: false,
        unix_only: false,
        requires: Some("Cargo.toml"),
        install_hint: "cargo install cargo-audit",
    },
    Preset {
        label: "cargo clippy (pedantic)",
        program: "cargo-clippy",
        command: "cargo clippy --all-targets -- -W clippy::pedantic",
        needs_target: false,
        unix_only: false,
        requires: Some("Cargo.toml"),
        install_hint: "rustup component add clippy",
    },
    Preset {
        label: "nmap: service scan",
        program: "nmap",
        command: "nmap -sV -T4 ",
        needs_target: true,
        unix_only: false,
        requires: None,
        install_hint: "nmap.org/download",
    },
    Preset {
        label: "gobuster: directories",
        program: "gobuster",
        command: "gobuster dir -w wordlist.txt -u ",
        needs_target: true,
        unix_only: false,
        requires: None,
        install_hint: "go install github.com/OJ/gobuster/v3@latest",
    },
    Preset {
        label: "valgrind: memcheck",
        program: "valgrind",
        command: "valgrind --leak-check=full ",
        needs_target: true,
        unix_only: true,
        requires: None,
        install_hint: "package manager: valgrind",
    },
    Preset {
        label: "strace: follow forks",
        program: "strace",
        command: "strace -f ",
        needs_target: true,
        unix_only: true,
        requires: None,
        install_hint: "package manager: strace",
    },
    Preset {
        label: "gdb",
        program: "gdb",
        command: "gdb -q ",
        needs_target: true,
        unix_only: false,
        requires: None,
        install_hint: "MSYS2 / package manager: gdb",
    },
];

/// Presets that apply to this OS and project (checks marker files: background only).
pub fn applicable(root: &Path) -> Vec<&'static Preset> {
    applicable_for(root, cfg!(unix))
}

fn applicable_for(root: &Path, unix: bool) -> Vec<&'static Preset> {
    PRESETS
        .iter()
        .filter(|p| unix || !p.unix_only)
        .filter(|p| p.requires.is_none_or(|f| root.join(f).is_file()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_by_platform_and_project() {
        let dir = std::env::temp_dir().join(format!("graphyne-presets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let labels = |unix| applicable_for(&dir, unix).iter().map(|p| p.program).collect::<Vec<_>>();
        assert_eq!(labels(false), ["nmap", "gobuster", "gdb"]);
        assert_eq!(labels(true), ["nmap", "gobuster", "valgrind", "strace", "gdb"]);
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(&labels(false)[..2], ["cargo-audit", "cargo-clippy"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
