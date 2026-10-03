//! GitHub, through `gh`: each of [`Repo`](super::Repo)'s calls as a `gh`
//! command, and what it answers with `--json` read into crystal's own
//! types.

use super::{
    Check, CheckState, Checks, Comment, Forge, Issue, IssueDetail, LIMIT, PullRequest,
    PullRequestDetail, Review, Topic, list, parse, run, text,
};
use serde::Deserialize;
use std::path::Path;

const FORGE: Forge = Forge::GitHub;

pub(super) fn pull_requests(dir: &Path) -> Result<Vec<PullRequest>, String> {
    let fields = "number,title,author,headRefName,isDraft,isCrossRepository,\
                  headRepositoryOwner,reviewDecision,statusCheckRollup,updatedAt,url";
    let args = [
        "pr", "list", "--state", "open", "--limit", LIMIT, "--json", fields,
    ];
    parse_pull_requests(&gh(dir, &args)?)
}

pub(super) fn pull_request(dir: &Path, number: u64) -> Result<PullRequestDetail, String> {
    let number = number.to_string();
    let fields = "baseRefName,body,statusCheckRollup,comments,reviews";
    let args = ["pr", "view", &number, "--json", fields];
    parse_pull_request(&gh(dir, &args)?)
}

pub(super) fn diff(dir: &Path, number: u64) -> Result<String, String> {
    let number = number.to_string();
    gh(dir, &["pr", "diff", &number, "--color", "never"])
}

pub(super) fn issues(dir: &Path) -> Result<Vec<Issue>, String> {
    let fields = "number,title,labels,updatedAt,author,url";
    let args = [
        "issue", "list", "--state", "open", "--limit", LIMIT, "--json", fields,
    ];
    parse_issues(&gh(dir, &args)?)
}

pub(super) fn issue(dir: &Path, number: u64) -> Result<IssueDetail, String> {
    let number = number.to_string();
    let args = ["issue", "view", &number, "--json", "body,comments"];
    parse_issue(&gh(dir, &args)?)
}

/// Posts `body` on `topic`, given on gh's standard input, so that the text
/// is never mistaken for an option however it starts.
pub(super) fn comment(dir: &Path, topic: Topic, body: &str) -> Result<(), String> {
    let (kind, number) = match topic {
        Topic::Issue(number) => ("issue", number),
        Topic::PullRequest(number) => ("pr", number),
    };
    let number = number.to_string();
    let args = [kind, "comment", &number, "--body-file", "-"];
    run(FORGE, dir, &args, Some(body))?;
    Ok(())
}

/// The new title is one argument with its option, and the text goes on
/// gh's standard input.
pub(super) fn edit_issue(dir: &Path, number: u64, title: &str, body: &str) -> Result<(), String> {
    let number = number.to_string();
    let title = format!("--title={title}");
    let args = ["issue", "edit", &number, &title, "--body-file", "-"];
    run(FORGE, dir, &args, Some(body))?;
    Ok(())
}

fn gh(dir: &Path, args: &[&str]) -> Result<String, String> {
    run(FORGE, dir, args, None)
}

/// A person, as gh names them; a deleted one's account is `null`.
#[derive(Deserialize)]
struct User {
    #[serde(default, deserialize_with = "text")]
    login: String,
}

fn login(user: Option<User>) -> String {
    user.map(|user| user.login).unwrap_or_default()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedPullRequest {
    number: u64,
    #[serde(default, deserialize_with = "text")]
    title: String,
    #[serde(default)]
    author: Option<User>,
    #[serde(default, deserialize_with = "text")]
    head_ref_name: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    is_cross_repository: bool,
    #[serde(default)]
    head_repository_owner: Option<User>,
    #[serde(default, deserialize_with = "text")]
    review_decision: String,
    /// Its checks: GitHub Actions runs, and the older commit statuses.
    #[serde(default, deserialize_with = "list")]
    status_check_rollup: Vec<GhCheck>,
    #[serde(default, deserialize_with = "text")]
    updated_at: String,
    #[serde(default, deserialize_with = "text")]
    url: String,
}

fn parse_pull_requests(json: &str) -> Result<Vec<PullRequest>, String> {
    let listed: Vec<ListedPullRequest> = parse(FORGE, json)?;
    Ok(listed.into_iter().map(pull_request_of).collect())
}

fn pull_request_of(listed: ListedPullRequest) -> PullRequest {
    let local_branch = local_branch(&listed);
    let review = match listed.review_decision.as_str() {
        "APPROVED" => Review::Approved,
        "CHANGES_REQUESTED" => Review::ChangesRequested,
        "REVIEW_REQUIRED" => Review::Required,
        _ => Review::None,
    };
    PullRequest {
        forge: FORGE,
        number: listed.number,
        title: listed.title,
        author: login(listed.author),
        branch: listed.head_ref_name,
        from_fork: listed.is_cross_repository,
        local_branch,
        draft: listed.is_draft,
        checks: Checks::of(listed.status_check_rollup.iter().map(GhCheck::state)),
        review,
        updated_at: listed.updated_at,
        url: listed.url,
    }
}

/// The branch a worktree of a pull request goes on: its own, or, for one
/// from a fork, its branch under the fork's owner, `ana/main`, the way
/// `gh pr checkout` names it. A fork that has since gone leaves no owner to
/// name: `pr-57/main`.
fn local_branch(listed: &ListedPullRequest) -> String {
    let branch = &listed.head_ref_name;
    if !listed.is_cross_repository {
        return branch.clone();
    }
    let owner = listed.head_repository_owner.as_ref();
    match owner.map(|owner| owner.login.as_str()) {
        Some(owner) if !owner.is_empty() => format!("{owner}/{branch}"),
        _ => format!("pr-{}/{branch}", listed.number),
    }
}

/// A check run, with a status and, once it's completed, a conclusion and
/// a name; or a commit status, with only a state and a context.
#[derive(Deserialize)]
struct GhCheck {
    #[serde(default, deserialize_with = "text")]
    name: String,
    #[serde(default, deserialize_with = "text")]
    context: String,
    #[serde(default, deserialize_with = "text")]
    status: String,
    #[serde(default, deserialize_with = "text")]
    conclusion: String,
    #[serde(default, deserialize_with = "text")]
    state: String,
}

impl GhCheck {
    /// A check that ended any way but well failed; a skipped or neutral
    /// one is fine, but a cancelled one didn't pass.
    fn state(&self) -> CheckState {
        let failed = matches!(
            self.conclusion.as_str(),
            "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE"
        ) || matches!(self.state.as_str(), "FAILURE" | "ERROR");
        let run_going = !self.status.is_empty() && self.status != "COMPLETED";
        let status_waiting = matches!(self.state.as_str(), "PENDING" | "EXPECTED");
        let skipped = matches!(self.conclusion.as_str(), "SKIPPED" | "NEUTRAL" | "STALE");
        if failed {
            CheckState::Failed
        } else if run_going || status_waiting {
            CheckState::Running
        } else if skipped {
            CheckState::Skipped
        } else {
            CheckState::Passed
        }
    }

    fn into_check(self) -> Check {
        let state = self.state();
        let name = if self.name.is_empty() {
            self.context
        } else {
            self.name
        };
        Check { name, state }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadPullRequest {
    #[serde(default, deserialize_with = "text")]
    base_ref_name: String,
    #[serde(default, deserialize_with = "text")]
    body: String,
    #[serde(default, deserialize_with = "list")]
    status_check_rollup: Vec<GhCheck>,
    #[serde(default, deserialize_with = "list")]
    comments: Vec<GhComment>,
    #[serde(default, deserialize_with = "list")]
    reviews: Vec<GhReview>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhComment {
    #[serde(default)]
    author: Option<User>,
    #[serde(default, deserialize_with = "text")]
    body: String,
    #[serde(default, deserialize_with = "text")]
    created_at: String,
}

impl GhComment {
    fn into_comment(self) -> Comment {
        Comment {
            author: login(self.author),
            at: self.created_at,
            body: self.body,
            verdict: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhReview {
    #[serde(default)]
    author: Option<User>,
    #[serde(default, deserialize_with = "text")]
    body: String,
    #[serde(default, deserialize_with = "text")]
    state: String,
    #[serde(default, deserialize_with = "text")]
    submitted_at: String,
}

impl GhReview {
    /// The review as part of the conversation, unless it says nothing: one
    /// that only commented on lines, with no text of its own, or one still
    /// pending, which only its author sees.
    fn into_comment(self) -> Option<Comment> {
        let verdict = match self.state.as_str() {
            "APPROVED" => Some(Review::Approved),
            "CHANGES_REQUESTED" => Some(Review::ChangesRequested),
            _ => None,
        };
        let says_nothing = verdict.is_none() && self.body.trim().is_empty();
        if says_nothing || self.state == "PENDING" {
            return None;
        }
        Some(Comment {
            author: login(self.author),
            at: self.submitted_at,
            body: self.body,
            verdict,
        })
    }
}

fn parse_pull_request(json: &str) -> Result<PullRequestDetail, String> {
    let read: ReadPullRequest = parse(FORGE, json)?;
    let comments = read.comments.into_iter().map(GhComment::into_comment);
    let reviews = read.reviews.into_iter().filter_map(GhReview::into_comment);
    let mut conversation: Vec<Comment> = comments.chain(reviews).collect();
    conversation.sort_by(|a, b| a.at.cmp(&b.at));
    Ok(PullRequestDetail {
        base: read.base_ref_name,
        body: read.body,
        checks: read
            .status_check_rollup
            .into_iter()
            .map(GhCheck::into_check)
            .collect(),
        conversation,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedIssue {
    number: u64,
    #[serde(default, deserialize_with = "text")]
    title: String,
    #[serde(default, deserialize_with = "list")]
    labels: Vec<Label>,
    #[serde(default, deserialize_with = "text")]
    updated_at: String,
    #[serde(default)]
    author: Option<User>,
    #[serde(default, deserialize_with = "text")]
    url: String,
}

#[derive(Deserialize)]
struct Label {
    #[serde(default, deserialize_with = "text")]
    name: String,
}

fn parse_issues(json: &str) -> Result<Vec<Issue>, String> {
    let listed: Vec<ListedIssue> = parse(FORGE, json)?;
    let issues = listed.into_iter().map(|issue| Issue {
        number: issue.number,
        title: issue.title,
        labels: issue.labels.into_iter().map(|label| label.name).collect(),
        updated_at: issue.updated_at,
        author: login(issue.author),
        url: issue.url,
    });
    Ok(issues.collect())
}

#[derive(Deserialize)]
struct ReadIssue {
    #[serde(default, deserialize_with = "text")]
    body: String,
    #[serde(default, deserialize_with = "list")]
    comments: Vec<GhComment>,
}

fn parse_issue(json: &str) -> Result<IssueDetail, String> {
    let read: ReadIssue = parse(FORGE, json)?;
    Ok(IssueDetail {
        body: read.body,
        comments: read
            .comments
            .into_iter()
            .map(GhComment::into_comment)
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(json: &str) -> PullRequest {
        let base = serde_json::json!({
            "number": 57,
            "title": "Fix the login redirect",
            "author": {"login": "ana"},
            "headRefName": "fix-login",
            "isDraft": false,
            "isCrossRepository": false,
            "headRepositoryOwner": {"login": "acme"},
            "reviewDecision": "",
            "statusCheckRollup": [],
            "updatedAt": "2026-10-02T09:30:00Z",
            "url": "https://github.com/acme/app/pull/57",
        });
        let mut value = base;
        let changes: serde_json::Value = serde_json::from_str(json).unwrap();
        for (key, change) in changes.as_object().unwrap() {
            value[key] = change.clone();
        }
        let json = serde_json::to_string(&[value]).unwrap();
        parse_pull_requests(&json).unwrap().remove(0)
    }

    #[test]
    fn a_pull_request_is_read_from_ghs_list() {
        let pr = listed(r#"{"reviewDecision": "REVIEW_REQUIRED"}"#);
        assert_eq!(pr.number, 57);
        assert_eq!(pr.author, "ana");
        assert_eq!(pr.branch, "fix-login");
        assert_eq!(pr.local_branch, "fix-login");
        assert!(!pr.from_fork);
        assert_eq!(pr.review, Review::Required);
        assert_eq!(pr.checks, Checks::None);
        assert_eq!(pr.label(), "#57");
    }

    #[test]
    fn a_failing_run_or_status_fails_the_checks() {
        let run = listed(
            r#"{"statusCheckRollup": [
                {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "FAILURE"}
            ]}"#,
        );
        assert_eq!(run.checks, Checks::Failed);
        let status = listed(r#"{"statusCheckRollup": [{"state": "ERROR"}]}"#);
        assert_eq!(status.checks, Checks::Failed);
        let running = listed(
            r#"{"statusCheckRollup": [{"status": "IN_PROGRESS", "conclusion": null},
                {"state": "SUCCESS"}]}"#,
        );
        assert_eq!(running.checks, Checks::Running);
        let skipped = listed(
            r#"{"statusCheckRollup": [{"status": "COMPLETED", "conclusion": "SKIPPED"},
                {"status": "COMPLETED", "conclusion": "NEUTRAL"}]}"#,
        );
        assert_eq!(skipped.checks, Checks::Passed);
    }

    #[test]
    fn a_forks_branch_goes_under_its_owner() {
        let fork = listed(
            r#"{"headRefName": "main", "isCrossRepository": true,
                "headRepositoryOwner": {"login": "ana"}}"#,
        );
        assert!(fork.from_fork);
        assert_eq!(fork.branch, "main");
        assert_eq!(fork.local_branch, "ana/main");
        let gone = listed(
            r#"{"headRefName": "main", "isCrossRepository": true, "headRepositoryOwner": null}"#,
        );
        assert_eq!(gone.local_branch, "pr-57/main");
    }

    #[test]
    fn a_pull_request_with_fields_missing_or_null_still_reads() {
        let json = r#"[{"number": 3, "title": null, "url": "https://github.com/a/b/pull/3",
            "statusCheckRollup": null, "author": null}]"#;
        let pr = parse_pull_requests(json).unwrap().remove(0);
        assert_eq!((pr.title.as_str(), pr.author.as_str()), ("", ""));
        assert_eq!(pr.checks, Checks::None);
        assert!(parse_pull_requests("not json").is_err());
    }

    #[test]
    fn a_pull_request_read_whole_has_its_checks_and_conversation_in_order() {
        let json = r#"{
            "baseRefName": "main",
            "body": "Sends you home after login.",
            "statusCheckRollup": [
                {"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"__typename": "CheckRun", "name": "test", "status": "IN_PROGRESS", "conclusion": ""},
                {"__typename": "StatusContext", "context": "ci/lint", "state": "FAILURE"}
            ],
            "comments": [
                {"author": {"login": "bo"}, "body": "Does it keep the query string?", "createdAt": "2026-10-02T10:00:00Z"}
            ],
            "reviews": [
                {"author": {"login": "cy"}, "body": "", "state": "APPROVED", "submittedAt": "2026-10-02T11:00:00Z"},
                {"author": {"login": "di"}, "body": "", "state": "COMMENTED", "submittedAt": "2026-10-02T09:00:00Z"},
                {"author": {"login": "ed"}, "body": "Not this way.", "state": "CHANGES_REQUESTED", "submittedAt": "2026-10-02T08:00:00Z"}
            ]
        }"#;
        let read = parse_pull_request(json).unwrap();
        assert_eq!(read.base, "main");
        assert_eq!(read.body, "Sends you home after login.");
        let checks: Vec<(&str, CheckState)> = read
            .checks
            .iter()
            .map(|check| (check.name.as_str(), check.state))
            .collect();
        assert_eq!(
            checks,
            [
                ("build", CheckState::Passed),
                ("test", CheckState::Running),
                ("ci/lint", CheckState::Failed)
            ]
        );
        let said: Vec<(&str, Option<Review>)> = read
            .conversation
            .iter()
            .map(|comment| (comment.author.as_str(), comment.verdict))
            .collect();
        assert_eq!(
            said,
            [
                ("ed", Some(Review::ChangesRequested)),
                ("bo", None),
                ("cy", Some(Review::Approved))
            ]
        );
    }

    #[test]
    fn issues_read_from_gh_keep_their_labels_and_author() {
        let json = r#"[{"number": 42, "title": "Login loops", "labels": [{"name": "bug"}],
            "updatedAt": "2026-10-02T09:30:00Z", "author": {"login": "ana"},
            "url": "https://github.com/acme/app/issues/42"}]"#;
        let issues = parse_issues(json).unwrap();
        assert_eq!(issues[0].labels, ["bug"]);
        assert_eq!(issues[0].author, "ana");
        let read = parse_issue(
            r#"{"body": "It loops.", "comments": [{"author": {"login": "bo"},
                "body": "Me too.", "createdAt": "2026-10-02T10:00:00Z"}]}"#,
        )
        .unwrap();
        assert_eq!(read.body, "It loops.");
        assert_eq!(read.comments[0].author, "bo");
        assert_eq!(read.comments[0].body, "Me too.");
    }
}
