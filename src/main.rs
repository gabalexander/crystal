mod agent_screen;
mod agents;
mod attach;
mod client;
mod daemon;
mod env;
mod git;
mod hook;
mod protocol;
mod session;
mod shell;
mod socket;
mod state;
mod tui;
mod viewer;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use protocol::{Request, Response, SessionInfo, State};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// One terminal for all your coding agents. With no command, opens the
/// TUI: every session in a sidebar, the selected one live beside it.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// The daemon's socket [default: $XDG_RUNTIME_DIR/crystal/default.sock,
    /// or /tmp/crystal-<uid>/default.sock]
    #[arg(short = 'S', long, global = true, env = "CRYSTAL_SOCKET")]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start a session running a command, or your shell if there's none,
    /// and attach to it when run in a terminal.
    New {
        /// The session's name [default: the program's name]
        #[arg(short, long)]
        name: Option<String>,

        /// The directory to start in [default: the current one]
        #[arg(short = 'c', long)]
        cwd: Option<PathBuf>,

        /// Don't attach; print the session's name instead.
        #[arg(short, long)]
        detached: bool,

        /// Start in a new git worktree on this branch, beside the
        /// repository in <repo>.worktrees/. The branch is made if it
        /// doesn't exist.
        #[arg(short, long, value_name = "BRANCH")]
        worktree: Option<String>,

        /// The command and its arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Work with git worktrees.
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommand,
    },
    /// List the sessions.
    #[command(visible_alias = "list")]
    Ls,
    /// Show a session in this terminal; Ctrl+\ detaches.
    #[command(visible_alias = "a")]
    Attach {
        /// The session [default: the newest one]
        name: Option<String>,
    },
    /// Stop a session and remove it from the list.
    Kill { name: String },
    /// Stop every session and the daemon.
    KillServer,
    /// Run the daemon in the foreground.
    #[command(hide = true)]
    Daemon,
    /// Tell the daemon about an agent's event; what the agent's hooks run.
    #[command(hide = true)]
    Hook { agent: String },
}

#[derive(Subcommand)]
enum WorktreeCommand {
    /// Remove a worktree, given its directory or its branch. Refuses while
    /// a session runs in it.
    #[command(visible_alias = "remove")]
    Rm {
        /// The worktree's directory, or the branch it has checked out.
        worktree: String,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("crystal: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let socket = cli.socket.unwrap_or_else(socket::default_path);
    let Some(command) = cli.command else {
        return tui::run(&socket);
    };
    match command {
        Command::New {
            name,
            cwd,
            detached,
            worktree,
            command,
        } => new_session(&socket, name, cwd, worktree, detached, command)?,
        Command::Worktree {
            command: WorktreeCommand::Rm { worktree },
        } => remove_worktree(&socket, &worktree)?,
        Command::Attach { name } => attach::run(&socket, name.as_deref())?,
        Command::Ls => {
            if let Some(Response::Sessions { sessions }) =
                client::ask(&socket, &Request::List, false)?
            {
                print_sessions(&sessions);
            }
        }
        Command::Kill { name } => {
            if client::ask(&socket, &Request::Kill { name }, false)?.is_none() {
                no_daemon(&socket)?;
            }
        }
        Command::KillServer => {
            if client::ask(&socket, &Request::Shutdown, false)?.is_none() {
                no_daemon(&socket)?;
            }
        }
        Command::Daemon => daemon::run(&socket)?,
        Command::Hook { agent } => hook::run(&socket, &agent),
    }
    Ok(())
}

fn new_session(
    socket: &Path,
    name: Option<String>,
    cwd: Option<PathBuf>,
    worktree: Option<String>,
    detached: bool,
    command: Vec<String>,
) -> Result<()> {
    let mut cwd = match cwd {
        Some(cwd) => std::path::absolute(cwd)?,
        None => std::env::current_dir()?,
    };
    if let Some(branch) = worktree {
        cwd = git::add_worktree(&cwd, &branch)?;
    }
    let name = client::new_session(socket, name, cwd, command)?;

    let in_a_terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if in_a_terminal && !detached {
        attach::run(socket, Some(&name))
    } else {
        println!("{name}");
        Ok(())
    }
}

/// Removes the worktree `target` names, unless sessions are still running
/// in it: removing a directory out from under a program would leave it
/// working on files that are gone.
fn remove_worktree(socket: &Path, target: &str) -> Result<()> {
    let path = git::find_worktree(&std::env::current_dir()?, target)?;
    let sessions = match client::ask(socket, &Request::List, false)? {
        Some(Response::Sessions { sessions }) => sessions,
        _ => Vec::new(),
    };
    let running: Vec<&str> = sessions
        .iter()
        .filter(|session| session.state == State::Running && runs_in(session, &path))
        .map(|session| session.name.as_str())
        .collect();
    if !running.is_empty() {
        bail!("{} still running in {}", running.join(", "), path.display());
    }
    git::remove_worktree(&path)
}

/// Whether `session` runs in the worktree at `path`.
fn runs_in(session: &SessionInfo, path: &Path) -> bool {
    session
        .worktree
        .as_ref()
        .is_some_and(|worktree| worktree.path == path)
}

fn no_daemon(socket: &Path) -> Result<()> {
    bail!("no daemon is running on {}", socket.display())
}

fn print_sessions(sessions: &[SessionInfo]) {
    if sessions.is_empty() {
        return;
    }
    let rows: Vec<[String; 7]> = sessions
        .iter()
        .map(|session| {
            let (project, branch) = project_and_branch(session);
            [
                session.name.clone(),
                status(session),
                session.pid.map_or("-".into(), |pid| pid.to_string()),
                project,
                branch,
                shell::home_relative(&session.cwd),
                session
                    .command
                    .iter()
                    .map(|arg| shell::quote(arg))
                    .collect::<Vec<_>>()
                    .join(" "),
            ]
        })
        .collect();
    let header = [
        "NAME",
        "STATE",
        "PID",
        "PROJECT",
        "BRANCH",
        "DIRECTORY",
        "COMMAND",
    ]
    .map(String::from);
    let mut widths = [0; 7];
    for row in std::iter::once(&header).chain(&rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    for row in std::iter::once(&header).chain(&rows) {
        let line: Vec<String> = row
            .iter()
            .zip(widths)
            .map(|(cell, width)| format!("{cell:width$}"))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
}

/// The project and branch a session runs in, for `ls`. Every cell holds a
/// word, so a script can split the table on spaces: `-` outside a
/// repository, and `(detached)` for a worktree on no branch.
fn project_and_branch(session: &SessionInfo) -> (String, String) {
    match &session.worktree {
        Some(worktree) => {
            let branch = worktree.branch.as_deref().unwrap_or("(detached)");
            (worktree.project.clone(), branch.to_string())
        }
        None => ("-".into(), "-".into()),
    }
}

/// What `ls` says about a session: what its agent is doing, when it runs
/// one that says, or else whether it's running or how it ended.
fn status(session: &SessionInfo) -> String {
    match (&session.state, session.activity) {
        (State::Running, Some(activity)) => activity.to_string(),
        (state, _) => state.to_string(),
    }
}
