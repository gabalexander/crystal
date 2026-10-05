//! `crystal project`: the projects crystal knows, which the TUI lists with
//! no session running in them, adding one and taking one off the list; and
//! running a worktree's project, or opening it, with the commands
//! [`crate::project_commands`] finds.

use crate::client;
use crate::config::Config;
use crate::git::Checkout;
use crate::output::outln;
use crate::project_commands::{self, Commands, Verb};
use crate::protocol::{Request, Response, SessionInfo, State, Worktree};
use crate::shell;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::Path;

/// A known project, as `crystal project --json` prints it.
#[derive(Serialize)]
struct Listed<'a> {
    name: &'a str,
    path: &'a Path,
    branch: Option<&'a str>,
    sessions: usize,
}

/// Prints the projects crystal knows, with how many sessions each has.
pub fn list(socket: &Path, json: bool) -> Result<()> {
    let projects = match client::ask(socket, &Request::Projects, true)? {
        Some(Response::Projects { projects }) => projects,
        _ => bail!("the daemon didn't say which projects it knows"),
    };
    let sessions = sessions(socket)?;
    let count = |project: &Worktree| {
        sessions
            .iter()
            .filter(|s| s.worktree.as_ref().map(|w| &w.project_path) == Some(&project.path))
            .count()
    };
    if json {
        let listed: Vec<Listed> = projects
            .iter()
            .map(|project| Listed {
                name: &project.project,
                path: &project.path,
                branch: project.branch.as_deref(),
                sessions: count(project),
            })
            .collect();
        outln!("{}", serde_json::to_string_pretty(&listed)?)?;
        return Ok(());
    }
    if projects.is_empty() {
        outln!("no projects yet: start a session in one, or add one with crystal project add")?;
        return Ok(());
    }
    let rows: Vec<[String; 4]> = projects
        .iter()
        .map(|project| {
            [
                project.project.clone(),
                project.branch.clone().unwrap_or_else(|| "-".to_string()),
                count(project).to_string(),
                shell::home_relative(&project.path),
            ]
        })
        .collect();
    crate::print_table(["NAME", "BRANCH", "SESSIONS", "DIRECTORY"], &rows)
}

/// Puts the project `dir` is in on the list, or takes it off.
pub fn change(socket: &Path, dir: &Path, listed: bool) -> Result<()> {
    let request = if listed {
        Request::AddProject {
            dir: dir.to_path_buf(),
        }
    } else {
        Request::RemoveProject {
            dir: dir.to_path_buf(),
        }
    };
    client::ask(socket, &request, true)?;
    Ok(())
}

/// Starts the project's run command in a session of its own in the
/// worktree `dir` is in, and returns its name; or, with `stop`, stops the
/// one running there. An ended one starts again.
pub fn run(socket: &Path, dir: &Path, stop: bool) -> Result<Option<String>> {
    let (worktree, line) = command_for(dir, Verb::Run)?;
    let sessions = sessions(socket)?;
    let running = sessions
        .iter()
        .find(|session| project_commands::is_run_of(session, &worktree.path, &line));
    match (running, stop) {
        (Some(session), true) => {
            let name = session.name.clone();
            client::ask(socket, &Request::Kill { name }, false)?;
            Ok(None)
        }
        (None, true) => bail!("nothing runs {line} in {}", worktree.path.display()),
        (Some(session), false) if session.state == State::Running => {
            bail!(
                "{} runs it already: crystal attach {}, or stop it with crystal project run --stop",
                session.name,
                session.name
            )
        }
        (Some(session), false) => {
            client::respawn(socket, &session.name)?;
            Ok(Some(session.name.clone()))
        }
        (None, false) => {
            let base = project_commands::run_name(&worktree.path);
            let name = free_name(&base, &sessions);
            let command = project_commands::shell_command(&line);
            let name = client::new_session(socket, Some(name), worktree.path, command)?;
            Ok(Some(name))
        }
    }
}

/// Runs the project's open command in the worktree `dir` is in.
pub fn open(dir: &Path) -> Result<()> {
    let (worktree, line) = command_for(dir, Verb::Open)?;
    project_commands::open(&line, &worktree.path)
}

/// The worktree `dir` is in, and its project's command for `which`.
fn command_for(dir: &Path, which: Verb) -> Result<(Worktree, String)> {
    let worktree = Checkout::find(dir)
        .with_context(|| format!("{} isn't in a git repository", dir.display()))?
        .worktree();
    let config = Config::load()?;
    let commands = Commands::of(&config, &worktree.path, &worktree.project_path)?;
    let line = commands.line(which, &worktree.project)?.to_string();
    Ok((worktree, line))
}

/// The sessions, or none without a daemon.
fn sessions(socket: &Path) -> Result<Vec<SessionInfo>> {
    match client::ask(socket, &Request::List, false)? {
        Some(Response::Sessions { sessions }) => Ok(sessions),
        _ => Ok(Vec::new()),
    }
}

/// `base`, or `base-2` or the next number no session has.
pub fn free_name(base: &str, sessions: &[SessionInfo]) -> String {
    let taken = |name: &str| sessions.iter().any(|session| session.name == name);
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|number| format!("{base}-{number}"))
        .find(|name| !taken(name))
        .expect("some number is free")
}
