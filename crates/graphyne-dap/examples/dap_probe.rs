//! Protocol smoke test: `cargo run -p graphyne-dap --example dap_probe -- <file> <root> <breakpoint-line-0based>`
use graphyne_dap::{DapClient, DapEvent, adapters, model};
use serde_json::json;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

struct W(std::thread::Thread);
impl Wake for W {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}
fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let waker = Waker::from(Arc::new(W(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park_timeout(Duration::from_millis(20));
    }
}

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let file = PathBuf::from(&a[1]);
    let root = PathBuf::from(&a[2]);
    let bp: usize = a[3].parse()?;
    let r = adapters::resolve(&file, &root).map_err(anyhow::Error::msg)?;
    println!("adapter {} -> {}", r.name, r.program.display());
    let client = DapClient::spawn(r.name, &r.program, &r.args, &r.env, &r.cwd, r.transport.clone())?;
    block_on(client.clone().initialize(r.adapter_id))?;
    println!("caps: configurationDone={} ", client.supports("supportsConfigurationDoneRequest"));
    let events = client.events();
    let launch = client.request("launch", r.launch.clone());
    let launch = std::thread::spawn(move || block_on(launch));
    let mut steps = 0;
    while let Ok(ev) = events.recv_timeout(Duration::from_secs(20)) {
        match ev {
            DapEvent::Initialized => {
                let b = block_on(client.request("setBreakpoints", json!({ "source": { "path": file }, "breakpoints": model::source_breakpoints(&[model::BreakpointSpec::at(bp)]) })))?;
                println!("breakpoints: {}", b["breakpoints"]);
                let _ = block_on(client.request("setExceptionBreakpoints", json!({ "filters": [] })));
                block_on(client.request("configurationDone", json!({})))?;
            }
            DapEvent::Stopped { reason, thread_id, .. } => {
                let tid = thread_id.unwrap_or(1);
                let st = model::parse_stack(&block_on(client.request("stackTrace", json!({ "threadId": tid, "startFrame": 0, "levels": 20 })))?);
                println!("STOPPED ({reason}) at {}:{} in {}", st[0].path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(), st[0].line + 1, st[0].name);
                let scopes = model::parse_scopes(&block_on(client.request("scopes", json!({ "frameId": st[0].id })))?);
                let vars = model::parse_variables(&block_on(client.request("variables", json!({ "variablesReference": scopes[0].reference })))?);
                println!("  {}: {:?}", scopes[0].name, vars.iter().map(|v| format!("{}={}", v.name, v.value)).collect::<Vec<_>>());
                steps += 1;
                let cmd = if steps < 3 { "next" } else { "continue" };
                block_on(client.request(cmd, json!({ "threadId": tid })))?;
            }
            DapEvent::Output { category, text } => print!("[{category}] {text}"),
            DapEvent::Exited { code } => println!("exited with {code}"),
            DapEvent::Terminated => {
                println!("terminated");
                break;
            }
            other => println!("event {other:?}"),
        }
    }
    println!("launch response: {:?}", launch.join().unwrap().map(|_| "ok"));
    let _ = block_on(client.request("disconnect", json!({ "terminateDebuggee": true })));
    Ok(())
}
