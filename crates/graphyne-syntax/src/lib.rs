//! Graphyne syntax layer.
//!
//! * Incremental parsing over a `ropey::Rope` (chunks fed directly, no full-string copy).
//! * Edit-adjusted trees: the UI calls [`apply_edit`] synchronously on every keystroke
//!   (cheap) and reparses in the background with the edited tree as a hint.
//! * Viewport-only highlighting via `QueryCursor::set_byte_range`.
//! * Outline / breadcrumbs / usage counts / structural extend-shrink selection.

use graphyne_core::{EditDelta, LanguageId};
use ropey::Rope;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::OnceLock;
use tree_sitter::{InputEdit, Language, Node, Parser, Point, Query, QueryCursor, StreamingIterator};

pub use tree_sitter::Tree;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HighlightKind {
    Plain,
    Keyword,
    String,
    Escape,
    Number,
    Comment,
    Function,
    Method,
    Macro,
    Type,
    TypeBuiltin,
    Constructor,
    Constant,
    ConstantBuiltin,
    Variable,
    VariableBuiltin,
    Parameter,
    Property,
    Attribute,
    Label,
    Operator,
    Punctuation,
    Tag,
    Module,
}

impl HighlightKind {
    pub fn from_capture(name: &str) -> HighlightKind {
        use HighlightKind::*;
        let starts = |p: &str| name == p || name.starts_with(&format!("{p}."));
        if starts("string.escape") || starts("escape") {
            Escape
        } else if starts("string.special") || starts("string") || starts("character") {
            String
        } else if starts("keyword") || starts("conditional") || starts("repeat") || starts("include") || starts("exception") || starts("storageclass") {
            Keyword
        } else if starts("number") || starts("float") || starts("constant.numeric") {
            Number
        } else if starts("comment") {
            Comment
        } else if starts("function.macro") {
            Macro
        } else if starts("function.method") || starts("method") {
            Method
        } else if starts("function") {
            Function
        } else if starts("type.builtin") {
            TypeBuiltin
        } else if starts("type") {
            Type
        } else if starts("constructor") {
            Constructor
        } else if starts("constant.builtin") || starts("boolean") {
            ConstantBuiltin
        } else if starts("constant") {
            Constant
        } else if starts("variable.parameter") || starts("parameter") {
            Parameter
        } else if starts("variable.builtin") {
            VariableBuiltin
        } else if starts("variable.member") || starts("property") || starts("field") {
            Property
        } else if starts("variable") {
            Variable
        } else if starts("attribute") {
            Attribute
        } else if starts("label") {
            Label
        } else if starts("operator") {
            Operator
        } else if starts("punctuation") {
            Punctuation
        } else if starts("tag") {
            Tag
        } else if starts("module") || starts("namespace") {
            Module
        } else {
            Plain
        }
    }
}

pub struct LanguageConfig {
    pub id: LanguageId,
    pub language: Language,
    pub highlights: Option<Query>,
    capture_kinds: Vec<HighlightKind>,
}

fn build(id: LanguageId, language: Language, query_src: &str) -> LanguageConfig {
    let highlights = match Query::new(&language, query_src) {
        Ok(q) => Some(q),
        Err(err) => {
            log::warn!("highlight query for {id:?} failed to compile: {err}");
            None
        }
    };
    let capture_kinds = highlights
        .as_ref()
        .map(|q| q.capture_names().iter().map(|n| HighlightKind::from_capture(n)).collect())
        .unwrap_or_default();
    LanguageConfig { id, language, highlights, capture_kinds }
}

/// Lazily-built, process-wide language registry.
pub fn config_for(id: LanguageId) -> Option<&'static LanguageConfig> {
    macro_rules! cached {
        ($id:expr, $lang:expr, $query:expr) => {{
            static CELL: OnceLock<LanguageConfig> = OnceLock::new();
            Some(CELL.get_or_init(|| build($id, $lang.into(), &$query)))
        }};
    }
    match id {
        LanguageId::Rust => cached!(id, tree_sitter_rust::LANGUAGE, tree_sitter_rust::HIGHLIGHTS_QUERY),
        LanguageId::Python => cached!(id, tree_sitter_python::LANGUAGE, tree_sitter_python::HIGHLIGHTS_QUERY),
        LanguageId::JavaScript => cached!(
            id,
            tree_sitter_javascript::LANGUAGE,
            format!("{}\n{}", tree_sitter_javascript::HIGHLIGHT_QUERY, tree_sitter_javascript::JSX_HIGHLIGHT_QUERY)
        ),
        LanguageId::TypeScript => cached!(
            id,
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            format!("{}\n{}", tree_sitter_javascript::HIGHLIGHT_QUERY, tree_sitter_typescript::HIGHLIGHTS_QUERY)
        ),
        LanguageId::Tsx => cached!(
            id,
            tree_sitter_typescript::LANGUAGE_TSX,
            format!(
                "{}\n{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            )
        ),
        LanguageId::Json => cached!(id, tree_sitter_json::LANGUAGE, tree_sitter_json::HIGHLIGHTS_QUERY),
        LanguageId::Toml => cached!(id, tree_sitter_toml_ng::LANGUAGE, tree_sitter_toml_ng::HIGHLIGHTS_QUERY),
        LanguageId::Go => cached!(id, tree_sitter_go::LANGUAGE, tree_sitter_go::HIGHLIGHTS_QUERY),
        LanguageId::C => cached!(id, tree_sitter_c::LANGUAGE, tree_sitter_c::HIGHLIGHT_QUERY),
        LanguageId::Cpp => cached!(
            id,
            tree_sitter_cpp::LANGUAGE,
            format!("{}\n{}", tree_sitter_c::HIGHLIGHT_QUERY, tree_sitter_cpp::HIGHLIGHT_QUERY)
        ),
        LanguageId::CSharp => cached!(id, tree_sitter_c_sharp::LANGUAGE, tree_sitter_c_sharp::HIGHLIGHTS_QUERY),
        LanguageId::Java => cached!(id, tree_sitter_java::LANGUAGE, tree_sitter_java::HIGHLIGHTS_QUERY),
        LanguageId::Kotlin => cached!(id, tree_sitter_kotlin_ng::LANGUAGE, include_str!("../queries/kotlin/highlights.scm")),
        LanguageId::Php => cached!(id, tree_sitter_php::LANGUAGE_PHP, tree_sitter_php::HIGHLIGHTS_QUERY),
        LanguageId::Dart => cached!(id, tree_sitter_dart::LANGUAGE, tree_sitter_dart::HIGHLIGHTS_QUERY),
        LanguageId::Lua => cached!(id, tree_sitter_lua::LANGUAGE, tree_sitter_lua::HIGHLIGHTS_QUERY),
        LanguageId::Ruby => cached!(id, tree_sitter_ruby::LANGUAGE, tree_sitter_ruby::HIGHLIGHTS_QUERY),
        LanguageId::Scala => cached!(id, tree_sitter_scala::LANGUAGE, tree_sitter_scala::HIGHLIGHTS_QUERY),
        LanguageId::Swift => cached!(id, tree_sitter_swift::LANGUAGE, tree_sitter_swift::HIGHLIGHTS_QUERY),
        LanguageId::Html => cached!(id, tree_sitter_html::LANGUAGE, tree_sitter_html::HIGHLIGHTS_QUERY),
        LanguageId::Css => cached!(id, tree_sitter_css::LANGUAGE, tree_sitter_css::HIGHLIGHTS_QUERY),
        LanguageId::Sql => cached!(id, tree_sitter_sequel::LANGUAGE, tree_sitter_sequel::HIGHLIGHTS_QUERY),
        LanguageId::Yaml => cached!(id, tree_sitter_yaml::LANGUAGE, tree_sitter_yaml::HIGHLIGHTS_QUERY),
        LanguageId::Shell => cached!(id, tree_sitter_bash::LANGUAGE, tree_sitter_bash::HIGHLIGHT_QUERY),
        LanguageId::Plugin(ix) => PLUGIN_GRAMMARS.read().unwrap_or_else(|e| e.into_inner()).get(&ix).copied(),
        _ => None,
    }
}

static PLUGIN_GRAMMARS: std::sync::RwLock<std::collections::BTreeMap<u16, &'static LanguageConfig>> = std::sync::RwLock::new(std::collections::BTreeMap::new());

/// Load an extension's tree-sitter grammar from a native library (`symbol` returns the
/// `TSLanguage*`) with its highlights query. The library stays loaded for the process
/// lifetime (the language points into it). Returns a warning when the query is invalid.
pub fn load_plugin_grammar(id: LanguageId, library: &std::path::Path, symbol: &str, highlights: &str) -> Result<Option<String>, String> {
    let LanguageId::Plugin(ix) = id else { return Err("not an extension language".into()) };
    // SAFETY: loading a library runs its initialisers; the IDE only calls this for
    // extensions the user granted the "native" permission.
    let lib = unsafe { libloading::Library::new(library) }.map_err(|e| format!("{}: {e}", library.display()))?;
    let func: unsafe extern "C" fn() -> *const () = unsafe { *lib.get(symbol.as_bytes()).map_err(|e| format!("{symbol}: {e}"))? };
    // SAFETY: tree-sitter grammars export `const TSLanguage *tree_sitter_<name>(void)`.
    let language: Language = unsafe { tree_sitter_language::LanguageFn::from_raw(func) }.into();
    Parser::new().set_language(&language).map_err(|e| format!("incompatible grammar: {e}"))?;
    std::mem::forget(lib);
    let config: &'static LanguageConfig = Box::leak(Box::new(build(id, language, highlights)));
    let warning = config.highlights.is_none().then(|| "highlights query failed to compile: no colours".to_string());
    PLUGIN_GRAMMARS.write().unwrap_or_else(|e| e.into_inner()).insert(ix, config);
    Ok(warning)
}

pub fn supports(id: LanguageId) -> bool {
    config_for(id).is_some()
}

fn to_point(p: graphyne_core::TextPoint) -> Point {
    Point { row: p.row, column: p.column }
}

/// Apply a buffer edit to an existing tree so its node positions stay valid until reparse.
pub fn apply_edit(tree: &mut Tree, delta: &EditDelta) {
    tree.edit(&InputEdit {
        start_byte: delta.start_byte,
        old_end_byte: delta.old_end_byte,
        new_end_byte: delta.new_end_byte,
        start_position: to_point(delta.start_point),
        old_end_position: to_point(delta.old_end_point),
        new_end_position: to_point(delta.new_end_point),
    });
}

/// (Re)parse `rope`, reusing `old` (which must already have had edits applied).
pub fn parse(id: LanguageId, rope: &Rope, old: Option<&Tree>) -> Option<Tree> {
    let config = config_for(id)?;
    let mut parser = Parser::new();
    parser.set_language(&config.language).ok()?;
    let len = rope.len_bytes();
    let mut callback = |byte: usize, _: Point| -> &[u8] {
        if byte >= len {
            return &[];
        }
        let (chunk, chunk_byte, _, _) = rope.chunk_at_byte(byte);
        &chunk.as_bytes()[byte - chunk_byte..]
    };
    parser.parse_with_options(&mut callback, old, None)
}

/// A highlighted byte range (absolute buffer offsets).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub range: Range<usize>,
    pub kind: HighlightKind,
}

fn rope_text_for(rope: &Rope, range: Range<usize>) -> String {
    let len = rope.len_bytes();
    let end = range.end.min(len);
    let start = range.start.min(end);
    let s = rope.byte_to_char(start);
    let e = rope.byte_to_char(end);
    rope.slice(s..e).to_string()
}

/// Highlight spans intersecting `byte_range`, sorted and non-overlapping.
/// Inner captures override outer ones; for identical ranges the first pattern wins.
pub fn highlight(id: LanguageId, tree: &Tree, rope: &Rope, byte_range: Range<usize>) -> Vec<HighlightSpan> {
    let Some(config) = config_for(id) else { return Vec::new() };
    let Some(query) = config.highlights.as_ref() else { return Vec::new() };
    let len = rope.len_bytes();
    let start = byte_range.start.min(len);
    let end = byte_range.end.min(len);
    if start >= end {
        return Vec::new();
    }

    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(start..end);
    let provider = |node: Node| std::iter::once(rope_text_for(rope, node.byte_range()));
    let mut raw: Vec<(Range<usize>, HighlightKind)> = Vec::new();
    let mut captures = cursor.captures(query, tree.root_node(), provider);
    while let Some((m, ix)) = captures.next() {
        let cap = m.captures[*ix];
        let r = cap.node.byte_range();
        let kind = config.capture_kinds.get(cap.index as usize).copied().unwrap_or(HighlightKind::Plain);
        // Editor-only captures (`@spell`, `@nospell`, `@conceal`…) must not reset a colour
        // set by the same pattern: `(comment) @comment @spell`.
        if kind == HighlightKind::Plain {
            continue;
        }
        // tree-sitter-highlight semantics: for the same node, the later pattern wins.
        if let Some(existing) = raw.iter_mut().rev().take(8).find(|(rr, _)| *rr == r) {
            existing.1 = kind;
            continue;
        }
        raw.push((r, kind));
    }

    // Paint into a per-byte map of the window; later (inner) captures override.
    raw.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(b.0.end.cmp(&a.0.end)));
    let mut paint: Vec<HighlightKind> = vec![HighlightKind::Plain; end - start];
    for (r, kind) in raw {
        if kind == HighlightKind::Plain {
            continue;
        }
        let s = r.start.max(start) - start;
        let e = r.end.min(end).saturating_sub(start);
        if s < e {
            paint[s..e].fill(kind);
        }
    }
    let mut spans = Vec::new();
    let mut i = 0;
    while i < paint.len() {
        let kind = paint[i];
        let mut j = i + 1;
        while j < paint.len() && paint[j] == kind {
            j += 1;
        }
        if kind != HighlightKind::Plain {
            spans.push(HighlightSpan { range: start + i..start + j, kind });
        }
        i = j;
    }
    spans
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Trait,
    Impl,
    Module,
    Interface,
}

impl SymbolKind {
    pub fn badge(&self) -> &'static str {
        match self {
            SymbolKind::Function => "ƒ",
            SymbolKind::Method => "m",
            SymbolKind::Class => "C",
            SymbolKind::Struct => "S",
            SymbolKind::Enum => "E",
            SymbolKind::Trait => "T",
            SymbolKind::Impl => "I",
            SymbolKind::Module => "M",
            SymbolKind::Interface => "I",
        }
    }
}

fn symbol_kind(kind: &str) -> Option<SymbolKind> {
    Some(match kind {
        "function_item" | "function_definition" | "function_declaration" | "function_signature_item" | "generator_function_declaration"
        | "function_signature" => SymbolKind::Function,
        "method_declaration" | "method_definition" | "method_spec" | "constructor_declaration" | "method" | "singleton_method" => SymbolKind::Method,
        "class_definition" | "class_declaration" | "class_specifier" | "abstract_class_declaration" | "class" | "object_declaration"
        | "object_definition" => SymbolKind::Class,
        "struct_item" | "struct_specifier" | "type_spec" | "struct_declaration" | "record_declaration" => SymbolKind::Struct,
        "enum_item" | "enum_declaration" | "enum_specifier" => SymbolKind::Enum,
        "trait_item" | "trait_declaration" | "trait_definition" => SymbolKind::Trait,
        "impl_item" => SymbolKind::Impl,
        "mod_item" | "namespace_definition" | "namespace_declaration" | "module" => SymbolKind::Module,
        "interface_declaration" | "type_alias_declaration" | "protocol_declaration" => SymbolKind::Interface,
        _ => return None,
    })
}

fn node_text(node: Node, rope: &Rope) -> String {
    rope_text_for(rope, node.byte_range())
}

fn symbol_name(node: Node, rope: &Rope) -> Option<String> {
    if node.kind() == "impl_item" {
        let ty = node.child_by_field_name("type").map(|n| node_text(n, rope))?;
        return Some(match node.child_by_field_name("trait") {
            Some(t) => format!("impl {} for {ty}", node_text(t, rope)),
            None => format!("impl {ty}"),
        });
    }
    if let Some(name) = node.child_by_field_name("name") {
        return Some(node_text(name, rope));
    }
    // C/C++: function_definition -> declarator -> function_declarator -> declarator
    let mut cur = node.child_by_field_name("declarator");
    for _ in 0..5 {
        let n = cur?;
        if n.kind().contains("identifier") {
            return Some(node_text(n, rope));
        }
        cur = n.child_by_field_name("declarator").or_else(|| n.child_by_field_name("name"));
    }
    None
}

#[derive(Clone, Debug)]
pub struct OutlineItem {
    pub name: String,
    pub kind: SymbolKind,
    pub depth: usize,
    /// 0-based row of the symbol start.
    pub row: usize,
    /// 0-based row of the name (where CodeLens is anchored).
    pub name_row: usize,
    /// Byte offset of the symbol's name (where reference queries are anchored).
    pub name_byte: usize,
    pub byte_range: Range<usize>,
}

/// Code folding regions: `(first row, last row hidden when folded)`, sorted by first row,
/// one per first row (the largest). Any named node spanning several lines can fold — function
/// and class bodies, blocks, multi-line calls, literals, comments. When the node's last line
/// only closes it (`}`, `)`, `]`, `end`, `</tag>`), that line stays visible, as in VS Code.
pub fn fold_ranges(tree: &Tree, rope: &Rope) -> Vec<(usize, usize)> {
    let mut found: Vec<(usize, usize)> = Vec::new();
    let mut cursor = tree.walk();
    let mut depth = 0usize;
    loop {
        let node = cursor.node();
        let start = node.start_position().row;
        let end_pos = node.end_position();
        // A node that ends with its newline ends at column 0 of the next line.
        let end = if end_pos.column == 0 && end_pos.row > start { end_pos.row - 1 } else { end_pos.row };
        // A node that begins exactly where its first named child does (Python's `block`
        // starting at its first statement, `foo(...)` at `foo`) folds as that child or a
        // sibling instead; taking it would fold the wrong rows from that line.
        let wraps_first_child = node.named_child(0).is_some_and(|c| c.start_byte() == node.start_byte());
        if depth > 0 && end > start && node.is_named() && !wraps_first_child {
            found.push((start, end));
        }
        // Single-line nodes can't contain multi-line ones: don't descend.
        if end > start && cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return finish_folds(found, rope);
            }
            depth -= 1;
        }
    }
}

fn finish_folds(mut found: Vec<(usize, usize)>, rope: &Rope) -> Vec<(usize, usize)> {
    found.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    found.dedup_by_key(|f| f.0);
    found
        .into_iter()
        .filter_map(|(start, end)| {
            let closes = end < rope.len_lines() && closing_line(&rope.line(end).to_string());
            let last_hidden = if closes { end - 1 } else { end };
            (last_hidden > start).then_some((start, last_hidden))
        })
        .collect()
}

/// A line that only closes a block: starts with a closing bracket, `</` or the word `end`.
fn closing_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with(['}', ')', ']']) || t.starts_with("</") || (t.starts_with("end") && !t[3..].starts_with(|c: char| c.is_alphanumeric() || c == '_'))
}

/// Document outline (structure view, CodeLens anchors).
pub fn outline(tree: &Tree, rope: &Rope) -> Vec<OutlineItem> {
    let mut out = Vec::new();
    fn walk(node: Node, rope: &Rope, depth: usize, out: &mut Vec<OutlineItem>) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            let mut next_depth = depth;
            if let Some(mut kind) = symbol_kind(child.kind()) {
                if let Some(name) = symbol_name(child, rope) {
                    if kind == SymbolKind::Function && depth > 0 {
                        kind = SymbolKind::Method;
                    }
                    let name_byte = child
                        .child_by_field_name("name")
                        .map(|n| n.start_byte())
                        .or_else(|| node_text(child, rope).find(name.as_str()).map(|i| child.start_byte() + i))
                        .unwrap_or(child.start_byte());
                    out.push(OutlineItem {
                        name_byte,
                        name,
                        kind,
                        depth,
                        row: child.start_position().row,
                        name_row: child.child_by_field_name("name").map(|n| n.start_position().row).unwrap_or(child.start_position().row),
                        byte_range: child.byte_range(),
                    });
                    next_depth = depth + 1;
                }
            }
            if out.len() < 5000 {
                walk(child, rope, next_depth, out);
            }
        }
    }
    walk(tree.root_node(), rope, 0, &mut out);
    out
}

/// Count of every identifier text in the file (drives the `N usages` CodeLens until LSP
/// `textDocument/references` is wired in).
pub fn identifier_counts(tree: &Tree, rope: &Rope) -> HashMap<String, u32> {
    let mut counts = HashMap::new();
    let mut cursor = tree.walk();
    let mut visited_children = false;
    loop {
        let node = cursor.node();
        if !visited_children {
            // PHP calls its identifiers `name`, Ruby its class names `constant`.
            if node.child_count() == 0 && (node.kind().ends_with("identifier") || matches!(node.kind(), "name" | "constant")) {
                *counts.entry(node_text(node, rope)).or_insert(0) += 1;
            }
            if cursor.goto_first_child() {
                continue;
            }
        }
        if cursor.goto_next_sibling() {
            visited_children = false;
        } else if cursor.goto_parent() {
            visited_children = true;
        } else {
            break;
        }
    }
    counts
}

/// Names of the symbols enclosing `byte` (outermost first) for the breadcrumbs.
pub fn enclosing_symbols(tree: &Tree, rope: &Rope, byte: usize) -> Vec<String> {
    let mut names = Vec::new();
    let Some(mut node) = tree.root_node().descendant_for_byte_range(byte, byte) else { return names };
    loop {
        if symbol_kind(node.kind()).is_some() {
            if let Some(name) = symbol_name(node, rope) {
                names.push(name);
            }
        }
        match node.parent() {
            Some(p) => node = p,
            None => break,
        }
    }
    names.reverse();
    names
}

/// The outermost expression that ends exactly at byte `end` — the receiver of a postfix
/// template once `.key` has been removed (`vec.iter().map(f)`, `a + b`, a multi-line
/// chain…). Climbs from the token before `end` through ancestors ending at `end`, stopping
/// at statements, declarations, blocks and argument wrappers.
pub fn expression_ending_at(tree: &Tree, end: usize) -> Option<Range<usize>> {
    if end == 0 {
        return None;
    }
    let hard_stop = |k: &str| {
        ["statement", "declaration", "declarator", "definition", "block", "body", "program", "source_file", "module", "translation_unit", "compilation_unit", "assignment", "clause"]
            .iter()
            .any(|p| k.contains(p))
            || k == "ERROR"
    };
    let wrapper = |k: &str| k.contains("argument") || k == "pair" || k.contains("parameter") || k == "interpolation" || k == "expression_list";
    let mut node = tree.root_node().descendant_for_byte_range(end - 1, end)?;
    let mut best: Option<Node> = None;
    loop {
        if node.end_byte() != end || hard_stop(node.kind()) {
            break;
        }
        if node.is_named() {
            if wrapper(node.kind()) {
                if best.is_some() {
                    break;
                }
            } else {
                best = Some(node);
            }
        }
        match node.parent() {
            Some(p) => node = p,
            None => break,
        }
    }
    best.map(|n| n.byte_range())
}

/// Kind and byte range of the smallest named node at `byte` (Lua `buffer.node_at_cursor`).
pub fn node_at(tree: &Tree, byte: usize) -> Option<(String, Range<usize>)> {
    let mut node = tree.root_node().descendant_for_byte_range(byte, byte)?;
    while !node.is_named() {
        node = node.parent()?;
    }
    Some((node.kind().to_string(), node.byte_range()))
}

/// Structural "Extend Selection" (Ctrl+W): the smallest node strictly larger than `range`.
pub fn expand_selection(tree: &Tree, range: Range<usize>) -> Option<Range<usize>> {
    let mut node = tree.root_node().descendant_for_byte_range(range.start, range.end)?;
    loop {
        let r = node.byte_range();
        if r.start <= range.start && r.end >= range.end && (r.start < range.start || r.end > range.end) {
            return Some(r);
        }
        node = node.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_highlight_and_outline() {
        let src = "fn connect(port: u16) -> u16 {\n    let x = port;\n    x\n}\nstruct A;\n";
        let rope = Rope::from_str(src);
        let tree = parse(LanguageId::Rust, &rope, None).expect("parse");
        let spans = highlight(LanguageId::Rust, &tree, &rope, 0..src.len());
        assert!(spans.iter().any(|s| s.kind == HighlightKind::Keyword && &src[s.range.clone()] == "fn"));
        assert!(spans.iter().any(|s| s.kind == HighlightKind::Function && &src[s.range.clone()] == "connect"));
        let items = outline(&tree, &rope);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "connect");
        let crumbs = enclosing_symbols(&tree, &rope, src.find("let").unwrap());
        assert_eq!(crumbs, vec!["connect".to_string()]);
        let counts = identifier_counts(&tree, &rope);
        assert_eq!(counts.get("port"), Some(&2));
        let sel = expand_selection(&tree, 35..36).unwrap();
        assert!(sel.start <= 35 && sel.end >= 36);
    }

    #[test]
    fn fold_ranges_keep_closing_lines_visible() {
        let src = "fn main() {\n    let v = vec![\n        1,\n        2,\n    ];\n    println!(\"{v:?}\");\n}\n";
        let rope = Rope::from_str(src);
        let tree = parse(LanguageId::Rust, &rope, None).expect("parse");
        // fn body: rows 0..=5 hidden from row 1 (the `}` on row 6 stays); vec! args: 1..=3.
        assert_eq!(fold_ranges(&tree, &rope), vec![(0, 5), (1, 3)]);

        let py = "def f(x):\n    if x:\n        return 1\n    return 2\n";
        let rope = Rope::from_str(py);
        let tree = parse(LanguageId::Python, &rope, None).expect("parse");
        // Python has no closing line: the whole body hides.
        assert_eq!(fold_ranges(&tree, &rope), vec![(0, 3), (1, 2)]);
    }

    #[test]
    fn all_queries_compile() {
        for id in [
            LanguageId::Rust,
            LanguageId::Python,
            LanguageId::JavaScript,
            LanguageId::TypeScript,
            LanguageId::Tsx,
            LanguageId::Json,
            LanguageId::Toml,
            LanguageId::Go,
            LanguageId::C,
            LanguageId::Cpp,
        ] {
            let c = config_for(id).unwrap();
            assert!(c.highlights.is_some(), "{id:?} query failed");
        }
    }

    #[test]
    fn postfix_receiver() {
        let check = |id: LanguageId, src: &str, before: &str, want: &str| {
            let rope = Rope::from_str(src);
            let tree = parse(id, &rope, None).expect("parse");
            let end = src.find(before).expect("marker") + before.len();
            let r = expression_ending_at(&tree, end).unwrap_or(0..0);
            assert_eq!(&src[r], want, "{id:?} {src:?}");
        };
        check(LanguageId::Rust, "fn f() { let v = vec.iter().map(g); }", "map(g)", "vec.iter().map(g)");
        check(LanguageId::Rust, "fn f() { a + b }", "a + b", "a + b");
        check(LanguageId::Rust, "fn f() { foo(a, items) }", "items", "items");
        check(LanguageId::Rust, "fn f() {
    list
        .iter()
        .count()
}", ".count()", "list
        .iter()
        .count()");
        check(LanguageId::Python, "def f():
    print(\"hello world\")
", "\"hello world\"", "\"hello world\"");
        check(LanguageId::Python, "f(x=items)
", "items", "items");
        check(LanguageId::TypeScript, "const n = await fetch(url)", "fetch(url)", "await fetch(url)");
        check(LanguageId::Go, "package m
func f() { a, b := g(), items }", "items", "items");
    }

    /// The spec §6.3 languages beyond the original ten: query compiles, comments and a
    /// keyword come out coloured.
    #[test]
    fn matrix_languages_highlight() {
        let cases: &[(LanguageId, &str, &str, Option<&str>)] = &[
            (LanguageId::CSharp, "// hi\npublic class Foo { void Bar() { var x = 1; } }", "// hi", Some("class")),
            (LanguageId::Java, "// hi\npublic class Foo { void bar() { int x = 1; } }", "// hi", Some("class")),
            (LanguageId::Kotlin, "// hi\nfun main() { val x = listOf(1) }", "// hi", Some("fun")),
            (LanguageId::Php, "<?php\n// hi\nfunction foo() { return 1; }", "// hi", Some("function")),
            (LanguageId::Dart, "// hi\nclass A { void f() { return; } }", "// hi", Some("class")),
            (LanguageId::Lua, "-- hi\nlocal x = 1", "-- hi", Some("local")),
            (LanguageId::Ruby, "# hi\ndef foo; end", "# hi", Some("def")),
            (LanguageId::Scala, "// hi\nobject A { def f = 1 }", "// hi", Some("object")),
            (LanguageId::Swift, "// hi\nfunc f() { let x = 1 }", "// hi", Some("func")),
            (LanguageId::Html, "<!-- hi -->\n<div class=\"a\">x</div>", "<!-- hi -->", None),
            (LanguageId::Css, "/* hi */\na { color: red; }", "/* hi */", None),
            (LanguageId::Sql, "-- hi\nSELECT a FROM t;", "-- hi", Some("SELECT")),
            (LanguageId::Yaml, "# hi\nkey: value", "# hi", None),
            (LanguageId::Shell, "# hi\nif true; then echo $HOME; fi", "# hi", Some("if")),
        ];
        for &(id, src, comment, keyword) in cases {
            assert!(config_for(id).and_then(|c| c.highlights.as_ref()).is_some(), "{id:?} query failed to compile");
            let rope = Rope::from_str(src);
            let tree = parse(id, &rope, None).expect("parse");
            let spans = highlight(id, &tree, &rope, 0..src.len());
            let has = |kind: HighlightKind, text: &str| spans.iter().any(|s| s.kind == kind && &src[s.range.clone()] == text);
            assert!(has(HighlightKind::Comment, comment), "{id:?}: comment not highlighted: {spans:?}");
            if let Some(kw) = keyword {
                assert!(has(HighlightKind::Keyword, kw), "{id:?}: keyword {kw:?} not highlighted: {spans:?}");
            }
        }
    }
}

#[cfg(test)]
mod precedence {
    use super::*;
    #[test]
    fn later_pattern_wins() {
        let src = "struct A { scroll_y: u32 }
fn f(a: A) { a.scroll_y; self.x(); }
";
        let rope = Rope::from_str(src);
        let tree = parse(LanguageId::Rust, &rope, None).unwrap();
        let spans = highlight(LanguageId::Rust, &tree, &rope, 0..src.len());
        let kind_at = |needle: &str| {
            let at = src.find(needle).unwrap();
            spans.iter().find(|s| s.range.start == at).map(|s| s.kind)
        };
        assert_eq!(kind_at("scroll_y"), Some(HighlightKind::Property));
        assert_eq!(kind_at("x()"), Some(HighlightKind::Method));
    }
}
