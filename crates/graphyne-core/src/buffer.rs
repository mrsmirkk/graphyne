//! Rope text buffer.
//!
//! All positions exposed by [`Buffer`] are *char indices* into the rope (ropey's native unit),
//! except [`EditDelta`] which carries byte offsets / byte-column points for tree-sitter.
//! Line endings are normalised to `\n` in memory and restored on save.

use ropey::{Rope, RopeSlice};
use std::ops::Range;
use std::time::{Duration, Instant};

/// Row / byte-column point (tree-sitter compatible).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextPoint {
    pub row: usize,
    pub column: usize,
}

/// Description of a single edit, in bytes, ready to be fed into `Tree::edit`.
#[derive(Clone, Copy, Debug)]
pub struct EditDelta {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_point: TextPoint,
    pub old_end_point: TextPoint,
    pub new_end_point: TextPoint,
}

/// A selection in char indices. `head` is where the caret is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn caret(pos: usize) -> Self {
        Self { anchor: pos, head: pos }
    }
    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    pub fn label(&self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
        }
    }
}

/// One primitive replacement, recorded in application order.
#[derive(Clone, Debug)]
struct Op {
    start: usize,
    removed: String,
    inserted: String,
}

/// An undo step: one or more ops (e.g. a completion + its auto-import).
#[derive(Clone, Debug)]
struct UndoEntry {
    ops: Vec<Op>,
    sel_before: Selection,
    sel_after: Selection,
    at: Instant,
}

/// The editable document.
pub struct Buffer {
    rope: Rope,
    version: u64,
    saved_version: u64,
    line_ending: LineEnding,
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
}

const UNDO_GROUP_WINDOW: Duration = Duration::from_millis(900);
const UNDO_LIMIT: usize = 10_000;

impl Buffer {
    pub fn new() -> Self {
        Self::from_text("")
    }

    pub fn from_text(text: &str) -> Self {
        let line_ending = if text.contains("\r\n") { LineEnding::CrLf } else { LineEnding::Lf };
        let rope = if line_ending == LineEnding::CrLf {
            Rope::from_str(&text.replace("\r\n", "\n"))
        } else {
            Rope::from_str(text)
        };
        Self { rope, version: 0, saved_version: 0, line_ending, undo: Vec::new(), redo: Vec::new() }
    }

    /// Stream a file into a buffer: CRLF is normalised and invalid UTF-8 replaced chunk by
    /// chunk, so peak memory stays about one copy of the text (a 1 GB log needs ~1 GB, not 3).
    pub fn from_reader(mut reader: impl std::io::Read) -> std::io::Result<Self> {
        const CHUNK: usize = 1 << 20;
        let mut builder = ropey::RopeBuilder::new();
        let mut buf = vec![0u8; CHUNK];
        // Bytes carried into the next chunk: an incomplete UTF-8 sequence or a trailing '\r'.
        let mut carry: Vec<u8> = Vec::new();
        let mut crlf = false;
        let mut text = String::new();
        loop {
            let n = reader.read(&mut buf)?;
            let eof = n == 0;
            let mut bytes = std::mem::take(&mut carry);
            bytes.extend_from_slice(&buf[..n]);
            if !eof {
                // Keep an unfinished UTF-8 sequence (at most 3 bytes) for the next round.
                let valid = match std::str::from_utf8(&bytes) {
                    Ok(_) => bytes.len(),
                    Err(e) if e.error_len().is_none() => e.valid_up_to(),
                    Err(_) => bytes.len(),
                };
                let mut keep = valid;
                // A '\r' at the very end may be the first half of "\r\n".
                if keep > 0 && bytes[keep - 1] == b'\r' {
                    keep -= 1;
                }
                carry.extend_from_slice(&bytes[keep..]);
                bytes.truncate(keep);
            }
            text.clear();
            text.push_str(&String::from_utf8_lossy(&bytes));
            if text.contains("\r\n") {
                crlf = true;
                text = text.replace("\r\n", "\n");
            }
            builder.append(&text);
            if eof {
                break;
            }
        }
        let line_ending = if crlf { LineEnding::CrLf } else { LineEnding::Lf };
        Ok(Self { rope: builder.finish(), version: 0, saved_version: 0, line_ending, undo: Vec::new(), redo: Vec::new() })
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }
    pub fn version(&self) -> u64 {
        self.version
    }
    pub fn is_dirty(&self) -> bool {
        self.version != self.saved_version
    }
    pub fn mark_saved(&mut self, version: u64) {
        self.saved_version = version;
    }
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Text as it should be written on disk (original line endings restored).
    pub fn text_for_save(&self) -> String {
        Self::render_for_save(&self.rope, self.line_ending)
    }

    /// File bytes for `rope` with `line_ending` restored. Takes a rope snapshot so big
    /// buffers can be serialised on a background thread.
    pub fn render_for_save(rope: &Rope, line_ending: LineEnding) -> String {
        let text = rope.to_string();
        match line_ending {
            LineEnding::Lf => text,
            LineEnding::CrLf => text.replace('\n', "\r\n"),
        }
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }
    pub fn line(&self, row: usize) -> RopeSlice<'_> {
        self.rope.line(row)
    }

    /// Length of a line in chars, excluding the trailing newline.
    pub fn line_len(&self, row: usize) -> usize {
        if row >= self.rope.len_lines() {
            return 0;
        }
        let line = self.rope.line(row);
        let mut len = line.len_chars();
        if len > 0 && line.char(len - 1) == '\n' {
            len -= 1;
        }
        len
    }

    /// Line text without the trailing newline.
    pub fn line_text(&self, row: usize) -> String {
        if row >= self.rope.len_lines() {
            return String::new();
        }
        let line = self.rope.line(row);
        let len = self.line_len(row);
        line.slice(..len).to_string()
    }

    pub fn char_to_line(&self, char_idx: usize) -> usize {
        self.rope.char_to_line(char_idx.min(self.rope.len_chars()))
    }
    pub fn line_to_char(&self, row: usize) -> usize {
        self.rope.line_to_char(row.min(self.rope.len_lines()))
    }
    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        self.rope.char_to_byte(char_idx.min(self.rope.len_chars()))
    }
    pub fn byte_to_char(&self, byte: usize) -> usize {
        self.rope.byte_to_char(byte.min(self.rope.len_bytes()))
    }

    /// (row, column-in-chars) of a char index.
    pub fn row_col(&self, char_idx: usize) -> (usize, usize) {
        let c = char_idx.min(self.rope.len_chars());
        let row = self.rope.char_to_line(c);
        (row, c - self.rope.line_to_char(row))
    }

    /// Char index for (row, column-in-chars), clamped to the line.
    pub fn char_at(&self, row: usize, col: usize) -> usize {
        let row = row.min(self.len_lines().saturating_sub(1));
        self.rope.line_to_char(row) + col.min(self.line_len(row))
    }

    pub fn byte_point(&self, byte: usize) -> TextPoint {
        let byte = byte.min(self.rope.len_bytes());
        let row = self.rope.byte_to_line(byte);
        TextPoint { row, column: byte - self.rope.line_to_byte(row) }
    }

    pub fn slice_to_string(&self, range: Range<usize>) -> String {
        let len = self.rope.len_chars();
        self.rope.slice(range.start.min(len)..range.end.min(len)).to_string()
    }

    fn apply(&mut self, range: Range<usize>, text: &str) -> EditDelta {
        let len = self.rope.len_chars();
        let range = range.start.min(len)..range.end.min(len);
        let start_byte = self.rope.char_to_byte(range.start);
        let old_end_byte = self.rope.char_to_byte(range.end);
        let start_point = self.byte_point(start_byte);
        let old_end_point = self.byte_point(old_end_byte);
        if !range.is_empty() {
            self.rope.remove(range.clone());
        }
        if !text.is_empty() {
            self.rope.insert(range.start, text);
        }
        let new_end_byte = start_byte + text.len();
        let new_end_point = self.byte_point(new_end_byte);
        self.version += 1;
        EditDelta { start_byte, old_end_byte, new_end_byte, start_point, old_end_point, new_end_point }
    }

    /// Replace `range` (chars) with `text`, recording undo history.
    pub fn edit(
        &mut self,
        range: Range<usize>,
        text: &str,
        sel_before: Selection,
        sel_after: Selection,
    ) -> EditDelta {
        let text = if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text.to_string() };
        let removed = self.slice_to_string(range.clone());
        let now = Instant::now();
        let mut merged = false;
        if let Some(last) = self.undo.last_mut().filter(|l| l.ops.len() == 1) {
            let recent = now.duration_since(last.at) < UNDO_GROUP_WINDOW;
            let op = &mut last.ops[0];
            let typing = removed.is_empty()
                && op.removed.is_empty()
                && text.chars().count() == 1
                && !text.contains('\n')
                && range.start == op.start + op.inserted.chars().count();
            let deleting_back = text.is_empty()
                && op.inserted.is_empty()
                && removed.chars().count() == 1
                && range.end == op.start;
            if recent && typing {
                op.inserted.push_str(&text);
                last.sel_after = sel_after;
                last.at = now;
                merged = true;
            } else if recent && deleting_back {
                op.removed.insert_str(0, &removed);
                op.start = range.start;
                last.sel_after = sel_after;
                last.at = now;
                merged = true;
            }
        }
        if !merged {
            self.push_undo(UndoEntry {
                ops: vec![Op { start: range.start, removed, inserted: text.clone() }],
                sel_before,
                sel_after,
                at: now,
            });
        }
        self.redo.clear();
        self.apply(range, &text)
    }

    fn push_undo(&mut self, entry: UndoEntry) {
        self.undo.push(entry);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
    }

    /// Apply several non-overlapping replacements (ranges in *current* char coordinates)
    /// as one undo step. Returns the deltas in application order (descending offsets).
    pub fn edit_many(
        &mut self,
        mut edits: Vec<(Range<usize>, String)>,
        sel_before: Selection,
        sel_after: Selection,
    ) -> Vec<EditDelta> {
        edits.sort_by(|a, b| b.0.start.cmp(&a.0.start).then(b.0.end.cmp(&a.0.end)));
        let mut ops = Vec::with_capacity(edits.len());
        let mut deltas = Vec::with_capacity(edits.len());
        for (range, text) in edits {
            let text = if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text };
            let removed = self.slice_to_string(range.clone());
            if removed.is_empty() && text.is_empty() {
                continue;
            }
            deltas.push(self.apply(range.clone(), &text));
            ops.push(Op { start: range.start, removed, inserted: text });
        }
        if !ops.is_empty() {
            self.push_undo(UndoEntry { ops, sel_before, sel_after, at: Instant::now() - UNDO_GROUP_WINDOW });
            self.redo.clear();
        }
        deltas
    }

    /// Replace the whole content (e.g. external reload). Clears history.
    pub fn replace_all(&mut self, text: &str) -> EditDelta {
        let normalized = text.replace("\r\n", "\n");
        let delta = self.apply(0..self.rope.len_chars(), &normalized);
        self.undo.clear();
        self.redo.clear();
        delta
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Undo one step. Ops are reverted in reverse application order, which keeps every
    /// recorded offset valid.
    pub fn undo(&mut self) -> Option<(Vec<EditDelta>, Selection)> {
        let mut entry = self.undo.pop()?;
        let mut deltas = Vec::with_capacity(entry.ops.len());
        for op in entry.ops.iter().rev() {
            let end = op.start + op.inserted.chars().count();
            deltas.push(self.apply(op.start..end, &op.removed));
        }
        let sel = entry.sel_before;
        entry.at = Instant::now() - UNDO_GROUP_WINDOW;
        self.redo.push(entry);
        Some((deltas, sel))
    }

    pub fn redo(&mut self) -> Option<(Vec<EditDelta>, Selection)> {
        let mut entry = self.redo.pop()?;
        let mut deltas = Vec::with_capacity(entry.ops.len());
        for op in entry.ops.iter() {
            let end = op.start + op.removed.chars().count();
            deltas.push(self.apply(op.start..end, &op.inserted));
        }
        let sel = entry.sel_after;
        entry.at = Instant::now() - UNDO_GROUP_WINDOW;
        self.undo.push(entry);
        Some((deltas, sel))
    }

    /// Leading whitespace of a row.
    pub fn indentation(&self, row: usize) -> String {
        self.line_text(row).chars().take_while(|c| *c == ' ' || *c == '\t').collect()
    }

    /// Word boundary helpers (chars). A "word" is a run of alphanumerics/underscore.
    pub fn prev_word_start(&self, pos: usize) -> usize {
        let mut i = pos.min(self.len_chars());
        while i > 0 && self.rope.char(i - 1).is_whitespace() && self.rope.char(i - 1) != '\n' {
            i -= 1;
        }
        if i > 0 && is_word_char(self.rope.char(i - 1)) {
            while i > 0 && is_word_char(self.rope.char(i - 1)) {
                i -= 1;
            }
        } else if i > 0 {
            i -= 1;
        }
        i
    }

    pub fn next_word_end(&self, pos: usize) -> usize {
        let len = self.len_chars();
        let mut i = pos.min(len);
        while i < len && self.rope.char(i).is_whitespace() && self.rope.char(i) != '\n' {
            i += 1;
        }
        if i < len && is_word_char(self.rope.char(i)) {
            while i < len && is_word_char(self.rope.char(i)) {
                i += 1;
            }
        } else if i < len {
            i += 1;
        }
        i
    }

    /// Range of the word under `pos` (for double-click).
    pub fn word_range_at(&self, pos: usize) -> Range<usize> {
        let len = self.len_chars();
        let pos = pos.min(len);
        let mut start = pos;
        let mut end = pos;
        while start > 0 && is_word_char(self.rope.char(start - 1)) {
            start -= 1;
        }
        while end < len && is_word_char(self.rope.char(end)) {
            end += 1;
        }
        start..end
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reader that hands out a few bytes at a time, to hit every chunk boundary.
    struct Trickle<'a>(&'a [u8], usize);
    impl std::io::Read for Trickle<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = self.1.min(self.0.len()).min(out.len());
            out[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn streaming_load_matches_from_text() {
        let text = "héllo\r\nwörld 😀\r\nlast";
        for step in 1..=7 {
            let b = Buffer::from_reader(Trickle(text.as_bytes(), step)).unwrap();
            assert_eq!(b.rope().to_string(), "héllo\nwörld 😀\nlast", "step {step}");
            assert_eq!(b.line_ending(), LineEnding::CrLf);
            assert_eq!(b.text_for_save(), text);
        }
        let lf = Buffer::from_reader("a\nb".as_bytes()).unwrap();
        assert_eq!(lf.line_ending(), LineEnding::Lf);
        let bad = Buffer::from_reader(&b"ok \xff end"[..]).unwrap();
        assert_eq!(bad.rope().to_string(), "ok \u{fffd} end");
    }

    #[test]
    fn crlf_roundtrip() {
        let b = Buffer::from_text("a\r\nb\r\n");
        assert_eq!(b.line_ending(), LineEnding::CrLf);
        assert_eq!(b.rope().to_string(), "a\nb\n");
        assert_eq!(b.text_for_save(), "a\r\nb\r\n");
    }

    #[test]
    fn undo_groups_typing() {
        let mut b = Buffer::new();
        for (i, ch) in "hello".chars().enumerate() {
            b.edit(i..i, &ch.to_string(), Selection::caret(i), Selection::caret(i + 1));
        }
        assert_eq!(b.rope().to_string(), "hello");
        b.undo();
        assert_eq!(b.rope().to_string(), "");
        b.redo();
        assert_eq!(b.rope().to_string(), "hello");
    }

    #[test]
    fn edit_many_is_one_undo_step() {
        let mut b = Buffer::from_text("fn main() {\n    Has\n}\n");
        // Completion replaces `Has` and adds an import at the top in one transaction.
        b.edit_many(
            vec![(16..19, "HashMap".into()), (0..0, "use std::collections::HashMap;\n".into())],
            Selection::caret(19),
            Selection::caret(54),
        );
        assert_eq!(b.rope().to_string(), "use std::collections::HashMap;\nfn main() {\n    HashMap\n}\n");
        let (deltas, sel) = b.undo().unwrap();
        assert_eq!(deltas.len(), 2);
        assert_eq!(sel, Selection::caret(19));
        assert_eq!(b.rope().to_string(), "fn main() {\n    Has\n}\n");
        b.redo();
        assert!(b.rope().to_string().starts_with("use std"));
    }

    #[test]
    fn edit_delta_bytes() {
        let mut b = Buffer::from_text("héllo\nworld");
        let d = b.edit(6..6, "X", Selection::caret(6), Selection::caret(7));
        assert_eq!(d.start_byte, 7);
        assert_eq!(d.new_end_point, TextPoint { row: 1, column: 1 });
    }
}
