//! Results to files: CSV (RFC 4180), JSON (an array of objects, exact numbers kept), and SQL
//! INSERT statements.

use crate::{DbKind, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
    SqlInserts,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 3] = [ExportFormat::Csv, ExportFormat::Json, ExportFormat::SqlInserts];

    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Csv => "CSV",
            ExportFormat::Json => "JSON",
            ExportFormat::SqlInserts => "SQL INSERT statements",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::Json => "json",
            ExportFormat::SqlInserts => "sql",
        }
    }
}

/// `rows` in `format`. `table` is the (already quoted) name INSERTs go into; `kind` decides
/// the quoting of names and literals.
pub fn export<'a>(format: ExportFormat, kind: &DbKind, table: &str, columns: &[String], rows: impl Iterator<Item = &'a Vec<Value>>) -> String {
    let mut out = String::new();
    match format {
        ExportFormat::Csv => {
            out.push_str(&columns.iter().map(|c| csv_field(c)).collect::<Vec<_>>().join(","));
            out.push_str("\r\n");
            for row in rows {
                let fields: Vec<String> = row.iter().map(|v| if v.is_null() { String::new() } else { csv_field(&v.export_text()) }).collect();
                out.push_str(&fields.join(","));
                out.push_str("\r\n");
            }
        }
        ExportFormat::Json => {
            let keys: Vec<String> = columns.iter().map(|c| json_string(c)).collect();
            out.push('[');
            for (i, row) in rows.enumerate() {
                out.push_str(if i == 0 { "\n  {" } else { ",\n  {" });
                for (j, (key, value)) in keys.iter().zip(row).enumerate() {
                    if j > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(key);
                    out.push_str(": ");
                    out.push_str(&json_value(value));
                }
                out.push('}');
            }
            out.push_str("\n]\n");
        }
        ExportFormat::SqlInserts => {
            let names: Vec<String> = columns.iter().map(|c| kind.quote_ident(c)).collect();
            let names = names.join(", ");
            for row in rows {
                let values: Vec<String> = row.iter().map(|v| kind.value_literal(v)).collect();
                out.push_str(&format!("INSERT INTO {table} ({names}) VALUES ({});\n", values.join(", ")));
            }
        }
    }
    out
}

/// Quoted when it holds a comma, a quote, a line break or edge spaces.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) || s.starts_with(' ') || s.ends_with(' ') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// A JSON number literal (exact decimals are written as they are, not through a float).
fn is_json_number(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() {
        return false;
    }
    let sign = usize::from(b[0] == b'-');
    let mut i = sign;
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > start
    };
    // No leading zeros ("007").
    if !digits(&mut i) || (b[sign] == b'0' && i > sign + 1) {
        return false;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        if !digits(&mut i) {
            return false;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !digits(&mut i) {
            return false;
        }
    }
    i == b.len()
}

fn json_value(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Int(i) => i.to_string(),
        Value::Real(r) if r.is_finite() => format!("{r:?}"),
        Value::Num(n) if is_json_number(n) => n.clone(),
        other => json_string(&other.export_text()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServerConfig;

    #[test]
    fn formats() {
        let columns = vec!["id".to_string(), "name".to_string(), "price".to_string(), "raw".to_string()];
        let rows = [vec![Value::Int(1), Value::Text("a, \"b\"".into()), Value::Num("12.50".into()), Value::blob(&[0xCA, 0xFE])], vec![Value::Int(2), Value::Null, Value::Real(0.25), Value::Null]];
        let sqlite = DbKind::Sqlite { path: "x".into() };
        assert_eq!(export(ExportFormat::Csv, &sqlite, "t", &columns, rows.iter()), "id,name,price,raw\r\n1,\"a, \"\"b\"\"\",12.50,0xCAFE\r\n2,,0.25,\r\n");
        assert_eq!(
            export(ExportFormat::Json, &sqlite, "t", &columns, rows.iter()),
            "[\n  {\"id\": 1, \"name\": \"a, \\\"b\\\"\", \"price\": 12.50, \"raw\": \"0xCAFE\"},\n  {\"id\": 2, \"name\": null, \"price\": 0.25, \"raw\": null}\n]\n"
        );
        let mysql = DbKind::Mysql(ServerConfig::default());
        assert_eq!(
            export(ExportFormat::SqlInserts, &mysql, "`t`", &columns, rows.iter().take(1)),
            "INSERT INTO `t` (`id`, `name`, `price`, `raw`) VALUES (1, 'a, \"b\"', 12.50, X'CAFE');\n"
        );
        assert_eq!(export(ExportFormat::Json, &sqlite, "t", &columns, [].iter()), "[\n]\n");
        for (s, ok) in [("12.50", true), ("-0.5", true), ("1e10", true), ("007", false), ("NaN", false), ("1.", false), ("", false)] {
            assert_eq!(is_json_number(s), ok, "{s}");
        }
    }
}
