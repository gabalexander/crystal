//! The commands about work: closing a task with `crystal done`, listing
//! tasks with `crystal tasks`, and keeping a project's backlog with
//! `crystal backlog`. The daemon keeps the history and the backlogs; these
//! ask it, and print what it says.

use crate::backlog;
use crate::catalog;
use crate::client::{self, Purpose};
use crate::config::Config;
use crate::env;
use crate::github;
use crate::protocol::{BacklogItem, Request, Response, TaskRecord};
use crate::tasks;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Closes a task: the one of the session this runs in, or the session
/// called `name`.
pub fn done(socket: &Path, name: Option<String>, failed: bool, summary: &str) -> Result<()> {
    tasks::ensure_enabled(&settings())?;
    let id = match &name {
        Some(_) => None,
        None => Some(env::own_session_id(socket).context(
            "this isn't running in a crystal session: say whose task it is with `-n <session>`",
        )?),
    };
    let close = Request::Close {
        id,
        name,
        failed,
        summary: summary.to_string(),
    };
    expect_done(client::ask(socket, &close, false)?, socket)
}

/// Prints the tasks of the project `dir` is in, or every project's with
/// `all`: those still open, then those closed, the latest first.
pub fn list_tasks(socket: &Path, dir: PathBuf, all: bool, json: bool) -> Result<()> {
    tasks::ensure_enabled(&settings())?;
    let tasks = match client::ask(socket, &Request::Tasks { dir, all }, true)? {
        Some(Response::Tasks { tasks }) => tasks,
        _ => bail!("the daemon didn't list the tasks"),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&tasks)?);
    } else {
        print!("{}", task_lines(&tasks, all));
    }
    Ok(())
}

/// Tasks as `crystal tasks` prints them: how each stands, where it ran, what
/// it was asked to do, and how it went. With `all`, the project too.
fn task_lines(tasks: &[TaskRecord], all: bool) -> String {
    let mut text = String::new();
    for task in tasks {
        let stands = match &task.outcome {
            None => "open",
            Some(outcome) if outcome.failed => "failed",
            Some(_) => "done",
        };
        let place = match (&task.branch, all) {
            (Some(branch), true) => format!("{} {branch}", task.project),
            (Some(branch), false) => branch.clone(),
            (None, _) => task.project.clone(),
        };
        let goal = first_line(&task.goal);
        let summary = match &task.outcome {
            Some(outcome) if !outcome.summary.is_empty() => format!(" — {}", outcome.summary),
            _ => String::new(),
        };
        text.push_str(&format!(
            "{stands:<6}  {:<12}  {place}  {goal}{summary}\n",
            task.session
        ));
    }
    text
}

/// What `crystal backlog` can do.
pub enum BacklogAction {
    List { all: bool, json: bool },
    Add { text: String, tags: Vec<String> },
    Mark { number: u64, done: bool },
    Remove { number: u64 },
    Export,
}

/// Does `action` to the backlog of the project `dir` is in.
pub fn change_backlog(socket: &Path, dir: PathBuf, action: BacklogAction) -> Result<()> {
    backlog::ensure_enabled(&settings())?;
    match action {
        BacklogAction::List { all, json } => {
            let backlog = client::backlog(socket, dir, all)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&backlog.items)?);
            } else {
                print!("{}", backlog_lines(&backlog.items));
            }
        }
        BacklogAction::Add { text, tags } => {
            let add = Request::BacklogAdd { dir, text, tags };
            match client::ask(socket, &add, true)? {
                Some(Response::Added { number }) => println!("#{number}"),
                _ => bail!("the daemon didn't add it"),
            }
        }
        BacklogAction::Mark { number, done } => {
            let mark = Request::BacklogMark { dir, number, done };
            expect_done(client::ask(socket, &mark, true)?, socket)?;
        }
        BacklogAction::Remove { number } => {
            let remove = Request::BacklogRemove { dir, number };
            expect_done(client::ask(socket, &remove, true)?, socket)?;
        }
        BacklogAction::Export => {
            let backlog = client::backlog(socket, dir, true)?;
            print!("{}", backlog::markdown(&backlog.project, &backlog.items));
        }
    }
    Ok(())
}

/// Starts a task for backlog item `number`: the agent the new-session
/// panel would start first, asked to do what the item says, here or with
/// `worktree` in a new worktree named after it. Closing the task done ticks
/// the item. Gives back the new session's name.
pub fn start_from_backlog(
    socket: &Path,
    dir: PathBuf,
    number: u64,
    worktree: bool,
    name: Option<String>,
) -> Result<String> {
    let config = settings();
    backlog::ensure_enabled(&config)?;
    let backlog = client::backlog(socket, dir.clone(), true)?;
    let item = backlog
        .items
        .iter()
        .find(|item| item.number == number)
        .with_context(|| format!("there's no #{number} on the backlog"))?;
    if item.done {
        bail!("#{number} is done already: `crystal backlog reopen {number}` first");
    }
    let mut command: Vec<String> = config
        .new_session
        .split_whitespace()
        .map(String::from)
        .collect();
    catalog::add_first_prompt(&mut command, &item.text);
    let cwd = if worktree {
        client::add_worktree(socket, &dir, &github::branch_for_issue(number, &item.text))?
    } else {
        dir
    };
    let purpose = Purpose {
        task: Some(item.text.clone()),
        backlog: Some(number),
    };
    client::new_session_for(socket, name, cwd, command, purpose)
}

/// Backlog items as `crystal backlog` prints them: number, a tick for one
/// that's done, text, tags.
fn backlog_lines(items: &[BacklogItem]) -> String {
    let mut text = String::new();
    for item in items {
        let tick = if item.done { "✓ " } else { "" };
        let tags: String = item.tags.iter().map(|tag| format!("  #{tag}")).collect();
        let number = format!("#{}", item.number);
        text.push_str(&format!(
            "{number:<4}  {tick}{}{tags}\n",
            first_line(&item.text)
        ));
    }
    text
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

fn expect_done(response: Option<Response>, socket: &Path) -> Result<()> {
    match response {
        Some(Response::Done) => Ok(()),
        Some(_) => bail!("the daemon answered something else"),
        None => bail!("no daemon is running on {}", socket.display()),
    }
}

/// The user's settings. One that can't be read leaves the defaults: these
/// commands only ask it whether tasks and the backlog are on.
fn settings() -> Config {
    Config::load().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TaskOutcome;

    fn task(goal: &str, outcome: Option<(bool, &str)>) -> TaskRecord {
        TaskRecord {
            goal: goal.into(),
            session: "claude".into(),
            project: "payments".into(),
            branch: Some("main".into()),
            background: false,
            backlog: None,
            outcome: outcome.map(|(failed, summary)| TaskOutcome {
                failed,
                summary: summary.into(),
                closed: 1,
            }),
        }
    }

    #[test]
    fn a_task_line_says_how_it_stands_where_and_how_it_went() {
        let lines = task_lines(
            &[
                task("fix the tests", None),
                task("write the docs\nall of them", Some((false, "wrote them"))),
                task("ship it", Some((true, "no network"))),
            ],
            false,
        );
        assert_eq!(
            lines,
            "open    claude        main  fix the tests\n\
             done    claude        main  write the docs — wrote them\n\
             failed  claude        main  ship it — no network\n"
        );
        assert!(task_lines(&[task("x", None)], true).contains("payments main"));
    }

    #[test]
    fn a_backlog_line_has_its_number_text_and_tags() {
        let item = |number, text: &str, done, tags: &[&str]| BacklogItem {
            number,
            text: text.into(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            done,
            created: 0,
            closed: None,
        };
        let lines = backlog_lines(&[
            item(1, "write the docs", false, &["docs"]),
            item(12, "fix it", true, &[]),
        ]);
        assert_eq!(lines, "#1    write the docs  #docs\n#12   ✓ fix it\n");
    }
}
