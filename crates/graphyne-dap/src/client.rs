//! DAP client: `Content-Length` framed JSON over stdio or a TCP socket.

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

pub struct DapClient {
    pub name: String,
    tx: flume::Sender<Value>,
    pending: Pending,
    seq: Arc<AtomicI64>,
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

        let label = name.to_string();
        if let Some(stderr) = child.stderr.take() {
            std::thread::Builder::new().name(format!("dap-{label}-stderr")).spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    log::debug!("[dap {label}] {line}");
                }
            })?;
        }

        let (reader, mut writer): (Box<dyn Read + Send>, Box<dyn Write + Send>) = match transport {
            Transport::Stdio => (Box::new(child.stdout.take().context("no stdout")?), Box::new(child.stdin.take().context("no stdin")?)),
            Transport::Tcp { port } => {
                let deadline = Instant::now() + Duration::from_secs(10);
                let stream = loop {
                    match std::net::TcpStream::connect(("127.0.0.1", port)) {
                        Ok(s) => break s,
                        Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
                        Err(e) => return Err(e).context("connecting to debug adapter"),
                    }
                };
                (Box::new(stream.try_clone()?), Box::new(stream))
            }
        };

        let (tx, out_rx) = flume::unbounded::<Value>();
        let (ev_tx, ev_rx) = flume::unbounded::<DapEvent>();
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let seq = Arc::new(AtomicI64::new(1));

        let label = name.to_string();
        std::thread::Builder::new().name(format!("dap-{label}-write")).spawn(move || {
            for msg in out_rx.iter() {
                let Ok(body) = serde_json::to_string(&msg) else { continue };
                if write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body).and_then(|_| writer.flush()).is_err() {
                    break;
                }
            }
        })?;

        {
            let label = name.to_string();
            let pending = pending.clone();
            let alive = alive.clone();
            let reply_tx = tx.clone();
            let seq = seq.clone();
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
                            if let Some(waiter) = pending.lock().remove(&id) {
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
                                let _ = ev_tx.send(ev);
                            }
                        }
                        // Reverse requests: `runInTerminal` goes to the UI (terminal tab);
                        // anything else (startDebugging…) is declined.
                        Some("request") if msg["command"] == "runInTerminal" => {
                            let a = &msg["arguments"];
                            let env = a["env"]
                                .as_object()
                                .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect())
                                .unwrap_or_default();
                            let args = a["args"].as_array().map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
                            let _ = ev_tx.send(DapEvent::RunInTerminal {
                                request_seq: msg["seq"].as_i64().unwrap_or(0),
                                title: a["title"].as_str().map(str::to_string),
                                cwd: a["cwd"].as_str().map(str::to_string),
                                args,
                                env,
                            });
                        }
                        Some("request") => {
                            let s = seq.fetch_add(1, Ordering::SeqCst);
                            let _ = reply_tx.send(json!({
                                "seq": s, "type": "response", "request_seq": msg["seq"], "success": false,
                                "command": msg["command"], "message": "not supported by Graphyne"
                            }));
                        }
                        _ => {}
                    }
                }
                alive.store(false, Ordering::SeqCst);
                for (_, waiter) in pending.lock().drain() {
                    let _ = waiter.send(Err("debug adapter exited".into()));
                }
                let _ = ev_tx.send(DapEvent::AdapterExited);
            })?;
        }

        Ok(Arc::new(Self {
            name: name.to_string(),
            tx,
            pending,
            seq,
            caps: RwLock::new(Value::Null),
            events: ev_rx,
            child: Mutex::new(Some(child)),
            alive,
        }))
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

    /// Send a request; resolves to the response `body` on any executor.
    pub fn request(&self, command: &str, arguments: Value) -> impl Future<Output = Result<Value>> + Send + 'static {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let (stx, srx) = flume::bounded(1);
        self.pending.lock().insert(seq, stx);
        let sent = self.tx.send(json!({ "seq": seq, "type": "request", "command": command, "arguments": arguments })).is_ok();
        let command = command.to_string();
        async move {
            if !sent {
                return Err(anyhow!("debug adapter is not running"));
            }
            match srx.recv_async().await {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(anyhow!("{command}: {e}")),
                Err(_) => Err(anyhow!("{command}: no response")),
            }
        }
    }

    /// `initialize` handshake; stores adapter capabilities.
    pub async fn initialize(self: Arc<Self>, adapter_id: &str) -> Result<()> {
        let caps = self
            .request(
                "initialize",
                json!({
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
                    "supportsProgressReporting": false,
                    "supportsInvalidatedEvent": false
                }),
            )
            .await?;
        *self.caps.write() = caps;
        Ok(())
    }

    /// Answer a reverse request (`runInTerminal`).
    pub fn respond(&self, request_seq: i64, command: &str, success: bool, body: Value) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let _ = self.tx.send(json!({ "seq": seq, "type": "response", "request_seq": request_seq, "success": success, "command": command, "body": body }));
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
