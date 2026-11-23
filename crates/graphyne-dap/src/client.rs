//! DAP client: `Content-Length` framed JSON over stdio or a TCP socket.
//!
//! Adapters that serve several sessions over TCP (js-debug) ask for child sessions with the
//! `startDebugging` reverse request: the client opens another connection to the same port,
//! repeats `initialize` and sends the given `launch` / `attach` there. From then on requests go
//! to the newest child session while it lives, and its events arrive in the same stream, so the
//! UI sees one session (it configures breakpoints again on the child's `initialized`).

use anyhow::{Context, Result, anyhow};
use parking_lot::{Mutex, RwLock};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

/// Events pushed by the adapter.
#[derive(Clone, Debug)]
pub enum DapEvent {
    Initialized,
    Stopped { reason: String, thread_id: Option<i64>, description: Option<String>, text: Option<String> },
    Continued { thread_id: Option<i64> },
    Output { category: String, text: String },
    Thread { reason: String, thread_id: i64 },
    Breakpoint { reason: String, body: Value },
    Terminated,
    Exited { code: i64 },
    /// The adapter process closed its stream.
    AdapterExited,
    /// Reverse request: start the debuggee in a terminal. Answer with [`DapClient::respond`].
    RunInTerminal { request_seq: i64, title: Option<String>, cwd: Option<String>, args: Vec<String>, env: Vec<(String, String)> },
}

/// How to talk to the adapter process.
#[derive(Clone, Debug)]
pub enum Transport {
    Stdio,
    /// The adapter listens on a TCP port (e.g. `dlv dap --listen=127.0.0.1:PORT`).
    Tcp { port: u16 },
}

type Pending = Arc<Mutex<HashMap<i64, flume::Sender<Result<Value, String>>>>>;

/// One connection to the adapter: its outgoing queue, waiting requests and numbering.
#[derive(Clone)]
struct Conn {
    tx: flume::Sender<Value>,
    pending: Pending,
    seq: Arc<AtomicI64>,
}

impl Conn {
    fn same(&self, other: &Conn) -> bool {
        Arc::ptr_eq(&self.pending, &other.pending)
    }

    /// Send a request; the receiver gets the response `body` or the error message.
    fn send(&self, command: &str, arguments: Value) -> Option<flume::Receiver<Result<Value, String>>> {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let (stx, srx) = flume::bounded(1);
        self.pending.lock().insert(seq, stx);
        self.tx.send(json!({ "seq": seq, "type": "request", "command": command, "arguments": arguments })).ok().map(|_| srx)
    }

    fn respond(&self, request_seq: i64, command: &str, success: bool, body: Value) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let mut msg = json!({ "seq": seq, "type": "response", "request_seq": request_seq, "success": success, "command": command, "body": body });
        if !success {
            msg["message"] = json!("not supported by Graphyne");
        }
        let _ = self.tx.send(msg);
    }
}

/// What every connection of one adapter shares.
struct Shared {
    name: String,
    port: Option<u16>,
    events: flume::Sender<DapEvent>,
    /// Live child sessions, newest last: requests go to the last one.
    children: Mutex<Vec<Conn>>,
    /// The `initialize` arguments, repeated for child sessions.
    init_args: Mutex<Value>,
    /// Reverse requests waiting for the UI's answer: our id → (connection, its `seq`).
    reverse: Mutex<HashMap<i64, (Conn, i64)>>,
    next_reverse: AtomicI64,
}

pub struct DapClient {
    pub name: String,
    base: Conn,
    shared: Arc<Shared>,
    caps: RwLock<Value>,
    events: flume::Receiver<DapEvent>,
    child: Mutex<Option<Child>>,
    alive: Arc<AtomicBool>,
}

fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = Some(v.trim().parse().context("bad Content-Length")?);
        }
    }
    let mut body = vec![0u8; len.unwrap_or(0)];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

fn parse_event(msg: &Value) -> Option<DapEvent> {
    let body = &msg["body"];
    Some(match msg["event"].as_str()? {
        "initialized" => DapEvent::Initialized,
        "stopped" => DapEvent::Stopped {
            reason: body["reason"].as_str().unwrap_or("pause").to_string(),
            thread_id: body["threadId"].as_i64(),
            description: body["description"].as_str().map(str::to_string),
            text: body["text"].as_str().map(str::to_string),
        },
        "continued" => DapEvent::Continued { thread_id: body["threadId"].as_i64() },
        "output" => DapEvent::Output {
            category: body["category"].as_str().unwrap_or("console").to_string(),
            text: body["output"].as_str().unwrap_or_default().to_string(),
        },
        "thread" => DapEvent::Thread { reason: body["reason"].as_str().unwrap_or_default().to_string(), thread_id: body["threadId"].as_i64().unwrap_or(0) },
        "breakpoint" => DapEvent::Breakpoint { reason: body["reason"].as_str().unwrap_or_default().to_string(), body: body.clone() },
        "terminated" => DapEvent::Terminated,
        "exited" => DapEvent::Exited { code: body["exitCode"].as_i64().unwrap_or(0) },
        _ => return None,
    })
}

fn connect_tcp(port: u16) -> Result<std::net::TcpStream> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => return Ok(s),
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e).context("connecting to debug adapter"),
        }
    }
}

/// Start the writer and reader threads of one connection. `alive` is set to false when the
/// main connection (`child == false`) closes.
fn open_conn(shared: &Arc<Shared>, reader: Box<dyn Read + Send>, mut writer: Box<dyn Write + Send>, child: bool, alive: Option<Arc<AtomicBool>>) -> Result<Conn> {
    let (tx, out_rx) = flume::unbounded::<Value>();
    let conn = Conn { tx, pending: Arc::default(), seq: Arc::new(AtomicI64::new(1)) };
    let label = if child { format!("{}-child", shared.name) } else { shared.name.clone() };
    std::thread::Builder::new().name(format!("dap-{label}-write")).spawn(move || {
        for msg in out_rx.iter() {
            let Ok(body) = serde_json::to_string(&msg) else { continue };
            if write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body).and_then(|_| writer.flush()).is_err() {
                break;
            }
        }
    })?;
    let (shared, me) = (shared.clone(), conn.clone());
    std::thread::Builder::new().name(format!("dap-{label}-read")).spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let msg = match read_message(&mut reader) {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => {
                    log::warn!("[dap {label}] protocol error: {e:#}");
                    break;
                }
            };
            match msg["type"].as_str() {
                Some("response") => {
                    let Some(id) = msg["request_seq"].as_i64() else { continue };
                    if let Some(waiter) = me.pending.lock().remove(&id) {
                        let result = if msg["success"].as_bool().unwrap_or(false) {
                            Ok(msg.get("body").cloned().unwrap_or(Value::Null))
                        } else {
                            let detail = msg["body"]["error"]["format"].as_str().or_else(|| msg["message"].as_str()).unwrap_or("request failed");
                            Err(detail.to_string())
                        };
                        let _ = waiter.send(result);
                    }
                }
                Some("event") => {
                    if let Some(ev) = parse_event(&msg) {
                        let _ = shared.events.send(ev);
                    }
                }
                // Reverse requests: `runInTerminal` goes to the UI (terminal tab), child
                // sessions are opened here, anything else is declined.
                Some("request") if msg["command"] == "runInTerminal" => {
                    let a = &msg["arguments"];
                    let env = a["env"]
                        .as_object()
                        .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect())
                        .unwrap_or_default();
                    let args = a["args"].as_array().map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
                    let id = shared.next_reverse.fetch_add(1, Ordering::SeqCst);
                    shared.reverse.lock().insert(id, (me.clone(), msg["seq"].as_i64().unwrap_or(0)));
                    let _ = shared.events.send(DapEvent::RunInTerminal {
                        request_seq: id,
                        title: a["title"].as_str().map(str::to_string),
                        cwd: a["cwd"].as_str().map(str::to_string),
                        args,
                        env,
                    });
                }
                Some("request") if msg["command"] == "startDebugging" && shared.port.is_some() => {
                    me.respond(msg["seq"].as_i64().unwrap_or(0), "startDebugging", true, json!({}));
                    let (shared, a) = (shared.clone(), msg["arguments"].clone());
                    std::thread::spawn(move || {
                        if let Err(e) = start_child(&shared, &a) {
                            log::warn!("[dap {}] child session: {e:#}", shared.name);
                            let _ = shared.events.send(DapEvent::Output { category: "stderr".into(), text: format!("Could not start the debug session: {e:#}\n") });
                        }
                    });
                }
                Some("request") => me.respond(msg["seq"].as_i64().unwrap_or(0), msg["command"].as_str().unwrap_or_default(), false, Value::Null),
                _ => {}
            }
        }
        for (_, waiter) in me.pending.lock().drain() {
            let _ = waiter.send(Err("debug adapter exited".into()));
        }
        if child {
            shared.children.lock().retain(|c| !c.same(&me));
        } else {
            if let Some(alive) = alive {
                alive.store(false, Ordering::SeqCst);
            }
            let _ = shared.events.send(DapEvent::AdapterExited);
        }
    })?;
    Ok(conn)
}

/// Open the child session asked for by `startDebugging` (`arguments` of that request).
fn start_child(shared: &Arc<Shared>, arguments: &Value) -> Result<()> {
    let port = shared.port.context("the adapter has no TCP port")?;
    let stream = connect_tcp(port)?;
    let conn = open_conn(shared, Box::new(stream.try_clone()?), Box::new(stream), true, None)?;
    // Route requests here first, so the UI's answer to this session's `initialized`
    // (breakpoints, `configurationDone`) arrives in it.
    shared.children.lock().push(conn.clone());
    let init = shared.init_args.lock().clone();
    let reply = conn.send("initialize", init).context("the child connection closed")?;
    match reply.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(anyhow!("initialize: {e}")),
        Err(_) => return Err(anyhow!("initialize: no response")),
    }
    let request = if arguments["request"] == "attach" { "attach" } else { "launch" };
    // Answered only after `configurationDone`: not waited for.
    let _ = conn.send(request, arguments["configuration"].clone());
    Ok(())
}

impl DapClient {
    /// Start the adapter. Blocking (process creation, TCP connect): call from a background
    /// executor.
    pub fn spawn(name: &str, program: &Path, args: &[String], env: &[(String, String)], cwd: &Path, transport: Transport) -> Result<Arc<Self>> {
        let mut cmd = Command::new(program);
        cmd.args(args).current_dir(cwd).stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        if matches!(transport, Transport::Stdio) {
            cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null()).stdout(Stdio::piped());
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().with_context(|| format!("starting debug adapter {}", program.display()))?;
        #[cfg(windows)]
        graphyne_core::process::end_with_graphyne(&child);

        let label = name.to_string();
        if let Some(stderr) = child.stderr.take() {
            std::thread::Builder::new().name(format!("dap-{label}-stderr")).spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    log::debug!("[dap {label}] {line}");
                }
            })?;
        }

        let port = match transport {
            Transport::Tcp { port } => Some(port),
            Transport::Stdio => None,
        };
        let (reader, writer): (Box<dyn Read + Send>, Box<dyn Write + Send>) = match transport {
            Transport::Stdio => (Box::new(child.stdout.take().context("no stdout")?), Box::new(child.stdin.take().context("no stdin")?)),
            Transport::Tcp { port } => {
                let stream = connect_tcp(port)?;
                (Box::new(stream.try_clone()?), Box::new(stream))
            }
        };

        let (ev_tx, ev_rx) = flume::unbounded::<DapEvent>();
        let shared = Arc::new(Shared {
            name: name.to_string(),
            port,
            events: ev_tx,
            children: Mutex::new(Vec::new()),
            init_args: Mutex::new(Value::Null),
            reverse: Mutex::new(HashMap::new()),
            next_reverse: AtomicI64::new(1),
        });
        let alive = Arc::new(AtomicBool::new(true));
        let base = open_conn(&shared, reader, writer, false, Some(alive.clone()))?;

        Ok(Arc::new(Self { name: name.to_string(), base, shared, caps: RwLock::new(Value::Null), events: ev_rx, child: Mutex::new(Some(child)), alive }))
    }

    pub fn events(&self) -> flume::Receiver<DapEvent> {
        self.events.clone()
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn capabilities(&self) -> Value {
        self.caps.read().clone()
    }

    pub fn supports(&self, cap: &str) -> bool {
        self.caps.read()[cap].as_bool().unwrap_or(false)
    }

    /// Where requests go: the newest child session, else the main connection.
    fn target(&self) -> Conn {
        self.shared.children.lock().last().cloned().unwrap_or_else(|| self.base.clone())
    }

    /// Send a request; resolves to the response `body` on any executor.
    pub fn request(&self, command: &str, arguments: Value) -> impl Future<Output = Result<Value>> + Send + 'static {
        let reply = self.target().send(command, arguments);
        let command = command.to_string();
        async move {
            let Some(reply) = reply else { return Err(anyhow!("debug adapter is not running")) };
            match reply.recv_async().await {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(anyhow!("{command}: {e}")),
                Err(_) => Err(anyhow!("{command}: no response")),
            }
        }
    }

    /// `initialize` handshake; stores adapter capabilities.
    pub async fn initialize(self: Arc<Self>, adapter_id: &str) -> Result<()> {
        let args = json!({
            "clientID": "graphyne",
            "clientName": "Graphyne",
            "adapterID": adapter_id,
            "locale": "en-US",
            "linesStartAt1": true,
            "columnsStartAt1": true,
            "pathFormat": "path",
            "supportsVariableType": true,
            "supportsVariablePaging": false,
            "supportsRunInTerminalRequest": true,
            "supportsStartDebuggingRequest": true,
            "supportsProgressReporting": false,
            "supportsInvalidatedEvent": false
        });
        *self.shared.init_args.lock() = args.clone();
        let caps = self.request("initialize", args).await?;
        *self.caps.write() = caps;
        Ok(())
    }

    /// Answer a reverse request (`runInTerminal`) on the connection that asked.
    pub fn respond(&self, request_seq: i64, command: &str, success: bool, body: Value) {
        if let Some((conn, seq)) = self.shared.reverse.lock().remove(&request_seq) {
            conn.respond(seq, command, success, body);
        }
    }

    pub fn kill(&self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.kill();
        }
    }
}

impl Drop for DapClient {
    fn drop(&mut self) {
        self.kill();
    }
}
