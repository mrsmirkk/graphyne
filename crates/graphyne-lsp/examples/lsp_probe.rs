//! Protocol smoke test: `cargo run -p graphyne-lsp --example lsp_probe -- <server> <root> <file> <line> <col>`
use graphyne_lsp::convert::path_to_uri;
use graphyne_lsp::{LspClient, ServerEvent, model};
use serde_json::json;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

struct ThreadWaker(std::thread::Thread);
impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park_timeout(Duration::from_millis(50));
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let server = graphyne_lsp::servers::which(&args[1]).expect("server on PATH");
    let root = PathBuf::from(&args[2]);
    let file = PathBuf::from(&args[3]);
    let (line, col): (u32, u32) = (args[4].parse()?, args[5].parse()?);
    let extra: Vec<&str> = args.iter().skip(6).map(String::as_str).collect();
    let t = Instant::now();
    let client = LspClient::spawn(&args[1], &server, &extra, &root)?;
    block_on(client.clone().initialize())?;
    println!("initialized in {:?}; sync={} triggers={:?}", t.elapsed(), client.sync_kind(), client.completion_triggers());
    let text = std::fs::read_to_string(&file)?;
    let uri = path_to_uri(&file);
    client.notify("textDocument/didOpen", json!({ "textDocument": { "uri": uri, "languageId": args.get(7).map(String::as_str).unwrap_or("rust"), "version": 1, "text": text } }));
    let pos = json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": col } });
    let events = client.events();
    // Give the server time to load the project.
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        match events.recv_timeout(Duration::from_millis(500)) {
            Ok(ServerEvent::Diagnostics { path, diagnostics, .. }) => println!("diagnostics for {}: {:?}", path.display(), diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()),
            Ok(ServerEvent::Progress { title, message, kind, .. }) => println!("progress {kind:?} {title:?} {message:?}"),
            Ok(other) => println!("event {other:?}"),
            Err(_) => {
                let Ok(hover) = block_on(client.request("textDocument/hover", pos.clone())) else { continue };
                if let Some(md) = model::parse_hover(&hover) {
                    println!("HOVER:\n{md}");
                    break;
                }
            }
        }
    }
    let completion = block_on(client.request("textDocument/completion", pos.clone()))?;
    let (incomplete, items) = model::parse_completions(&completion);
    println!("completion: incomplete={incomplete} {} items, first: {:?}", items.len(), items.iter().take(5).map(|i| &i.label).collect::<Vec<_>>());
    let hints = block_on(client.request("textDocument/inlayHint", json!({ "textDocument": { "uri": uri }, "range": { "start": {"line":0,"character":0}, "end": {"line":200,"character":0} } })))?;
    println!("inlay hints: {:?}", model::parse_inlay_hints(&hints).iter().map(|h| (h.pos.line, h.label.clone())).collect::<Vec<_>>());
    client.shutdown();
    Ok(())
}
