//! Debug adapters Graphyne implements itself, for debug engines that speak their own protocol
//! instead of DAP: Xdebug (PHP, DBGp) and the JVM (Java, Kotlin, Scala; JDWP). Graphyne starts itself as `graphyne --dap-bridge <kind>`
//! and the bridge speaks DAP on stdin / stdout like any other adapter, so the client side is
//! the same for every language.

pub mod dbgp;
pub mod jdwp;
pub mod jvm;

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

/// The command-line switch that turns Graphyne into a debug adapter.
pub const ARG: &str = "--dap-bridge";

/// Graphyne was started as one of its debug adapters: serve that session and return true (the
/// caller exits right after). Otherwise false.
pub fn serve_if_asked() -> bool {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some(ARG) {
        return false;
    }
    let kind = args.next().unwrap_or_default();
    let result = match kind.as_str() {
        "php" => dbgp::serve(),
        "jvm" => jvm::serve(),
        other => Err(format!("no debug bridge called \u{201C}{other}\u{201D}")),
    };
    if let Err(e) = result {
        eprintln!("graphyne debug bridge: {e}");
    }
    true
}

/// The client's side of a bridge: its messages come in on a thread reading stdin; responses,
/// events and reverse requests go out on stdout.
pub struct Dap {
    out: Mutex<std::io::Stdout>,
    seq: AtomicI64,
}

impl Dap {
    /// Start reading the client's messages: each goes to `deliver`, then `None` once the
    /// client has gone.
    pub fn stdio(deliver: impl Fn(Option<Value>) + Send + 'static) -> Self {
        std::thread::spawn(move || {
            let mut input = BufReader::new(std::io::stdin());
            while let Some(msg) = read_message(&mut input) {
                if !msg.is_null() {
                    deliver(Some(msg));
                }
            }
            deliver(None);
        });
        Self { out: Mutex::new(std::io::stdout()), seq: AtomicI64::new(1) }
    }

    fn send(&self, mut msg: Value) {
        msg["seq"] = json!(self.seq.fetch_add(1, Ordering::SeqCst));
        let body = msg.to_string();
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let _ = write!(out, "Content-Length: {}\r\n\r\n{}", body.len(), body);
        let _ = out.flush();
    }

    pub fn respond(&self, req: &Value, body: Value) {
        self.send(json!({ "type": "response", "request_seq": req["seq"], "success": true, "command": req["command"], "body": body }));
    }

    pub fn fail(&self, req: &Value, message: &str) {
        self.send(json!({ "type": "response", "request_seq": req["seq"], "success": false, "command": req["command"], "message": message }));
    }

    pub fn event(&self, event: &str, body: Value) {
        self.send(json!({ "type": "event", "event": event, "body": body }));
    }

    /// A reverse request (`runInTerminal`); its response arrives like any other message.
    pub fn request(&self, command: &str, arguments: Value) {
        self.send(json!({ "type": "request", "command": command, "arguments": arguments }));
    }

    /// Text for the debug console.
    pub fn output(&self, category: &str, text: &str) {
        self.event("output", json!({ "category": category, "output": text }));
    }
}

/// One `Content-Length` framed message; `Null` for one that isn't JSON, `None` at the end.
fn read_message(input: &mut impl BufRead) -> Option<Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; len?];
    input.read_exact(&mut body).ok()?;
    Some(serde_json::from_slice(&body).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framed_messages() {
        let text = "Content-Length: 13\r\n\r\n{\"seq\":1,\"a\":2}".replace("13", &"{\"seq\":1,\"a\":2}".len().to_string());
        let mut input = std::io::Cursor::new(format!("{text}Content-Length: 4\r\n\r\nnope"));
        assert_eq!(read_message(&mut input).unwrap()["a"], 2);
        assert!(read_message(&mut input).unwrap().is_null());
        assert!(read_message(&mut input).is_none());
    }
}
