//! Project-wide language-server registry.
//!
//! Servers are discovered and started lazily on background executors (one instance per
//! server per project). Their event streams are pumped back onto the UI thread, where
//! diagnostics and progress are stored and observers (editors, status bar) are notified.

use gpui::{AppContext, Context, EventEmitter, Task};
use graphyne_core::LanguageId;
use graphyne_lsp::model::{Diag, FileEdits};
use graphyne_lsp::{LspClient, ProgressKind, ServerEvent};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub enum LspStoreEvent {
    Notify(String, bool),
    ApplyEdit { client: Arc<LspClient>, id: Value, edits: Vec<FileEdits> },
}

#[derive(Clone)]
enum Slot {
    Starting,
    Running(Arc<LspClient>),
    Missing { program: &'static str, hint: &'static str },
    Failed(String),
    NoServer,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ServerStatus {
    /// No server is defined for this language.
    None,
    Missing { program: &'static str, hint: &'static str },
    Starting,
    Indexing { server: String, message: String },
    Ready { server: String },
    Failed { server: String, error: String },
}

pub struct LspStore {
    root: PathBuf,
    slots: HashMap<LanguageId, Slot>,
    diagnostics: HashMap<String, Arc<Vec<Diag>>>,
    progress: HashMap<(String, String), String>,
    _pumps: Vec<Task<()>>,
}

impl EventEmitter<LspStoreEvent> for LspStore {}

impl LspStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root, slots: HashMap::new(), diagnostics: HashMap::new(), progress: HashMap::new(), _pumps: Vec::new() }
    }

    /// Make sure a server for `lang` is running or starting.
    pub fn ensure(&mut self, lang: LanguageId, cx: &mut Context<Self>) {
        if self.slots.contains_key(&lang) {
            return;
        }
        if graphyne_lsp::servers::specs_for(lang).is_empty() {
            self.slots.insert(lang, Slot::NoServer);
            return;
        }
        // Languages that share a server (TS/TSX/JS, C/C++) reuse a running instance.
        let names: Vec<&str> = graphyne_lsp::servers::specs_for(lang).iter().map(|s| s.name).collect();
        if let Some(existing) = self.slots.values().find(|s| matches!(s, Slot::Running(c) if names.contains(&c.name.as_str()))).cloned() {
            self.slots.insert(lang, existing);
            cx.notify();
            return;
        }
        self.slots.insert(lang, Slot::Starting);
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let started = cx
                .background_spawn(async move {
                    let (spec, path) = match graphyne_lsp::servers::find(lang) {
                        Ok(found) => found,
                        Err(Some(spec)) => return Err(Slot::Missing { program: spec.program, hint: spec.install_hint }),
                        Err(None) => return Err(Slot::NoServer),
                    };
                    let client = LspClient::spawn(spec.name, &path, spec.args, &root).map_err(|e| Slot::Failed(format!("{e:#}")))?;
                    client.clone().initialize().await.map_err(|e| Slot::Failed(format!("{e:#}")))?;
                    Ok(client)
                })
                .await;
            this.update(cx, |this, cx| {
                match started {
                    Ok(client) => {
                        log::info!("language server {} ready", client.name);
                        this.slots.insert(lang, Slot::Running(client.clone()));
                        this.pump(client, cx);
                    }
                    Err(slot) => {
                        if let Slot::Failed(err) = &slot {
                            cx.emit(LspStoreEvent::Notify(format!("Language server failed to start: {err}"), true));
                        }
                        this.slots.insert(lang, slot);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn pump(&mut self, client: Arc<LspClient>, cx: &mut Context<Self>) {
        let events = client.events();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv_async().await {
                let client = client.clone();
                let alive = this.update(cx, |this, cx| this.handle(client, event, cx)).is_ok();
                if !alive {
                    break;
                }
            }
        });
        self._pumps.push(task);
    }

    fn handle(&mut self, client: Arc<LspClient>, event: ServerEvent, cx: &mut Context<Self>) {
        match event {
            ServerEvent::Diagnostics { path, diagnostics, .. } => {
                self.diagnostics.insert(graphyne_lsp::convert::path_key(&path), Arc::new(diagnostics));
                cx.notify();
            }
            ServerEvent::Progress { token, kind, title, message, percentage } => {
                let key = (client.name.clone(), token);
                match kind {
                    ProgressKind::End => {
                        self.progress.remove(&key);
                    }
                    _ => {
                        let entry = self.progress.entry(key).or_default();
                        if let Some(t) = title {
                            *entry = t;
                        }
                        let base = entry.split(" — ").next().unwrap_or_default().to_string();
                        let detail = match (message, percentage) {
                            (Some(m), Some(p)) => format!("{m} ({p}%)"),
                            (Some(m), None) => m,
                            (None, Some(p)) => format!("{p}%"),
                            (None, None) => String::new(),
                        };
                        *entry = if detail.is_empty() { base } else { format!("{base} — {detail}") };
                    }
                }
                cx.notify();
            }
            ServerEvent::Message { level, text } => {
                if level <= 2 {
                    cx.emit(LspStoreEvent::Notify(format!("{}: {text}", client.name), level == 1));
                }
            }
            ServerEvent::ApplyEdit { id, edits } => cx.emit(LspStoreEvent::ApplyEdit { client, id, edits }),
            ServerEvent::Exited => {
                let name = client.name.clone();
                for slot in self.slots.values_mut() {
                    if matches!(slot, Slot::Running(c) if c.name == name) {
                        *slot = Slot::Failed("server exited".into());
                    }
                }
                self.progress.retain(|(s, _), _| *s != name);
                cx.emit(LspStoreEvent::Notify(format!("{name} exited"), true));
                cx.notify();
            }
        }
    }

    pub fn client_for(&self, lang: LanguageId) -> Option<Arc<LspClient>> {
        match self.slots.get(&lang) {
            Some(Slot::Running(c)) if c.is_alive() => Some(c.clone()),
            _ => None,
        }
    }

    pub fn diagnostics_for(&self, path: &Path) -> Option<Arc<Vec<Diag>>> {
        self.diagnostics.get(&graphyne_lsp::convert::path_key(path)).cloned()
    }

    pub fn status_for(&self, lang: LanguageId) -> ServerStatus {
        match self.slots.get(&lang) {
            None | Some(Slot::NoServer) => ServerStatus::None,
            Some(Slot::Starting) => ServerStatus::Starting,
            Some(Slot::Missing { program, hint }) => ServerStatus::Missing { program, hint },
            Some(Slot::Failed(e)) => ServerStatus::Failed { server: graphyne_lsp::servers::specs_for(lang).first().map(|s| s.name).unwrap_or("server").into(), error: e.clone() },
            Some(Slot::Running(c)) => match self.progress.iter().find(|((s, _), _)| *s == c.name) {
                Some((_, msg)) => ServerStatus::Indexing { server: c.name.clone(), message: msg.clone() },
                None => ServerStatus::Ready { server: c.name.clone() },
            },
        }
    }

    pub fn shutdown_all(&self) {
        let mut seen = Vec::new();
        for slot in self.slots.values() {
            if let Slot::Running(c) = slot {
                if !seen.contains(&c.name) {
                    seen.push(c.name.clone());
                    c.shutdown();
                }
            }
        }
    }
}
