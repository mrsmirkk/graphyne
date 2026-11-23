//! PHP debugging: a DAP adapter over Xdebug's DBGp protocol (xdebug.org/docs/dbgp). The bridge
//! listens on a free port, asks the client (Graphyne) to run `php -d xdebug.… file.php` in a
//! terminal tab (`runInTerminal`), and Xdebug connects back. DBGp is request / response: the
//! bridge sends `command -i <id> [args] [-- base64]`, Xdebug answers with XML.

use super::Dap;
use base64::Engine as _;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

enum Msg {
    Dap(Value),
    ClientGone,
    /// Xdebug connected: the connection to write commands to.
    Connected(TcpStream),
    /// One message from Xdebug (XML).
    Engine(String),
    EngineGone,
}

/// What a `variablesReference` stands for.
#[derive(Clone)]
enum VarRef {
    Context { depth: u32, context: u32 },
    Property { depth: u32, context: u32, fullname: String },
}

#[derive(Clone)]
struct Bp {
    line: i64,
    condition: Option<String>,
}

struct Session {
    dap: Dap,
    tx: flume::Sender<Msg>,
    rx: flume::Receiver<Msg>,
    /// Messages that arrived while waiting for an answer from Xdebug.
    deferred: VecDeque<Msg>,
    xdebug: Option<TcpStream>,
    /// Xdebug's `init` arrived.
    ready: bool,
    /// The client sent `configurationDone`.
    configured: bool,
    started: bool,
    /// A `run` / `step_*` is under way: its transaction id and why the program will stop.
    resume: Option<(u32, &'static str)>,
    txid: u32,
    /// Breakpoints per file URI, and Xdebug's ids for the ones set.
    breakpoints: HashMap<String, Vec<Bp>>,
    set: HashMap<String, Vec<String>>,
    /// Files whose breakpoints changed while the program ran.
    pending_files: Vec<String>,
    refs: Vec<VarRef>,
    ended: bool,
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
        xdebug: None,
        ready: false,
        configured: false,
        started: false,
        resume: None,
        txid: 0,
        breakpoints: HashMap::new(),
        set: HashMap::new(),
        pending_files: Vec::new(),
        refs: Vec::new(),
        ended: false,
    };
    s.run();
    Ok(())
}

// ---------------------------------------------------------------- DBGp wire format

/// `<length>\0<xml>\0`
fn read_packet(input: &mut impl Read) -> Option<String> {
    let mut len = Vec::new();
    loop {
        let mut b = [0u8; 1];
        input.read_exact(&mut b).ok()?;
        if b[0] == 0 {
            break;
        }
        len.push(b[0]);
    }
    let len: usize = std::str::from_utf8(&len).ok()?.trim().parse().ok()?;
    let mut body = vec![0; len + 1];
    input.read_exact(&mut body).ok()?;
    body.pop();
    Some(String::from_utf8_lossy(&body).into_owned())
}

/// A DBGp argument value: quoted when it has spaces or quotes.
fn quote(value: &str) -> String {
    if !value.contains([' ', '"', '\\']) && !value.is_empty() {
        return value.to_string();
    }
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

fn unb64(text: &str) -> String {
    let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(clean).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

/// `C:\a b\x.php` → `file:///C:/a%20b/x.php`
pub fn file_uri(path: &str) -> String {
    let p = path.replace('\\', "/");
    let p = if p.starts_with('/') { p } else { format!("/{p}") };
    let mut out = String::from("file://");
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The other way round; `None` for code that isn't a file (`dbgp://…`, eval'd code).
pub fn uri_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    let path = String::from_utf8_lossy(&out).into_owned();
    if cfg!(windows) {
        // /C:/x → C:\x
        let p = if path.len() > 2 && path.as_bytes()[0] == b'/' && path.as_bytes()[2] == b':' { &path[1..] } else { &path[..] };
        Some(p.replace('/', "\\"))
    } else {
        Some(path)
    }
}

fn transaction_id(xml: &str) -> Option<u32> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    doc.root_element().attribute("transaction_id")?.parse().ok()
}

/// The `<error><message>` of a response, if it is one.
fn error_message(doc: &roxmltree::Document) -> Option<String> {
    let error = doc.root_element().children().find(|n| n.has_tag_name("error"))?;
    let message = error.children().find(|n| n.has_tag_name("message")).and_then(|m| m.text()).unwrap_or("error");
    Some(message.to_string())
}

/// A property's value as the variables tree shows it.
fn display(p: roxmltree::Node) -> String {
    let ty = p.attribute("type").unwrap_or("");
    let raw: String = p.children().filter(|n| n.is_text()).filter_map(|n| n.text()).collect();
    let text = if p.attribute("encoding") == Some("base64") { unb64(&raw) } else { raw.trim().to_string() };
    match ty {
        "string" => format!("\"{text}\""),
        "bool" => (if text == "1" { "true" } else { "false" }).to_string(),
        "null" => "null".to_string(),
        "uninitialized" => "uninitialized".to_string(),
        "array" => format!("array({})", p.attribute("numchildren").unwrap_or("0")),
        "object" => p.attribute("classname").unwrap_or("object").to_string(),
        _ => text,
    }
}

// ---------------------------------------------------------------- the session

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
                    self.close();
                    return;
                }
                Msg::Connected(stream) => self.xdebug = Some(stream),
                Msg::Engine(xml) => self.engine(&xml),
                Msg::EngineGone => self.finished(),
            }
        }
    }

    // ------------------------------------------------------------ talking to Xdebug

    fn send(&mut self, command: &str, args: &str, data: Option<&str>) -> Result<u32, String> {
        self.txid += 1;
        let id = self.txid;
        let mut line = format!("{command} -i {id}");
        if !args.is_empty() {
            line.push(' ');
            line.push_str(args);
        }
        if let Some(d) = data {
            line.push_str(" -- ");
            line.push_str(&b64(d));
        }
        line.push('\0');
        let stream = self.xdebug.as_mut().ok_or("Xdebug isn't connected")?;
        stream.write_all(line.as_bytes()).map_err(|e| format!("Xdebug: {e}"))?;
        Ok(id)
    }

    /// Send a command and wait for its answer (other messages wait their turn).
    fn call(&mut self, command: &str, args: &str, data: Option<&str>) -> Result<String, String> {
        if self.resume.is_some() {
            return Err("the program is running".into());
        }
        let id = self.send(command, args, data)?;
        loop {
            match self.rx.recv_timeout(Duration::from_secs(30)) {
                Ok(Msg::Engine(xml)) if transaction_id(&xml) == Some(id) => return Ok(xml),
                Ok(Msg::EngineGone) => {
                    self.deferred.push_back(Msg::EngineGone);
                    return Err("Xdebug disconnected".into());
                }
                Ok(other) => self.deferred.push_back(other),
                Err(_) => return Err(format!("Xdebug didn't answer {command}")),
            }
        }
    }

    fn resume_with(&mut self, command: &str, reason: &'static str) -> Result<(), String> {
        if self.resume.is_some() {
            return Ok(());
        }
        let id = self.send(command, "", None)?;
        self.resume = Some((id, reason));
        self.refs.clear();
        Ok(())
    }

    /// Set the breakpoints of `uri` in Xdebug (replacing the ones set before).
    fn apply(&mut self, uri: &str) {
        for id in self.set.remove(uri).unwrap_or_default() {
            let _ = self.call("breakpoint_remove", &format!("-d {id}"), None);
        }
        let bps = self.breakpoints.get(uri).cloned().unwrap_or_default();
        let mut ids = Vec::new();
        for bp in bps {
            let args = match &bp.condition {
                Some(_) => format!("-t conditional -f {} -n {}", quote(uri), bp.line),
                None => format!("-t line -f {} -n {}", quote(uri), bp.line),
            };
            if let Ok(xml) = self.call("breakpoint_set", &args, bp.condition.as_deref()) {
                if let Some(id) = roxmltree::Document::parse(&xml).ok().and_then(|d| d.root_element().attribute("id").map(str::to_string)) {
                    ids.push(id);
                }
            }
        }
        self.set.insert(uri.to_string(), ids);
    }

    fn start_if_ready(&mut self) {
        if self.ready && self.configured && !self.started {
            self.started = true;
            let files: Vec<String> = self.breakpoints.keys().cloned().collect();
            for uri in files {
                self.apply(&uri);
            }
            if let Err(e) = self.resume_with("run", "breakpoint") {
                self.dap.output("stderr", &format!("{e}\n"));
            }
        }
    }

    fn engine(&mut self, xml: &str) {
        let Ok(doc) = roxmltree::Document::parse(xml) else { return };
        let root = doc.root_element();
        match root.tag_name().name() {
            "init" => {
                self.ready = true;
                // One level of children per request; long strings in full up to 64 KB.
                for (name, value) in [("max_depth", "1"), ("max_children", "200"), ("max_data", "65536")] {
                    let _ = self.call("feature_set", &format!("-n {name} -v {value}"), None);
                }
                self.start_if_ready();
            }
            "response" => {
                let id: Option<u32> = root.attribute("transaction_id").and_then(|t| t.parse().ok());
                let Some((resume_id, reason)) = self.resume else { return };
                if id != Some(resume_id) {
                    return;
                }
                self.resume = None;
                match root.attribute("status") {
                    Some("break") => {
                        // Breakpoints changed while it ran.
                        for uri in std::mem::take(&mut self.pending_files) {
                            self.apply(&uri);
                        }
                        self.dap.event("stopped", json!({ "reason": reason, "threadId": 1, "allThreadsStopped": true }));
                    }
                    Some("stopping") | Some("stopped") => self.finished(),
                    _ => {}
                }
            }
            "stream" => {
                let text = unb64(root.text().unwrap_or(""));
                let category = if root.attribute("type") == Some("stderr") { "stderr" } else { "stdout" };
                self.dap.output(category, &text);
            }
            _ => {}
        }
    }

    /// The script ended (or Xdebug went away).
    fn finished(&mut self) {
        if self.ended {
            return;
        }
        self.ended = true;
        if self.xdebug.is_some() {
            let _ = self.send("stop", "", None);
        }
        self.xdebug = None;
        self.dap.event("exited", json!({ "exitCode": 0 }));
        self.dap.event("terminated", json!({}));
    }

    fn close(&mut self) {
        if let Some(stream) = self.xdebug.take() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    fn reference(&mut self, r: VarRef) -> i64 {
        self.refs.push(r);
        self.refs.len() as i64
    }

    fn variable(&mut self, p: roxmltree::Node, depth: u32, context: u32) -> Value {
        let name = p.attribute("name").unwrap_or("?").to_string();
        let fullname = p.attribute("fullname").unwrap_or(&name).to_string();
        let reference = if p.attribute("children") == Some("1") { self.reference(VarRef::Property { depth, context, fullname: fullname.clone() }) } else { 0 };
        json!({ "name": name, "value": display(p), "type": p.attribute("type").unwrap_or(""), "variablesReference": reference, "evaluateName": fullname })
    }

    // ------------------------------------------------------------ DAP requests

    /// One request from the client; false once the session is over.
    fn handle(&mut self, req: Value) -> bool {
        if req["type"] != "request" {
            return true; // e.g. the answer to runInTerminal
        }
        let command = req["command"].as_str().unwrap_or("").to_string();
        let result = match command.as_str() {
            "initialize" => {
                self.dap.respond(
                    &req,
                    json!({ "supportsConfigurationDoneRequest": true, "supportsConditionalBreakpoints": true, "supportsEvaluateForHovers": true, "supportsTerminateRequest": true }),
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
            "threads" => {
                self.dap.respond(&req, json!({ "threads": [{ "id": 1, "name": "PHP" }] }));
                Ok(())
            }
            "stackTrace" => self.stack(&req),
            "scopes" => self.scopes(&req),
            "variables" => self.variables(&req),
            "evaluate" => self.evaluate(&req),
            "continue" | "next" | "stepIn" | "stepOut" => {
                let (cmd, reason) = match command.as_str() {
                    "continue" => ("run", "breakpoint"),
                    "next" => ("step_over", "step"),
                    "stepIn" => ("step_into", "step"),
                    _ => ("step_out", "step"),
                };
                self.resume_with(cmd, reason).map(|()| self.dap.respond(&req, json!({ "allThreadsContinued": true })))
            }
            "pause" => Err("Xdebug can't pause a running PHP script: set a breakpoint".into()),
            "disconnect" | "terminate" => {
                // Paused: Xdebug ends the script. Running: it can't be reached, it runs to the end.
                if self.resume.is_none() && self.xdebug.is_some() && !self.ended {
                    let _ = self.send("stop", "", None);
                }
                self.dap.respond(&req, json!({}));
                if command == "disconnect" {
                    self.close();
                    return false;
                }
                Ok(())
            }
            _ => Err(format!("{command} isn't supported for PHP")),
        };
        if let Err(e) = result {
            self.dap.fail(&req, &e);
        }
        true
    }

    fn launch(&mut self, req: &Value) -> Result<(), String> {
        let a = &req["arguments"];
        let php = a["php"].as_str().ok_or("launch: no php")?;
        let program = a["program"].as_str().ok_or("launch: no program")?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("no free port for Xdebug: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else { return };
            let Ok(writer) = stream.try_clone() else { return };
            let _ = tx.send(Msg::Connected(writer));
            let mut reader = BufReader::new(stream);
            while let Some(xml) = read_packet(&mut reader) {
                if tx.send(Msg::Engine(xml)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Msg::EngineGone);
        });
        let mut args = vec![php.to_string()];
        args.extend(a["phpArgs"].as_array().into_iter().flatten().filter_map(|v| v.as_str()).map(str::to_string));
        args.extend(["-d".to_string(), format!("xdebug.client_port={port}"), program.to_string()]);
        self.dap.request("runInTerminal", json!({ "kind": "integrated", "title": "PHP", "cwd": a["cwd"], "args": args, "env": a["env"] }));
        self.dap.respond(req, json!({}));
        self.dap.event("initialized", json!({}));
        Ok(())
    }

    fn set_breakpoints(&mut self, req: &Value) -> Result<(), String> {
        let a = &req["arguments"];
        let path = a["source"]["path"].as_str().ok_or("setBreakpoints: no path")?;
        let uri = file_uri(path);
        let mut bps = Vec::new();
        let mut answer = Vec::new();
        for b in a["breakpoints"].as_array().into_iter().flatten() {
            let line = b["line"].as_i64().unwrap_or(1);
            if b["logMessage"].as_str().is_some_and(|m| !m.is_empty()) {
                answer.push(json!({ "verified": false, "line": line, "message": "Logpoints aren't supported for PHP yet" }));
                continue;
            }
            let condition = b["condition"].as_str().filter(|c| !c.trim().is_empty()).map(str::to_string);
            bps.push(Bp { line, condition });
            answer.push(json!({ "verified": true, "line": line }));
        }
        self.breakpoints.insert(uri.clone(), bps);
        if self.started {
            if self.resume.is_some() {
                // Xdebug reads commands only while the script is paused.
                if !self.pending_files.contains(&uri) {
                    self.pending_files.push(uri);
                }
            } else {
                self.apply(&uri);
            }
        }
        self.dap.respond(req, json!({ "breakpoints": answer }));
        Ok(())
    }

    fn stack(&mut self, req: &Value) -> Result<(), String> {
        let xml = self.call("stack_get", "", None)?;
        let doc = roxmltree::Document::parse(&xml).map_err(|e| e.to_string())?;
        let frames: Vec<Value> = doc
            .root_element()
            .children()
            .filter(|n| n.has_tag_name("stack"))
            .map(|f| {
                let level: i64 = f.attribute("level").and_then(|l| l.parse().ok()).unwrap_or(0);
                let line: i64 = f.attribute("lineno").and_then(|l| l.parse().ok()).unwrap_or(0);
                let name = f.attribute("where").unwrap_or("?");
                let mut frame = json!({ "id": level + 1, "name": name, "line": line, "column": 1 });
                if let Some(path) = f.attribute("filename").and_then(uri_path) {
                    let short = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    frame["source"] = json!({ "path": path, "name": short });
                }
                frame
            })
            .collect();
        self.dap.respond(req, json!({ "stackFrames": frames, "totalFrames": frames.len() }));
        Ok(())
    }

    fn scopes(&mut self, req: &Value) -> Result<(), String> {
        let depth = (req["arguments"]["frameId"].as_i64().unwrap_or(1) - 1).max(0) as u32;
        let xml = self.call("context_names", &format!("-d {depth}"), None)?;
        let doc = roxmltree::Document::parse(&xml).map_err(|e| e.to_string())?;
        let contexts: Vec<(String, u32)> = doc
            .root_element()
            .children()
            .filter(|n| n.has_tag_name("context"))
            .map(|c| (c.attribute("name").unwrap_or("?").to_string(), c.attribute("id").and_then(|i| i.parse().ok()).unwrap_or(0)))
            .collect();
        let scopes: Vec<Value> = contexts
            .into_iter()
            .map(|(name, context)| {
                let reference = self.reference(VarRef::Context { depth, context });
                json!({ "name": name, "variablesReference": reference, "expensive": context != 0 })
            })
            .collect();
        self.dap.respond(req, json!({ "scopes": scopes }));
        Ok(())
    }

    fn variables(&mut self, req: &Value) -> Result<(), String> {
        let r = req["arguments"]["variablesReference"].as_i64().unwrap_or(0);
        let target = usize::try_from(r - 1).ok().and_then(|i| self.refs.get(i).cloned()).ok_or("this value is gone (the program moved on)")?;
        let (xml, depth, context, nested) = match &target {
            VarRef::Context { depth, context } => (self.call("context_get", &format!("-d {depth} -c {context}"), None)?, *depth, *context, false),
            VarRef::Property { depth, context, fullname } => {
                (self.call("property_get", &format!("-d {depth} -c {context} -n {}", quote(fullname)), None)?, *depth, *context, true)
            }
        };
        let doc = roxmltree::Document::parse(&xml).map_err(|e| e.to_string())?;
        if let Some(e) = error_message(&doc) {
            return Err(e);
        }
        let parent = if nested { doc.root_element().children().find(|n| n.has_tag_name("property")) } else { Some(doc.root_element()) };
        // Variables not assigned yet (further down the function) are left out.
        let props: Vec<roxmltree::Node> =
            parent.into_iter().flat_map(|p| p.children().filter(|n| n.has_tag_name("property") && n.attribute("type") != Some("uninitialized"))).collect();
        let vars: Vec<Value> = props.into_iter().map(|p| self.variable(p, depth, context)).collect();
        self.dap.respond(req, json!({ "variables": vars }));
        Ok(())
    }

    fn evaluate(&mut self, req: &Value) -> Result<(), String> {
        let a = &req["arguments"];
        let expr = a["expression"].as_str().unwrap_or("").trim().to_string();
        let depth = (a["frameId"].as_i64().unwrap_or(1) - 1).max(0) as u32;
        // A variable (`$total`, `$items['a']`) in any frame; other expressions run in the top one.
        let simple = expr.starts_with('$') && !expr.contains([' ', '+', '-', '*', '/', '(', '.', '=', '?', ':', '<', '>', '!', '&', '|']);
        let xml = if simple || depth > 0 {
            self.call("property_get", &format!("-d {depth} -n {}", quote(&expr)), None)?
        } else {
            self.call("eval", "", Some(&expr))?
        };
        let doc = roxmltree::Document::parse(&xml).map_err(|e| e.to_string())?;
        if let Some(e) = error_message(&doc) {
            return Err(e);
        }
        let p = doc.root_element().children().find(|n| n.has_tag_name("property")).ok_or("no result")?;
        let v = self.variable(p, depth, 0);
        self.dap.respond(req, json!({ "result": v["value"], "type": v["type"], "variablesReference": v["variablesReference"] }));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_and_quoting() {
        assert_eq!(file_uri(r"C:\My Projects\a.php"), "file:///C:/My%20Projects/a.php");
        if cfg!(windows) {
            assert_eq!(uri_path("file:///C:/My%20Projects/a.php").as_deref(), Some(r"C:\My Projects\a.php"));
        }
        assert_eq!(uri_path("dbgp://1"), None);
        assert_eq!(quote("$items"), "$items");
        assert_eq!(quote("$items[\"a b\"]"), "\"$items[\\\"a b\\\"]\"");
        let mut packet = std::io::Cursor::new(b"5\0<a/>x\0".to_vec());
        assert_eq!(read_packet(&mut packet).as_deref(), Some("<a/>x"));
    }

    #[test]
    fn values() {
        let xml = r#"<response xmlns="urn:debugger_protocol_v1" command="context_get" transaction_id="4">
            <property name="$name" fullname="$name" type="string" size="8" encoding="base64"><![CDATA[R3JhcGh5bmU=]]></property>
            <property name="$total" fullname="$total" type="int"><![CDATA[6]]></property>
            <property name="$ok" fullname="$ok" type="bool"><![CDATA[1]]></property>
            <property name="$items" fullname="$items" type="array" children="1" numchildren="2"></property>
            <property name="$p" fullname="$p" type="object" classname="Point" children="1" numchildren="2"></property>
        </response>"#;
        assert_eq!(transaction_id(xml), Some(4));
        let doc = roxmltree::Document::parse(xml).unwrap();
        let shown: Vec<String> = doc.root_element().children().filter(|n| n.has_tag_name("property")).map(display).collect();
        assert_eq!(shown, ["\"Graphyne\"", "6", "true", "array(2)", "Point"]);
    }
}
