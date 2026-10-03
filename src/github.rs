//! What crystal asks GitHub: the pull requests open on a project, its open
//! issues, and an issue's text. Everything goes through `gh`, GitHub's own
//! command line tool, so the user's login works as it always does and
//! crystal never sees a token.
//!
//! None of it is needed to use crystal. A project whose `origin` isn't on
//! GitHub, a machine without `gh`, or a `gh` that isn't logged in, all give
//! an error here that says so in a line, for the TUI to show only when the
//! user asks for something from GitHub.

use crate::config::Config;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Whether crystal asks GitHub anything, the `github` plugin: off, no
/// pull requests on worktree lines, no `o` or `i`, and gh is never run.
pub fn enabled(config: &Config) -> bool {
    crate::plugins::enabled(config, "github")
}

/// How long gh gets to answer before it's given up on: it goes over the
/// network, and a TUI waiting on it would rather hear nothing than hang.
const GH_TIMEOUT: Duration = Duration::from_secs(20);

/// How many pull requests or issues to ask for at once.
const LIMIT: &str = "100";

/// The longest a branch named after an issue's title gets, past its number.
const BRANCH_WORDS_MAX: usize = 40;

/// An open pull request, as `gh pr list --json` describes it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    /// The branch it would merge, which is how a worktree finds its pull
    /// request.
    pub head_ref_name: String,
    pub is_draft: bool,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, or nothing.
    #[serde(default)]
    pub review_decision: Option<String>,
    /// Its checks: GitHub Actions runs, and the older commit statuses.
    #[serde(default)]
    pub status_check_rollup: Vec<Check>,
    pub url: String,
}

/// One check on a pull request. A check run has a status and, once it's
/// completed, a conclusion; a commit status has only a state.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Check {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
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
        let checks = &self.status_check_rollup;
        let review = self.review_decision.as_deref().unwrap_or("");
        if checks.iter().any(Check::failed) {
            PullRequestState::ChecksFailing
        } else if review == "CHANGES_REQUESTED" {
            PullRequestState::ChangesRequested
        } else if self.is_draft {
            PullRequestState::Draft
        } else if checks.iter().any(Check::running) {
            PullRequestState::ChecksRunning
        } else if review == "APPROVED" {
            PullRequestState::Approved
        } else {
            PullRequestState::Ready
        }
    }
}

impl Check {
    /// Whether it ended any way but well. A skipped or neutral check is
    /// fine; a cancelled one didn't pass.
    fn failed(&self) -> bool {
        let failed_conclusion = matches!(
            self.conclusion.as_deref(),
            Some("FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE")
        );
        let failed_state = matches!(self.state.as_deref(), Some("FAILURE" | "ERROR"));
        failed_conclusion || failed_state
    }

    /// Whether it hasn't finished yet.
    fn running(&self) -> bool {
        let run_going = self
            .status
            .as_deref()
            .is_some_and(|status| status != "COMPLETED");
        let status_waiting = matches!(self.state.as_deref(), Some("PENDING" | "EXPECTED"));
        run_going || status_waiting
    }
}

/// An open issue, as `gh issue list --json` describes it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub labels: Vec<Label>,
    /// When it last changed, as GitHub writes it: `2026-10-02T09:30:00Z`.
    /// Written that way, later times sort after earlier ones.
    pub updated_at: String,
    pub author: Author,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Label {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Author {
    pub login: String,
}

/// The pull requests open on the project at `project`.
pub fn pull_requests(project: &Path) -> Result<Vec<PullRequest>, String> {
    on_github(project)?;
    let fields = "number,title,headRefName,isDraft,reviewDecision,statusCheckRollup,url";
    let args = [
        "pr", "list", "--state", "open", "--limit", LIMIT, "--json", fields,
    ];
    parse(&gh(project, &args)?)
}

/// The issues open on the project at `project`, the latest to change first.
pub fn issues(project: &Path) -> Result<Vec<Issue>, String> {
    on_github(project)?;
    let fields = "number,title,labels,updatedAt,author,url";
    let args = [
        "issue", "list", "--state", "open", "--limit", LIMIT, "--json", fields,
    ];
    let mut issues: Vec<Issue> = parse(&gh(project, &args)?)?;
    issues.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(issues)
}

/// What issue `number` says.
pub fn issue_body(project: &Path, number: u64) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Body {
        body: String,
    }
    let number = number.to_string();
    let args = ["issue", "view", number.as_str(), "--json", "body"];
    let body: Body = parse(&gh(project, &args)?)?;
    Ok(body.body)
}

/// Opens pull request `number` in the user's browser.
pub fn open_pull_request(project: &Path, number: u64) -> Result<(), String> {
    let number = number.to_string();
    gh(project, &["pr", "view", "--web", number.as_str()])?;
    Ok(())
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

/// Whether `url`, a git remote, is on github.com.
pub fn is_github_url(url: &str) -> bool {
    let url = url.trim();
    let prefixes = [
        "https://github.com/",
        "http://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ];
    prefixes.iter().any(|prefix| url.starts_with(prefix))
}

/// An error that says so unless `project`'s origin remote is on GitHub.
fn on_github(project: &Path) -> Result<(), String> {
    let name = project.file_name().map_or_else(
        || project.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let output = Command::new("git")
        .arg("-C")
        .arg(project)
        .args(["remote", "get-url", "origin"])
        .stderr(Stdio::null())
        .output();
    let url = match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        _ => {
            return Err(format!(
                "{name} has no origin remote, so nothing from GitHub"
            ));
        }
    };
    if is_github_url(&url) {
        Ok(())
    } else {
        Err(format!("{name}'s origin isn't on GitHub"))
    }
}

/// Reads gh's JSON answer.
fn parse<T: DeserializeOwned>(json: &str) -> Result<T, String> {
    serde_json::from_str(json).map_err(|err| format!("couldn't read what gh answered: {err}"))
}

/// Runs gh in `dir` and gives back what it printed, or, when it couldn't
/// run, failed or took too long, a line saying so.
fn gh(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut child = Command::new("gh")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "gh isn't installed: it's GitHub's CLI, at cli.github.com".to_string())?;
    // Read on threads of their own while gh runs: a long answer would
    // otherwise fill the pipe, and gh would wait on it while it's waited on.
    let stdout = read_to_end(child.stdout.take());
    let stderr = read_to_end(child.stderr.take());

    let deadline = Instant::now() + GH_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("gh took too long to answer".to_string());
            }
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if status.success() {
        Ok(stdout)
    } else {
        // gh's own first line says what's wrong, like that it isn't logged
        // in.
        let reason = stderr.lines().find(|line| !line.trim().is_empty());
        Err(format!("gh: {}", reason.unwrap_or("it failed").trim()))
    }
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

    fn pull_request(json: &str) -> PullRequest {
        let base = serde_json::json!({
            "number": 57,
            "title": "Fix the login redirect",
            "headRefName": "fix-login",
            "isDraft": false,
            "reviewDecision": "",
            "statusCheckRollup": [],
            "url": "https://github.com/acme/app/pull/57",
        });
        let mut value = base;
        let changes: serde_json::Value = serde_json::from_str(json).unwrap();
        for (key, change) in changes.as_object().unwrap() {
            value[key] = change.clone();
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_pull_request_with_nothing_to_say_is_ready() {
        assert_eq!(pull_request("{}").state(), Ready);
    }

    #[test]
    fn a_failing_check_matters_most() {
        let pr = pull_request(
            r#"{"isDraft": true, "reviewDecision": "APPROVED",
                "statusCheckRollup": [
                    {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SUCCESS"},
                    {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "FAILURE"}
                ]}"#,
        );
        assert_eq!(pr.state(), ChecksFailing);
        let status = pull_request(
            r#"{"statusCheckRollup": [{"__typename": "StatusContext", "state": "ERROR"}]}"#,
        );
        assert_eq!(status.state(), ChecksFailing);
    }

    #[test]
    fn reviews_drafts_and_running_checks_come_in_order() {
        let changes = pull_request(r#"{"reviewDecision": "CHANGES_REQUESTED", "isDraft": true}"#);
        assert_eq!(changes.state(), ChangesRequested);
        let draft = pull_request(
            r#"{"isDraft": true, "statusCheckRollup": [{"status": "IN_PROGRESS", "conclusion": null}]}"#,
        );
        assert_eq!(draft.state(), Draft);
        let running = pull_request(
            r#"{"reviewDecision": "APPROVED", "statusCheckRollup": [{"state": "PENDING"}]}"#,
        );
        assert_eq!(running.state(), ChecksRunning);
        assert_eq!(
            pull_request(r#"{"reviewDecision": "APPROVED"}"#).state(),
            Approved
        );
    }

    #[test]
    fn skipped_and_neutral_checks_pass() {
        let pr = pull_request(
            r#"{"statusCheckRollup": [
                {"status": "COMPLETED", "conclusion": "SKIPPED"},
                {"status": "COMPLETED", "conclusion": "NEUTRAL"}
            ]}"#,
        );
        assert_eq!(pr.state(), Ready);
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
    fn github_remotes_are_told_apart_from_others() {
        assert!(is_github_url("https://github.com/acme/app.git\n"));
        assert!(is_github_url("git@github.com:acme/app.git"));
        assert!(is_github_url("ssh://git@github.com/acme/app"));
        assert!(!is_github_url("git@gitlab.com:acme/app.git"));
        assert!(!is_github_url("https://github.company.com/acme/app"));
    }

    #[test]
    fn issues_read_from_gh_keep_their_labels_and_author() {
        let json = r#"[{"number": 42, "title": "Login loops", "labels": [{"name": "bug"}],
            "updatedAt": "2026-10-02T09:30:00Z", "author": {"login": "ana"},
            "url": "https://github.com/acme/app/issues/42"}]"#;
        let issues: Vec<Issue> = parse(json).unwrap();
        assert_eq!(issues[0].labels[0].name, "bug");
        assert_eq!(issues[0].author.login, "ana");
    }
}
