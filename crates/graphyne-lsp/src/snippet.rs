//! LSP snippet syntax (`$1`, `${1:default}`, `${1|a,b|}`, `$0`, `\$`) → plain text.

use std::ops::Range;

/// Expanded snippet: text to insert and the char range to select afterwards
/// (first tab stop's placeholder, or an empty range at `$0`, or the end).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expanded {
    pub text: String,
    pub selection: Range<usize>,
}

pub fn expand(snippet: &str) -> Expanded {
    let chars: Vec<char> = snippet.chars().collect();
    let mut out = String::new();
    let mut stops: Vec<(u32, Range<usize>)> = Vec::new();
    parse(&chars, &mut 0, &mut out, &mut stops, false);
    let len = out.chars().count();
    let first = stops.iter().filter(|(n, _)| *n > 0).min_by_key(|(n, _)| *n).map(|(_, r)| r.clone());
    let zero = stops.iter().find(|(n, _)| *n == 0).map(|(_, r)| r.clone());
    let selection = first.or(zero).unwrap_or(len..len);
    Expanded { text: out, selection }
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Recursive-descent parse; stops at an unescaped `}` when `nested`.
fn parse(c: &[char], i: &mut usize, out: &mut String, stops: &mut Vec<(u32, Range<usize>)>, nested: bool) {
    while *i < c.len() {
        match c[*i] {
            '\\' if *i + 1 < c.len() => {
                out.push(c[*i + 1]);
                *i += 2;
            }
            '}' if nested => return,
            '$' => {
                *i += 1;
                if *i < c.len() && c[*i].is_ascii_digit() {
                    let n = read_number(c, i);
                    let at = char_len(out);
                    stops.push((n, at..at));
                } else if *i < c.len() && c[*i] == '{' {
                    *i += 1;
                    if *i < c.len() && c[*i].is_ascii_digit() {
                        let n = read_number(c, i);
                        let start = char_len(out);
                        if *i < c.len() && c[*i] == ':' {
                            *i += 1;
                            parse(c, i, out, stops, true);
                        } else if *i < c.len() && c[*i] == '|' {
                            // Choice: take the first option.
                            *i += 1;
                            let mut first = String::new();
                            let mut done_first = false;
                            while *i < c.len() && c[*i] != '|' {
                                if c[*i] == ',' {
                                    done_first = true;
                                } else if !done_first {
                                    first.push(c[*i]);
                                }
                                *i += 1;
                            }
                            *i += 1; // '|'
                            out.push_str(&first);
                        }
                        if *i < c.len() && c[*i] == '}' {
                            *i += 1;
                        }
                        stops.push((n, start..char_len(out)));
                    } else {
                        // Variable `${NAME:default}` — keep the default.
                        while *i < c.len() && (c[*i].is_ascii_alphanumeric() || c[*i] == '_') {
                            *i += 1;
                        }
                        if *i < c.len() && c[*i] == ':' {
                            *i += 1;
                            parse(c, i, out, stops, true);
                        }
                        if *i < c.len() && c[*i] == '}' {
                            *i += 1;
                        }
                    }
                } else {
                    // `$NAME` variable: drop it.
                    while *i < c.len() && (c[*i].is_ascii_alphanumeric() || c[*i] == '_') {
                        *i += 1;
                    }
                }
            }
            ch => {
                out.push(ch);
                *i += 1;
            }
        }
    }
}

fn read_number(c: &[char], i: &mut usize) -> u32 {
    let mut n = 0u32;
    while *i < c.len() && c[*i].is_ascii_digit() {
        n = n * 10 + c[*i].to_digit(10).unwrap_or(0);
        *i += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders() {
        // Per the snippet grammar a `}` inside a placeholder is escaped.
        let e = expand("println!(\"${1:{\\}}\", $2)$0");
        assert_eq!(e.text, "println!(\"{}\", )");
        assert_eq!(e.selection, 10..12);
        let e = expand("foo($1)$0");
        assert_eq!(e.text, "foo()");
        assert_eq!(e.selection, 4..4);
        let e = expand("plain");
        assert_eq!(e.selection, 5..5);
        let e = expand("${1|one,two|} \\$x");
        assert_eq!(e.text, "one $x");
        assert_eq!(e.selection, 0..3);
        let e = expand("new(${1:a: ${2:i32}})");
        assert_eq!(e.text, "new(a: i32)");
    }
}
