//! JSON-RPC 2.0 language-server client over stdio.

use crate::convert::{path_to_uri, uri_to_path};
use crate::model::{Diag, parse_diagnostics, parse_workspace_edit, FileEdits};
use anyhow::{Context, Result, anyhow};
use parking_lot::{Mutex, RwLock};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressKind {
    Begin,
    Report,
    End,
}

/// Everything the server pushes to us.
#[derive(Debug)]
pub enum ServerEvent {
    Diagnostics { path: PathBuf, version: Option<i64>, diagnostics: Vec<Diag> },
    Progress { token: String, kind: ProgressKind, title: Option<String>, message: Option<String>, percentage: Option<u32> },
    /// `window/showMessage` (1 = error … 4 = log).
    Message { level: u8, text: String },
    /// `workspace/applyEdit`: apply, then answer with [`LspClient::respond`].
    ApplyEdit { id: Value, edits: Vec<FileEdits> },
    Exited,
}

type Pending = Arc<Mutex<HashMap<i64, flume::Sender<Result<Value, String>>>>>;

pub struct LspClient {
    pub name: String,
    pub root: PathBuf,
    tx: flume::Sender<Value>,
    pending: Pending,
    next_id: AtomicI64,
    caps: RwLock<Value>,
    events: flume::Receiver<ServerEvent>,
    child: Mutex<Option<Child>>,
    alive: Arc<AtomicBool>,
}

fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            if content_length.is_some() {
                break;
            }
            continue;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            content_length = Some(v.trim().parse().context("bad Content-Length")?);
        }
    }
    let mut body = vec![0u8; content_length.unwrap_or(0)];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

impl LspClient {
    /// Spawn the server process and its I/O threads. Blocking (process creation):
    /// call from a background executor.
    pub fn spawn(name: &str, program: &Path, args: &[&str], root: &Path) -> Result<Arc<Self>> {
        let mut cmd = Command::new(program);
        cmd.args(args).current_dir(root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().with_context(|| format!("starting {}", program.display()))?;
        let mut stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;

        let (tx, out_rx) = flume::unbounded::<Value>();
        let (ev_tx, ev_rx) = flume::unbounded::<ServerEvent>();
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));

        let label = name.to_string();
        std::thread::Builder::new().name(format!("lsp-{label}-write")).spawn(move || {
            for msg in out_rx.iter() {
                let Ok(body) = serde_json::to_string(&msg) else { continue };
                if write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        })?;

        let label = name.to_string();
        std::thread::Builder::new().name(format!("lsp-{label}-stderr")).spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log::debug!("[{label}] {line}");
            }
        })?;

        {
            let label = name.to_string();
            let pending = pending.clone();
            let alive = alive.clone();
            let reply_tx = tx.clone();
            let root = root.to_path_buf();
            std::thread::Builder::new().name(format!("lsp-{label}-read")).spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let msg = match read_message(&mut reader) {
                        Ok(Some(m)) => m,
                        Ok(None) => break,
                        Err(e) => {
                            log::warn!("[{label}] protocol error: {e:#}");
                            break;
                        }
                    };
                    let method = msg.get("method").and_then(Value::as_str).map(str::to_string);
                    let id = msg.get("id").cloned();
                    match (method, id) {
                        // Response to one of our requests.
                        (None, Some(id)) => {
                            let Some(id) = id.as_i64() else { continue };
                            if let Some(waiter) = pending.lock().remove(&id) {
                                let result = match msg.get("error") {
                                    Some(err) => Err(err.get("message").and_then(Value::as_str).unwrap_or("request failed").to_string()),
                                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                                };
                                let _ = waiter.send(result);
                            }
                        }
                        // Server → client request: we must always answer.
                        (Some(method), Some(id)) => {
                            let params = msg.get("params").cloned().unwrap_or(Value::Null);
                            let result = match method.as_str() {
                                "workspace/configuration" => {
                                    // Python servers ask for section "python": point them at the project's interpreter.
                                    let items = params.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
                                    let venv = graphyne_core::python_env::interpreter(&root);
                                    Some(Value::Array(
                                        items
                                            .iter()
                                            .map(|item| match (item.get("section").and_then(Value::as_str), &venv) {
                                                (Some("python"), Some(python)) => json!({ "pythonPath": python }),
                                                _ => Value::Null,
                                            })
                                            .collect(),
                                    ))
                                }
                                "workspace/workspaceFolders" => Some(json!([{ "uri": path_to_uri(&root), "name": crate::convert::path_key(&root) }])),
                                "workspace/applyEdit" => {
                                    let edits = parse_workspace_edit(params.get("edit").unwrap_or(&Value::Null));
                                    let _ = ev_tx.send(ServerEvent::ApplyEdit { id: id.clone(), edits });
                                    None // answered by the UI via `respond`
                                }
                                _ => Some(Value::Null),
                            };
                            if let Some(result) = result {
                                let _ = reply_tx.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
                            }
                        }
                        // Notification.
                        (Some(method), None) => {
                            let params = msg.get("params").cloned().unwrap_or(Value::Null);
                            match method.as_str() {
                                "textDocument/publishDiagnostics" => {
                                    if let Some((uri, version, diagnostics)) = parse_diagnostics(&params) {
                                        if let Some(path) = uri_to_path(&uri) {
                                            let _ = ev_tx.send(ServerEvent::Diagnostics { path, version, diagnostics });
                                        }
                                    }
                                }
                                "$/progress" => {
                                    let token = match &params["token"] {
                                        Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    };
                                    let v = &params["value"];
                                    let kind = match v["kind"].as_str() {
                                        Some("begin") => ProgressKind::Begin,
                                        Some("end") => ProgressKind::End,
                                        _ => ProgressKind::Report,
                                    };
                                    let _ = ev_tx.send(ServerEvent::Progress {
                                        token,
                                        kind,
                                        title: v["title"].as_str().map(str::to_string),
                                        message: v["message"].as_str().map(str::to_string),
                                        percentage: v["percentage"].as_u64().map(|p| p as u32),
                                    });
                                }
                                "window/showMessage" => {
                                    let _ = ev_tx.send(ServerEvent::Message {
                                        level: params["type"].as_u64().unwrap_or(3) as u8,
                                        text: params["message"].as_str().unwrap_or_default().to_string(),
                                    });
                                }
                                "window/logMessage" => log::debug!("[{label}] {}", params["message"].as_str().unwrap_or_default()),
                                _ => {}
                            }
                        }
                        (None, None) => {}
                    }
                }
                alive.store(false, Ordering::SeqCst);
                for (_, waiter) in pending.lock().drain() {
                    let _ = waiter.send(Err("language server exited".into()));
                }
                let _ = ev_tx.send(ServerEvent::Exited);
            })?;
        }

        Ok(Arc::new(Self {
            name: name.to_string(),
            root: root.to_path_buf(),
            tx,
            pending,
            next_id: AtomicI64::new(1),
            caps: RwLock::new(Value::Null),
            events: ev_rx,
            child: Mutex::new(Some(child)),
            alive,
        }))
    }

    pub fn events(&self) -> flume::Receiver<ServerEvent> {
        self.events.clone()
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Send a request; the returned future resolves on any executor.
    pub fn request(&self, method: &str, params: Value) -> impl Future<Output = Result<Value>> + Send + 'static {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (stx, srx) = flume::bounded(1);
        self.pending.lock().insert(id, stx);
        let sent = self.tx.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).is_ok();
        let method = method.to_string();
        async move {
            if !sent {
                return Err(anyhow!("language server is not running"));
            }
            match srx.recv_async().await {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(anyhow!("{method}: {e}")),
                Err(_) => Err(anyhow!("{method}: no response")),
            }
        }
    }

    pub fn notify(&self, method: &str, params: Value) {
        let _ = self.tx.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    pub fn respond(&self, id: Value, result: Value) {
        let _ = self.tx.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    /// `initialize` + `initialized` handshake.
    pub async fn initialize(self: Arc<Self>) -> Result<()> {
        let root_uri = path_to_uri(&self.root);
        let name = self.root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "Graphyne", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": root_uri,
            "rootPath": self.root.to_string_lossy(),
            "workspaceFolders": [{ "uri": root_uri, "name": name }],
            "capabilities": client_capabilities(),
        });
        let result = self.request("initialize", params).await?;
        *self.caps.write() = result.get("capabilities").cloned().unwrap_or(Value::Null);
        self.notify("initialized", json!({}));
        Ok(())
    }

    pub fn capabilities(&self) -> Value {
        self.caps.read().clone()
    }

    fn cap(&self, path: &[&str]) -> Value {
        let caps = self.caps.read();
        let mut v = &*caps;
        for p in path {
            v = &v[*p];
        }
        v.clone()
    }

    /// A provider is advertised if present and not `false`.
    pub fn supports(&self, provider: &str) -> bool {
        !matches!(self.cap(&[provider]), Value::Null | Value::Bool(false))
    }

    fn string_list(&self, path: &[&str]) -> Vec<String> {
        self.cap(path).as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default()
    }

    pub fn completion_triggers(&self) -> Vec<String> {
        self.string_list(&["completionProvider", "triggerCharacters"])
    }

    pub fn signature_triggers(&self) -> Vec<String> {
        let mut v = self.string_list(&["signatureHelpProvider", "triggerCharacters"]);
        v.extend(self.string_list(&["signatureHelpProvider", "retriggerCharacters"]));
        v
    }

    pub fn completion_resolve(&self) -> bool {
        self.cap(&["completionProvider", "resolveProvider"]).as_bool().unwrap_or(false)
    }

    pub fn code_action_resolve(&self) -> bool {
        self.cap(&["codeActionProvider", "resolveProvider"]).as_bool().unwrap_or(false)
    }

    /// Full (1) or incremental (2) document sync.
    pub fn sync_kind(&self) -> u64 {
        match self.cap(&["textDocumentSync"]) {
            Value::Number(n) => n.as_u64().unwrap_or(1),
            v => v["change"].as_u64().unwrap_or(1),
        }
    }

    /// Graceful shutdown (fire and forget); the process is killed on drop anyway.
    pub fn shutdown(&self) {
        let _ = self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.kill();
        }
    }
}

fn client_capabilities() -> Value {
    json!({
        "general": { "positionEncodings": ["utf-16"] },
        // rust-analyzer: code-action edits may carry `$0` / `${0:name}` caret markers.
        "experimental": { "snippetTextEdit": true },
        "window": { "workDoneProgress": true, "showMessage": {} },
        "workspace": {
            "configuration": true,
            "workspaceFolders": true,
            "applyEdit": true,
            "workspaceEdit": { "documentChanges": true },
            "didChangeWatchedFiles": { "dynamicRegistration": false }
        },
        "textDocument": {
            "synchronization": { "didSave": true, "willSave": false, "dynamicRegistration": false },
            "completion": {
                "contextSupport": true,
                "completionItem": {
                    "snippetSupport": true,
                    "labelDetailsSupport": true,
                    "documentationFormat": ["markdown", "plaintext"],
                    "resolveSupport": { "properties": ["documentation", "detail", "additionalTextEdits"] },
                    "insertReplaceSupport": false,
                    "deprecatedSupport": true
                },
                "completionItemKind": { "valueSet": (1..=25).collect::<Vec<u32>>() }
            },
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "signatureHelp": {
                "contextSupport": true,
                "signatureInformation": {
                    "documentationFormat": ["markdown", "plaintext"],
                    "parameterInformation": { "labelOffsetSupport": true },
                    "activeParameterSupport": true
                }
            },
            "definition": { "linkSupport": true },
            "declaration": { "linkSupport": true },
            "references": {},
            "publishDiagnostics": { "relatedInformation": false, "versionSupport": true, "codeDescriptionSupport": true, "dataSupport": true },
            "inlayHint": { "dynamicRegistration": false },
            "codeAction": {
                "codeActionLiteralSupport": {
                    "codeActionKind": { "valueSet": ["", "quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite", "source", "source.organizeImports", "source.fixAll"] }
                },
                "isPreferredSupport": true,
                "disabledSupport": true,
                "dataSupport": true,
                "resolveSupport": { "properties": ["edit"] }
            }
        }
    })
}
