-- Zig language support. The grammar library is native code, so Graphyne loads it only
-- after you allow the "native" permission on the Plugins page.

local libs = { windows = "zig.dll", macos = "libzig.dylib", linux = "libzig.so" }

graphyne.languages.register({
  name = "Zig",
  extensions = { "zig", "zon" },
  line_comment = "//",
  -- https://github.com/zigtools/zls — used when zls is on PATH.
  lsp = { command = "zls" },
  -- {file}, {dir} and {name} are filled in by Run (Shift+F10).
  run = "zig run {file}",
  grammar = {
    library = libs[graphyne.os] or "libzig.so",
    symbol = "tree_sitter_zig",
    highlights = "highlights.scm",
  },
})
