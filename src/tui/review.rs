//! What the diff view keeps from one run to the next, a document in the
//! database beside the tabs ([`crate::db::DIFF`]): the files marked
//! reviewed, and whether its files are listed as a tree.
//!
//! Marks belong to one diff of one worktree, made at one commit: once HEAD
//! moves, the next diff is new work to read, and none of them hold. Each
//! keeps its file's hash too ([`super::diff::FileDiff::hash`]), so a file
//! that has changed again since comes back unmarked. A pull request's diff
//! isn't the worktree's: its marks are kept apart, by its number, and only
//! a file changing takes them off.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The files marked reviewed, by path, each with its hash when it was.
pub type Marks = BTreeMap<String, u64>;

/// Which diff marks are for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// The worktree, or, for a pull request, its project.
    pub dir: PathBuf,
    /// Which of its diffs: `uncommitted`, `branch`, or `pull request 57`.
    pub diff: String,
    /// The commit HEAD was on; empty for a pull request.
    pub head: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Kept {
    /// Whether the diff view lists its files as a tree.
    #[serde(default)]
    pub tree: bool,
    #[serde(default)]
    reviewed: Vec<Reviewed>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Reviewed {
    #[serde(flatten)]
    scope: Scope,
    files: Marks,
}

impl Kept {
    /// The marks kept for the diff `scope` names, if they were made at the
    /// commit it's at.
    pub fn marks(&self, scope: &Scope) -> Marks {
        self.reviewed
            .iter()
            .find(|kept| kept.scope == *scope)
            .map(|kept| kept.files.clone())
            .unwrap_or_default()
    }

    /// Keeps `files` as the marks of the diff `scope` names, in place of
    /// what it had, at whichever commit. Those of worktrees that have gone
    /// go too.
    pub fn set_marks(&mut self, scope: Scope, files: Marks) {
        self.reviewed.retain(|kept| {
            let same = kept.scope.dir == scope.dir && kept.scope.diff == scope.diff;
            !same && kept.scope.dir.is_dir()
        });
        if !files.is_empty() {
            self.reviewed.push(Reviewed { scope, files });
        }
    }
}

/// What's kept, from the database's document: when there's none, or it
/// can't be read, it's nothing kept, since losing a mark is no reason to
/// stop.
pub fn read(json: Option<&str>) -> Kept {
    json.and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn scope(dir: &Path, diff: &str, head: &str) -> Scope {
        Scope {
            dir: dir.to_path_buf(),
            diff: diff.to_string(),
            head: head.to_string(),
        }
    }

    fn marks(paths: &[&str]) -> Marks {
        paths.iter().map(|path| (path.to_string(), 7)).collect()
    }

    #[test]
    fn marks_hold_for_their_diff_at_their_commit_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut kept = read(None);
        kept.set_marks(scope(dir.path(), "uncommitted", "c1"), marks(&["a.rs"]));
        let json = serde_json::to_string(&kept).unwrap();

        let kept = read(Some(&json));
        let at = |diff, head| kept.marks(&scope(dir.path(), diff, head));
        assert_eq!(at("uncommitted", "c1"), marks(&["a.rs"]));
        // A commit moved HEAD; the branch's diff is another diff.
        assert!(at("uncommitted", "c2").is_empty());
        assert!(at("branch", "c1").is_empty());
    }

    #[test]
    fn new_marks_replace_a_diffs_old_ones_and_gone_worktrees_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("gone");
        std::fs::create_dir(&gone).unwrap();
        let mut kept = Kept::default();
        kept.set_marks(scope(&gone, "uncommitted", "c1"), marks(&["x.rs"]));
        kept.set_marks(scope(dir.path(), "uncommitted", "c1"), marks(&["a.rs"]));
        kept.set_marks(scope(dir.path(), "uncommitted", "c2"), marks(&["b.rs"]));
        assert_eq!(kept.reviewed.len(), 2);

        std::fs::remove_dir(&gone).unwrap();
        kept.set_marks(scope(dir.path(), "uncommitted", "c2"), Marks::new());
        assert!(kept.reviewed.is_empty());
    }

    #[test]
    fn a_document_that_cant_be_read_is_nothing_kept() {
        let kept = read(Some("not json"));
        assert!(!kept.tree);
        assert!(kept.reviewed.is_empty());
    }
}
