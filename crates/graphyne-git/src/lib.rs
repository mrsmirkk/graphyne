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

/// Stage exactly `rel_paths` (adds / removals) and create a commit on HEAD.
pub fn commit_paths(root: &Path, rel_paths: &[String], message: &str) -> Result<String> {
    if message.trim().is_empty() {
        return Err(anyhow!("Commit message is empty"));
    }
    if rel_paths.is_empty() {
        return Err(anyhow!("Nothing selected to commit"));
    }
    let repo = Repository::open(root)?;
    let mut index = repo.index()?;
    for rel in rel_paths {
        if root.join(rel).exists() {
            index.add_path(Path::new(rel)).with_context(|| format!("staging {rel}"))?;
        } else {
            index.remove_path(Path::new(rel)).with_context(|| format!("removing {rel}"))?;
        }
    }
    index.write()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let sig = repo
        .signature()
        .context("No git identity configured (set user.name and user.email)")?;
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
