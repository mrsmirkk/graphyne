# Graphyne extensions

An extension is a folder in `plugins\` next to `graphyne.exe` — like extensions in VS Code or
plugins in JetBrains IDEs. Graphyne loads new and changed extensions immediately, without a
restart. Manage them on the **Plugins** page (the `+` in the left panel, or the main menu ⚙).

```
plugins/
  my-extension/
    plugin.json   # name, version, description, author, permissions it needs
    init.lua      # the code: registers themes, languages, shortcuts, panel tabs…
```

`plugin.json`:

```json
{
  "name": "My Extension",
  "version": "1.0.0",
  "description": "What it adds",
  "author": "Me",
  "permissions": ["native"]
}
```

## What an extension can add

- **A theme:** `graphyne.themes.register({ name, ui = { canvas = "#…", … }, syntax = { keyword = "#…", … } })`.
  See [`nord-theme`](nord-theme/init.lua) for every colour token. Themes appear next to the
  built-in Graphyne Dark and Graphyne Light in Settings.
- **A language:** `graphyne.languages.register({ name, extensions, file_names, line_comment, lsp = { command, args }, run, grammar = { library, symbol, highlights } })`.
  See [`zig`](zig/init.lua): highlighting through a tree-sitter grammar, the `zls` language server
  and `zig run {file}` for Run.
- **Shortcuts, panel tabs and commands** that read and edit the open file — see the API below.

## API

| Call | What it does |
| --- | --- |
| `graphyne.keymap.set("editor" \| "global", "ctrl-alt-d", fn)` | A keyboard shortcut. |
| `graphyne.buffer.get_text()`, `get_path()`, `get_language()` | The active file. |
| `graphyne.buffer.get_cursor()`, `get_selection()`, `node_at_cursor()` | Caret, selection, syntax-tree node under the caret. |
| `graphyne.buffer.insert(text)`, `replace_selection(text)`, `set_text(text)` | Edit the active file. |
| `graphyne.terminal.run("cmd", { tab = "name" })` | Run a command in a terminal tab. |
| `graphyne.ui.notify(message, "error"?)`, `print(...)` | Show a notification. |
| `graphyne.ui.register_tab({ title, side = "left" \| "right", items = fn })` | A tab in a side panel; items are strings or `{ text, detail, on_click }`. |
| `graphyne.ui.refresh_tab(tab)` | Re-read a tab's items. |
| `graphyne.workspace.root()`, `graphyne.workspace.open(path, line)` | Project folder; open a file. |
| `graphyne.themes.register{…}`, `graphyne.languages.register{…}` | See above. |
| `graphyne.settings.set("editor.font_size", 15)` | Override a setting for this session (`editor.font_size`, `terminal.font_size`, `editor.autosave_delay_ms`). |
| `graphyne.version`, `graphyne.os` | Graphyne's version and the operating system. |

Lua runs on its own thread: a script that hangs doesn't freeze the UI and is stopped after
2 seconds. That is why JIT compilation is off by default (compiled code skips that check); turn it
on with `jit.on()` if you need it.

## Security

Extensions run in a sandbox: there is no `os.execute`, `io.popen`, `ffi` or `debug`, and `require`
only loads modules from the extension's own folder. An extension that needs more lists it under
`"permissions"`, and the user allows it on the Plugins page:

| Permission | What it unlocks |
| --- | --- |
| `os` | Running programs, deleting and renaming files |
| `io` | `io.popen` — shell commands |
| `ffi` | Calling C code through the LuaJIT FFI |
| `native` | Loading a grammar library — native machine code outside the Lua sandbox |

Your own `~/.config/graphyne/init.lua` is trusted: it runs with the full API and no sandbox.

## Language grammars

A grammar is a tree-sitter library that exports `tree_sitter_<language>`. The simplest way to build
one is a small Rust project, as in [`zig/grammar`](zig/grammar):

```
cd zig/grammar
cargo build --release
copy target\release\graphyne_zig_grammar.dll ..\zig.dll
```

The release script (`packaging\package-windows.ps1`) does this for every extension that has a
`grammar/` folder.
