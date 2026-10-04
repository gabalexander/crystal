//! The commands about work: closing a task with `crystal done`, leaving a
//! note for the sessions after with `crystal handoff`, making, listing,
//! showing and cancelling tasks with `crystal tasks`, and keeping a
//! project's backlog with `crystal backlog`. The daemon keeps the tasks,
//! the history, the handoff files and the backlogs; these ask it, and print
//! what it says.

use crate::artifacts;
use crate::backlog;
use crate::catalog;
use crate::client::{self, Purpose};
use crate::config::Config;
use crate::env;
use crate::forge;
use crate::handoff;
use crate::protocol::{
    BacklogItem, PendingTask, Request, Response, State, TaskSpec, TaskStart, TaskState, TaskView,
    task_label,
};
use crate::shell;
use crate::tasks;
use crate::tui::sidebar::ago;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Closes a task: the one of the session this runs in, or the session
/// called `name`, keeping the files at `artifacts` with it.
pub fn done(
    socket: &Path,
    name: Option<String>,
    failed: bool,
    summary: &str,
    artifacts: Vec<PathBuf>,
) -> Result<()> {
    tasks::ensure_enabled(&settings())?;
    let id = own_session(socket, &name, "whose task it is")?;
    // The daemon reads them, wherever it runs.
    let artifacts = artifacts
        .into_iter()
        .map(std::path::absolute)
        .collect::<std::io::Result<Vec<_>>>()?;
    let close = Request::Close {
        id,
        name,
        failed,
        summary: summary.to_string(),
        artifacts,
    };
    expect_done(client::ask(socket, &close, false)?, socket)
}

/// Adds `note` to the handoff file of the worktree the session this runs
/// in works in, or the session called `name`.
pub fn handoff(socket: &Path, name: Option<String>, note: &str) -> Result<()> {
    handoff::ensure_enabled(&settings())?;
    let id = own_session(socket, &name, "whose worktree it's for")?;
    let request = Request::Handoff {
        id,
        name,
        note: note.to_string(),
    };
    expect_done(client::ask(socket, &request, false)?, socket)
}

/// The id of the session this runs in, unless `name` names another: a
/// command for a session that says `what` with `-n` from outside one.
pub fn own_session(socket: &Path, name: &Option<String>, what: &str) -> Result<Option<String>> {
    if name.is_some() {
        return Ok(None);
    }
    let id = env::own_session_id(socket).with_context(|| {
        format!("this isn't running in a crystal session: say {what} with `-n <session>`")
    })?;
    Ok(Some(id))
}

/// Prints the tasks of the project `dir` is in, or every project's with
/// `all`: those still open, then those waiting to start, then those
/// closed, the latest first.
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

/// A task to make with `crystal tasks new`.
pub struct NewTask {
    pub goal: String,
    pub cwd: PathBuf,
    /// Its session's name: `None` names it for its goal, or else after its
    /// program.
    pub name: Option<String>,
    /// Run it in the background, `claude -p` with `claude_args`, rather
    /// than the agent the new-session panel starts first, in a terminal.
    pub background: bool,
    pub claude_args: Vec<String>,
    /// Start it now, rather than leave it waiting to start.
    pub launch: bool,
}

/// Makes a task, and starts it, unless it's to wait. Prints its number.
pub fn new_task(socket: &Path, task: NewTask) -> Result<()> {
    let config = settings();
    tasks::ensure_enabled(&config)?;
    let NewTask {
        goal,
        cwd,
        name,
        background,
        claude_args,
        launch,
    } = task;
    let start = if background {
        TaskStart::Background { args: claude_args }
    } else {
        TaskStart::Agent {
            command: agent_command(&config, &goal),
        }
    };
    if !launch {
        let pending = PendingTask {
            id: 0,
            goal,
            cwd,
            name,
            start,
            backlog: None,
            created: 0,
        };
        let id = client::add_task(socket, pending)?;
        println!("{}", task_label(Some(id)));
        return Ok(());
    }
    let started = match start {
        TaskStart::Agent { command } => {
            let purpose = Purpose {
                task: Some(goal),
                backlog: None,
            };
            client::new_session_for(socket, name, cwd, command, purpose)?
        }
        TaskStart::Background { args } => {
            let spec = TaskSpec { prompt: goal, args };
            client::new_task(socket, name, cwd, spec, None)?
        }
    };
    println!("{}", task_label(started.task));
    Ok(())
}

/// Starts task `id`, which was made to wait. Prints its session's name.
pub fn start_task(socket: &Path, id: &str) -> Result<()> {
    tasks::ensure_enabled(&settings())?;
    let id =
        tasks::parse_id(id).with_context(|| format!("{id} isn't a task's number, like t12"))?;
    let started = client::start_task(socket, id)?;
    println!("{}", started.name);
    Ok(())
}

/// Prints one task, by its number or its session's name: how it stands,
/// where, what it was asked to do, and how it went.
pub fn show_task(socket: &Path, task: &str, json: bool) -> Result<()> {
    tasks::ensure_enabled(&settings())?;
    let request = Request::ShowTask {
        task: task.to_string(),
    };
    let Response::Task(task) = ask_running(socket, &request)? else {
        bail!("the daemon didn't send the task");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&task)?);
    } else {
        print!("{}", task_card(&task, now()));
    }
    Ok(())
}

/// Cancels a task, by its number or its session's name, and stops the
/// session working on it.
pub fn cancel_task(socket: &Path, task: &str) -> Result<()> {
    tasks::ensure_enabled(&settings())?;
    let request = Request::CancelTask {
        task: task.to_string(),
    };
    expect_done(client::ask(socket, &request, false)?, socket)
}

/// Prints what happened to a task: how it stands, then its session's
/// transcript, while the session is still there.
pub fn task_log(socket: &Path, task: &str) -> Result<()> {
    tasks::ensure_enabled(&settings())?;
    let request = Request::TaskLog {
        task: task.to_string(),
    };
    let Response::TaskLog { task, transcript } = ask_running(socket, &request)? else {
        bail!("the daemon didn't send the task's log");
    };
    print!("{}", task_card(&task, now()));
    println!();
    match transcript {
        Some(rows) => print!("{}", transcript_text(&rows)),
        None if task.state == TaskState::Pending => println!("Nothing has worked on it yet."),
        None => println!(
            "Its session, {}, has gone, and its transcript with it.",
            task.record.session
        ),
    }
    Ok(())
}

/// Tasks as `crystal tasks` prints them: each one's number, how it stands,
/// its session, where it ran, what it was asked to do, and how it went.
/// With `all`, the project too.
fn task_lines(tasks: &[TaskView], all: bool) -> String {
    let mut text = String::new();
    for task in tasks {
        let record = &task.record;
        let place = match (&record.branch, all) {
            (Some(branch), true) => format!("{} {branch}", record.project),
            (Some(branch), false) => branch.clone(),
            (None, _) => record.project.clone(),
        };
        let session = if record.session.is_empty() {
            "-"
        } else {
            &record.session
        };
        let goal = first_line(&record.goal);
        let summary = match &record.outcome {
            Some(outcome) if !outcome.summary.is_empty() => format!(" — {}", outcome.summary),
            _ => String::new(),
        };
        text.push_str(&format!(
            "{:<4}  {:<9}  {session:<12}  {place}  {goal}{summary}\n",
            task_label(record.id),
            task.state.word(),
        ));
    }
    text
}

/// One task, as `crystal tasks show` prints it.
fn task_card(task: &TaskView, now: u64) -> String {
    let record = &task.record;
    let label = task_label(record.id);
    let mut card = format!(
        "{label}  {}  {}\n",
        task.state.word(),
        first_line(&record.goal)
    );
    let mut line = |name: &str, said: String| card.push_str(&format!("  {name:<8}  {said}\n"));
    match (&record.session[..], &task.session_state) {
        ("", _) => {}
        (session, Some(State::Running)) => line("session", session.to_string()),
        (session, Some(state)) => line("session", format!("{session}, {state}")),
        (session, None) => line("session", format!("{session}, gone")),
    }
    let place = match &record.branch {
        Some(branch) => format!("{} {branch}", record.project),
        None => record.project.clone(),
    };
    line("where", place);
    let how = match (record.background, task.cost_usd) {
        (true, Some(cost)) => format!("in the background, ${cost:.2} so far"),
        (true, None) => "in the background".to_string(),
        (false, _) => "in a terminal".to_string(),
    };
    line("runs", how);
    if let Some(asking) = &task.asking {
        let handle = if record.id.is_some() {
            &label
        } else {
            &record.session
        };
        line(
            "asking",
            format!(
                "{} {}: crystal answer {handle} y|n|always",
                asking.tool, asking.gist
            ),
        );
    }
    if let Some(number) = record.backlog {
        line("backlog", format!("#{number}"));
    }
    if record.created > 0 {
        line("made", when(record.created, now));
    }
    if let Some(outcome) = &record.outcome {
        let summary = if outcome.summary.is_empty() {
            String::new()
        } else {
            format!(": {}", outcome.summary)
        };
        line(
            "closed",
            format!(
                "{}, {}{summary}",
                when(outcome.closed, now),
                task.state.word()
            ),
        );
    }
    for (index, artifact) in record.artifacts.iter().enumerate() {
        let label = if index == 0 { "kept" } else { "" };
        let path = shell::home_relative(&artifact.path);
        line(
            label,
            format!("{path} ({})", artifacts::size(artifact.bytes)),
        );
    }
    if record.goal.lines().count() > 1 {
        card.push('\n');
        for goal_line in record.goal.lines() {
            card.push_str(&format!("  {goal_line}\n"));
        }
    }
    card
}

/// How long ago `then` was, at `now`: `just now`, `5m ago`.
fn when(then: u64, now: u64) -> String {
    match ago(then, now).as_str() {
        "now" => "just now".to_string(),
        ago => format!("{ago} ago"),
    }
}

/// A session's rows as text: each without its trailing spaces, and no
/// blank rows at the end.
fn transcript_text(rows: &[String]) -> String {
    let rows: Vec<&str> = rows.iter().map(|row| row.trim_end()).collect();
    let end = rows
        .iter()
        .rposition(|row| !row.is_empty())
        .map_or(0, |last| last + 1);
    rows[..end].iter().map(|row| format!("{row}\n")).collect()
}

/// The agent the new-session panel starts first, asked to do `goal`.
fn agent_command(config: &Config, goal: &str) -> Vec<String> {
    let mut command: Vec<String> = config
        .new_session
        .split_whitespace()
        .map(String::from)
        .collect();
    catalog::add_first_prompt(&mut command, goal);
    command
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
    let command = agent_command(&config, &item.text);
    let cwd = if worktree {
        client::add_worktree(
            socket,
            &dir,
            &forge::branch_for_issue(number, &item.text),
            None,
        )?
    } else {
        dir
    };
    let purpose = Purpose {
        task: Some(item.text.clone()),
        backlog: Some(number),
    };
    Ok(client::new_session_for(socket, name, cwd, command, purpose)?.name)
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

/// Asks the daemon, which must be running already: these commands are
/// about tasks it has.
fn ask_running(socket: &Path, request: &Request) -> Result<Response> {
    match client::ask(socket, request, false)? {
        Some(response) => Ok(response),
        None => bail!("no daemon is running on {}", socket.display()),
    }
}

/// Now, in seconds since the Unix epoch.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
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
    use crate::protocol::{Artifact, ArtifactKind, Asking, TaskOutcome, TaskRecord};

    fn task(id: u64, goal: &str, outcome: Option<(TaskState, &str)>) -> TaskView {
        TaskView::of_record(TaskRecord {
            id: Some(id),
            goal: goal.into(),
            session: "claude".into(),
            project: "payments".into(),
            branch: Some("main".into()),
            background: false,
            backlog: None,
            pending: false,
            waiting: false,
            created: 100,
            outcome: outcome.map(|(state, summary)| TaskOutcome::new(state, summary, 1000)),
            artifacts: Vec::new(),
        })
    }

    #[test]
    fn a_task_line_says_how_it_stands_where_and_how_it_went() {
        let lines = task_lines(
            &[
                task(3, "fix the tests", None),
                task(
                    2,
                    "write the docs\nall of them",
                    Some((TaskState::Done, "wrote them")),
                ),
                task(1, "ship it", Some((TaskState::Failed, "no network"))),
            ],
            false,
        );
        assert_eq!(
            lines,
            "t3    running    claude        main  fix the tests\n\
             t2    done       claude        main  write the docs — wrote them\n\
             t1    failed     claude        main  ship it — no network\n"
        );
        assert!(task_lines(&[task(1, "x", None)], true).contains("payments main"));
    }

    #[test]
    fn a_task_waiting_to_start_has_no_session_yet() {
        let mut pending = task(4, "later", None);
        pending.record.session = String::new();
        pending.record.pending = true;
        pending.state = pending.record.state();
        assert!(task_lines(&[pending], false).starts_with("t4    pending    -     "));
    }

    #[test]
    fn a_card_says_what_a_background_task_asks_for_and_what_it_cost() {
        let mut asking = task(7, "fix the tests", None);
        asking.record.background = true;
        asking.session_state = Some(State::Running);
        asking.cost_usd = Some(0.4213);
        asking.asking = Some(Asking {
            tool: "Bash".into(),
            gist: "cargo test".into(),
        });
        let card = task_card(&asking, 160);
        assert!(card.starts_with("t7  running  fix the tests\n"), "{card}");
        assert!(card.contains("  session   claude\n"), "{card}");
        assert!(card.contains("in the background, $0.42 so far"), "{card}");
        assert!(
            card.contains("asking    Bash cargo test: crystal answer t7 y|n|always"),
            "{card}"
        );
        assert!(card.contains("made      1m ago"), "{card}");

        let cancelled = task(
            8,
            "x",
            Some((TaskState::Cancelled, "cancelled by the user")),
        );
        let card = task_card(&cancelled, 1000);
        assert!(card.contains("  session   claude, gone\n"), "{card}");
        assert!(card.contains("closed    just now, cancelled: cancelled by the user"));
    }

    #[test]
    fn a_card_lists_the_files_kept_with_the_task() {
        let mut kept = task(9, "write the plan", Some((TaskState::Done, "wrote it")));
        let artifact = |kind, name: &str, bytes| Artifact {
            kind,
            name: name.into(),
            path: Path::new("/state/tasks/t9").join(name),
            bytes,
        };
        kept.record.artifacts = vec![
            artifact(ArtifactKind::File, "plan.md", 9),
            artifact(ArtifactKind::Handoff, "handoff.md", 3000),
        ];
        let card = task_card(&kept, 1000);
        assert!(
            card.contains(
                "  kept      /state/tasks/t9/plan.md (9 bytes)\n\
                 \x20           /state/tasks/t9/handoff.md (3 KiB)\n"
            ),
            "{card}"
        );
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
