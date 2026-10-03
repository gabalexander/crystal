//! What crystal needs from git: which project and worktree a directory is
//! in, which branch that worktree is on, making and removing worktrees, and
//! what changed in one, for the diff and the file finder. It runs the `git`
//! command rather than using a library, so it behaves exactly like the git
//! the user runs.

use crate::protocol::Worktree;
use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where a directory sits in git, found once. A session's directory never
/// changes, so neither does this; the branch can, so it's read again each
/// time it's asked for.
#[derive(Debug, Clone)]
pub struct Checkout {
    project: String,
    project_path: PathBuf,
    worktree: PathBuf,
    main: bool,
    /// The worktree's HEAD file, which says which branch it's on.
    head: PathBuf,
}

impl Checkout {
    /// The repository and worktree `dir` is in, or `None` when it isn't in
    /// one, or git isn't installed.
    pub fn find(dir: &Path) -> Option<Checkout> {
        let args = [
            "rev-parse",
            "--show-toplevel",
            "--git-dir",
            "--git-common-dir",
        ];
        let output = git(dir, &args).ok()?;
        let mut lines = output.lines();
        let worktree = absolute(dir, lines.next()?)?;
        let git_dir = absolute(dir, lines.next()?)?;
        let common_dir = absolute(dir, lines.next()?)?;
        let (project, project_path) = project_of(&common_dir);
        Some(Checkout {
            project,
            project_path,
            worktree,
            // A linked worktree has a git dir of its own, inside the shared
            // one; the main worktree uses the shared one.
            main: git_dir == common_dir,
            head: git_dir.join("HEAD"),
        })
    }

    /// The worktree as it is now: reading one small file tells which
    /// branch it's on, much cheaper than running git.
    pub fn worktree(&self) -> Worktree {
        let head = std::fs::read_to_string(&self.head).unwrap_or_default();
        Worktree {
            project: self.project.clone(),
            project_path: self.project_path.clone(),
            path: self.worktree.clone(),
            main: self.main,
            branch: branch_from_head(&head),
        }
    }
}

/// Makes a worktree for `branch` in the repository `dir` is in, and returns
/// its directory. A new branch starts from the commit `dir`'s worktree is
/// on; a branch that exists already is checked out as it is.
pub fn add_worktree(dir: &Path, branch: &str) -> Result<PathBuf> {
    let checkout = Checkout::find(dir)
        .with_context(|| format!("{} isn't in a git repository", dir.display()))?;
    let target = worktree_dir(&checkout.project_path, branch);
    let target_arg = target.to_string_lossy();
    if branch_exists(dir, branch) {
        git(dir, &["worktree", "add", &target_arg, branch])?;
    } else {
        git(dir, &["worktree", "add", "-b", branch, &target_arg])?;
    }
    Ok(target)
}

/// The worktree `target` names: a directory, taken from `dir` when it's
/// relative, or else a branch that a worktree of `dir`'s repository has
/// checked out.
pub fn find_worktree(dir: &Path, target: &str) -> Result<PathBuf> {
    let as_dir = dir.join(target);
    if as_dir.is_dir() {
        return Ok(std::fs::canonicalize(as_dir)?);
    }
    let list = git(dir, &["worktree", "list", "--porcelain"])?;
    let found = parse_worktree_list(&list)
        .into_iter()
        .find(|(_, branch)| branch.as_deref() == Some(target));
    match found {
        Some((path, _)) => Ok(std::fs::canonicalize(&path).unwrap_or(path)),
        None => bail!("no worktree at {target} or on a branch called {target}"),
    }
}

/// Removes the worktree at `path` the way `git worktree remove` does, which
/// refuses the main worktree, and one with changes not yet committed.
pub fn remove_worktree(path: &Path) -> Result<()> {
    let checkout = Checkout::find(path)
        .with_context(|| format!("{} isn't in a git repository", path.display()))?;
    git(
        &checkout.project_path,
        &["worktree", "remove", &path.to_string_lossy()],
    )?;
    Ok(())
}

/// What `git diff` is asked for every patch crystal reads: no colors, no
/// diff program of the user's, renames found, and the usual `a/` and `b/`
/// in front of paths whatever the user's config says, so that the patch
/// always reads the same way. Paths come as they are, not quoted.
const PATCH: &[&str] = &[
    "-c",
    "core.quotePath=false",
    "diff",
    "--no-color",
    "--no-ext-diff",
    "--find-renames",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

/// The changes in the worktree at `dir` that aren't committed yet, staged
/// or not, as a patch against its last commit. Files git doesn't know about
/// yet aren't in it: see [`untracked_files`].
pub fn uncommitted_patch(dir: &Path) -> Result<String> {
    git(dir, &[PATCH, &["HEAD"]].concat())
}

/// The changes committed on the branch at `dir` since `commit`, as a patch.
pub fn patch_since(dir: &Path, commit: &str) -> Result<String> {
    git(dir, &[PATCH, &[commit, "HEAD"]].concat())
}

/// The files in the worktree at `dir` that git doesn't track yet and that
/// aren't ignored, by their paths from its top.
pub fn untracked_files(dir: &Path) -> Result<Vec<String>> {
    let args = [
        "-c",
        "core.quotePath=false",
        "ls-files",
        "--others",
        "--exclude-standard",
    ];
    Ok(lines(&git(dir, &args)?))
}

/// Every file in the worktree at `dir` that git tracks, or would: the ones
/// it knows and the new ones that aren't ignored.
pub fn files(dir: &Path) -> Result<Vec<String>> {
    let args = [
        "-c",
        "core.quotePath=false",
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
    ];
    let mut files = lines(&git(dir, &args)?);
    // A file deleted but not committed yet is still known to git.
    files.retain(|file| dir.join(file).exists());
    Ok(files)
}

/// Where the branch at `dir` started: the name of the repository's default
/// branch, and the commit the two have in common.
pub fn branch_start(dir: &Path) -> Result<(String, String)> {
    let default = default_branch(dir)?;
    let commit = git(dir, &["merge-base", &default, "HEAD"])?;
    Ok((default, commit.trim().to_string()))
}

/// The branch the repository's work goes back into: `origin`'s, when the
/// clone knows which that is, or else `main` or `master`.
fn default_branch(dir: &Path) -> Result<String> {
    let origin_head = [
        "symbolic-ref",
        "--quiet",
        "--short",
        "refs/remotes/origin/HEAD",
    ];
    if let Ok(origin) = git(dir, &origin_head) {
        return Ok(origin.trim().to_string());
    }
    for name in ["main", "master"] {
        if branch_exists(dir, name) {
            return Ok(name.to_string());
        }
    }
    bail!("there's no branch to compare with: no origin/HEAD, main or master")
}

/// The lines git printed, without blank ones.
fn lines(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect()
}

/// Where a new worktree for `branch` goes: beside the project, in a
/// directory named `<project>.worktrees` with one directory per branch.
/// A `/` in the branch becomes a `-`, so each worktree is one level down.
pub fn worktree_dir(project_path: &Path, branch: &str) -> PathBuf {
    let project = project_path.file_name().unwrap_or_default();
    let parent = project_path.parent().unwrap_or(project_path);
    parent
        .join(format!("{}.worktrees", project.to_string_lossy()))
        .join(branch.replace('/', "-"))
}

/// The project a repository's shared git dir belongs to: the main worktree,
/// which holds the `.git` dir, named after its directory. A bare repository
/// has no main worktree, so it stands for itself.
fn project_of(common_dir: &Path) -> (String, PathBuf) {
    let project_path = match common_dir.parent() {
        Some(parent) if common_dir.file_name() == Some(OsStr::new(".git")) => parent,
        _ => common_dir,
    };
    let name = match project_path.file_name() {
        Some(name) => name.to_string_lossy().trim_end_matches(".git").to_string(),
        None => project_path.display().to_string(),
    };
    (name, project_path.to_path_buf())
}

/// The branch a HEAD file names: `ref: refs/heads/main` is on `main`. A
/// detached HEAD holds a commit id instead, and is on no branch.
fn branch_from_head(head: &str) -> Option<String> {
    let branch = head.trim().strip_prefix("ref: refs/heads/")?;
    Some(branch.to_string())
}

/// The worktrees in `git worktree list --porcelain`, each with the branch
/// it has checked out. Each worktree is a block of lines like
/// `worktree /path` and `branch refs/heads/main`.
fn parse_worktree_list(list: &str) -> Vec<(PathBuf, Option<String>)> {
    let mut worktrees: Vec<(PathBuf, Option<String>)> = Vec::new();
    for line in list.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            worktrees.push((PathBuf::from(path), None));
        } else if let (Some(branch), Some(last)) = (
            line.strip_prefix("branch refs/heads/"),
            worktrees.last_mut(),
        ) {
            last.1 = Some(branch.to_string());
        }
    }
    worktrees
}

fn branch_exists(dir: &Path, branch: &str) -> bool {
    let reference = format!("refs/heads/{branch}");
    git(dir, &["show-ref", "--verify", "--quiet", &reference]).is_ok()
}

/// `path`, as git printed it for `dir`, made absolute and with symbolic
/// links resolved, so that the same directory always has the same path.
fn absolute(dir: &Path, path: &str) -> Option<PathBuf> {
    std::fs::canonicalize(dir.join(path)).ok()
}

/// Runs git in `dir` and gives back what it printed. When git fails, its
/// own words are the error.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("couldn't run git")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_names_the_branch_or_is_detached() {
        assert_eq!(
            branch_from_head("ref: refs/heads/feat/login\n"),
            Some("feat/login".into())
        );
        assert_eq!(
            branch_from_head("9fceb02d0ae598e95dc970b74767f19372d61af8\n"),
            None
        );
        assert_eq!(branch_from_head(""), None);
    }

    #[test]
    fn a_project_is_named_after_its_main_worktree() {
        let (name, path) = project_of(Path::new("/code/app/.git"));
        assert_eq!(name, "app");
        assert_eq!(path, Path::new("/code/app"));
    }

    #[test]
    fn a_bare_repository_is_its_own_project() {
        let (name, path) = project_of(Path::new("/srv/app.git"));
        assert_eq!(name, "app");
        assert_eq!(path, Path::new("/srv/app.git"));
    }

    #[test]
    fn worktrees_go_beside_the_project_one_per_branch() {
        let project = Path::new("/code/app");
        assert_eq!(
            worktree_dir(project, "fix-typo"),
            Path::new("/code/app.worktrees/fix-typo")
        );
        assert_eq!(
            worktree_dir(project, "feat/login"),
            Path::new("/code/app.worktrees/feat-login")
        );
    }

    #[test]
    fn the_worktree_list_gives_each_worktree_its_branch() {
        let list = "worktree /code/app\n\
                    HEAD 9fceb02d0ae598e95dc970b74767f19372d61af8\n\
                    branch refs/heads/main\n\
                    \n\
                    worktree /code/app.worktrees/feat-login\n\
                    HEAD 0e7b51f1e2a4cdb0b27c2ac83c20f0c6ce3a1c51\n\
                    branch refs/heads/feat/login\n\
                    \n\
                    worktree /code/app.worktrees/spike\n\
                    HEAD 0e7b51f1e2a4cdb0b27c2ac83c20f0c6ce3a1c51\n\
                    detached\n";
        assert_eq!(
            parse_worktree_list(list),
            [
                (PathBuf::from("/code/app"), Some("main".into())),
                (
                    PathBuf::from("/code/app.worktrees/feat-login"),
                    Some("feat/login".into())
                ),
                (PathBuf::from("/code/app.worktrees/spike"), None),
            ]
        );
    }
}
