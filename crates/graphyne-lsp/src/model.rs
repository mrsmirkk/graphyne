//! Lenient parsing of LSP payloads into small UI-friendly types.
//! Positions stay in LSP coordinates (line, UTF-16 column); the editor converts them
//! against its own rope with [`crate::convert::position_to_char`].

use crate::convert::uri_to_path;
use lsp_types::{Position, Range};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error = 1,
    Warning = 2,
    Info = 3,
    Hint = 4,
}

#[derive(Clone, Debug)]
pub struct Diag {
    pub range: Range,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
    /// Original JSON, echoed back in `codeAction` context.
    pub raw: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub range: Range,
    /// Plain text to insert (snippet edits are already expanded).
    pub new_text: String,
    /// Caret / selection inside `new_text` (char offsets) requested by a snippet text
    /// edit (`$0`, `${0:name}`), e.g. rust-analyzer's "Extract into variable".
    pub cursor: Option<std::ops::Range<usize>>,
}

#[derive(Clone, Debug)]
pub struct FileEdits {
    pub path: PathBuf,
    pub edits: Vec<Edit>,
}

fn range(v: &Value) -> Option<Range> {
    serde_json::from_value(v.clone()).ok()
}

fn position(v: &Value) -> Option<Position> {
    serde_json::from_value(v.clone()).ok()
}

fn edit(v: &Value) -> Option<Edit> {
    let raw = v["newText"].as_str()?;
    // `SnippetTextEdit` (experimental.snippetTextEdit): insertTextFormat 2 = snippet.
    let (new_text, cursor) = if v["insertTextFormat"].as_u64() == Some(2) {
        let e = crate::snippet::expand(raw);
        (e.text, Some(e.selection))
    } else {
        (raw.to_string(), None)
    };
    Some(Edit { range: range(&v["range"])?, new_text, cursor })
}

fn opt_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// `string | MarkupContent | MarkedString | MarkedString[]` → markdown.
pub fn markup_to_markdown(v: &Value) -> Option<String> {
    let text = match v {
        Value::String(s) => s.clone(),
        Value::Object(o) if o.contains_key("kind") => o.get("value").and_then(Value::as_str).unwrap_or_default().to_string(),
        Value::Object(o) if o.contains_key("language") => {
            format!("```{}\n{}\n```", o.get("language").and_then(Value::as_str).unwrap_or(""), o.get("value").and_then(Value::as_str).unwrap_or(""))
        }
        Value::Array(items) => items.iter().filter_map(markup_to_markdown).collect::<Vec<_>>().join("\n\n---\n\n"),
        _ => return None,
    };
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub fn parse_diagnostics(params: &Value) -> Option<(String, Option<i64>, Vec<Diag>)> {
    let uri = params["uri"].as_str()?.to_string();
    let version = params["version"].as_i64();
    let diags = params["diagnostics"]
        .as_array()?
        .iter()
        .filter_map(|d| {
            Some(Diag {
                range: range(&d["range"])?,
                severity: match d["severity"].as_u64() {
                    Some(1) | None => Severity::Error,
                    Some(2) => Severity::Warning,
                    Some(3) => Severity::Info,
                    _ => Severity::Hint,
                },
                message: d["message"].as_str().unwrap_or_default().to_string(),
                source: d["source"].as_str().map(str::to_string),
                code: opt_string(&d["code"]),
                raw: d.clone(),
            })
        })
        .collect();
    Some((uri, version, diags))
}

/// `WorkspaceEdit` (`changes` or `documentChanges`) → per-file text edits.
pub fn parse_workspace_edit(v: &Value) -> Vec<FileEdits> {
    let mut out: Vec<FileEdits> = Vec::new();
    let mut push = |uri: &str, edits: Vec<Edit>| {
        let Some(path) = uri_to_path(uri) else { return };
        match out.iter_mut().find(|f| f.path == path) {
            Some(f) => f.edits.extend(edits),
            None => out.push(FileEdits { path, edits }),
        }
    };
    if let Some(changes) = v["documentChanges"].as_array() {
        for change in changes {
            if let (Some(uri), Some(edits)) = (change["textDocument"]["uri"].as_str(), change["edits"].as_array()) {
                push(uri, edits.iter().filter_map(edit).collect());
            }
        }
    } else if let Some(changes) = v["changes"].as_object() {
        for (uri, edits) in changes {
            if let Some(edits) = edits.as_array() {
                push(uri, edits.iter().filter_map(edit).collect());
            }
        }
    }
    out
}

pub fn parse_hover(v: &Value) -> Option<String> {
    markup_to_markdown(&v["contents"])
}

#[derive(Clone, Debug)]
pub struct Signature {
    pub label: String,
    /// Byte range of the active parameter inside `label`.
    pub active_param: Option<std::ops::Range<usize>>,
    pub documentation: Option<String>,
    pub param_documentation: Option<String>,
    pub index: usize,
    pub count: usize,
}

fn utf16_to_byte(s: &str, u16_offset: usize) -> usize {
    let mut units = 0;
    for (i, ch) in s.char_indices() {
        if units >= u16_offset {
            return i;
        }
        units += ch.len_utf16();
    }
    s.len()
}

pub fn parse_signature_help(v: &Value) -> Option<Signature> {
    let sigs = v["signatures"].as_array()?;
    if sigs.is_empty() {
        return None;
    }
    let index = (v["activeSignature"].as_u64().unwrap_or(0) as usize).min(sigs.len() - 1);
    let sig = &sigs[index];
    let label = sig["label"].as_str()?.to_string();
    let active = sig["activeParameter"].as_u64().or_else(|| v["activeParameter"].as_u64()).unwrap_or(0) as usize;
    let param = sig["parameters"].as_array().and_then(|p| p.get(active));
    let active_param = param.and_then(|p| match &p["label"] {
        Value::String(s) => label.find(s.as_str()).map(|start| start..start + s.len()),
        Value::Array(a) if a.len() == 2 => {
            let s = utf16_to_byte(&label, a[0].as_u64()? as usize);
            let e = utf16_to_byte(&label, a[1].as_u64()? as usize);
            Some(s..e)
        }
        _ => None,
    });
    Some(Signature {
        documentation: markup_to_markdown(&sig["documentation"]),
        param_documentation: param.and_then(|p| markup_to_markdown(&p["documentation"])),
        label,
        active_param,
        index,
        count: sigs.len(),
    })
}

/// `Location | Location[] | LocationLink[] | null` → (path, position).
pub fn parse_locations(v: &Value) -> Vec<(PathBuf, Position)> {
    let one = |l: &Value| -> Option<(PathBuf, Position)> {
        if let Some(uri) = l["targetUri"].as_str() {
            let r = range(&l["targetSelectionRange"]).or_else(|| range(&l["targetRange"]))?;
            return Some((uri_to_path(uri)?, r.start));
        }
        Some((uri_to_path(l["uri"].as_str()?)?, range(&l["range"])?.start))
    };
    match v {
        Value::Array(a) => a.iter().filter_map(one).collect(),
        Value::Object(_) => one(v).into_iter().collect(),
        _ => Vec::new(),
    }
}

#[derive(Clone, Debug)]
pub struct Hint {
    pub pos: Position,
    pub label: String,
    pub pad_left: bool,
    pub pad_right: bool,
    /// 1 = type, 2 = parameter.
    pub kind: u8,
}

pub fn parse_inlay_hints(v: &Value) -> Vec<Hint> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|h| {
                    let label = match &h["label"] {
                        Value::String(s) => s.clone(),
                        Value::Array(parts) => parts.iter().filter_map(|p| p["value"].as_str()).collect::<String>(),
                        _ => return None,
                    };
                    Some(Hint {
                        pos: position(&h["position"])?,
                        label,
                        pad_left: h["paddingLeft"].as_bool().unwrap_or(false),
                        pad_right: h["paddingRight"].as_bool().unwrap_or(false),
                        kind: h["kind"].as_u64().unwrap_or(0) as u8,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Clone, Debug)]
pub struct Action {
    pub title: String,
    pub kind: String,
    pub preferred: bool,
    pub disabled: Option<String>,
    pub edits: Option<Vec<FileEdits>>,
    /// `Command` object to run with `workspace/executeCommand`.
    pub command: Option<Value>,
    pub raw: Value,
}

pub fn parse_code_action(v: &Value) -> Option<Action> {
    let title = v["title"].as_str()?.to_string();
    // A bare `Command` has a string `command` field.
    if v["command"].is_string() {
        return Some(Action { title, kind: String::new(), preferred: false, disabled: None, edits: None, command: Some(v.clone()), raw: v.clone() });
    }
    Some(Action {
        title,
        kind: v["kind"].as_str().unwrap_or_default().to_string(),
        preferred: v["isPreferred"].as_bool().unwrap_or(false),
        disabled: v["disabled"]["reason"].as_str().map(str::to_string),
        edits: v.get("edit").filter(|e| !e.is_null()).map(parse_workspace_edit),
        command: v.get("command").filter(|c| c.is_object()).cloned(),
        raw: v.clone(),
    })
}

pub fn parse_code_actions(v: &Value) -> Vec<Action> {
    v.as_array().map(|a| a.iter().filter_map(parse_code_action).collect()).unwrap_or_default()
}

#[derive(Clone, Debug)]
pub struct Completion {
    pub label: String,
    pub label_detail: Option<String>,
    pub kind: u8,
    pub detail: Option<String>,
    pub filter_text: String,
    pub sort_text: String,
    pub insert_text: String,
    pub is_snippet: bool,
    /// Explicit replacement range from `textEdit` (otherwise: the word before the caret).
    pub edit_range: Option<Range>,
    pub additional: Vec<Edit>,
    pub documentation: Option<String>,
    pub preselect: bool,
    pub deprecated: bool,
    pub raw: Value,
}

pub fn parse_completion_item(v: &Value, defaults: &Value) -> Option<Completion> {
    let label = v["label"].as_str()?.to_string();
    let format = v["insertTextFormat"].as_u64().or_else(|| defaults["insertTextFormat"].as_u64()).unwrap_or(1);
    let (insert_text, edit_range) = match &v["textEdit"] {
        Value::Object(te) => {
            let r = te.get("range").and_then(range).or_else(|| te.get("insert").and_then(range));
            (te.get("newText").and_then(Value::as_str).unwrap_or(&label).to_string(), r)
        }
        _ => {
            let text = v["textEditText"].as_str().or_else(|| v["insertText"].as_str()).unwrap_or(&label).to_string();
            let r = range(&defaults["editRange"]).or_else(|| range(&defaults["editRange"]["insert"]));
            (text, r)
        }
    };
    Some(Completion {
        label_detail: v["labelDetails"]["detail"].as_str().or_else(|| v["labelDetails"]["description"].as_str()).map(str::to_string),
        kind: v["kind"].as_u64().unwrap_or(0) as u8,
        detail: v["detail"].as_str().map(str::to_string),
        filter_text: v["filterText"].as_str().unwrap_or(&label).to_string(),
        sort_text: v["sortText"].as_str().unwrap_or(&label).to_string(),
        insert_text,
        is_snippet: format == 2,
        edit_range,
        additional: v["additionalTextEdits"].as_array().map(|a| a.iter().filter_map(edit).collect()).unwrap_or_default(),
        documentation: markup_to_markdown(&v["documentation"]),
        preselect: v["preselect"].as_bool().unwrap_or(false),
        deprecated: v["deprecated"].as_bool().unwrap_or(false)
            || v["tags"].as_array().is_some_and(|t| t.iter().any(|x| x.as_u64() == Some(1))),
        label,
        raw: v.clone(),
    })
}

/// `CompletionList | CompletionItem[] | null` → (is_incomplete, items).
pub fn parse_completions(v: &Value) -> (bool, Vec<Completion>) {
    match v {
        Value::Array(items) => (false, items.iter().filter_map(|i| parse_completion_item(i, &Value::Null)).collect()),
        Value::Object(o) => {
            let defaults = o.get("itemDefaults").cloned().unwrap_or(Value::Null);
            let items = o.get("items").and_then(Value::as_array).map(|a| a.iter().filter_map(|i| parse_completion_item(i, &defaults)).collect()).unwrap_or_default();
            (o.get("isIncomplete").and_then(Value::as_bool).unwrap_or(false), items)
        }
        _ => (false, Vec::new()),
    }
}

/// Short badge + colour for a `CompletionItemKind` / `SymbolKind`.
pub fn kind_badge(kind: u8) -> (&'static str, u32) {
    match kind {
        2 | 3 => ("ƒ", 0xEFB080),       // method / function
        4 => ("c", 0xEFB080),           // constructor
        5 => ("f", 0xAA9BF5),           // field
        6 => ("v", 0xD6D6DD),           // variable
        7 | 22 => ("C", 0x87C3FF),      // class / struct
        8 => ("I", 0x87C3FF),           // interface
        9 => ("M", 0x9BB5E8),           // module
        10 => ("p", 0xAA9BF5),          // property
        13 => ("E", 0x87C3FF),          // enum
        14 => ("k", 0x83D6C5),          // keyword
        15 => ("s", 0xA8CC7C),          // snippet
        20 => ("e", 0xAA9BF5),          // enum member
        21 => ("π", 0xAA9BF5),          // constant
        25 => ("T", 0x82D2CE),          // type parameter
        _ => ("·", 0x8A8F98),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn completion_with_auto_import() {
        let v = json!({ "isIncomplete": true, "items": [{
            "label": "HashMap", "kind": 22, "detail": "std::collections",
            "textEdit": { "range": { "start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 7} }, "newText": "HashMap" },
            "additionalTextEdits": [{ "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0} }, "newText": "use std::collections::HashMap;\n" }]
        }]});
        let (incomplete, items) = parse_completions(&v);
        assert!(incomplete);
        assert_eq!(items[0].additional.len(), 1);
        assert_eq!(items[0].edit_range.unwrap().start.character, 4);
    }

    #[test]
    fn signature_offsets() {
        let v = json!({ "signatures": [{ "label": "connect(port: u16, host: &str)", "parameters": [{ "label": [8, 17] }, { "label": "host: &str" }] }], "activeParameter": 1 });
        let s = parse_signature_help(&v).unwrap();
        assert_eq!(&s.label[s.active_param.clone().unwrap()], "host: &str");
    }

    #[test]
    fn workspace_edit_both_shapes() {
        let a = json!({ "changes": { "file:///D:/x.rs": [{ "range": { "start": {"line":0,"character":0}, "end": {"line":0,"character":1} }, "newText": "y" }] } });
        assert_eq!(parse_workspace_edit(&a)[0].edits.len(), 1);
        let b = json!({ "documentChanges": [{ "textDocument": { "uri": "file:///D:/x.rs", "version": 3 }, "edits": [{ "range": { "start": {"line":0,"character":0}, "end": {"line":0,"character":1} }, "newText": "y" }] }] });
        assert_eq!(parse_workspace_edit(&b)[0].edits[0].new_text, "y");
    }

    #[test]
    fn snippet_text_edit_carries_cursor() {
        let v = json!({ "documentChanges": [{ "textDocument": { "uri": "file:///D:/x.rs", "version": 3 }, "edits": [
            { "range": { "start": {"line":1,"character":4}, "end": {"line":1,"character":4} }, "newText": "let ${0:var_name} = a + b;\n    ", "insertTextFormat": 2 },
            { "range": { "start": {"line":1,"character":8}, "end": {"line":1,"character":13} }, "newText": "var_name" }
        ] }] });
        let e = &parse_workspace_edit(&v)[0].edits;
        assert_eq!(e[0].new_text, "let var_name = a + b;\n    ");
        assert_eq!(e[0].cursor, Some(4..12));
        assert_eq!(e[1].cursor, None);
    }
}
