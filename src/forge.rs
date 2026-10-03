//! What crystal asks the forge a project is on, GitHub or GitLab: the pull
//! requests open on it (merge requests, GitLab calls them), its open
//! issues, one of either read with its conversation, a pull request's diff,
//! and the comments and issue edits written back. Everything goes through
//! the forge's own command line tool, `gh` or `glab`, so the user's login
//! works as it always does and crystal never sees a token.
//!
//! Which forge a project is on is read off its remote's host: github.com,
//! or a host `gh` is logged in to, like a GitHub Enterprise, is GitHub;
//! gitlab.com, or a host glab's config names, like a GitLab of the user's
//! own, is GitLab. Telling them apart reads git's config and the two CLIs'
//! files, and never goes over the network. [`github`] and [`gitlab`] each
//! turn a call into their CLI's command and read what it answers into the
//! same types.
//!
//! None of it is needed to use crystal. A project on neither, a machine
//! without the CLI, or a CLI that isn't logged in, all give an error here
//! that says so in a line, for the TUI to show only when the user asks for
//! something from the forge.

mod github;
mod gitlab;

use crate::config::Config;
use serde::{Deserialize, Deserializer};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Whether crystal asks the forge anything, the `github` plugin, named
/// for the forge it began with: off, no pull requests on worktree lines,
/// no `o`, `O` or `i`, and neither gh nor glab is ever run.
pub fn enabled(config: &Config) -> bool {
    crate::plugins::enabled(config, "github")
}

/// How long the forge's CLI gets to answer before it's given up on: it
/// goes over the network, and a TUI waiting on it would rather hear nothing
/// than hang.
const TIMEOUT: Duration = Duration::from_secs(20);

/// How many pull requests or issues to ask for at once: GitLab gives no
/// more than this a page.
const LIMIT: &str = "100";

/// The longest a branch named after an issue's title gets, past its number.
const BRANCH_WORDS_MAX: usize = 40;

/// The forges crystal speaks to, each through its own CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forge {
    GitHub,
    GitLab,
}

impl Forge {
    pub fn name(self) -> &'static str {
        match self {
            Forge::GitHub => "GitHub",
            Forge::GitLab => "GitLab",
        }
    }

    /// What it calls a pull request.
    pub fn pull_request(self) -> &'static str {
        match self {
            Forge::GitHub => "pull request",
            Forge::GitLab => "merge request",
        }
    }

    pub fn pull_requests(self) -> &'static str {
        match self {
            Forge::GitHub => "pull requests",
            Forge::GitLab => "merge requests",
        }
    }

    /// Pull request `number` as the forge writes it: `#57` on GitHub, `!57`
    /// on GitLab. Issues are `#7` on both.
    pub fn label(self, number: u64) -> String {
        match self {
            Forge::GitHub => format!("#{number}"),
            Forge::GitLab => format!("!{number}"),
        }
    }

    /// Where the forge keeps the commits of pull request `number`, on the
    /// project's own repository: how one from a fork is fetched.
    fn head_ref(self, number: u64) -> String {
        match self {
            Forge::GitHub => format!("refs/pull/{number}/head"),
            Forge::GitLab => format!("refs/merge-requests/{number}/head"),
        }
    }

    /// The command line tool it's asked through.
    fn cli(self) -> &'static str {
        match self {
            Forge::GitHub => "gh",
            Forge::GitLab => "glab",
        }
    }

    /// Where its CLI comes from, for one that isn't installed.
    fn cli_home(self) -> &'static str {
        match self {
            Forge::GitHub => "cli.github.com",
            Forge::GitLab => "gitlab.com/gitlab-org/cli",
        }
    }
}

/// An issue or a pull request, by its number: what a comment goes on, or
/// what's opened in the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    Issue(u64),
    PullRequest(u64),
}

/// An open pull request, as its forge lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub forge: Forge,
    pub number: u64,
    pub title: String,
    pub author: String,
    /// The branch it would merge, as the repository it comes from names it.
    pub branch: String,
    /// Whether it comes from a fork, whose branches aren't the project's
    /// whatever they're called: its commits are fetched from where the
    /// forge keeps them.
    pub from_fork: bool,
    /// The branch a worktree of it is on, which is how a worktree finds its
    /// pull request: its own branch, or, from a fork, that under the fork's
    /// owner, like `ana/main`, so that a fork's `main` is never taken for
    /// the project's.
    pub local_branch: String,
    pub draft: bool,
    pub checks: Checks,
    pub review: Review,
    /// When it last changed, as the forge writes it: `2026-10-02T09:30:00Z`.
    pub updated_at: String,
    pub url: String,
}

/// How a pull request's checks stand, all together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checks {
    /// It has none, or the forge didn't say: GitLab's list doesn't.
    None,
    Passed,
    Running,
    Failed,
}

impl Checks {
    /// All of `states` together: one failed fails them all, and one still
    /// running keeps them running. Skipped ones pass.
    fn of(states: impl IntoIterator<Item = CheckState>) -> Checks {
        let states: Vec<CheckState> = states.into_iter().collect();
        if states.contains(&CheckState::Failed) {
            Checks::Failed
        } else if states.contains(&CheckState::Running) {
            Checks::Running
        } else if states.is_empty() {
            Checks::None
        } else {
            Checks::Passed
        }
    }
}

/// What a pull request's reviewers decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Review {
    /// Nothing asked or said, or the forge didn't say.
    None,
    Required,
    Approved,
    ChangesRequested,
}

/// What matters most about a pull request, in the order it matters: a
/// failing check before anything else, since it's what has to be fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullRequestState {
    ChecksFailing,
    ChangesRequested,
    Draft,
    ChecksRunning,
    Approved,
    Ready,
}

impl PullRequest {
    pub fn state(&self) -> PullRequestState {
        if self.checks == Checks::Failed {
            PullRequestState::ChecksFailing
        } else if self.review == Review::ChangesRequested {
            PullRequestState::ChangesRequested
        } else if self.draft {
            PullRequestState::Draft
        } else if self.checks == Checks::Running {
            PullRequestState::ChecksRunning
        } else if self.review == Review::Approved {
            PullRequestState::Approved
        } else {
            PullRequestState::Ready
        }
    }

    /// Its number as its forge writes it: `#57`, or `!57` on GitLab.
    pub fn label(&self) -> String {
        self.forge.label(self.number)
    }

    /// What a worktree of it in the project at `project` takes.
    pub fn checkout(&self, project: &Path) -> Checkout {
        let fetch = if self.from_fork {
            self.forge.head_ref(self.number)
        } else {
            self.branch.clone()
        };
        Checkout {
            project: project.to_path_buf(),
            branch: self.local_branch.clone(),
            fetch,
        }
    }
}

/// What a worktree of a pull request takes: the project it's made in, the
/// branch it goes on, and what to fetch from `origin` for it, the pull
/// request's branch or, for one from a fork, the ref its forge keeps it
/// under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    pub project: PathBuf,
    pub branch: String,
    pub fetch: String,
}

/// A pull request read whole, beyond what its line in the list says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullRequestDetail {
    /// The branch it would merge into.
    pub base: String,
    pub body: String,
    pub checks: Vec<Check>,
    /// Its comments and reviews, the oldest first.
    pub conversation: Vec<Comment>,
}

/// One of a pull request's checks: a CI job, a commit status, or on
/// GitLab, its pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub state: CheckState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Passed,
    Failed,
    Running,
    Skipped,
}

/// Something someone said on an issue or a pull request: a comment, or a
/// review, with what it decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub author: String,
    /// When, as the forge writes it.
    pub at: String,
    pub body: String,
    /// What a review decided, for a review that approved or asked for
    /// changes.
    pub verdict: Option<Review>,
}

/// An open issue, as its forge lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub labels: Vec<String>,
    /// When it last changed, as the forge writes it: `2026-10-02T09:30:00Z`.
    /// Written that way, later times sort after earlier ones.
    pub updated_at: String,
    pub author: String,
    pub url: String,
}

/// An issue read whole: its text, and what's been said on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueDetail {
    pub body: String,
    /// The oldest first.
    pub comments: Vec<Comment>,
}

/// A project on its forge: which forge, and the directory its CLI runs in,
/// where it finds the repository from the remotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub forge: Forge,
    dir: PathBuf,
}

impl Repo {
    /// The project at `project` on the forge its remote is on, or a line
    /// that says why it's on none.
    pub fn find(project: &Path) -> Result<Repo, String> {
        let name = project.file_name().map_or_else(
            || project.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let Some(url) = remote_url(project) else {
            return Err(format!(
                "{name} has no remote, so nothing from GitHub or GitLab"
            ));
        };
        let Some(host) = remote_host(&url) else {
            return Err(format!(
                "{name}'s remote is on this machine, not GitHub or GitLab"
            ));
        };
        match Hosts::known().forge_of(&host) {
            Some(forge) => Ok(Repo {
                forge,
                dir: project.to_path_buf(),
            }),
            None => Err(format!(
                "{name}'s remote is on {host}, which neither gh nor glab is logged in to"
            )),
        }
    }

    /// The pull requests open on it.
    pub fn pull_requests(&self) -> Result<Vec<PullRequest>, String> {
        match self.forge {
            Forge::GitHub => github::pull_requests(&self.dir),
            Forge::GitLab => gitlab::pull_requests(&self.dir),
        }
    }

    pub fn pull_request(&self, number: u64) -> Result<PullRequestDetail, String> {
        match self.forge {
            Forge::GitHub => github::pull_request(&self.dir, number),
            Forge::GitLab => gitlab::pull_request(&self.dir, number),
        }
    }

    /// What pull request `number` changes, as a patch.
    pub fn diff(&self, number: u64) -> Result<String, String> {
        match self.forge {
            Forge::GitHub => github::diff(&self.dir, number),
            Forge::GitLab => gitlab::diff(&self.dir, number),
        }
    }

    /// The issues open on it, the latest to change first.
    pub fn issues(&self) -> Result<Vec<Issue>, String> {
        let mut issues = match self.forge {
            Forge::GitHub => github::issues(&self.dir),
            Forge::GitLab => gitlab::issues(&self.dir),
        }?;
        issues.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(issues)
    }

    pub fn issue(&self, number: u64) -> Result<IssueDetail, String> {
        match self.forge {
            Forge::GitHub => github::issue(&self.dir, number),
            Forge::GitLab => gitlab::issue(&self.dir, number),
        }
    }

    /// Posts `text` on `topic`, as the user the CLI is logged in as.
    pub fn comment(&self, topic: Topic, text: &str) -> Result<(), String> {
        match self.forge {
            Forge::GitHub => github::comment(&self.dir, topic, text),
            Forge::GitLab => gitlab::comment(&self.dir, topic, text),
        }
    }

    /// Gives issue `number` this title and text.
    pub fn edit_issue(&self, number: u64, title: &str, body: &str) -> Result<(), String> {
        match self.forge {
            Forge::GitHub => github::edit_issue(&self.dir, number, title, body),
            Forge::GitLab => gitlab::edit_issue(&self.dir, number, title, body),
        }
    }

    /// Opens `topic` in the user's browser.
    pub fn open(&self, topic: Topic) -> Result<(), String> {
        let (kind, number) = match (self.forge, topic) {
            (Forge::GitHub, Topic::PullRequest(number)) => ("pr", number),
            (Forge::GitLab, Topic::PullRequest(number)) => ("mr", number),
            (_, Topic::Issue(number)) => ("issue", number),
        };
        let number = number.to_string();
        run(
            self.forge,
            &self.dir,
            &[kind, "view", "--web", &number],
            None,
        )?;
        Ok(())
    }
}

/// A branch named for issue `number`: its number, then the words of its
/// title, lower case and joined by dashes, as many as fit in about forty
/// characters. "Fix login redirect!" as #42 is `42-fix-login-redirect`.
pub fn branch_for_issue(number: u64, title: &str) -> String {
    let words = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase);
    let mut slug = String::new();
    for word in words {
        let room = BRANCH_WORDS_MAX.saturating_sub(slug.chars().count());
        if slug.is_empty() {
            // A first word too long for the room is cut, rather than lost.
            slug = word.chars().take(BRANCH_WORDS_MAX).collect();
        } else if word.chars().count() < room {
            // A dash and the word, in what's left.
            slug.push('-');
            slug.push_str(&word);
        } else {
            break;
        }
    }
    if slug.is_empty() {
        number.to_string()
    } else {
        format!("{number}-{slug}")
    }
}

/// The URL of the remote the forge's CLI goes by: `origin`, or the first
/// remote when there's no `origin`. Read as configured rather than as
/// `insteadOf` rewrites it, since the forge is whatever the user named.
fn remote_url(project: &Path) -> Option<String> {
    let url_of = |remote: &str| {
        let key = format!("remote.{remote}.url");
        git_says(project, &["config", "--get", &key])
    };
    url_of("origin").or_else(|| {
        let remotes = git_says(project, &["remote"])?;
        url_of(remotes.lines().next()?)
    })
}

/// What git printed, trimmed, or `None` when it failed or printed nothing.
fn git_says(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let said = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !said.is_empty()).then_some(said)
}

/// The host a git remote is on, in lower case, without a user or a port:
/// `git@gitlab.com:group/app.git` is on `gitlab.com`. A path on this
/// machine, or a `file://` URL, is on none.
fn remote_host(url: &str) -> Option<String> {
    let url = url.trim();
    let authority = match url.split_once("://") {
        Some(("ssh" | "git" | "git+ssh" | "https" | "http", rest)) => rest.split('/').next()?,
        Some(_) => return None,
        // scp's way, `user@host:path`; a path has a slash before any colon.
        None => match url.split_once(':') {
            Some((host, _)) if !host.contains('/') => host,
            _ => return None,
        },
    };
    let host = authority.rsplit('@').next()?;
    let host = match host.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => host,
    };
    let host = host.trim().to_ascii_lowercase();
    (host.len() > 1 && !host.starts_with(['.', '/'])).then_some(host)
}

/// The hosts besides github.com and gitlab.com that the two CLIs know: the
/// ones `gh` is logged in to, and the ones glab's config names, each with
/// the host its environment variable points at.
#[derive(Debug, Default)]
struct Hosts {
    github: Vec<String>,
    gitlab: Vec<String>,
}

impl Hosts {
    /// The hosts as the CLIs' files and the environment have them now.
    fn known() -> Hosts {
        let read = |path: PathBuf| std::fs::read_to_string(path).unwrap_or_default();
        let mut github = parse_gh_hosts(&read(gh_hosts_file()));
        let mut gitlab: Vec<String> = glab_config_files()
            .into_iter()
            .flat_map(|file| parse_glab_hosts(&read(file)))
            .collect();
        let variable = |name: &str| {
            let value = std::env::var(name).ok().filter(|value| !value.is_empty())?;
            // GITLAB_HOST may be a URL as well as a host.
            remote_host(&value).or(Some(value.to_ascii_lowercase()))
        };
        github.extend(variable("GH_HOST"));
        gitlab.extend(variable("GITLAB_HOST").or_else(|| variable("GL_HOST")));
        Hosts { github, gitlab }
    }

    fn forge_of(&self, host: &str) -> Option<Forge> {
        let has = |hosts: &[String]| hosts.iter().any(|known| known == host);
        if matches!(host, "github.com" | "ssh.github.com") || has(&self.github) {
            Some(Forge::GitHub)
        } else if matches!(host, "gitlab.com" | "altssh.gitlab.com") || has(&self.gitlab) {
            Some(Forge::GitLab)
        } else {
            None
        }
    }
}

/// gh's `hosts.yml`, where it looks for it: in `$GH_CONFIG_DIR`, or else
/// `$XDG_CONFIG_HOME/gh`, or else `~/.config/gh`.
fn gh_hosts_file() -> PathBuf {
    config_dir("GH_CONFIG_DIR", "gh").join("hosts.yml")
}

/// glab's `config.yml`, where it looks for it: in `$GLAB_CONFIG_DIR`, or
/// else `$XDG_CONFIG_HOME/glab-cli`, or else `~/.config/glab-cli` or, on a
/// Mac, where newer glabs keep it, in `~/Library/Application Support`.
fn glab_config_files() -> Vec<PathBuf> {
    let mut dirs = vec![config_dir("GLAB_CONFIG_DIR", "glab-cli")];
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    if !set("GLAB_CONFIG_DIR") && !set("XDG_CONFIG_HOME") {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        dirs.push(home.join("Library/Application Support/glab-cli"));
    }
    dirs.into_iter().map(|dir| dir.join("config.yml")).collect()
}

/// A CLI's config directory: the one its own variable names, or else
/// `name` in `$XDG_CONFIG_HOME`, or else in `~/.config`.
fn config_dir(own: &str, name: &str) -> PathBuf {
    let var = |key: &str| std::env::var_os(key).filter(|value| !value.is_empty());
    if let Some(dir) = var(own) {
        return PathBuf::from(dir);
    }
    match var("XDG_CONFIG_HOME") {
        Some(config) => PathBuf::from(config).join(name),
        None => PathBuf::from(var("HOME").unwrap_or_default())
            .join(".config")
            .join(name),
    }
}

/// The hosts in gh's `hosts.yml`: its keys at the top, one a host.
fn parse_gh_hosts(yaml: &str) -> Vec<String> {
    yaml.lines()
        .filter(|line| !line.starts_with([' ', '\t', '#', '-']))
        .filter_map(yaml_key)
        .collect()
}

/// The hosts in glab's `config.yml`: the keys one level in under `hosts:`.
fn parse_glab_hosts(yaml: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    let mut under_hosts = false;
    let mut indent = None;
    for line in yaml.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let depth = line.len() - trimmed.len();
        if depth == 0 {
            under_hosts = yaml_key(line).as_deref() == Some("hosts");
            indent = None;
        } else if under_hosts && depth == *indent.get_or_insert(depth) {
            hosts.extend(yaml_key(trimmed));
        }
    }
    hosts
}

/// The key a YAML line starts with, `key:` or `"key":`, in lower case.
fn yaml_key(line: &str) -> Option<String> {
    let (key, _) = line.split_once(':')?;
    let key = key.trim().trim_matches(['"', '\'']);
    (!key.is_empty() && !key.contains(' ')).then(|| key.to_ascii_lowercase())
}

/// Reads a JSON string that may be missing or `null` as an empty one, the
/// way both forges leave out what's blank.
fn text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// Reads a JSON list that may be missing or `null` as an empty one.
fn list<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

/// Reads what the forge's CLI answered.
fn parse<T: serde::de::DeserializeOwned>(forge: Forge, json: &str) -> Result<T, String> {
    serde_json::from_str(json)
        .map_err(|err| format!("couldn't read what {} answered: {err}", forge.cli()))
}

/// Runs the forge's CLI in `dir`, with `input` on its standard input if
/// there is some, and gives back what it printed, or, when it couldn't
/// run, failed or took too long, a line saying so.
fn run(forge: Forge, dir: &Path, args: &[&str], input: Option<&str>) -> Result<String, String> {
    let cli = forge.cli();
    let stdin = if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let mut child = Command::new(cli)
        .args(args)
        .current_dir(dir)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| {
            format!(
                "{cli} isn't installed: it's {}'s CLI, at {}",
                forge.name(),
                forge.cli_home()
            )
        })?;
    if let (Some(mut pipe), Some(input)) = (child.stdin.take(), input) {
        let input = input.to_string();
        // Written on a thread of its own, closing the pipe when it's done,
        // so that neither side waits on the other.
        thread::spawn(move || {
            let _ = pipe.write_all(input.as_bytes());
        });
    }
    // Read on threads of their own while it runs: a long answer would
    // otherwise fill the pipe, and the CLI would wait on it while it's
    // waited on.
    let stdout = read_to_end(child.stdout.take());
    let stderr = read_to_end(child.stderr.take());

    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{cli} took too long to answer"));
            }
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if status.success() {
        Ok(stdout)
    } else {
        Err(format!("{cli}: {}", first_line(&stderr)))
    }
}

/// The CLI's first line of what's wrong, like that it isn't logged in.
/// glab puts its errors under an `ERROR` banner, which says nothing.
fn first_line(stderr: &str) -> &str {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && *line != "ERROR")
        .unwrap_or("it failed")
}

fn read_to_end(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<String> {
    thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use PullRequestState::*;

    fn pull_request() -> PullRequest {
        PullRequest {
            forge: Forge::GitHub,
            number: 57,
            title: "Fix the login redirect".into(),
            author: "ana".into(),
            branch: "fix-login".into(),
            from_fork: false,
            local_branch: "fix-login".into(),
            draft: false,
            checks: Checks::None,
            review: Review::None,
            updated_at: "2026-10-02T09:30:00Z".into(),
            url: "https://github.com/acme/app/pull/57".into(),
        }
    }

    #[test]
    fn a_pull_request_with_nothing_to_say_is_ready() {
        assert_eq!(pull_request().state(), Ready);
    }

    #[test]
    fn a_failing_check_matters_most() {
        let pr = PullRequest {
            draft: true,
            review: Review::Approved,
            checks: Checks::Failed,
            ..pull_request()
        };
        assert_eq!(pr.state(), ChecksFailing);
    }

    #[test]
    fn reviews_drafts_and_running_checks_come_in_order() {
        let base = pull_request();
        let changes = PullRequest {
            review: Review::ChangesRequested,
            draft: true,
            ..base.clone()
        };
        assert_eq!(changes.state(), ChangesRequested);
        let draft = PullRequest {
            draft: true,
            checks: Checks::Running,
            ..base.clone()
        };
        assert_eq!(draft.state(), Draft);
        let running = PullRequest {
            review: Review::Approved,
            checks: Checks::Running,
            ..base.clone()
        };
        assert_eq!(running.state(), ChecksRunning);
        let approved = PullRequest {
            review: Review::Approved,
            checks: Checks::Passed,
            ..base
        };
        assert_eq!(approved.state(), Approved);
    }

    #[test]
    fn checks_together_fail_on_one_failure_and_pass_when_skipped() {
        use CheckState::*;
        assert_eq!(Checks::of([Passed, Failed, Running]), Checks::Failed);
        assert_eq!(Checks::of([Passed, Running]), Checks::Running);
        assert_eq!(Checks::of([Skipped, Passed]), Checks::Passed);
        assert_eq!(Checks::of([]), Checks::None);
    }

    #[test]
    fn a_fork_is_checked_out_from_where_its_forge_keeps_it() {
        let project = Path::new("/code/app");
        let own = pull_request().checkout(project);
        assert_eq!(
            (own.branch.as_str(), own.fetch.as_str()),
            ("fix-login", "fix-login")
        );
        let fork = PullRequest {
            from_fork: true,
            branch: "main".into(),
            local_branch: "ana/main".into(),
            ..pull_request()
        };
        let checkout = fork.checkout(project);
        assert_eq!(checkout.branch, "ana/main");
        assert_eq!(checkout.fetch, "refs/pull/57/head");
        let merge_request = PullRequest {
            forge: Forge::GitLab,
            ..fork
        };
        assert_eq!(
            merge_request.checkout(project).fetch,
            "refs/merge-requests/57/head"
        );
        assert_eq!(merge_request.label(), "!57");
    }

    #[test]
    fn a_branch_for_an_issue_starts_with_its_number() {
        assert_eq!(
            branch_for_issue(42, "Fix login redirect!"),
            "42-fix-login-redirect"
        );
        assert_eq!(
            branch_for_issue(7, "Crash when the user's cart is empty (iOS 18)"),
            "7-crash-when-the-user-s-cart-is-empty-ios"
        );
        assert_eq!(branch_for_issue(9, "!!!"), "9");
    }

    #[test]
    fn a_long_title_keeps_whole_words_and_a_long_word_is_cut() {
        let branch = branch_for_issue(1, "word ".repeat(30).as_str());
        assert!(branch.chars().count() <= 2 + BRANCH_WORDS_MAX);
        assert!(branch.ends_with("word"));
        let long = branch_for_issue(3, &"x".repeat(60));
        assert_eq!(long, format!("3-{}", "x".repeat(BRANCH_WORDS_MAX)));
    }

    #[test]
    fn a_remotes_host_is_found_whatever_way_it_is_written() {
        let cases = [
            ("git@github.com:acme/app.git", Some("github.com")),
            ("https://github.com/acme/app.git\n", Some("github.com")),
            (
                "ssh://git@ssh.github.com:443/acme/app.git",
                Some("ssh.github.com"),
            ),
            (
                "https://x:ghp_token@github.com/acme/app",
                Some("github.com"),
            ),
            ("git@gitlab.com:group/sub/app.git", Some("gitlab.com")),
            ("ssh://git@GitLab.com:22/group/app.git", Some("gitlab.com")),
            ("https://git.acme.io/platform/api.git", Some("git.acme.io")),
            ("/srv/git/app.git", None),
            ("file:///srv/git/app.git", None),
            ("../sibling", None),
        ];
        for (url, host) in cases {
            assert_eq!(remote_host(url).as_deref(), host, "{url}");
        }
    }

    #[test]
    fn hosts_the_clis_know_are_their_forges_and_others_are_neither() {
        let hosts = Hosts {
            github: parse_gh_hosts("github.com:\n    user: me\nghe.corp.dev:\n    user: me\n"),
            gitlab: parse_glab_hosts(
                "git_protocol: ssh\nhosts:\n    gitlab.com:\n        api_protocol: https\n    \
                 git.acme.io:\n        token: x\n        api_host: git.acme.io\nno_prompt: false\n",
            ),
        };
        assert_eq!(hosts.github, ["github.com", "ghe.corp.dev"]);
        assert_eq!(hosts.gitlab, ["gitlab.com", "git.acme.io"]);
        assert_eq!(hosts.forge_of("github.com"), Some(Forge::GitHub));
        assert_eq!(hosts.forge_of("ghe.corp.dev"), Some(Forge::GitHub));
        assert_eq!(hosts.forge_of("gitlab.com"), Some(Forge::GitLab));
        assert_eq!(hosts.forge_of("git.acme.io"), Some(Forge::GitLab));
        assert_eq!(
            Hosts::default().forge_of("altssh.gitlab.com"),
            Some(Forge::GitLab)
        );
        assert_eq!(hosts.forge_of("forge.example.org"), None);
    }

    #[test]
    fn a_clis_error_is_its_first_line_that_says_something() {
        assert_eq!(
            first_line("\n   ERROR  \n\n  401 Unauthorized\n"),
            "401 Unauthorized"
        );
        assert_eq!(
            first_line("To get started with GitHub CLI, please run:  gh auth login\n"),
            "To get started with GitHub CLI, please run:  gh auth login"
        );
        assert_eq!(first_line(""), "it failed");
    }
}
