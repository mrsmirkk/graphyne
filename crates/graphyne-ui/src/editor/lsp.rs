//! Editor ↔ language-server features: document sync, diagnostics, inlay hints, completion
//! (resolve + auto-import), hover / quick documentation, signature help, go to definition
//! and Alt+Enter code actions with a live diff preview.

use super::markdown::{self, MdBlock};
use super::*;
use crate::lsp_store::LspStore;
use gpui::{Corner, FontWeight, HighlightStyle, StyledText, Subscription};
use graphyne_lsp::convert::{char_to_position, path_key, path_to_uri, position_to_char};
use graphyne_lsp::model::{self, Action, Diag, Edit, Severity, Signature};
use graphyne_lsp::{LspClient, snippet};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub(super) struct EditorDiag {
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
    pub raw: Value,
}

#[derive(Clone, Debug)]
pub(super) struct InlayHint {
    pub pos: usize,
    pub label: String,
}

pub(super) struct HoverPopup {
    pub anchor: Point<Pixels>,
    pub range: Range<usize>,
    pub diags: Vec<(Severity, String)>,
    pub blocks: Vec<MdBlock>,
}

pub(super) struct ActionsPopup {
    pub anchor: Point<Pixels>,
    pub actions: Vec<Action>,
    pub selected: usize,
}

pub(super) struct LspCompletionCache {
    pub word_start: usize,
    pub incomplete: bool,
    pub items: Arc<Vec<model::Completion>>,
}

/// All per-editor language-server state, grouped to keep `Editor` readable.
pub(super) struct LspState {
    pub store: Option<Entity<LspStore>>,
    pub client: Option<Arc<LspClient>>,
    pub doc_version: i32,
    pub synced: u64,
    /// Incremental-sync changes recorded since the last `didChange`.
    pub pending: Vec<Value>,
    /// Buffer version the last recorded change produced; an edit that bypassed
    /// recording (undo, reload) makes this stale and forces a full-text sync.
    pub pending_tail: u64,
    pub pending_broken: bool,
    /// Reference counts from `textDocument/references`, keyed by (symbol name, ordinal
    /// among same-named outline items).
    pub usages: Arc<HashMap<(String, usize), usize>>,
    pub usages_task: Option<Task<()>>,
    pub sync_task: Option<Task<()>>,
    pub diags_src: Option<Arc<Vec<Diag>>>,
    pub diags: Arc<Vec<EditorDiag>>,
    pub hints: Arc<Vec<InlayHint>>,
    pub hints_task: Option<Task<()>>,
    pub hover: Option<HoverPopup>,
    pub hover_timer: Option<Task<()>>,
    pub hover_request: Option<Task<()>>,
    pub hover_target: Option<usize>,
    pub hover_popup_hovered: bool,
    pub signature: Option<Signature>,
    pub signature_task: Option<Task<()>>,
    pub actions: Option<ActionsPopup>,
    pub actions_task: Option<Task<()>>,
    pub completions: Option<LspCompletionCache>,
    pub completion_task: Option<Task<()>>,
    pub completion_seq: u64,
    pub subs: Vec<Subscription>,
}

impl LspState {
    pub fn new(store: Option<Entity<LspStore>>) -> Self {
        Self {
            store,
            client: None,
            doc_version: 0,
            synced: 0,
            pending: Vec::new(),
            pending_tail: 0,
            pending_broken: false,
            usages: Arc::new(HashMap::new()),
            usages_task: None,
            sync_task: None,
            diags_src: None,
            diags: Arc::new(Vec::new()),
            hints: Arc::new(Vec::new()),
            hints_task: None,
            hover: None,
            hover_timer: None,
            hover_request: None,
            hover_target: None,
            hover_popup_hovered: false,
            signature: None,
            signature_task: None,
            actions: None,
            actions_task: None,
            completions: None,
            completion_task: None,
            completion_seq: 0,
            subs: Vec::new(),
        }
    }
}

fn severity_color(s: Severity) -> gpui::Hsla {
    match s {
        Severity::Error => color(c::RED),
        Severity::Warning => color(c::YELLOW),
        Severity::Info => color(c::BLUE),
        Severity::Hint => color(c::TEXT_DIM),
    }
}

/// Where the caret lands after applying non-overlapping `edits` (sorted by start):
/// a snippet `$0` wins; a caret inside a replaced range moves to the end of the new text;
/// otherwise it follows the text it was in.
pub(super) fn map_selection_through_edits(head: usize, edits: &[(Range<usize>, String, Option<Range<usize>>)]) -> Selection {
    let mut shift: isize = 0;
    let mut caret: Option<usize> = None;
    let mut snippet: Option<Selection> = None;
    for (r, t, cursor) in edits {
        let start_new = (r.start as isize + shift).max(0) as usize;
        let len = t.chars().count();
        if let (None, Some(c)) = (&snippet, cursor) {
            snippet = Some(Selection::new(start_new + c.start.min(len), start_new + c.end.min(len)));
        }
        if caret.is_none() {
            if r.start < head && head < r.end {
                caret = Some(start_new + len);
            } else if r.start >= head && !(r.is_empty() && r.start == head) {
                caret = Some((head as isize + shift).max(0) as usize);
            }
        }
        shift += len as isize - r.len() as isize;
    }
    snippet.unwrap_or_else(|| Selection::caret(caret.unwrap_or_else(|| (head as isize + shift).max(0) as usize)))
}

impl Editor {
    // ------------------------------------------------------------------ lifecycle / sync

    pub(super) fn lsp_init(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.lsp.store.clone() else { return };
        if self.path.is_none() {
            return;
        }
        let lang = self.language;
        store.update(cx, |s, cx| s.ensure(lang, cx));
        let observe = cx.observe(&store, |e, store, cx| e.on_lsp_store_changed(store, cx));
        let release = cx.on_release(|e, _| e.lsp_close());
        self.lsp.subs.push(observe);
        self.lsp.subs.push(release);
        self.on_lsp_store_changed(store, cx);
    }

    fn on_lsp_store_changed(&mut self, store: Entity<LspStore>, cx: &mut Context<Self>) {
        if self.lsp.client.is_none() {
            let client = store.read(cx).client_for(self.language);
            if let Some(client) = client {
                self.lsp_attach(client, cx);
            }
        }
        if let Some(path) = self.path.clone() {
            let diags = store.read(cx).diagnostics_for(&path);
            let changed = match (&diags, &self.lsp.diags_src) {
                (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
                (None, None) => false,
                _ => true,
            };
            if changed {
                let rope = self.buffer.rope();
                let converted = diags
                    .as_ref()
                    .map(|list| {
                        list.iter()
                            .map(|d| {
                                let s = position_to_char(rope, d.range.start);
                                let mut e = position_to_char(rope, d.range.end);
                                if e <= s {
                                    e = (s + 1).min(rope.len_chars()).max(s);
                                }
                                EditorDiag { range: s..e, severity: d.severity, message: d.message.clone(), source: d.source.clone(), raw: d.raw.clone() }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.lsp.diags = Arc::new(converted);
                self.lsp.diags_src = diags;
                // Fresh diagnostics mean the server finished (re)analysing: hints are now valid.
                self.schedule_inlay_hints(Duration::from_millis(80), cx);
                self.schedule_usages(Duration::from_millis(400), cx);
            }
        }
        cx.notify();
    }

    fn lsp_attach(&mut self, client: Arc<LspClient>, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else { return };
        if self.buffer.len_bytes() > 8 * 1024 * 1024 {
            return;
        }
        self.lsp.doc_version = 1;
        client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": path_to_uri(&path),
                "languageId": graphyne_lsp::servers::language_id(self.language),
                "version": 1,
                "text": self.buffer.rope().to_string()
            }}),
        );
        self.lsp.synced = self.buffer.version();
        self.lsp.pending.clear();
        self.lsp.pending_tail = self.buffer.version();
        self.lsp.pending_broken = false;
        self.lsp.client = Some(client);
        self.schedule_inlay_hints(Duration::from_millis(200), cx);
    }

    fn uri(&self) -> Option<String> {
        self.path.as_deref().map(path_to_uri)
    }

    fn pos_params(&self, char_idx: usize) -> Option<Value> {
        Some(json!({ "textDocument": { "uri": self.uri()? }, "position": char_to_position(self.buffer.rope(), char_idx) }))
    }

    pub(super) fn lsp_close(&mut self) {
        if let (Some(client), Some(uri)) = (&self.lsp.client, self.uri()) {
            client.notify("textDocument/didClose", json!({ "textDocument": { "uri": uri } }));
        }
    }

    pub(super) fn lsp_did_save(&mut self) {
        if let (Some(client), Some(uri)) = (&self.lsp.client, self.uri()) {
            client.notify("textDocument/didSave", json!({ "textDocument": { "uri": uri } }));
        }
    }

    /// Record edits for incremental sync. Call *before* the buffer applies them, with
    /// `edits` in application order (char ranges of the pre-edit text).
    pub(super) fn lsp_record_changes(&mut self, edits: &[(Range<usize>, &str)]) {
        if self.lsp.client.is_none() {
            return;
        }
        if self.lsp.pending_tail != self.buffer.version() || self.lsp.pending.len() > 256 {
            self.lsp.pending_broken = true;
        }
        if self.lsp.pending_broken {
            return;
        }
        // Each change is relative to the text after the previous one: replay on a scratch rope.
        let mut rope = self.buffer.rope().clone();
        for (r, t) in edits {
            let r = r.start.min(rope.len_chars())..r.end.min(rope.len_chars());
            // Same line-ending normalisation as `Buffer::edit`.
            let t: std::borrow::Cow<str> = if t.contains('\r') { t.replace("\r\n", "\n").replace('\r', "\n").into() } else { (*t).into() };
            self.lsp.pending.push(graphyne_lsp::convert::content_change(&rope, r.clone(), &t));
            if edits.len() > 1 {
                rope.remove(r.clone());
                rope.insert(r.start, &t);
            }
        }
    }

    /// Refresh `N usages` lenses from `textDocument/references` (all files, declaration
    /// excluded). Runs after the server re-analyses; stops early if the buffer changes.
    pub(super) fn schedule_usages(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let Some(client) = self.lsp.client.clone() else { return };
        if !client.supports("referencesProvider") {
            return;
        }
        self.lsp.usages_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let Ok((targets, uri, version)) = this.update(cx, |e, _| {
                e.lsp_flush();
                let rope = e.buffer.rope();
                let targets: Vec<((String, usize), Value)> = super::lens_items(&e.outline)
                    .into_iter()
                    .take(200)
                    .map(|(item, key)| (key, json!(char_to_position(rope, rope.byte_to_char(item.name_byte.min(rope.len_bytes()))))))
                    .collect();
                (targets, e.uri(), e.buffer.version())
            }) else {
                return;
            };
            let Some(uri) = uri else { return };
            let mut usages = HashMap::new();
            for (key, pos) in targets {
                let params = json!({ "textDocument": { "uri": uri }, "position": pos, "context": { "includeDeclaration": false } });
                if let Ok(v) = client.request("textDocument/references", params).await {
                    usages.insert(key, v.as_array().map(Vec::len).unwrap_or(0));
                }
                if this.read_with(cx, |e, _| e.buffer.version() != version).unwrap_or(true) {
                    return;
                }
            }
            this.update(cx, |e, cx| {
                e.lsp.usages = Arc::new(usages);
                e.rebuild_lenses();
                cx.notify();
            })
            .ok();
        }));
    }

    /// Mark the recorded changes as matching the buffer (call right after the edit).
    pub(super) fn lsp_recorded(&mut self) {
        self.lsp.pending_tail = self.buffer.version();
    }

    /// Push buffer changes to the server: only the edited ranges when the server supports
    /// incremental sync and every edit since the last push was recorded, else the full text.
    pub(super) fn lsp_flush(&mut self) {
        let Some(client) = self.lsp.client.clone() else { return };
        if self.lsp.synced == self.buffer.version() {
            return;
        }
        let Some(uri) = self.uri() else { return };
        let incremental = client.sync_kind() == 2
            && !self.lsp.pending_broken
            && self.lsp.pending_tail == self.buffer.version()
            && !self.lsp.pending.is_empty();
        let changes = if incremental { Value::Array(std::mem::take(&mut self.lsp.pending)) } else { json!([{ "text": self.buffer.rope().to_string() }]) };
        self.lsp.pending.clear();
        self.lsp.pending_broken = false;
        self.lsp.pending_tail = self.buffer.version();
        self.lsp.doc_version += 1;
        client.notify("textDocument/didChange", json!({ "textDocument": { "uri": uri, "version": self.lsp.doc_version }, "contentChanges": changes }));
        self.lsp.synced = self.buffer.version();
    }

    pub(super) fn after_lsp_edit(&mut self, cx: &mut Context<Self>) {
        self.lsp.hover = None;
        self.lsp.actions = None;
        if self.lsp.client.is_none() {
            return;
        }
        self.lsp.sync_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            this.update(cx, |e, _| e.lsp_flush()).ok();
        }));
        self.schedule_inlay_hints(Duration::from_millis(450), cx);
    }

    /// Keep hints / diagnostics anchored while the server catches up.
    pub(super) fn shift_lsp_positions(&mut self, start: usize, removed: usize, inserted: usize) {
        let end = start + removed;
        let map = |p: usize| if p < start { p } else if p >= end { p - removed + inserted } else { start + inserted };
        if !self.lsp.hints.is_empty() {
            let hints: Vec<InlayHint> = self
                .lsp
                .hints
                .iter()
                .filter(|h| h.pos <= start || h.pos >= end)
                .map(|h| InlayHint { pos: map(h.pos), label: h.label.clone() })
                .collect();
            self.lsp.hints = Arc::new(hints);
        }
        if !self.lsp.diags.is_empty() {
            let diags: Vec<EditorDiag> = self
                .lsp
                .diags
                .iter()
                .map(|d| {
                    let s = map(d.range.start);
                    let e = map(d.range.end).max(s + 1);
                    EditorDiag { range: s..e, ..d.clone() }
                })
                .collect();
            self.lsp.diags = Arc::new(diags);
        }
    }

    // ------------------------------------------------------------------ inlay hints

    fn schedule_inlay_hints(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let Some(client) = self.lsp.client.clone() else { return };
        if !client.supports("inlayHintProvider") {
            return;
        }
        let Some(uri) = self.uri() else { return };
        self.lsp.hints_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let Ok((params, version)) = this.update(cx, |e, _| {
                e.lsp_flush();
                let end = char_to_position(e.buffer.rope(), e.buffer.len_chars());
                (json!({ "textDocument": { "uri": uri }, "range": { "start": { "line": 0, "character": 0 }, "end": end } }), e.buffer.version())
            }) else {
                return;
            };
            let Ok(result) = client.request("textDocument/inlayHint", params).await else { return };
            this.update(cx, |e, cx| {
                if e.buffer.version() != version {
                    return;
                }
                let rope = e.buffer.rope();
                let mut hints: Vec<InlayHint> = model::parse_inlay_hints(&result)
                    .into_iter()
                    .map(|h| InlayHint {
                        pos: position_to_char(rope, h.pos),
                        label: format!("{}{}{}", if h.pad_left { " " } else { "" }, h.label.trim(), if h.pad_right { " " } else { "" }),
                    })
                    .collect();
                hints.sort_by_key(|h| h.pos);
                e.lsp.hints = Arc::new(hints);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Hints for one buffer row as (column, label).
    pub(super) fn hints_for_row(&self, row: usize) -> Vec<(usize, String)> {
        if self.lsp.hints.is_empty() {
            return Vec::new();
        }
        let start = self.buffer.line_to_char(row);
        let end = start + self.buffer.line_len(row);
        let hints = &self.lsp.hints;
        let first = hints.partition_point(|h| h.pos < start);
        hints[first..].iter().take_while(|h| h.pos <= end).map(|h| (h.pos - start, h.label.clone())).collect()
    }

    // ------------------------------------------------------------------ completion

    pub(super) fn lsp_completion_ready(&self) -> bool {
        self.lsp.client.as_ref().is_some_and(|c| c.supports("completionProvider"))
    }

    pub(super) fn request_lsp_completion(&mut self, trigger: Option<char>, cx: &mut Context<Self>) {
        let Some(client) = self.lsp.client.clone() else { return };
        if !client.supports("completionProvider") {
            return;
        }
        self.lsp_flush();
        let pos = self.sel.head;
        let word_start = self.word_start_before(pos);
        let Some(mut params) = self.pos_params(pos) else { return };
        params["context"] = match trigger {
            Some(ch) => json!({ "triggerKind": 2, "triggerCharacter": ch.to_string() }),
            None => json!({ "triggerKind": 1 }),
        };
        self.lsp.completion_seq += 1;
        let seq = self.lsp.completion_seq;
        let explicit = trigger.is_some();
        self.lsp.completion_task = Some(cx.spawn(async move |this, cx| {
            let Ok(result) = client.request("textDocument/completion", params).await else { return };
            let (incomplete, items) = cx.background_spawn(async move { model::parse_completions(&result) }).await;
            this.update(cx, |e, cx| {
                if e.lsp.completion_seq != seq {
                    return;
                }
                e.lsp.completions = Some(LspCompletionCache { word_start, incomplete, items: Arc::new(items) });
                e.rebuild_completion(explicit, cx);
            })
            .ok();
        }));
    }

    /// LSP entries ranked client-side (CamelHump via nucleo, ties by `sortText`).
    pub(super) fn lsp_completion_entries(&self, start: usize, prefix: &str) -> Option<Vec<CompletionEntry>> {
        let cache = self.lsp.completions.as_ref().filter(|c| c.word_start == start)?;
        let texts: Vec<&str> = cache.items.iter().map(|i| i.filter_text.as_str()).collect();
        let mut matches = fuzzy_filter(&texts, prefix, 400, false);
        matches.sort_by(|a, b| {
            b.score.cmp(&a.score).then_with(|| cache.items[a.index].sort_text.cmp(&cache.items[b.index].sort_text))
        });
        Some(
            matches
                .into_iter()
                .filter(|m| !cache.items[m.index].deprecated || prefix.len() > 2)
                .take(80)
                .map(|m| {
                    let it = &cache.items[m.index];
                    let indices = if it.label == it.filter_text || prefix.is_empty() {
                        m.indices
                    } else {
                        fuzzy_filter(&[it.label.as_str()], prefix, 1, false).first().map(|x| x.indices.clone()).unwrap_or_default()
                    };
                    CompletionEntry {
                        label: it.label.clone(),
                        indices,
                        detail: it.label_detail.clone().or_else(|| it.detail.clone()),
                        kind: it.kind,
                        lsp: Some(it.clone()),
                    }
                })
                .collect(),
        )
    }

    pub(super) fn accept_lsp_completion(&mut self, item: model::Completion, word_start: usize, cx: &mut Context<Self>) {
        self.lsp.completions = None;
        let client = self.lsp.client.clone();
        let needs_resolve = item.additional.is_empty() && client.as_ref().is_some_and(|c| c.completion_resolve());
        if !needs_resolve {
            self.apply_lsp_completion(item, word_start, cx);
            return;
        }
        let Some(client) = client else { return };
        let version = self.buffer.version();
        cx.spawn(async move |this, cx| {
            let resolved = client.request("completionItem/resolve", item.raw.clone()).await.ok().and_then(|v| model::parse_completion_item(&v, &Value::Null));
            this.update(cx, |e, cx| {
                if e.buffer.version() != version {
                    return;
                }
                let mut item = item;
                if let Some(r) = resolved {
                    if !r.additional.is_empty() {
                        item.additional = r.additional;
                    }
                    if item.edit_range.is_none() && r.edit_range.is_some() {
                        item.edit_range = r.edit_range;
                        item.insert_text = r.insert_text;
                        item.is_snippet = r.is_snippet;
                    }
                }
                e.apply_lsp_completion(item, word_start, cx);
            })
            .ok();
        })
        .detach();
    }

    fn apply_lsp_completion(&mut self, item: model::Completion, word_start: usize, cx: &mut Context<Self>) {
        let rope = self.buffer.rope();
        let cursor = self.sel.head;
        let start = item.edit_range.map(|r| position_to_char(rope, r.start)).unwrap_or(word_start).min(cursor);
        let (text, sel_rel) = if item.is_snippet {
            let e = snippet::expand(&item.insert_text);
            (e.text, e.selection)
        } else {
            let n = item.insert_text.chars().count();
            (item.insert_text.clone(), n..n)
        };
        let opens_call = text.contains('(');
        let mut edits = vec![(start..cursor, text)];
        // Auto-import: `additionalTextEdits` (usually `use …;` at the top) in the same undo step.
        let mut shift: isize = 0;
        for add in &item.additional {
            let s = position_to_char(rope, add.range.start);
            let e = position_to_char(rope, add.range.end).max(s);
            if e <= start {
                shift += add.new_text.chars().count() as isize - (e - s) as isize;
            } else if s < cursor {
                continue; // overlaps the main edit: invalid per spec, skip
            }
            edits.push((s..e, add.new_text.clone()));
        }
        let base = (start as isize + shift).max(0) as usize;
        let sel = Selection::new(base + sel_rel.start, base + sel_rel.end);
        self.completion = None;
        self.apply_batch(edits, sel, cx);
        if opens_call {
            self.request_signature(false, cx);
        }
    }

    /// Apply LSP text edits to this document (code actions, server `applyEdit`).
    pub fn apply_lsp_text_edits(&mut self, edits: &[Edit], cx: &mut Context<Self>) {
        let rope = self.buffer.rope();
        let mut converted: Vec<(Range<usize>, String, Option<Range<usize>>)> = edits
            .iter()
            .map(|e| {
                let s = position_to_char(rope, e.range.start);
                let en = position_to_char(rope, e.range.end).max(s);
                (s..en, e.new_text.clone(), e.cursor.clone())
            })
            .collect();
        converted.sort_by_key(|(r, _, _)| r.start);
        let sel = map_selection_through_edits(self.sel.head, &converted);
        self.apply_batch(converted.into_iter().map(|(r, t, _)| (r, t)).collect(), sel, cx);
    }

    pub(super) fn is_my_path(&self, path: &std::path::Path) -> bool {
        self.path.as_deref().is_some_and(|p| path_key(p) == path_key(path))
    }

    // ------------------------------------------------------------------ hover / quick doc

    pub(super) fn quick_documentation(&mut self, _: &QuickDocumentation, _: &mut Window, cx: &mut Context<Self>) {
        let c = self.sel.head;
        let anchor = self.caret_point().unwrap_or_default();
        self.request_hover(c, anchor, true, cx);
    }

    fn diags_at(&self, c: usize) -> Vec<(Severity, String)> {
        let mut out: Vec<(Severity, String)> = if self.lsp.client.is_some() {
            self.lsp
                .diags
                .iter()
                .filter(|d| d.range.start <= c && c < d.range.end.max(d.range.start + 1))
                .map(|d| (d.severity, match &d.source { Some(s) => format!("{} ({s})", d.message), None => d.message.clone() }))
                .collect()
        } else {
            let byte = self.buffer.char_to_byte(c);
            self.syntax_errors.iter().filter(|r| r.start <= byte && byte < r.end).map(|_| (Severity::Error, "Syntax error".to_string())).collect()
        };
        let (row, col) = self.buffer.row_col(c);
        for s in self.secrets.iter().filter(|s| s.line == row && col >= s.column && col < s.column + 8) {
            out.push((Severity::Warning, format!("Secrets Guard: possible {} ({})", s.kind, s.preview)));
        }
        out
    }

    pub(super) fn request_hover(&mut self, c: usize, anchor: Point<Pixels>, explicit: bool, cx: &mut Context<Self>) {
        let range = {
            let w = self.buffer.word_range_at(c);
            if w.is_empty() { c..c + 1 } else { w }
        };
        let diags = self.diags_at(c);
        let client = self.lsp.client.clone().filter(|cl| cl.supports("hoverProvider"));
        let Some(client) = client else {
            if !diags.is_empty() || explicit {
                let blocks = if diags.is_empty() { markdown::parse("*No documentation available*") } else { Vec::new() };
                self.lsp.hover = Some(HoverPopup { anchor, range, diags, blocks });
                cx.notify();
            }
            return;
        };
        self.lsp_flush();
        let Some(params) = self.pos_params(c) else { return };
        let version = self.buffer.version();
        self.lsp.hover_request = Some(cx.spawn(async move |this, cx| {
            let md = client.request("textDocument/hover", params).await.ok().and_then(|v| model::parse_hover(&v));
            let blocks = cx.background_spawn(async move { md.map(|m| markdown::parse(&m)).unwrap_or_default() }).await;
            this.update(cx, |e, cx| {
                if e.buffer.version() != version {
                    return;
                }
                if blocks.is_empty() && diags.is_empty() && !explicit {
                    return;
                }
                let blocks = if blocks.is_empty() && diags.is_empty() { markdown::parse("*No documentation available*") } else { blocks };
                e.lsp.hover = Some(HoverPopup { anchor, range, diags, blocks });
                cx.notify();
            })
            .ok();
        }));
    }

    /// The char exactly under the mouse (not the nearest one) and a popup anchor below it.
    fn char_under_mouse(&self, position: Point<Pixels>) -> Option<(usize, Point<Pixels>)> {
        let layout = self.layout.as_ref()?;
        if position.x < layout.bounds.left() + layout.gutter_width || !layout.bounds.contains(&position) {
            return None;
        }
        let r = layout.rows.iter().find(|r| position.y >= r.y && position.y < r.y + layout.line_height)?;
        let RowKind::Line { row, line, char_start, .. } = &r.kind else { return None };
        let byte = line.index_for_x(position.x - layout.text_x)?;
        let col = char_start.partition_point(|b| *b <= byte).saturating_sub(1);
        if col >= self.buffer.line_len(*row) {
            return None;
        }
        Some((self.buffer.line_to_char(*row) + col, point(position.x - px(12.), r.y + layout.line_height + px(2.))))
    }

    pub(super) fn on_hover_move(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.lsp.hover_popup_hovered {
            return;
        }
        let hit = self.char_under_mouse(position);
        if let Some(h) = &self.lsp.hover {
            if hit.as_ref().is_some_and(|(c, _)| h.range.contains(c)) {
                return;
            }
            self.lsp.hover = None;
            cx.notify();
        }
        let Some((c, anchor)) = hit else {
            self.lsp.hover_target = None;
            self.lsp.hover_timer = None;
            return;
        };
        let word = self.buffer.word_range_at(c);
        if self.lsp.hover_target.is_some_and(|t| t == c || (!word.is_empty() && word.contains(&t))) {
            return;
        }
        self.lsp.hover_target = Some(c);
        self.lsp.hover_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(450)).await;
            this.update(cx, |e, cx| {
                // No mouse hover card over an open quick-fix menu.
                if e.lsp.hover_target == Some(c) && e.drag.is_none() && e.lsp.actions.is_none() {
                    e.request_hover(c, anchor, false, cx);
                }
            })
            .ok();
        }));
    }

    // ------------------------------------------------------------------ signature help

    pub(super) fn parameter_info(&mut self, _: &ParameterInfo, _: &mut Window, cx: &mut Context<Self>) {
        self.request_signature(true, cx);
    }

    pub(super) fn request_signature(&mut self, explicit: bool, cx: &mut Context<Self>) {
        let Some(client) = self.lsp.client.clone().filter(|c| c.supports("signatureHelpProvider")) else { return };
        self.lsp_flush();
        let Some(mut params) = self.pos_params(self.sel.head) else { return };
        let retrigger = self.lsp.signature.is_some();
        params["context"] = json!({ "triggerKind": if explicit { 1 } else { 3 }, "isRetrigger": retrigger });
        let version = self.buffer.version();
        self.lsp.signature_task = Some(cx.spawn(async move |this, cx| {
            let sig = client.request("textDocument/signatureHelp", params).await.ok().and_then(|v| model::parse_signature_help(&v));
            this.update(cx, |e, cx| {
                if e.buffer.version() == version {
                    e.lsp.signature = sig;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Called after every typed character.
    pub(super) fn on_char_typed(&mut self, ch: char, cx: &mut Context<Self>) {
        let Some(client) = self.lsp.client.clone() else { return };
        let s = ch.to_string();
        if self.lsp.signature.is_some() || client.signature_triggers().contains(&s) {
            self.request_signature(false, cx);
        }
        if !is_word_char(ch) && client.completion_triggers().contains(&s) {
            self.request_lsp_completion(Some(ch), cx);
        }
    }

    // ------------------------------------------------------------------ navigation

    pub(super) fn go_to_definition(&mut self, _: &GoToDefinition, _: &mut Window, cx: &mut Context<Self>) {
        self.request_definition(self.sel.head, cx);
    }

    pub(super) fn request_definition(&mut self, c: usize, cx: &mut Context<Self>) {
        let Some(client) = self.lsp.client.clone().filter(|cl| cl.supports("definitionProvider")) else {
            cx.emit(EditorEvent::Notify("Go to Definition needs a language server for this file".into()));
            return;
        };
        self.lsp_flush();
        let Some(params) = self.pos_params(c) else { return };
        cx.spawn(async move |this, cx| {
            let Ok(v) = client.request("textDocument/definition", params).await else { return };
            let target = model::parse_locations(&v).into_iter().next();
            this.update(cx, |_, cx| match target {
                Some((path, pos)) => cx.emit(EditorEvent::OpenLocation(path, pos.line as usize, pos.character as usize)),
                None => cx.emit(EditorEvent::Notify("Cannot find declaration to go to".into())),
            })
            .ok();
        })
        .detach();
    }

    // ------------------------------------------------------------------ code actions (Alt+Enter)

    pub(super) fn show_code_actions(&mut self, _: &ShowCodeActions, _: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.lsp.client.clone().filter(|c| c.supports("codeActionProvider")) else {
            cx.emit(EditorEvent::Notify("Quick fixes need a language server for this file".into()));
            return;
        };
        self.lsp_flush();
        let Some(uri) = self.uri() else { return };
        self.lsp.hover = None;
        self.lsp.hover_timer = None;
        let r = self.sel.range();
        let rope = self.buffer.rope();
        let diags: Vec<Value> = self
            .lsp
            .diags
            .iter()
            .filter(|d| d.range.start <= r.end.max(r.start + 1) && r.start <= d.range.end)
            .map(|d| d.raw.clone())
            .collect();
        let params = json!({
            "textDocument": { "uri": uri },
            "range": { "start": char_to_position(rope, r.start), "end": char_to_position(rope, r.end) },
            "context": { "diagnostics": diags, "triggerKind": 1 }
        });
        let anchor = self.caret_point().unwrap_or_default();
        self.lsp.actions_task = Some(cx.spawn(async move |this, cx| {
            let actions = client.request("textDocument/codeAction", params).await.ok().map(|v| model::parse_code_actions(&v)).unwrap_or_default();
            this.update(cx, |e, cx| {
                let mut actions: Vec<Action> = actions.into_iter().filter(|a| a.disabled.is_none()).collect();
                actions.sort_by_key(|a| (!a.preferred, !a.kind.starts_with("quickfix")));
                if actions.is_empty() {
                    cx.emit(EditorEvent::Notify("No quick fixes or intentions available here".into()));
                    return;
                }
                e.lsp.actions = Some(ActionsPopup { anchor, actions, selected: 0 });
                e.resolve_selected_action(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Resolve lazily-computed edits so the preview can show them.
    pub(super) fn resolve_selected_action(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.lsp.client.clone() else { return };
        let Some(popup) = &self.lsp.actions else { return };
        let ix = popup.selected;
        let Some(action) = popup.actions.get(ix) else { return };
        if action.edits.is_some() || !client.code_action_resolve() {
            return;
        }
        let raw = action.raw.clone();
        let title = action.title.clone();
        cx.spawn(async move |this, cx| {
            let Ok(v) = client.request("codeAction/resolve", raw).await else { return };
            let Some(resolved) = model::parse_code_action(&v) else { return };
            this.update(cx, |e, cx| {
                if let Some(a) = e.lsp.actions.as_mut().and_then(|p| p.actions.get_mut(ix)).filter(|a| a.title == title) {
                    a.edits = resolved.edits;
                    if resolved.command.is_some() {
                        a.command = resolved.command;
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn move_action_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(p) = self.lsp.actions.as_mut() {
            let n = p.actions.len() as isize;
            p.selected = ((p.selected as isize + delta).rem_euclid(n)) as usize;
        }
        self.resolve_selected_action(cx);
        cx.notify();
    }

    pub(super) fn apply_selected_action(&mut self, cx: &mut Context<Self>) {
        let Some(popup) = self.lsp.actions.take() else { return };
        let Some(action) = popup.actions.get(popup.selected).cloned() else { return };
        let client = self.lsp.client.clone();
        if action.edits.is_none() && client.as_ref().is_some_and(|c| c.code_action_resolve()) {
            let Some(client) = client else { return };
            cx.spawn(async move |this, cx| {
                let resolved = client.request("codeAction/resolve", action.raw.clone()).await.ok().and_then(|v| model::parse_code_action(&v)).unwrap_or(action);
                this.update(cx, |e, cx| e.apply_action_now(resolved, cx)).ok();
            })
            .detach();
        } else {
            self.apply_action_now(action, cx);
        }
        cx.notify();
    }

    fn apply_action_now(&mut self, action: Action, cx: &mut Context<Self>) {
        let mut others = Vec::new();
        for file in action.edits.unwrap_or_default() {
            if self.is_my_path(&file.path) {
                self.apply_lsp_text_edits(&file.edits, cx);
            } else {
                others.push(file);
            }
        }
        if !others.is_empty() {
            cx.emit(EditorEvent::ApplyEdits(others));
        }
        if let (Some(cmd), Some(client)) = (action.command, self.lsp.client.clone()) {
            let params = json!({ "command": cmd["command"], "arguments": cmd.get("arguments").cloned().unwrap_or(json!([])) });
            cx.background_spawn(async move {
                let _ = client.request("workspace/executeCommand", params).await;
            })
            .detach();
        }
        cx.notify();
    }

    /// Before/after lines for the live diff preview. Edits of this file are applied
    /// together, so several small edits on one line (snippet edits) read as one change.
    fn action_preview(&self, action: &Action) -> Vec<(bool, String)> {
        let Some(files) = &action.edits else { return Vec::new() };
        let mut out = Vec::new();
        for file in files {
            if !self.is_my_path(&file.path) {
                out.push((true, format!("✎ {}", file.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())));
                continue;
            }
            let rope = self.buffer.rope();
            let mut edits: Vec<(Range<usize>, &str)> = file
                .edits
                .iter()
                .map(|e| {
                    let s = position_to_char(rope, e.range.start);
                    (s..position_to_char(rope, e.range.end).max(s), e.new_text.as_str())
                })
                .collect();
            edits.sort_by_key(|(r, _)| r.start);
            // Group edits whose lines touch into hunks.
            let mut hunks: Vec<(usize, usize, Vec<(Range<usize>, &str)>)> = Vec::new();
            for (r, t) in edits {
                let (l0, l1) = (rope.char_to_line(r.start), rope.char_to_line(r.end));
                match hunks.last_mut() {
                    Some((_, end, list)) if l0 <= *end + 1 => {
                        *end = (*end).max(l1);
                        list.push((r, t));
                    }
                    _ => hunks.push((l0, l1, vec![(r, t)])),
                }
            }
            for (l0, l1, list) in hunks.into_iter().take(3) {
                let ls = rope.line_to_char(l0);
                let le = self.buffer.char_at(l1, usize::MAX);
                let old = self.buffer.slice_to_string(ls..le);
                let mut new = String::new();
                let mut at = ls;
                for (r, t) in list {
                    if r.start < at {
                        continue; // overlapping edits: keep the first
                    }
                    new.push_str(&self.buffer.slice_to_string(at..r.start));
                    new.push_str(t);
                    at = r.end;
                }
                new.push_str(&self.buffer.slice_to_string(at..le));
                out.extend(old.lines().take(4).map(|l| (false, l.to_string())));
                out.extend(new.lines().take(6).map(|l| (true, l.to_string())));
            }
        }
        out.truncate(14);
        out
    }

    // ------------------------------------------------------------------ popups

    pub(super) fn render_hover(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let hover = self.lsp.hover.as_ref()?;
        let blocks = hover.blocks.clone();
        let diags = hover.diags.clone();
        Some(deferred(
            anchored().position(hover.anchor).snap_to_window_with_margin(px(8.)).child(
                div()
                    .id("hover-popup")
                    .occlude()
                    .on_hover(cx.listener(|e, hovered: &bool, _, cx| {
                        e.lsp.hover_popup_hovered = *hovered;
                        if !*hovered {
                            e.lsp.hover = None;
                            e.lsp.hover_target = None;
                            cx.notify();
                        }
                    }))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .max_w(px(580.))
                    .max_h(px(400.))
                    .overflow_y_scroll()
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(color(c::POPUP))
                    .border_1()
                    .border_color(color(c::POPUP_BORDER))
                    .shadow_lg()
                    .font_family(theme::fonts().ui)
                    .children(diags.into_iter().map(|(sev, msg)| {
                        div()
                            .flex()
                            .gap(px(6.))
                            .text_size(px(12.5))
                            .text_color(severity_color(sev))
                            .child(icon(if sev == Severity::Error { Icon::Error } else { Icon::Warning }, 13., severity_color(sev)))
                            .child(div().flex_1().child(msg))
                    }))
                    .child(markdown::render(&blocks)),
            ),
        ))
    }

    pub(super) fn render_signature(&self) -> Option<impl IntoElement + use<>> {
        let sig = self.lsp.signature.as_ref()?;
        let caret = self.caret_point()?;
        let lh = self.layout.as_ref().map(|l| l.line_height).unwrap_or(px(22.));
        let highlights: Vec<(Range<usize>, HighlightStyle)> = sig
            .active_param
            .clone()
            .map(|r| vec![(r, HighlightStyle { color: Some(color(c::MATCH)), font_weight: Some(FontWeight::BOLD), ..Default::default() })])
            .unwrap_or_default();
        let label = StyledText::new(SharedString::from(sig.label.clone())).with_highlights(highlights);
        let counter = (sig.count > 1).then(|| format!("{}/{}", sig.index + 1, sig.count));
        let doc = sig.param_documentation.clone().or_else(|| sig.documentation.clone()).map(|d| d.lines().next().unwrap_or_default().to_string());
        Some(deferred(
            anchored().anchor(Corner::BottomLeft).position(point(caret.x - px(10.), caret.y - lh - px(4.))).snap_to_window_with_margin(px(8.)).child(
                div()
                    .occlude()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .max_w(px(640.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(8.))
                    .bg(color(c::POPUP))
                    .border_1()
                    .border_color(color(c::POPUP_BORDER))
                    .shadow_lg()
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .font_family(theme::fonts().code)
                            .text_size(px(12.5))
                            .text_color(color(c::TEXT_MUTED))
                            .children(counter.map(|n| div().text_color(color(c::TEXT_DIM)).child(n)))
                            .child(label),
                    )
                    .children(doc.filter(|d| !d.trim().is_empty()).map(|d| div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).font_family(theme::fonts().ui).child(d))),
            ),
        ))
    }

    pub(super) fn render_actions(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let popup = self.lsp.actions.as_ref()?;
        let selected = popup.selected;
        let titles: Vec<(String, bool, String)> = popup.actions.iter().map(|a| (a.title.clone(), a.preferred, a.kind.clone())).collect();
        let preview = popup.actions.get(selected).map(|a| self.action_preview(a)).unwrap_or_default();
        let entity = cx.entity();
        Some(deferred(
            anchored().position(popup.anchor).snap_to_window_with_margin(px(8.)).child(
                div()
                    .id("code-actions")
                    .occlude()
                    .flex()
                    .flex_col()
                    .w(px(520.))
                    .rounded(px(8.))
                    .bg(color(c::POPUP))
                    .border_1()
                    .border_color(color(c::POPUP_BORDER))
                    .shadow_lg()
                    .overflow_hidden()
                    .font_family(theme::fonts().ui)
                    .text_size(px(12.5))
                    .child(
                        div().p(px(4.)).flex().flex_col().children(titles.into_iter().enumerate().map(move |(ix, (title, preferred, kind))| {
                            let entity = entity.clone();
                            let tint = if preferred { color(c::YELLOW) } else if kind.starts_with("quickfix") { color(c::ACCENT) } else { color(c::TEXT_DIM) };
                            div()
                                .id(ix)
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .h(px(26.))
                                .px(px(8.))
                                .rounded(px(5.))
                                .text_color(color(c::TEXT))
                                .cursor_pointer()
                                .when(ix == selected, |d| d.bg(color(c::ACCENT_SOFT)))
                                .hover(|d| d.bg(color(c::HOVER)))
                                .on_click(move |_, _, cx| {
                                    entity.update(cx, |e, cx| {
                                        if let Some(p) = e.lsp.actions.as_mut() {
                                            p.selected = ix;
                                        }
                                        e.apply_selected_action(cx);
                                    })
                                })
                                .child(icon(Icon::Bolt, 13., tint))
                                .child(div().flex_1().whitespace_nowrap().overflow_hidden().text_ellipsis().child(title))
                        })),
                    )
                    .when(!preview.is_empty(), |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .p(px(8.))
                                .border_t_1()
                                .border_color(color(c::BORDER))
                                .bg(color(0x16181B))
                                .font_family(theme::fonts().code)
                                .text_size(px(12.))
                                .children(preview.into_iter().map(|(added, line)| {
                                    div()
                                        .whitespace_nowrap()
                                        .overflow_hidden()
                                        .px(px(4.))
                                        .bg(if added { color_a(c::GREEN, 0.10) } else { color_a(c::RED, 0.10) })
                                        .text_color(if added { color(0xA8D8B0) } else { color(0xE0A3A3) })
                                        .child(format!("{} {}", if added { "+" } else { "-" }, if line.is_empty() { " ".to_string() } else { line }))
                                })),
                        )
                    }),
            ),
        ))
    }

    /// Smart Mode / problems helpers for the workspace.
    pub fn lsp_problem_count(&self) -> Option<(usize, usize)> {
        self.lsp.client.as_ref()?;
        let errors = self.lsp.diags.iter().filter(|d| d.severity == Severity::Error).count();
        let warnings = self.lsp.diags.iter().filter(|d| d.severity == Severity::Warning).count();
        Some((errors, warnings))
    }

    /// Diagnostic underline colour for a char, if any.
    pub(super) fn lsp_underline(diags: &[EditorDiag], ch: usize) -> Option<gpui::Hsla> {
        diags
            .iter()
            .filter(|d| d.range.start <= ch && ch < d.range.end)
            .min_by_key(|d| d.severity)
            .map(|d| match d.severity {
                Severity::Hint => color_a(c::TEXT_DIM, 0.6),
                s => severity_color(s),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_follows_lsp_edits() {
        let e = |r: Range<usize>, t: &str, c: Option<Range<usize>>| (r, t.to_string(), c);
        // Auto-import above the caret shifts it down.
        assert_eq!(map_selection_through_edits(10, &[e(0..0, "use x;\n", None)]), Selection::caret(17));
        // Caret inside a replaced expression lands after the replacement.
        assert_eq!(map_selection_through_edits(7, &[e(5..10, "abc", None)]), Selection::caret(8));
        // Edits after the caret leave it alone.
        assert_eq!(map_selection_through_edits(10, &[e(30..35, "", None)]), Selection::caret(10));
        // A snippet `${0:v}` selects the placeholder, positioned after earlier edits.
        assert_eq!(
            map_selection_through_edits(25, &[e(0..0, "use x;\n", None), e(20..20, "let v = e;\n", Some(4..5))]),
            Selection::new(31, 32)
        );
    }
}
