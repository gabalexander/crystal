//! What the wiki needs of git: a clean checkout of the commit it's written
//! from, apart from the user's worktrees, so that nothing uncommitted
//! shows; the commits that touched some files; what changed between two
//! commits; and where a line of a file at one commit is at another, so
//! that the links of what isn't written again still point where they did.

use super::crystal::{self, Forge};
use super::model::CodeLink;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Runs git in `dir` and gives back what it printed.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("couldn't run git")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The tip of a repository's default branch: its name, and the commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tip {
    pub branch: String,
    pub commit: String,
}

/// How long a fetch from `origin` may take before the wiki is written
/// from what was fetched last.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// The tip of the default branch of the repository whose main worktree is
/// `project`: `origin`'s, fetched first when `fetch` says to (as it was
/// last fetched when that fails), since that's what the repository has
/// published and what its forge's links can show; or with no `origin`, its
/// own `main` or `master`; or with neither, its `HEAD`.
pub fn default_tip(project: &Path, fetch: bool) -> Result<Tip> {
    let commit_of = |rev: &str| -> Result<String> {
        Ok(git(
            project,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{rev}^{{commit}}"),
            ],
        )?
        .trim()
        .to_string())
    };
    if git(project, &["remote", "get-url", "origin"]).is_ok() {
        let known = git(
            project,
            &[
                "symbolic-ref",
                "--quiet",
                "--short",
                "refs/remotes/origin/HEAD",
            ],
        )
        .ok()
        .and_then(|head| head.trim().strip_prefix("origin/").map(str::to_string));
        let branch = match known {
            Some(branch) => Some(branch),
            None if fetch => remote_head(project),
            None => None,
        };
        if let Some(branch) = branch {
            if fetch {
                // Offline, it's as it was last fetched.
                let _ = fetch_branch(project, &branch);
            }
            if let Ok(commit) = commit_of(&format!("origin/{branch}")) {
                return Ok(Tip { branch, commit });
            }
        }
    }
    for branch in ["main", "master"] {
        if let Ok(commit) = commit_of(&format!("refs/heads/{branch}")) {
            return Ok(Tip {
                branch: branch.to_string(),
                commit,
            });
        }
    }
    let branch = git(project, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map(|branch| branch.trim().to_string())
        .unwrap_or_else(|_| "HEAD".to_string());
    let commit = commit_of("HEAD").context("the repository has no commit yet")?;
    Ok(Tip { branch, commit })
}

/// The branch `origin`'s HEAD is on, asked of `origin`, and remembered.
fn remote_head(project: &Path) -> Option<String> {
    let said = git_within(project, &["ls-remote", "--symref", "origin", "HEAD"]).ok()?;
    let line = said.lines().next()?.strip_prefix("ref: refs/heads/")?;
    let (branch, _) = line.split_once('\t')?;
    fetch_branch(project, branch).ok()?;
    let _ = git(project, &["remote", "set-head", "origin", branch]);
    Some(branch.to_string())
}

/// Fetches `branch` from `origin`, bringing `origin/<branch>` up to date.
fn fetch_branch(project: &Path, branch: &str) -> Result<()> {
    let refspec = format!("refs/heads/{branch}");
    git_within(
        project,
        &["fetch", "--quiet", "--no-tags", "origin", &refspec],
    )?;
    Ok(())
}

/// Runs git in `dir` for a call that talks to a remote: stopped, and an
/// error, once it has taken [`FETCH_TIMEOUT`].
fn git_within(dir: &Path, args: &[&str]) -> Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("couldn't run git")?;
    let mut stdout = child.stdout.take().expect("its output is piped");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + FETCH_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("git {} took too long", args.join(" "));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !status.success() {
        bail!("git {} failed", args.first().unwrap_or(&""));
    }
    Ok(reader.join().unwrap_or_default())
}

/// A repository on its forge's web site, for links to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Web {
    /// `owner/name`, or for GitLab, `group/subgroup/name`.
    pub path: String,
    /// The repository's page: `https://github.com/owner/name`.
    pub url: String,
    /// Where a file is at a commit, `{commit}` and `{path}` to fill in.
    pub code_url: String,
}

/// The project at `project` on the web, by its remote, when that's on a
/// forge crystal knows.
pub fn web_of(project: &Path) -> Option<Web> {
    let forge = crystal::forge_of(project)?;
    let url = git(project, &["config", "--get", "remote.origin.url"])
        .ok()
        .or_else(|| {
            let remotes = git(project, &["remote"]).ok()?;
            let first = remotes.lines().next()?.to_string();
            git(
                project,
                &["config", "--get", &format!("remote.{first}.url")],
            )
            .ok()
        })?;
    web(url.trim(), forge)
}

/// [`web_of`] the remote at `url`, on `forge`.
fn web(url: &str, forge: Forge) -> Option<Web> {
    let (authority, path) = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?,
        // scp's way, `user@host:path`.
        None => url.split_once(':')?,
    };
    let host = authority.rsplit('@').next()?;
    let host = match host.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => host,
    };
    let host = match host.to_ascii_lowercase().as_str() {
        "ssh.github.com" => "github.com".to_string(),
        "altssh.gitlab.com" => "gitlab.com".to_string(),
        other => other.to_string(),
    };
    let path = path
        .trim_matches('/')
        .trim_end_matches(".git")
        .trim_matches('/');
    if host.is_empty() || path.split('/').count() < 2 || path.split('/').any(str::is_empty) {
        return None;
    }
    let url = format!("https://{host}/{path}");
    let code_url = match forge {
        Forge::GitHub => format!("{url}/blob/{{commit}}/{{path}}"),
        Forge::GitLab => format!("{url}/-/blob/{{commit}}/{{path}}"),
    };
    Some(Web {
        path: path.to_string(),
        url,
        code_url,
    })
}

/// Makes `checkout` a clean checkout of `commit` of the repository whose
/// main worktree is `project`: a clone that shares its objects, made once,
/// so it costs no more disk than its files; not a worktree of it, which
/// the user would see among theirs.
pub fn prepare(project: &Path, checkout: &Path, commit: &str) -> Result<()> {
    if !checkout.join(".git").exists() {
        if checkout.exists() {
            std::fs::remove_dir_all(checkout)
                .with_context(|| format!("couldn't clear {}", checkout.display()))?;
        }
        if let Some(parent) = checkout.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let out = Command::new("git")
            .args(["clone", "--quiet", "--shared", "--no-checkout"])
            .arg(project)
            .arg(checkout)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .context("couldn't run git")?;
        if !out.status.success() {
            bail!(
                "couldn't make a checkout of {}: {}",
                project.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    git(
        checkout,
        &["checkout", "--quiet", "--force", "--detach", commit],
    )
    .with_context(|| format!("couldn't check out {commit}"))?;
    git(checkout, &["clean", "-ffdxq"])?;
    Ok(())
}

/// The commits that touched `paths` lately, the newest first, a line each:
/// `a1b2c3d 2026-10-01 Fix the ledger`. Nothing when there are none or git
/// can't say.
pub fn log(checkout: &Path, paths: &[String], most: usize) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let most = most.to_string();
    let mut args = vec![
        "log",
        "--no-merges",
        "--format=%h %ad %s",
        "--date=short",
        "-n",
        &most,
        "--",
    ];
    args.extend(paths.iter().map(|path| path.trim_end_matches('/')));
    git(checkout, &args).unwrap_or_default()
}

/// What changed in `paths` from `old` to `new`, as a patch cut to `most`
/// bytes.
pub fn diff(checkout: &Path, old: &str, new: &str, paths: &[String], most: usize) -> String {
    let mut args = vec!["diff", "--no-color", "--no-ext-diff", "-M", old, new, "--"];
    args.extend(paths.iter().map(|path| path.trim_end_matches('/')));
    let mut patch = git(checkout, &args).unwrap_or_default();
    if patch.len() > most {
        let mut cut = most;
        while !patch.is_char_boundary(cut) {
            cut -= 1;
        }
        patch.truncate(cut);
        patch.push_str("\n[the rest of the diff is cut]\n");
    }
    patch
}

/// How many commits `new` is past `old`, or `None` when git can't say.
pub fn commits_between(dir: &Path, old: &str, new: &str) -> Option<u64> {
    git(dir, &["rev-list", "--count", &format!("{old}..{new}")])
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Where the lines of each file at one commit are at another.
#[derive(Debug, Default)]
pub struct Remap {
    /// Files renamed, from their old path to their new.
    renames: HashMap<String, String>,
    /// Each changed file's hunks, by its old path, in order.
    hunks: HashMap<String, Vec<Hunk>>,
}

/// A hunk of `git diff -U0`: `old_len` lines from `old_start` became
/// `new_len` from `new_start`. With `old_len` 0, the lines went in after
/// line `old_start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hunk {
    old_start: u32,
    old_len: u32,
    new_start: u32,
    new_len: u32,
}

impl Remap {
    /// What changed from `old` to `new` in the checkout at `dir`.
    pub fn between(dir: &Path, old: &str, new: &str) -> Result<Remap> {
        let patch = git(
            dir,
            &["diff", "--no-color", "--no-ext-diff", "-M", "-U0", old, new],
        )?;
        Ok(Remap::of_patch(&patch))
    }

    /// Reads a patch of `git diff -M -U0`.
    fn of_patch(patch: &str) -> Remap {
        let mut remap = Remap::default();
        let mut file: Option<String> = None;
        for line in patch.lines() {
            if line.starts_with("diff --git ") {
                file = None;
            } else if let Some(from) = line.strip_prefix("rename from ") {
                file = Some(from.to_string());
            } else if let Some(to) = line.strip_prefix("rename to ") {
                if let Some(from) = &file {
                    remap.renames.insert(from.clone(), to.to_string());
                }
            } else if let Some(path) = line.strip_prefix("--- a/") {
                file = Some(path.to_string());
            } else if let Some(header) = line.strip_prefix("@@ ") {
                let (Some(file), Some(hunk)) = (&file, hunk_of(header)) else {
                    continue;
                };
                remap.hunks.entry(file.clone()).or_default().push(hunk);
            }
        }
        remap
    }

    /// Where line `line` of `path` at the old commit is at the new: moved
    /// by the lines put in and taken out above it, or for a line that
    /// changed, the start of what it became.
    fn line(&self, path: &str, line: u32) -> u32 {
        let mut offset: i64 = 0;
        for hunk in self.hunks.get(path).into_iter().flatten() {
            if hunk.old_len == 0 {
                if line > hunk.old_start {
                    offset += i64::from(hunk.new_len);
                    continue;
                }
                break;
            }
            if line < hunk.old_start {
                break;
            }
            if line < hunk.old_start + hunk.old_len {
                return hunk.new_start.max(1);
            }
            offset += i64::from(hunk.new_len) - i64::from(hunk.old_len);
        }
        (i64::from(line) + offset).max(1) as u32
    }

    /// `link`, written at the old commit, where it is at the new.
    pub fn link(&self, link: &CodeLink) -> CodeLink {
        let path = self
            .renames
            .get(&link.path)
            .cloned()
            .unwrap_or_else(|| link.path.clone());
        let start = link.start.map(|line| self.line(&link.path, line));
        let end = match (link.end, start) {
            (Some(end), Some(start)) => Some(self.line(&link.path, end).max(start)),
            _ => None,
        };
        CodeLink { path, start, end }
    }

    /// Whether nothing moved.
    pub fn is_empty(&self) -> bool {
        self.renames.is_empty() && self.hunks.is_empty()
    }
}

/// `-12,3 +14,0 @@ …` read.
fn hunk_of(header: &str) -> Option<Hunk> {
    let mut parts = header.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let range = |text: &str| -> Option<(u32, u32)> {
        match text.split_once(',') {
            Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
            None => Some((text.parse().ok()?, 1)),
        }
    };
    let (old_start, old_len) = range(old)?;
    let (new_start, new_len) = range(new)?;
    Some(Hunk {
        old_start,
        old_len,
        new_start,
        new_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_line_moves_with_what_went_in_and_out_above_it() {
        let patch = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
@@ -2,0 +3,2 @@ fn x\n+one\n+two\n@@ -10,3 +12 @@\n-a\n-b\n-c\n+d\n\
diff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-x\n+y\n";
        let remap = Remap::of_patch(patch);
        // Above the first change, nothing moves.
        assert_eq!(remap.line("src/a.rs", 2), 2);
        // Two lines went in after line 2.
        assert_eq!(remap.line("src/a.rs", 3), 5);
        assert_eq!(remap.line("src/a.rs", 9), 11);
        // Lines 10 to 12 became line 12.
        assert_eq!(remap.line("src/a.rs", 11), 12);
        // Below, two in and two out.
        assert_eq!(remap.line("src/a.rs", 20), 20);
        assert_eq!(remap.line("untouched.rs", 7), 7);
        let link = remap.link(&CodeLink {
            path: "old.rs".into(),
            start: Some(1),
            end: Some(4),
        });
        assert_eq!(
            link,
            CodeLink {
                path: "new.rs".into(),
                start: Some(1),
                end: Some(4)
            }
        );
        assert!(!remap.is_empty());
    }

    #[test]
    fn a_remote_on_a_forge_has_a_page_and_its_files_a_place() {
        let github = web("git@github.com:acme/app.git", Forge::GitHub).unwrap();
        assert_eq!(github.path, "acme/app");
        assert_eq!(github.url, "https://github.com/acme/app");
        assert_eq!(
            github.code_url,
            "https://github.com/acme/app/blob/{commit}/{path}"
        );
        let ssh = web("ssh://git@ssh.github.com:443/acme/app.git", Forge::GitHub).unwrap();
        assert_eq!(ssh.url, "https://github.com/acme/app");
        let token = web("https://x:ghp_token@github.com/acme/app", Forge::GitHub).unwrap();
        assert_eq!(token.url, "https://github.com/acme/app");
        let gitlab = web("https://git.acme.io/platform/sub/api.git", Forge::GitLab).unwrap();
        assert_eq!(gitlab.path, "platform/sub/api");
        assert_eq!(
            gitlab.code_url,
            "https://git.acme.io/platform/sub/api/-/blob/{commit}/{path}"
        );
        assert_eq!(web("git@github.com:app.git", Forge::GitHub), None);
    }

    #[test]
    fn the_default_branch_is_origin_s_then_main_or_master() {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin");
        let clone = dir.path().join("clone");
        fs::create_dir(&origin).unwrap();
        let run = |at: &Path, args: &[&str]| git(at, args).unwrap();
        run(&origin, &["init", "-q", "-b", "main"]);
        fs::write(origin.join("a.rs"), "fn a() {}\n").unwrap();
        run(&origin, &["add", "."]);
        run(
            &origin,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "first",
            ],
        );
        let first = run(&origin, &["rev-parse", "HEAD"]).trim().to_string();
        let tip = default_tip(&origin, false).unwrap();
        assert_eq!(
            (tip.branch.as_str(), tip.commit.as_str()),
            ("main", first.as_str())
        );
        let out = Command::new("git")
            .args(["clone", "-q"])
            .arg(&origin)
            .arg(&clone)
            .output()
            .unwrap();
        assert!(out.status.success());
        // The clone's own main moves ahead, but origin's is what's published.
        fs::write(clone.join("b.rs"), "fn b() {}\n").unwrap();
        run(&clone, &["add", "."]);
        run(
            &clone,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "local",
            ],
        );
        assert_eq!(default_tip(&clone, false).unwrap().commit, first);
        // Until origin moves, and it's fetched.
        fs::write(origin.join("c.rs"), "fn c() {}\n").unwrap();
        run(&origin, &["add", "."]);
        run(
            &origin,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "second",
            ],
        );
        let second = run(&origin, &["rev-parse", "HEAD"]).trim().to_string();
        assert_eq!(default_tip(&clone, false).unwrap().commit, first);
        assert_eq!(default_tip(&clone, true).unwrap().commit, second);
    }

    #[test]
    fn a_checkout_is_the_commit_and_nothing_uncommitted() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("app");
        fs::create_dir(&project).unwrap();
        let run = |args: &[&str]| git(&project, args).unwrap();
        run(&["init", "-q"]);
        fs::write(project.join("a.rs"), "fn a() {}\n").unwrap();
        run(&["add", "."]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "first",
        ]);
        let first = run(&["rev-parse", "HEAD"]).trim().to_string();
        fs::write(project.join("a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qam",
            "second",
        ]);
        let second = run(&["rev-parse", "HEAD"]).trim().to_string();
        fs::write(project.join("a.rs"), "uncommitted\n").unwrap();
        let checkout = dir.path().join("wiki/checkout");
        prepare(&project, &checkout, &first).unwrap();
        assert_eq!(
            fs::read_to_string(checkout.join("a.rs")).unwrap(),
            "fn a() {}\n"
        );
        fs::write(checkout.join("stray.txt"), "x").unwrap();
        prepare(&project, &checkout, &second).unwrap();
        assert_eq!(
            fs::read_to_string(checkout.join("a.rs")).unwrap(),
            "fn a() {}\nfn b() {}\n"
        );
        assert!(!checkout.join("stray.txt").exists());
        assert!(log(&checkout, &["a.rs".into()], 5).contains(" second\n"));
        assert_eq!(commits_between(&checkout, &first, &second), Some(1));
        let remap = Remap::between(&checkout, &first, &second).unwrap();
        assert_eq!(remap.line("a.rs", 1), 1);
        assert!(diff(&checkout, &first, &second, &["a.rs".into()], 10_000).contains("+fn b() {}"));
        // The project has no worktree of it.
        assert!(!run(&["worktree", "list"]).contains("checkout"));
    }
}
