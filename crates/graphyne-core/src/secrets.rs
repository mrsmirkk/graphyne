//! Secrets Guard: a regex + entropy scanner for leaked credentials.
//! Used live on open buffers and as a pre-commit gate on the Git index.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretFinding {
    /// 0-based line.
    pub line: usize,
    /// 0-based char column.
    pub column: usize,
    pub kind: &'static str,
    /// Redacted preview, never the full secret.
    pub preview: String,
}

struct Rule {
    kind: &'static str,
    re: Regex,
    /// Capture group holding the secret (for entropy checks); 0 = whole match.
    group: usize,
    min_entropy: f64,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let r = |kind, pat: &str, group, min_entropy| Rule { kind, re: Regex::new(pat).expect("valid secret regex"), group, min_entropy };
        vec![
            r("AWS Access Key ID", r"\b(?:AKIA|ASIA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA)[0-9A-Z]{16}\b", 0, 0.0),
            r("AWS Secret Access Key", r#"(?i)aws.{0,20}(?:secret|private).{0,20}['"]([0-9a-zA-Z/+]{40})['"]"#, 1, 3.5),
            r("Private Key", r"-----BEGIN (?:RSA |DSA |EC |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY(?: BLOCK)?-----", 0, 0.0),
            r("GitHub Token", r"\bgh[pousr]_[A-Za-z0-9]{36,255}\b", 0, 0.0),
            r("GitHub Fine-grained PAT", r"\bgithub_pat_[A-Za-z0-9_]{22,255}\b", 0, 0.0),
            r("GitLab Token", r"\bglpat-[A-Za-z0-9_\-]{20,}\b", 0, 0.0),
            r("Slack Token", r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b", 0, 0.0),
            r("Stripe Live Key", r"\b(?:sk|rk)_live_[0-9a-zA-Z]{24,}\b", 0, 0.0),
            r("Google API Key", r"\bAIza[0-9A-Za-z_\-]{35}\b", 0, 0.0),
            r("JSON Web Token", r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}", 0, 0.0),
            r(
                "High-entropy secret",
                r#"(?i)(?:secret|token|passw(?:or)?d|api[_-]?key|access[_-]?key|client[_-]?secret)["']?\s*[:=]\s*["']([^"'\s]{16,})["']"#,
                1,
                3.6,
            ),
        ]
    })
}

/// Shannon entropy in bits per char.
pub fn shannon_entropy(s: &str) -> f64 {
    let mut counts = [0u32; 256];
    let mut n = 0u32;
    for b in s.bytes() {
        counts[b as usize] += 1;
        n += 1;
    }
    if n == 0 {
        return 0.0;
    }
    counts
        .iter()
        .filter(|c| **c > 0)
        .map(|c| {
            let p = *c as f64 / n as f64;
            -p * p.log2()
        })
        .sum()
}

fn redact(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 8 {
        return "*".repeat(chars.len());
    }
    let head: String = chars[..4].iter().collect();
    format!("{head}…{}", "*".repeat(6))
}

pub fn scan(text: &str) -> Vec<SecretFinding> {
    let mut out = Vec::new();
    for (line_ix, line) in text.lines().enumerate() {
        if line.len() > 4096 {
            continue;
        }
        for rule in rules() {
            for caps in rule.re.captures_iter(line) {
                let Some(m) = caps.get(rule.group).or_else(|| caps.get(0)) else { continue };
                if rule.min_entropy > 0.0 && shannon_entropy(m.as_str()) < rule.min_entropy {
                    continue;
                }
                let column = line[..m.start()].chars().count();
                if out.iter().any(|f: &SecretFinding| f.line == line_ix && f.column == column) {
                    continue;
                }
                out.push(SecretFinding { line: line_ix, column, kind: rule.kind, preview: redact(m.as_str()) });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_aws_and_pem() {
        let text = "key = \"AKIAABCDEFGHIJKLMNOP\"\n-----BEGIN RSA PRIVATE KEY-----\nnothing here";
        let f = scan(text);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].kind, "AWS Access Key ID");
        assert_eq!(f[1].line, 1);
    }
    #[test]
    fn ignores_low_entropy_assignments() {
        assert!(scan("password = \"aaaaaaaaaaaaaaaaaaaa\"").is_empty());
        assert_eq!(scan("api_key = \"q8Zr2LmP0vX7kT4nB9sW1yHc\"").len(), 1);
    }
}
