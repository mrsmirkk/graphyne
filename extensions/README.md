# Расширения Graphyne

Расширение — папка в `plugins/` рядом с `graphyne.exe` (как Extensions в VS Code или плагины
JetBrains). Graphyne подхватывает новые и изменённые расширения сразу, без перезапуска.
Управление — страница **Plugins** (кнопка «+» в левой панели или главное меню ⚙).

```
plugins/
  my-extension/
    plugin.json   # название, версия, описание, автор, нужные разрешения
    init.lua      # код: регистрирует темы, языки, клавиши, вкладки…
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

## Что может расширение

- **Тему:** `graphyne.themes.register({ name, ui = { canvas = "#…", … }, syntax = { keyword = "#…", … } })`.
  Пример — [`nord-theme`](nord-theme/init.lua), там перечислены все токены.
- **Язык:** `graphyne.languages.register({ name, extensions, file_names, line_comment, lsp = { command, args }, run, grammar = { library, symbol, highlights } })`.
  Пример — [`zig`](zig/init.lua): подсветка через грамматику tree-sitter, языковой сервер `zls`,
  запуск `zig run {file}`.
- **Клавиши, вкладки, команды:** `graphyne.keymap.set`, `graphyne.ui.register_tab`,
  `graphyne.terminal.run`, `graphyne.buffer.*` — см. README релиза.

## Безопасность

Расширения работают в песочнице: нет `os.execute`, `io.popen`, `ffi`, `debug`; `require`
загружает модули только из папки самого расширения. Если расширению нужно больше, оно
перечисляет это в `"permissions"`, а пользователь разрешает на странице Plugins:

| Разрешение | Что открывает |
| --- | --- |
| `os` | запуск программ, удаление и переименование файлов |
| `io` | `io.popen` — команды оболочки |
| `ffi` | вызов C-кода через LuaJIT FFI |
| `native` | загрузка библиотеки грамматики — машинного кода вне песочницы Lua |

## Грамматика языка

Грамматика — библиотека tree-sitter, которая экспортирует `tree_sitter_<язык>`. Самый простой
способ её собрать — маленький проект на Rust, как в [`zig/grammar`](zig/grammar):

```
cd zig/grammar
cargo build --release
copy target\release\graphyne_zig_grammar.dll ..\zig.dll
```

Скрипт упаковки релиза делает это сам для каждого расширения с папкой `grammar/`.
