mod agent_screen;
mod agents;
mod attach;
mod client;
mod config;
mod daemon;
mod drive;
mod env;
mod git;
mod history;
mod hook;
mod keys;
mod notify;
mod protocol;
mod remote;
mod session;
mod shell;
mod skill;
mod socket;
mod state;
mod tui;
mod typing;
mod viewer;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use protocol::{Request, Response, SessionInfo, State};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

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
    Ls {
        /// Print them as a JSON array, for scripts and agents.
        #[arg(long)]
        json: bool,
    },
    /// Show a session in this terminal; Ctrl+\ detaches.
    #[command(visible_alias = "a")]
    Attach {
        /// The session [default: the newest one]
        name: Option<String>,
    },
    /// Press keys in a session, the way tmux's send-keys does: key names
    /// (Enter, Escape, Tab, BTab, BSpace, Space, Up, Down, Left, Right, Home,
    /// End, PageUp, PageDown, Delete, F1-F12, C-x, M-x) or text, typed as
    /// keys. What answers an agent's question: `send-keys reviewer 1`.
    SendKeys {
        name: String,

        #[arg(required = true)]
        keys: Vec<String>,

        /// Then wait for the turn they start or carry on to end, and print
        /// how it ended.
        #[arg(long)]
        wait: bool,

        /// With --wait, give up after this many seconds.
        #[arg(long, value_name = "SECONDS", requires = "wait")]
        timeout: Option<f64>,
    },
    /// Type text into a session and press Enter, the way a person would.
    Send {
        name: String,

        /// The text to type. Several words are joined with spaces; put
        /// `--` before text that starts with a `-`.
        #[arg(required = true)]
        text: Vec<String>,

        /// Type the text without pressing Enter.
        #[arg(long)]
        no_enter: bool,

        /// Then wait for the turn it starts to end, and print how it ended.
        #[arg(long)]
        wait: bool,

        /// With --wait, give up after this many seconds.
        #[arg(long, value_name = "SECONDS", requires = "wait")]
        timeout: Option<f64>,
    },
    /// Wait until a session's agent isn't working, or its program has
    /// ended, and print which: done, waiting, idle, exited 0…
    Wait {
        name: String,

        /// Give up after this many seconds, and fail.
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<f64>,
    },
    /// Print what's on a session's screen.
    Read {
        name: String,

        /// Only the last this many rows that aren't blank.
        #[arg(short = 'n', long)]
        lines: Option<usize>,

        /// The rows that have scrolled up off the screen too, ahead of it.
        #[arg(long)]
        history: bool,
    },
    /// Give a session another name.
    Rename { name: String, new_name: String },
    /// Run an ended session's command again, in the same directory and
    /// under the same name. Claude Code comes back in its conversation.
    Respawn { name: String },
    /// Stop a session and remove it from the list.
    Kill { name: String },
    /// Stop every session and the daemon.
    KillServer,
    /// Restart the daemon on this crystal, say after installing a new one.
    /// Running sessions come back: Claude Code in its conversation, other
    /// programs from the start.
    RestartServer,
    /// Show where the config file is, and the settings in effect, as the
    /// file would hold them.
    Config,
    /// Print the Claude Code skill that teaches an agent to drive crystal.
    Skill {
        /// Install it into Claude Code's skills, in $CLAUDE_CONFIG_DIR or
        /// ~/.claude.
        #[arg(long)]
        install: bool,

        /// With --install, write over a skill file that has been changed.
        #[arg(long, requires = "install")]
        force: bool,
    },
    /// Run crystal on another machine, over your own ssh: its TUI, or a
    /// crystal command there, like `crystal ssh box ls`.
    Ssh {
        /// Install crystal there, or upgrade it, without asking.
        #[arg(long)]
        install: bool,

        /// Where to, the way ssh takes it: `box`, or `me@box.example.com`.
        destination: String,

        /// The crystal command to run there [default: the TUI]
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
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
        Command::Ls { json } => {
            // Without a daemon, there are no sessions.
            let sessions = match client::ask(&socket, &Request::List, false)? {
                Some(Response::Sessions { sessions }) => sessions,
                _ => Vec::new(),
            };
            if json {
                print_sessions_json(&sessions)?;
            } else {
                print_sessions(&sessions);
            }
        }
        Command::Send {
            name,
            text,
            no_enter,
            wait,
            timeout,
        } => {
            drive::send(&socket, &name, &text.join(" "), !no_enter)?;
            if wait {
                drive::wait_for_turn(&socket, &name, seconds(timeout))?;
            }
        }
        Command::SendKeys {
            name,
            keys,
            wait,
            timeout,
        } => {
            drive::send_keys(&socket, &name, keys)?;
            if wait {
                drive::wait_for_turn(&socket, &name, seconds(timeout))?;
            }
        }
        Command::Wait { name, timeout } => drive::wait(&socket, &name, seconds(timeout))?,
        Command::Read {
            name,
            lines,
            history,
        } => drive::read(&socket, &name, lines, history)?,
        Command::Rename { name, new_name } => client::rename(&socket, &name, &new_name)?,
        Command::Respawn { name } => client::respawn(&socket, &name)?,
        Command::Kill { name } => {
            if client::ask(&socket, &Request::Kill { name }, false)?.is_none() {
                no_daemon(&socket)?;
            }
        }
        Command::KillServer => {
            let shutdown = Request::Shutdown {
                keep_sessions: false,
            };
            if client::ask(&socket, &shutdown, false)?.is_none() {
                no_daemon(&socket)?;
            }
        }
        Command::RestartServer => {
            if client::restart_daemon(&socket)? {
                println!("restarted the daemon");
            } else {
                println!("no daemon was running");
            }
        }
        Command::Config => print_config()?,
        Command::Skill { install, force } => {
            if install {
                skill::install(force)?;
            } else {
                skill::print();
            }
        }
        Command::Ssh {
            install,
            destination,
            args,
        } => {
            let code = remote::run(&destination, &args, install)?;
            // The remote command's own exit code is crystal's.
            std::process::exit(code);
        }
        Command::Daemon => daemon::run(&socket)?,
        Command::Hook { agent } => hook::run(&socket, &agent),
    }
    Ok(())
}

/// Prints the config file's path, as a comment, then the settings in
/// effect: a file to start from.
fn print_config() -> Result<()> {
    let settings = config::Config::load()?;
    let path = config::path();
    if path.exists() {
        println!("# {}", path.display());
    } else {
        println!("# {} (no file yet: these are the defaults)", path.display());
    }
    print!("{}", settings.to_toml());
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

/// Removes the worktree `target` names: a directory, or the branch it has
/// checked out.
fn remove_worktree(socket: &Path, target: &str) -> Result<()> {
    let path = git::find_worktree(&std::env::current_dir()?, target)?;
    client::remove_worktree(socket, &path)
}

/// A number of seconds from the command line, as a `Duration`.
fn seconds(seconds: Option<f64>) -> Option<Duration> {
    seconds.map(Duration::from_secs_f64)
}

fn no_daemon(socket: &Path) -> Result<()> {
    bail!("no daemon is running on {}", socket.display())
}

/// A session as `ls --json` prints it: every field the daemon sends, so
/// that a field added to the protocol shows up here too, plus `status`, the
/// one word the STATE column shows, which is easier for a script to test
/// than `state` and `activity` together.
#[derive(serde::Serialize)]
struct ListedSession<'a> {
    #[serde(flatten)]
    session: &'a SessionInfo,
    status: String,
}

fn print_sessions_json(sessions: &[SessionInfo]) -> Result<()> {
    let listed: Vec<ListedSession> = sessions
        .iter()
        .map(|session| ListedSession {
            session,
            status: status(session),
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&listed)?);
    Ok(())
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
