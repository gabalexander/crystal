//! What crystal needs from git: which project and worktree a directory is
//! in, which branch that worktree is on, making and removing worktrees,
//! what changed in one, for the diff and the file finder, and searching
//! its files. [`branches`] lists a worktree's branches and moves it onto
//! another. It runs the `git` command rather than using a library, so it
//! behaves exactly like the git the user runs.

pub mod branches;

use crate::protocol::Worktree;
use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

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

/// Makes a worktree in the repository `dir` is in on a new branch: `branch`,
/// or else `branch-2`, `branch-3`… whichever is neither a branch yet nor
/// has a worktree's directory in the way. Returns its directory, and the
/// branch it's on.
pub fn add_new_worktree(dir: &Path, branch: &str) -> Result<(PathBuf, String)> {
    let checkout = Checkout::find(dir)
        .with_context(|| format!("{} isn't in a git repository", dir.display()))?;
    let free = (1..)
        .map(|n| match n {
            1 => branch.to_string(),
            n => format!("{branch}-{n}"),
        })
        .find(|name| {
            !branch_exists(dir, name) && !worktree_dir(&checkout.project_path, name).exists()
        })
        .expect("some number is free");
    let path = add_worktree(dir, &free)?;
    Ok((path, free))
}

/// The worktree of the repository at `project` that has `branch` checked
/// out, if one has: git checks a branch out in one worktree at a time.
pub fn worktree_on(project: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let list = git(project, &["worktree", "list", "--porcelain"])?;
    let found = parse_worktree_list(&list)
        .into_iter()
        .find(|listed| !listed.prunable && listed.branch.as_deref() == Some(branch));
    Ok(found.map(|listed| std::fs::canonicalize(&listed.path).unwrap_or(listed.path)))
}

/// Makes a worktree in the repository at `project` on `branch`, with what
/// `fetch` names on `origin`: a branch, or a ref like `refs/pull/57/head`.
/// It's fetched first. A new branch starts there and follows `fetch` on
/// `origin`, so that `git pull` brings what's pushed to it later; a branch
/// that's there already is brought up to it, unless it has commits of its
/// own. Returns the worktree's directory.
pub fn add_fetched_worktree(project: &Path, branch: &str, fetch: &str) -> Result<PathBuf> {
    // Both come from the forge, and go to git as arguments.
    for name in [branch, fetch] {
        if name.is_empty() || name.starts_with('-') || name.contains(char::is_whitespace) {
            bail!("{name:?} isn't a branch git can check out");
        }
    }
    let checkout = Checkout::find(project)
        .with_context(|| format!("{} isn't in a git repository", project.display()))?;
    git(project, &["fetch", "--quiet", "origin", fetch])
        .with_context(|| format!("couldn't fetch {fetch} from origin"))?;
    // By its commit: another fetch in the repository could move FETCH_HEAD.
    let tip = git(project, &["rev-parse", "--verify", "FETCH_HEAD^{commit}"])?;
    let tip = tip.trim();
    let target = worktree_dir(&checkout.project_path, branch);
    let target_arg = target.to_string_lossy();
    if branch_exists(project, branch) {
        git(project, &["worktree", "add", &target_arg, branch])?;
        // Not fast-forward, it has work of its own, which is left as it is.
        let _ = git(&target, &["merge", "--ff-only", "--quiet", tip]);
    } else {
        git(
            project,
            &["worktree", "add", "-b", branch, &target_arg, tip],
        )?;
        let merge = if fetch.starts_with("refs/") {
            fetch.to_string()
        } else {
            format!("refs/heads/{fetch}")
        };
        git(
            project,
            &["config", &format!("branch.{branch}.remote"), "origin"],
        )?;
        git(
            project,
            &["config", &format!("branch.{branch}.merge"), &merge],
        )?;
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
        .find(|listed| listed.branch.as_deref() == Some(target));
    match found {
        Some(Listed { path, .. }) => Ok(std::fs::canonicalize(&path).unwrap_or(path)),
        None => bail!("no worktree at {target} or on a branch called {target}"),
    }
}

/// The worktrees linked to the repository whose main worktree is
/// `project_path`: every one git lists but the main worktree, which it
/// lists first. One whose directory has gone is left out: git only keeps
/// it until it's pruned.
pub fn linked_worktrees(project_path: &Path) -> Result<Vec<Worktree>> {
    let checkout = Checkout::find(project_path)
        .with_context(|| format!("{} isn't in a git repository", project_path.display()))?;
    let list = git(project_path, &["worktree", "list", "--porcelain"])?;
    let linked = parse_worktree_list(&list)
        .into_iter()
        .skip(1)
        .filter(|listed| !listed.prunable)
        .filter_map(|listed| {
            // Resolved the way a session's worktree is, so that the two
            // can be compared; a directory that has gone can't be.
            let path = std::fs::canonicalize(&listed.path).ok()?;
            Some(Worktree {
                project: checkout.project.clone(),
                project_path: checkout.project_path.clone(),
                path,
                main: false,
                branch: listed.branch,
            })
        })
        .collect();
    Ok(linked)
}

/// Removes the worktree at `path` the way `git worktree remove` does, which
/// refuses the main worktree, and, unless `force`, one with changes not yet
/// committed.
pub fn remove_worktree(path: &Path, force: bool) -> Result<()> {
    let checkout = Checkout::find(path)
        .with_context(|| format!("{} isn't in a git repository", path.display()))?;
    let path = path.to_string_lossy();
    let mut args = vec!["worktree", "remove", &path];
    if force {
        args.push("--force");
    }
    git(&checkout.project_path, &args)?;
    Ok(())
}

/// Whether the worktree at `dir` has changes that `git worktree remove`
/// would only remove when forced: files changed or new and not committed,
/// found the way it finds them.
pub fn has_changes(dir: &Path) -> Result<bool> {
    let status = git(dir, &["status", "--porcelain", "--ignore-submodules=none"])?;
    Ok(!status.trim().is_empty())
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

/// The commit the worktree at `dir` is on, or an empty string before its
/// first commit.
pub fn head(dir: &Path) -> String {
    git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .map(|head| head.trim().to_string())
        .unwrap_or_default()
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

/// A line `git grep` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The file's path from the top of the worktree.
    pub path: String,
    /// The line's number, from 1.
    pub line: usize,
    /// The line, without the space it starts with, and cut short when it's
    /// long.
    pub text: String,
}

/// What a search found: its hits, in the order git found them, file by
/// file, and whether it stopped short of finding them all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub hits: Vec<Hit>,
    pub more: bool,
}

/// How much of a line a hit keeps: enough to read it by.
const HIT_CHARS: usize = 300;

/// The lines with `text` in them, in the files of the worktree at `dir`
/// that git tracks and the new ones it would, binary files left out: no
/// more than `most`. The case of letters counts only when `text` has a
/// capital in it. git is stopped as soon as `stale` says the search isn't
/// wanted any more, and then there's nothing: `None`.
pub fn grep(
    dir: &Path,
    text: &str,
    most: usize,
    stale: &dyn Fn() -> bool,
) -> Result<Option<Found>> {
    let mut args = vec![
        "grep",
        "-z",
        "--line-number",
        "-I",
        "--untracked",
        "--no-color",
        "--fixed-strings",
    ];
    if !text.chars().any(char::is_uppercase) {
        args.push("--ignore-case");
    }
    args.extend(["-e", text]);
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.quotePath=false"])
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("couldn't run git")?;
    let output = child.stdout.take().context("git grep has no output")?;
    let mut hits = Vec::new();
    // Each hit is `<path>\0<line>\0<text>\n`: a NUL can't be in a path.
    for record in BufReader::new(output).split(b'\n') {
        if stale() {
            stop(&mut child);
            return Ok(None);
        }
        if hits.len() == most {
            stop(&mut child);
            return Ok(Some(Found { hits, more: true }));
        }
        hits.extend(parse_hit(&record?));
    }
    let mut complaint = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut complaint);
    }
    // git grep says it found nothing by ending with 1.
    match child.wait()?.code() {
        Some(0 | 1) => Ok(Some(Found { hits, more: false })),
        _ => bail!("{}", complaint.trim()),
    }
}

/// One hit, from what `git grep -z --line-number` printed for it.
fn parse_hit(record: &[u8]) -> Option<Hit> {
    let mut fields = record.splitn(3, |byte| *byte == 0);
    let path = String::from_utf8_lossy(fields.next()?).into_owned();
    let line = String::from_utf8_lossy(fields.next()?).parse().ok()?;
    let text = String::from_utf8_lossy(fields.next()?);
    let text = text.trim();
    Some(Hit {
        path,
        line,
        text: text.chars().take(HIT_CHARS).collect(),
    })
}

/// Stops a git that's no longer wanted.
fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
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

/// One worktree as `git worktree list --porcelain` lists it.
#[derive(Debug, PartialEq, Eq)]
struct Listed {
    path: PathBuf,
    /// The branch it has checked out, or `None` when HEAD is detached.
    branch: Option<String>,
    /// Whether git would prune it: its directory has gone.
    prunable: bool,
}

/// The worktrees in `git worktree list --porcelain`, in its order. Each
/// worktree is a block of lines like `worktree /path`, `branch
/// refs/heads/main`, and `prunable …` once its directory has gone.
fn parse_worktree_list(list: &str) -> Vec<Listed> {
    let mut worktrees: Vec<Listed> = Vec::new();
    for line in list.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            worktrees.push(Listed {
                path: PathBuf::from(path),
                branch: None,
                prunable: false,
            });
            continue;
        }
        let Some(last) = worktrees.last_mut() else {
            continue;
        };
        if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            last.branch = Some(branch.to_string());
        } else if line == "prunable" || line.starts_with("prunable ") {
            last.prunable = true;
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
/// own words are the error. It never asks for a password on the terminal,
/// which may be the TUI's: a fetch that needs one fails instead.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
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
    fn a_hit_is_a_path_a_line_number_and_the_line() {
        assert_eq!(
            parse_hit(b"src/a b.rs\x0012\x00    let x = grep_me();"),
            Some(Hit {
                path: "src/a b.rs".into(),
                line: 12,
                text: "let x = grep_me();".into(),
            })
        );
        assert_eq!(parse_hit(b"Binary file matches"), None);
    }

    /// A repository with one commit in a new directory, made with git's
    /// config left out.
    pub(crate) fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.name", "crystal"]);
        run(&["config", "user.email", "crystal@example.com"]);
        // crystal's own git reads the machine's config: none of its
        // signing or hooks here.
        run(&["config", "commit.gpgsign", "false"]);
        run(&["config", "core.hooksPath", "/dev/null"]);
        std::fs::write(dir.path().join("tracked.txt"), "a needle in a haystack\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "first"]);
        dir
    }

    #[test]
    fn grep_finds_tracked_and_new_files_minding_case_only_for_capitals() {
        let dir = repo();
        std::fs::write(dir.path().join("new.txt"), "another Needle\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "needle\n").unwrap();
        let found = |text| grep(dir.path(), text, 10, &|| false).unwrap().unwrap();

        let mut paths: Vec<String> = found("needle")
            .hits
            .into_iter()
            .map(|hit| hit.path)
            .collect();
        paths.sort();
        assert_eq!(paths, ["new.txt", "tracked.txt"]);
        assert_eq!(found("Needle").hits.len(), 1);
        assert_eq!(
            found("nothing like it"),
            Found {
                hits: vec![],
                more: false
            }
        );
    }

    #[test]
    fn grep_stops_at_its_most_and_when_its_stale() {
        let dir = repo();
        std::fs::write(dir.path().join("many.txt"), "needle\n".repeat(20)).unwrap();
        let found = grep(dir.path(), "needle", 5, &|| false).unwrap().unwrap();
        assert_eq!(found.hits.len(), 5);
        assert!(found.more);
        assert_eq!(grep(dir.path(), "needle", 5, &|| true).unwrap(), None);
    }

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
    fn the_worktree_list_gives_each_worktree_its_branch_and_says_which_have_gone() {
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
                    detached\n\
                    \n\
                    worktree /code/app.worktrees/gone\n\
                    HEAD 0e7b51f1e2a4cdb0b27c2ac83c20f0c6ce3a1c51\n\
                    branch refs/heads/gone\n\
                    prunable gitdir file points to non-existent location\n";
        let listed = |path: &str, branch: Option<&str>, prunable| Listed {
            path: PathBuf::from(path),
            branch: branch.map(String::from),
            prunable,
        };
        assert_eq!(
            parse_worktree_list(list),
            [
                listed("/code/app", Some("main"), false),
                listed("/code/app.worktrees/feat-login", Some("feat/login"), false),
                listed("/code/app.worktrees/spike", None, false),
                listed("/code/app.worktrees/gone", Some("gone"), true),
            ]
        );
    }
}
