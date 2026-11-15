//! The language / toolchain matrix (spec §6.3).

use std::path::Path;
use std::sync::RwLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LanguageId {
    Rust,
    C,
    Cpp,
    Python,
    Go,
    TypeScript,
    Tsx,
    JavaScript,
    CSharp,
    Java,
    Kotlin,
    Php,
    Dart,
    Lua,
    Ruby,
    Scala,
    Swift,
    Html,
    Css,
    Sql,
    Json,
    Toml,
    Yaml,
    Markdown,
    Shell,
    PowerShell,
    PlainText,
    /// Added by an extension (`graphyne.languages.register`): index into the registry.
    Plugin(u16),
}

/// A language registered by an extension.
#[derive(Clone)]
pub struct PluginLanguage {
    pub name: &'static str,
    pub extensions: Vec<String>,
    pub file_names: Vec<String>,
    pub info: &'static LanguageInfo,
    /// Run command template (`{file}`, `{dir}`, `{name}`).
    pub run: Option<String>,
}

static PLUGIN_LANGUAGES: RwLock<Vec<PluginLanguage>> = RwLock::new(Vec::new());

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Register (or update, by name) an extension language; returns its id. Re-registering
/// keeps the id, so open files and caches stay valid across plugin reloads.
pub fn register_plugin_language(name: &str, extensions: &[String], file_names: &[String], line_comment: Option<&str>, run: Option<&str>) -> LanguageId {
    let mut langs = PLUGIN_LANGUAGES.write().unwrap_or_else(|e| e.into_inner());
    let ix = langs.iter().position(|l| l.name == name);
    let info: &'static LanguageInfo = match ix.map(|i| langs[i].info) {
        // Unchanged comment token: keep the leaked info (no leak per reload).
        Some(old) if old.line_comment == line_comment => old,
        _ => Box::leak(Box::new(LanguageInfo {
            name: leak(name.to_string()),
            badge: "EXT",
            badge_color: 0x8A8F98,
            line_comment: line_comment.map(|c| leak(c.to_string())),
            lsp: None,
            dap: None,
            toolchain: None,
        })),
    };
    let entry = PluginLanguage {
        name: info.name,
        extensions: extensions.iter().map(|e| e.trim_start_matches('.').to_ascii_lowercase()).collect(),
        file_names: file_names.to_vec(),
        info,
        run: run.map(str::to_string),
    };
    match ix {
        Some(i) => {
            langs[i] = entry;
            LanguageId::Plugin(i as u16)
        }
        None => {
            langs.push(entry);
            LanguageId::Plugin((langs.len() - 1) as u16)
        }
    }
}

pub fn plugin_language(ix: u16) -> Option<PluginLanguage> {
    PLUGIN_LANGUAGES.read().unwrap_or_else(|e| e.into_inner()).get(ix as usize).cloned()
}

/// Extension languages win only for files no built-in language claims.
fn plugin_language_for(file_name: &str, ext: &str) -> Option<LanguageId> {
    let langs = PLUGIN_LANGUAGES.read().unwrap_or_else(|e| e.into_inner());
    langs
        .iter()
        .position(|l| l.file_names.iter().any(|f| f == file_name) || (!ext.is_empty() && l.extensions.iter().any(|e| e == ext)))
        .map(|i| LanguageId::Plugin(i as u16))
}

/// Static description of a language row of the matrix.
pub struct LanguageInfo {
    pub name: &'static str,
    /// Two/three letter badge drawn as a file icon and its colour (0xRRGGBB).
    pub badge: &'static str,
    pub badge_color: u32,
    pub line_comment: Option<&'static str>,
    pub lsp: Option<&'static str>,
    pub dap: Option<&'static str>,
    /// Toolchain probe: program + args, printed in the status bar.
    pub toolchain: Option<(&'static str, &'static [&'static str])>,
}

impl LanguageId {
    pub fn from_path(path: &Path) -> LanguageId {
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        match file_name {
            "Cargo.lock" => return LanguageId::Toml,
            "Dockerfile" | "Makefile" => return LanguageId::Shell,
            _ => {}
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        match ext.as_str() {
            "rs" => LanguageId::Rust,
            "c" | "h" => LanguageId::C,
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "ino" => LanguageId::Cpp,
            "py" | "pyi" | "pyw" => LanguageId::Python,
            "go" => LanguageId::Go,
            "ts" | "mts" | "cts" => LanguageId::TypeScript,
            "tsx" => LanguageId::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => LanguageId::JavaScript,
            "cs" => LanguageId::CSharp,
            "java" => LanguageId::Java,
            "kt" | "kts" => LanguageId::Kotlin,
            "php" => LanguageId::Php,
            "dart" => LanguageId::Dart,
            "lua" => LanguageId::Lua,
            "rb" => LanguageId::Ruby,
            "scala" | "sc" => LanguageId::Scala,
            "swift" => LanguageId::Swift,
            "html" | "htm" => LanguageId::Html,
            "css" | "scss" => LanguageId::Css,
            "sql" => LanguageId::Sql,
            "json" | "jsonc" => LanguageId::Json,
            "toml" => LanguageId::Toml,
            "yaml" | "yml" => LanguageId::Yaml,
            "md" | "markdown" => LanguageId::Markdown,
            "sh" | "bash" | "zsh" => LanguageId::Shell,
            "ps1" | "psm1" => LanguageId::PowerShell,
            _ => plugin_language_for(file_name, &ext).unwrap_or(LanguageId::PlainText),
        }
    }

    pub fn info(self) -> &'static LanguageInfo {
        use LanguageId::*;
        macro_rules! info {
            ($name:expr, $badge:expr, $color:expr, $comment:expr, $lsp:expr, $dap:expr, $tool:expr) => {{
                static INFO: LanguageInfo = LanguageInfo {
                    name: $name,
                    badge: $badge,
                    badge_color: $color,
                    line_comment: $comment,
                    lsp: $lsp,
                    dap: $dap,
                    toolchain: $tool,
                };
                &INFO
            }};
        }
        match self {
            Rust => info!("Rust", "RS", 0xE8875B, Some("//"), Some("rust-analyzer"), Some("lldb-dap"), Some(("rustc", &["--version"]))),
            C => info!("C", "C", 0x6E9BD8, Some("//"), Some("clangd"), Some("lldb-dap"), Some(("gcc", &["--version"]))),
            Cpp => info!("C++", "C++", 0x5B8DD6, Some("//"), Some("clangd"), Some("lldb-dap"), Some(("g++", &["--version"]))),
            Python => info!("Python", "PY", 0x4B8BBE, Some("#"), Some("pyright"), Some("debugpy"), Some(("python", &["--version"]))),
            Go => info!("Go", "GO", 0x4FC1DE, Some("//"), Some("gopls"), Some("delve"), Some(("go", &["version"]))),
            TypeScript => info!("TypeScript", "TS", 0x3178C6, Some("//"), Some("vtsls"), Some("js-debug"), Some(("node", &["--version"]))),
            Tsx => info!("TSX", "TSX", 0x3178C6, Some("//"), Some("vtsls"), Some("js-debug"), Some(("node", &["--version"]))),
            JavaScript => info!("JavaScript", "JS", 0xD9B937, Some("//"), Some("vtsls"), Some("js-debug"), Some(("node", &["--version"]))),
            CSharp => info!("C#", "C#", 0x8E5CC4, Some("//"), Some("roslyn-ls"), Some("netcoredbg"), Some(("dotnet", &["--version"]))),
            Java => info!("Java", "JV", 0xE76F00, Some("//"), Some("jdtls"), Some("java-debug-adapter"), Some(("java", &["--version"]))),
            Kotlin => info!("Kotlin", "KT", 0xA97BFF, Some("//"), Some("kotlin-language-server"), Some("kotlin-debug"), Some(("kotlinc", &["-version"]))),
            Php => info!("PHP", "PHP", 0x8892BF, Some("//"), Some("intelephense"), Some("xdebug"), Some(("php", &["--version"]))),
            Dart => info!("Dart", "DT", 0x2CB7F6, Some("//"), Some("dart language-server"), Some("dart-debug"), Some(("dart", &["--version"]))),
            Lua => info!("Lua", "LUA", 0x5A6FD6, Some("--"), Some("lua-language-server"), Some("local-lua-debugger"), Some(("lua", &["-v"]))),
            Ruby => info!("Ruby", "RB", 0xD8453E, Some("#"), Some("ruby-lsp"), Some("rdbg"), Some(("ruby", &["--version"]))),
            Scala => info!("Scala", "SC", 0xDC322F, Some("//"), Some("metals"), Some("metals-dap"), Some(("scala-cli", &["version"]))),
            Swift => info!("Swift", "SW", 0xF05138, Some("//"), Some("sourcekit-lsp"), Some("lldb-dap"), Some(("swift", &["--version"]))),
            Html => info!("HTML", "<>", 0xE44D26, None, Some("vscode-html-language-server"), None, None),
            Css => info!("CSS", "#", 0x5E8DE0, None, Some("vscode-css-language-server"), None, None),
            Sql => info!("SQL", "SQL", 0xC9A35A, Some("--"), Some("sqls"), None, None),
            Json => info!("JSON", "{}", 0xCBA052, None, None, None, None),
            Toml => info!("TOML", "TM", 0x9C8D7B, Some("#"), Some("taplo"), None, None),
            Yaml => info!("YAML", "YML", 0xCB5A5A, Some("#"), None, None, None),
            Markdown => info!("Markdown", "MD", 0x7C9CBF, None, None, None, None),
            Shell => info!("Shell", "SH", 0x89B482, Some("#"), Some("bash-language-server"), None, None),
            PowerShell => info!("PowerShell", "PS", 0x5391FE, Some("#"), None, None, None),
            PlainText => info!("Plain Text", "TXT", 0x8A8F98, None, None, None, None),
            Plugin(ix) => plugin_language(ix).map(|l| l.info).unwrap_or_else(|| PlainText.info()),
        }
    }

    pub fn name(self) -> &'static str {
        self.info().name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detect() {
        assert_eq!(LanguageId::from_path(Path::new("src/main.rs")), LanguageId::Rust);
        assert_eq!(LanguageId::from_path(Path::new("a/B.TSX")), LanguageId::Tsx);
        assert_eq!(LanguageId::from_path(Path::new("Cargo.lock")), LanguageId::Toml);
    }
}
