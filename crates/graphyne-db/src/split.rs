//! Splitting a SQL script into statements and finding the one under the cursor, the way
//! "Run" in a console needs it. `;` inside strings, quoted names, comments and trigger bodies
//! (`CREATE TRIGGER … BEGIN …; END`) does not end a statement.

use std::ops::Range;

/// Byte ranges of the statements in `sql`: without surrounding whitespace and without the
/// closing `;`. Pieces holding only comments are left out.
pub fn statements(sql: &str) -> Vec<Range<usize>> {
    let b = sql.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    // Words seen in the current statement (to recognise CREATE [TEMP] TRIGGER).
    let mut words = 0usize;
    let mut trigger = false;
    // BEGIN / CASE … END nesting inside a trigger body.
    let mut depth = 0usize;
    while i < n {
        match b[i] {
            q @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < n {
                    if b[i] == q {
                        if i + 1 < n && b[i + 1] == q {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            // PostgreSQL dollar quoting: `$$ … $$`, `$body$ … $body$` (not `$1` parameters).
            b'$' => {
                let mut j = i + 1;
                while j < n && (b[j].is_ascii_alphabetic() || b[j] == b'_' || (j > i + 1 && b[j].is_ascii_digit())) {
                    j += 1;
                }
                if j < n && b[j] == b'$' {
                    let tag = &sql[i..=j];
                    i = match sql[j + 1..].find(tag) {
                        Some(p) => j + 1 + p + tag.len(),
                        None => n,
                    };
                } else {
                    i += 1;
                }
            }
            b'[' => {
                while i < n && b[i] != b']' {
                    i += 1;
                }
                i += 1;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < n && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            b';' => {
                if !(trigger && depth > 0) {
                    push(sql, start..i, &mut out);
                    start = i + 1;
                    words = 0;
                    trigger = false;
                    depth = 0;
                }
                i += 1;
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let w = i;
                while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$') {
                    i += 1;
                }
                let word = &sql[w..i];
                words += 1;
                if words <= 4 && word.eq_ignore_ascii_case("trigger") && first_word_is_create(&sql[start..w]) {
                    trigger = true;
                } else if trigger && (word.eq_ignore_ascii_case("begin") || (depth > 0 && word.eq_ignore_ascii_case("case"))) {
                    depth += 1;
                } else if trigger && depth > 0 && word.eq_ignore_ascii_case("end") {
                    depth -= 1;
                }
            }
            _ => i += 1,
        }
    }
    push(sql, start..n, &mut out);
    out
}

/// The statement to run for a cursor at byte `offset`: the one it is in, else the closest
/// one before it (the cursor sits after a `;`), else the first.
pub fn statement_at(sql: &str, offset: usize) -> Option<Range<usize>> {
    let all = statements(sql);
    all.iter()
        .find(|r| r.start <= offset && offset <= r.end)
        .or_else(|| all.iter().rev().find(|r| r.end <= offset))
        .or_else(|| all.first())
        .cloned()
}

fn first_word_is_create(before: &str) -> bool {
    strip_comments(before).split_whitespace().next().is_some_and(|w| w.eq_ignore_ascii_case("create"))
}

/// Trim whitespace and leading comments; keep the piece if anything else is left.
fn push(sql: &str, range: Range<usize>, out: &mut Vec<Range<usize>>) {
    let text = &sql[range.clone()];
    if strip_comments(text).trim().is_empty() {
        return;
    }
    let start = range.start + (text.len() - text.trim_start().len());
    let end = range.end - (text.len() - text.trim_end().len());
    // Leading comments belong to the statement only if they touch it; skip whole comment lines.
    let mut s = start;
    loop {
        let rest = &sql[s..end];
        if let Some(after) = rest.strip_prefix("--") {
            let line = after.find('\n').map(|p| p + 3).unwrap_or(rest.len());
            s += line;
        } else if rest.starts_with("/*") {
            match rest.find("*/") {
                Some(p) => s += p + 2,
                None => break,
            }
        } else {
            break;
        }
        s += sql[s..end].len() - sql[s..end].trim_start().len();
    }
    out.push(s..end);
}

/// `sql` without its comments (string contents are left alone).
fn strip_comments(sql: &str) -> String {
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    let mut last = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                i += 1;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                out.push_str(&sql[last..i]);
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                last = i;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                out.push_str(&sql[last..i]);
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                last = i;
            }
            _ => i += 1,
        }
    }
    out.push_str(&sql[last.min(sql.len())..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(sql: &str) -> Vec<&str> {
        statements(sql).into_iter().map(|r| &sql[r]).collect()
    }

    #[test]
    fn splits_on_semicolons_outside_strings_and_comments() {
        let sql = "SELECT 'a;b' AS \"x;y\";\n-- note; not a statement\nINSERT INTO t VALUES (1); /* c; */ SELECT [a;b] FROM t\n";
        assert_eq!(texts(sql), vec!["SELECT 'a;b' AS \"x;y\"", "INSERT INTO t VALUES (1)", "SELECT [a;b] FROM t"]);
        assert_eq!(texts("  ;; -- only a comment\n ; "), Vec::<&str>::new());
        assert_eq!(texts("select 'it''s; fine'"), vec!["select 'it''s; fine'"]);
    }

    #[test]
    fn dollar_quoted_bodies_stay_whole() {
        let sql = "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql;
DO $body$ BEGIN PERFORM 1; END $body$;
SELECT $1, price$ FROM t";
        let parts = texts(sql);
        assert_eq!(parts.len(), 3, "{parts:?}");
        assert!(parts[0].ends_with("LANGUAGE plpgsql") && parts[1].ends_with("END $body$"));
        assert_eq!(parts[2], "SELECT $1, price$ FROM t");
    }

    #[test]
    fn trigger_bodies_stay_whole() {
        let sql = "CREATE TRIGGER t AFTER INSERT ON a BEGIN\n  UPDATE b SET n = CASE WHEN 1 THEN 2 ELSE 3 END;\n  DELETE FROM c;\nEND;\nBEGIN TRANSACTION; COMMIT;";
        let parts = texts(sql);
        assert_eq!(parts.len(), 3, "{parts:?}");
        assert!(parts[0].starts_with("CREATE TRIGGER") && parts[0].ends_with("END"));
        assert_eq!(&parts[1..], ["BEGIN TRANSACTION", "COMMIT"]);
    }

    #[test]
    fn statement_under_the_cursor() {
        let sql = "SELECT 1;\n\nSELECT 2\n  FROM t;\n";
        let at = |offset: usize| statement_at(sql, offset).map(|r| &sql[r]);
        assert_eq!(at(0), Some("SELECT 1"));
        assert_eq!(at(9), Some("SELECT 1")); // right after the `;`
        assert_eq!(at(10), Some("SELECT 1")); // blank line below it
        assert_eq!(at(sql.find("FROM").unwrap()), Some("SELECT 2\n  FROM t"));
        assert_eq!(at(sql.len()), Some("SELECT 2\n  FROM t"));
        assert_eq!(statement_at("  ", 1), None);
        // A comment line above a statement is not sent with it.
        let commented = "-- users\nSELECT * FROM users";
        assert_eq!(statement_at(commented, 3).map(|r| &commented[r]), Some("SELECT * FROM users"));
    }
}
