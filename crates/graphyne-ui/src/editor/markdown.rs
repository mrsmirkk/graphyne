//! Minimal Markdown → GPUI renderer for documentation popups (hover, completion docs).
//! Parsing (and code-fence syntax highlighting) happens once when the popup content
//! arrives; rendering every frame only builds elements.

use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{Div, FontStyle, FontWeight, HighlightStyle, SharedString, StyledText, div, px};
use graphyne_core::LanguageId;
use ropey::Rope;
use std::ops::Range;

#[derive(Clone)]
pub enum MdBlock {
    Code { text: SharedString, highlights: Vec<(Range<usize>, HighlightStyle)> },
    Para { text: SharedString, highlights: Vec<(Range<usize>, HighlightStyle)>, bullet: bool },
    Heading(SharedString),
    Rule,
}

fn fence_language(tag: &str) -> Option<LanguageId> {
    Some(match tag.trim().to_ascii_lowercase().as_str() {
        "rust" | "rs" => LanguageId::Rust,
        "python" | "py" => LanguageId::Python,
        "typescript" | "ts" => LanguageId::TypeScript,
        "tsx" => LanguageId::Tsx,
        "javascript" | "js" => LanguageId::JavaScript,
        "go" => LanguageId::Go,
        "c" => LanguageId::C,
        "cpp" | "c++" => LanguageId::Cpp,
        "json" => LanguageId::Json,
        "toml" => LanguageId::Toml,
        "php" => LanguageId::Php,
        "java" => LanguageId::Java,
        "kotlin" | "kt" => LanguageId::Kotlin,
        "scala" => LanguageId::Scala,
        "csharp" | "cs" | "c#" => LanguageId::CSharp,
        "ruby" | "rb" => LanguageId::Ruby,
        "lua" => LanguageId::Lua,
        "dart" => LanguageId::Dart,
        "swift" => LanguageId::Swift,
        "sh" | "bash" | "shell" => LanguageId::Shell,
        "sql" => LanguageId::Sql,
        "html" => LanguageId::Html,
        "css" => LanguageId::Css,
        _ => return None,
    })
}

fn highlight_code(code: &str, lang: Option<LanguageId>) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(lang) = lang else { return Vec::new() };
    let rope = Rope::from_str(code);
    let Some(tree) = graphyne_syntax::parse(lang, &rope, None) else { return Vec::new() };
    graphyne_syntax::highlight(lang, &tree, &rope, 0..code.len())
        .into_iter()
        .map(|s| (s.range, HighlightStyle { color: Some(theme::syntax(s.kind)), ..Default::default() }))
        .collect()
}

/// Inline markup: `code`, **bold** / __bold__, *italic* / _italic_, [link](url), `\` escapes.
/// Underscores mark emphasis only at a word's edge, so `snake_case` stays as it is.
fn inline(src: &str) -> (String, Vec<(Range<usize>, HighlightStyle)>) {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::new();
    let mut hl = Vec::new();
    let mut i = 0;
    let push_styled = |out: &mut String, hl: &mut Vec<(Range<usize>, HighlightStyle)>, text: &str, style: HighlightStyle| {
        let start = out.len();
        out.push_str(text);
        if !text.is_empty() {
            hl.push((start..out.len(), style));
        }
    };
    while i < chars.len() {
        let ch = chars[i];
        if ch == '\\' && i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() {
            out.push(chars[i + 1]);
            i += 2;
        } else if ch == '`' {
            if let Some(end) = chars[i + 1..].iter().position(|c| *c == '`') {
                let code: String = chars[i + 1..i + 1 + end].iter().collect();
                push_styled(&mut out, &mut hl, &code, HighlightStyle {
                    color: Some(color(0xE3D7C2)),
                    background_color: Some(color_a(c::TEXT, 0.07)),
                    ..Default::default()
                });
                i += end + 2;
            } else {
                out.push(ch);
                i += 1;
            }
        } else if ch == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            if let Some(end) = (i + 2..chars.len().saturating_sub(1)).find(|j| chars[*j] == '*' && chars[*j + 1] == '*') {
                let text: String = chars[i + 2..end].iter().collect();
                push_styled(&mut out, &mut hl, &text, HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), color: Some(color(c::TEXT)), ..Default::default() });
                i = end + 2;
            } else {
                out.push(ch);
                i += 1;
            }
        } else if ch == '*' && i + 1 < chars.len() && !chars[i + 1].is_whitespace() {
            if let Some(end) = chars[i + 1..].iter().position(|c| *c == '*') {
                let text: String = chars[i + 1..i + 1 + end].iter().collect();
                push_styled(&mut out, &mut hl, &text, HighlightStyle { font_style: Some(FontStyle::Italic), ..Default::default() });
                i += end + 2;
            } else {
                out.push(ch);
                i += 1;
            }
        } else if ch == '_' && (i == 0 || !chars[i - 1].is_alphanumeric()) && i + 1 < chars.len() {
            // __bold__ or _italic_, closed by underscores that end a word.
            let double = chars[i + 1] == '_';
            let open = if double { 2 } else { 1 };
            let closes_at = |j: usize| {
                let end = j + open;
                (if double { j + 1 < chars.len() && chars[j] == '_' && chars[j + 1] == '_' } else { chars[j] == '_' })
                    && j > i + open
                    && !chars[j - 1].is_whitespace()
                    && chars.get(end).is_none_or(|c| !c.is_alphanumeric())
            };
            match (i + open..chars.len()).find(|&j| closes_at(j)) {
                Some(end) if chars.get(i + open).is_some_and(|c| !c.is_whitespace()) => {
                    let text: String = chars[i + open..end].iter().collect();
                    let style = if double {
                        HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), color: Some(color(c::TEXT)), ..Default::default() }
                    } else {
                        HighlightStyle { font_style: Some(FontStyle::Italic), ..Default::default() }
                    };
                    push_styled(&mut out, &mut hl, &text, style);
                    i = end + open;
                }
                _ => {
                    // Not closed: the underscores are text (both of a pair).
                    for _ in 0..open {
                        out.push('_');
                    }
                    i += open;
                }
            }
        } else if ch == '[' {
            let close = chars[i + 1..].iter().position(|c| *c == ']').map(|p| i + 1 + p);
            match close {
                Some(cl) if cl + 1 < chars.len() && chars[cl + 1] == '(' => {
                    let text: String = chars[i + 1..cl].iter().collect();
                    let url_end = chars[cl + 2..].iter().position(|c| *c == ')').map(|p| cl + 2 + p).unwrap_or(chars.len() - 1);
                    let (t, inner) = inline(&text);
                    let start = out.len();
                    out.push_str(&t);
                    hl.extend(inner.into_iter().map(|(r, s)| (r.start + start..r.end + start, s)));
                    hl.push((start..out.len(), HighlightStyle { color: Some(color(c::MATCH)), ..Default::default() }));
                    i = url_end + 1;
                }
                _ => {
                    out.push(ch);
                    i += 1;
                }
            }
        } else {
            out.push(ch);
            i += 1;
        }
    }
    (out, hl)
}

pub fn parse(md: &str) -> Vec<MdBlock> {
    let mut blocks = Vec::new();
    let mut para = String::new();
    let flush = |para: &mut String, blocks: &mut Vec<MdBlock>| {
        let t = para.trim();
        if !t.is_empty() {
            let (text, highlights) = inline(t);
            blocks.push(MdBlock::Para { text: text.into(), highlights, bullet: false });
        }
        para.clear();
    };
    let mut lines = md.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if let Some(tag) = trimmed.strip_prefix("```") {
            flush(&mut para, &mut blocks);
            // A fence may be longer than three backticks (the Kotlin server sends four); it
            // closes with at least as many.
            let fence = 3 + tag.chars().take_while(|c| *c == '`').count();
            let lang = fence_language(tag.trim_start_matches('`'));
            let mut code = String::new();
            for l in lines.by_ref() {
                let t = l.trim_start();
                if t.chars().take_while(|c| *c == '`').count() >= fence {
                    break;
                }
                code.push_str(l);
                code.push('\n');
            }
            let code = code.trim_end().to_string();
            if !code.is_empty() {
                let highlights = highlight_code(&code, lang);
                blocks.push(MdBlock::Code { text: code.into(), highlights });
            }
        } else if trimmed.is_empty() {
            flush(&mut para, &mut blocks);
        } else if trimmed.chars().all(|c| c == '-' || c == '_' || c == '*') && trimmed.len() >= 3 {
            flush(&mut para, &mut blocks);
            blocks.push(MdBlock::Rule);
        } else if trimmed.starts_with('#') {
            flush(&mut para, &mut blocks);
            blocks.push(MdBlock::Heading(inline(trimmed.trim_start_matches('#').trim()).0.into()));
        } else if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            flush(&mut para, &mut blocks);
            let (text, highlights) = inline(item);
            blocks.push(MdBlock::Para { text: text.into(), highlights, bullet: true });
        } else {
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(trimmed);
        }
    }
    flush(&mut para, &mut blocks);
    blocks
}

pub fn render(blocks: &[MdBlock]) -> Div {
    div().flex().flex_col().gap(px(6.)).children(blocks.iter().map(|b| match b {
        MdBlock::Code { text, highlights } => div()
            .px(px(8.))
            .py(px(6.))
            .rounded(px(6.))
            .bg(color(0x16181B))
            .font_family(theme::fonts().code)
            .text_size(px(12.5))
            .text_color(color(0xD6D6DD))
            .child(StyledText::new(text.clone()).with_highlights(highlights.clone()))
            .into_any_element(),
        MdBlock::Para { text, highlights, bullet } => div()
            .flex()
            .gap(px(6.))
            .text_size(px(12.5))
            .text_color(color(c::TEXT_MUTED))
            .line_height(px(18.))
            .when(*bullet, |d| d.child(div().text_color(color(c::TEXT_DIM)).child("•")))
            .child(div().flex_1().child(StyledText::new(text.clone()).with_highlights(highlights.clone())))
            .into_any_element(),
        MdBlock::Heading(t) => div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(color(c::TEXT)).child(t.clone()).into_any_element(),
        MdBlock::Rule => div().h(px(1.)).bg(color(c::BORDER)).into_any_element(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blocks_and_inline() {
        let md = "```rust\nfn f() {}\n```\n\n---\n\nReturns **the** `value` for [`Hash`](https://x).\n\n- item one";
        let blocks = parse(md);
        assert!(matches!(&blocks[0], MdBlock::Code { highlights, .. } if !highlights.is_empty()));
        assert!(matches!(blocks[1], MdBlock::Rule));
        match &blocks[2] {
            MdBlock::Para { text, highlights, .. } => {
                assert_eq!(text.as_ref(), "Returns the value for Hash.");
                assert!(highlights.len() >= 3);
            }
            _ => panic!(),
        }
        assert!(matches!(&blocks[3], MdBlock::Para { bullet: true, .. }));
    }

    #[test]
    fn long_fences() {
        let blocks = parse("````kotlin
fun greet(name: String): String
````
");
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], MdBlock::Code { text, highlights } if text.as_ref() == "fun greet(name: String): String" && !highlights.is_empty()));
    }

    #[test]
    fn underscore_emphasis() {
        // intelephense: `__greet__` (bold), `_@param_ string $name` (italic).
        assert_eq!(inline("__greet__").0, "greet");
        assert_eq!(inline("_@param_ string $name").0, "@param string $name");
        // Not emphasis: identifiers and lone underscores.
        assert_eq!(inline("snake_case_name and __init__ma").0, "snake_case_name and __init__ma");
        assert_eq!(inline("a _ b").0, "a _ b");
        assert_eq!(inline("__init__").0, "init");
    }
}
