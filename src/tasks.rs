//! Tasks: sessions started with something to do. A task is numbered as
//! it's made, `t12`, and stays open until its agent closes it with `crystal
//! done`, the user closes or cancels it, or, for a background task, its run
//! ends. Its session ending while it's open fails it, and killing the
//! session cancels it. Once closed, it goes into its project's history:
//! what it was asked to do, how it went, and where it ran. The daemon adds
//! a row to the project's history in the database as each task closes, so
//! the history outlives the sessions it happened in.
//!
//! A task can also be made to start later (`--no-launch`): the daemon keeps
//! it in the database until it's started, with nothing working on it.
//!
//! Beside its goal, a task can carry acceptance criteria, which its agent
//! is given under its goal in its first prompt, and the pull request and
//! the issue it's about, which its agent is told of with crystal's notes
//! (see [`TaskBrief`]).
//!
//! Everything tasks add to crystal goes through [`enabled`], so they can be
//! switched off as one.

use crate::catalog;
use crate::config::Config;
use crate::git::Checkout;
use crate::plugins;
use crate::project;
use crate::protocol::{ForgeLink, PendingTask, TaskBrief, TaskRecord, TaskStart};
use anyhow::{Result, ensure};
use std::fs;
use std::path::Path;

/// Whether tasks are on: the `tasks` plugin.
pub fn enabled(config: &Config) -> bool {
    plugins::enabled(config, "tasks")
}

/// Refuses a command that's only about tasks while they're off.
pub fn ensure_enabled(config: &Config) -> Result<()> {
    plugins::ensure_enabled(config, "tasks")
}

/// The most a prompt crystal puts together to start an agent may be: a flow
/// step's, or what an agent is told on top of its task. It may go on the
/// agent's command line, so it's kept well within what one can hold.
pub const MAX_PROMPT_BYTES: usize = 16 * 1024;

/// What an agent given a task is told about it: how to close it, and, with
/// `backlog` on, where to put what it notices for later.
pub fn instructions(backlog: bool) -> String {
    let mut text = "What you were asked to do is a task, and crystal shows the user \
                    whether it's still open. Once you've done it, the last step is to \
                    close it: run `crystal done \"<one line on what you did>\"`. If you \
                    can't do it, run `crystal done --failed \"<why>\"` instead."
        .to_string();
    if backlog {
        text.push_str(
            " Anything you notice that's worth doing later but isn't part of this \
             task, put on the project's backlog with `crystal backlog add \"<what>\"` \
             rather than doing it now.",
        );
    }
    text
}

/// What an agent is told when it ends a turn with its task still open, once
/// a task: agents don't always remember to close theirs, Haiku least of all.
/// One that isn't through, say because it's waiting on the user, is told to
/// leave it open.
pub const REMINDER: &str = "Your crystal task is still open. If you've done it, close it now: \
                            run `crystal done \"<one line on what you did>\"`, or `crystal \
                            done --failed \"<why>\"` if you couldn't do it. If you aren't \
                            through, say you're waiting on the user, leave it open and end \
                            your turn.";

/// The most a task's acceptance criteria may come to, all together: they
/// go on the agent's command line with its goal.
pub const MAX_CRITERIA_BYTES: usize = 8 * 1024;

/// A task's acceptance criteria as given: each one trimmed, and those that
/// say nothing left out. Refuses more than [`MAX_CRITERIA_BYTES`].
pub fn criteria(given: impl IntoIterator<Item = String>) -> Result<Vec<String>> {
    let criteria: Vec<String> = given
        .into_iter()
        .map(|criterion| criterion.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|criterion| !criterion.is_empty())
        .collect();
    let bytes: usize = criteria.iter().map(String::len).sum();
    ensure!(
        bytes <= MAX_CRITERIA_BYTES,
        "the acceptance criteria come to {bytes} bytes: they can be {MAX_CRITERIA_BYTES} at most"
    );
    Ok(criteria)
}

/// The acceptance criteria in a file: a line each, a list's `-`, `*` or
/// `[ ]` taken off, the blank lines and `#` headings left out.
pub fn criteria_in(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let line = line
                .strip_prefix("- ")
                .or_else(|| line.strip_prefix("* "))
                .unwrap_or(line)
                .trim_start();
            let line = line
                .strip_prefix("[ ]")
                .or_else(|| line.strip_prefix("[x]"))
                .unwrap_or(line);
            line.trim().to_string()
        })
        .collect()
}

/// What an agent is asked to do: `goal`, with its acceptance criteria
/// under it when it has some.
pub fn with_criteria(goal: &str, accept: &[String]) -> String {
    if accept.is_empty() {
        return goal.to_string();
    }
    let mut prompt = format!("{}\n\nAcceptance criteria:", goal.trim_end());
    for criterion in accept {
        prompt.push_str(&format!("\n- {criterion}"));
    }
    prompt
}

/// `command` with the acceptance criteria under `goal`, its agent's first
/// prompt, where it is in the command. A command whose first prompt isn't
/// `goal` stays as it is.
pub fn with_criteria_in(mut command: Vec<String>, goal: &str, accept: &[String]) -> Vec<String> {
    if accept.is_empty() {
        return command;
    }
    if let Some(at) = catalog::first_prompt_at(&command)
        && command[at] == goal
    {
        command[at] = with_criteria(goal, accept);
    }
    command
}

/// The goal of a task on a pull request or an issue that wasn't given
/// one: to work on the pull request, or else to fix the issue, in the
/// words the TUI uses when it starts a session on either.
pub fn goal_for(brief: &TaskBrief) -> Option<String> {
    if let Some(pull_request) = &brief.pull_request {
        let forge = pull_request.forge;
        return Some(format!(
            "Work on {} {}: {} ({})",
            forge.pull_request(),
            forge.label(pull_request.number),
            pull_request.title,
            pull_request.url
        ));
    }
    let issue = brief.issue.as_ref()?;
    Some(format!(
        "Fix issue #{}: {} ({})",
        issue.number, issue.title, issue.url
    ))
}

/// What an agent in `worktree` is told of the pull request and the issue
/// its session is about, a paragraph each: what to read first, and to keep
/// to it. Its task's, when it was started as a `task`, or else the
/// session's, which it's told each time it starts all the same. `None` for
/// a session about neither.
pub fn forge_notes(brief: &TaskBrief, worktree: &Path, task: bool) -> Option<String> {
    let mut notes = Vec::new();
    let what = if task { "Your task" } else { "This session" };
    if let Some(pull_request) = &brief.pull_request {
        notes.push(pull_request_note(pull_request, worktree, what));
    }
    if let Some(issue) = &brief.issue {
        notes.push(issue_note(issue, what));
    }
    (!notes.is_empty()).then(|| notes.join("\n\n"))
}

fn pull_request_note(pull_request: &ForgeLink, worktree: &Path, what_is: &str) -> String {
    let forge = pull_request.forge;
    let what = forge.pull_request();
    let view = match forge {
        crate::forge::Forge::GitHub => "pr",
        crate::forge::Forge::GitLab => "mr",
    };
    let branch = match &pull_request.branch {
        Some(branch) => format!(", on its branch `{branch}`"),
        None => String::new(),
    };
    format!(
        "{what_is} is about {} {what} {}, \"{}\" ({}). You're in its worktree, {}{branch}: \
         read the {what} and its conversation first (`{} {view} view {} --comments`), then do \
         every edit, test, commit and push there, and keep to what the {what} needs. Other \
         sessions may work in this worktree too, so pull before you push.",
        forge.name(),
        forge.label(pull_request.number),
        pull_request.title,
        pull_request.url,
        worktree.display(),
        forge.cli(),
        pull_request.number,
    )
}

fn issue_note(issue: &ForgeLink, what_is: &str) -> String {
    let forge = issue.forge;
    format!(
        "{what_is} is for {} issue #{}, \"{}\" ({}): read it first (`{} issue view {} \
         --comments`), then keep to what resolves it. Mention it in your commit messages, and \
         close it from the {} that fixes it (`Closes #{}`).",
        forge.name(),
        issue.number,
        issue.title,
        issue.url,
        forge.cli(),
        issue.number,
        forge.pull_request(),
        issue.number,
    )
}

/// A task waiting to start, as `crystal tasks` lists it.
pub fn pending_record(task: &PendingTask) -> TaskRecord {
    let worktree = Checkout::find(&task.cwd).map(|checkout| checkout.worktree());
    let project = match &worktree {
        Some(worktree) => worktree.project.clone(),
        None => project::of(&task.cwd).name,
    };
    TaskRecord {
        id: Some(task.id),
        goal: task.goal.clone(),
        session: String::new(),
        project,
        branch: worktree.and_then(|worktree| worktree.branch),
        background: matches!(task.start, TaskStart::Background { .. }),
        backlog: task.backlog,
        pending: true,
        waiting: false,
        created: task.created,
        outcome: None,
        artifacts: Vec::new(),
        brief: task.brief.clone(),
    }
}

/// The number a task is given as on the command line: `t12`, or just `12`.
pub fn parse_id(text: &str) -> Option<u64> {
    text.strip_prefix('t').unwrap_or(text).parse().ok()
}

/// The file a project's closed tasks were kept in before the database, in
/// its directory: one JSON line each.
pub const OLD_FILE: &str = "tasks.jsonl";

/// The closed tasks kept in the project directory `dir` before the
/// database, in the order they closed. A line that can't be read, say one
/// cut short by a crash, is left out.
pub fn load_old(dir: &Path) -> Vec<TaskRecord> {
    let Ok(text) = fs::read_to_string(dir.join(OLD_FILE)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TaskState;

    #[test]
    fn a_task_waiting_to_start_is_listed_as_pending() {
        let task = PendingTask {
            id: 4,
            goal: "later".into(),
            cwd: "/nowhere".into(),
            name: None,
            start: TaskStart::Agent {
                command: vec!["claude".into(), "later".into()],
            },
            backlog: None,
            created: 1,
            brief: Default::default(),
        };
        let record = pending_record(&task);
        assert_eq!(record.state(), TaskState::Pending);
        assert_eq!(record.id, Some(4));
        assert_eq!(record.project, "nowhere");
        assert!(record.session.is_empty());
    }

    #[test]
    fn criteria_are_given_one_by_one_or_a_line_each_in_a_file() {
        let given = ["  the tests pass ".to_string(), String::new()];
        assert_eq!(criteria(given).unwrap(), ["the tests pass"]);
        let file =
            "# Done when\n\n- the tests pass\n* the docs say so\n- [ ] no warnings\n[x] fast\n";
        assert_eq!(
            criteria_in(file),
            ["the tests pass", "the docs say so", "no warnings", "fast"]
        );
        let long = vec!["x".repeat(MAX_CRITERIA_BYTES + 1)];
        assert!(criteria(long).is_err());
    }

    #[test]
    fn criteria_go_under_the_goal_in_the_first_prompt() {
        let accept = ["the tests pass".to_string(), "no warnings".to_string()];
        assert_eq!(
            with_criteria("Fix the tests\n", &accept),
            "Fix the tests\n\nAcceptance criteria:\n- the tests pass\n- no warnings"
        );
        assert_eq!(with_criteria("Fix the tests", &[]), "Fix the tests");

        let command =
            |args: &[&str]| -> Vec<String> { args.iter().map(|a| a.to_string()).collect() };
        assert_eq!(
            with_criteria_in(command(&["claude", "Fix it"]), "Fix it", &accept[..1]),
            command(&["claude", "Fix it\n\nAcceptance criteria:\n- the tests pass"])
        );
        // A first prompt that isn't the goal is left alone.
        let other = command(&["claude", "Something else"]);
        assert_eq!(with_criteria_in(other.clone(), "Fix it", &accept), other);
    }

    fn link(forge: crate::forge::Forge, number: u64, branch: Option<&str>) -> Box<ForgeLink> {
        Box::new(ForgeLink {
            forge,
            number,
            title: "Fix the login redirect".into(),
            url: format!("https://example.com/{number}"),
            branch: branch.map(String::from),
        })
    }

    #[test]
    fn a_task_on_a_pull_request_or_an_issue_is_to_work_on_it_unless_told() {
        use crate::forge::Forge;
        let on_pull_request = TaskBrief {
            pull_request: Some(link(Forge::GitLab, 57, Some("fix-login"))),
            issue: Some(link(Forge::GitLab, 7, None)),
            ..TaskBrief::default()
        };
        assert_eq!(
            goal_for(&on_pull_request).unwrap(),
            "Work on merge request !57: Fix the login redirect (https://example.com/57)"
        );
        let on_issue = TaskBrief {
            issue: Some(link(Forge::GitHub, 7, None)),
            ..TaskBrief::default()
        };
        assert_eq!(
            goal_for(&on_issue).unwrap(),
            "Fix issue #7: Fix the login redirect (https://example.com/7)"
        );
        assert_eq!(goal_for(&TaskBrief::default()), None);
    }

    #[test]
    fn its_agent_is_told_to_read_the_pull_request_and_the_issue_first() {
        use crate::forge::Forge;
        let brief = TaskBrief {
            pull_request: Some(link(Forge::GitHub, 57, Some("fix-login"))),
            issue: Some(link(Forge::GitHub, 7, None)),
            ..TaskBrief::default()
        };
        let notes = forge_notes(&brief, Path::new("/work/app-fix-login"), true).unwrap();
        let (pull_request, issue) = notes.split_once("\n\n").unwrap();
        assert!(
            pull_request.starts_with(
                "Your task is about GitHub pull request #57, \"Fix the login redirect\" \
                 (https://example.com/57). You're in its worktree, /work/app-fix-login, on its \
                 branch `fix-login`"
            ),
            "{pull_request}"
        );
        assert!(
            pull_request.contains("`gh pr view 57 --comments`"),
            "{pull_request}"
        );
        assert!(issue.contains("`gh issue view 7 --comments`"), "{issue}");
        assert!(issue.contains("(`Closes #7`)"), "{issue}");

        let on_gitlab = TaskBrief {
            pull_request: Some(link(Forge::GitLab, 57, None)),
            ..TaskBrief::default()
        };
        let notes = forge_notes(&on_gitlab, Path::new("/w"), true).unwrap();
        assert!(notes.contains("GitLab merge request !57"), "{notes}");
        assert!(notes.contains("`glab mr view 57 --comments`"), "{notes}");
        assert_eq!(
            forge_notes(&TaskBrief::default(), Path::new("/w"), true),
            None
        );

        // A session with no task is told of them all the same.
        let notes = forge_notes(&brief, Path::new("/w"), false).unwrap();
        assert!(
            notes.starts_with("This session is about GitHub pull request #57"),
            "{notes}"
        );
        assert!(
            notes.contains("\n\nThis session is for GitHub issue #7"),
            "{notes}"
        );
    }

    #[test]
    fn an_id_is_given_with_its_t_or_without() {
        assert_eq!(parse_id("t12"), Some(12));
        assert_eq!(parse_id("12"), Some(12));
        assert_eq!(parse_id("fixer"), None);
        assert_eq!(parse_id("t"), None);
    }

    #[test]
    fn a_record_from_before_tasks_had_numbers_still_reads() {
        let old = r#"{"goal":"x","session":"s","project":"p","outcome":{"failed":true,"summary":"no","closed":3}}"#;
        let record: TaskRecord = serde_json::from_str(old).unwrap();
        assert_eq!(record.id, None);
        assert_eq!(record.state(), TaskState::Failed);
    }

    #[test]
    fn a_broken_line_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let kept = r#"{"goal": "kept", "session": "claude", "project": "payments"}"#;
        let text = format!("{kept}\n{{\"goal\": \"cut sh");
        fs::write(dir.path().join(OLD_FILE), text).unwrap();
        let goals: Vec<String> = load_old(dir.path())
            .into_iter()
            .map(|task| task.goal)
            .collect();
        assert_eq!(goals, ["kept"]);
        assert!(load_old(&dir.path().join("none")).is_empty());
    }
}
