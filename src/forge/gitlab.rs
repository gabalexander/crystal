//! GitLab, through `glab`: each of [`Repo`](super::Repo)'s calls as a `glab`
//! command, and what it answers as JSON read into the same types as
//! GitHub's, a merge request as a pull request.
//!
//! Where GitLab's answers differ: a number is a merge request's `iid`, a
//! link its `web_url`, a person their `username`, and a description its
//! `description`. The list carries no checks or reviews, though it does
//! say whether one has conflicts; reading one merge
//! request gives its head pipeline, as its one check, and its notes, where
//! an approval is a note GitLab writes itself, `approved this merge
//! request`. A fork's owner isn't named, so its worktree's branch is
//! `mr-57/<branch>`.

use super::{
    Check, CheckState, Checks, Comment, Forge, Issue, IssueDetail, LIMIT, MERGED_LIMIT,
    PullRequest, PullRequestDetail, Review, Topic, list, parse, run, text,
};
use serde::Deserialize;
use std::path::Path;

const FORGE: Forge = Forge::GitLab;

pub(super) fn pull_requests(dir: &Path) -> Result<Vec<PullRequest>, String> {
    let limit = LIMIT.to_string();
    let json = glab(dir, &["mr", "list", "-F", "json", "-P", &limit])?;
    parse_merge_requests(&json)
}

/// The merge requests merged lately.
pub(super) fn merged_pull_requests(dir: &Path) -> Result<Vec<PullRequest>, String> {
    let limit = MERGED_LIMIT.to_string();
    let args = ["mr", "list", "--merged", "-F", "json", "-P", &limit];
    parse_merge_requests(&glab(dir, &args)?)
}

pub(super) fn pull_request(dir: &Path, number: u64) -> Result<PullRequestDetail, String> {
    let json = with_comments(dir, &["mr", "view", &number.to_string(), "-F", "json"])?;
    parse_merge_request(&json)
}

pub(super) fn diff(dir: &Path, number: u64) -> Result<String, String> {
    let number = number.to_string();
    glab(dir, &["mr", "diff", &number, "--raw", "--color=never"])
}

pub(super) fn issues(dir: &Path) -> Result<Vec<Issue>, String> {
    // `issue list` takes its JSON switch as `-O`: its `-F` is another.
    let limit = LIMIT.to_string();
    let json = glab(dir, &["issue", "list", "-O", "json", "-P", &limit])?;
    parse_issues(&json)
}

pub(super) fn issue(dir: &Path, number: u64) -> Result<IssueDetail, String> {
    let json = with_comments(dir, &["issue", "view", &number.to_string(), "-F", "json"])?;
    parse_issue(&json)
}

/// Posts `body` on `topic`. A merge request's note is given on glab's
/// standard input; an issue's has no way in but the command line, as one
/// argument with its option, so that it's never mistaken for an option
/// however it starts.
pub(super) fn comment(dir: &Path, topic: Topic, body: &str) -> Result<(), String> {
    match topic {
        Topic::PullRequest(number) => {
            let number = number.to_string();
            run(FORGE, dir, &["mr", "note", "create", &number], Some(body))?;
        }
        Topic::Issue(number) => {
            let message = format!("--message={body}");
            glab(dir, &["issue", "note", &number.to_string(), &message])?;
        }
    }
    Ok(())
}

/// The new title is one argument with its option, and the description
/// goes on glab's standard input.
pub(super) fn edit_issue(dir: &Path, number: u64, title: &str, body: &str) -> Result<(), String> {
    let number = number.to_string();
    let title = format!("--title={title}");
    let args = [
        "issue",
        "update",
        &number,
        &title,
        "--description-file",
        "-",
    ];
    run(FORGE, dir, &args, Some(body))?;
    Ok(())
}

fn glab(dir: &Path, args: &[&str]) -> Result<String, String> {
    run(FORGE, dir, args, None)
}

/// `args` with `--comments`, for what's been said too; or, when glab
/// refuses that, without. GitLab wants a login to read notes, even on a
/// project anyone can see, and the rest is still worth showing.
fn with_comments(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut with = args.to_vec();
    with.push("--comments");
    glab(dir, &with).or_else(|_| glab(dir, args))
}

#[derive(Deserialize)]
struct User {
    #[serde(default, deserialize_with = "text")]
    username: String,
}

fn username(user: Option<User>) -> String {
    user.map(|user| user.username).unwrap_or_default()
}

#[derive(Deserialize)]
struct ListedMergeRequest {
    iid: u64,
    #[serde(default, deserialize_with = "text")]
    title: String,
    #[serde(default)]
    author: Option<User>,
    #[serde(default, deserialize_with = "text")]
    source_branch: String,
    #[serde(default)]
    draft: bool,
    /// What an older GitLab calls a draft.
    #[serde(default)]
    work_in_progress: bool,
    #[serde(default)]
    has_conflicts: bool,
    #[serde(default)]
    source_project_id: Option<u64>,
    #[serde(default)]
    target_project_id: Option<u64>,
    #[serde(default, deserialize_with = "text")]
    updated_at: String,
    #[serde(default, deserialize_with = "text")]
    web_url: String,
}

fn parse_merge_requests(json: &str) -> Result<Vec<PullRequest>, String> {
    let listed: Vec<ListedMergeRequest> = parse(FORGE, json)?;
    Ok(listed.into_iter().map(pull_request_of).collect())
}

fn pull_request_of(listed: ListedMergeRequest) -> PullRequest {
    let draft = listed.draft || listed.work_in_progress;
    let from_fork = match (listed.source_project_id, listed.target_project_id) {
        (Some(source), Some(target)) => source != target,
        _ => false,
    };
    let local_branch = if from_fork {
        format!("mr-{}/{}", listed.iid, listed.source_branch)
    } else {
        listed.source_branch.clone()
    };
    PullRequest {
        forge: FORGE,
        number: listed.iid,
        title: without_draft(&listed.title, draft),
        author: username(listed.author),
        branch: listed.source_branch,
        from_fork,
        local_branch,
        draft,
        conflicts: listed.has_conflicts,
        merged: false,
        checks: Checks::None,
        review: Review::None,
        updated_at: listed.updated_at,
        url: listed.web_url,
    }
}

/// A draft's title without the `Draft:` GitLab keeps in it: crystal says
/// it's a draft its own way.
fn without_draft(title: &str, draft: bool) -> String {
    let prefixes = ["Draft:", "[Draft]", "(Draft)", "WIP:", "[WIP]"];
    let rest = prefixes
        .iter()
        .find_map(|prefix| title.strip_prefix(prefix));
    match rest {
        Some(rest) if draft => rest.trim_start().to_string(),
        _ => title.to_string(),
    }
}

#[derive(Deserialize)]
struct ReadMergeRequest {
    #[serde(default, deserialize_with = "text")]
    target_branch: String,
    #[serde(default, deserialize_with = "text")]
    description: String,
    #[serde(default)]
    head_pipeline: Option<Pipeline>,
    /// Its threads of notes, with `--comments`.
    #[serde(rename = "Discussions", default, deserialize_with = "list")]
    discussions: Vec<Discussion>,
}

#[derive(Deserialize)]
struct Pipeline {
    #[serde(default, deserialize_with = "text")]
    status: String,
}

impl Pipeline {
    /// A cancelled pipeline didn't pass; anything not over yet, waiting
    /// on someone to start a job by hand too, is still running.
    fn into_check(self) -> Check {
        let state = match self.status.as_str() {
            "success" => CheckState::Passed,
            "skipped" => CheckState::Skipped,
            "failed" | "canceled" | "canceling" => CheckState::Failed,
            _ => CheckState::Running,
        };
        Check {
            name: "pipeline".to_string(),
            state,
        }
    }
}

#[derive(Deserialize)]
struct Discussion {
    #[serde(default, deserialize_with = "list")]
    notes: Vec<Note>,
}

#[derive(Deserialize)]
struct Note {
    #[serde(default)]
    author: Option<User>,
    #[serde(default, deserialize_with = "text")]
    body: String,
    #[serde(default, deserialize_with = "text")]
    created_at: String,
    /// Whether GitLab wrote it itself, about something done, like a commit
    /// pushed or a label added.
    #[serde(default)]
    system: bool,
}

impl Note {
    /// The note as part of the conversation: what someone wrote, or an
    /// approval, GitLab's verdict on a merge request. The rest of what
    /// GitLab writes itself is what happened, not what was said.
    fn into_comment(self) -> Option<Comment> {
        let verdict = if !self.system {
            None
        } else if self.body.starts_with("approved this merge request") {
            Some(Review::Approved)
        } else if self.body.starts_with("requested changes") {
            Some(Review::ChangesRequested)
        } else {
            return None;
        };
        let body = if verdict.is_some() {
            String::new()
        } else {
            self.body
        };
        Some(Comment {
            author: username(self.author),
            at: self.created_at,
            body,
            verdict,
        })
    }
}

/// The notes that are part of the conversation, the oldest first.
fn conversation(notes: impl IntoIterator<Item = Note>) -> Vec<Comment> {
    let mut comments: Vec<Comment> = notes.into_iter().filter_map(Note::into_comment).collect();
    comments.sort_by(|a, b| a.at.cmp(&b.at));
    comments
}

fn parse_merge_request(json: &str) -> Result<PullRequestDetail, String> {
    let read: ReadMergeRequest = parse(FORGE, json)?;
    let notes = read
        .discussions
        .into_iter()
        .flat_map(|discussion| discussion.notes);
    Ok(PullRequestDetail {
        base: read.target_branch,
        body: read.description,
        checks: read
            .head_pipeline
            .map(Pipeline::into_check)
            .into_iter()
            .collect(),
        conversation: conversation(notes),
    })
}

#[derive(Deserialize)]
struct ListedIssue {
    iid: u64,
    #[serde(default, deserialize_with = "text")]
    title: String,
    /// Plain names, unlike GitHub's.
    #[serde(default, deserialize_with = "list")]
    labels: Vec<String>,
    #[serde(default, deserialize_with = "text")]
    updated_at: String,
    #[serde(default)]
    author: Option<User>,
    #[serde(default, deserialize_with = "text")]
    web_url: String,
}

fn parse_issues(json: &str) -> Result<Vec<Issue>, String> {
    let listed: Vec<ListedIssue> = parse(FORGE, json)?;
    let issues = listed.into_iter().map(|issue| Issue {
        number: issue.iid,
        title: issue.title,
        labels: issue.labels,
        updated_at: issue.updated_at,
        author: username(issue.author),
        url: issue.web_url,
    });
    Ok(issues.collect())
}

#[derive(Deserialize)]
struct ReadIssue {
    #[serde(default, deserialize_with = "text")]
    description: String,
    /// Its notes, with `--comments`: one list, not threads.
    #[serde(rename = "Notes", default, deserialize_with = "list")]
    notes: Vec<Note>,
}

fn parse_issue(json: &str) -> Result<IssueDetail, String> {
    let read: ReadIssue = parse(FORGE, json)?;
    Ok(IssueDetail {
        body: read.description,
        comments: conversation(read.notes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTED: &str = r#"[{
        "iid": 57, "title": "Draft: Fix the login redirect", "draft": true,
        "author": {"username": "ana"}, "source_branch": "fix-login",
        "source_project_id": 7, "target_project_id": 7,
        "updated_at": "2026-10-02T09:30:00.123Z",
        "web_url": "https://gitlab.com/acme/app/-/merge_requests/57",
        "has_conflicts": false, "detailed_merge_status": "draft_status"
    }, {
        "iid": 58, "title": "Use the new API", "draft": false,
        "author": {"username": "bo"}, "source_branch": "main",
        "source_project_id": 9, "target_project_id": 7,
        "updated_at": "2026-10-01T09:30:00Z",
        "web_url": "https://gitlab.com/acme/app/-/merge_requests/58",
        "has_conflicts": true
    }]"#;

    #[test]
    fn merge_requests_are_read_as_pull_requests() {
        let listed = parse_merge_requests(LISTED).unwrap();
        let draft = &listed[0];
        assert_eq!(draft.forge, Forge::GitLab);
        assert_eq!(draft.label(), "!57");
        assert_eq!(draft.title, "Fix the login redirect");
        assert!(draft.draft);
        assert_eq!(draft.author, "ana");
        assert_eq!(draft.local_branch, "fix-login");
        assert_eq!(draft.checks, Checks::None);
        assert!(!draft.conflicts);
        let fork = &listed[1];
        assert!(fork.from_fork);
        assert!(fork.conflicts);
        assert_eq!(fork.local_branch, "mr-58/main");
        assert_eq!(
            fork.checkout(Path::new("/code/app")).fetch,
            "refs/merge-requests/58/head"
        );
    }

    #[test]
    fn a_merge_request_read_whole_has_its_pipeline_and_what_people_said() {
        let json = r#"{
            "iid": 57, "target_branch": "main", "description": null,
            "head_pipeline": {"id": 3, "status": "failed"},
            "Discussions": [
                {"notes": [
                    {"author": {"username": "bo"}, "body": "Why here?", "created_at": "2026-10-02T10:00:00Z", "system": false},
                    {"author": {"username": "ana"}, "body": "Because.", "created_at": "2026-10-02T10:05:00Z", "system": false}
                ]},
                {"notes": [{"author": {"username": "ana"}, "body": "added 2 commits", "created_at": "2026-10-02T09:00:00Z", "system": true}]},
                {"notes": [{"author": {"username": "cy"}, "body": "approved this merge request", "created_at": "2026-10-02T11:00:00Z", "system": true}]}
            ]
        }"#;
        let read = parse_merge_request(json).unwrap();
        assert_eq!(read.base, "main");
        assert_eq!(read.body, "");
        assert_eq!(
            read.checks,
            [Check {
                name: "pipeline".into(),
                state: CheckState::Failed
            }]
        );
        let said: Vec<(&str, &str, Option<Review>)> = read
            .conversation
            .iter()
            .map(|c| (c.author.as_str(), c.body.as_str(), c.verdict))
            .collect();
        assert_eq!(
            said,
            [
                ("bo", "Why here?", None),
                ("ana", "Because.", None),
                ("cy", "", Some(Review::Approved))
            ]
        );
    }

    #[test]
    fn a_merge_request_read_without_its_notes_still_reads() {
        let read =
            parse_merge_request(r#"{"description": "Fixes it.", "head_pipeline": null}"#).unwrap();
        assert_eq!(read.body, "Fixes it.");
        assert!(read.checks.is_empty() && read.conversation.is_empty());
    }

    #[test]
    fn issues_read_from_glab_keep_their_plain_labels() {
        let json = r#"[{"iid": 8, "title": "Rotate keys", "labels": ["bug", "security"],
            "updated_at": "2026-10-03T15:27:57.24Z", "author": {"username": "di"},
            "web_url": "https://gitlab.com/acme/app/-/work_items/8"}]"#;
        let issues = parse_issues(json).unwrap();
        assert_eq!(issues[0].number, 8);
        assert_eq!(issues[0].labels, ["bug", "security"]);
        assert_eq!(issues[0].author, "di");
        let read = parse_issue(
            r#"{"description": "Keys never rotate.", "Notes": [
                {"author": {"username": "bo"}, "body": "added ~bug", "created_at": "2026-10-02T09:00:00Z", "system": true},
                {"author": {"username": "ed"}, "body": "On it.", "created_at": "2026-10-02T10:00:00Z", "system": false}
            ]}"#,
        )
        .unwrap();
        assert_eq!(read.body, "Keys never rotate.");
        assert_eq!(read.comments.len(), 1);
        assert_eq!(read.comments[0].body, "On it.");
    }

    #[test]
    fn only_a_drafts_title_loses_its_draft_prefix() {
        assert_eq!(without_draft("Draft: Fix it", true), "Fix it");
        assert_eq!(without_draft("WIP: Fix it", true), "Fix it");
        assert_eq!(without_draft("Draft: Fix it", false), "Draft: Fix it");
        assert_eq!(without_draft("Fix it", true), "Fix it");
    }
}
