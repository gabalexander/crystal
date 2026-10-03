//! Which project a directory belongs to: the git repository it's in, named
//! after its main worktree's directory as the sidebar names it, or, outside
//! any repository, the directory itself. A project's backlog and the tasks
//! done in it are kept by project, so every worktree of a repository shares
//! them.

use crate::git::Checkout;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    /// The main worktree's directory, or the directory itself outside git.
    pub path: PathBuf,
}

/// The project `dir` is in.
pub fn of(dir: &Path) -> Project {
    if let Some(checkout) = Checkout::find(dir) {
        let worktree = checkout.worktree();
        return Project {
            name: worktree.project,
            path: worktree.project_path,
        };
    }
    let path = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    Project {
        name: name_of(&path),
        path,
    }
}

/// A directory's own name, or the whole path for one that has none, like
/// `/`.
fn name_of(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_outside_git_is_a_project_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes");
        std::fs::create_dir(&notes).unwrap();
        let project = of(&notes);
        assert_eq!(project.name, "notes");
        assert_eq!(project.path, std::fs::canonicalize(&notes).unwrap());
    }
}
