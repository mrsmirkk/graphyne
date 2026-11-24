//! Language → language-server matrix (spec §6.3) and executable discovery.

use graphyne_core::LanguageId;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ServerSpec {
    /// Stable key; one server instance per key per project.
    pub name: &'static str,
    pub program: &'static str,
    pub args: &'static [&'static str],
    /// Shown when the server is missing.
    pub install_hint: &'static str,
}

macro_rules! spec {
    ($name:expr, $prog:expr, [$($arg:expr),*], $hint:expr) => {
        ServerSpec { name: $name, program: $prog, args: &[$($arg),*], install_hint: $hint }
    };
}

static RUST: &[ServerSpec] = &[spec!("rust-analyzer", "rust-analyzer", [], "rustup component add rust-analyzer")];
static PYTHON: &[ServerSpec] = &[
    spec!("pyright", "pyright-langserver", ["--stdio"], "pip install pyright"),
    spec!("basedpyright", "basedpyright-langserver", ["--stdio"], "pip install basedpyright"),
    spec!("pylsp", "pylsp", [], "pip install python-lsp-server"),
    spec!("jedi", "jedi-language-server", [], "pip install jedi-language-server"),
];
static TS: &[ServerSpec] = &[
    spec!("vtsls", "vtsls", ["--stdio"], "npm i -g @vtsls/language-server"),
    spec!("tsserver", "typescript-language-server", ["--stdio"], "npm i -g typescript-language-server typescript"),
];
static GO: &[ServerSpec] = &[spec!("gopls", "gopls", [], "go install golang.org/x/tools/gopls@latest")];
static C_FAMILY: &[ServerSpec] = &[spec!("clangd", "clangd", [], "install LLVM (clangd)")];
static CSHARP: &[ServerSpec] = &[
    spec!("roslyn-ls", "roslyn-language-server", ["--stdio"], "dotnet tool install -g roslyn-language-server"),
    spec!("csharp-ls", "csharp-ls", [], "dotnet tool install -g csharp-ls"),
];
static JAVA: &[ServerSpec] = &[spec!("jdtls", "jdtls", [], "install Eclipse JDT LS")];
static KOTLIN: &[ServerSpec] = &[
    spec!("kotlin-lsp", "kotlin-lsp", ["--stdio"], "install JetBrains' Kotlin LSP (github.com/Kotlin/kotlin-lsp)"),
    spec!("kotlin-ls", "kotlin-language-server", [], "install kotlin-language-server"),
];
static PHP: &[ServerSpec] = &[spec!("intelephense", "intelephense", ["--stdio"], "npm i -g intelephense")];
static DART: &[ServerSpec] = &[spec!("dart", "dart", ["language-server"], "install the Dart SDK")];
static LUA: &[ServerSpec] = &[spec!("lua-ls", "lua-language-server", [], "install lua-language-server")];
static RUBY: &[ServerSpec] = &[spec!("ruby-lsp", "ruby-lsp", [], "gem install ruby-lsp")];
static SCALA: &[ServerSpec] = &[spec!("metals", "metals", [], "cs install metals")];
static SWIFT: &[ServerSpec] = &[spec!("sourcekit-lsp", "sourcekit-lsp", [], "install the Swift toolchain")];
static HTML: &[ServerSpec] = &[spec!("html-ls", "vscode-html-language-server", ["--stdio"], "npm i -g vscode-langservers-extracted")];
static CSS: &[ServerSpec] = &[spec!("css-ls", "vscode-css-language-server", ["--stdio"], "npm i -g vscode-langservers-extracted")];
static JSON: &[ServerSpec] = &[spec!("json-ls", "vscode-json-language-server", ["--stdio"], "npm i -g vscode-langservers-extracted")];
static SQL: &[ServerSpec] = &[spec!("sqls", "sqls", [], "go install github.com/sqls-server/sqls@latest")];
static TOML: &[ServerSpec] = &[spec!("taplo", "taplo", ["lsp", "stdio"], "cargo install taplo-cli --features lsp")];
static SHELL: &[ServerSpec] = &[spec!("bash-ls", "bash-language-server", ["start"], "npm i -g bash-language-server")];

pub fn specs_for(lang: LanguageId) -> &'static [ServerSpec] {
    use LanguageId::*;
    match lang {
        Rust => RUST,
        Python => PYTHON,
        TypeScript | Tsx | JavaScript => TS,
        Go => GO,
        C | Cpp => C_FAMILY,
        CSharp => CSHARP,
        Java => JAVA,
        Kotlin => KOTLIN,
        Php => PHP,
        Dart => DART,
        Lua => LUA,
        Ruby => RUBY,
        Scala => SCALA,
        Swift => SWIFT,
        Html => HTML,
        Css => CSS,
        Json => JSON,
        Sql => SQL,
        Toml => TOML,
        Shell => SHELL,
        Yaml | Markdown | PowerShell | PlainText => &[],
        Plugin(ix) => PLUGIN_SERVERS.read().unwrap_or_else(|e| e.into_inner()).get(&ix).map(|(s, _)| *s).unwrap_or(&[]),
    }
}

/// Language servers declared by extensions, with the LSP languageId (lower-case name).
static PLUGIN_SERVERS: std::sync::RwLock<std::collections::BTreeMap<u16, (&'static [ServerSpec], &'static str)>> = std::sync::RwLock::new(std::collections::BTreeMap::new());

/// `graphyne.languages.register{ lsp = { command, args } }`.
pub fn register_plugin_server(lang: LanguageId, command: &str, args: &[String]) {
    let LanguageId::Plugin(ix) = lang else { return };
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    let args: Vec<&'static str> = args.iter().map(|a| leak(a.clone())).collect();
    let spec = ServerSpec {
        name: leak(format!("{}-{command}", lang.name())),
        program: leak(command.to_string()),
        args: Box::leak(args.into_boxed_slice()),
        install_hint: "see the extension's description",
    };
    let id = leak(lang.name().to_ascii_lowercase());
    PLUGIN_SERVERS.write().unwrap_or_else(|e| e.into_inner()).insert(ix, (Box::leak(vec![spec].into_boxed_slice()), id));
}

/// LSP `languageId` for `textDocument/didOpen`.
pub fn language_id(lang: LanguageId) -> &'static str {
    use LanguageId::*;
    match lang {
        Rust => "rust",
        C => "c",
        Cpp => "cpp",
        Python => "python",
        Go => "go",
        TypeScript => "typescript",
        Tsx => "typescriptreact",
        JavaScript => "javascript",
        CSharp => "csharp",
        Java => "java",
        Kotlin => "kotlin",
        Php => "php",
        Dart => "dart",
        Lua => "lua",
        Ruby => "ruby",
        Scala => "scala",
        Swift => "swift",
        Html => "html",
        Css => "css",
        Sql => "sql",
        Json => "json",
        Toml => "toml",
        Yaml => "yaml",
        Markdown => "markdown",
        Shell => "shellscript",
        PowerShell => "powershell",
        PlainText => "plaintext",
        Plugin(ix) => PLUGIN_SERVERS.read().unwrap_or_else(|e| e.into_inner()).get(&ix).map(|(_, id)| *id).unwrap_or("plaintext"),
    }
}

/// Locate an executable on PATH (plus `~/.cargo/bin`), honouring Windows extensions.
pub fn which(program: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".cargo").join("bin"));
    }
    for dir in dirs {
        for ext in exts {
            let candidate = dir.join(format!("{program}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// How to start a server: the program, its arguments and extra environment.
#[derive(Clone, Debug, PartialEq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// `initializationOptions` for `initialize`.
    pub init_options: Option<serde_json::Value>,
}

/// First installed server for a language: one on PATH (the user's own) wins over the copy
/// Graphyne installed (`managed`). Blocking (filesystem probing, sometimes a version check).
pub fn find(lang: LanguageId, root: &Path) -> Result<(&'static ServerSpec, Launch), Option<&'static ServerSpec>> {
    let specs = specs_for(lang);
    for spec in specs {
        if let Some(path) = which(spec.program) {
            return Ok((spec, Launch { program: path, args: spec.args.iter().map(|a| a.to_string()).collect(), env: Vec::new(), init_options: None }));
        }
    }
    for spec in specs {
        if let Some(launch) = crate::managed::launch(spec.name, root) {
            return Ok((spec, launch));
        }
    }
    Err(specs.first())
}
