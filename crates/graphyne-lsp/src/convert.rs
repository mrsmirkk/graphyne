//! URI and position conversions.

use lsp_types::Position;
use ropey::Rope;
use std::path::{Path, PathBuf};

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/')
}

/// `D:\My Projects\a.rs` → `file:///D:/My%20Projects/a.rs`.
pub fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for (i, b) in s.bytes().enumerate() {
        // Keep the drive colon (`/D:/`) readable; encode everything else that is reserved.
        let drive_colon = b == b':' && i == 1;
        if is_unreserved(b) || drive_colon {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `file:///d%3A/x/a.rs` → `D:\x\a.rs` (drive letter upper-cased for stable comparisons).
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest);
    if cfg!(windows) {
        let trimmed = decoded.strip_prefix('/').unwrap_or(&decoded);
        let mut chars: Vec<char> = trimmed.chars().collect();
        if chars.len() >= 2 && chars[1] == ':' {
            chars[0] = chars[0].to_ascii_uppercase();
        }
        let s: String = chars.into_iter().collect();
        Some(PathBuf::from(s.replace('/', "\\")))
    } else {
        Some(PathBuf::from(decoded))
    }
}

/// Case/separator-insensitive key for matching paths coming back from servers.
pub fn path_key(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// Rope char index → LSP position (UTF-16 code units).
pub fn char_to_position(rope: &Rope, char_idx: usize) -> Position {
    let c = char_idx.min(rope.len_chars());
    let line = rope.char_to_line(c);
    let line_start = rope.line_to_char(line);
    let col = rope.char_to_utf16_cu(c) - rope.char_to_utf16_cu(line_start);
    Position { line: line as u32, character: col as u32 }
}

/// LSP position (UTF-16) → rope char index, clamped to the line.
pub fn position_to_char(rope: &Rope, pos: Position) -> usize {
    let lines = rope.len_lines();
    let line = (pos.line as usize).min(lines.saturating_sub(1));
    let line_start = rope.line_to_char(line);
    let next = if line + 1 < lines { rope.line_to_char(line + 1) } else { rope.len_chars() };
    // Exclude the trailing newline from the addressable range.
    let mut line_end = next;
    if line_end > line_start && rope.char(line_end - 1) == '\n' {
        line_end -= 1;
    }
    let start_u16 = rope.char_to_utf16_cu(line_start);
    let target = start_u16 + pos.character as usize;
    let end_u16 = rope.char_to_utf16_cu(line_end);
    if target >= end_u16 {
        return line_end;
    }
    rope.utf16_cu_to_char(target)
}

/// Incremental-sync `TextDocumentContentChangeEvent` for replacing the char `range` of
/// `rope` with `text`. `rope` must be the document *before* this edit (earlier changes of
/// the same notification already applied), as the server applies changes in order.
pub fn content_change(rope: &Rope, range: std::ops::Range<usize>, text: &str) -> serde_json::Value {
    serde_json::json!({
        "range": { "start": char_to_position(rope, range.start), "end": char_to_position(rope, range.end) },
        "text": text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference server-side application of a content change (UTF-16 positions).
    fn apply_change(doc: &mut String, change: &serde_json::Value) {
        let offset = |p: &serde_json::Value| -> usize {
            let (line, col) = (p["line"].as_u64().unwrap() as usize, p["character"].as_u64().unwrap() as usize);
            let line_start: usize = doc.split_inclusive('\n').take(line).map(str::len).sum();
            let mut units = 0;
            let mut byte = line_start;
            for ch in doc[line_start..].chars() {
                if units >= col || ch == '\n' {
                    break;
                }
                units += ch.len_utf16();
                byte += ch.len_utf8();
            }
            byte
        };
        let (s, e) = (offset(&change["range"]["start"]), offset(&change["range"]["end"]));
        doc.replace_range(s..e, change["text"].as_str().unwrap());
    }

    #[test]
    fn incremental_changes_replay_on_server() {
        let original = "héllo 😀 wörld\nsecond line\n𝄞 third\n";
        let mut rope = Rope::from_str(original);
        let mut server = original.to_string();
        // (char range, text) edits applied one after another, including multi-line ones
        // and a descending batch like `Buffer::edit_many`.
        let edits: &[(std::ops::Range<usize>, &str)] =
            &[(6..7, "🎉🎉"), (0..1, "H"), (14..20, "line A\nline B"), (30..30, "X"), (rope.len_chars() - 1..rope.len_chars(), "\n// end\n")];
        let mut changes = Vec::new();
        for (r, t) in edits {
            let r = r.start.min(rope.len_chars())..r.end.min(rope.len_chars());
            changes.push(content_change(&rope, r.clone(), t));
            rope.remove(r.clone());
            rope.insert(r.start, t);
        }
        for c in &changes {
            apply_change(&mut server, c);
        }
        assert_eq!(server, rope.to_string());
    }

    #[test]
    fn uri_roundtrip() {
        if cfg!(windows) {
            let p = PathBuf::from(r"D:\My Projects\a b\lib.rs");
            let u = path_to_uri(&p);
            assert_eq!(u, "file:///D:/My%20Projects/a%20b/lib.rs");
            assert_eq!(uri_to_path(&u).unwrap(), p);
            assert_eq!(uri_to_path("file:///d%3A/My%20Projects/x.rs").unwrap(), PathBuf::from(r"D:\My Projects\x.rs"));
        }
    }

    #[test]
    fn positions_utf16() {
        let rope = Rope::from_str("a😀b\nxyz\n");
        // '😀' is 2 UTF-16 code units.
        assert_eq!(char_to_position(&rope, 2), Position { line: 0, character: 3 });
        assert_eq!(position_to_char(&rope, Position { line: 0, character: 3 }), 2);
        assert_eq!(position_to_char(&rope, Position { line: 1, character: 99 }), 7);
        assert_eq!(position_to_char(&rope, Position { line: 9, character: 0 }), rope.len_chars());
    }
}
