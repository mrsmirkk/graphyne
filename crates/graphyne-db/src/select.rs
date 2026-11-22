//! Can a query's result be edited like a table? Only a plain `SELECT` of columns from one
//! table: then every row of the result is a row of that table, and every column one of its
//! columns, so changed cells can be written back by primary key.

/// The table a plain `SELECT` reads, as written in the query (`schema` when qualified).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectSource {
    pub schema: Option<String>,
    pub table: String,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// A bare word (keyword or name), as written.
    Word(String),
    /// `"name"`, `` `name` `` or `[name]`, without the quotes.
    Quoted(String),
    /// A string or number literal, a `$1` parameter…
    Literal,
    Punct(char),
}

impl Tok {
    fn is_kw(&self, kw: &str) -> bool {
        matches!(self, Tok::Word(w) if w.eq_ignore_ascii_case(kw))
    }

    fn name(&self) -> Option<String> {
        match self {
            Tok::Word(w) if !KEYWORDS.iter().any(|k| w.eq_ignore_ascii_case(k)) => Some(w.clone()),
            Tok::Quoted(q) => Some(q.clone()),
            _ => None,
        }
    }
}

/// Words that end or change the shape of a query (never a bare table or column name here).
const KEYWORDS: &[&str] = &[
    "select", "from", "where", "group", "having", "order", "limit", "offset", "fetch", "for", "window", "union", "intersect", "except", "join", "inner", "left",
    "right", "full", "outer", "cross", "natural", "straight_join", "on", "using", "as", "distinct", "all", "into", "with", "lateral", "values",
];

fn tokens(sql: &str) -> Vec<Tok> {
    let chars: Vec<char> = sql.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    let quoted = |i: &mut usize, close: char| -> String {
        let mut s = String::new();
        *i += 1;
        while *i < n {
            if chars[*i] == close {
                if close != ']' && *i + 1 < n && chars[*i + 1] == close {
                    s.push(close);
                    *i += 2;
                    continue;
                }
                break;
            }
            s.push(chars[*i]);
            *i += 1;
        }
        *i += 1;
        s
    };
    while i < n {
        let ch = chars[i];
        match ch {
            c if c.is_whitespace() => i += 1,
            '-' if chars.get(i + 1) == Some(&'-') => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
            }
            '#' => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i < n && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    i += 1;
                }
                i += 2;
            }
            '\'' => {
                quoted(&mut i, '\'');
                out.push(Tok::Literal);
            }
            '"' => out.push(Tok::Quoted(quoted(&mut i, '"'))),
            '`' => out.push(Tok::Quoted(quoted(&mut i, '`'))),
            '[' => out.push(Tok::Quoted(quoted(&mut i, ']'))),
            c if c.is_ascii_digit() || (c == '$' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) => {
                i += 1;
                while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                    i += 1;
                }
                out.push(Tok::Literal);
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < n && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$') {
                    i += 1;
                }
                out.push(Tok::Word(chars[start..i].iter().collect()));
            }
            c => {
                out.push(Tok::Punct(c));
                i += 1;
            }
        }
    }
    while out.last() == Some(&Tok::Punct(';')) {
        out.pop();
    }
    out
}

/// A column of the select list: `*`, `t.*`, `col`, `t.col` or `s.t.col` (no expressions,
/// no aliases — the result's column must be the table's column).
fn plain_column(item: &[Tok]) -> bool {
    match item {
        [Tok::Punct('*')] => true,
        [a, Tok::Punct('.'), Tok::Punct('*')] => a.name().is_some(),
        [a] => a.name().is_some(),
        [a, Tok::Punct('.'), b] => a.name().is_some() && b.name().is_some(),
        [a, Tok::Punct('.'), b, Tok::Punct('.'), c] => a.name().is_some() && b.name().is_some() && c.name().is_some(),
        _ => false,
    }
}

/// The table of a plain `SELECT columns FROM table [WHERE …] [ORDER BY …] [LIMIT …]`, or
/// why the result can't be written back.
pub fn single_table_select(sql: &str) -> Result<SelectSource, &'static str> {
    let toks = tokens(sql);
    let Some(first) = toks.first() else { return Err("not a SELECT") };
    if first.is_kw("with") {
        return Err("a query with WITH");
    }
    if !first.is_kw("select") {
        return Err("not a SELECT");
    }
    // Shape words at the top level (subqueries in WHERE are fine).
    let mut depth = 0i32;
    for t in &toks {
        match t {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => depth -= 1,
            _ if depth == 0 => {
                if t.is_kw("union") || t.is_kw("intersect") || t.is_kw("except") {
                    return Err("a UNION of several queries");
                }
                if t.is_kw("group") || t.is_kw("having") || t.is_kw("distinct") {
                    return Err("grouped rows (GROUP BY / DISTINCT)");
                }
                if t.is_kw("join") || t.is_kw("straight_join") || t.is_kw("natural") {
                    return Err("rows of several tables (JOIN)");
                }
                if t.is_kw("into") || t.is_kw("window") {
                    return Err("not a plain SELECT from one table");
                }
            }
            _ => {}
        }
    }
    let from = {
        let mut depth = 0i32;
        toks.iter().position(|t| {
            match t {
                Tok::Punct('(') => depth += 1,
                Tok::Punct(')') => depth -= 1,
                _ => {}
            }
            depth == 0 && t.is_kw("from")
        })
    };
    let Some(from) = from else { return Err("not a SELECT from a table") };
    let mut list = &toks[1..from];
    if list.first().is_some_and(|t| t.is_kw("all")) {
        list = &list[1..];
    }
    if list.is_empty() || !list.split(|t| *t == Tok::Punct(',')).all(plain_column) {
        return Err("computed columns or aliases in the column list");
    }
    // FROM name [. name] [[AS] alias] — then the end, WHERE, ORDER BY, LIMIT…
    let rest = &toks[from + 1..];
    let (schema, table, mut at) = match rest {
        [Tok::Punct('('), ..] => return Err("a subquery in FROM"),
        [a, Tok::Punct('.'), b, ..] => match (a.name(), b.name()) {
            (Some(a), Some(b)) => (Some(a), b, 3),
            _ => return Err("not a SELECT from a table"),
        },
        [a, ..] => match a.name() {
            Some(a) => (None, a, 1),
            None => return Err("not a SELECT from a table"),
        },
        [] => return Err("not a SELECT from a table"),
    };
    if rest.get(at).is_some_and(|t| t.is_kw("as")) {
        at += 1;
        if rest.get(at).and_then(Tok::name).is_none() {
            return Err("not a SELECT from a table");
        }
        at += 1;
    } else if rest.get(at).and_then(Tok::name).is_some() {
        at += 1;
    }
    match rest.get(at) {
        None => {}
        Some(Tok::Punct(',')) => return Err("rows of several tables"),
        Some(t) if ["where", "order", "limit", "offset", "fetch", "for"].iter().any(|k| t.is_kw(k)) => {}
        Some(t) if ["inner", "left", "right", "full", "cross", "outer"].iter().any(|k| t.is_kw(k)) => return Err("rows of several tables (JOIN)"),
        Some(_) => return Err("not a plain SELECT from one table"),
    }
    Ok(SelectSource { schema, table })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(sql: &str) -> Result<(Option<String>, String), &'static str> {
        single_table_select(sql).map(|s| (s.schema, s.table))
    }

    #[test]
    fn plain_selects_are_editable() {
        assert_eq!(table("SELECT * FROM users"), Ok((None, "users".into())));
        assert_eq!(table("select id, name from users where age > 30 order by name limit 10;"), Ok((None, "users".into())));
        assert_eq!(table("SELECT u.* FROM public.users u WHERE u.id IN (SELECT user_id FROM orders GROUP BY user_id)"), Ok((Some("public".into()), "users".into())));
        assert_eq!(table("SELECT \"Id\", [Name] FROM \"My Table\" AS t -- comment\n"), Ok((None, "My Table".into())));
        assert_eq!(table("/* hi */ SELECT `id` FROM `shop`.`items` WHERE note = 'a, b JOIN c'"), Ok((Some("shop".into()), "items".into())));
    }

    #[test]
    fn other_queries_are_read_only() {
        assert!(table("SELECT a.id, b.x FROM a JOIN b ON a.id = b.a").is_err());
        assert!(table("SELECT * FROM a, b").is_err());
        assert!(table("SELECT * FROM a LEFT OUTER JOIN b USING (id)").is_err());
        assert!(table("SELECT count(*) FROM users").is_err());
        assert!(table("SELECT name AS n FROM users").is_err());
        assert!(table("SELECT name || 'x' FROM users").is_err());
        assert!(table("SELECT DISTINCT name FROM users").is_err());
        assert!(table("SELECT city FROM users GROUP BY city").is_err());
        assert!(table("SELECT id FROM a UNION SELECT id FROM b").is_err());
        assert!(table("SELECT * FROM (SELECT * FROM users) s").is_err());
        assert!(table("WITH x AS (SELECT 1) SELECT * FROM x").is_err());
        assert!(table("UPDATE users SET a = 1").is_err());
        assert!(table("SELECT 1").is_err());
    }
}
