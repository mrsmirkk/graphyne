//! Java, Kotlin and Scala debugging: a DAP adapter over the JVM's own debug protocol (JDWP).
//! The bridge listens on a free port and asks the client to run the program's `java` command in
//! a terminal tab with `-agentlib:jdwp=…,server=n,suspend=y` added; the JVM connects back,
//! suspended before any of the program's code runs. Breakpoints are set as the program's
//! classes load (a class-prepare request), by the source file name the class records, so the
//! same code serves every JVM language.

use super::Dap;
use super::jdwp::{self, IdSizes, Location, Reader, Value, Writer, cmd, event};
use serde_json::{Value as Json, json};
use std::collections::{HashMap, VecDeque};
use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

enum Msg {
    Dap(Json),
    ClientGone,
    Connected(TcpStream),
    Packet(jdwp::Packet),
    EngineGone,
}

/// Packages whose code the user doesn't step into and Graphyne doesn't look up sources for.
const LIBRARY: [&str; 10] = ["java.*", "javax.*", "jdk.*", "sun.*", "com.sun.*", "kotlin.*", "kotlinx.*", "scala.*", "org.jetbrains.*", "dotty.*"];

fn library_class(signature: &str) -> bool {
    let name = signature.trim_start_matches('L').replace('/', ".");
    LIBRARY.iter().any(|p| name.starts_with(p.trim_end_matches('*')))
}

#[derive(Clone)]
struct Bp {
    line: i32,
    condition: Option<String>,
    log: Option<String>,
}

#[derive(Clone)]
struct Class {
    tag: u8,
    signature: String,
    path: Option<String>,
}

#[derive(Clone)]
struct Method {
    id: u64,
    name: String,
    signature: String,
}

#[derive(Clone)]
struct Local {
    code_index: u64,
    length: u64,
    name: String,
    signature: String,
    slot: i32,
}

#[derive(Clone)]
enum VarRef {
    Locals(usize),
    Children(Value),
}

/// A value the expression evaluator computed.
#[derive(Clone)]
enum Eval {
    Jvm(Value),
    Text(String),
}

struct Session {
    dap: Dap,
    tx: flume::Sender<Msg>,
    rx: flume::Receiver<Msg>,
    deferred: VecDeque<Msg>,
    vm: Option<TcpStream>,
    ids: IdSizes,
    next_id: u32,
    ready: bool,
    configured: bool,
    started: bool,
    ended: bool,
    /// While a method runs for a value's text: events that suspend are let go at once.
    invoking: bool,
    /// A method ran on the stopped thread since its frames were read: their ids are no longer
    /// valid (JDWP resumes the thread for the call).
    frames_stale: bool,
    /// Breakpoints by file (the path as the client sent it, compared lowercased).
    files: HashMap<String, Vec<Bp>>,
    /// Breakpoint requests per file, and what each is for.
    requests: HashMap<String, Vec<i32>>,
    request_bp: HashMap<i32, (String, i32)>,
    classes: HashMap<u64, Class>,
    methods: HashMap<u64, Vec<Method>>,
    lines: HashMap<(u64, u64), Vec<(u64, i32)>>,
    locals: HashMap<(u64, u64), Option<Vec<Local>>>,
    superclass: HashMap<u64, u64>,
    step: Option<i32>,
    /// Steps taken on their own past code without a line number (a stop there means nothing).
    extra_steps: u32,
    thread: Option<u64>,
    frames: Vec<(u64, u64, Location)>,
    refs: Vec<VarRef>,
    roots: Vec<PathBuf>,
    project: Option<PathBuf>,
    found: HashMap<String, Option<String>>,
}

pub fn serve() -> Result<(), String> {
    let (tx, rx) = flume::unbounded::<Msg>();
    let to_main = tx.clone();
    let dap = Dap::stdio(move |m| drop(to_main.send(m.map(Msg::Dap).unwrap_or(Msg::ClientGone))));
    let mut s = Session {
        dap,
        tx,
        rx,
        deferred: VecDeque::new(),
        vm: None,
        ids: IdSizes::default(),
        next_id: 0,
        ready: false,
        configured: false,
        started: false,
        ended: false,
        invoking: false,
        frames_stale: false,
        files: HashMap::new(),
        requests: HashMap::new(),
        request_bp: HashMap::new(),
        classes: HashMap::new(),
        methods: HashMap::new(),
        lines: HashMap::new(),
        locals: HashMap::new(),
        superclass: HashMap::new(),
        step: None,
        extra_steps: 0,
        thread: None,
        frames: Vec::new(),
        refs: Vec::new(),
        roots: Vec::new(),
        project: None,
        found: HashMap::new(),
    };
    s.run();
    Ok(())
}

fn key(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

impl Session {
    fn run(&mut self) {
        loop {
            let msg = match self.deferred.pop_front() {
                Some(m) => m,
                None => match self.rx.recv() {
                    Ok(m) => m,
                    Err(_) => return,
                },
            };
            match msg {
                Msg::Dap(req) => {
                    if !self.handle(req) {
                        return;
                    }
                }
                Msg::ClientGone => {
                    self.kill();
                    return;
                }
                Msg::Connected(stream) => self.connected(stream),
                Msg::Packet(p) if !p.reply && p.command == cmd::EVENT_COMPOSITE => self.events(&p),
                Msg::Packet(_) => {}
                Msg::EngineGone => self.finished(),
            }
        }
    }

    // ------------------------------------------------------------ talking to the JVM

    fn send(&mut self, command: (u8, u8), data: &[u8]) -> Result<u32, String> {
        self.next_id += 1;
        let id = self.next_id;
        let vm = self.vm.as_mut().ok_or("the JVM isn't connected")?;
        jdwp::write_command(vm, id, command, data).map_err(|e| format!("JDWP: {e}"))?;
        Ok(id)
    }

    /// A command and its reply: the error code (0 = fine) and the data.
    fn call_raw(&mut self, command: (u8, u8), data: Vec<u8>) -> Result<(u16, Vec<u8>), String> {
        let id = self.send(command, &data)?;
        loop {
            match self.rx.recv_timeout(Duration::from_secs(30)) {
                Ok(Msg::Packet(p)) if p.reply && p.id == id => return Ok((p.error, p.data)),
                Ok(Msg::Packet(p)) if !p.reply && self.invoking => self.release(&p),
                Ok(Msg::EngineGone) => {
                    self.deferred.push_back(Msg::EngineGone);
                    return Err("the program has ended".into());
                }
                Ok(other) => self.deferred.push_back(other),
                Err(_) => return Err("the JVM didn't answer".into()),
            }
        }
    }

    fn call(&mut self, command: (u8, u8), data: Vec<u8>) -> Result<Vec<u8>, String> {
        match self.call_raw(command, data)? {
            (0, data) => Ok(data),
            (code, _) => Err(format!("JDWP error {code}")),
        }
    }

    fn w(&self) -> Writer {
        Writer::new(self.ids)
    }

    /// Events that arrive while a method runs for a value: a thread they suspend goes on.
    fn release(&mut self, p: &jdwp::Packet) {
        let mut r = Reader::new(&p.data, self.ids);
        let Ok(policy) = r.u8() else { return };
        if policy == 0 {
            return;
        }
        let _count = r.i32();
        let Ok(kind) = r.u8() else { return };
        let _request = r.i32();
        if policy == 2 || kind == event::VM_DEATH {
            let _ = self.send(cmd::VM_RESUME, &[]);
        } else if let Ok(thread) = r.object() {
            let data = self.w().object(thread).done();
            let _ = self.send(cmd::TR_RESUME, &data);
        }
    }

    fn connected(&mut self, stream: TcpStream) {
        self.vm = Some(stream);
        match self.call(cmd::VM_ID_SIZES, Vec::new()) {
            Ok(data) => {
                let mut r = Reader::new(&data, self.ids);
                let mut size = || r.i32().map(|v| v.max(1) as usize).unwrap_or(8);
                self.ids = IdSizes { field: size(), method: size(), object: size(), reftype: size(), frame: size() };
            }
            Err(e) => self.dap.output("stderr", &format!("{e}\n")),
        }
    }

    fn resume_vm(&mut self) {
        self.frames.clear();
        self.refs.clear();
        let _ = self.call(cmd::VM_RESUME, Vec::new());
    }

    fn start_if_ready(&mut self) {
        if !self.ready || !self.configured || self.started {
            return;
        }
        self.started = true;
        // Every class of the program (not the libraries'): its breakpoints are set before it runs.
        let mut w = self.w().u8(event::CLASS_PREPARE).u8(1).i32(LIBRARY.len() as i32);
        for pattern in LIBRARY {
            w = w.u8(6).string(pattern);
        }
        if let Err(e) = self.call(cmd::ER_SET, w.done()) {
            self.dap.output("stderr", &format!("can't watch classes load: {e}\n"));
        }
        self.resume_vm();
    }

    fn signature(&mut self, class: u64) -> String {
        if let Some(c) = self.classes.get(&class) {
            return c.signature.clone();
        }
        let data = self.w().reftype(class).done();
        let sig = self.call(cmd::RT_SIGNATURE, data).ok().and_then(|d| Reader::new(&d, self.ids).string().ok()).unwrap_or_default();
        self.classes.insert(class, Class { tag: 1, signature: sig.clone(), path: None });
        sig
    }

    fn methods(&mut self, class: u64) -> Vec<Method> {
        if let Some(m) = self.methods.get(&class) {
            return m.clone();
        }
        let data = self.w().reftype(class).done();
        let mut list = Vec::new();
        if let Ok(d) = self.call(cmd::RT_METHODS, data) {
            let mut r = Reader::new(&d, self.ids);
            let n = r.i32().unwrap_or(0);
            for _ in 0..n {
                let (Ok(id), Ok(name), Ok(signature), Ok(_)) = (r.method(), r.string(), r.string(), r.i32()) else { break };
                list.push(Method { id, name, signature });
            }
        }
        self.methods.insert(class, list.clone());
        list
    }

    fn line_table(&mut self, class: u64, method: u64) -> Vec<(u64, i32)> {
        if let Some(t) = self.lines.get(&(class, method)) {
            return t.clone();
        }
        let data = self.w().reftype(class).method(method).done();
        let mut table = Vec::new();
        if let Ok(d) = self.call(cmd::M_LINE_TABLE, data) {
            let mut r = Reader::new(&d, self.ids);
            let _ = (r.u64(), r.u64());
            let n = r.i32().unwrap_or(0);
            for _ in 0..n {
                let (Ok(index), Ok(line)) = (r.u64(), r.i32()) else { break };
                table.push((index, line));
            }
        }
        table.sort();
        self.lines.insert((class, method), table.clone());
        table
    }

    fn line_of(&mut self, loc: &Location) -> i32 {
        let table = self.line_table(loc.class, loc.method);
        table.iter().take_while(|(i, _)| *i <= loc.index).last().map(|(_, l)| *l).unwrap_or(0)
    }

    fn super_of(&mut self, class: u64) -> u64 {
        if let Some(s) = self.superclass.get(&class) {
            return *s;
        }
        let data = self.w().reftype(class).done();
        let sup = self.call(cmd::CT_SUPERCLASS, data).ok().and_then(|d| Reader::new(&d, self.ids).reftype().ok()).unwrap_or(0);
        self.superclass.insert(class, sup);
        sup
    }

    /// A method by name and signature (`None`: any with no arguments) in `class` or above:
    /// the class that declares it, and its id.
    fn find_method(&mut self, class: u64, name: &str, signature: Option<&str>) -> Option<(u64, Method)> {
        let mut c = class;
        for _ in 0..32 {
            if c == 0 {
                return None;
            }
            if let Some(m) = self.methods(c).into_iter().find(|m| m.name == name && signature.map_or(m.signature.starts_with("()"), |s| m.signature == s)) {
                return Some((c, m));
            }
            c = self.super_of(c);
        }
        None
    }

    fn type_of(&mut self, object: u64) -> Option<u64> {
        let data = self.w().object(object).done();
        let d = self.call(cmd::OR_REFERENCE_TYPE, data).ok()?;
        let mut r = Reader::new(&d, self.ids);
        let _ = r.u8();
        r.reftype().ok()
    }

    fn invoke(&mut self, object: u64, class: u64, method: u64, args: &[Value]) -> Result<Value, String> {
        let thread = self.thread.ok_or("the program isn't stopped")?;
        let mut w = self.w().object(object).object(thread).reftype(class).method(method).i32(args.len() as i32);
        for a in args {
            w = w.value(a);
        }
        let data = w.i32(1).done(); // INVOKE_SINGLE_THREADED
        self.invoking = true;
        let result = self.call(cmd::OR_INVOKE_METHOD, data);
        self.invoking = false;
        self.frames_stale = true;
        let d = result?;
        let mut r = Reader::new(&d, self.ids);
        let value = r.value()?;
        let exception = r.value()?;
        if exception.object_id().is_some() {
            return Err("the method threw an exception".into());
        }
        Ok(value)
    }

    fn string(&mut self, id: u64) -> String {
        let data = self.w().object(id).done();
        self.call(cmd::SR_VALUE, data).ok().and_then(|d| Reader::new(&d, self.ids).string().ok()).unwrap_or_default()
    }

    // ------------------------------------------------------------ sources and breakpoints

    /// The file a class was compiled from: the breakpoint files first, then the source roots
    /// and the project.
    fn source_path(&mut self, class: u64, signature: &str) -> Option<String> {
        if library_class(signature) {
            return None;
        }
        let data = self.w().reftype(class).done();
        let name = self.call(cmd::RT_SOURCE_FILE, data).ok().and_then(|d| Reader::new(&d, self.ids).string().ok())?;
        let inner = signature.trim_start_matches('L').trim_end_matches(';');
        let package = inner.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
        let cache_key = format!("{package}/{name}");
        if let Some(found) = self.found.get(&cache_key) {
            return found.clone();
        }
        let suffix = if package.is_empty() { name.to_lowercase() } else { key(&format!("{package}/{name}")) };
        let same_name = |p: &str| Path::new(p).file_name().is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(&name));
        let mut found = self.files.keys().find(|p| key(p).ends_with(&suffix)).or_else(|| self.files.keys().find(|p| same_name(p))).cloned();
        if found.is_none() {
            for root in &self.roots {
                for candidate in [root.join(&package).join(&name), root.join(&name)] {
                    if candidate.is_file() {
                        found = Some(candidate.to_string_lossy().to_string());
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
        }
        if found.is_none() {
            if let Some(project) = self.project.clone() {
                found = search(&project, &name, &suffix);
            }
        }
        self.found.insert(cache_key, found.clone());
        found
    }

    fn class_prepared(&mut self, tag: u8, class: u64, signature: String) {
        let path = self.source_path(class, &signature);
        self.classes.insert(class, Class { tag, signature, path: path.clone() });
        if let Some(path) = path {
            self.set_class_breakpoints(class, &path);
        }
    }

    fn file_for(&self, path: &str) -> Option<String> {
        let k = key(path);
        self.files.keys().find(|p| key(p) == k).cloned()
    }

    /// Breakpoint requests for the lines of `path`'s breakpoints that `class` has code on.
    fn set_class_breakpoints(&mut self, class: u64, path: &str) {
        let Some(file) = self.file_for(path) else { return };
        let bps = self.files.get(&file).cloned().unwrap_or_default();
        let tag = self.classes.get(&class).map(|c| c.tag).unwrap_or(1);
        for m in self.methods(class) {
            let table = self.line_table(class, m.id);
            for bp in &bps {
                let Some(index) = table.iter().filter(|(_, l)| *l == bp.line).map(|(i, _)| *i).min() else { continue };
                let loc = Location { tag, class, method: m.id, index };
                let data = self.w().u8(event::BREAKPOINT).u8(2).i32(1).u8(7).location(&loc).done();
                if let Ok(d) = self.call(cmd::ER_SET, data) {
                    if let Ok(id) = Reader::new(&d, self.ids).i32() {
                        self.requests.entry(file.clone()).or_default().push(id);
                        self.request_bp.insert(id, (file.clone(), bp.line));
                    }
                }
            }
        }
    }

    fn set_breakpoints(&mut self, req: &Json) -> Result<(), String> {
        let a = &req["arguments"];
        let path = a["source"]["path"].as_str().ok_or("setBreakpoints: no path")?.to_string();
        let file = self.file_for(&path).unwrap_or(path.clone());
        let mut bps = Vec::new();
        let mut answer = Vec::new();
        for b in a["breakpoints"].as_array().into_iter().flatten() {
            let line = b["line"].as_i64().unwrap_or(1) as i32;
            let text = |k: &str| b[k].as_str().filter(|t| !t.trim().is_empty()).map(str::to_string);
            bps.push(Bp { line, condition: text("condition"), log: text("logMessage") });
            answer.push(json!({ "verified": true, "line": line }));
        }
        self.files.insert(file.clone(), bps);
        if self.vm.is_some() && self.started {
            for id in self.requests.remove(&file).unwrap_or_default() {
                self.request_bp.remove(&id);
                let data = self.w().u8(event::BREAKPOINT).i32(id).done();
                let _ = self.call(cmd::ER_CLEAR, data);
            }
            let k = key(&file);
            let loaded: Vec<u64> = self.classes.iter().filter(|(_, c)| c.path.as_deref().map(key) == Some(k.clone())).map(|(id, _)| *id).collect();
            for class in loaded {
                self.set_class_breakpoints(class, &file);
            }
        }
        self.dap.respond(req, json!({ "breakpoints": answer }));
        Ok(())
    }

    // ------------------------------------------------------------ events

    fn events(&mut self, p: &jdwp::Packet) {
        let mut r = Reader::new(&p.data, self.ids);
        let Ok(policy) = r.u8() else { return };
        let n = r.i32().unwrap_or(0);
        let mut stop: Option<(u8, i32, u64, Location)> = None;
        let mut prepared: Vec<(u8, u64, String, u64)> = Vec::new();
        for _ in 0..n {
            let (Ok(kind), Ok(request)) = (r.u8(), r.i32()) else { break };
            match kind {
                event::VM_START => {
                    let _ = r.object();
                    self.ready = true;
                }
                event::CLASS_PREPARE => {
                    let (Ok(thread), Ok(tag), Ok(class), Ok(sig), Ok(_)) = (r.object(), r.u8(), r.reftype(), r.string(), r.i32()) else { break };
                    prepared.push((tag, class, sig, thread));
                }
                event::BREAKPOINT | event::SINGLE_STEP => {
                    let (Ok(thread), Ok(loc)) = (r.object(), r.location()) else { break };
                    if stop.is_none() || kind == event::BREAKPOINT {
                        stop = Some((kind, request, thread, loc));
                    }
                }
                event::EXCEPTION => {
                    let _ = (r.object(), r.location(), r.value(), r.location());
                }
                event::THREAD_START | event::THREAD_DEATH => {
                    let _ = r.object();
                }
                event::CLASS_UNLOAD => {
                    let _ = r.string();
                }
                event::VM_DEATH => {
                    self.finished();
                    return;
                }
                _ => break,
            }
        }
        for (tag, class, sig, _) in &prepared {
            self.class_prepared(*tag, *class, sig.clone());
        }
        if let Some((kind, request, thread, loc)) = stop {
            self.stopped(kind, request, thread, loc);
            return;
        }
        // Class loads suspend only their thread; the start suspends everything.
        if !prepared.is_empty() && policy == 1 {
            if let Some(&(_, _, _, thread)) = prepared.first() {
                let data = self.w().object(thread).done();
                let _ = self.call(cmd::TR_RESUME, data);
            }
        }
        self.start_if_ready();
    }

    fn stopped(&mut self, kind: u8, request: i32, thread: u64, loc: Location) {
        self.thread = Some(thread);
        self.refs.clear();
        self.frames = vec![(thread, 0, loc)];
        if let Some(step) = self.step.take() {
            let data = self.w().u8(event::SINGLE_STEP).i32(step).done();
            let _ = self.call(cmd::ER_CLEAR, data);
        }
        if kind == event::BREAKPOINT {
            let bp = self.request_bp.get(&request).cloned().and_then(|(file, line)| self.files.get(&file).and_then(|b| b.iter().find(|b| b.line == line).cloned()));
            if let Some(bp) = bp {
                if bp.condition.is_some() || bp.log.is_some() {
                    self.load_frames(thread);
                }
                if let Some(cond) = &bp.condition {
                    let go_on = match self.eval(cond, 0) {
                        Ok(v) => !self.truthy(&v),
                        Err(_) => false,
                    };
                    if go_on {
                        self.resume_vm();
                        return;
                    }
                }
                if let Some(message) = &bp.log {
                    let text = self.interpolate(message);
                    self.dap.output("console", &format!("{text}\n"));
                    self.resume_vm();
                    return;
                }
            }
        }
        // Code with no line (Kotlin's parameter checks at a function's start): step on to a line.
        if kind == event::SINGLE_STEP && self.extra_steps < 20 && self.line_of(&loc) == 0 {
            self.extra_steps += 1;
            // One instruction at a time: a line step would skip the line these belong to.
            if self.step_by(0, 1).is_ok() {
                return;
            }
        }
        self.extra_steps = 0;
        let reason = if kind == event::BREAKPOINT { "breakpoint" } else { "step" };
        self.dap.event("stopped", json!({ "reason": reason, "threadId": thread as i64, "allThreadsStopped": true }));
    }

    fn interpolate(&mut self, message: &str) -> String {
        let mut out = String::new();
        let mut rest = message;
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let Some(end) = rest[start..].find('}') else { break };
            let expr = &rest[start + 1..start + end];
            match self.eval(expr, 0) {
                Ok(v) => out.push_str(&self.display_eval(&v, false)),
                Err(e) => out.push_str(&format!("<{e}>")),
            }
            rest = &rest[start + end + 1..];
        }
        out.push_str(rest);
        out
    }

    fn finished(&mut self) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.vm = None;
        self.dap.event("terminated", json!({}));
    }

    fn kill(&mut self) {
        if self.vm.is_some() && !self.ended {
            let data = self.w().i32(1).done();
            let _ = self.send(cmd::VM_EXIT, &data);
        }
        if let Some(vm) = self.vm.take() {
            let _ = vm.shutdown(std::net::Shutdown::Both);
        }
    }

    // ------------------------------------------------------------ frames and values

    fn load_frames(&mut self, thread: u64) {
        self.frames_stale = false;
        let data = self.w().object(thread).i32(0).i32(-1).done();
        let Ok(d) = self.call(cmd::TR_FRAMES, data) else { return };
        let mut r = Reader::new(&d, self.ids);
        let n = r.i32().unwrap_or(0);
        self.frames.clear();
        for _ in 0..n {
            let (Ok(frame), Ok(loc)) = (r.frame(), r.location()) else { break };
            self.frames.push((thread, frame, loc));
        }
    }

    fn local_table(&mut self, class: u64, method: u64) -> Option<Vec<Local>> {
        if let Some(t) = self.locals.get(&(class, method)) {
            return t.clone();
        }
        let data = self.w().reftype(class).method(method).done();
        let table = match self.call_raw(cmd::M_VARIABLE_TABLE, data) {
            Ok((0, d)) => {
                let mut r = Reader::new(&d, self.ids);
                let _ = r.i32();
                let n = r.i32().unwrap_or(0);
                let mut list = Vec::new();
                for _ in 0..n {
                    let (Ok(code_index), Ok(name), Ok(signature), Ok(length), Ok(slot)) = (r.u64(), r.string(), r.string(), r.i32(), r.i32()) else { break };
                    list.push(Local { code_index, length: length.max(0) as u64, name, signature, slot });
                }
                Some(list)
            }
            _ => None,
        };
        self.locals.insert((class, method), table.clone());
        table
    }

    /// The frame's local variables in scope, with `this` first.
    fn frame_locals(&mut self, ix: usize) -> Result<Vec<(String, Value, String)>, String> {
        if self.frames_stale {
            if let Some(&(thread, _, _)) = self.frames.first() {
                self.load_frames(thread);
            }
        }
        let (thread, frame, loc) = *self.frames.get(ix).ok_or("no such frame")?;
        let mut out = Vec::new();
        let data = self.w().object(thread).frame(frame).done();
        if let Ok(d) = self.call(cmd::SF_THIS_OBJECT, data) {
            if let Ok(v) = Reader::new(&d, self.ids).value() {
                if v.object_id().is_some() {
                    out.push(("this".to_string(), v, String::new()));
                }
            }
        }
        let Some(table) = self.local_table(loc.class, loc.method) else {
            return Err("no variable information: compile with debug info (-g)".into());
        };
        let mut visible: Vec<Local> = Vec::new();
        for l in table.into_iter().filter(|l| l.code_index <= loc.index && loc.index < l.code_index + l.length) {
            // Compiler-made locals (`$i$a$…`, `$this$…`) aren't the program's.
            if l.name.starts_with('$') || l.name.is_empty() {
                continue;
            }
            visible.retain(|v| v.name != l.name);
            visible.push(l);
        }
        visible.sort_by_key(|l| l.slot);
        if visible.is_empty() {
            return Ok(out);
        }
        let mut w = self.w().object(thread).frame(frame).i32(visible.len() as i32);
        for l in &visible {
            w = w.i32(l.slot).u8(l.signature.as_bytes().first().copied().unwrap_or(b'L'));
        }
        let d = self.call(cmd::SF_GET_VALUES, w.done())?;
        let mut r = Reader::new(&d, self.ids);
        let n = r.i32().unwrap_or(0) as usize;
        for l in visible.into_iter().take(n) {
            let Ok(v) = r.value() else { break };
            out.push((l.name, v, l.signature));
        }
        Ok(out)
    }

    /// Instance fields of an object (this class and the ones above it).
    fn fields(&mut self, object: u64) -> Vec<(String, Value, String)> {
        let Some(mut class) = self.type_of(object) else { return Vec::new() };
        let mut fields: Vec<(u64, String, String)> = Vec::new();
        for _ in 0..16 {
            if class == 0 {
                break;
            }
            let sig = self.signature(class);
            if sig == "Ljava/lang/Object;" {
                break;
            }
            let data = self.w().reftype(class).done();
            if let Ok(d) = self.call(cmd::RT_FIELDS, data) {
                let mut r = Reader::new(&d, self.ids);
                let n = r.i32().unwrap_or(0);
                let mut own = Vec::new();
                for _ in 0..n {
                    let (Ok(id), Ok(name), Ok(sig), Ok(modifiers)) = (r.field(), r.string(), r.string(), r.i32()) else { break };
                    // Static and compiler-made fields aren't the object's state.
                    if modifiers & 0x0008 == 0 && modifiers & 0x1000 == 0 && !name.starts_with('$') {
                        own.push((id, name, sig));
                    }
                }
                own.extend(fields);
                fields = own;
            }
            class = self.super_of(class);
        }
        if fields.is_empty() {
            return Vec::new();
        }
        let mut w = self.w().object(object).i32(fields.len() as i32);
        for (id, _, _) in &fields {
            w = w.field(*id);
        }
        let Ok(d) = self.call(cmd::OR_GET_VALUES, w.done()) else { return Vec::new() };
        let mut r = Reader::new(&d, self.ids);
        let n = r.i32().unwrap_or(0) as usize;
        fields.into_iter().take(n).filter_map(|(_, name, sig)| r.value().ok().map(|v| (name, v, sig))).collect()
    }

    fn array(&mut self, id: u64, max: i32) -> (i32, Vec<Value>) {
        let data = self.w().object(id).done();
        let len = self.call(cmd::AR_LENGTH, data).ok().and_then(|d| Reader::new(&d, self.ids).i32().ok()).unwrap_or(0);
        let count = len.min(max);
        if count <= 0 {
            return (len, Vec::new());
        }
        let data = self.w().object(id).i32(0).i32(count).done();
        let Ok(d) = self.call(cmd::AR_GET_VALUES, data) else { return (len, Vec::new()) };
        let mut r = Reader::new(&d, self.ids);
        let Ok(tag) = r.u8() else { return (len, Vec::new()) };
        let n = r.i32().unwrap_or(0);
        let primitive = b"ZBCSIJFD".contains(&tag);
        let mut values = Vec::new();
        for _ in 0..n {
            let v = if primitive { r.untagged(tag) } else { r.value() };
            let Ok(v) = v else { break };
            values.push(v);
        }
        (len, values)
    }

    /// A value's text: primitives as Java writes them, strings quoted, collections, records and
    /// anything with its own `toString()` by that, other objects by their class name.
    fn display(&mut self, v: &Value) -> String {
        if let Some(text) = v.primitive_text() {
            return text;
        }
        let Some(id) = v.object_id() else { return "null".into() };
        match v {
            Value::Object { tag: b's', .. } => format!("\"{}\"", clip(&self.string(id), 1000)),
            Value::Object { tag: b'[', .. } => {
                let sig = self.type_of(id).map(|c| self.signature(c)).unwrap_or_default();
                let (len, items) = self.array(id, 12);
                let element = jdwp::type_name(&sig).trim_end_matches("[]").to_string();
                if len <= 12 && items.iter().all(|i| !i.is_object() || matches!(i, Value::Object { tag: b's', .. })) {
                    let parts: Vec<String> = items.iter().map(|i| self.display(i)).collect();
                    format!("[{}]", parts.join(", "))
                } else {
                    format!("{}[{len}]", jdwp::simple_name(&element))
                }
            }
            _ => {
                let Some(class) = self.type_of(id) else { return "object".into() };
                let sig = self.signature(class);
                let name = jdwp::simple_name(&jdwp::type_name(&sig)).to_string();
                match self.find_method(class, "toString", Some("()Ljava/lang/String;")) {
                    Some((declaring, m)) if self.signature(declaring) != "Ljava/lang/Object;" => match self.invoke(id, declaring, m.id, &[]) {
                        Ok(Value::Object { id: s, .. }) if s != 0 => clip(&self.string(s), 200),
                        _ => name,
                    },
                    _ => name,
                }
            }
        }
    }

    fn has_children(&mut self, v: &Value) -> bool {
        match v {
            Value::Object { tag: b's', .. } => false,
            Value::Object { id, .. } => *id != 0,
            _ => false,
        }
    }

    /// The parts of a value: array elements, a collection's elements (`toArray()`), a map's
    /// entries, else the object's fields.
    fn children(&mut self, v: &Value) -> Vec<(String, Value, String)> {
        let Some(id) = v.object_id() else { return Vec::new() };
        if matches!(v, Value::Object { tag: b'[', .. }) {
            let (_, items) = self.array(id, 300);
            return items.into_iter().enumerate().map(|(i, v)| (format!("[{i}]"), v, String::new())).collect();
        }
        let Some(class) = self.type_of(id) else { return Vec::new() };
        let sig = self.signature(class);
        if sig.starts_with("Ljava/util/") {
            if let Some((declaring, m)) = self.find_method(class, "entrySet", Some("()Ljava/util/Set;")) {
                if let Ok(Value::Object { id: set, .. }) = self.invoke(id, declaring, m.id, &[]) {
                    if let Some(set_class) = self.type_of(set) {
                        if let Some((d2, m2)) = self.find_method(set_class, "toArray", Some("()[Ljava/lang/Object;")) {
                            if let Ok(Value::Object { id: arr, .. }) = self.invoke(set, d2, m2.id, &[]) {
                                let (_, entries) = self.array(arr, 300);
                                let mut out = Vec::new();
                                for e in entries {
                                    let Some(eid) = e.object_id() else { continue };
                                    let Some(ec) = self.type_of(eid) else { continue };
                                    let key = self.find_method(ec, "getKey", Some("()Ljava/lang/Object;")).and_then(|(d, m)| self.invoke(eid, d, m.id, &[]).ok());
                                    let value = self.find_method(ec, "getValue", Some("()Ljava/lang/Object;")).and_then(|(d, m)| self.invoke(eid, d, m.id, &[]).ok());
                                    if let (Some(k), Some(v)) = (key, value) {
                                        let name = self.display(&k).trim_matches('"').to_string();
                                        out.push((name, v, String::new()));
                                    }
                                }
                                return out;
                            }
                        }
                    }
                }
            }
            if let Some((declaring, m)) = self.find_method(class, "toArray", Some("()[Ljava/lang/Object;")) {
                if let Ok(Value::Object { id: arr, .. }) = self.invoke(id, declaring, m.id, &[]) {
                    let (_, items) = self.array(arr, 300);
                    return items.into_iter().enumerate().map(|(i, v)| (format!("[{i}]"), v, String::new())).collect();
                }
            }
        }
        self.fields(id)
    }

    fn variable(&mut self, name: &str, v: Value, sig: &str) -> Json {
        let value = self.display(&v);
        let reference = if self.has_children(&v) {
            self.refs.push(VarRef::Children(v));
            self.refs.len() as i64
        } else {
            0
        };
        let ty = match v {
            Value::Object { id, .. } if id != 0 => self.type_of(id).map(|c| self.signature(c)).map(|s| jdwp::type_name(&s)).unwrap_or_default(),
            _ if !sig.is_empty() => jdwp::type_name(sig),
            _ => String::new(),
        };
        json!({ "name": name, "value": value, "type": jdwp::simple_name(&ty), "variablesReference": reference })
    }

    // ------------------------------------------------------------ expressions

    fn eval(&mut self, expr: &str, frame: usize) -> Result<Eval, String> {
        let tokens = tokenize(expr)?;
        let mut p = Parser { tokens, pos: 0 };
        let tree = p.expr()?;
        if p.pos < p.tokens.len() {
            return Err(format!("unexpected `{}`", p.tokens[p.pos]));
        }
        self.eval_node(&tree, frame)
    }

    fn eval_node(&mut self, node: &Node, frame: usize) -> Result<Eval, String> {
        Ok(match node {
            Node::Int(n) => Eval::Jvm(if i32::try_from(*n).is_ok() { Value::Int(*n as i32) } else { Value::Long(*n) }),
            Node::Float(f) => Eval::Jvm(Value::Double(*f)),
            Node::Str(s) => Eval::Text(s.clone()),
            Node::Bool(b) => Eval::Jvm(Value::Bool(*b)),
            Node::Null => Eval::Jvm(Value::Object { tag: b'L', id: 0 }),
            Node::Name(name) => {
                let locals = self.frame_locals(frame).unwrap_or_default();
                if let Some((_, v, _)) = locals.iter().find(|(n, _, _)| n == name) {
                    return Ok(Eval::Jvm(*v));
                }
                // A field of `this`.
                if let Some((_, this, _)) = locals.iter().find(|(n, _, _)| n == "this") {
                    let this = *this;
                    if let Some((_, v, _)) = self.fields(this.object_id().unwrap_or(0)).into_iter().find(|(n, _, _)| n == name) {
                        return Ok(Eval::Jvm(v));
                    }
                }
                return Err(format!("no variable `{name}` here"));
            }
            Node::Member(target, name, call) => {
                let target = self.eval_node(target, frame)?;
                let Eval::Jvm(v) = target else { return Err(format!("`.{name}` of a computed text")) };
                let id = v.object_id().ok_or_else(|| format!("`.{name}` of null or a number"))?;
                if !call && name == "length" && matches!(v, Value::Object { tag: b'[', .. }) {
                    return Ok(Eval::Jvm(Value::Int(self.array(id, 0).0)));
                }
                if !call {
                    if let Some((_, f, _)) = self.fields(id).into_iter().find(|(n, _, _)| n == name) {
                        return Ok(Eval::Jvm(f));
                    }
                }
                // A method without arguments (Kotlin / Scala properties are getters too).
                let class = self.type_of(id).ok_or("no class")?;
                let getter = format!("get{}{}", name.chars().next().map(|c| c.to_ascii_uppercase()).unwrap_or_default(), name.chars().skip(1).collect::<String>());
                let found = self.find_method(class, name, None).or_else(|| self.find_method(class, &getter, None));
                let (declaring, m) = found.ok_or_else(|| format!("no `{name}` in {}", jdwp::simple_name(&jdwp::type_name(&self.signature(class)))))?;
                Eval::Jvm(self.invoke(id, declaring, m.id, &[])?)
            }
            Node::Index(target, index) => {
                let Eval::Jvm(v) = self.eval_node(target, frame)? else { return Err("indexing a computed text".into()) };
                let i = match self.eval_node(index, frame)? {
                    Eval::Jvm(i) => i.as_i64().ok_or("the index isn't a number")?,
                    Eval::Text(_) => return Err("the index isn't a number".into()),
                };
                let id = v.object_id().ok_or("indexing null")?;
                if matches!(v, Value::Object { tag: b'[', .. }) {
                    let (len, items) = self.array(id, (i + 1).min(i32::MAX as i64) as i32);
                    return items.get(i as usize).map(|v| Eval::Jvm(*v)).ok_or_else(|| format!("index {i} out of bounds (length {len})"));
                }
                // A list: get(int).
                let class = self.type_of(id).ok_or("no class")?;
                let (declaring, m) = self.find_method(class, "get", Some("(I)Ljava/lang/Object;")).ok_or("can't index this value")?;
                Eval::Jvm(self.invoke(id, declaring, m.id, &[Value::Int(i as i32)])?)
            }
            Node::Neg(inner) => match self.eval_node(inner, frame)? {
                Eval::Jvm(v) => match v {
                    Value::Double(d) => Eval::Jvm(Value::Double(-d)),
                    Value::Float(f) => Eval::Jvm(Value::Double(-(f as f64))),
                    other => Eval::Jvm(Value::Long(-other.as_i64().ok_or("`-` of a non-number")?)),
                },
                Eval::Text(_) => return Err("`-` of a text".into()),
            },
            Node::Not(inner) => {
                let v = self.eval_node(inner, frame)?;
                Eval::Jvm(Value::Bool(!self.truthy(&v)))
            }
            Node::Bin(op, a, b) => {
                let a = self.eval_node(a, frame)?;
                let b = self.eval_node(b, frame)?;
                self.binary(op, a, b)?
            }
        })
    }

    fn truthy(&mut self, v: &Eval) -> bool {
        match v {
            Eval::Jvm(Value::Bool(b)) => *b,
            Eval::Jvm(Value::Object { id, .. }) => *id != 0,
            Eval::Jvm(other) => other.as_f64().is_some_and(|n| n != 0.0),
            Eval::Text(t) => !t.is_empty(),
        }
    }

    fn display_eval(&mut self, v: &Eval, quote: bool) -> String {
        match v {
            Eval::Text(t) => {
                if quote {
                    format!("\"{t}\"")
                } else {
                    t.clone()
                }
            }
            Eval::Jvm(Value::Object { tag: b's', id }) if !quote && *id != 0 => self.string(*id),
            Eval::Jvm(v) => self.display(v),
        }
    }

    fn binary(&mut self, op: &str, a: Eval, b: Eval) -> Result<Eval, String> {
        let is_text = |e: &Eval| matches!(e, Eval::Text(_) | Eval::Jvm(Value::Object { tag: b's', .. }));
        if op == "+" && (is_text(&a) || is_text(&b)) {
            return Ok(Eval::Text(format!("{}{}", self.display_eval(&a, false), self.display_eval(&b, false))));
        }
        if op == "&&" || op == "||" {
            let (x, y) = (self.truthy(&a), self.truthy(&b));
            return Ok(Eval::Jvm(Value::Bool(if op == "&&" { x && y } else { x || y })));
        }
        if op == "==" || op == "!=" {
            let equal = match (&a, &b) {
                (Eval::Jvm(x), Eval::Jvm(y)) if x.as_f64().is_some() && y.as_f64().is_some() => x.as_f64() == y.as_f64(),
                (Eval::Jvm(Value::Bool(x)), Eval::Jvm(Value::Bool(y))) => x == y,
                _ if is_text(&a) && is_text(&b) => self.display_eval(&a, false) == self.display_eval(&b, false),
                (Eval::Jvm(x), Eval::Jvm(y)) => x.object_id() == y.object_id(),
                _ => false,
            };
            return Ok(Eval::Jvm(Value::Bool(if op == "==" { equal } else { !equal })));
        }
        let (Eval::Jvm(x), Eval::Jvm(y)) = (&a, &b) else { return Err(format!("`{op}` needs numbers")) };
        let integral = |v: &Value| v.as_i64().is_some();
        if integral(x) && integral(y) {
            let (p, q) = (x.as_i64().unwrap_or(0), y.as_i64().unwrap_or(0));
            let r = match op {
                "+" => p.wrapping_add(q),
                "-" => p.wrapping_sub(q),
                "*" => p.wrapping_mul(q),
                "/" if q == 0 => return Err("division by zero".into()),
                "/" => p / q,
                "%" if q == 0 => return Err("division by zero".into()),
                "%" => p % q,
                "<" => return Ok(Eval::Jvm(Value::Bool(p < q))),
                ">" => return Ok(Eval::Jvm(Value::Bool(p > q))),
                "<=" => return Ok(Eval::Jvm(Value::Bool(p <= q))),
                ">=" => return Ok(Eval::Jvm(Value::Bool(p >= q))),
                _ => return Err(format!("unknown operator {op}")),
            };
            return Ok(Eval::Jvm(if i32::try_from(r).is_ok() { Value::Int(r as i32) } else { Value::Long(r) }));
        }
        let (p, q) = (x.as_f64().ok_or("not a number")?, y.as_f64().ok_or("not a number")?);
        Ok(Eval::Jvm(match op {
            "+" => Value::Double(p + q),
            "-" => Value::Double(p - q),
            "*" => Value::Double(p * q),
            "/" => Value::Double(p / q),
            "%" => Value::Double(p % q),
            "<" => Value::Bool(p < q),
            ">" => Value::Bool(p > q),
            "<=" => Value::Bool(p <= q),
            ">=" => Value::Bool(p >= q),
            _ => return Err(format!("unknown operator {op}")),
        }))
    }

    // ------------------------------------------------------------ DAP requests

    fn handle(&mut self, req: Json) -> bool {
        if req["type"] != "request" {
            return true;
        }
        let command = req["command"].as_str().unwrap_or("").to_string();
        let result = match command.as_str() {
            "initialize" => {
                self.dap.respond(
                    &req,
                    json!({ "supportsConfigurationDoneRequest": true, "supportsConditionalBreakpoints": true, "supportsLogPoints": true, "supportsEvaluateForHovers": true, "supportsTerminateRequest": true }),
                );
                Ok(())
            }
            "launch" => self.launch(&req),
            "setBreakpoints" => self.set_breakpoints(&req),
            "setExceptionBreakpoints" => {
                self.dap.respond(&req, json!({ "breakpoints": [] }));
                Ok(())
            }
            "configurationDone" => {
                self.dap.respond(&req, json!({}));
                self.configured = true;
                self.start_if_ready();
                Ok(())
            }
            "threads" => self.threads(&req),
            "stackTrace" => self.stack(&req),
            "scopes" => {
                let frame = req["arguments"]["frameId"].as_i64().unwrap_or(1);
                self.refs.push(VarRef::Locals((frame - 1).max(0) as usize));
                let reference = self.refs.len() as i64;
                self.dap.respond(&req, json!({ "scopes": [{ "name": "Locals", "variablesReference": reference, "expensive": false }] }));
                Ok(())
            }
            "variables" => self.variables(&req),
            "evaluate" => {
                let a = &req["arguments"];
                let expr = a["expression"].as_str().unwrap_or("").to_string();
                let frame = (a["frameId"].as_i64().unwrap_or(1) - 1).max(0) as usize;
                match self.eval(&expr, frame) {
                    Ok(Eval::Jvm(v)) => {
                        let var = self.variable("", v, "");
                        self.dap.respond(&req, json!({ "result": var["value"], "type": var["type"], "variablesReference": var["variablesReference"] }));
                        Ok(())
                    }
                    Ok(text) => {
                        let shown = self.display_eval(&text, true);
                        self.dap.respond(&req, json!({ "result": shown, "variablesReference": 0 }));
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            "continue" => {
                self.resume_vm();
                self.dap.respond(&req, json!({ "allThreadsContinued": true }));
                Ok(())
            }
            "next" | "stepIn" | "stepOut" => {
                let depth = match command.as_str() {
                    "stepIn" => 0,
                    "next" => 1,
                    _ => 2,
                };
                self.step(depth).map(|()| self.dap.respond(&req, json!({})))
            }
            "pause" => {
                let _ = self.call(cmd::VM_SUSPEND, Vec::new());
                let main = self.main_thread();
                self.thread = main;
                self.dap.respond(&req, json!({}));
                self.dap.event("stopped", json!({ "reason": "pause", "threadId": main.unwrap_or(1) as i64, "allThreadsStopped": true }));
                Ok(())
            }
            "disconnect" | "terminate" => {
                if req["arguments"]["terminateDebuggee"].as_bool().unwrap_or(true) {
                    self.kill();
                } else if self.vm.is_some() {
                    let _ = self.send(cmd::VM_DISPOSE, &[]);
                }
                self.dap.respond(&req, json!({}));
                if command == "disconnect" {
                    return false;
                }
                Ok(())
            }
            _ => Err(format!("{command} isn't supported for the JVM")),
        };
        if let Err(e) = result {
            self.dap.fail(&req, &e);
        }
        true
    }

    fn launch(&mut self, req: &Json) -> Result<(), String> {
        let a = &req["arguments"];
        let command: Vec<String> = a["command"].as_array().into_iter().flatten().filter_map(|v| v.as_str()).map(str::to_string).collect();
        let (java, rest) = command.split_first().ok_or("launch: no command")?;
        self.roots = a["sourceRoots"].as_array().into_iter().flatten().filter_map(|v| v.as_str()).map(PathBuf::from).collect();
        self.project = a["project"].as_str().map(PathBuf::from);
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("no free port for the JVM: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else { return };
            // The debugger sends the handshake, the VM echoes it.
            let mut echo = [0u8; 14];
            if stream.write_all(jdwp::HANDSHAKE).is_err() || stream.read_exact(&mut echo).is_err() || echo != jdwp::HANDSHAKE {
                let _ = tx.send(Msg::EngineGone);
                return;
            }
            let Ok(writer) = stream.try_clone() else { return };
            let _ = tx.send(Msg::Connected(writer));
            let mut reader = BufReader::new(stream);
            while let Ok(p) = jdwp::read_packet(&mut reader) {
                if tx.send(Msg::Packet(p)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Msg::EngineGone);
        });
        let mut args = vec![java.clone(), format!("-agentlib:jdwp=transport=dt_socket,server=n,suspend=y,address=127.0.0.1:{port}")];
        args.extend(rest.iter().cloned());
        self.dap.request("runInTerminal", json!({ "kind": "integrated", "title": "JVM", "cwd": a["cwd"], "args": args, "env": a["env"] }));
        self.dap.respond(req, json!({}));
        self.dap.event("initialized", json!({}));
        Ok(())
    }

    fn main_thread(&mut self) -> Option<u64> {
        let ids = self.thread_ids();
        for id in &ids {
            if self.thread_name(*id) == "main" {
                return Some(*id);
            }
        }
        ids.first().copied()
    }

    fn thread_ids(&mut self) -> Vec<u64> {
        let Ok(d) = self.call(cmd::VM_ALL_THREADS, Vec::new()) else { return Vec::new() };
        let mut r = Reader::new(&d, self.ids);
        let n = r.i32().unwrap_or(0);
        (0..n).filter_map(|_| r.object().ok()).collect()
    }

    fn thread_name(&mut self, id: u64) -> String {
        let data = self.w().object(id).done();
        self.call(cmd::TR_NAME, data).ok().and_then(|d| Reader::new(&d, self.ids).string().ok()).unwrap_or_default()
    }

    fn threads(&mut self, req: &Json) -> Result<(), String> {
        let ids = self.thread_ids();
        let mut threads: Vec<Json> = Vec::new();
        for id in ids {
            let name = self.thread_name(id);
            threads.push(json!({ "id": id as i64, "name": name }));
        }
        // The stopped thread first: the client shows the first thread when it doesn't know.
        if let Some(t) = self.thread {
            threads.sort_by_key(|j| j["id"].as_i64() != Some(t as i64));
        }
        self.dap.respond(req, json!({ "threads": threads }));
        Ok(())
    }

    fn stack(&mut self, req: &Json) -> Result<(), String> {
        let thread = req["arguments"]["threadId"].as_i64().map(|t| t as u64).or(self.thread).ok_or("no thread")?;
        self.load_frames(thread);
        let mut out = Vec::new();
        for (i, (_, _, loc)) in self.frames.clone().into_iter().enumerate() {
            let sig = self.signature(loc.class);
            let method = self.methods(loc.class).into_iter().find(|m| m.id == loc.method).map(|m| m.name).unwrap_or_else(|| "?".into());
            let line = self.line_of(&loc);
            // Generated code with no line (Kotlin's `main(String[])` calling `main()`).
            if i > 0 && line == 0 {
                continue;
            }
            let class_name = jdwp::type_name(&sig);
            let mut frame = json!({ "id": i + 1, "name": format!("{}.{method}", jdwp::simple_name(&class_name)), "line": line, "column": 1 });
            let path = match self.classes.get(&loc.class).and_then(|c| c.path.clone()) {
                Some(p) => Some(p),
                None => {
                    let p = self.source_path(loc.class, &sig);
                    if let Some(c) = self.classes.get_mut(&loc.class) {
                        c.path = p.clone();
                    }
                    p
                }
            };
            if let Some(path) = path {
                let short = Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                frame["source"] = json!({ "path": path, "name": short });
            }
            out.push(frame);
        }
        self.dap.respond(req, json!({ "stackFrames": out, "totalFrames": self.frames.len() }));
        Ok(())
    }

    fn variables(&mut self, req: &Json) -> Result<(), String> {
        let r = req["arguments"]["variablesReference"].as_i64().unwrap_or(0);
        let target = usize::try_from(r - 1).ok().and_then(|i| self.refs.get(i).cloned()).ok_or("this value is gone (the program moved on)")?;
        let items = match target {
            VarRef::Locals(frame) => match self.frame_locals(frame) {
                Ok(items) => items,
                Err(e) => {
                    self.dap.respond(req, json!({ "variables": [{ "name": "(locals)", "value": e, "variablesReference": 0 }] }));
                    return Ok(());
                }
            },
            VarRef::Children(v) => self.children(&v),
        };
        let mut vars = Vec::new();
        for (name, v, sig) in items {
            vars.push(self.variable(&name, v, &sig));
        }
        self.dap.respond(req, json!({ "variables": vars }));
        Ok(())
    }

    fn step(&mut self, depth: i32) -> Result<(), String> {
        self.step_by(1, depth)
    }

    /// A step request: `size` 1 = to the next line, 0 = one instruction.
    fn step_by(&mut self, size: i32, depth: i32) -> Result<(), String> {
        let thread = self.thread.ok_or("the program isn't stopped")?;
        let mut w = self.w().u8(event::SINGLE_STEP).u8(2).i32(1 + LIBRARY.len() as i32).u8(10).object(thread).i32(size).i32(depth);
        for pattern in LIBRARY {
            w = w.u8(6).string(pattern);
        }
        let d = self.call(cmd::ER_SET, w.done())?;
        self.step = Reader::new(&d, self.ids).i32().ok();
        self.resume_vm();
        Ok(())
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max { format!("{}…", s.chars().take(max).collect::<String>()) } else { s.to_string() }
}

/// A file called `name` in the project, preferring the one whose path ends with `suffix`
/// (its package folders). Build output and dependency folders are skipped.
fn search(root: &Path, name: &str, suffix: &str) -> Option<String> {
    let mut stack = vec![root.to_path_buf()];
    let mut by_name = None;
    let mut seen = 0;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            seen += 1;
            if seen > 50_000 {
                return by_name;
            }
            let path = e.path();
            let file_name = e.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if !matches!(file_name.as_str(), "target" | "build" | "out" | "node_modules" | ".git" | ".gradle" | ".idea" | ".bsp" | ".scala-build" | ".metals" | ".bloop") {
                    stack.push(path);
                }
            } else if file_name.eq_ignore_ascii_case(name) {
                let p = path.to_string_lossy().to_string();
                if key(&p).ends_with(suffix) {
                    return Some(p);
                }
                by_name.get_or_insert(p);
            }
        }
    }
    by_name
}

// ---------------------------------------------------------------- the expression parser

#[derive(Debug)]
enum Node {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    Name(String),
    Member(Box<Node>, String, bool),
    Index(Box<Node>, Box<Node>),
    Neg(Box<Node>),
    Not(Box<Node>),
    Bin(String, Box<Node>, Box<Node>),
}

fn tokenize(s: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_') {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
        } else if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$') {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
        } else if c == '"' || c == '\'' {
            let mut text = String::from('"');
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                }
                text.push(chars[i]);
                i += 1;
            }
            if i >= chars.len() {
                return Err("unterminated string".into());
            }
            i += 1;
            out.push(text);
        } else {
            let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
            if ["==", "!=", "<=", ">=", "&&", "||"].contains(&two.as_str()) {
                out.push(two);
                i += 2;
            } else if "+-*/%<>()[].!".contains(c) {
                out.push(c.to_string());
                i += 1;
            } else {
                return Err(format!("Graphyne can't read `{c}` in an expression"));
            }
        }
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<String>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(String::as_str)
    }
    fn next(&mut self) -> Option<String> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn expr(&mut self) -> Result<Node, String> {
        self.binary(0)
    }
    fn binary(&mut self, level: usize) -> Result<Node, String> {
        const LEVELS: [&[&str]; 5] = [&["||"], &["&&"], &["==", "!=", "<", ">", "<=", ">="], &["+", "-"], &["*", "/", "%"]];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        while let Some(op) = self.peek().filter(|t| LEVELS[level].contains(t)).map(str::to_string) {
            self.pos += 1;
            let right = self.binary(level + 1)?;
            left = Node::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }
    fn unary(&mut self) -> Result<Node, String> {
        match self.peek() {
            Some("-") => {
                self.pos += 1;
                Ok(Node::Neg(Box::new(self.unary()?)))
            }
            Some("!") => {
                self.pos += 1;
                Ok(Node::Not(Box::new(self.unary()?)))
            }
            _ => self.postfix(),
        }
    }
    fn postfix(&mut self) -> Result<Node, String> {
        let mut node = self.primary()?;
        loop {
            match self.peek() {
                Some(".") => {
                    self.pos += 1;
                    let name = self.next().filter(|t| t.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')).ok_or("a name after `.`")?;
                    let call = self.peek() == Some("(");
                    if call {
                        self.pos += 1;
                        if self.next().as_deref() != Some(")") {
                            return Err("Graphyne calls methods without arguments only".into());
                        }
                    }
                    node = Node::Member(Box::new(node), name, call);
                }
                Some("[") => {
                    self.pos += 1;
                    let index = self.expr()?;
                    if self.next().as_deref() != Some("]") {
                        return Err("missing `]`".into());
                    }
                    node = Node::Index(Box::new(node), Box::new(index));
                }
                _ => return Ok(node),
            }
        }
    }
    fn primary(&mut self) -> Result<Node, String> {
        let t = self.next().ok_or("an expression")?;
        if t == "(" {
            let inner = self.expr()?;
            if self.next().as_deref() != Some(")") {
                return Err("missing `)`".into());
            }
            return Ok(inner);
        }
        if let Some(text) = t.strip_prefix('"') {
            return Ok(Node::Str(text.to_string()));
        }
        if t.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            let clean = t.trim_end_matches(['L', 'l', 'f', 'F', 'd', 'D']).replace('_', "");
            if let Ok(n) = clean.parse::<i64>() {
                return Ok(Node::Int(n));
            }
            return clean.parse::<f64>().map(Node::Float).map_err(|_| format!("`{t}` isn't a number"));
        }
        Ok(match t.as_str() {
            "true" => Node::Bool(true),
            "false" => Node::Bool(false),
            "null" => Node::Null,
            _ if t.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$') => Node::Name(t),
            _ => return Err(format!("unexpected `{t}`")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> String {
        let mut p = Parser { tokens: tokenize(s).unwrap(), pos: 0 };
        format!("{:?}", p.expr().unwrap())
    }

    #[test]
    fn expressions() {
        assert_eq!(parse("total + i * 10"), r#"Bin("+", Name("total"), Bin("*", Name("i"), Int(10)))"#);
        assert_eq!(parse("point.x * point.y"), r#"Bin("*", Member(Name("point"), "x", false), Member(Name("point"), "y", false))"#);
        assert_eq!(parse("items.size() > 1 && !done"), r#"Bin("&&", Bin(">", Member(Name("items"), "size", true), Int(1)), Not(Name("done")))"#);
        assert_eq!(parse("args[0] + \"!\""), r#"Bin("+", Index(Name("args"), Int(0)), Str("!"))"#);
        assert!(tokenize("a ? b : c").is_err());
        assert!(library_class("Ljava/util/ArrayList;") && library_class("Lkotlin/collections/CollectionsKt;") && !library_class("Lcom/demo/Main;"));
    }
}
