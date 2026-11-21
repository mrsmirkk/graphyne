//! A cell of a result: what the database returned, ready to show, copy and sort.

use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Real(f64),
    /// An exact number kept as the server wrote it (PostgreSQL `numeric`, MySQL `DECIMAL`,
    /// unsigned 64-bit): `12.50` must not become `12.5`.
    Num(String),
    Text(String),
    /// Binary data: its size and the first bytes (for display).
    Blob { len: usize, head: Vec<u8> },
}

/// Bytes of a BLOB kept for display.
const BLOB_HEAD: usize = 32;

impl Value {
    pub fn blob(bytes: &[u8]) -> Self {
        Value::Blob { len: bytes.len(), head: bytes[..bytes.len().min(BLOB_HEAD)].to_vec() }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn is_number(&self) -> bool {
        matches!(self, Value::Int(_) | Value::Real(_) | Value::Num(_))
    }

    /// The whole value as text — what Copy puts on the clipboard.
    pub fn to_text(&self) -> String {
        match self {
            Value::Null => "NULL".into(),
            Value::Int(i) => i.to_string(),
            Value::Real(r) => format_real(*r),
            Value::Num(s) | Value::Text(s) => s.clone(),
            Value::Blob { len, head } => {
                let hex: String = head.iter().map(|b| format!("{b:02X}")).collect();
                let more = if *len > head.len() { "…" } else { "" };
                format!("0x{hex}{more} ({len} bytes)")
            }
        }
    }

    /// For exports: binary data as `0xCAFE` when it was read whole, otherwise as shown.
    pub fn export_text(&self) -> String {
        match self {
            Value::Blob { len, head } if *len == head.len() => format!("0x{}", head.iter().map(|b| format!("{b:02X}")).collect::<String>()),
            other => other.to_text(),
        }
    }

    /// One line for a grid cell: line breaks shown as `⏎`, cut to `max_chars` with `…`.
    pub fn display(&self, max_chars: usize) -> String {
        let text = match self {
            // Cut first: a cell never needs more than a screenful of a huge text.
            Value::Text(s) => s.chars().take(max_chars + 1).collect::<String>().replace("\r\n", "⏎").replace(['\n', '\r'], "⏎").replace('\t', " "),
            other => other.to_text(),
        };
        if text.chars().count() > max_chars {
            let mut cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
            cut.push('…');
            cut
        } else {
            text
        }
    }

    /// Order for sorting a column: NULLs first, then numbers, text, binary data.
    pub fn sort_cmp(&self, other: &Value) -> Ordering {
        fn rank(v: &Value) -> u8 {
            match v {
                Value::Null => 0,
                Value::Int(_) | Value::Real(_) | Value::Num(_) => 1,
                Value::Text(_) => 2,
                Value::Blob { .. } => 3,
            }
        }
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Int(_) | Value::Real(_) | Value::Num(_), Value::Int(_) | Value::Real(_) | Value::Num(_)) => self.as_f64().total_cmp(&other.as_f64()),
            (Value::Text(a), Value::Text(b)) => a.cmp(b),
            (Value::Blob { len: a, .. }, Value::Blob { len: b, .. }) => a.cmp(b),
            _ => rank(self).cmp(&rank(other)),
        }
    }

    fn as_f64(&self) -> f64 {
        match self {
            Value::Int(i) => *i as f64,
            Value::Real(r) => *r,
            Value::Num(s) => s.parse().unwrap_or(0.),
            _ => 0.,
        }
    }
}

/// `2.0` rather than `2` for a REAL, so the type stays visible.
fn format_real(r: f64) -> String {
    if r.fract() == 0. && r.abs() < 1e15 { format!("{r:.1}") } else { r.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_display_copy_and_sort() {
        assert_eq!(Value::Real(2.0).to_text(), "2.0");
        assert_eq!(Value::Real(0.25).to_text(), "0.25");
        assert_eq!(Value::Null.to_text(), "NULL");
        assert_eq!(Value::Text("a\nb\r\nc".into()).display(20), "a⏎b⏎c");
        assert_eq!(Value::Text("abcdefghij".into()).display(5), "abcd…");
        assert_eq!(Value::blob(&[0xDE, 0xAD]).to_text(), "0xDEAD (2 bytes)");
        assert!(Value::blob(&[0u8; 100]).to_text().ends_with("00… (100 bytes)"));
        let mut column = vec![Value::Text("b".into()), Value::Int(10), Value::Null, Value::Num("3.50".into()), Value::Real(2.5), Value::Text("a".into())];
        column.sort_by(Value::sort_cmp);
        assert_eq!(column, vec![Value::Null, Value::Real(2.5), Value::Num("3.50".into()), Value::Int(10), Value::Text("a".into()), Value::Text("b".into())]);
        assert_eq!(Value::Num("12.50".into()).to_text(), "12.50");
    }
}
