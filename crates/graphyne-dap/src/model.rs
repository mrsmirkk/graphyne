//! Debugger data model parsed from DAP responses.

use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Thread {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StackFrame {
    pub id: i64,
    pub name: String,
    pub path: Option<PathBuf>,
    /// 0-based line.
    pub line: usize,
    /// 0-based column.
    pub column: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Scope {
    pub name: String,
    pub reference: i64,
    pub expensive: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    /// > 0 when the variable has children.
    pub reference: i64,
}

pub fn parse_threads(body: &Value) -> Vec<Thread> {
    body["threads"]
        .as_array()
        .map(|a| a.iter().filter_map(|t| Some(Thread { id: t["id"].as_i64()?, name: t["name"].as_str().unwrap_or("thread").to_string() })).collect())
        .unwrap_or_default()
}

/// The frames of a `stackTrace` response. Frames the adapter marks as not the user's (js-debug:
/// Node internals and `skipFiles`) are left out, except the top one.
/// A frame of code the user can't open or doesn't care about: what the adapter marks so (Node
/// internals), frames without source, Rust's std / core / alloc and the program's start-up.
fn library_frame(f: &Value) -> bool {
    if f["source"]["presentationHint"] == "deemphasize" || f["presentationHint"] == "subtle" {
        return true;
    }
    if f["source"]["path"].as_str().is_none() && f["source"]["sourceReference"].as_i64().unwrap_or(0) == 0 {
        return true;
    }
    let name = f["name"].as_str().unwrap_or("").trim_start_matches('<');
    const LIBRARY: [&str; 11] = ["std::", "core::", "alloc::", "fn(", "panic_unwind::", "__rust", "invoke_main", "__scrt_common_main", "mainCRTStartup", "BaseThreadInitThunk", "RtlUserThreadStart"];
    LIBRARY.iter().any(|p| name.starts_with(p))
}

pub fn parse_stack(body: &Value) -> Vec<StackFrame> {
    let hidden = library_frame;
    body["stackFrames"]
        .as_array()
        .map(|a| {
            a.iter()
                .enumerate()
                .filter(|(i, f)| *i == 0 || !hidden(f))
                .map(|(_, f)| f)
                .filter_map(|f| {
                    Some(StackFrame {
                        id: f["id"].as_i64()?,
                        name: f["name"].as_str().unwrap_or("?").to_string(),
                        path: f["source"]["path"].as_str().map(PathBuf::from),
                        line: f["line"].as_u64().unwrap_or(1).saturating_sub(1) as usize,
                        column: f["column"].as_u64().unwrap_or(1).saturating_sub(1) as usize,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_scopes(body: &Value) -> Vec<Scope> {
    body["scopes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    Some(Scope {
                        name: s["name"].as_str().unwrap_or("Scope").to_string(),
                        reference: s["variablesReference"].as_i64()?,
                        expensive: s["expensive"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The interpreter's own bookkeeping, not the program's variables: Lua's loop state
/// (`(for state #1)`, `(for index)` in older versions) and temporaries.
fn internal_variable(name: &str) -> bool {
    name.starts_with("(for ") || matches!(name, "(temporary)" | "(*temporary)" | "(C temporary)")
}

pub fn parse_variables(body: &Value) -> Vec<Variable> {
    body["variables"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|v| !v["name"].as_str().is_some_and(internal_variable))
                .filter_map(|v| {
                    Some(Variable {
                        name: v["name"].as_str()?.to_string(),
                        value: v["value"].as_str().unwrap_or_default().to_string(),
                        type_name: v["type"].as_str().filter(|t| !t.is_empty()).map(str::to_string),
                        reference: v["variablesReference"].as_i64().unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A source breakpoint: 0-based line, optional condition (`x > 3`) and log message
/// (`total = {total}`, which turns it into a logpoint that never pauses).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BreakpointSpec {
    pub line: usize,
    pub condition: Option<String>,
    pub log_message: Option<String>,
}

impl BreakpointSpec {
    pub fn at(line: usize) -> Self {
        Self { line, ..Default::default() }
    }
}

/// `breakpoints` argument of `setBreakpoints` (0-based lines in, 1-based out).
pub fn source_breakpoints<'a>(specs: impl IntoIterator<Item = &'a BreakpointSpec>) -> Value {
    Value::Array(
        specs
            .into_iter()
            .map(|b| {
                let mut v = serde_json::json!({ "line": b.line + 1 });
                if let Some(c) = b.condition.as_deref().filter(|c| !c.trim().is_empty()) {
                    v["condition"] = c.into();
                }
                if let Some(m) = b.log_message.as_deref().filter(|m| !m.trim().is_empty()) {
                    v["logMessage"] = m.into();
                }
                v
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_loop_state_is_hidden() {
        let body = serde_json::json!({ "variables": [
            { "name": "total", "value": "0", "variablesReference": 0 },
            { "name": "(for state #1)", "value": "1", "variablesReference": 0 },
            { "name": "(for index)", "value": "1", "variablesReference": 0 },
            { "name": "i", "value": "1", "variablesReference": 0 },
        ] });
        let names: Vec<String> = parse_variables(&body).into_iter().map(|v| v.name).collect();
        assert_eq!(names, ["total", "i"]);
    }
    use serde_json::json;

    #[test]
    fn hides_frames_the_adapter_deemphasizes() {
        let internal = |id: i64| json!({ "id": id, "name": "load", "line": 1, "source": { "name": "loader", "presentationHint": "deemphasize" } });
        let user = json!({ "id": 2, "name": "main", "line": 7, "source": { "path": "/p/app.js" } });
        let ids = |frames: Vec<StackFrame>| frames.into_iter().map(|f| f.id).collect::<Vec<_>>();
        assert_eq!(ids(parse_stack(&json!({ "stackFrames": [user.clone(), internal(3), internal(4)] }))), [2]);
        // Paused inside Node itself: the top frame stays.
        assert_eq!(ids(parse_stack(&json!({ "stackFrames": [internal(1), user, internal(3)] }))), [1, 2]);
    }

    #[test]
    fn hides_rust_std_and_start_up_frames() {
        let frame = |id: i64, name: &str, path: Option<&str>| match path {
            Some(p) => json!({ "id": id, "name": name, "line": 1, "source": { "path": p } }),
            None => json!({ "id": id, "name": name, "line": 1 }),
        };
        let ids = |frames: Vec<StackFrame>| frames.into_iter().map(|f| f.id).collect::<Vec<_>>();
        let stack = json!({ "stackFrames": [
            frame(1, "rust_demo::fib", Some("C:/p/src/main.rs")),
            frame(2, "rust_demo::main", Some("C:/p/src/main.rs")),
            frame(3, "<fn() as core::ops::function::FnOnce<()>>::call_once", Some("/rustc/abc/library/core/src/ops/function.rs")),
            frame(4, "std::rt::lang_start::<()>::{closure#0}", Some("/rustc/abc/library/std/src/rt.rs")),
            frame(5, "invoke_main", None),
            frame(6, "BaseThreadInitThunk", None),
        ] });
        assert_eq!(ids(parse_stack(&stack)), [1, 2]);
    }

    #[test]
    fn parses_stack_and_variables() {
        let frames = parse_stack(&json!({ "stackFrames": [{ "id": 3, "name": "main", "line": 12, "column": 5, "source": { "path": "/p/a.py" } }] }));
        assert_eq!(frames[0].line, 11);
        assert_eq!(frames[0].path.as_deref(), Some(std::path::Path::new("/p/a.py")));
        let vars = parse_variables(&json!({ "variables": [{ "name": "total", "value": "70", "type": "int", "variablesReference": 0 }, { "name": "names", "value": "['ada']", "type": "list", "variablesReference": 7 }] }));
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[1].reference, 7);
        let specs = [BreakpointSpec::at(4), BreakpointSpec { line: 9, condition: Some("n > 2".into()), log_message: Some("n = {n}".into()) }];
        assert_eq!(source_breakpoints(&specs), json!([{ "line": 5 }, { "line": 10, "condition": "n > 2", "logMessage": "n = {n}" }]));
    }
}
