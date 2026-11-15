//! CamelHump / substring fuzzy ranking on top of `nucleo`.
//!
//! `gupbi` and `ProfileId` both find `getUserProfileById`; matched char indices are
//! returned so the UI can embolden them.

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};

#[derive(Clone, Debug)]
pub struct FuzzyMatch {
    pub index: usize,
    pub score: u32,
    /// Matched char indices into the candidate, sorted & deduplicated.
    pub indices: Vec<u32>,
}

/// Rank `candidates` against `query`. When `paths` is true the matcher uses
/// path-aware bonuses (separators, file name).
pub fn fuzzy_filter<S: AsRef<str>>(candidates: &[S], query: &str, limit: usize, paths: bool) -> Vec<FuzzyMatch> {
    let config = if paths { Config::DEFAULT.match_paths() } else { Config::DEFAULT };
    let mut matcher = Matcher::new(config);
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    for (index, cand) in candidates.iter().enumerate() {
        let cand = cand.as_ref();
        if query.is_empty() {
            out.push(FuzzyMatch { index, score: 0, indices: Vec::new() });
            if out.len() >= limit {
                break;
            }
            continue;
        }
        let haystack = Utf32Str::new(cand, &mut buf);
        let mut indices = Vec::new();
        if let Some(mut score) = pattern.indices(haystack, &mut matcher, &mut indices) {
            indices.sort_unstable();
            indices.dedup();
            if paths {
                // Prefer hits inside the file name over hits in directories.
                let name_start = cand.rfind('/').map(|i| cand[..=i].chars().count() as u32).unwrap_or(0);
                let in_name = indices.iter().filter(|i| **i >= name_start).count() as u32;
                score += in_name * 8;
            }
            out.push(FuzzyMatch { index, score, indices });
        }
    }
    if !query.is_empty() {
        out.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| candidates[a.index].as_ref().len().cmp(&candidates[b.index].as_ref().len()))
        });
        out.truncate(limit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_hump() {
        let c = ["getUserProfileById", "getUser", "profile_id_cache", "UserProfile"];
        let r = fuzzy_filter(&c, "gupbi", 10, false);
        assert_eq!(r[0].index, 0);
        let r = fuzzy_filter(&c, "ProfileId", 10, false);
        assert!(r.iter().any(|m| m.index == 0));
    }
}
