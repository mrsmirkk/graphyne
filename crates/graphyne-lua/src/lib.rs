//! Graphyne plugin engine (spec §8): a LuaJIT VM on its own thread.
//!
//! - `~/.config/graphyne/init.lua` (trusted, full standard library) and plugins from
//!   `~/.config/graphyne/plugins/` (`name.lua` or `name/init.lua`, sandboxed).
//! - Hot reload: any `.lua` change in the config folder rebuilds the VM from scratch.
//! - The IDE never waits for Lua: it sends [`ToLua`] messages and reacts to [`FromLua`]
//!   events. A script stuck in a loop blocks only this thread; a count hook stops callbacks
//!   that run longer than [`CALLBACK_BUDGET`]. The JIT compiler is off by default because
//!   compiled loops never reach that hook; init.lua can turn it on with `jit.on()`.
//! - Sandbox for plugins: no `os.execute` / `os.exit` / `os.remove` …, no `io.popen`, no
//!   `ffi`, `debug`, `getfenv`/`setfenv`, `package`; `load`/`loadstring` keep the sandbox
//!   and refuse bytecode; `require` only loads modules from the plugin's own folder.
//!   `graphyne.plugins.allow("name", { "os", "io", "ffi" })` in init.lua lifts parts of it.

use mlua::chunk::ChunkMode;
use mlua::{Function, HookTriggers, Lua, LuaOptions, StdLib, Table, Value, VmState};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long a single callback (or a file being loaded) may run.
pub const CALLBACK_BUDGET: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyContext {
    /// Only while an editor is focused.
    Editor,
    /// Anywhere in the window.
    Global,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TabItem {
    pub text: String,
    pub detail: Option<String>,
    pub clickable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BufferEdit {
    Insert(String),
    ReplaceSelection(String),
    SetText(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum SettingValue {
    Number(f64),
    Bool(bool),
    Text(String),
}

/// Events from the VM to the IDE.
#[derive(Clone, Debug, PartialEq)]
pub enum FromLua {
    /// A (re)load starts: forget key bindings and tabs of the previous load.
    Reset,
    /// Load finished: files that ran and errors (one message per failed file).
    Loaded { files: Vec<String>, errors: Vec<String> },
    Notify { message: String, error: bool },
    Keymap { id: u64, keys: String, context: KeyContext },
    RegisterTab { id: u64, title: String, side: Side },
    TabItems { tab: u64, items: Vec<TabItem> },
    Terminal { command: String, tab: Option<String> },
    Edit(BufferEdit),
    OpenFile { path: String, line: Option<usize> },
    Setting { key: String, value: SettingValue },
}

/// Snapshot of the active editor, sent with every callback (`graphyne.buffer.*` reads it).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditorContext {
    pub project_root: String,
    pub path: Option<String>,
    pub language: Option<String>,
    /// `None` without an editor or for very large files.
    pub text: Option<String>,
    /// 1-based.
    pub line: usize,
    pub column: usize,
    pub selection: String,
    pub node_kind: Option<String>,
    pub node_text: Option<String>,
}

/// Messages from the IDE to the VM.
#[derive(Clone, Debug)]
pub enum ToLua {
    Reload,
    Key { id: u64, ctx: EditorContext },
    TabRefresh { tab: u64 },
    TabClick { tab: u64, item: usize, ctx: EditorContext },
    Shutdown,
}

/// Settings `graphyne.settings.set` accepts, with their valid range.
pub const SETTINGS: &[(&str, f64, f64)] = &[("editor.font_size", 9., 32.), ("editor.autosave_delay_ms", 100., 10_000.), ("terminal.font_size", 9., 32.)];

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub struct LuaHost {
    tx: flume::Sender<ToLua>,
    events: flume::Receiver<FromLua>,
    config_dir: PathBuf,
    watcher: Option<notify::RecommendedWatcher>,
}

impl LuaHost {
    /// `~/.config/graphyne` on every platform (the spec's location); `GRAPHYNE_CONFIG_DIR`
    /// overrides it (portable installs, tests).
    pub fn default_config_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os("GRAPHYNE_CONFIG_DIR").filter(|d| !d.is_empty()) {
            return PathBuf::from(dir);
        }
        dirs::home_dir().unwrap_or_default().join(".config").join("graphyne")
    }

    /// Spawn the VM thread and load init.lua + plugins.
    pub fn start(config_dir: PathBuf) -> Self {
        let (tx, rx) = flume::unbounded();
        let (etx, erx) = flume::unbounded();
        let dir = config_dir.clone();
        let _ = std::thread::Builder::new().name("graphyne-lua".into()).spawn(move || vm_thread(dir, rx, etx));
        let mut host = Self { tx, events: erx, config_dir, watcher: None };
        host.watch();
        host.send(ToLua::Reload);
        host
    }

    pub fn send(&self, msg: ToLua) {
        let _ = self.tx.send(msg);
    }

    pub fn events(&self) -> flume::Receiver<FromLua> {
        self.events.clone()
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn init_file(&self) -> PathBuf {
        self.config_dir.join("init.lua")
    }

    pub fn plugins_dir(&self) -> PathBuf {
        self.config_dir.join("plugins")
    }

    /// Watch the config folder for `.lua` changes (call again once the folder exists).
    pub fn watch(&mut self) {
        if self.watcher.is_some() || !self.config_dir.is_dir() {
            return;
        }
        use notify::Watcher;
        let tx = self.tx.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            if matches!(ev.kind, notify::EventKind::Access(_)) {
                return;
            }
            if ev.paths.iter().any(|p| p.extension().is_some_and(|e| e == "lua")) {
                let _ = tx.send(ToLua::Reload);
            }
        });
        if let Ok(mut w) = watcher {
            if w.watch(&self.config_dir, notify::RecursiveMode::Recursive).is_ok() {
                self.watcher = Some(w);
            }
        }
    }
}

impl Drop for LuaHost {
    fn drop(&mut self) {
        let _ = self.tx.send(ToLua::Shutdown);
    }
}

fn vm_thread(dir: PathBuf, rx: flume::Receiver<ToLua>, out: flume::Sender<FromLua>) {
    let mut vm: Option<Vm> = None;
    while let Ok(msg) = rx.recv() {
        match msg {
            ToLua::Reload => {
                // An editor save fires several file events: settle, then reload once.
                std::thread::sleep(Duration::from_millis(150));
                let mut pending = Vec::new();
                for m in rx.try_iter() {
                    match m {
                        ToLua::Reload => {}
                        ToLua::Shutdown => return,
                        other => pending.push(other),
                    }
                }
                drop(vm.take());
                let fresh = Vm::load(&dir, &out);
                for m in pending {
                    fresh.handle(m, &out);
                }
                vm = Some(fresh);
            }
            ToLua::Shutdown => return,
            other => {
                if let Some(vm) = &vm {
                    vm.handle(other, &out);
                }
            }
        }
    }
}

#[derive(Default)]
struct Registry {
    keys: HashMap<u64, Function>,
    tabs: HashMap<u64, Function>,
    tab_clicks: HashMap<(u64, usize), Function>,
    allow: HashMap<String, HashSet<String>>,
    ctx: EditorContext,
    refresh: Vec<u64>,
}

struct Vm {
    lua: Lua,
    reg: Rc<RefCell<Registry>>,
    deadline: Rc<Cell<Option<Instant>>>,
}

/// Builds a plugin environment: `function(name, dir, allowed) -> env`.
const SANDBOX: &str = r#"
local real_require, real_loadstring, setfenv, pairs, ipairs = require, loadstring, setfenv, pairs, ipairs
local function copy(t) local r = {} for k, v in pairs(t) do r[k] = v end return r end
return function(name, dir, allowed)
  local allow = {}
  for _, p in ipairs(allowed or {}) do allow[p] = true end
  local env = {}
  for _, k in ipairs({ "assert", "error", "ipairs", "next", "pairs", "pcall", "select", "tonumber",
      "tostring", "type", "unpack", "xpcall", "rawequal", "rawget", "rawset", "setmetatable",
      "getmetatable", "print", "_VERSION", "collectgarbage" }) do env[k] = _G[k] end
  env.string, env.table, env.math, env.coroutine = copy(string), copy(table), copy(math), copy(coroutine)
  if bit then env.bit = copy(bit) end
  env.os = allow.os and os or { time = os.time, clock = os.clock, date = os.date, difftime = os.difftime, getenv = os.getenv }
  env.io = allow.io and io or { open = io.open, lines = io.lines, type = io.type, write = io.write, read = io.read }
  env.graphyne = graphyne
  env.loadstring = function(s, chunkname)
    if type(s) ~= "string" then return nil, "only source strings can be loaded" end
    if s:byte(1) == 27 then return nil, "bytecode is not allowed" end
    local fn, err = real_loadstring(s, chunkname)
    if fn then setfenv(fn, env) end
    return fn, err
  end
  env.load = function(s, chunkname) return env.loadstring(s, chunkname) end
  local loaded = {}
  env.require = function(mod)
    if mod == "ffi" then
      if allow.ffi then return real_require("ffi") end
      error("plugin '" .. name .. "' may not use ffi (allow it with graphyne.plugins.allow in init.lua)", 2)
    end
    if loaded[mod] ~= nil then return loaded[mod] end
    local file = dir .. "/" .. mod:gsub("%.", "/") .. ".lua"
    local f = io.open(file, "r")
    if not f then error("module '" .. mod .. "' not found in the folder of plugin '" .. name .. "'", 2) end
    local src = f:read("*a")
    f:close()
    local chunk, err = env.loadstring(src, "@" .. file)
    if not chunk then error(err, 2) end
    loaded[mod] = chunk() or true
    return loaded[mod]
  end
  env._G = env
  return env
end
"#;

fn to_value_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.to_string_lossy(),
        Value::Nil => "nil".into(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        other => format!("{other:?}"),
    }
}

impl Vm {
    fn load(dir: &Path, out: &flume::Sender<FromLua>) -> Vm {
        let _ = out.send(FromLua::Reset);
        // SAFETY: the full library set (with `ffi`) is for the user's own init.lua; plugins
        // only see the sandbox environment built by SANDBOX.
        let lua = unsafe { Lua::unsafe_new_with(StdLib::ALL, LuaOptions::default()) };
        let reg = Rc::new(RefCell::new(Registry::default()));
        let deadline: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
        let d = deadline.clone();
        let _ = lua.set_hook(HookTriggers::new().every_nth_instruction(10_000), move |_, _| {
            if d.get().is_some_and(|t| Instant::now() > t) {
                d.set(None);
                Err(mlua::Error::runtime(format!("stopped: ran longer than {} s", CALLBACK_BUDGET.as_secs())))
            } else {
                Ok(VmState::Continue)
            }
        });
        // JIT-compiled loops never reach the count hook, so a `while true do end` could not
        // be stopped. The LuaJIT interpreter honours it; init.lua may call jit.on() to opt in.
        let _ = lua.load("jit.off()").exec();
        let vm = Vm { lua, reg, deadline };
        let mut files = Vec::new();
        let mut errors = Vec::new();
        if let Err(e) = vm.install_api(out.clone()) {
            errors.push(format!("Graphyne Lua API: {e}"));
        }
        let init = dir.join("init.lua");
        if init.is_file() {
            match vm.run_file(&init, None) {
                Ok(()) => files.push("init.lua".to_string()),
                Err(e) => errors.push(format!("init.lua: {e}")),
            }
        }
        for (name, file, folder) in discover_plugins(&dir.join("plugins")) {
            let allowed: Vec<String> = vm.reg.borrow().allow.get(&name).map(|s| s.iter().cloned().collect()).unwrap_or_default();
            let env = vm
                .lua
                .named_registry_value::<Function>("graphyne.sandbox")
                .and_then(|make| make.call::<Table>((name.clone(), folder.to_string_lossy().replace('\\', "/"), allowed)));
            let result = env.and_then(|env| vm.run_file(&file, Some(env)).map_err(mlua::Error::runtime));
            match result {
                Ok(()) => files.push(format!("plugins/{name}")),
                Err(e) => errors.push(format!("plugin {name}: {e}")),
            }
        }
        for e in &errors {
            let _ = out.send(FromLua::Notify { message: format!("Lua: {e}"), error: true });
        }
        let _ = out.send(FromLua::Loaded { files, errors });
        vm
    }

    fn run_file(&self, file: &Path, env: Option<Table>) -> Result<(), String> {
        let src = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
        let mut chunk = self.lua.load(src).set_name(format!("@{}", file.display())).set_mode(ChunkMode::Text);
        if let Some(env) = env {
            chunk = chunk.set_environment(env);
        }
        self.deadline.set(Some(Instant::now() + CALLBACK_BUDGET));
        let result = chunk.exec();
        self.deadline.set(None);
        result.map_err(|e| e.to_string())
    }

    fn call(&self, f: &Function, out: &flume::Sender<FromLua>) {
        self.deadline.set(Some(Instant::now() + CALLBACK_BUDGET));
        let result = f.call::<()>(());
        self.deadline.set(None);
        if let Err(e) = result {
            let _ = out.send(FromLua::Notify { message: format!("Lua: {e}"), error: true });
        }
    }

    fn handle(&self, msg: ToLua, out: &flume::Sender<FromLua>) {
        match msg {
            ToLua::Key { id, ctx } => {
                self.reg.borrow_mut().ctx = ctx;
                let f = self.reg.borrow().keys.get(&id).cloned();
                if let Some(f) = f {
                    self.call(&f, out);
                }
            }
            ToLua::TabRefresh { tab } => self.refresh_tab(tab, out),
            ToLua::TabClick { tab, item, ctx } => {
                self.reg.borrow_mut().ctx = ctx;
                let f = self.reg.borrow().tab_clicks.get(&(tab, item)).cloned();
                if let Some(f) = f {
                    self.call(&f, out);
                }
            }
            ToLua::Reload | ToLua::Shutdown => {}
        }
        let pending = std::mem::take(&mut self.reg.borrow_mut().refresh);
        for tab in pending {
            self.refresh_tab(tab, out);
        }
    }

    fn refresh_tab(&self, tab: u64, out: &flume::Sender<FromLua>) {
        let Some(items_fn) = self.reg.borrow().tabs.get(&tab).cloned() else { return };
        self.deadline.set(Some(Instant::now() + CALLBACK_BUDGET));
        let result = items_fn.call::<Table>(());
        self.deadline.set(None);
        let list = match result {
            Ok(t) => t,
            Err(e) => {
                let _ = out.send(FromLua::Notify { message: format!("Lua tab: {e}"), error: true });
                return;
            }
        };
        let mut items = Vec::new();
        let mut clicks = Vec::new();
        for (i, v) in list.sequence_values::<Value>().flatten().enumerate() {
            match v {
                Value::Table(t) => {
                    let text = t.get::<Option<String>>("text").ok().flatten().unwrap_or_default();
                    let detail = t.get::<Option<String>>("detail").ok().flatten();
                    let click = t.get::<Option<Function>>("on_click").ok().flatten();
                    items.push(TabItem { text, detail, clickable: click.is_some() });
                    if let Some(f) = click {
                        clicks.push((i, f));
                    }
                }
                other => items.push(TabItem { text: to_value_string(&other), detail: None, clickable: false }),
            }
        }
        {
            let mut reg = self.reg.borrow_mut();
            reg.tab_clicks.retain(|(t, _), _| *t != tab);
            for (i, f) in clicks {
                reg.tab_clicks.insert((tab, i), f);
            }
        }
        let _ = out.send(FromLua::TabItems { tab, items });
    }

    /// The `graphyne` global.
    fn install_api(&self, out: flume::Sender<FromLua>) -> mlua::Result<()> {
        let lua = &self.lua;
        let g = lua.create_table()?;
        g.set("version", env!("CARGO_PKG_VERSION"))?;

        let notify = {
            let out = out.clone();
            lua.create_function(move |_, (msg, level): (Value, Option<String>)| {
                let _ = out.send(FromLua::Notify { message: to_value_string(&msg), error: level.as_deref() == Some("error") });
                Ok(())
            })?
        };
        g.set("notify", notify.clone())?;

        // ui
        let ui = lua.create_table()?;
        ui.set("notify", notify)?;
        {
            let (out, reg) = (out.clone(), self.reg.clone());
            ui.set(
                "register_tab",
                lua.create_function(move |_, spec: Table| {
                    let title: String = spec.get("title")?;
                    let side = match spec.get::<Option<String>>("side")?.as_deref() {
                        Some("right") => Side::Right,
                        _ => Side::Left,
                    };
                    let items: Function = spec.get("items")?;
                    // Same title + side ⇒ same id across hot reloads, so an open tab stays open.
                    let id = {
                        use std::hash::{Hash, Hasher};
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        (title.as_str(), side == Side::Left).hash(&mut h);
                        // LuaJIT numbers are doubles: stay below 2^53, above the sequential ids.
                        (h.finish() & ((1 << 52) - 1)) | (1 << 52)
                    };
                    reg.borrow_mut().tabs.insert(id, items);
                    let _ = out.send(FromLua::RegisterTab { id, title, side });
                    Ok(id)
                })?,
            )?;
        }
        {
            let reg = self.reg.clone();
            ui.set(
                "refresh_tab",
                lua.create_function(move |_, id: u64| {
                    reg.borrow_mut().refresh.push(id);
                    Ok(())
                })?,
            )?;
        }
        g.set("ui", ui)?;

        // keymap
        let keymap = lua.create_table()?;
        {
            let (out, reg) = (out.clone(), self.reg.clone());
            keymap.set(
                "set",
                lua.create_function(move |_, (mode, keys, f): (String, String, Function)| {
                    let context = match mode.as_str() {
                        "global" | "g" | "workspace" => KeyContext::Global,
                        _ => KeyContext::Editor,
                    };
                    let id = next_id();
                    reg.borrow_mut().keys.insert(id, f);
                    let _ = out.send(FromLua::Keymap { id, keys, context });
                    Ok(id)
                })?,
            )?;
        }
        g.set("keymap", keymap)?;

        // terminal
        let terminal = lua.create_table()?;
        {
            let out = out.clone();
            terminal.set(
                "run",
                lua.create_function(move |_, (command, opts): (String, Option<Table>)| {
                    let tab = opts.and_then(|o| o.get::<Option<String>>("tab").ok().flatten());
                    let _ = out.send(FromLua::Terminal { command, tab });
                    Ok(())
                })?,
            )?;
        }
        g.set("terminal", terminal)?;

        // buffer: reads the snapshot sent with the current callback, edits go to the IDE
        let buffer = lua.create_table()?;
        macro_rules! getter {
            ($name:literal, $ty:ty, |$ctx:ident| $body:expr) => {{
                let reg = self.reg.clone();
                buffer.set(
                    $name,
                    lua.create_function(move |_, ()| -> mlua::Result<$ty> {
                        let $ctx = &reg.borrow().ctx;
                        Ok($body)
                    })?,
                )?;
            }};
        }
        getter!("get_text", Option<String>, |c| c.text.clone());
        getter!("get_path", Option<String>, |c| c.path.clone());
        getter!("get_language", Option<String>, |c| c.language.clone());
        getter!("get_selection", String, |c| c.selection.clone());
        getter!("get_cursor", (usize, usize), |c| (c.line, c.column));
        {
            let reg = self.reg.clone();
            buffer.set(
                "node_at_cursor",
                lua.create_function(move |lua, ()| {
                    let reg = reg.borrow();
                    match &reg.ctx.node_kind {
                        Some(kind) => {
                            let t = lua.create_table()?;
                            t.set("kind", kind.clone())?;
                            t.set("text", reg.ctx.node_text.clone().unwrap_or_default())?;
                            Ok(Value::Table(t))
                        }
                        None => Ok(Value::Nil),
                    }
                })?,
            )?;
        }
        for (name, make) in [
            ("insert", BufferEdit::Insert as fn(String) -> BufferEdit),
            ("replace_selection", BufferEdit::ReplaceSelection),
            ("set_text", BufferEdit::SetText),
        ] {
            let out = out.clone();
            buffer.set(
                name,
                lua.create_function(move |_, text: String| {
                    let _ = out.send(FromLua::Edit(make(text)));
                    Ok(())
                })?,
            )?;
        }
        g.set("buffer", buffer)?;

        // workspace
        let workspace = lua.create_table()?;
        {
            let reg = self.reg.clone();
            workspace.set("root", lua.create_function(move |_, ()| Ok(reg.borrow().ctx.project_root.clone()))?)?;
        }
        {
            let out = out.clone();
            workspace.set(
                "open",
                lua.create_function(move |_, (path, line): (String, Option<usize>)| {
                    let _ = out.send(FromLua::OpenFile { path, line });
                    Ok(())
                })?,
            )?;
        }
        g.set("workspace", workspace)?;

        // settings
        let settings = lua.create_table()?;
        {
            let out = out.clone();
            settings.set(
                "set",
                lua.create_function(move |_, (key, value): (String, Value)| {
                    let Some((_, min, max)) = SETTINGS.iter().find(|(k, _, _)| *k == key) else {
                        let known: Vec<&str> = SETTINGS.iter().map(|(k, _, _)| *k).collect();
                        return Err(mlua::Error::runtime(format!("unknown setting '{key}' (known: {})", known.join(", "))));
                    };
                    let value = match value {
                        Value::Integer(i) => SettingValue::Number((i as f64).clamp(*min, *max)),
                        Value::Number(n) => SettingValue::Number(n.clamp(*min, *max)),
                        Value::Boolean(b) => SettingValue::Bool(b),
                        Value::String(s) => SettingValue::Text(s.to_string_lossy()),
                        other => return Err(mlua::Error::runtime(format!("bad value for {key}: {other:?}"))),
                    };
                    let _ = out.send(FromLua::Setting { key, value });
                    Ok(())
                })?,
            )?;
        }
        g.set("settings", settings)?;

        // plugins.allow (meaningful in init.lua, before plugins load)
        let plugins = lua.create_table()?;
        {
            let reg = self.reg.clone();
            plugins.set(
                "allow",
                lua.create_function(move |_, (name, perms): (String, Vec<String>)| {
                    reg.borrow_mut().allow.entry(name).or_default().extend(perms);
                    Ok(())
                })?,
            )?;
        }
        g.set("plugins", plugins)?;

        lua.globals().set("graphyne", g)?;

        // print → notification
        {
            let out = out.clone();
            lua.globals().set(
                "print",
                lua.create_function(move |_, args: mlua::Variadic<Value>| {
                    let text: Vec<String> = args.iter().map(to_value_string).collect();
                    let _ = out.send(FromLua::Notify { message: text.join("\t"), error: false });
                    Ok(())
                })?,
            )?;
        }

        let make_env: Function = lua.load(SANDBOX).set_name("=graphyne.sandbox").eval()?;
        lua.set_named_registry_value("graphyne.sandbox", make_env)?;
        Ok(())
    }
}

/// `plugins/name.lua` and `plugins/name/init.lua`, sorted by name: (name, file, folder).
fn discover_plugins(dir: &Path) -> Vec<(String, PathBuf, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(String, PathBuf, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.is_dir() {
                let init = path.join("init.lua");
                init.is_file().then(|| (e.file_name().to_string_lossy().to_string(), init, path))
            } else if path.extension().is_some_and(|x| x == "lua") {
                let name = path.file_stem()?.to_string_lossy().to_string();
                Some((name, path.clone(), dir.to_path_buf()))
            } else {
                None
            }
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config(init: &str, plugins: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("graphyne-lua-{}-{}", std::process::id(), next_id()));
        std::fs::create_dir_all(dir.join("plugins")).unwrap();
        std::fs::write(dir.join("init.lua"), init).unwrap();
        for (name, src) in plugins {
            std::fs::write(dir.join("plugins").join(format!("{name}.lua")), src).unwrap();
        }
        dir
    }

    /// Drive the VM synchronously on this thread.
    fn load(dir: &Path) -> (Vm, Vec<FromLua>, flume::Receiver<FromLua>, flume::Sender<FromLua>) {
        let (tx, rx) = flume::unbounded();
        let vm = Vm::load(dir, &tx);
        let events: Vec<FromLua> = rx.try_iter().collect();
        (vm, events, rx, tx)
    }

    #[test]
    fn luajit_runs() {
        let lua = Lua::new();
        let v: String = lua.load("return jit.version").eval().unwrap();
        assert!(v.starts_with("LuaJIT"), "{v}");
    }

    #[test]
    fn init_registers_keys_settings_and_edits_buffer() {
        let dir = temp_config(
            r#"
            graphyne.settings.set("editor.font_size", 40)
            graphyne.keymap.set("editor", "ctrl-alt-u", function()
              local line, col = graphyne.buffer.get_cursor()
              graphyne.buffer.insert(graphyne.buffer.get_text():upper() .. " " .. line .. ":" .. col)
            end)
            graphyne.ui.notify("hello from init")
            "#,
            &[],
        );
        let (vm, events, rx, tx) = load(&dir);
        assert!(events.contains(&FromLua::Setting { key: "editor.font_size".into(), value: SettingValue::Number(32.) }), "clamped: {events:?}");
        assert!(events.contains(&FromLua::Notify { message: "hello from init".into(), error: false }));
        let id = events.iter().find_map(|e| if let FromLua::Keymap { id, keys, context: KeyContext::Editor } = e { (keys == "ctrl-alt-u").then_some(*id) } else { None }).unwrap();
        let ctx = EditorContext { text: Some("abc".into()), line: 3, column: 7, ..Default::default() };
        vm.handle(ToLua::Key { id, ctx }, &tx);
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![FromLua::Edit(BufferEdit::Insert("ABC 3:7".into()))]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn plugins_are_sandboxed_unless_allowed() {
        let probe = r#"
            local out = {}
            out[#out + 1] = "execute=" .. tostring(os.execute ~= nil)
            out[#out + 1] = "popen=" .. tostring(io.popen ~= nil)
            out[#out + 1] = "ffi=" .. tostring(pcall(require, "ffi"))
            out[#out + 1] = "escape=" .. tostring(loadstring("return os.execute")() ~= nil)
            out[#out + 1] = "debug=" .. tostring(debug ~= nil)
            graphyne.ui.notify(table.concat(out, " "))
        "#;
        let dir = temp_config(r#"graphyne.plugins.allow("trusted", { "os", "io", "ffi" })"#, &[("probe", probe), ("trusted", probe)]);
        let (_vm, events, _rx, _tx) = load(&dir);
        let notes: Vec<&str> = events.iter().filter_map(|e| if let FromLua::Notify { message, .. } = e { Some(message.as_str()) } else { None }).collect();
        assert_eq!(notes, ["execute=false popen=false ffi=false escape=false debug=false", "execute=true popen=true ffi=true escape=true debug=false"]);
        assert!(matches!(events.last(), Some(FromLua::Loaded { errors, .. }) if errors.is_empty()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tabs_errors_and_runaway_callbacks() {
        let dir = temp_config(
            r#"
            local count = 0
            local tab
            tab = graphyne.ui.register_tab({ title = "TODO", side = "right", items = function()
              return { "plain", { text = "clicks: " .. count, detail = "click me", on_click = function() count = count + 1 graphyne.ui.refresh_tab(tab) end } }
            end })
            graphyne.keymap.set("global", "ctrl-alt-l", function() while true do local _ = tostring(count) end end)
            "#,
            &[("broken", "this is not lua")],
        );
        let (vm, events, rx, tx) = load(&dir);
        let tab = events.iter().find_map(|e| if let FromLua::RegisterTab { id, side: Side::Right, .. } = e { Some(*id) } else { None }).unwrap();
        assert!(matches!(events.last(), Some(FromLua::Loaded { errors, .. }) if errors.len() == 1 && errors[0].starts_with("plugin broken")));
        vm.handle(ToLua::TabRefresh { tab }, &tx);
        vm.handle(ToLua::TabClick { tab, item: 1, ctx: EditorContext::default() }, &tx);
        let items: Vec<FromLua> = rx.try_iter().collect();
        assert!(matches!(&items[1], FromLua::TabItems { items, .. } if items[1].text == "clicks: 1" && items[1].clickable && !items[0].clickable));
        let key = events.iter().find_map(|e| if let FromLua::Keymap { id, context: KeyContext::Global, .. } = e { Some(*id) } else { None }).unwrap();
        let started = Instant::now();
        vm.handle(ToLua::Key { id: key, ctx: EditorContext::default() }, &tx);
        assert!(started.elapsed() < CALLBACK_BUDGET + Duration::from_secs(2));
        assert!(matches!(rx.try_iter().next(), Some(FromLua::Notify { message, error: true }) if message.contains("stopped")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
