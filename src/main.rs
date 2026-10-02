mod agent_screen;
mod agents;
mod attach;
mod client;
mod daemon;
mod env;
mod hook;
mod protocol;
mod session;
mod shell;
mod socket;
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

        /// The command and its arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
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
            command,
        } => new_session(&socket, name, cwd, detached, command)?,
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
    detached: bool,
    command: Vec<String>,
) -> Result<()> {
    let cwd = match cwd {
        Some(cwd) => std::path::absolute(cwd)?,
        None => std::env::current_dir()?,
    };
    let name = client::new_session(socket, name, cwd, command)?;

    let in_a_terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if in_a_terminal && !detached {
        attach::run(socket, Some(&name))
    } else {
        println!("{name}");
        Ok(())
    }
}

fn no_daemon(socket: &Path) -> Result<()> {
    bail!("no daemon is running on {}", socket.display())
}

fn print_sessions(sessions: &[SessionInfo]) {
    if sessions.is_empty() {
        return;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let rows: Vec<[String; 5]> = sessions
        .iter()
        .map(|session| {
            let cwd = session.cwd.to_string_lossy();
            let cwd = match cwd.strip_prefix(&home) {
                Some(rest) if !home.is_empty() => format!("~{rest}"),
                _ => cwd.into_owned(),
            };
            [
                session.name.clone(),
                status(session),
                session.pid.map_or("-".into(), |pid| pid.to_string()),
                cwd,
                session
                    .command
                    .iter()
                    .map(|arg| shell::quote(arg))
                    .collect::<Vec<_>>()
                    .join(" "),
            ]
        })
        .collect();
    let header = ["NAME", "STATE", "PID", "DIRECTORY", "COMMAND"].map(String::from);
    let mut widths = [0; 5];
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

/// What `ls` says about a session: what its agent is doing, when it runs
/// one that says, or else whether it's running or how it ended.
fn status(session: &SessionInfo) -> String {
    match (&session.state, session.activity) {
        (State::Running, Some(activity)) => activity.to_string(),
        (state, _) => state.to_string(),
    }
}
