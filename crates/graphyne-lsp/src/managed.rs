//! Language servers Graphyne installs itself, on request, into its own folder
//! (`<state>/lsp/<server>`) rather than into the user's global npm / gem / dotnet setup:
//! intelephense (PHP), ruby-lsp (Ruby), csharp-ls (C#), Eclipse JDT LS (Java), JetBrains'
//! Kotlin server and Metals (Scala). Each installs and runs with the language's own
//! toolchain (Node.js, Ruby, the .NET SDK, Java); the Kotlin server brings its own Java.
//! Versions are pinned; downloads Graphyne makes itself are checked against a SHA-256.
//! Blocking (processes, downloads): background executors only.

use crate::servers::Launch;
use graphyne_core::download::{fetch_file, fetch_unpacked};
use graphyne_core::toolchain::{find, hidden_command};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const INTELEPHENSE_VERSION: &str = "1.18.5";
const RUBY_LSP_VERSION: &str = "0.26.11";
const LSP_PROTOCOL_GEM_VERSION: &str = "3.17.0.6";
const JDTLS_URL: &str = "https://download.eclipse.org/jdtls/milestones/1.63.0/jdt-language-server-1.63.0-202610191322.tar.gz";
const JDTLS_SHA256: &str = "b1f2abe0b3cccb462cf64fb4a5587c7d09df60cf8be7748b152cbd89d018bdc4";
const KOTLIN_URL: &str = "https://download.jetbrains.com/language-server/kotlin-server/271.2085.0/kotlin-server-271.2085.0.win.zip";
const KOTLIN_SHA256: &str = "da210a7d84a06f855b8b7b04c5bd54936d270e79f3ea13a9c6ddc2f1046b2a98";
const COURSIER_URL: &str = "https://github.com/coursier/coursier/releases/download/v2.1.26/coursier";
const COURSIER_SHA256: &str = "eb0c35379e76f44404f777ab0a2335ca5326f6b7721b8cdfa873e87bd9f4ac0c";
const METALS: &str = "org.scalameta:metals_2.13:1.6.9";

/// Written last by an install: the folder holds a working server.
const MARKER: &str = "graphyne-install.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Need {
    Node,
    Ruby,
    DotnetSdk,
    Java(u32),
    /// Windows x64 only (the server ships per platform; Graphyne pins the Windows build).
    WindowsX64,
}

struct Recipe {
    server: &'static str,
    title: &'static str,
    size: &'static str,
    need: Need,
}

static RECIPES: &[Recipe] = &[
    Recipe { server: "intelephense", title: "intelephense", size: "about 150 MB", need: Need::Node },
    Recipe { server: "ruby-lsp", title: "Ruby LSP", size: "about 5 MB", need: Need::Ruby },
    Recipe { server: "csharp-ls", title: "csharp-ls", size: "about 100 MB", need: Need::DotnetSdk },
    Recipe { server: "jdtls", title: "Eclipse JDT Language Server", size: "a 50 MB download", need: Need::Java(21) },
    Recipe { server: "kotlin-lsp", title: "JetBrains Kotlin LSP", size: "a 370 MB download, 1.2 GB unpacked", need: Need::WindowsX64 },
    Recipe { server: "metals", title: "Metals", size: "about 180 MB", need: Need::Java(17) },
];

fn recipe(server: &str) -> Option<&'static Recipe> {
    RECIPES.iter().find(|r| r.server == server)
}

/// A server Graphyne can install, as the UI offers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub server: &'static str,
    /// "intelephense".
    pub title: &'static str,
    /// "about 60 MB".
    pub size: &'static str,
    /// Why it can't be installed on this computer yet ("Node.js isn't installed…").
    pub blocked: Option<String>,
}

/// The server Graphyne would install for `lang`: the first of its servers it knows how to.
/// Blocking: it looks for the toolchain.
pub fn offer_for(lang: graphyne_core::LanguageId) -> Option<Offer> {
    crate::servers::specs_for(lang).iter().find_map(|s| offer(s.name))
}

/// What Graphyne could install for `server`. Blocking: it looks for the toolchain.
pub fn offer(server: &str) -> Option<Offer> {
    let r = recipe(server)?;
    Some(Offer { server: r.server, title: r.title, size: r.size, blocked: blocked(r.need) })
}

fn blocked(need: Need) -> Option<String> {
    match need {
        Need::Node => (find("node").is_none() || find("npm").is_none()).then(|| "it needs Node.js with npm (nodejs.org)".to_string()),
        Need::Ruby => (find("ruby").is_none() || find("gem").is_none()).then(|| "it needs Ruby (rubyinstaller.org)".to_string()),
        Need::DotnetSdk => match find("dotnet").and_then(|d| dotnet_sdk_major(&d)) {
            Some(major) if csharp_ls_version(major).is_some() => None,
            Some(major) => Some(format!("it needs the .NET SDK 8 or newer (this computer has .NET {major})")),
            None => Some("it needs the .NET SDK (dot.net)".to_string()),
        },
        Need::Java(min) => match java_major() {
            Some(v) if v >= min => None,
            Some(v) => Some(format!("it needs Java {min} or newer (this computer has Java {v}); get one from adoptium.net")),
            None => Some(format!("it needs Java {min} or newer (adoptium.net)")),
        },
        Need::WindowsX64 => (!(cfg!(windows) && cfg!(target_arch = "x86_64"))).then(|| "Graphyne installs it on Windows only; elsewhere install it from github.com/Kotlin/kotlin-lsp".to_string()),
    }
}

fn root_dir() -> Option<PathBuf> {
    Some(graphyne_core::session::state_dir()?.join("lsp"))
}

fn server_dir(server: &str) -> Option<PathBuf> {
    Some(root_dir()?.join(server))
}

/// The copy Graphyne installed, if any, and how to start it for project `root`.
pub fn launch(server: &str, root: &Path) -> Option<Launch> {
    let dir = server_dir(server)?;
    if !dir.join(MARKER).is_file() {
        return None;
    }
    let plain = |program: PathBuf, args: Vec<String>| Some(Launch { program, args, env: Vec::new(), init_options: None });
    match server {
        "intelephense" => {
            let script = dir.join("node_modules").join("intelephense").join("lib").join("intelephense.js");
            plain(find("node")?, vec![path_arg(&script), "--stdio".into()])
        }
        "ruby-lsp" => Some(Launch { program: find("ruby")?, args: vec![path_arg(&dir.join("bin").join("ruby-lsp"))], env: ruby_env(&dir), init_options: None }),
        "csharp-ls" => plain(dir.join(exe("csharp-ls")), Vec::new()),
        "jdtls" => {
            let java = find("java")?;
            let launcher = std::fs::read_dir(dir.join("plugins")).ok()?.flatten().map(|e| e.path()).find(|p| {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                name.starts_with("org.eclipse.equinox.launcher_") && name.ends_with(".jar")
            })?;
            let data = graphyne_core::session::project_data_dir(root)?.join("jdtls");
            let config = dir.join(if cfg!(windows) {
                "config_win"
            } else if cfg!(target_os = "macos") {
                if cfg!(target_arch = "aarch64") { "config_mac_arm" } else { "config_mac" }
            } else if cfg!(target_arch = "aarch64") {
                "config_linux_arm"
            } else {
                "config_linux"
            });
            let mut args: Vec<String> = [
                "-Declipse.application=org.eclipse.jdt.ls.core.id1",
                "-Dosgi.bundles.defaultStartLevel=4",
                "-Declipse.product=org.eclipse.jdt.ls.core.product",
                "-Dlog.level=ERROR",
                "-Xms256m",
                "-Xmx2G",
                "--add-modules=ALL-SYSTEM",
                "--add-opens",
                "java.base/java.util=ALL-UNNAMED",
                "--add-opens",
                "java.base/java.lang=ALL-UNNAMED",
                "-jar",
            ]
            .into_iter()
            .map(String::from)
            .collect();
            args.extend([path_arg(&launcher), "-configuration".into(), path_arg(&config), "-data".into(), path_arg(&data)]);
            plain(java, args)
        }
        "kotlin-lsp" => {
            let data = graphyne_core::session::project_data_dir(root)?.join("kotlin-lsp");
            let args = vec!["--stdio".into(), "--data-sharing=none".into(), format!("--system-path={}", path_arg(&data.join("system")))];
            // Its index goes to the user's caches folder unless the client names one.
            let init_options = Some(serde_json::json!({ "indexDir": path_arg(&data.join("index")) }));
            Some(Launch { program: dir.join("bin").join(exe("intellij-server")), args, env: Vec::new(), init_options })
        }
        // Metals fetches compiler parts while it runs: into its own cache too.
        "metals" => Some(Launch {
            program: find("java")?,
            args: vec![format!("@{}", path_arg(&dir.join("metals.args")))],
            env: vec![("COURSIER_CACHE".into(), path_arg(&dir.join("cache")))],
            init_options: None,
        }),
        _ => None,
    }
}

/// Install `server` into Graphyne's folder (replacing an earlier copy). `progress` gets
/// one-line updates for the status bar.
pub fn install(server: &str, progress: &dyn Fn(&str)) -> Result<(), String> {
    let r = recipe(server).ok_or_else(|| format!("Graphyne can't install {server}"))?;
    if let Some(why) = blocked(r.need) {
        return Err(format!("{} can't be installed: {why}", r.title));
    }
    let dir = server_dir(server).ok_or("no folder for Graphyne's state")?;
    // Metals keeps its download cache: an interrupted install resumes.
    if server == "metals" {
        let _ = std::fs::remove_file(dir.join(MARKER));
    } else {
        remove_dir(&dir)?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let version = match server {
        "intelephense" => install_intelephense(&dir, progress)?,
        "ruby-lsp" => install_ruby_lsp(&dir, progress)?,
        "csharp-ls" => install_csharp_ls(&dir, progress)?,
        "jdtls" => install_archive(&dir, JDTLS_URL, JDTLS_SHA256, "Eclipse JDT Language Server (50 MB)", progress)?,
        "kotlin-lsp" => install_kotlin(&dir, progress)?,
        "metals" => install_metals(&dir, progress)?,
        _ => return Err(format!("Graphyne can't install {server}")),
    };
    std::fs::write(dir.join(MARKER), format!("{{ \"server\": \"{server}\", \"version\": \"{version}\" }}\n")).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(())
}

fn install_intelephense(dir: &Path, progress: &dyn Fn(&str)) -> Result<String, String> {
    let npm = find("npm").ok_or("npm isn't installed")?;
    progress(&format!("Installing intelephense {INTELEPHENSE_VERSION} with npm…"));
    let mut cmd = command(&npm);
    cmd.args(["install", "--prefix"]).arg(dir).arg(format!("intelephense@{INTELEPHENSE_VERSION}")).args(["--no-audit", "--no-fund", "--omit=dev", "--loglevel=error"]);
    run(cmd, "npm")?;
    Ok(INTELEPHENSE_VERSION.into())
}

fn ruby_env(dir: &Path) -> Vec<(String, String)> {
    // The trailing separator keeps Ruby's own gem folders (prism, rbs) after Graphyne's.
    let sep = if cfg!(windows) { ";" } else { ":" };
    vec![
        ("GEM_HOME".into(), dir.to_string_lossy().into_owned()),
        ("GEM_PATH".into(), format!("{}{sep}", dir.to_string_lossy())),
        // ruby-lsp otherwise sets up a bundle in the project (`.ruby-lsp/`) and runs
        // `bundle install`: Graphyne's empty Gemfile skips that.
        ("BUNDLE_GEMFILE".into(), dir.join("Gemfile").to_string_lossy().into_owned()),
    ]
}

fn install_ruby_lsp(dir: &Path, progress: &dyn Fn(&str)) -> Result<String, String> {
    let ruby = find("ruby").ok_or("Ruby isn't installed")?;
    let gem = find("gem").ok_or("gem isn't installed")?;
    // Ruby 3.4+ brings prism and rbs itself: then nothing needs compiling (prism's own
    // gem builds a C extension, which on Windows needs Ruby's Devkit).
    let own = command(&ruby)
        .args(["-e", "require 'prism'; require 'rbs'; exit(Gem::Version.new(Prism::VERSION) >= Gem::Version.new('1.2') ? 0 : 1)"])
        .env_remove("GEM_HOME")
        .env_remove("GEM_PATH")
        .status()
        .is_ok_and(|s| s.success());
    progress(&format!("Installing Ruby LSP {RUBY_LSP_VERSION} with gem…"));
    let gems: Vec<(&str, &str)> = if own { vec![("ruby-lsp", RUBY_LSP_VERSION), ("language_server-protocol", LSP_PROTOCOL_GEM_VERSION)] } else { vec![("ruby-lsp", RUBY_LSP_VERSION)] };
    for (name, version) in gems {
        let mut cmd = command(&gem);
        cmd.args(["install", name, "-v", version, "--no-document", "--install-dir"]).arg(dir).current_dir(dir).envs(ruby_env(dir)).env_remove("BUNDLE_GEMFILE");
        if own {
            cmd.arg("--ignore-dependencies");
        }
        run(cmd, "gem install").map_err(|e| if own { e } else { format!("{e}\n(Ruby LSP needs prism, which this Ruby can't build: install Ruby 3.4 or newer, or Ruby with the Devkit)") })?;
    }
    std::fs::write(dir.join("Gemfile"), "source \"https://rubygems.org\"\n").map_err(|e| e.to_string())?;
    // `gem install` leaves the downloaded .gem files in its working folder.
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.path().extension().is_some_and(|e| e == "gem") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(RUBY_LSP_VERSION.into())
}

/// The newest csharp-ls that runs on .NET SDK `major` (each release targets one .NET).
fn csharp_ls_version(major: u32) -> Option<&'static str> {
    match major {
        11.. => Some("0.29.0"),
        10 => Some("0.28.0"),
        9 => Some("0.20.0"),
        8 => Some("0.16.0"),
        _ => None,
    }
}

/// The newest installed .NET SDK's major version (`dotnet --list-sdks`).
fn dotnet_sdk_major(dotnet: &Path) -> Option<u32> {
    let out = command(dotnet).arg("--list-sdks").env("DOTNET_NOLOGO", "1").output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.split('.').next()?.trim().parse::<u32>().ok()).max()
}

fn install_csharp_ls(dir: &Path, progress: &dyn Fn(&str)) -> Result<String, String> {
    let dotnet = find("dotnet").ok_or(".NET isn't installed")?;
    let major = dotnet_sdk_major(&dotnet).ok_or("the .NET SDK isn't installed")?;
    let version = csharp_ls_version(major).ok_or_else(|| format!("csharp-ls needs the .NET SDK 8 or newer (this computer has .NET {major})"))?;
    progress(&format!("Installing csharp-ls {version} with dotnet…"));
    let mut cmd = command(&dotnet);
    cmd.args(["tool", "install", "csharp-ls", "--version", version, "--tool-path"])
        .arg(dir)
        .env("DOTNET_NOLOGO", "1")
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .current_dir(dir);
    run(cmd, "dotnet tool install")?;
    Ok(version.into())
}

/// Download, check and unpack `url` into `dir` (its top folder, if it has one, flattened).
fn install_archive(dir: &Path, url: &str, sha256: &str, what: &str, progress: &dyn Fn(&str)) -> Result<String, String> {
    progress(&format!("Downloading {what}…"));
    let staging = fetch_unpacked(url, sha256, dir).map_err(|e| format!("Could not download {url}: {e}. Check the connection"))?;
    progress(&format!("Unpacking {what}…"));
    for entry in std::fs::read_dir(&staging).map_err(|e| e.to_string())?.flatten() {
        let to = dir.join(entry.file_name());
        std::fs::rename(entry.path(), &to).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    let _ = std::fs::remove_dir_all(&staging);
    Ok(url.rsplit('/').next().unwrap_or(url).to_string())
}

fn install_kotlin(dir: &Path, progress: &dyn Fn(&str)) -> Result<String, String> {
    let version = install_archive(dir, KOTLIN_URL, KOTLIN_SHA256, "the Kotlin language server (370 MB)", progress)?;
    // Its settings, caches and logs stay in Graphyne's folder, not in the user's profile.
    let data = root_dir().ok_or("no folder for Graphyne's state")?.join("kotlin-lsp-data");
    let fwd = |p: PathBuf| p.to_string_lossy().replace('\\', "/");
    let extra = format!(
        "\nidea.config.path={}\nidea.system.path={}\nidea.log.path={}\nidea.plugins.path={}\n",
        fwd(data.join("config")),
        fwd(data.join("system")),
        fwd(data.join("log")),
        fwd(data.join("plugins"))
    );
    let props = dir.join("bin").join("idea.properties");
    let mut text = std::fs::read_to_string(&props).unwrap_or_default();
    text.push_str(&extra);
    std::fs::write(&props, text).map_err(|e| format!("{}: {e}", props.display()))?;
    if !dir.join("bin").join(exe("intellij-server")).is_file() {
        return Err("the Kotlin server's package has no bin/intellij-server".into());
    }
    Ok(version)
}

fn install_metals(dir: &Path, progress: &dyn Fn(&str)) -> Result<String, String> {
    let java = find("java").ok_or("Java isn't installed")?;
    let coursier = dir.join("coursier.jar");
    if !coursier.is_file() {
        progress("Downloading coursier (Scala's package fetcher)…");
        fetch_file(COURSIER_URL, COURSIER_SHA256, &coursier).map_err(|e| format!("Could not download {COURSIER_URL}: {e}. Check the connection"))?;
    }
    progress(&format!("Downloading Metals ({METALS}, about 180 MB)…"));
    let mut cmd = command(&java);
    cmd.arg("-jar").arg(&coursier).args(["fetch", "--classpath", METALS]).env("COURSIER_CACHE", dir.join("cache")).current_dir(dir);
    let classpath = run(cmd, "coursier")?;
    let classpath = classpath.lines().map(str::trim).rfind(|l| !l.is_empty()).ok_or("coursier gave no classpath")?;
    // A Java argument file: the classpath is longer than Windows allows on a command line.
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "/"));
    let args = format!("-Xss4m\n-Xms100m\n-Dmetals.client=graphyne\n-cp\n{}\nscala.meta.metals.Main\n", quote(classpath));
    std::fs::write(dir.join("metals.args"), args).map_err(|e| e.to_string())?;
    Ok(METALS.rsplit(':').next().unwrap_or(METALS).to_string())
}

/// `java -version`'s major version (8 for "1.8.0_402", 21 for "21.0.2").
fn java_major() -> Option<u32> {
    let java = find("java")?;
    let out = command(&java).arg("-version").output().ok()?;
    parse_java_major(&String::from_utf8_lossy(&out.stderr))
}

fn parse_java_major(text: &str) -> Option<u32> {
    let quoted = text.split('"').nth(1)?;
    let mut parts = quoted.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 { parts.next()?.parse().ok() } else { Some(first) }
}

fn command(program: &Path) -> Command {
    let mut cmd = hidden_command(&program.to_string_lossy());
    cmd.stdin(Stdio::null());
    cmd
}

/// Run to completion; stdout on success, else the last lines of its output.
fn run(mut cmd: Command, what: &str) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let all = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let lines: Vec<&str> = all.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    Err(format!("{what} failed:\n{}", lines[lines.len().saturating_sub(8)..].join("\n")))
}

fn remove_dir(dir: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{} is in use ({e}): close what runs from it and try again", dir.display())),
    }
}

fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

fn path_arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_versions() {
        assert_eq!(parse_java_major("openjdk version \"25.0.1\" 2025-10-21"), Some(25));
        assert_eq!(parse_java_major("java version \"1.8.0_402\""), Some(8));
        assert_eq!(parse_java_major("openjdk version \"21\" 2023-09-19"), Some(21));
        assert_eq!(parse_java_major("no java here"), None);
    }

    #[test]
    fn csharp_ls_follows_the_sdk() {
        assert_eq!(csharp_ls_version(10), Some("0.28.0"));
        assert_eq!(csharp_ls_version(12), Some("0.29.0"));
        assert_eq!(csharp_ls_version(6), None);
    }

    #[test]
    fn every_recipe_has_a_spec() {
        use graphyne_core::LanguageId::*;
        let names: Vec<&str> = [Php, Ruby, CSharp, Java, Kotlin, Scala].into_iter().flat_map(|l| crate::servers::specs_for(l).iter().map(|s| s.name)).collect();
        for r in RECIPES {
            assert!(names.contains(&r.server), "{} has no ServerSpec", r.server);
        }
    }
}
