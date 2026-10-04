//! A worktree's branches, and moving it onto another, for the branch
//! switcher: the local branches, and the remote ones no local branch has
//! the name of, each with its last commit; fetching the remotes, so theirs
//! are as they are now; what isn't committed in the worktree; and the
//! switch, which asks what's to become of that first:
//! stashed, brought along, committed, or thrown away.
//!
//! Agents run git in the same repository while a switch runs, so nothing
//! here trusts a place or a moment: a stash entry is found by its commit
//! and message, not as the top of the stack, and the changes the user was
//! shown are looked at again before any are thrown away.

use super::git;
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// How long fetching the remotes may take: a fetch writes packs, and one
/// cut short starts over the next time, but a remote that has stalled
/// mustn't hold it for good.
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a fetch that's stopped gets to tidy its lock and its half
/// written packs away before it's killed.
const FETCH_GRACE: Duration = Duration::from_secs(2);

/// A branch the worktree can be switched to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    /// `fix-login`, or a remote's, `origin/fix-login`.
    pub name: String,
    pub remote: bool,
    /// Whether the worktree is on it.
    pub current: bool,
    /// The other worktree it's checked out in, if one has it: git keeps a
    /// branch in one worktree at a time, so it can't be switched to.
    pub elsewhere: Option<PathBuf>,
    /// When its last commit was made, in seconds since the Unix epoch.
    pub committed: u64,
    /// Its last commit's subject.
    pub subject: String,
}

impl Branch {
    /// A branch the switch is to make, from the commit the worktree is on.
    pub fn new(name: &str) -> Branch {
        Branch {
            name: name.to_string(),
            remote: false,
            current: false,
            elsewhere: None,
            committed: 0,
            subject: String::new(),
        }
    }

    /// The local branch a switch to it lands on: for a remote branch, its
    /// name without the remote's, the branch `git switch` makes to follow
    /// it.
    pub fn local_name(&self) -> &str {
        match self.name.split_once('/') {
            Some((_, name)) if self.remote => name,
            _ => &self.name,
        }
    }
}

/// What `git for-each-ref` is asked for each branch: its full name, what it
/// points at when it's only a name for another, a `*` when the worktree is
/// on it, the worktree that has it checked out, and its last commit's time
/// and subject.
const REF_FORMAT: &str = "--format=%(refname)%00%(symref)%00%(HEAD)%00%(worktreepath)%00%(committerdate:unix)%00%(contents:subject)";

/// The branches of the repository the worktree at `dir` is in: the one
/// it's on first, then the other local branches, the latest commit first,
/// then the same for the remote branches no local branch has the name of.
pub fn list(dir: &Path) -> Result<Vec<Branch>> {
    let args = [
        "for-each-ref",
        "--sort=-committerdate",
        REF_FORMAT,
        "refs/heads",
        "refs/remotes",
    ];
    Ok(parse_refs(&git(dir, &args)?))
}

/// Fetches every remote of the repository the worktree at `dir` is in,
/// `git fetch --all`, so their branches are as they are now. It's a session
/// of its own, with nothing on its standard input, so an ssh asking for a
/// passphrase or about a host can't take the terminal, which may be the
/// TUI's, and fails instead; and it's stopped, with the ssh it started,
/// once it has taken [`FETCH_TIMEOUT`]. An error is the line of what git
/// said that says what failed.
pub fn fetch(dir: &Path) -> Result<(), String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(["fetch", "--all", "--quiet"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // SAFETY: setsid is safe to call between fork and exec, and has no
    // preconditions.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|_| "couldn't run git".to_string())?;
    let said = super::read_all(child.stderr.take());
    let deadline = Instant::now() + FETCH_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err(first_line(&said.join().unwrap_or_default())),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            _ => {
                stop_all(&mut child);
                return Err("git fetch took too long".to_string());
            }
        }
    }
}

/// Stops `child` and whatever it started, all in its process group: asked
/// first, so git can take its lock away, then, after [`FETCH_GRACE`],
/// killed.
fn stop_all(child: &mut Child) {
    let Ok(group) = i32::try_from(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    // SAFETY: plain signals to a process group this process started and
    // hasn't waited for the leader of, so its number can't be another's.
    unsafe { libc::kill(-group, libc::SIGTERM) };
    let grace = Instant::now() + FETCH_GRACE;
    while Instant::now() < grace {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: as above.
    unsafe { libc::kill(-group, libc::SIGKILL) };
    let _ = child.wait();
}

/// The branches in what `git for-each-ref` printed in [`REF_FORMAT`], in
/// the order [`list`] gives them. A remote's `HEAD` only names one of its
/// branches, so it's left out.
fn parse_refs(refs: &str) -> Vec<Branch> {
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for record in refs.lines() {
        let fields: Vec<&str> = record.splitn(6, '\0').collect();
        let &[name, symref, head, worktree, committed, subject] = fields.as_slice() else {
            continue;
        };
        let branch = |name: &str, remote: bool| {
            let current = head == "*";
            Branch {
                name: name.to_string(),
                remote,
                current,
                elsewhere: (!current && !worktree.is_empty()).then(|| PathBuf::from(worktree)),
                committed: committed.trim().parse().unwrap_or(0),
                subject: subject.trim().to_string(),
            }
        };
        if let Some(name) = name.strip_prefix("refs/heads/") {
            local.push(branch(name, false));
        } else if let Some(name) = name.strip_prefix("refs/remotes/")
            && symref.is_empty()
            && name.contains('/')
        {
            remote.push(branch(name, true));
        }
    }
    let local_names: HashSet<String> = local.iter().map(|branch| branch.name.clone()).collect();
    remote.retain(|branch: &Branch| !local_names.contains(branch.local_name()));
    // Stable, so the latest commits still come first among the rest.
    local.sort_by_key(|branch| !branch.current);
    local.extend(remote);
    local
}

/// A change in the worktree that isn't committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// git's two letters for it, staged and not: ` M`, `A `, `??`.
    pub status: String,
    /// From the top of the worktree; a directory git doesn't know ends in
    /// `/`.
    pub path: String,
}

impl Change {
    /// Whether git has left the file with conflicts in it.
    fn conflicted(&self) -> bool {
        self.status.contains('U') || self.status == "AA" || self.status == "DD"
    }

    /// Whether it's a directory git doesn't know that's a repository of
    /// its own, like an agent's worktree inside the project: no switch
    /// moves it, and a commit mustn't take it in.
    fn is_repository(&self, dir: &Path) -> bool {
        self.status == "??"
            && self.path.ends_with('/')
            && dir.join(&self.path).join(".git").exists()
    }
}

/// What isn't committed in the worktree at `dir`, staged or not, new files
/// included, as `git status` has it: but repositories of their own inside
/// it, which aren't the worktree's to change.
pub fn changes(dir: &Path) -> Result<Vec<Change>> {
    let mut changes = all_changes(dir)?;
    changes.retain(|change| !change.is_repository(dir));
    Ok(changes)
}

fn all_changes(dir: &Path) -> Result<Vec<Change>> {
    let status = git(
        dir,
        &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
    )?;
    Ok(parse_status(&status))
}

/// The changes in `git status --porcelain=v1 -z`: `XY path`, each ended by
/// a NUL, and a renamed or copied file's old path after it, which isn't a
/// change of its own.
fn parse_status(status: &str) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut records = status.split('\0').filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        let Some((letters, path)) = record.split_at_checked(2) else {
            continue;
        };
        if letters.starts_with(['R', 'C']) {
            records.next();
        }
        changes.push(Change {
            status: letters.to_string(),
            path: path.trim_start().to_string(),
        });
    }
    changes
}

/// The changes as the user was shown them, each with its file's size and
/// when it was last written, so another edit to a file already listed
/// counts as a change too.
pub fn fingerprint(dir: &Path, changes: &[Change]) -> Vec<String> {
    changes
        .iter()
        .map(|change| {
            let stamp = std::fs::symlink_metadata(dir.join(&change.path)).map(|meta| {
                let written = meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |since| since.as_nanos());
                format!("{} {written}", meta.len())
            });
            let stamp = stamp.unwrap_or_else(|_| "gone".to_string());
            format!("{} {} {stamp}", change.status, change.path)
        })
        .collect()
}

/// The worktree's changes as they are now, fingerprinted; none when git
/// can't say.
fn fingerprint_now(dir: &Path) -> Vec<String> {
    changes(dir)
        .map(|changes| fingerprint(dir, &changes))
        .unwrap_or_default()
}

/// What becomes of the worktree's changes in a switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Carry {
    /// There should be none: if there are, the switch stops to ask.
    Ask,
    /// Stash them, new files too, then switch. If git won't switch, they
    /// come back out of the stash.
    Stash,
    /// Switch with them: git does, unless they're in files the branch has
    /// changed.
    Bring,
    /// Commit them all, new files too, with this message, then switch.
    Commit(String),
    /// Throw away the changes to the files git knows, then switch. New files
    /// stay, and so does anything only staged that git never had: it's
    /// unstaged first. The changes as they were shown ([`fingerprint`]): if
    /// they're not the same now, nothing is thrown away, and the user is
    /// asked again.
    Discard(Vec<String>),
    /// Make the branch, from the commit the worktree is on, and switch to
    /// it, the changes coming along.
    Create,
}

/// What a switch came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The worktree is on `branch`; `note` says what became of its changes,
    /// or what git said once it had switched.
    Switched {
        branch: String,
        note: Option<String>,
    },
    /// There are changes, and the switch wants to know what's to become of
    /// them: nothing was touched. `fingerprint` is theirs.
    Dirty {
        changes: Vec<Change>,
        fingerprint: Vec<String>,
    },
    /// Nothing moved: the worktree is as it was.
    Failed(String),
    /// The switch failed, but the worktree isn't as it was: a commit went
    /// in, or git left something behind, so what the user was shown of it
    /// no longer holds.
    Stopped(String),
}

/// Moves the worktree at `dir` onto `target`, its changes going as `carry`
/// says. Nothing switches in the middle of a merge or a rebase, or with
/// conflicts, and a switch git refuses leaves the worktree as it was, as
/// far as it can: the stash put back, what a discard unstaged staged again.
pub fn switch(dir: &Path, target: &Branch, carry: &Carry) -> Outcome {
    match try_switch(dir, target, carry) {
        Ok(outcome) => outcome,
        Err(err) => Outcome::Failed(complaint(&err)),
    }
}

/// [`switch`], where an error is something that stopped it before anything
/// changed.
fn try_switch(dir: &Path, target: &Branch, carry: &Carry) -> Result<Outcome> {
    let to = target.local_name();
    if let Some(what) = super::in_progress(dir) {
        bail!(
            "the worktree is in the middle of {}: finish it or abort it first",
            what.what()
        );
    }
    let all = all_changes(dir)?;
    if let Some(change) = all.iter().find(|change| change.conflicted()) {
        bail!("{} has conflicts: resolve them first", change.path);
    }
    let (repositories, changes): (Vec<Change>, Vec<Change>) = all
        .into_iter()
        .partition(|change| change.is_repository(dir));
    let from = head_branch(dir);

    let mut note = None;
    let mut stash = None;
    let mut index = None;
    let mut args = vec!["switch", "--quiet"];
    match carry {
        Carry::Ask if !changes.is_empty() => {
            let fingerprint = fingerprint(dir, &changes);
            return Ok(Outcome::Dirty {
                changes,
                fingerprint,
            });
        }
        Carry::Ask | Carry::Bring => {}
        Carry::Create => check_new_name(dir, &target.name)?,
        Carry::Stash => {
            let from = from.as_deref().unwrap_or("a detached HEAD");
            let message = format!("crystal: {from} before switching to {to}");
            stash = stash_push(dir, &message).context("couldn't stash the changes")?;
            if stash.is_some() {
                note = Some(format!("changes stashed as \"{message}\""));
            }
        }
        Carry::Commit(message) => {
            let Some(from) = &from else {
                bail!("HEAD is detached, so a commit here would be on no branch: stash instead");
            };
            commit_all(dir, message, &repositories)?;
            let commit = git(dir, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
            note = Some(format!("committed {} on {from}", commit.trim()));
        }
        Carry::Discard(shown) => {
            let now = fingerprint(dir, &changes);
            if now != *shown {
                return Ok(Outcome::Dirty {
                    changes,
                    fingerprint: now,
                });
            }
            index = SavedIndex::take(dir);
            git(dir, &["reset", "--quiet"]).context("couldn't unstage the changes")?;
            args.push("--discard-changes");
        }
    }
    if target.remote {
        args.push("--track");
    }
    if *carry == Carry::Create {
        args.push("--create");
    }
    args.push(&target.name);

    // What the worktree holds as the switch starts, to tell a switch git
    // refused from one it gave up on halfway.
    let before = fingerprint_now(dir);
    let mut error = match git(dir, &args) {
        Ok(_) => {
            return Ok(Outcome::Switched {
                branch: to.to_string(),
                note,
            });
        }
        Err(err) => complaint(&err),
    };
    // git fails when a post-checkout hook fails, after HEAD has moved: that
    // switch happened, and putting the changes back would put them on the
    // wrong branch.
    if head_branch(dir).as_deref() == Some(to) && from.as_deref() != Some(to) {
        let complained = format!("git complained after switching: {error}");
        let note = match note {
            Some(note) => format!("{note} · {complained}"),
            None => complained,
        };
        return Ok(Outcome::Switched {
            branch: to.to_string(),
            note: Some(note),
        });
    }
    if error.contains("would be overwritten") {
        error = format!(
            "changes in the worktree are in files {to} changes: stash or commit them instead"
        );
    }
    let left_behind = fingerprint_now(dir) != before;
    if left_behind {
        error =
            format!("{error}; git stopped halfway and left files from {to} behind: see git status");
    }
    if let Some(index) = index {
        index.put_back();
    }
    if let Some(entry) = &stash
        && !stash_restore(dir, entry)
    {
        return Ok(Outcome::Stopped(format!(
            "{error} (your changes are still in the stash)"
        )));
    }
    if let (Carry::Commit(_), Some(note)) = (carry, note) {
        return Ok(Outcome::Stopped(format!(
            "{note}, but the switch failed: {error}"
        )));
    }
    if left_behind {
        return Ok(Outcome::Stopped(error));
    }
    Ok(Outcome::Failed(error))
}

/// The branch the worktree at `dir` is on: `None` when HEAD is detached.
fn head_branch(dir: &Path) -> Option<String> {
    let branch = git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok()?;
    Some(branch.trim().to_string()).filter(|branch| !branch.is_empty())
}

/// Refuses a name for a new branch that git would read as an option, or
/// that a remote's branches start with, like `origin/fix`: it would be
/// taken for the remote's branch wherever git reads it.
fn check_new_name(dir: &Path, name: &str) -> Result<()> {
    if name.starts_with('-') {
        bail!("a branch's name can't start with -");
    }
    let remotes = git(dir, &["remote"])?;
    let taken = remotes
        .lines()
        .map(str::trim)
        .find(|remote| !remote.is_empty() && name.starts_with(&format!("{remote}/")));
    if let Some(remote) = taken {
        bail!("a branch called {name} would be taken for {remote}'s: choose another name");
    }
    Ok(())
}

/// The stash, newest first, each entry's commit and subject.
fn stash_entries(dir: &Path) -> Vec<(String, String)> {
    let list = git(dir, &["stash", "list", "--format=%H%x00%s"]).unwrap_or_default();
    list.lines()
        .filter_map(|line| line.split_once('\0'))
        .map(|(commit, subject)| (commit.to_string(), subject.to_string()))
        .collect()
}

/// Stashes the worktree's changes, new files too, as `message`, and returns
/// the entry's commit: found as the new entry with that message, not the
/// top of a stack other worktrees push onto too. `None` when there was
/// nothing to stash.
fn stash_push(dir: &Path, message: &str) -> Result<Option<String>> {
    let before: HashSet<String> = stash_entries(dir)
        .into_iter()
        .map(|(commit, _)| commit)
        .collect();
    let args = [
        "stash",
        "push",
        "--quiet",
        "--include-untracked",
        "-m",
        message,
    ];
    git(dir, &args)?;
    let entry = stash_entries(dir)
        .into_iter()
        .find(|(commit, subject)| !before.contains(commit) && subject.ends_with(message));
    Ok(entry.map(|(commit, _)| commit))
}

/// Puts the stash entry `commit` back into the worktree, staged as it was
/// if it can be, and drops it. Says whether it went back; when it didn't,
/// it's still in the stash. The entry is dropped by its place in the
/// stack, so the place is checked to still be the entry first.
fn stash_restore(dir: &Path, commit: &str) -> bool {
    let applied = git(dir, &["stash", "apply", "--quiet", "--index", commit]).is_ok()
        || git(dir, &["stash", "apply", "--quiet", commit]).is_ok();
    if !applied {
        return false;
    }
    let place = stash_entries(dir)
        .iter()
        .position(|(entry, _)| entry == commit);
    if let Some(place) = place {
        let entry = format!("stash@{{{place}}}");
        let still_there = git(dir, &["rev-parse", &entry]).is_ok_and(|at| at.trim() == commit);
        if still_there {
            let _ = git(dir, &["stash", "drop", "--quiet", &entry]);
        }
    }
    true
}

/// Commits every change in the worktree at `dir`, new files too, but the
/// `repositories` inside it. If git won't commit, a hook said no, say, what
/// was staged is staged again, as it was.
fn commit_all(dir: &Path, message: &str, repositories: &[Change]) -> Result<()> {
    let index = SavedIndex::take(dir);
    let left_out: Vec<String> = repositories
        .iter()
        .map(|repository| format!(":(exclude,literal){}", repository.path))
        .collect();
    let mut add = vec!["add", "--all", "--", "."];
    add.extend(left_out.iter().map(String::as_str));
    let committed = git(dir, &add)
        .context("couldn't add the changes")
        .and_then(|_| git(dir, &["commit", "--quiet", "-m", message]).context("couldn't commit"));
    if committed.is_err()
        && let Some(index) = index
    {
        index.put_back();
    }
    committed.map(drop)
}

/// The index file, as it was before a commit or a discard changed it, to
/// put back exactly if what follows fails: what was staged, and the files
/// only marked to be added, which git's own ways of saving it lose.
struct SavedIndex {
    path: PathBuf,
    bytes: Vec<u8>,
}

impl SavedIndex {
    /// The worktree's index as it is now, if it has one.
    fn take(dir: &Path) -> Option<SavedIndex> {
        let path = git(dir, &["rev-parse", "--git-path", "index"]).ok()?;
        let path = dir.join(path.trim());
        let bytes = std::fs::read(&path).ok()?;
        Some(SavedIndex { path, bytes })
    }

    /// Writes it back the way git writes an index, into `index.lock` and
    /// then over it, so never while another git has it locked.
    fn put_back(&self) {
        let mut lock = self.path.clone().into_os_string();
        lock.push(".lock");
        let lock = PathBuf::from(lock);
        // Another git has it locked: the index is that git's now.
        let Ok(mut file) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        else {
            return;
        };
        let written = file.write_all(&self.bytes);
        drop(file);
        if written.is_err() || std::fs::rename(&lock, &self.path).is_err() {
            let _ = std::fs::remove_file(&lock);
        }
    }
}

/// An error in a line: each thing that went wrong, and of git's own words,
/// the line that says what failed.
fn complaint(err: &anyhow::Error) -> String {
    let said: Vec<String> = err
        .chain()
        .map(|cause| first_line(&cause.to_string()))
        .collect();
    said.join(": ")
}

/// The line of what git printed that says what failed, without its
/// `error:` or `fatal:`, or else the first it printed.
fn first_line(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let line = lines
        .iter()
        .find(|line| line.starts_with("error: ") || line.starts_with("fatal: "))
        .or(lines.first())
        .copied()
        .unwrap_or("git failed");
    line.strip_prefix("error: ")
        .or_else(|| line.strip_prefix("fatal: "))
        .unwrap_or(line)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::repo;
    use std::process::Command;

    /// Runs git in `dir` for a test, with the machine's config left out.
    fn run(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// A repository on `main` with a branch `other`, where `tracked.txt`
    /// says something else.
    fn repo_with_other() -> tempfile::TempDir {
        let dir = repo();
        run(dir.path(), &["checkout", "-q", "-b", "other"]);
        std::fs::write(dir.path().join("tracked.txt"), "other's\n").unwrap();
        run(dir.path(), &["commit", "-q", "-am", "on other"]);
        run(dir.path(), &["checkout", "-q", "main"]);
        dir
    }

    fn branch(dir: &Path, name: &str) -> Branch {
        list(dir)
            .unwrap()
            .into_iter()
            .find(|branch| branch.name == name)
            .unwrap()
    }

    fn on(dir: &Path) -> String {
        head_branch(dir).unwrap()
    }

    fn read(dir: &Path, file: &str) -> String {
        std::fs::read_to_string(dir.join(file)).unwrap()
    }

    #[test]
    fn fetching_brings_a_remotes_new_branches_and_says_why_it_cant() {
        let origin = repo();
        let clone = tempfile::tempdir().unwrap();
        let from = origin.path().to_str().unwrap();
        run(clone.path(), &["clone", "-q", from, "."]);
        run(origin.path(), &["branch", "fresh"]);
        let has_fresh = || {
            list(clone.path())
                .unwrap()
                .iter()
                .any(|branch| branch.name == "origin/fresh")
        };
        assert!(!has_fresh());
        fetch(clone.path()).unwrap();
        assert!(has_fresh());

        run(
            clone.path(),
            &["remote", "set-url", "origin", "/no/such/repository"],
        );
        let why = fetch(clone.path()).unwrap_err();
        assert!(!why.is_empty() && !why.contains('\n'), "{why}");
    }

    #[test]
    fn the_current_branch_comes_first_and_a_remotes_twins_are_left_out() {
        let refs: String = [
            ["refs/heads/old", "", "", "", "100", "old work"],
            ["refs/heads/main", "", "*", "/code/app", "200", "the latest"],
            [
                "refs/heads/fix",
                "",
                "",
                "/code/app.worktrees/fix",
                "150",
                "a fix",
            ],
            [
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
                "",
                "",
                "200",
                "the latest",
            ],
            ["refs/remotes/origin/main", "", "", "", "200", "the latest"],
            [
                "refs/remotes/origin/theirs",
                "",
                "",
                "",
                "300",
                "their work",
            ],
        ]
        .iter()
        .map(|fields| fields.join("\0") + "\n")
        .collect();
        let branches = parse_refs(&refs);
        let names: Vec<&str> = branches.iter().map(|branch| branch.name.as_str()).collect();
        assert_eq!(names, ["main", "old", "fix", "origin/theirs"]);
        assert!(branches[0].current);
        assert_eq!(branches[0].elsewhere, None);
        assert_eq!(
            branches[2].elsewhere,
            Some(PathBuf::from("/code/app.worktrees/fix"))
        );
        assert_eq!(branches[3].local_name(), "theirs");
        assert_eq!(
            (branches[3].committed, branches[3].subject.as_str()),
            (300, "their work")
        );
    }

    #[test]
    fn a_renamed_files_old_path_isnt_a_change_of_its_own() {
        let status = "R  new.rs\0old.rs\0 M a.rs\0?? notes/\0";
        assert_eq!(
            parse_status(status),
            [
                Change {
                    status: "R ".into(),
                    path: "new.rs".into()
                },
                Change {
                    status: " M".into(),
                    path: "a.rs".into()
                },
                Change {
                    status: "??".into(),
                    path: "notes/".into()
                },
            ]
        );
    }

    #[test]
    fn a_clean_worktree_switches_and_a_changed_one_asks_first() {
        let dir = repo_with_other();
        let other = branch(dir.path(), "other");
        std::fs::write(dir.path().join("tracked.txt"), "mine\n").unwrap();
        let Outcome::Dirty {
            changes,
            fingerprint,
        } = switch(dir.path(), &other, &Carry::Ask)
        else {
            panic!("it should ask");
        };
        assert_eq!(changes.len(), 1);
        assert_eq!(fingerprint.len(), 1);
        assert_eq!(on(dir.path()), "main");

        run(dir.path(), &["checkout", "-q", "--", "."]);
        assert_eq!(
            switch(dir.path(), &other, &Carry::Ask),
            Outcome::Switched {
                branch: "other".into(),
                note: None
            }
        );
        assert_eq!(read(dir.path(), "tracked.txt"), "other's\n");
    }

    #[test]
    fn stashing_takes_the_changes_away_under_a_name_that_says_whence() {
        let dir = repo_with_other();
        std::fs::write(dir.path().join("tracked.txt"), "mine\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        let other = branch(dir.path(), "other");
        let Outcome::Switched { branch, note } = switch(dir.path(), &other, &Carry::Stash) else {
            panic!("it should switch");
        };
        assert_eq!(branch, "other");
        let message = "crystal: main before switching to other";
        assert_eq!(note, Some(format!("changes stashed as \"{message}\"")));
        assert!(!dir.path().join("new.txt").exists());
        assert!(run(dir.path(), &["stash", "list"]).contains(message));
    }

    #[test]
    fn changes_come_along_unless_they_collide() {
        let dir = repo_with_other();
        let other = branch(dir.path(), "other");
        // A file `other` changes: git won't, and nothing moves.
        std::fs::write(dir.path().join("tracked.txt"), "mine\n").unwrap();
        let Outcome::Failed(why) = switch(dir.path(), &other, &Carry::Bring) else {
            panic!("it should fail");
        };
        assert!(why.contains("stash or commit them instead"), "{why}");
        assert_eq!(read(dir.path(), "tracked.txt"), "mine\n");

        // A new file comes along.
        run(dir.path(), &["checkout", "-q", "--", "."]);
        std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        assert!(matches!(
            switch(dir.path(), &other, &Carry::Bring),
            Outcome::Switched { .. }
        ));
        assert_eq!(read(dir.path(), "new.txt"), "new\n");
    }

    #[test]
    fn committing_puts_every_change_on_the_branch_it_leaves() {
        let dir = repo_with_other();
        std::fs::write(dir.path().join("tracked.txt"), "mine\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        let other = branch(dir.path(), "other");
        let carry = Carry::Commit("my work".into());
        let Outcome::Switched { note, .. } = switch(dir.path(), &other, &carry) else {
            panic!("it should switch");
        };
        assert!(note.unwrap().ends_with("on main"));
        assert_eq!(
            run(dir.path(), &["log", "-1", "--format=%s", "main"]),
            "my work\n"
        );
        let committed = run(dir.path(), &["show", "--name-only", "--format=", "main"]);
        assert_eq!(committed, "new.txt\ntracked.txt\n");
    }

    #[test]
    fn a_commit_a_hook_refuses_leaves_what_was_staged_as_it_was() {
        let dir = repo_with_other();
        let hooks = dir.path().join(".git/test-hooks");
        std::fs::create_dir(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho no >&2\nexit 1\n").unwrap();
        run(
            dir.path(),
            &["config", "core.hooksPath", &hooks.to_string_lossy()],
        );
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&hook, permissions).unwrap();
        std::fs::write(dir.path().join("staged.txt"), "staged\n").unwrap();
        run(dir.path(), &["add", "staged.txt"]);
        std::fs::write(dir.path().join("tracked.txt"), "not staged\n").unwrap();

        let carry = Carry::Commit("my work".into());
        let Outcome::Failed(why) = switch(dir.path(), &branch(dir.path(), "other"), &carry) else {
            panic!("it should fail");
        };
        assert!(why.starts_with("couldn't commit"), "{why}");
        assert_eq!(
            run(dir.path(), &["diff", "--cached", "--name-only"]),
            "staged.txt\n"
        );
        assert_eq!(on(dir.path()), "main");
    }

    #[test]
    fn discarding_throws_away_only_what_was_shown_and_keeps_new_files() {
        let dir = repo_with_other();
        std::fs::write(dir.path().join("tracked.txt"), "mine\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        let other = branch(dir.path(), "other");
        let shown = fingerprint(dir.path(), &changes(dir.path()).unwrap());

        // Something changed since it was shown: it asks again.
        std::fs::write(dir.path().join("late.txt"), "late\n").unwrap();
        let carry = Carry::Discard(shown);
        let Outcome::Dirty { fingerprint, .. } = switch(dir.path(), &other, &carry) else {
            panic!("it should ask again");
        };
        assert_eq!(read(dir.path(), "tracked.txt"), "mine\n");

        let carry = Carry::Discard(fingerprint);
        assert!(matches!(
            switch(dir.path(), &other, &carry),
            Outcome::Switched { .. }
        ));
        assert_eq!(read(dir.path(), "tracked.txt"), "other's\n");
        assert_eq!(read(dir.path(), "new.txt"), "new\n");
    }

    #[test]
    fn a_new_branch_starts_where_the_worktree_is_but_not_with_a_remotes_name() {
        let dir = repo();
        std::fs::write(dir.path().join("tracked.txt"), "mine\n").unwrap();
        let outcome = switch(dir.path(), &Branch::new("fresh"), &Carry::Create);
        assert!(matches!(outcome, Outcome::Switched { .. }), "{outcome:?}");
        assert_eq!(on(dir.path()), "fresh");
        assert_eq!(read(dir.path(), "tracked.txt"), "mine\n");

        run(
            dir.path(),
            &["remote", "add", "origin", "https://example.com/app.git"],
        );
        let Outcome::Failed(why) = switch(dir.path(), &Branch::new("origin/x"), &Carry::Create)
        else {
            panic!("it should refuse");
        };
        assert!(why.contains("taken for origin's"), "{why}");
        assert!(matches!(
            switch(dir.path(), &Branch::new("-x"), &Carry::Create),
            Outcome::Failed(_)
        ));
    }

    #[test]
    fn a_remote_branch_becomes_a_local_one_following_it() {
        let origin = repo_with_other();
        let clone = tempfile::tempdir().unwrap();
        let from = origin.path().to_string_lossy().into_owned();
        let to = clone.path().join("app");
        run(clone.path(), &["clone", "-q", &from, &to.to_string_lossy()]);
        let names: Vec<String> = list(&to).unwrap().into_iter().map(|b| b.name).collect();
        assert_eq!(names, ["main", "origin/other"]);

        let theirs = branch(&to, "origin/other");
        assert!(matches!(
            switch(&to, &theirs, &Carry::Ask),
            Outcome::Switched { .. }
        ));
        assert_eq!(on(&to), "other");
        let upstream = run(&to, &["rev-parse", "--abbrev-ref", "other@{upstream}"]);
        assert_eq!(upstream, "origin/other\n");
    }

    #[test]
    fn nothing_switches_in_the_middle_of_a_merge() {
        let dir = repo_with_other();
        let git_dir = dir.path().join(".git");
        let head = run(dir.path(), &["rev-parse", "other"]);
        std::fs::write(git_dir.join("MERGE_HEAD"), head).unwrap();
        let Outcome::Failed(why) = switch(dir.path(), &branch(dir.path(), "other"), &Carry::Ask)
        else {
            panic!("it should refuse");
        };
        assert!(why.contains("in the middle of a merge"), "{why}");
    }

    #[test]
    fn gits_complaint_is_the_line_that_says_what_failed() {
        let said = "hint: something\nerror: Your local changes would be overwritten\nAborting\n";
        assert_eq!(first_line(said), "Your local changes would be overwritten");
        assert_eq!(first_line("plain\n"), "plain");
    }
}
