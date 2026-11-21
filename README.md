<div align="center">

# Graphyne

**A fast, native code editor written in Rust — with a debugger, Git, a terminal and
LuaJIT extensions built in.**

![License: MIT](https://img.shields.io/badge/license-MIT-blue)
![Platform: Windows](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078D6)
![Status: alpha](https://img.shields.io/badge/status-alpha-orange)
![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-B7410E)

</div>

![Graphyne debugging a Python file: paused at a breakpoint with inline values, call stack and variables](docs/screenshots/debugger-dark.png)

Graphyne is an IDE built from scratch on [GPUI](https://www.gpui.rs) — the GPU-accelerated UI
framework behind the Zed editor — with an "islands" layout inspired by JetBrains Fleet. Every
pixel is drawn on the GPU, every language feature runs off the UI thread, and the whole IDE is a
single portable `.exe`.

> **Early alpha.** Graphyne is usable for everyday editing, running and debugging, but it is
> young: it is developed and tested on Windows 10/11 only, and some planned features are missing
> (see [Roadmap](#roadmap)). Bug reports and ideas are very welcome.

## Contents

- [Highlights](#highlights)
- [Screenshots](#screenshots)
- [Features](#features)
- [Roadmap](#roadmap)
- [Install](#install)
- [Keyboard shortcuts](#keyboard-shortcuts)
- [Extensions](#extensions)
- [Building from source](#building-from-source)
- [Project layout](#project-layout)
- [License](#license)

## Highlights

- **Native and portable** — one ~65 MB executable, no installer, no runtime to install.
- **Run anything with one key** — `Shift+F10` runs the current file in ~20 languages; HTML gets a
  live-reloading preview.
- **Real debugger** — breakpoints, conditions, logpoints, inline values, variables and an
  evaluation console over the Debug Adapter Protocol.
- **Zero-setup Python** — create a `.py` file and Graphyne makes a `.venv` with `debugpy` in it,
  so Run and Debug just work.
- **Tells you what's missing** — no `g++`, no JDK, no Go? Graphyne says what to install instead of
  failing in the terminal.
- **Git, the visual way** — branch graph, side-by-side diffs with word-level highlights, and
  commits of selected changes only.
- **Picks up where you left off** — open tabs, cursors, scroll positions, folded code, panels and
  window placement come back on the next launch.
- **Extensible with LuaJIT** — themes, new languages (with tree-sitter highlighting), shortcuts
  and panels, in a sandbox with explicit permissions.

## Screenshots

| Side-by-side diff with partial commits (Graphyne Light) | Settings |
| --- | --- |
| ![Side-by-side diff in the light theme](docs/screenshots/diff-light.png) | ![The Settings tab](docs/screenshots/settings-light.png) |

## Features

### Editor
- Syntax highlighting with **tree-sitter for 24 languages**: Rust, Python, JavaScript,
  TypeScript/TSX, Go, C, C++, C#, Java, Kotlin, PHP, Dart, Lua, Ruby, Scala, Swift, HTML, CSS,
  SQL, JSON, TOML, YAML, Bash — more through extensions.
- **Code folding** from the syntax tree (gutter arrows, `Ctrl+-` / `Ctrl+=`), remembered per file.
- **Smart selection** (`Ctrl+W` / `Ctrl+Shift+W`), line duplicate / delete / move, toggle comment.
- **Postfix templates**: type `expr.if`, `.for`, `.not`, `.sout` … and press `Tab`.
- **"N usages"** above declarations, **Structure** panel, breadcrumbs in the status bar.
- **Autosave**, a **hex viewer** for binary files, and a plain mode for files over 16 MB.
- **Secrets Guard** flags API keys, private keys and tokens before they reach a commit.
- Shortcuts work on **any keyboard layout** (Cyrillic included) — they follow the physical key.

### Language intelligence (LSP)
- Diagnostics, completion with auto-import, documentation on hover (`Ctrl+Q`), go to definition
  (`Ctrl+B`, `F12`, `Ctrl+click`), parameter info (`Ctrl+P`), quick fixes with preview
  (`Alt+Enter`), inlay hints and usage counts, with incremental document sync.
- Language servers are found on `PATH` automatically: `rust-analyzer`, `pyright` / `basedpyright` /
  `pylsp`, `gopls`, `clangd`, `vtsls` / `typescript-language-server`, `lua-language-server` and more.
  Without one, Graphyne still highlights and completes words from the file.

### Run
- **One-click Run** (`Shift+F10`) for Rust (Cargo or single files), C/C++, Python, Go,
  JavaScript/TypeScript, C#, Java, Kotlin, PHP, Dart, Lua, Ruby, Scala, Swift and shell scripts —
  each in its own terminal tab.
- **Live preview** for HTML/CSS: a local server on `127.0.0.1` that reloads the page on every save.
- **Run presets** for auditing and reverse engineering: `cargo audit`, `clippy`, `nmap`,
  `gobuster`, `valgrind`, `strace`, `gdb` (only the installed ones are enabled).
- **Missing toolchain detection**: if the compiler or runtime isn't installed, Graphyne says so —
  with a link and a `winget` command — and shows "`g++` not found" in the status bar.

### Debug
- Debug Adapter Protocol client: **breakpoints, conditional breakpoints and logpoints** (remembered
  per project), step over / into / out, pause, restart, **inline variable values**, call stack,
  variables tree and an evaluation console. The program runs in a terminal tab, so it can read input.
- **Python** via `debugpy` — fully supported and tested.
- Rust, C and C++ via `lldb-dap` / CodeLLDB / `gdb`, Go via `dlv` — wired up, not yet verified.

### Python projects
- When a Python file appears in a project (or on the first Run / Debug), Graphyne creates `.venv`
  and installs `debugpy`. Run, Debug and the language server all use that interpreter.
- If there is a `requirements.txt`, Graphyne **asks** before installing it (Install / Not now /
  Don't ask again) and asks again when the file changes. Poetry, Pipenv and conda projects are left
  to their own tools.

### Terminal
- Built-in terminal with tabs, scrollback, selection, clickable `file:line` links, and a
  **detachable window** (`Alt+Shift+F12`).
- Choose the shell in Settings: PowerShell 7, Windows PowerShell, Command Prompt, Git Bash or WSL
  (whatever is installed).

### Git
- Branch, changed files and commit from the **Git** tab; **History** with a branch graph.
- **Side-by-side diff** with changed words highlighted and horizontal scrolling.
- **Partial commits**: tick the blocks to include in the next commit.
- Change markers in the gutter, with a popup to view or roll back a change.

### Databases
- A **Database** panel (right side): connections → tables and views → columns with types and
  primary keys. **SQLite is built in** — nothing to install; opening a `.db` / `.sqlite` file from
  the project tree adds it.
- **SQL console** per connection: Run (the ▷ in the title bar) or `Ctrl+Enter` runs the statement
  under the cursor or the selection (scripts too), **Stop** cancels a long query, errors show with the failing statement.
  The console text is kept between sessions.
- **Data grid** for results and tables: virtualized (200,000 rows scroll smoothly), sort by a
  column, resize columns, keyboard navigation, copy a cell; tables show the total row count.
- Connections and consoles live in Graphyne's own state folder — nothing is written into the
  project.

### Navigation and workspace
- **File operations in the project tree**: right-click for New File, New Folder, Rename, Delete,
  Copy Path / Copy Relative Path and Reveal in File Explorer. Names are typed right in the tree
  (`src/utils/io.py` creates the folders too), `F2` renames, `Delete` moves to the **Recycle Bin**
  after a confirmation, and open tabs follow renamed files and folders. A single click previews a
  file and keeps the keyboard in the tree; a double click moves it to the editor.
- **Drag and drop** in the tree moves files and folders (the target folder is highlighted; open
  tabs follow). Files and folders dropped from Explorer are **copied** into the project.
- A **File menu** (☰ at the left of the title bar): New File / Folder, Open File, Open Folder,
  Save, **Save As**, Save All, Close Tab, Close All Tabs, Close Project, Settings, Exit.
- **Search Everywhere** (`Shift Shift` / `Ctrl+K`) for files, symbols and actions; **Find in
  Files** (`Ctrl+Shift+F`).
- **External Libraries**: the standard library and dependencies (Cargo, Python `site-packages`)
  right in the file tree.
- **Welcome screen** with recent projects, one window per project, **session restore** on launch,
  and **Close Project** to go back to the Welcome screen.
- Windows open at a size that follows the screen and its scaling (maximized on small laptop
  screens), and come back on the monitor they were closed on — shrunk to fit if that screen is
  smaller now.

### Settings
- A native **Settings** tab (`Ctrl+,`), saved to `settings.json`:
  - built-in **Graphyne Dark** and **Graphyne Light** themes, plus themes from extensions;
  - separate **interface, editor and terminal fonts** — any installed font, picked from a
    searchable list;
  - font sizes, usage / inlay hints, autosave and its delay, terminal shell, reopening projects on
    launch, Python project set-up.

## Roadmap

Planned, roughly in this order:

- [ ] **Databases, next steps** — PostgreSQL and MySQL connections (SQLite, the schema browser,
      the SQL console and the data grid are done), editing cells in the grid, exporting results.
- [ ] **Keymap editor** in Settings.
- [ ] **Debugging beyond Python** verified end to end: Rust / C / C++ (`lldb-dap`), Go (`delve`),
      JavaScript / TypeScript.
- [ ] **Python**: dependencies from `pyproject.toml` (uv, Poetry, PDM) and an interpreter picker.
- [ ] **macOS and Linux** builds (GPUI supports both; Graphyne isn't tested there yet).
- [ ] **Faster start-up** — most of today's ~0.4 s goes to Direct3D initialization.
- [ ] Easier extension install (from a URL or a gallery).

Have an idea? Open an issue.

## Install

1. Download `Graphyne-<version>-windows-x64.zip` from the [Releases](../../releases) page.
2. Unpack it anywhere and run `graphyne.exe`. There is no installer. Graphyne keeps its own data in
   `%APPDATA%\Graphyne` (settings, recent projects, session) and `~/.config/graphyne` (extensions'
   settings, your `init.lua`); in a project it only adds `.graphyne/` (breakpoints) and, for Python,
   `.venv`.

You can also start it with a folder: `graphyne.exe "C:\path\to\project"`, or drop a folder onto
`graphyne.exe` in Explorer.

**Requirements:** Windows 10 or 11 (x64) with a DirectX 11 GPU. JetBrains Mono and Inter fonts are
used when installed (otherwise Cascadia Mono / Segoe UI). For language features, run and debug,
install the toolchains you use (Python, Rust, Node.js, …) and their language servers — Graphyne
tells you when something is missing.

## Keyboard shortcuts

| Action | Shortcut |
| --- | --- |
| Search Everywhere | `Shift Shift`, `Ctrl+K` |
| All actions | `Ctrl+Shift+O` |
| Find in Files | `Ctrl+Shift+F` |
| Project tree: rename / delete to the Recycle Bin | `F2` / `Delete` |
| New file / open file | `Alt+Insert` / `Ctrl+O` |
| Settings | `Ctrl+,` |
| Run current file | `Shift+F10` (or `Ctrl+R`) |
| Debug / continue / step over / into / out / stop | `Shift+F9` / `F9` / `F8` / `F7` / `Shift+F8` / `Ctrl+F2` |
| Toggle breakpoint / edit condition or logpoint | `Ctrl+F8` (or click the line number) / `Ctrl+Shift+F8` |
| Go to definition / documentation / parameters | `Ctrl+B`, `F12` / `Ctrl+Q` / `Ctrl+P` |
| Quick fixes | `Alt+Enter` |
| SQL console: run statement / selection | `Ctrl+Enter` |
| Extend / shrink selection | `Ctrl+W` / `Ctrl+Shift+W` |
| Fold / unfold block (all: add `Shift`) | `Ctrl+-` / `Ctrl+=` |
| Toggle comment, duplicate line, delete line | `Ctrl+/`, `Ctrl+D`, `Ctrl+Shift+K` |
| Move line up / down | `Alt+Shift+↑` / `Alt+Shift+↓` |
| Terminal: show / new tab / detach | `Ctrl+J` / `Ctrl+Shift+T` / `Alt+Shift+F12` |
| Left / right panel | `Ctrl+1` / `Ctrl+Alt+B` |
| Next / previous tab, close tab | `Ctrl+Tab` / `Ctrl+Shift+Tab`, `Ctrl+F4` |
| Save / save as / save all | `Ctrl+S` / `Ctrl+Shift+S` / `Ctrl+Alt+S` |

## Extensions

An extension is a folder in `plugins\` next to `graphyne.exe` with a `plugin.json` and an
`init.lua`. Graphyne picks up new and changed extensions immediately — no restart. The
**Plugins** page (the `+` in the left panel, or the main menu) lists them, turns them on and off and
manages their permissions.

```lua
-- plugins/hello/init.lua
graphyne.keymap.set("editor", "ctrl-alt-d", function()
  graphyne.buffer.insert(os.date("%Y-%m-%d"))
end)

graphyne.themes.register({
  name = "Midnight",
  ui = { canvas = "#0b0e14", island = "#11151c", accent = "#7aa2f7" },
  syntax = { keyword = "#bb9af7", string = "#9ece6a" },
})
```

Extensions can add **themes**, **languages** (file types, comment syntax, a language server, a run
command and a tree-sitter grammar for highlighting), **shortcuts**, **panel tabs** and commands that
edit the open file. They run in a LuaJIT sandbox: no `os.execute`, `io.popen`, FFI or `debug`, and
`require` only sees the extension's own folder — anything more (`os`, `io`, `ffi`, or loading a
native grammar library) needs a permission the user grants on the Plugins page.

Two examples ship with Graphyne: the **Nord** theme and the **Zig** language with a tree-sitter
grammar — see [`extensions/`](extensions) and the [extension guide](extensions/README.md).

Your own `~/.config/graphyne/init.lua` runs at start-up and on every save, with the full Lua API and
no sandbox.

## Building from source

**Prerequisites (Windows):**
- Rust **1.88 or newer** with the MSVC toolchain (`rustup default stable-x86_64-pc-windows-msvc`).
- Visual Studio 2022 Build Tools with the **"Desktop development with C++"** workload (LuaJIT,
  libgit2 and the tree-sitter grammars are compiled from C).

```powershell
cargo build --release          # → target\release\graphyne.exe
cargo test --workspace         # unit tests of all crates
```

> If the build of LuaJIT fails with *"'minilua' is not recognized"*, your environment sets
> `NoDefaultCurrentDirectoryInExePath` (some shells and tools do). Remove it for the build:
> `Remove-Item Env:NoDefaultCurrentDirectoryInExePath`.

**Release package:** `powershell -ExecutionPolicy Bypass -File packaging\package-windows.ps1`
builds the release, compiles the example extensions' grammars, and writes
`dist\Graphyne-<version>-windows-x64.zip` with the license, third-party notices and a user guide.

The C runtime is linked statically (`.cargo/config.toml`), so the executable runs without the
Visual C++ Redistributable.

## Project layout

| Path | What it is |
| --- | --- |
| [`crates/graphyne`](crates/graphyne) | The executable (entry point, icon, Windows resources). |
| [`crates/graphyne-ui`](crates/graphyne-ui) | The GPUI front end: workspace, editor, panels, terminal view, debugger UI, settings, plugins page. |
| [`crates/graphyne-core`](crates/graphyne-core) | UI-independent core: text buffer, languages, run plans, sessions, settings, Python environments, toolchain checks, project search, Secrets Guard. |
| [`crates/graphyne-syntax`](crates/graphyne-syntax) | tree-sitter parsing, highlighting, outline, folding, selection expansion. |
| [`crates/graphyne-lsp`](crates/graphyne-lsp) | Language Server Protocol client and the server registry. |
| [`crates/graphyne-dap`](crates/graphyne-dap) | Debug Adapter Protocol client and adapter set-up. |
| [`crates/graphyne-terminal`](crates/graphyne-terminal) | Terminal emulation (alacritty_terminal) on a pseudo-console. |
| [`crates/graphyne-git`](crates/graphyne-git) | Git through libgit2: status, history graph, diffs, partial commits. |
| [`crates/graphyne-lua`](crates/graphyne-lua) | The LuaJIT extension host: sandbox, permissions, API. |
| [`third_party/gpui`](third_party/gpui) | GPUI 0.2.2 with a few Windows fixes ([what changed](third_party/gpui/GRAPHYNE-CHANGES.md)). |
| [`extensions`](extensions) | Example extensions (Nord theme, Zig language). |
| [`assets`](assets) | App icon and UI / file icons. |
| [`packaging`](packaging) | Release script and the user guide shipped in the zip. |

## License

Graphyne is released under the [MIT License](LICENSE).

It bundles third-party components under their own licenses: **GPUI** (Apache-2.0, © Zed
Industries, vendored with small changes), **Lucide** icons (ISC), **Material Icon Theme** file icons
(MIT), and many Rust crates — the release zip lists them all in `THIRD-PARTY-NOTICES.md`.

Graphyne is an independent project. It is not affiliated with or endorsed by JetBrains or Zed
Industries; "Fleet" is a trademark of JetBrains s.r.o.
