//! Debugger state: breakpoints (saved per project in `.graphyne/`) and the active DAP session.
//! All adapter I/O runs on background executors; events are pumped onto the UI thread.

use gpui::{AppContext, Context, EventEmitter, SharedString, Task};
use graphyne_dap::model::{self, BreakpointSpec, Scope, StackFrame, Thread, Variable};
use graphyne_dap::{DapClient, DapEvent, Transport, adapters};
use graphyne_lsp::convert::path_key;
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Project-local debugger state, next to the sources (like Fleet's `.fleet/`).
const STATE_DIR: &str = ".graphyne";
const BREAKPOINTS_FILE: &str = "breakpoints.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BpKind {
    Plain,
    Conditional,
    Log,
}

/// Per-breakpoint options; both empty = a plain breakpoint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BpOpts {
    pub condition: Option<String>,
    pub log_message: Option<String>,
}

impl BpOpts {
    pub fn kind(&self) -> BpKind {
        let set = |v: &Option<String>| v.as_deref().is_some_and(|t| !t.trim().is_empty());
        if set(&self.log_message) {
            BpKind::Log
        } else if set(&self.condition) {
            BpKind::Conditional
        } else {
            BpKind::Plain
        }
    }
}

pub enum DebuggerEvent {
    /// Reveal an execution point (path, 0-based line).
    Reveal(PathBuf, usize),
    Notify(String, bool),
    Started,
    /// Run the debuggee in a terminal tab (title, cwd, argv, env).
    RunInTerminal(String, PathBuf, Vec<String>, Vec<(String, String)>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Starting(String),
    Running,
    Paused { reason: String, description: Option<String> },
    Exited(i64),
    Stopped,
    Failed(String),
}

impl Status {
    pub fn is_live(&self) -> bool {
        matches!(self, Status::Starting(_) | Status::Running | Status::Paused { .. })
    }
}

pub struct Session {
    pub client: Option<Arc<DapClient>>,
    pub title: SharedString,
    pub file: PathBuf,
    pub adapter: String,
    pub status: Status,
    pub thread_id: Option<i64>,
    pub threads: Vec<Thread>,
    pub frames: Vec<StackFrame>,
    pub selected_frame: usize,
    pub scopes: Vec<Scope>,
    pub children: HashMap<i64, Vec<Variable>>,
    pub expanded: HashSet<i64>,
    pub console: Vec<(SharedString, String)>,
    /// Counts stops: a stack asked for at an earlier stop is dropped when it arrives late
    /// (Dart stops at the isolate's start, then at the breakpoint, in quick succession).
    stops: u64,
    _tasks: Vec<Task<()>>,
}

/// One rendered row of the variables tree.
pub struct VarRow {
    pub depth: usize,
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    pub reference: i64,
    pub expanded: bool,
    pub is_scope: bool,
}

pub struct Debugger {
    root: PathBuf,
    breakpoints: BTreeMap<String, (PathBuf, BTreeMap<usize, BpOpts>)>,
    pub session: Option<Session>,
    save_task: Option<Task<()>>,
}

impl EventEmitter<DebuggerEvent> for Debugger {}

impl Debugger {
    pub fn new(root: PathBuf) -> Self {
        Self { root, breakpoints: BTreeMap::new(), session: None, save_task: None }
    }

    // ------------------------------------------------------------------ breakpoints

    /// Breakpoint lines (0-based) of `path` with their kind, ascending.
    pub fn breakpoints_for(&self, path: &Path) -> Vec<(usize, BpKind)> {
        self.breakpoints.get(&path_key(path)).map(|(_, m)| m.iter().map(|(l, o)| (*l, o.kind())).collect()).unwrap_or_default()
    }

    pub fn breakpoint(&self, path: &Path, line: usize) -> Option<BpOpts> {
        self.breakpoints.get(&path_key(path)).and_then(|(_, m)| m.get(&line).cloned())
    }

    fn specs_for(&self, path: &Path) -> Vec<BreakpointSpec> {
        self.breakpoints
            .get(&path_key(path))
            .map(|(_, m)| m.iter().map(|(l, o)| BreakpointSpec { line: *l, condition: o.condition.clone(), log_message: o.log_message.clone() }).collect())
            .unwrap_or_default()
    }

    pub fn toggle_breakpoint(&mut self, path: &Path, line: usize, cx: &mut Context<Self>) {
        let entry = self.breakpoints.entry(path_key(path)).or_insert_with(|| (path.to_path_buf(), BTreeMap::new()));
        if entry.1.remove(&line).is_none() {
            entry.1.insert(line, BpOpts::default());
        }
        self.changed(path, cx);
    }

    /// Create / update (`Some`) or remove (`None`) the breakpoint on `line`.
    pub fn set_breakpoint(&mut self, path: &Path, line: usize, opts: Option<BpOpts>, cx: &mut Context<Self>) {
        let entry = self.breakpoints.entry(path_key(path)).or_insert_with(|| (path.to_path_buf(), BTreeMap::new()));
        match opts {
            Some(o) => {
                entry.1.insert(line, o);
            }
            None => {
                entry.1.remove(&line);
            }
        }
        self.changed(path, cx);
    }

    fn changed(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.sync_breakpoints(path, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Keep breakpoints on their code lines when lines are inserted / removed above them.
    pub fn shift_breakpoints(&mut self, path: &Path, start_row: usize, old_end_row: usize, new_end_row: usize, cx: &mut Context<Self>) {
        let Some((_, map)) = self.breakpoints.get_mut(&path_key(path)) else { return };
        if map.is_empty() || (old_end_row == new_end_row && old_end_row == start_row) {
            return;
        }
        let delta = new_end_row as isize - old_end_row as isize;
        let shifted: BTreeMap<usize, BpOpts> = map
            .iter()
            .filter_map(|(&l, o)| {
                if l <= start_row {
                    Some((l, o.clone()))
                } else if l <= old_end_row {
                    None
                } else {
                    Some(((l as isize + delta).max(0) as usize, o.clone()))
                }
            })
            .collect();
        if shifted != *map {
            *map = shifted;
            self.schedule_save(cx);
            cx.notify();
        }
    }

    fn sync_breakpoints(&self, path: &Path, cx: &mut Context<Self>) {
        let Some(client) = self.live_client() else { return };
        let specs = self.specs_for(path);
        let req = client.request("setBreakpoints", json!({ "source": { "path": path }, "breakpoints": model::source_breakpoints(&specs) }));
        cx.background_spawn(async move {
            let _ = req.await;
        })
        .detach();
    }

    /// Restore breakpoints saved in `<project>/.graphyne/breakpoints.json` (background read).
    pub fn load(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move {
                    let text = std::fs::read_to_string(root.join(STATE_DIR).join(BREAKPOINTS_FILE)).ok()?;
                    // A byte-order mark (the file edited by hand) is not JSON.
                    let v: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
                    let list = v["breakpoints"].as_array()?;
                    Some(
                        list.iter()
                            .filter_map(|b| {
                                let saved = b["path"].as_str()?;
                                let p = Path::new(saved);
                                // Saved with `/`: rebuilt part by part so the path has the
                                // system's separators (netcoredbg doesn't match `App/Program.cs`).
                                let path = if p.is_absolute() {
                                    p.to_path_buf()
                                } else {
                                    saved.split(['/', '\\']).filter(|s| !s.is_empty()).fold(root.clone(), |acc, part| acc.join(part))
                                };
                                let line = (b["line"].as_u64()? as usize).checked_sub(1)?;
                                let field = |k: &str| b[k].as_str().map(str::to_string).filter(|t| !t.is_empty());
                                Some((path, line, BpOpts { condition: field("condition"), log_message: field("logMessage") }))
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .await;
            let Some(list) = loaded else { return };
            this.update(cx, |d, cx| {
                for (path, line, opts) in list {
                    let entry = d.breakpoints.entry(path_key(&path)).or_insert_with(|| (path.clone(), BTreeMap::new()));
                    entry.1.entry(line).or_insert(opts);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Debounced write of all breakpoints (paths relative to the project root). The state
    /// folder gets a `*` .gitignore: it is per-machine and never shows up in commits.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let mut list = Vec::new();
        for (path, lines) in self.breakpoints.values() {
            let shown = match path.strip_prefix(&root) {
                Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
                Err(_) => path.to_string_lossy().to_string(),
            };
            for (line, o) in lines {
                let mut v = json!({ "path": shown, "line": line + 1 });
                if let Some(c) = o.condition.as_deref().filter(|t| !t.is_empty()) {
                    v["condition"] = c.into();
                }
                if let Some(m) = o.log_message.as_deref().filter(|t| !t.is_empty()) {
                    v["logMessage"] = m.into();
                }
                list.push(v);
            }
        }
        self.save_task = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            cx.background_spawn(async move {
                let dir = root.join(STATE_DIR);
                let file = dir.join(BREAKPOINTS_FILE);
                if list.is_empty() && !file.exists() {
                    return;
                }
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    log::warn!("saving breakpoints: {e}");
                    return;
                }
                let ignore = dir.join(".gitignore");
                if !ignore.exists() {
                    let _ = std::fs::write(&ignore, "# Graphyne workspace state, local to this machine\n*\n");
                }
                let text = serde_json::to_string_pretty(&json!({ "version": 1, "breakpoints": list })).unwrap_or_default();
                if let Err(e) = graphyne_core::fs_util::atomic_write(&file, text.as_bytes()) {
                    log::warn!("saving breakpoints: {e:#}");
                }
            })
            .await;
        }));
    }

    // ------------------------------------------------------------------ queries for views

    fn live_client(&self) -> Option<Arc<DapClient>> {
        self.session.as_ref().filter(|s| s.status.is_live()).and_then(|s| s.client.clone())
    }

    pub fn is_live(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.status.is_live())
    }

    /// Current execution line in `path` (selected frame), if paused there.
    pub fn execution_line(&self, path: &Path) -> Option<usize> {
        let s = self.session.as_ref()?;
        if !matches!(s.status, Status::Paused { .. }) {
            return None;
        }
        let f = s.frames.get(s.selected_frame)?;
        (f.path.as_deref().map(path_key) == Some(path_key(path))).then_some(f.line)
    }

    /// Locals of the selected frame (first scope) for inline values.
    pub fn inline_locals(&self) -> Vec<(String, String)> {
        let Some(s) = self.session.as_ref().filter(|s| matches!(s.status, Status::Paused { .. })) else { return Vec::new() };
        let Some(scope) = s.scopes.first() else { return Vec::new() };
        s.children.get(&scope.reference).map(|v| v.iter().map(|v| (v.name.clone(), v.value.clone())).collect()).unwrap_or_default()
    }

    pub fn variable_rows(&self) -> Vec<VarRow> {
        let mut rows = Vec::new();
        let Some(s) = &self.session else { return rows };
        fn walk(s: &Session, reference: i64, depth: usize, rows: &mut Vec<VarRow>) {
            let Some(vars) = s.children.get(&reference) else { return };
            for v in vars.iter().take(300) {
                let expanded = v.reference > 0 && s.expanded.contains(&v.reference);
                rows.push(VarRow { depth, name: v.name.clone(), value: v.value.clone(), type_name: v.type_name.clone(), reference: v.reference, expanded, is_scope: false });
                if expanded && depth < 8 {
                    walk(s, v.reference, depth + 1, rows);
                }
            }
        }
        for scope in &s.scopes {
            let expanded = s.expanded.contains(&scope.reference);
            rows.push(VarRow { depth: 0, name: scope.name.clone(), value: String::new(), type_name: None, reference: scope.reference, expanded, is_scope: true });
            if expanded {
                walk(s, scope.reference, 1, &mut rows);
            }
        }
        rows
    }

    // ------------------------------------------------------------------ session lifecycle

    pub fn start(&mut self, file: PathBuf, cx: &mut Context<Self>) {
        if let Some(client) = self.live_client() {
            client.kill();
        }
        let title: SharedString = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default().into();
        self.session = Some(Session {
            client: None,
            title,
            file: file.clone(),
            adapter: String::new(),
            status: Status::Starting("Resolving debug adapter…".into()),
            thread_id: None,
            threads: Vec::new(),
            frames: Vec::new(),
            selected_frame: 0,
            stops: 0,
            scopes: Vec::new(),
            children: HashMap::new(),
            expanded: HashSet::new(),
            console: Vec::new(),
            _tasks: Vec::new(),
        });
        cx.emit(DebuggerEvent::Started);
        cx.notify();
        let root = self.root.clone();
        // Slow steps of resolving (cargo build, downloading an adapter) in the panel's status.
        let (tx, rx) = flume::unbounded::<String>();
        let progress = cx.spawn(async move |this, cx| {
            while let Ok(text) = rx.recv_async().await {
                let alive = this.update(cx, |d, cx| {
                    if let Some(s) = d.session.as_mut().filter(|s| matches!(s.status, Status::Starting(_))) {
                        s.status = Status::Starting(text);
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let task = cx.spawn(async move |this, cx| {
            let resolved = cx.background_spawn(async move { adapters::resolve_with_progress(&file, &root, &|text| drop(tx.send(text.to_string()))) }).await;
            let r = match resolved {
                Ok(r) => r,
                Err(err) => {
                    this.update(cx, |d, cx| d.fail(err, cx)).ok();
                    return;
                }
            };
            // The program serves DAP itself (Ruby, a JVM): it runs in a terminal tab first.
            if matches!(r.transport, Transport::Terminal { .. }) {
                let args: Vec<String> = std::iter::once(r.program.to_string_lossy().to_string()).chain(r.args.iter().cloned()).collect();
                let (cwd, env) = (r.cwd.clone(), r.env.clone());
                if this.update(cx, |d, cx| d.open_program_terminal(cwd, args, env, cx)).is_err() {
                    return;
                }
            }
            let started = cx
                .background_spawn(async move {
                    let client = DapClient::spawn(r.name, &r.program, &r.args, &r.env, &r.cwd, r.transport.clone()).map_err(|e| format!("{e:#}"))?;
                    client.clone().initialize(r.adapter_id).await.map_err(|e| format!("initialize: {e:#}"))?;
                    Ok::<_, String>((client, r))
                })
                .await;
            let (client, resolved) = match started {
                Ok(v) => v,
                Err(err) => {
                    this.update(cx, |d, cx| d.fail(err, cx)).ok();
                    return;
                }
            };
            let events = client.events();
            let launch = client.request("launch", resolved.launch.clone());
            let ok = this
                .update(cx, |d, cx| {
                    let Some(s) = d.session.as_mut() else { return false };
                    s.client = Some(client.clone());
                    s.adapter = resolved.name.to_string();
                    s.status = Status::Starting(format!("Launching with {}…", resolved.name));
                    let pump = cx.spawn(async move |this, cx| {
                        while let Ok(ev) = events.recv_async().await {
                            if this.update(cx, |d, cx| d.handle(ev, cx)).is_err() {
                                break;
                            }
                        }
                    });
                    s._tasks.push(pump);
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !ok {
                return;
            }
            if let Err(err) = launch.await {
                this.update(cx, |d, cx| d.fail(format!("{err:#}"), cx)).ok();
            }
        });
        if let Some(s) = self.session.as_mut() {
            s._tasks.push(progress);
            s._tasks.push(task);
        }
    }

    /// Run the debuggee in its own terminal tab, where it reads input.
    fn open_program_terminal(&mut self, cwd: PathBuf, args: Vec<String>, env: Vec<(String, String)>, cx: &mut Context<Self>) {
        let Some(s) = self.session.as_mut() else { return };
        let tab = format!("Debug: {}", s.title);
        s.console.push(("system".into(), format!("Program input / output: terminal tab \"{tab}\"\n")));
        cx.emit(DebuggerEvent::RunInTerminal(tab, cwd, args, env));
        cx.notify();
    }

    fn fail(&mut self, err: String, cx: &mut Context<Self>) {
        if let Some(s) = self.session.as_mut() {
            if s.status.is_live() {
                s.status = Status::Failed(err.clone());
                s.console.push(("error".into(), format!("{err}\n")));
            }
        }
        cx.emit(DebuggerEvent::Notify(format!("Debugger: {err}"), true));
        cx.notify();
    }

    fn handle(&mut self, ev: DapEvent, cx: &mut Context<Self>) {
        match ev {
            DapEvent::Initialized => self.configure(cx),
            DapEvent::Stopped { reason, thread_id, description, .. } => {
                if let Some(s) = self.session.as_mut() {
                    s.status = Status::Paused { reason, description };
                    s.thread_id = thread_id.or(s.thread_id);
                    s.stops += 1;
                }
                self.load_stack(cx);
            }
            DapEvent::Continued { .. } => self.mark_running(cx),
            DapEvent::Output { category, text } => {
                if category == "telemetry" {
                    return;
                }
                if let Some(s) = self.session.as_mut() {
                    // Streams arrive in chunks: continue the previous entry until it ends a line.
                    match s.console.last_mut() {
                        Some((cat, prev)) if cat.as_ref() == category && !prev.ends_with('\n') => prev.push_str(&text),
                        last => {
                            // A different stream starts on its own line (logpoint vs stdout).
                            if let Some((_, prev)) = last.filter(|(_, p)| !p.ends_with('\n')) {
                                prev.push('\n');
                            }
                            s.console.push((category.into(), text))
                        }
                    }
                    if s.console.len() > 5000 {
                        s.console.drain(..1000);
                    }
                }
            }
            DapEvent::Thread { reason, thread_id } => {
                if let Some(s) = self.session.as_mut() {
                    if reason == "started" && s.thread_id.is_none() {
                        s.thread_id = Some(thread_id);
                    }
                }
            }
            DapEvent::Exited { code } => {
                if let Some(s) = self.session.as_mut() {
                    // After Stop the status stays "Stopped"; the exit code still goes to the console.
                    if s.status.is_live() {
                        s.status = Status::Exited(code);
                    }
                    s.console.push(("system".into(), format!("\nProcess finished with exit code {code}\n")));
                }
            }
            DapEvent::Terminated => {
                if let Some(s) = self.session.as_mut() {
                    if s.status.is_live() {
                        s.status = Status::Exited(0);
                    }
                    if let Some(client) = s.client.clone() {
                        let req = client.request("disconnect", json!({ "terminateDebuggee": true }));
                        cx.background_spawn(async move {
                            let _ = req.await;
                            // Adapters serving over TCP (js-debug) keep running after the
                            // session: end the process once the last output has arrived.
                            std::thread::sleep(std::time::Duration::from_millis(500));
                            client.kill();
                        })
                        .detach();
                    }
                }
            }
            DapEvent::AdapterExited => {
                if let Some(s) = self.session.as_mut() {
                    if s.status.is_live() {
                        s.status = Status::Failed("debug adapter exited".into());
                    }
                }
            }
            DapEvent::Breakpoint { .. } => {}
            DapEvent::RunInTerminal { request_seq, title: _, cwd, args, env } => {
                if let Some(s) = self.session.as_mut() {
                    let tab = format!("Debug: {}", s.title);
                    let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| s.file.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone()));
                    s.console.push(("system".into(), format!("Program input / output: terminal tab \"{tab}\"\n")));
                    if let Some(client) = s.client.clone() {
                        client.respond(request_seq, "runInTerminal", true, json!({}));
                    }
                    cx.emit(DebuggerEvent::RunInTerminal(tab, cwd, args, env));
                }
            }
        }
        cx.notify();
    }

    /// `initialized` → breakpoints for every file, exception filters, `configurationDone`.
    fn configure(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.session.as_ref().and_then(|s| s.client.clone()) else { return };
        let files: Vec<(PathBuf, Vec<BreakpointSpec>)> = self.breakpoints.values().map(|(p, _)| (p.clone(), self.specs_for(p))).collect();
        let reqs: Vec<_> = files
            .into_iter()
            .map(|(p, specs)| client.request("setBreakpoints", json!({ "source": { "path": p }, "breakpoints": model::source_breakpoints(&specs) })))
            .collect();
        let exc = client.request("setExceptionBreakpoints", json!({ "filters": [] }));
        let done_client = client.clone();
        cx.background_spawn(async move {
            for r in reqs {
                let _ = r.await;
            }
            let _ = exc.await;
            let _ = done_client.request("configurationDone", json!({})).await;
        })
        .detach();
        if let Some(s) = self.session.as_mut() {
            s.status = Status::Running;
        }
    }

    fn mark_running(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = self.session.as_mut() {
            if s.status.is_live() {
                s.status = Status::Running;
            }
            s.frames.clear();
            s.scopes.clear();
            s.children.clear();
        }
        cx.notify();
    }

    fn load_stack(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.session.as_ref() else { return };
        let Some(client) = s.client.clone() else { return };
        let thread_id = s.thread_id;
        let stop = s.stops;
        cx.spawn(async move |this, cx| {
            let threads = client.request("threads", json!({})).await.map(|b| model::parse_threads(&b)).unwrap_or_default();
            let tid = thread_id.or_else(|| threads.first().map(|t| t.id)).unwrap_or(1);
            let frames = client.request("stackTrace", json!({ "threadId": tid, "startFrame": 0, "levels": 64 })).await.map(|b| model::parse_stack(&b)).unwrap_or_default();
            this.update(cx, |d, cx| {
                if d.session.as_ref().is_some_and(|s| s.stops != stop) {
                    return; // the program has stopped again since
                }
                if let Some(s) = d.session.as_mut() {
                    s.threads = threads;
                    s.thread_id = Some(tid);
                    s.frames = frames;
                    s.selected_frame = 0;
                }
                d.select_frame(0, cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn select_frame(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(s) = self.session.as_mut() else { return };
        let Some(frame) = s.frames.get(ix).cloned() else { return };
        let Some(client) = s.client.clone() else { return };
        s.selected_frame = ix;
        s.scopes.clear();
        s.children.clear();
        s.expanded.clear();
        if let Some(path) = frame.path.clone() {
            cx.emit(DebuggerEvent::Reveal(path, frame.line));
        }
        cx.spawn(async move |this, cx| {
            let scopes = client.request("scopes", json!({ "frameId": frame.id })).await.map(|b| model::parse_scopes(&b)).unwrap_or_default();
            let first = scopes.iter().find(|s| !s.expensive).map(|s| s.reference);
            let vars = match first {
                Some(r) => client.request("variables", json!({ "variablesReference": r })).await.map(|b| model::parse_variables(&b)).unwrap_or_default(),
                None => Vec::new(),
            };
            this.update(cx, |d, cx| {
                if let Some(s) = d.session.as_mut().filter(|s| s.selected_frame == ix) {
                    s.scopes = scopes;
                    if let Some(r) = first {
                        s.children.insert(r, vars);
                        s.expanded.insert(r);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn toggle_variable(&mut self, reference: i64, cx: &mut Context<Self>) {
        let Some(s) = self.session.as_mut() else { return };
        if reference <= 0 {
            return;
        }
        if !s.expanded.remove(&reference) {
            s.expanded.insert(reference);
            if !s.children.contains_key(&reference) {
                let Some(client) = s.client.clone() else { return };
                cx.spawn(async move |this, cx| {
                    let vars = client.request("variables", json!({ "variablesReference": reference })).await.map(|b| model::parse_variables(&b)).unwrap_or_default();
                    this.update(cx, |d, cx| {
                        if let Some(s) = d.session.as_mut() {
                            s.children.insert(reference, vars);
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
        cx.notify();
    }

    // ------------------------------------------------------------------ controls

    fn thread_command(&mut self, command: &str, cx: &mut Context<Self>) {
        let Some(s) = self.session.as_ref() else { return };
        if !matches!(s.status, Status::Paused { .. }) && command != "pause" {
            return;
        }
        let Some(client) = s.client.clone() else { return };
        let tid = s.thread_id.unwrap_or(1);
        let req = client.request(command, json!({ "threadId": tid }));
        cx.background_spawn(async move {
            let _ = req.await;
        })
        .detach();
        if command != "pause" {
            self.mark_running(cx);
        }
    }

    pub fn resume(&mut self, cx: &mut Context<Self>) {
        self.thread_command("continue", cx);
    }
    pub fn step_over(&mut self, cx: &mut Context<Self>) {
        self.thread_command("next", cx);
    }
    pub fn step_into(&mut self, cx: &mut Context<Self>) {
        self.thread_command("stepIn", cx);
    }
    pub fn step_out(&mut self, cx: &mut Context<Self>) {
        self.thread_command("stepOut", cx);
    }
    pub fn pause(&mut self, cx: &mut Context<Self>) {
        if matches!(self.session.as_ref().map(|s| &s.status), Some(Status::Running)) {
            self.thread_command("pause", cx);
        }
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.session.as_mut() else { return };
        if let Some(client) = s.client.clone() {
            let req = client.request("disconnect", json!({ "terminateDebuggee": true }));
            cx.background_spawn(async move {
                let _ = req.await;
                std::thread::sleep(std::time::Duration::from_millis(500));
                client.kill();
            })
            .detach();
        }
        if s.status.is_live() {
            s.status = Status::Stopped;
            s.console.push(("system".into(), "\nDebug session stopped\n".into()));
        }
        s.frames.clear();
        s.scopes.clear();
        cx.notify();
    }

    pub fn restart(&mut self, cx: &mut Context<Self>) {
        let Some(file) = self.session.as_ref().map(|s| s.file.clone()) else { return };
        self.stop(cx);
        self.start(file, cx);
    }

    pub fn evaluate(&mut self, expression: String, cx: &mut Context<Self>) {
        let Some(s) = self.session.as_mut() else { return };
        s.console.push(("input".into(), format!("> {expression}\n")));
        let Some(client) = s.client.clone().filter(|_| s.status.is_live()) else {
            s.console.push(("error".into(), "No live debug session\n".into()));
            cx.notify();
            return;
        };
        let frame_id = s.frames.get(s.selected_frame).map(|f| f.id);
        cx.spawn(async move |this, cx| {
            let result = client.request("evaluate", json!({ "expression": expression, "frameId": frame_id, "context": "repl" })).await;
            this.update(cx, |d, cx| {
                if let Some(s) = d.session.as_mut() {
                    match result {
                        Ok(b) => s.console.push(("result".into(), format!("{}\n", b["result"].as_str().unwrap_or_default()))),
                        Err(e) => s.console.push(("error".into(), format!("{e}\n"))),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}
