//! Graphyne git engine — direct libgit2 access, never shells out to `git`.
//! All functions block; call them from background executors.

use anyhow::{Context, Result, anyhow};
use git2::{Repository, Sort, Status, StatusOptions};
use std::ops::Range;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct RepoInfo {
    pub root: PathBuf,
    pub branch: String,
}

/// Find the repository containing `path` and its current branch.
pub fn discover(path: &Path) -> Option<RepoInfo> {
    let repo = Repository::discover(path).ok()?;
    let root = repo.workdir()?.to_path_buf();
    let branch = match repo.head() {
        Ok(head) if head.is_branch() => head.shorthand().unwrap_or("HEAD").to_string(),
        Ok(head) => head.target().map(|o| format!("detached {}", &o.to_string()[..7])).unwrap_or_else(|| "HEAD".into()),
        // Unborn branch (fresh `git init`): read the symbolic target.
        Err(_) => repo
            .find_reference("HEAD")
            .ok()
            .and_then(|r| r.symbolic_target().map(|t| t.trim_start_matches("refs/heads/").to_string()))
            .unwrap_or_else(|| "main".into()),
    };
    Some(RepoInfo { root, branch })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

impl ChangeKind {
    pub fn letter(&self) -> &'static str {
        match self {
            ChangeKind::Added => "A",
            ChangeKind::Modified => "M",
            ChangeKind::Deleted => "D",
            ChangeKind::Renamed => "R",
            ChangeKind::Untracked => "U",
            ChangeKind::Conflicted => "C",
        }
    }
}

#[derive(Clone, Debug)]
pub struct FileChange {
    pub rel_path: String,
    pub kind: ChangeKind,
    pub staged: bool,
}

pub fn status(root: &Path) -> Result<Vec<FileChange>> {
    let repo = Repository::open(root)?;
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true).include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts))?;
    let mut out = Vec::new();
    for entry in statuses.iter() {
        let Some(path) = entry.path() else { continue };
        let s = entry.status();
        let kind = if s.contains(Status::CONFLICTED) {
            ChangeKind::Conflicted
        } else if s.intersects(Status::WT_NEW) && !s.intersects(Status::INDEX_NEW) {
            ChangeKind::Untracked
        } else if s.intersects(Status::INDEX_NEW) {
            ChangeKind::Added
        } else if s.intersects(Status::WT_DELETED | Status::INDEX_DELETED) {
            ChangeKind::Deleted
        } else if s.intersects(Status::WT_RENAMED | Status::INDEX_RENAMED) {
            ChangeKind::Renamed
        } else if s.intersects(Status::WT_MODIFIED | Status::INDEX_MODIFIED | Status::WT_TYPECHANGE | Status::INDEX_TYPECHANGE) {
            ChangeKind::Modified
        } else {
            continue;
        };
        let staged = s.intersects(Status::INDEX_NEW | Status::INDEX_MODIFIED | Status::INDEX_DELETED | Status::INDEX_RENAMED);
        out.push(FileChange { rel_path: path.to_string(), kind, staged });
    }
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct CommitInfo {
    pub id: String,
    pub short_id: String,
    pub summary: String,
    pub author: String,
    /// Unix seconds.
    pub time: i64,
    pub refs: Vec<String>,
    pub parent_ids: Vec<String>,
}

/// Commits reachable from HEAD and every local / remote branch, newest first,
/// children always before their parents.
pub fn log(root: &Path, limit: usize) -> Result<Vec<CommitInfo>> {
    let repo = Repository::open(root)?;
    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TIME | Sort::TOPOLOGICAL)?;
    let head = walk.push_head().is_ok();
    let heads = walk.push_glob("heads").is_ok();
    let _ = walk.push_glob("remotes");
    if !head && !heads {
        return Ok(Vec::new()); // unborn branch
    }
    // Map commit id -> branch / tag names.
    let mut ref_names: std::collections::HashMap<git2::Oid, Vec<String>> = Default::default();
    if let Ok(refs) = repo.references() {
        for r in refs.flatten() {
            if let (Some(name), Ok(target)) = (r.shorthand(), r.peel_to_commit()) {
                ref_names.entry(target.id()).or_default().push(name.to_string());
            }
        }
    }
    let mut out = Vec::new();
    for oid in walk.take(limit) {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        let id = oid.to_string();
        out.push(CommitInfo {
            short_id: id[..7.min(id.len())].to_string(),
            id,
            summary: commit.summary().unwrap_or("").to_string(),
            author: commit.author().name().unwrap_or("unknown").to_string(),
            time: commit.time().seconds(),
            refs: ref_names.remove(&oid).unwrap_or_default(),
            parent_ids: commit.parent_ids().map(|p| p.to_string()).collect(),
        });
    }
    Ok(out)
}

/// How a graph segment crosses one history row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgePart {
    /// A branch passing by: top of lane `from` to bottom of lane `to`.
    Through,
    /// A child line ending in this row's commit: top of `from` to the node.
    In,
    /// A parent line leaving this row's commit: the node to the bottom of `to`.
    Out,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphEdge {
    pub from: usize,
    pub to: usize,
    pub part: EdgePart,
}

/// Commit-graph layout for one row of [`log`] output.
#[derive(Clone, Debug, Default)]
pub struct GraphRow {
    /// Column of the commit node.
    pub lane: usize,
    pub edges: Vec<GraphEdge>,
    /// Columns in use in this row.
    pub width: usize,
}

/// Assign every commit a lane and the line segments that connect it to its parents.
/// Lanes never shift sideways, so a branch keeps its column (and colour) for its lifetime;
/// freed columns are reused by the next new branch.
pub fn graph(commits: &[CommitInfo]) -> Vec<GraphRow> {
    fn free_slot(lanes: &mut Vec<Option<usize>>) -> usize {
        match lanes.iter().position(Option::is_none) {
            Some(i) => i,
            None => {
                lanes.push(None);
                lanes.len() - 1
            }
        }
    }
    let index: std::collections::HashMap<&str, usize> = commits.iter().enumerate().map(|(i, c)| (c.id.as_str(), i)).collect();
    // Each lane holds the commit (by id key) it is waiting for. Commits outside the
    // loaded range get a key past the end so they still occupy a lane.
    let mut keys: std::collections::HashMap<&str, usize> = Default::default();
    let mut lanes: Vec<Option<usize>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());
    for (ix, c) in commits.iter().enumerate() {
        let hits: Vec<usize> = lanes.iter().enumerate().filter(|(_, l)| **l == Some(ix)).map(|(i, _)| i).collect();
        let lane = match hits.first() {
            Some(&l) => l,
            None => free_slot(&mut lanes),
        };
        let before = lanes.clone();
        for &h in &hits {
            lanes[h] = None;
        }
        let mut edges = Vec::new();
        for (i, l) in before.iter().enumerate() {
            match l {
                Some(x) if *x == ix => edges.push(GraphEdge { from: i, to: lane, part: EdgePart::In }),
                Some(_) => edges.push(GraphEdge { from: i, to: i, part: EdgePart::Through }),
                None => {}
            }
        }
        for (k, p) in c.parent_ids.iter().enumerate() {
            let target_key = match index.get(p.as_str()) {
                Some(&i) => i,
                None => {
                    let n = keys.len();
                    commits.len() + *keys.entry(p.as_str()).or_insert(n)
                }
            };
            let to = if let Some(j) = lanes.iter().position(|l| *l == Some(target_key)) {
                j
            } else if k == 0 {
                lanes[lane] = Some(target_key);
                lane
            } else {
                let j = free_slot(&mut lanes);
                lanes[j] = Some(target_key);
                j
            };
            edges.push(GraphEdge { from: lane, to, part: EdgePart::Out });
        }
        while lanes.last() == Some(&None) {
            lanes.pop();
        }
        let width = before.len().max(lanes.len()).max(lane + 1);
        rows.push(GraphRow { lane, edges, width });
    }
    rows
}

/// Content of `rel_path` at `HEAD` (None for new / untracked files or binary blobs).
pub fn head_text(root: &Path, rel_path: &str) -> Option<String> {
    let repo = Repository::open(root).ok()?;
    let tree = repo.head().ok()?.peel_to_tree().ok()?;
    let entry = tree.get_path(Path::new(rel_path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    if blob.is_binary() {
        return None;
    }
    Some(String::from_utf8_lossy(blob.content()).replace("\r\n", "\n"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineChange {
    Added,
    Modified,
    Deleted,
}

/// A changed region: `new` lines in the buffer, `old` lines in the base (0-based, half-open).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunk {
    pub kind: LineChange,
    pub new: Range<u32>,
    pub old: Range<u32>,
}

/// Line diff between `base` (HEAD) and `current` (buffer) — gutter markers.
pub fn line_diff(base: &str, current: &str) -> Vec<DiffHunk> {
    use imara_diff::intern::InternedInput;
    use imara_diff::{Algorithm, diff};
    let input = InternedInput::new(base, current);
    let mut hunks = Vec::new();
    diff(Algorithm::Histogram, &input, |before: Range<u32>, after: Range<u32>| {
        let kind = if before.is_empty() {
            LineChange::Added
        } else if after.is_empty() {
            LineChange::Deleted
        } else {
            LineChange::Modified
        };
        hunks.push(DiffHunk { kind, new: after, old: before });
    });
    hunks
}

/// What a side-by-side row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideKind {
    Same,
    Modified,
    Added,
    Deleted,
}

/// A changed block, as seen from one of its rows: the connector between the two columns
/// is drawn from these numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HunkShape {
    /// Row index inside the block.
    pub index: u32,
    /// Lines of the block on the left (old) and right (new) side.
    pub old_len: u32,
    pub new_len: u32,
}

/// One aligned row of a side-by-side diff: a line of `base` (left), of `current` (right),
/// or both. Inside a changed block the shorter side gets empty filler rows, so one scroll
/// position always shows matching content on both sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SideRow {
    /// 0-based line numbers.
    pub left: Option<u32>,
    pub right: Option<u32>,
    pub kind: SideKind,
    pub hunk: Option<HunkShape>,
}

pub fn side_by_side(base: &str, current: &str) -> Vec<SideRow> {
    let (old_total, new_total) = (base.lines().count() as u32, current.lines().count() as u32);
    let mut rows = Vec::new();
    let (mut old, mut new) = (0u32, 0u32);
    let mut same_until = |rows: &mut Vec<SideRow>, old: &mut u32, new: &mut u32, old_end: u32, new_end: u32| {
        while *old < old_end && *new < new_end {
            rows.push(SideRow { left: Some(*old), right: Some(*new), kind: SideKind::Same, hunk: None });
            *old += 1;
            *new += 1;
        }
    };
    for h in line_diff(base, current) {
        same_until(&mut rows, &mut old, &mut new, h.old.start, h.new.start);
        let (old_len, new_len) = (h.old.end - h.old.start, h.new.end - h.new.start);
        let kind = match h.kind {
            LineChange::Added => SideKind::Added,
            LineChange::Deleted => SideKind::Deleted,
            LineChange::Modified => SideKind::Modified,
        };
        for i in 0..old_len.max(new_len) {
            rows.push(SideRow {
                left: (i < old_len).then_some(h.old.start + i),
                right: (i < new_len).then_some(h.new.start + i),
                kind,
                hunk: Some(HunkShape { index: i, old_len, new_len }),
            });
        }
        old = h.old.end;
        new = h.new.end;
    }
    same_until(&mut rows, &mut old, &mut new, old_total, new_total);
    rows
}

fn hunk_signature(old: &[&str], new: &[&str]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    old.hash(&mut h);
    0xFFu8.hash(&mut h);
    new.hash(&mut h);
    h.finish()
}

fn clamp_range(r: &Range<u32>, len: usize) -> Range<usize> {
    (r.start as usize).min(len)..(r.end as usize).min(len)
}

/// Identity of each changed block (in order) by its old and new lines, so a block keeps
/// its "selected for commit" state while other parts of the file are edited. LF texts.
pub fn hunk_signatures(base: &str, current: &str) -> Vec<u64> {
    let (b, c): (Vec<&str>, Vec<&str>) = (base.lines().collect(), current.lines().collect());
    line_diff(base, current).iter().map(|h| hunk_signature(&b[clamp_range(&h.old, b.len())], &c[clamp_range(&h.new, c.len())])).collect()
}

/// The content to stage when only some blocks are selected: `base` with every changed
/// block applied except those whose signature is in `excluded`. Returns the text and
/// (applied, total) block counts. LF texts.
pub fn apply_selected_hunks(base: &str, current: &str, excluded: &std::collections::HashSet<u64>) -> (String, usize, usize) {
    let (b_lines, c_lines): (Vec<&str>, Vec<&str>) = (base.lines().collect(), current.lines().collect());
    let (b_raw, c_raw): (Vec<&str>, Vec<&str>) = (base.split_inclusive('\n').collect(), current.split_inclusive('\n').collect());
    let hunks = line_diff(base, current);
    let mut out = String::with_capacity(current.len().max(base.len()));
    let mut pos = 0usize;
    let mut applied = 0;
    for h in &hunks {
        let (old, new) = (clamp_range(&h.old, b_raw.len()), clamp_range(&h.new, c_raw.len()));
        out.extend(b_raw[pos.min(old.start)..old.start].iter().copied());
        let sig = hunk_signature(&b_lines[clamp_range(&h.old, b_lines.len())], &c_lines[clamp_range(&h.new, c_lines.len())]);
        if excluded.contains(&sig) {
            out.extend(b_raw[old.clone()].iter().copied());
        } else {
            applied += 1;
            out.extend(c_raw[new].iter().copied());
        }
        pos = old.end;
    }
    out.extend(b_raw[pos.min(b_raw.len())..].iter().copied());
    (out, applied, hunks.len())
}

/// Changed byte ranges inside a modified pair of lines, word by word (identifier runs,
/// whitespace runs, single other characters). Very long lines are marked as a whole.
pub fn word_diff(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    fn tokens(s: &str) -> Vec<Range<usize>> {
        let mut out: Vec<Range<usize>> = Vec::new();
        let class = |c: char| if c.is_alphanumeric() || c == '_' { 0 } else if c.is_whitespace() { 1 } else { 2 };
        let mut last: Option<u8> = None;
        for (i, ch) in s.char_indices() {
            let k = class(ch);
            match out.last_mut() {
                Some(r) if last == Some(k) && k != 2 => r.end = i + ch.len_utf8(),
                _ => out.push(i..i + ch.len_utf8()),
            }
            last = Some(k);
        }
        out
    }
    let (a, b) = (tokens(old), tokens(new));
    if a.len() * b.len() > 160_000 {
        return (vec![0..old.len()], vec![0..new.len()]);
    }
    // LCS over tokens; unmatched tokens are the changes.
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![0u16; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * (m + 1) + j] = if old[a[i].clone()] == new[b[j].clone()] {
                dp[(i + 1) * (m + 1) + j + 1] + 1
            } else {
                dp[(i + 1) * (m + 1) + j].max(dp[i * (m + 1) + j + 1])
            };
        }
    }
    let (mut keep_a, mut keep_b) = (vec![false; n], vec![false; m]);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[a[i].clone()] == new[b[j].clone()] {
            keep_a[i] = true;
            keep_b[j] = true;
            i += 1;
            j += 1;
        } else if dp[(i + 1) * (m + 1) + j] >= dp[i * (m + 1) + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let changed = |toks: &[Range<usize>], keep: &[bool], text: &str| {
        let mut out: Vec<Range<usize>> = Vec::new();
        for (r, k) in toks.iter().zip(keep) {
            // Whitespace alone is not worth a highlight.
            if *k || text[r.clone()].trim().is_empty() {
                continue;
            }
            match out.last_mut() {
                Some(last) if text[last.end..r.start].trim().is_empty() => last.end = r.end,
                _ => out.push(r.clone()),
            }
        }
        out
    };
    (changed(&a, &keep_a, old), changed(&b, &keep_b, new))
}

/// Stage exactly `rel_paths` (adds / removals) and create a commit on HEAD.
pub fn commit_paths(root: &Path, rel_paths: &[String], message: &str) -> Result<String> {
    commit_selection(root, rel_paths, &[], message)
}

/// Commit whole files (`rel_paths`) plus files of which only some changed blocks are
/// selected (`partial`: path and the exact content to stage, see [`apply_selected_hunks`]).
pub fn commit_selection(root: &Path, rel_paths: &[String], partial: &[(String, Vec<u8>)], message: &str) -> Result<String> {
    if message.trim().is_empty() {
        return Err(anyhow!("Commit message is empty"));
    }
    if rel_paths.is_empty() && partial.is_empty() {
        return Err(anyhow!("Nothing selected to commit"));
    }
    let repo = Repository::open(root)?;
    // Before touching the index: a failed commit must leave the staging area as it was.
    let sig = repo
        .signature()
        .context("No git identity configured (set user.name and user.email)")?;
    let mut index = repo.index()?;
    for rel in rel_paths {
        if root.join(rel).exists() {
            index.add_path(Path::new(rel)).with_context(|| format!("staging {rel}"))?;
        } else {
            index.remove_path(Path::new(rel)).with_context(|| format!("removing {rel}"))?;
        }
    }
    for (rel, data) in partial {
        let entry = index.get_path(Path::new(rel), 0).unwrap_or_else(|| git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode: 0o100644,
            uid: 0,
            gid: 0,
            file_size: 0,
            id: git2::Oid::zero(),
            flags: 0,
            flags_extended: 0,
            path: rel.as_bytes().to_vec(),
        });
        index.add_frombuffer(&entry, data).with_context(|| format!("staging selected changes of {rel}"))?;
    }
    index.write()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;
    Ok(oid.to_string()[..7].to_string())
}

/// Relative path of `file` inside `root`, with `/` separators.
pub fn rel_path(root: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_kinds() {
        let base = "a\nb\nc\nd\n";
        let cur = "a\nB\nc\nnew\nd\n";
        let h = line_diff(base, cur);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].kind, LineChange::Modified);
        assert_eq!(h[0].new, 1..2);
        assert_eq!(h[1].kind, LineChange::Added);
        assert_eq!(h[1].new, 3..4);
        let h = line_diff("a\nb\n", "a\n");
        assert_eq!(h[0].kind, LineChange::Deleted);
    }

    #[test]
    fn side_by_side_alignment() {
        let rows = side_by_side("a\nb\nc\nd\nold\n", "a\nB\nc\nnew\nd\n");
        let view: Vec<(Option<u32>, Option<u32>, SideKind)> = rows.iter().map(|r| (r.left, r.right, r.kind)).collect();
        assert_eq!(
            view,
            vec![
                (Some(0), Some(0), SideKind::Same),
                (Some(1), Some(1), SideKind::Modified),
                (Some(2), Some(2), SideKind::Same),
                (None, Some(3), SideKind::Added),
                (Some(3), Some(4), SideKind::Same),
                (Some(4), None, SideKind::Deleted),
            ]
        );
        // A 1-line block replaced by 3 lines: filler rows on the left, shape for the connector.
        let rows = side_by_side("x\nold\ny\n", "x\nn1\nn2\nn3\ny\n");
        assert_eq!(rows[1].hunk, Some(HunkShape { index: 0, old_len: 1, new_len: 3 }));
        assert_eq!((rows[3].left, rows[3].right), (None, Some(3)));
        assert_eq!((rows[4].left, rows[4].right, rows[4].kind), (Some(2), Some(4), SideKind::Same));
        // New file: everything added.
        assert!(side_by_side("", "a\nb\n").iter().all(|r| r.kind == SideKind::Added && r.left.is_none()));
    }

    #[test]
    fn partial_staging_and_word_diff() {
        let base = "a\nb\nc\nd\n";
        let current = "a\nB\nc\nd\ne\n";
        let sigs = hunk_signatures(base, current);
        assert_eq!(sigs.len(), 2);
        let none = std::collections::HashSet::new();
        assert_eq!(apply_selected_hunks(base, current, &none), (current.to_string(), 2, 2));
        let skip_second: std::collections::HashSet<u64> = [sigs[1]].into();
        assert_eq!(apply_selected_hunks(base, current, &skip_second), ("a\nB\nc\nd\n".to_string(), 1, 2));
        let skip_all: std::collections::HashSet<u64> = sigs.iter().copied().collect();
        assert_eq!(apply_selected_hunks(base, current, &skip_all).0, base);
        // Signatures survive edits elsewhere in the file.
        assert_eq!(hunk_signatures(base, "x\na\nB\nc\nd\ne\n")[1..], sigs[..]);

        let (old, new) = word_diff("let total = items.len();", "let count = items.len() + 1;");
        assert_eq!(old.iter().map(|r| &"let total = items.len();"[r.clone()]).collect::<Vec<_>>(), ["total"]);
        assert_eq!(new.iter().map(|r| &"let count = items.len() + 1;"[r.clone()]).collect::<Vec<_>>(), ["count", "+ 1"]);
    }

    fn commit(id: &str, parents: &[&str]) -> CommitInfo {
        CommitInfo {
            id: id.into(),
            short_id: id.into(),
            summary: String::new(),
            author: String::new(),
            time: 0,
            refs: Vec::new(),
            parent_ids: parents.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn graph_branch_and_merge() {
        // M merges feature F into B; both branch off A.
        let g = graph(&[commit("M", &["B", "F"]), commit("F", &["A"]), commit("B", &["A"]), commit("A", &[])]);
        let e = |from, to, part| GraphEdge { from, to, part };
        assert_eq!(g[0].lane, 0);
        assert_eq!(g[0].edges, vec![e(0, 0, EdgePart::Out), e(0, 1, EdgePart::Out)]);
        assert_eq!(g[1].lane, 1);
        assert_eq!(g[1].edges, vec![e(0, 0, EdgePart::Through), e(1, 1, EdgePart::In), e(1, 1, EdgePart::Out)]);
        assert_eq!(g[2].lane, 0);
        assert_eq!(g[2].edges, vec![e(0, 0, EdgePart::In), e(1, 1, EdgePart::Through), e(0, 1, EdgePart::Out)]);
        assert_eq!(g[3].lane, 1);
        assert_eq!(g[3].edges, vec![e(1, 1, EdgePart::In)]);
        assert_eq!(g.iter().map(|r| r.width).max(), Some(2));
    }

    #[test]
    fn graph_parent_outside_loaded_range() {
        let g = graph(&[commit("B", &["A"])]);
        assert_eq!(g[0].edges, vec![GraphEdge { from: 0, to: 0, part: EdgePart::Out }]);
    }
}
