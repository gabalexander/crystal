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
use crate::printable;
use crate::protocol::{
    Backlog, BacklogItem, ForgeLink, PendingTask, Request, Response, State, TaskBrief, TaskSpec,
    TaskStart, TaskState, TaskView, task_label,
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
        print!("{}", printable::text(&task_lines(&tasks, all)));
    }
    Ok(())
}

/// What a task is to carry beside its goal, as the command line says it:
/// its acceptance criteria, given one by one or in a file, and the pull
/// request and the issue it's about, by their numbers.
#[derive(Debug, Default)]
pub struct Brief {
    pub accept: Vec<String>,
    pub accept_file: Option<PathBuf>,
    pub pull_request: Option<u64>,
    pub issue: Option<u64>,
}

/// Reads `brief` for a task in the project `dir` is in: its criteria, and
/// what the forge says of its pull request and issue. A task on a pull
/// request runs in that pull request's worktree, the project's own on its
/// branch or else one made for it, its commits fetched, which comes back
/// with it.
pub fn read_brief(socket: &Path, dir: &Path, brief: Brief) -> Result<(TaskBrief, Option<PathBuf>)> {
    let Brief {
        mut accept,
        accept_file,
        pull_request,
        issue,
    } = brief;
    if let Some(file) = &accept_file {
        let text = std::fs::read_to_string(file)
            .with_context(|| format!("couldn't read {}", file.display()))?;
        accept.extend(tasks::criteria_in(&text));
    }
    let mut read = TaskBrief {
        accept: tasks::criteria(accept)?,
        ..TaskBrief::default()
    };
    // Neither asks the forge anything.
    if pull_request.is_none() && issue.is_none() {
        return Ok((read, None));
    }
    let config = settings();
    if !forge::enabled(&config) {
        bail!(
            "--pr and --issue ask the forge, which is the github plugin: {}",
            crate::plugins::off("github")
        );
    }
    let project = crate::project::of(dir).path;
    let repo = forge::Repo::find(&project).map_err(anyhow::Error::msg)?;
    let mut worktree = None;
    if let Some(number) = pull_request {
        let found = repo
            .listed_pull_request(number)
            .map_err(anyhow::Error::msg)?;
        worktree = Some(client::pull_request_worktree(
            socket,
            &found.checkout(&project),
        )?);
        read.pull_request = Some(Box::new(ForgeLink {
            forge: repo.forge,
            number,
            title: found.title,
            url: found.url,
            branch: Some(found.local_branch),
        }));
    }
    if let Some(number) = issue {
        let found = repo.listed_issue(number).map_err(anyhow::Error::msg)?;
        read.issue = Some(Box::new(ForgeLink {
            forge: repo.forge,
            number,
            title: found.title,
            url: found.url,
            branch: None,
        }));
    }
    Ok((read, worktree))
}

/// A task's goal: the words given, or for a task on a pull request or an
/// issue given none, to work on it.
pub fn goal(words: &[String], brief: &TaskBrief) -> Result<String> {
    let said = words.join(" ");
    if !said.trim().is_empty() {
        return Ok(said);
    }
    tasks::goal_for(brief).context("say what it's to do")
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
    pub brief: TaskBrief,
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
        brief,
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
            brief,
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
                brief,
            };
            client::new_session_for(socket, name, cwd, command, purpose)?
        }
        TaskStart::Background { args } => {
            let spec = TaskSpec { prompt: goal, args };
            client::new_task(socket, name, cwd, spec, None, brief)?
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
        print!("{}", printable::text(&task_card(&task, now())));
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

/// Opens a background task, by its number or its session's name, in a
/// terminal, and prints its session's name. A background task runs with
/// the tasks plugin off too, so this does.
pub fn task_to_terminal(socket: &Path, task: &str) -> Result<()> {
    let request = Request::TaskToTerminal {
        task: task.to_string(),
        env: env::current(),
    };
    let Response::Created { name, .. } = ask_running(socket, &request)? else {
        bail!("the daemon didn't open it in a terminal");
    };
    println!("{name}");
    Ok(())
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
    print!("{}", printable::text(&task_card(&task, now())));
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
    if let Some(context) = task.context {
        line("context", context.words());
    }
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
    let brief = &record.brief;
    if let Some(pull_request) = &brief.pull_request {
        let forge = pull_request.forge;
        let label = match forge {
            forge::Forge::GitHub => "pr",
            forge::Forge::GitLab => "mr",
        };
        line(
            label,
            forge_line(&forge.label(pull_request.number), pull_request),
        );
    }
    if let Some(issue) = &brief.issue {
        line("issue", forge_line(&format!("#{}", issue.number), issue));
    }
    for (index, criterion) in brief.accept.iter().enumerate() {
        let label = if index == 0 { "accept" } else { "" };
        line(label, criterion.clone());
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

/// A pull request or an issue on a task's card: its number as `label`,
/// its title and where it is.
fn forge_line(label: &str, link: &ForgeLink) -> String {
    format!("{label} {} · {}", link.title, link.url)
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
    List {
        all: bool,
        json: bool,
        tags: Vec<String>,
    },
    Add {
        text: String,
        body: String,
        tags: Vec<String>,
    },
    Show {
        number: u64,
        json: bool,
    },
    Edit {
        number: u64,
        text: Option<String>,
        body: Option<String>,
        tags: Option<Vec<String>>,
    },
    Mark {
        number: u64,
        done: bool,
    },
    Remove {
        number: u64,
    },
    Export,
    /// Reads the items of a markdown file, or of standard input for `-`.
    Import {
        file: PathBuf,
    },
}

/// Does `action` to the backlog of the project `dir` is in.
pub fn change_backlog(socket: &Path, dir: PathBuf, action: BacklogAction) -> Result<()> {
    backlog::ensure_enabled(&settings())?;
    match action {
        BacklogAction::List { all, json, tags } => {
            let backlog = client::backlog(socket, dir, all)?;
            let items: Vec<&BacklogItem> = backlog
                .items
                .iter()
                .filter(|item| backlog::has_tags(item, &tags))
                .collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                print!("{}", printable::text(&backlog_lines(&items)));
            }
        }
        BacklogAction::Add { text, body, tags } => {
            let add = Request::BacklogAdd {
                dir,
                text,
                body,
                tags,
            };
            match client::ask(socket, &add, true)? {
                Some(Response::Added { number }) => println!("#{number}"),
                _ => bail!("the daemon didn't add it"),
            }
        }
        BacklogAction::Show { number, json } => {
            let backlog = client::backlog(socket, dir, true)?;
            let item = backlog_item(&backlog, number)?;
            let tasks = backlog.tasks_for(number);
            if json {
                let shown = ShownItem { item, tasks };
                println!("{}", serde_json::to_string_pretty(&shown)?);
            } else {
                print!("{}", printable::text(&item_card(item, &tasks, now())));
            }
        }
        BacklogAction::Edit {
            number,
            text,
            body,
            tags,
        } => {
            if text.is_none() && body.is_none() && tags.is_none() {
                bail!("say what to change: its line, --body, or --tag or --no-tags");
            }
            let edit = Request::BacklogEdit {
                dir,
                number,
                text,
                body,
                tags,
            };
            expect_done(client::ask(socket, &edit, true)?, socket)?;
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
            let markdown = backlog::markdown(&backlog.project, &backlog.items);
            print!("{}", printable::text(&markdown));
        }
        BacklogAction::Import { file } => {
            let text = if file == Path::new("-") {
                std::io::read_to_string(std::io::stdin()).context("couldn't read the input")?
            } else {
                std::fs::read_to_string(&file)
                    .with_context(|| format!("couldn't read {}", file.display()))?
            };
            let items = backlog::read_markdown(&text);
            if items.is_empty() {
                bail!("there's no `- [ ]` item at the start of a line to import");
            }
            let import = Request::BacklogImport { dir, items };
            let Some(Response::Imported { added, skipped }) = client::ask(socket, &import, true)?
            else {
                bail!("the daemon didn't import them");
            };
            print!("{}", imported_lines(&added, skipped));
        }
    }
    Ok(())
}

/// An item as `crystal backlog show --json` prints it: its fields, and
/// the tasks started for it.
#[derive(serde::Serialize)]
struct ShownItem<'a> {
    #[serde(flatten)]
    item: &'a BacklogItem,
    tasks: Vec<&'a TaskView>,
}

/// Item `number` of `backlog`, or an error that says it isn't there.
fn backlog_item(backlog: &Backlog, number: u64) -> Result<&BacklogItem> {
    backlog
        .items
        .iter()
        .find(|item| item.number == number)
        .with_context(|| format!("there's no #{number} on the backlog"))
}

/// What an import says it did: the numbers its items got, and how many it
/// passed over.
fn imported_lines(added: &[u64], skipped: usize) -> String {
    let mut text = match added {
        [] => "added nothing\n".to_string(),
        added => {
            let numbers: Vec<String> = added.iter().map(|number| format!("#{number}")).collect();
            format!("added {}\n", numbers.join(" "))
        }
    };
    match skipped {
        0 => {}
        1 => text.push_str("passed over 1 already on the backlog\n"),
        n => text.push_str(&format!("passed over {n} already on the backlog\n")),
    }
    text
}

/// How `crystal backlog start` starts a task for an item.
#[derive(Debug, Default)]
pub struct BacklogStart {
    pub number: u64,
    /// In a new worktree, on a branch named after the item.
    pub worktree: bool,
    /// The profile it runs with: its agent, its options and its prompt,
    /// and a new worktree when it says so.
    pub profile: Option<String>,
    /// The pull request it's on, by its number: it runs in its worktree,
    /// and is told of it.
    pub pull_request: Option<u64>,
    /// In the background, `claude -p` with `claude_args`.
    pub background: bool,
    pub claude_args: Vec<String>,
    /// Its session's name: `None` names it for the item.
    pub name: Option<String>,
}

/// Starts a task for a backlog item: the agent the new-session panel would
/// start first, or the one its profile says, or Claude in the background,
/// asked to do what the item says, here, in a new worktree named after it,
/// or in a pull request's. Closing the task done ticks the item. Gives back
/// the new session's name.
pub fn start_from_backlog(socket: &Path, dir: PathBuf, start: BacklogStart) -> Result<String> {
    let config = settings();
    backlog::ensure_enabled(&config)?;
    let BacklogStart {
        number,
        worktree,
        profile,
        pull_request,
        background,
        claude_args,
        name,
    } = start;
    let backlog = client::backlog(socket, dir.clone(), true)?;
    let item = backlog_item(&backlog, number)?;
    if item.done {
        bail!("#{number} is done already: `crystal backlog reopen {number}` first");
    }
    let profile = match profile {
        Some(name) => {
            crate::plugins::ensure_enabled(&config, "profiles")?;
            let found = config.profiles.iter().find(|profile| profile.name == name);
            let found = found.with_context(|| {
                format!("there's no profile called {name}; `crystal profile` lists them")
            })?;
            Some(found.clone())
        }
        None => None,
    };
    let goal = backlog::goal(item);
    let brief = Brief {
        pull_request,
        ..Brief::default()
    };
    let (brief, on_pull_request) = read_brief(socket, &dir, brief)?;
    let in_worktree = profile
        .as_ref()
        .is_some_and(|profile| profile.start_in == Some(crate::profile::StartIn::Worktree));
    let cwd = match on_pull_request {
        Some(path) => path,
        None if worktree || in_worktree => client::add_worktree(
            socket,
            &dir,
            &forge::branch_for_issue(number, &item.text),
            None,
            None,
        )?,
        None => dir,
    };
    if background {
        let spec = TaskSpec {
            prompt: goal,
            args: claude_args,
        };
        return Ok(client::new_task(socket, name, cwd, spec, Some(number), brief)?.name);
    }
    let command = match &profile {
        Some(profile) => profile.command(&goal),
        None => agent_command(&config, &goal),
    };
    let purpose = Purpose {
        task: Some(goal),
        backlog: Some(number),
        brief,
    };
    Ok(client::new_session_for(socket, name, cwd, command, purpose)?.name)
}

/// Backlog items as `crystal backlog` prints them: number, a tick for one
/// that's done, text, tags, and a `+` for one with more in its body.
fn backlog_lines(items: &[&BacklogItem]) -> String {
    let mut text = String::new();
    for item in items {
        let tick = if item.done { "✓ " } else { "" };
        let more = if item.body.is_empty() { "" } else { " +" };
        let tags: String = item.tags.iter().map(|tag| format!("  #{tag}")).collect();
        let number = format!("#{}", item.number);
        text.push_str(&format!(
            "{number:<4}  {tick}{}{more}{tags}\n",
            first_line(&item.text)
        ));
    }
    text
}

/// One item, as `crystal backlog show` prints it: whether it's done, its
/// tags, when it was added and done, the tasks started for it, then its
/// body.
fn item_card(item: &BacklogItem, tasks: &[&TaskView], now: u64) -> String {
    let state = if item.done { "done" } else { "open" };
    let mut card = format!("#{}  {state}  {}\n", item.number, first_line(&item.text));
    let mut line = |name: &str, said: String| card.push_str(&format!("  {name:<8}  {said}\n"));
    if !item.tags.is_empty() {
        let tags: Vec<String> = item.tags.iter().map(|tag| format!("#{tag}")).collect();
        line("tags", tags.join(" "));
    }
    if item.created > 0 {
        line("added", when(item.created, now));
    }
    if let Some(closed) = item.closed {
        line("done", when(closed, now));
    }
    for (index, task) in tasks.iter().enumerate() {
        let label = if index == 0 { "tasks" } else { "" };
        line(label, task_history_line(task, now));
    }
    if !item.body.is_empty() {
        card.push('\n');
        for body_line in item.body.lines() {
            card.push_str(format!("  {body_line}").trim_end());
            card.push('\n');
        }
    }
    card
}

/// A task started for an item, on its card: its number and how it stands,
/// then how it went or where it runs.
fn task_history_line(task: &TaskView, now: u64) -> String {
    let record = &task.record;
    let mut said = format!("{:<4}  {}", task_label(record.id), task.state.word());
    match &record.outcome {
        Some(outcome) if outcome.summary.is_empty() => {
            said.push_str(&format!(", {}", when(outcome.closed, now)));
        }
        Some(outcome) => {
            said.push_str(&format!(
                ", {}: {}",
                when(outcome.closed, now),
                outcome.summary
            ));
        }
        None if record.session.is_empty() => {}
        None => said.push_str(&format!(", in {}", record.session)),
    }
    said
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
            brief: Default::default(),
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
    fn a_card_says_what_a_task_is_about_what_it_must_meet_and_how_full_it_is() {
        let mut about = task(9, "fix the login", None);
        about.record.background = true;
        about.context = Some(crate::protocol::ContextUse {
            tokens: 24_000,
            window: 200_000,
        });
        let link = |number, kind: &str| {
            Box::new(ForgeLink {
                forge: forge::Forge::GitHub,
                number,
                title: "Login loops".into(),
                url: format!("https://github.com/acme/app/{kind}/{number}"),
                branch: None,
            })
        };
        about.record.brief = TaskBrief {
            accept: vec!["the tests pass".into(), "no redirect loop".into()],
            pull_request: Some(link(57, "pull")),
            issue: Some(link(7, "issues")),
        };
        let card = task_card(&about, 160);
        assert!(
            card.contains("  context   24k of 200k tokens, 12%\n"),
            "{card}"
        );
        assert!(
            card.contains("  pr        #57 Login loops · https://github.com/acme/app/pull/57\n"),
            "{card}"
        );
        assert!(
            card.contains("  issue     #7 Login loops · https://github.com/acme/app/issues/7\n"),
            "{card}"
        );
        assert!(
            card.contains("  accept    the tests pass\n            no redirect loop\n"),
            "{card}"
        );
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

    fn item(number: u64, text: &str, done: bool, tags: &[&str]) -> BacklogItem {
        BacklogItem {
            number,
            text: text.into(),
            body: String::new(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            done,
            created: 0,
            closed: None,
        }
    }

    #[test]
    fn a_backlog_line_has_its_number_text_and_tags() {
        let mut more = item(3, "ship it", false, &[]);
        more.body = "once it's green".into();
        let lines = backlog_lines(&[
            &item(1, "write the docs", false, &["docs"]),
            &item(12, "fix it", true, &[]),
            &more,
        ]);
        assert_eq!(
            lines,
            "#1    write the docs  #docs\n#12   ✓ fix it\n#3    ship it +\n"
        );
    }

    #[test]
    fn an_item_s_card_has_its_tasks_oldest_first_then_its_body() {
        let mut shown = item(4, "retry the webhook", false, &["payments", "ci"]);
        shown.created = 100;
        shown.body = "On a timeout only.\n\nNot on a 4xx.".into();
        let mut failed = task(
            7,
            "retry the webhook",
            Some((TaskState::Failed, "no network")),
        );
        failed.record.created = 200;
        let mut running = task(9, "retry the webhook", None);
        running.record.created = 900;
        let backlog = Backlog {
            project: "shop".into(),
            path: PathBuf::from("/code/shop"),
            items: vec![shown.clone()],
            tasks: [running, failed]
                .into_iter()
                .map(|mut task| {
                    task.record.backlog = Some(4);
                    task
                })
                .chain([task(8, "something else", None)])
                .collect(),
        };
        let tasks = backlog.tasks_for(4);
        assert_eq!(
            item_card(&shown, &tasks, 1000),
            "#4  open  retry the webhook\n\
             \x20 tags      #payments #ci\n\
             \x20 added     15m ago\n\
             \x20 tasks     t7    failed, just now: no network\n\
             \x20           t9    running, in claude\n\
             \n\
             \x20 On a timeout only.\n\
             \n\
             \x20 Not on a 4xx.\n"
        );
    }

    #[test]
    fn an_import_says_what_it_added_and_passed_over() {
        assert_eq!(imported_lines(&[4, 5], 0), "added #4 #5\n");
        assert_eq!(
            imported_lines(&[], 2),
            "added nothing\npassed over 2 already on the backlog\n"
        );
    }
}
