//! Importing CSV files: parsing (commas, semicolons or tabs, quoted fields with line breaks),
//! guessing column types for a new table, and the statements that load the rows.

use crate::{DbKind, EditStatement};

/// Rows per INSERT statement.
const BATCH: usize = 200;

/// A parsed CSV file: the header row and the data rows (as long as the header).
#[derive(Debug, PartialEq)]
pub struct Csv {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColumnType {
    Integer,
    Real,
    Text,
}

/// Whether non-UTF-8 text looks like Windows-1251: Cyrillic words are made of bytes ≥ 0x80
/// only, while accented Latin letters (Windows-1252) come one or two at a time among ASCII ones.
fn looks_cyrillic(bytes: &[u8]) -> bool {
    let (mut high, mut in_runs, mut run) = (0usize, 0usize, 0usize);
    for &b in bytes.iter().chain(std::iter::once(&b' ')) {
        if b >= 0x80 {
            high += 1;
            run += 1;
        } else {
            if run >= 3 {
                in_runs += run;
            }
            run = 0;
        }
    }
    high > 0 && in_runs * 2 > high
}

/// The text of a CSV file: UTF-8 (with or without a BOM), UTF-16 with a BOM (Excel's "Unicode
/// Text"), else Windows-1251 when it looks Cyrillic (a Russian Excel's "CSV"), else the
/// system's ANSI code page.
pub fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => code_page(bytes, if looks_cyrillic(bytes) { 1251 } else { 0 }),
    }
}

/// Decode with a Windows code page (`0`: the system's ANSI one).
#[cfg(windows)]
fn code_page(bytes: &[u8], page: u32) -> String {
    use windows_sys::Win32::Globalization::MultiByteToWideChar;
    let Ok(len) = i32::try_from(bytes.len()) else { return String::from_utf8_lossy(bytes).into_owned() };
    // SAFETY: the buffers and lengths passed match.
    unsafe {
        let n = MultiByteToWideChar(page, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0);
        if n <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut wide = vec![0u16; n as usize];
        let n = MultiByteToWideChar(page, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), n);
        wide.truncate(n.max(0) as usize);
        String::from_utf16_lossy(&wide)
    }
}

#[cfg(not(windows))]
fn code_page(bytes: &[u8], _page: u32) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The separator used by the first line: the most frequent of `,` `;` and tab outside quotes.
fn guess_delimiter(text: &str) -> char {
    let (mut quoted, mut counts) = (false, [0usize; 3]);
    for ch in text.chars() {
        match ch {
            '"' => quoted = !quoted,
            '\n' if !quoted => break,
            ',' if !quoted => counts[0] += 1,
            ';' if !quoted => counts[1] += 1,
            '\t' if !quoted => counts[2] += 1,
            _ => {}
        }
    }
    let best = (0..3).max_by_key(|&i| (counts[i], std::cmp::Reverse(i))).unwrap_or(0);
    [',', ';', '\t'][best]
}

/// Parse CSV text. The first row is the header; blank lines are skipped; a row with fewer
/// fields than the header is padded, one with more is an error.
pub fn parse_csv(text: &str) -> Result<Csv, String> {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let delimiter = guess_delimiter(text);
    let mut records: Vec<(usize, Vec<String>)> = Vec::new();
    let (mut record, mut field) = (Vec::new(), String::new());
    let (mut quoted, mut was_quoted, mut line, mut record_line) = (false, false, 1, 1);
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            match ch {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                '\r' if chars.peek() == Some(&'\n') => {}
                '\n' => {
                    line += 1;
                    field.push('\n');
                }
                _ => field.push(ch),
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() && !was_quoted => {
                quoted = true;
                was_quoted = true;
            }
            c if c == delimiter => {
                record.push(std::mem::take(&mut field));
                was_quoted = false;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                record.push(std::mem::take(&mut field));
                let blank = record.len() == 1 && record[0].is_empty() && !was_quoted;
                if !blank {
                    records.push((record_line, std::mem::take(&mut record)));
                }
                record.clear();
                was_quoted = false;
                line += 1;
                record_line = line;
            }
            _ => field.push(ch),
        }
    }
    if quoted {
        return Err(format!("line {record_line}: a quoted field is not closed"));
    }
    if !field.is_empty() || !record.is_empty() || was_quoted {
        record.push(field);
        records.push((record_line, record));
    }
    let mut records = records.into_iter();
    let Some((_, header)) = records.next() else { return Err("the file is empty".into()) };
    let mut names: Vec<String> = Vec::with_capacity(header.len());
    for (i, name) in header.iter().enumerate() {
        let base = match name.trim() {
            "" => format!("column{}", i + 1),
            n => n.to_string(),
        };
        let mut unique = base.clone();
        let mut n = 2;
        while names.iter().any(|x| x.eq_ignore_ascii_case(&unique)) {
            unique = format!("{base}_{n}");
            n += 1;
        }
        names.push(unique);
    }
    let mut rows = Vec::new();
    for (at, mut row) in records {
        if row.len() > names.len() {
            return Err(format!("line {at} has {} fields, the header has {}", row.len(), names.len()));
        }
        row.resize(names.len(), String::new());
        rows.push(row);
    }
    Ok(Csv { header: names, rows })
}

/// `007` is not a number: codes and phone numbers keep their zeros as text.
fn leading_zero(s: &str) -> bool {
    let b = s.trim_start_matches(['-', '+']).as_bytes();
    b.len() > 1 && b[0] == b'0' && b[1].is_ascii_digit()
}

fn is_integer(s: &str) -> bool {
    let digits = s.strip_prefix('-').unwrap_or(s);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) && !leading_zero(s) && s.parse::<i64>().is_ok()
}

fn is_real(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E')) && !leading_zero(s) && s.parse::<f64>().is_ok_and(f64::is_finite)
}

impl Csv {
    /// The narrowest type that holds every non-empty value of each column.
    pub fn column_types(&self) -> Vec<ColumnType> {
        (0..self.header.len())
            .map(|col| {
                let mut values = self.rows.iter().map(|r| r[col].as_str()).filter(|v| !v.is_empty()).peekable();
                if values.peek().is_none() {
                    return ColumnType::Text;
                }
                let mut ty = ColumnType::Integer;
                for v in values {
                    if ty == ColumnType::Integer && !is_integer(v) {
                        ty = ColumnType::Real;
                    }
                    if ty == ColumnType::Real && !is_real(v) {
                        return ColumnType::Text;
                    }
                }
                ty
            })
            .collect()
    }
}

impl DbKind {
    fn type_name(&self, ty: ColumnType) -> &'static str {
        match (self, ty) {
            (DbKind::Sqlite { .. }, ColumnType::Integer) => "INTEGER",
            (DbKind::Sqlite { .. }, ColumnType::Real) => "REAL",
            (DbKind::Postgres(_), ColumnType::Integer) => "bigint",
            (DbKind::Postgres(_), ColumnType::Real) => "double precision",
            (DbKind::Postgres(_), ColumnType::Text) => "text",
            (DbKind::Mysql(_), ColumnType::Integer) => "BIGINT",
            (DbKind::Mysql(_), ColumnType::Real) => "DOUBLE",
            (_, ColumnType::Text) => "TEXT",
        }
    }

    /// `CREATE TABLE` for the CSV's columns with their guessed types. A first column `id`
    /// holding distinct whole numbers becomes the primary key, so the grid can edit the table.
    pub fn create_table_sql(&self, table: &str, csv: &Csv) -> EditStatement {
        let types = csv.column_types();
        let id_key = csv.header.first().is_some_and(|h| h.eq_ignore_ascii_case("id")) && types.first() == Some(&ColumnType::Integer) && {
            let mut seen = std::collections::HashSet::new();
            csv.rows.iter().all(|r| !r[0].is_empty() && seen.insert(r[0].as_str()))
        };
        let columns: Vec<String> = csv
            .header
            .iter()
            .zip(types)
            .enumerate()
            .map(|(i, (name, ty))| format!("{} {}{}", self.quote_ident(name), self.type_name(ty), if i == 0 && id_key { " PRIMARY KEY" } else { "" }))
            .collect();
        EditStatement { sql: format!("CREATE TABLE {table} ({})", columns.join(", ")), expect_one_row: false }
    }

    /// INSERTs of all rows into `table` (already quoted), in batches. `columns` are the
    /// target column names in CSV order; numbers go in as numbers where `types` says so, the
    /// rest as text, which every engine converts to the column's type. Empty fields are NULL.
    pub fn import_statements(&self, table: &str, columns: &[String], types: Option<&[ColumnType]>, rows: &[Vec<String>]) -> Vec<EditStatement> {
        let names: Vec<String> = columns.iter().map(|c| self.quote_ident(c)).collect();
        rows.chunks(BATCH)
            .map(|chunk| {
                let values: Vec<String> = chunk
                    .iter()
                    .map(|row| {
                        let literals: Vec<String> = row
                            .iter()
                            .enumerate()
                            .map(|(i, v)| match types.and_then(|t| t.get(i)) {
                                _ if v.is_empty() => "NULL".to_string(),
                                Some(ColumnType::Integer | ColumnType::Real) => v.clone(),
                                _ => self.text_literal(v),
                            })
                            .collect();
                        format!("({})", literals.join(", "))
                    })
                    .collect();
                EditStatement { sql: format!("INSERT INTO {table} ({}) VALUES {}", names.join(", "), values.join(", ")), expect_one_row: false }
            })
            .collect()
    }
}

/// Map the CSV header to the columns of an existing table (case-insensitively): the
/// table's column names in CSV order, or the headers that match none of them.
pub fn match_columns(header: &[String], table_columns: &[String]) -> Result<Vec<String>, String> {
    let mut found = Vec::with_capacity(header.len());
    let mut missing = Vec::new();
    for h in header {
        match table_columns.iter().find(|c| c.eq_ignore_ascii_case(h.trim())) {
            Some(c) => found.push(c.clone()),
            None => missing.push(h.as_str()),
        }
    }
    if missing.is_empty() { Ok(found) } else { Err(format!("the table has no column {}", missing.iter().map(|m| format!("\u{201C}{m}\u{201D}")).collect::<Vec<_>>().join(", "))) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServerConfig;
    use std::path::PathBuf;

    #[test]
    fn parses_quotes_delimiters_and_line_breaks() {
        let csv = parse_csv("\u{FEFF}id,name,note\r\n1,\"Smith, J\",\"said \"\"hi\"\"\nand left\"\r\n\r\n2,Lee\n").unwrap();
        assert_eq!(csv.header, ["id", "name", "note"]);
        assert_eq!(csv.rows, vec![vec!["1".to_string(), "Smith, J".into(), "said \"hi\"\nand left".into()], vec!["2".into(), "Lee".into(), "".into()]]);
        let semi = parse_csv("a;b\n1,5;x").unwrap();
        assert_eq!(semi.rows, vec![vec!["1,5".to_string(), "x".into()]]);
        let tabs = parse_csv("a\tb\n1\t2").unwrap();
        assert_eq!(tabs.rows, vec![vec!["1".to_string(), "2".into()]]);
        assert_eq!(parse_csv("a,a,\n").unwrap().header, ["a", "a_2", "column3"]);
        assert!(parse_csv("a,b\n1,2,3").unwrap_err().contains("line 2"));
        assert!(parse_csv("a\n\"open").is_err());
        assert!(parse_csv("").is_err());
    }

    #[test]
    fn decodes_utf8_utf16_and_the_ansi_code_page() {
        assert_eq!(decode_text("имя,город\n".as_bytes()), "имя,город\n");
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("имя\tгород".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(decode_text(&utf16), "имя\tгород");
        // A Russian Excel's CSV (Windows-1251): "имя;город\nАня;Казань".
        let cp1251 = [0xE8, 0xEC, 0xFF, b';', 0xE3, 0xEE, 0xF0, 0xEE, 0xE4, b'\n', 0xC0, 0xED, 0xFF, b';', 0xCA, 0xE0, 0xE7, 0xE0, 0xED, 0xFC];
        assert_eq!(decode_text(&cp1251), "имя;город\nАня;Казань");
        // Accented Latin letters are not taken for Cyrillic.
        assert!(!looks_cyrillic(&[b'c', b'a', b'f', 0xE9, b',', b'c', b'r', 0xE8, b'm', b'e']));
        let latin = decode_text(&[b'c', b'a', b'f', 0xE9]);
        assert!(latin.starts_with("caf") && latin.chars().count() == 4 && !latin.contains('\u{FFFD}'), "{latin:?}");
    }

    #[test]
    fn guesses_types_and_builds_statements() {
        let csv = parse_csv("n,x,code,t,e\n1,1.5,007,a,\n-2,3,12,b,\n").unwrap();
        assert_eq!(csv.column_types(), [ColumnType::Integer, ColumnType::Real, ColumnType::Text, ColumnType::Text, ColumnType::Text]);
        let lite = DbKind::Sqlite { path: PathBuf::from("x.db") };
        assert_eq!(lite.create_table_sql("\"t\"", &csv).sql, "CREATE TABLE \"t\" (\"n\" INTEGER, \"x\" REAL, \"code\" TEXT, \"t\" TEXT, \"e\" TEXT)");
        let pg = DbKind::Postgres(ServerConfig::default());
        assert!(pg.create_table_sql("t", &csv).sql.contains("\"x\" double precision"));
        let keyed = parse_csv("ID,v\n1,a\n2,b").unwrap();
        assert_eq!(lite.create_table_sql("t", &keyed).sql, "CREATE TABLE t (\"ID\" INTEGER PRIMARY KEY, \"v\" TEXT)");
        let repeated = parse_csv("id,v\n1,a\n1,b").unwrap();
        assert_eq!(lite.create_table_sql("t", &repeated).sql, "CREATE TABLE t (\"id\" INTEGER, \"v\" TEXT)");
        let types = csv.column_types();
        let stmts = lite.import_statements("\"t\"", &csv.header, Some(&types), &csv.rows);
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0].sql, "INSERT INTO \"t\" (\"n\", \"x\", \"code\", \"t\", \"e\") VALUES (1, 1.5, '007', 'a', NULL), (-2, 3, '12', 'b', NULL)");
        let many: Vec<Vec<String>> = (0..450).map(|i| vec![i.to_string()]).collect();
        assert_eq!(lite.import_statements("t", &["n".into()], None, &many).len(), 3);
        assert_eq!(match_columns(&["ID".into(), "Body".into()], &["id".into(), "body".into()]).unwrap(), ["id", "body"]);
        assert!(match_columns(&["id".into(), "nope".into()], &["id".into()]).unwrap_err().contains("nope"));
    }
}
