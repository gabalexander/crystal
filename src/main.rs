mod attach;
mod client;
mod daemon;
mod env;
mod protocol;
mod session;
mod socket;
mod tui;
mod viewer;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use protocol::{Request, Response, SessionInfo};
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
                session.state.to_string(),
                session.pid.map_or("-".into(), |pid| pid.to_string()),
                cwd,
                session
                    .command
                    .iter()
                    .map(|arg| quote(arg))
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

/// `arg` as you'd type it into a shell.
fn quote(arg: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c);
    if !arg.is_empty() && arg.chars().all(plain) {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::quote;

    #[test]
    fn quote_leaves_plain_words_alone() {
        assert_eq!(quote("--model=opus"), "--model=opus");
    }

    #[test]
    fn quote_wraps_what_a_shell_would_split() {
        assert_eq!(quote("exit 3"), "'exit 3'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote(""), "''");
    }
}
