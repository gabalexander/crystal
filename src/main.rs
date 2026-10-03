mod agent_screen;
mod agents;
mod attach;
mod backlog;
mod catalog;
mod client;
mod clipboard;
mod codex;
mod config;
mod daemon;
mod db;
mod distill;
mod drive;
mod embed;
mod env;
mod flow_cli;
mod flow_run;
mod flows;
mod forge;
mod front;
mod git;
mod hook;
mod keys;
mod mcp;
mod memory;
mod memory_cli;
mod names;
mod notify;
mod plugin_cli;
mod plugin_hooks;
mod plugin_manifest;
mod plugins;
mod profile;
mod project;
mod protocol;
mod remote;
mod secrets;
mod session;
mod shell;
mod skill;
mod socket;
mod state;
mod task;
mod tasks;
mod transcript;
mod tui;
mod typing;
mod viewer;
mod vt;
mod work;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use profile::{Profile, StartIn};
use protocol::{Request, Response, SessionInfo, State, TaskSpec};
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

        /// Give the session this to do, which makes it a task: open until
        /// it's closed with `crystal done`. An agent crystal knows gets it
        /// as its first prompt. A prompt given as the agent's only
        /// argument, like `claude "fix it"`, makes a task too.
        #[arg(short, long, value_name = "TEXT")]
        task: Option<String>,

        /// The command and its arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Close a task, the one of the session this runs in unless `-n` says
    /// another's: done, with a line on what was done, or `--failed`, with
    /// why.
    Done {
        /// The session whose task to close [default: the one this runs in]
        #[arg(short, long)]
        name: Option<String>,

        /// It couldn't be done.
        #[arg(long)]
        failed: bool,

        /// What was done, or why it couldn't be. Several words are joined
        /// with spaces.
        summary: Vec<String>,
    },
    /// List the tasks of the project this directory is in: those still
    /// open, then those closed, the latest first.
    Tasks {
        /// Every project's tasks.
        #[arg(long)]
        all: bool,

        /// The project's directory [default: the current one]
        #[arg(short = 'C', long = "dir", value_name = "DIR")]
        dir: Option<PathBuf>,

        /// Print them as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Keep the project's backlog: things worth doing later. With no
    /// command, lists what's open.
    Backlog {
        /// The project's directory [default: the current one]
        #[arg(short = 'C', long = "dir", value_name = "DIR", global = true)]
        dir: Option<PathBuf>,

        /// With no command: what's done too.
        #[arg(long)]
        all: bool,

        /// With no command: print it as JSON.
        #[arg(long)]
        json: bool,

        #[command(subcommand)]
        command: Option<BacklogCommand>,
    },
    /// Start a task: Claude Code runs a prompt without a terminal (`claude
    /// -p`), in the background. Its transcript shows like any session's, and
    /// `crystal send` gives it follow-ups. Prints the task's name.
    Task {
        /// The task's name [default: task, task-2…]
        #[arg(short, long)]
        name: Option<String>,

        /// The directory to start in [default: the current one]
        #[arg(short = 'c', long)]
        cwd: Option<PathBuf>,

        /// Start in a new git worktree on this branch, as `new -w` does.
        #[arg(short, long, value_name = "BRANCH")]
        worktree: Option<String>,

        /// Then wait for the run to end, and print how it ended.
        #[arg(long)]
        wait: bool,

        /// With --wait, give up after this many seconds.
        #[arg(long, value_name = "SECONDS", requires = "wait")]
        timeout: Option<f64>,

        /// The prompt. Several words are joined with spaces.
        #[arg(required = true)]
        prompt: Vec<String>,

        /// Arguments for each `claude -p` the task runs, after `--`: for
        /// example `-- --permission-mode acceptEdits`.
        #[arg(last = true, value_name = "CLAUDE ARGS")]
        claude_args: Vec<String>,
    },
    /// Run flows: chains of background tasks on one goal, from the config
    /// file's `[[flow]]` tables. With no command, lists the runs.
    Flow {
        /// With no command: print the runs as JSON.
        #[arg(long)]
        json: bool,

        #[command(subcommand)]
        command: Option<FlowCommand>,
    },
    /// Print a task's answer: what Claude said at the end of its last run.
    Result {
        name: String,

        /// Everything the task has come to, as JSON: the answer, whether the
        /// run failed, the conversation's id, the cost so far and how many
        /// runs it has had.
        #[arg(long)]
        json: bool,
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
    /// Remember something about this project for its later sessions: a
    /// decision, a gotcha, a command that works, a note.
    Remember {
        /// What sort of thing it is.
        #[arg(short, long, value_enum, default_value_t = memory::Kind::Note)]
        kind: memory::Kind,

        /// A file it's about; once the file changes, the entry is marked
        /// stale. Give it once a file.
        #[arg(short = 'f', long = "file", value_name = "FILE")]
        files: Vec<String>,

        /// The project's directory [default: the current one]
        #[arg(short = 'C', long = "dir", value_name = "DIR")]
        dir: Option<PathBuf>,

        /// What to remember. Several words are joined with spaces.
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// What this project's sessions have remembered, newest first.
    Memory {
        /// The project's directory [default: the current one]
        #[arg(short = 'C', long = "dir", value_name = "DIR", global = true)]
        dir: Option<PathBuf>,

        #[command(subcommand)]
        command: Option<MemoryCommand>,
    },
    /// List the agent profiles in the config file.
    Profile {
        #[command(subcommand)]
        command: Option<ProfileCommand>,
    },
    /// List plugins, crystal's own and yours, with whether they're on; or
    /// switch, run, install, make or remove one.
    Plugin {
        #[command(subcommand)]
        command: Option<PluginCommand>,
    },
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
    /// Serve a project's memory to Claude over MCP, on standard input and
    /// output: what a task in the background searches it with.
    #[command(hide = true)]
    Mcp {
        /// The project's directory [default: the current one]
        #[arg(short = 'C', long = "dir", value_name = "DIR")]
        dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum MemoryCommand {
    /// The entries that have to do with these words, the best first.
    Search {
        #[arg(required = true)]
        words: Vec<String>,
    },
    /// Forget an entry, by its id.
    #[command(visible_alias = "remove")]
    Rm { id: u64 },
    /// Write an entry into the project's CLAUDE.md, or its AGENTS.md, under
    /// a "Notes" heading, for every session to read.
    Promote {
        id: u64,

        /// Don't ask first.
        #[arg(long)]
        yes: bool,
    },
    /// Download the model that searches by meaning, if it isn't here yet,
    /// and give every entry its vector: see `embeddings` under `[memory]`
    /// in the config.
    Embed,
    /// Have a model read what a session did and keep what a later session
    /// would need, now: what happens by itself once a task closes.
    Distill {
        /// The session, by its name.
        name: String,
    },
}

#[derive(Subcommand)]
enum FlowCommand {
    /// Start a run of a flow on a goal. Prints the run's name.
    Run {
        /// The flow, by its name in the config file.
        flow: String,

        /// What the flow is to do: `{goal}` in its steps' prompts. Several
        /// words are joined with spaces.
        #[arg(required = true)]
        goal: Vec<String>,

        /// The directory to start in [default: the current one]
        #[arg(short = 'c', long)]
        cwd: Option<PathBuf>,

        /// Then wait for it, as `crystal flow wait` does.
        #[arg(long)]
        wait: bool,
    },
    /// Show a run: each step, how it stands, and the first line of what it
    /// answered.
    Show {
        run: String,

        /// Everything about the run, as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Go on past the gate a run waits at.
    Approve { run: String },
    /// Send a run back from the gate it waits at, with notes on what to do
    /// differently: its gate's `back_to` step runs again, with the notes as
    /// `{feedback}`.
    Back {
        run: String,

        /// What to do differently. Several words are joined with spaces.
        notes: Vec<String>,
    },
    /// Run the step that stopped a run again: it failed, or a restart cut it
    /// short.
    Retry { run: String },
    /// Wait until a run stops running, at a gate or at its end, and print
    /// which. A step that failed or was cut short is an error.
    Wait {
        run: String,

        /// Give up after this many seconds.
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<f64>,
    },
    /// Print an example flow, with the profiles it runs with, to copy into
    /// the config file.
    Example,
}

#[derive(Subcommand)]
enum WorktreeCommand {
    /// Remove a worktree, given its directory or its branch. Refuses while
    /// a session runs in it, and when it has changes not committed.
    #[command(visible_alias = "remove")]
    Rm {
        /// The worktree's directory, or the branch it has checked out.
        worktree: String,

        /// Remove it even with changes not committed, which go with it.
        #[arg(long, short)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum ProfileCommand {
    /// Show a profile: its agent, where it starts, and the command it runs
    /// for a task.
    Show { name: String },
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Turn a plugin on.
    Enable { name: String },
    /// Turn a plugin off.
    Disable { name: String },
    /// Run one of a plugin's actions.
    Run {
        plugin: String,
        action: String,

        /// The session to run it for [default: the one this runs in, if
        /// any]
        #[arg(short, long)]
        session: Option<String>,
    },
    /// Install a plugin from a git repository or a directory, once you've
    /// seen what it runs and said yes. It starts off.
    Install {
        /// A git repository's URL, or a directory.
        source: String,

        /// Don't ask first.
        #[arg(long)]
        yes: bool,

        /// Turn it on once it's installed.
        #[arg(long)]
        enable: bool,
    },
    /// Remove a plugin you installed.
    #[command(visible_alias = "rm")]
    Remove { name: String },
    /// Make a plugin to start from, in your plugins directory.
    New { name: String },
    /// Print what a plugin's commands printed, and how they failed.
    Log { name: String },
}

#[derive(Subcommand)]
enum BacklogCommand {
    /// Put something on the backlog. Prints its number.
    Add {
        /// What to do later. Several words are joined with spaces.
        #[arg(required = true)]
        text: Vec<String>,

        /// A tag for it, like `ui`; give it more than once for more.
        #[arg(short, long = "tag", value_name = "TAG")]
        tags: Vec<String>,
    },
    /// Mark an item done.
    Done { number: u64 },
    /// Mark a done item open again.
    Reopen { number: u64 },
    /// Take an item off the backlog.
    #[command(visible_alias = "remove")]
    Rm { number: u64 },
    /// Start a task for an item, with the agent the new-session panel
    /// starts first. Closing the task done ticks the item.
    Start {
        number: u64,

        /// In a new worktree, on a branch named after the item.
        #[arg(short, long)]
        worktree: bool,

        /// The session's name [default: the agent's name]
        #[arg(short, long)]
        name: Option<String>,

        /// Don't attach; print the session's name instead.
        #[arg(short, long)]
        detached: bool,
    },
    /// Print the backlog as markdown checkboxes, done items too.
    Export,
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
    // Made absolute here: the daemon runs from `/`, where a relative path
    // would name another socket.
    let socket = std::path::absolute(cli.socket.unwrap_or_else(socket::default_path))?;
    let Some(command) = cli.command else {
        return tui::run(&socket);
    };
    match command {
        Command::New {
            name,
            cwd,
            detached,
            worktree,
            task,
            command,
        } => new_session(&socket, name, cwd, worktree, detached, command, task)?,
        Command::Done {
            name,
            failed,
            summary,
        } => work::done(&socket, name, failed, &summary.join(" "))?,
        Command::Tasks { all, dir, json } => work::list_tasks(&socket, here(dir)?, all, json)?,
        Command::Backlog {
            dir,
            all,
            json,
            command,
        } => backlog(&socket, here(dir)?, all, json, command)?,
        Command::Task {
            name,
            cwd,
            worktree,
            wait,
            timeout,
            prompt,
            claude_args,
        } => {
            let spec = TaskSpec {
                prompt: prompt.join(" "),
                args: claude_args,
            };
            let cwd = start_dir(&socket, cwd, worktree)?;
            let name = client::new_task(&socket, name, cwd, spec, None)?;
            println!("{name}");
            if wait {
                drive::wait_for_turn(&socket, &name, seconds(timeout))?;
            }
        }
        Command::Flow { json, command } => flow(&socket, json, command)?,
        Command::Result { name, json } => drive::result(&socket, &name, json)?,
        Command::Worktree {
            command: WorktreeCommand::Rm { worktree, force },
        } => remove_worktree(&socket, &worktree, force)?,
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
            if !client::stop_daemon(&socket, false)? {
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
        Command::Remember {
            kind,
            files,
            dir,
            text,
        } => memory_cli::remember(&socket, dir, kind, files, &text.join(" "))?,
        Command::Memory { dir, command } => match command {
            None => memory_cli::list(&socket, dir)?,
            Some(MemoryCommand::Search { words }) => memory_cli::search(&socket, dir, &words)?,
            Some(MemoryCommand::Rm { id }) => memory_cli::remove(&socket, dir, id)?,
            Some(MemoryCommand::Distill { name }) => memory_cli::distill(&socket, &name)?,
            Some(MemoryCommand::Embed) => memory_cli::embed(&socket)?,
            Some(MemoryCommand::Promote { id, yes }) => {
                memory_cli::promote(&socket, dir, id, yes)?;
            }
        },
        Command::Profile { command } => {
            let settings = config::Config::load()?;
            plugins::ensure_enabled(&settings, "profiles")?;
            match command {
                None => print_profiles(&settings.profiles),
                Some(ProfileCommand::Show { name }) => print_profile(&settings.profiles, &name)?,
            }
        }
        Command::Plugin { command } => match command {
            None => plugin_cli::list(&socket)?,
            Some(PluginCommand::Enable { name }) => plugin_cli::switch(&socket, &name, true)?,
            Some(PluginCommand::Disable { name }) => plugin_cli::switch(&socket, &name, false)?,
            Some(PluginCommand::Run {
                plugin,
                action,
                session,
            }) => {
                let code = plugin_cli::run(&socket, &plugin, &action, session)?;
                // The action's own exit code is crystal's.
                std::process::exit(code);
            }
            Some(PluginCommand::Install {
                source,
                yes,
                enable,
            }) => plugin_cli::install(&socket, &source, yes, enable)?,
            Some(PluginCommand::Remove { name }) => plugin_cli::remove(&name)?,
            Some(PluginCommand::New { name }) => plugin_cli::new(&name)?,
            Some(PluginCommand::Log { name }) => plugin_cli::log(&socket, &name)?,
        },
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
        Command::Mcp { dir } => mcp::run(&socket, &here(dir)?)?,
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

/// A table of the profiles: one a row, its description last, since it's
/// the one with spaces.
fn print_profiles(profiles: &[Profile]) {
    if profiles.is_empty() {
        println!(
            "no profiles yet: add one with P in the TUI, or in {}",
            shell::home_relative(&config::path())
        );
        return;
    }
    let rows: Vec<[String; 4]> = profiles
        .iter()
        .map(|profile| {
            let place = match profile.start_in {
                Some(StartIn::Here) => "here",
                Some(StartIn::Worktree) => "worktree",
                None => "-",
            };
            [
                profile.name.clone(),
                profile.agent.clone(),
                place.to_string(),
                profile.description.clone().unwrap_or_default(),
            ]
        })
        .collect();
    print_table(["NAME", "AGENT", "WHERE", "DESCRIPTION"], &rows);
}

/// What the profile called `name` is: its agent, where it starts, and the
/// command it runs for a task, which is written `<task>`.
fn print_profile(profiles: &[Profile], name: &str) -> Result<()> {
    let Some(profile) = profiles.iter().find(|profile| profile.name == name) else {
        bail!("there's no profile called {name}; `crystal profile` lists them");
    };
    let agent = catalog::find(&profile.agent).map_or(profile.agent.as_str(), |agent| agent.name);
    let place = match profile.start_in {
        Some(StartIn::Here) => "where the selected session runs",
        Some(StartIn::Worktree) => "in a new worktree",
        None => "wherever the new-session panel is set",
    };
    let command: Vec<String> = profile
        .command("<task>")
        .iter()
        .map(|arg| shell::quote(arg))
        .collect();
    println!("{}", profile.name);
    if let Some(description) = &profile.description {
        println!("  {description}");
    }
    println!("agent   {agent}");
    println!("starts  {place}");
    println!("runs    {}", command.join(" "));
    Ok(())
}

fn new_session(
    socket: &Path,
    name: Option<String>,
    cwd: Option<PathBuf>,
    worktree: Option<String>,
    detached: bool,
    mut command: Vec<String>,
    task: Option<String>,
) -> Result<()> {
    let cwd = start_dir(socket, cwd, worktree)?;
    // A task given with `-t` goes to the agent as its first prompt; one
    // given as the agent's only argument is a task all the same.
    let task = match task {
        Some(task) => {
            catalog::add_first_prompt(&mut command, &task);
            Some(task)
        }
        None => catalog::first_prompt_in(&command),
    };
    let purpose = client::Purpose {
        task,
        backlog: None,
    };
    let name = client::new_session_for(socket, name, cwd, command, purpose)?;
    attach_or_print(socket, &name, detached)
}

/// Attaches to the new session `name` when run in a terminal, unless
/// `detached`; prints its name otherwise.
fn attach_or_print(socket: &Path, name: &str, detached: bool) -> Result<()> {
    let in_a_terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if in_a_terminal && !detached {
        attach::run(socket, Some(name))
    } else {
        println!("{name}");
        Ok(())
    }
}

/// `crystal backlog` and its commands, for the project `dir` is in.
fn backlog(
    socket: &Path,
    dir: PathBuf,
    all: bool,
    json: bool,
    command: Option<BacklogCommand>,
) -> Result<()> {
    use work::BacklogAction;
    let action = match command {
        None => BacklogAction::List { all, json },
        Some(BacklogCommand::Add { text, tags }) => BacklogAction::Add {
            text: text.join(" "),
            tags,
        },
        Some(BacklogCommand::Done { number }) => BacklogAction::Mark { number, done: true },
        Some(BacklogCommand::Reopen { number }) => BacklogAction::Mark {
            number,
            done: false,
        },
        Some(BacklogCommand::Rm { number }) => BacklogAction::Remove { number },
        Some(BacklogCommand::Export) => BacklogAction::Export,
        Some(BacklogCommand::Start {
            number,
            worktree,
            name,
            detached,
        }) => {
            let name = work::start_from_backlog(socket, dir, number, worktree, name)?;
            return attach_or_print(socket, &name, detached);
        }
    };
    work::change_backlog(socket, dir, action)
}

/// `crystal flow` and its commands.
fn flow(socket: &Path, json: bool, command: Option<FlowCommand>) -> Result<()> {
    match command {
        None => flow_cli::list(socket, json),
        Some(FlowCommand::Run {
            flow,
            goal,
            cwd,
            wait,
        }) => {
            let cwd = start_dir(socket, cwd, None)?;
            flow_cli::run(socket, &flow, &goal.join(" "), cwd, wait)
        }
        Some(FlowCommand::Show { run, json }) => flow_cli::show(socket, &run, json),
        Some(FlowCommand::Approve { run }) => flow_cli::approve(socket, &run),
        Some(FlowCommand::Back { run, notes }) => flow_cli::back(socket, &run, &notes.join(" ")),
        Some(FlowCommand::Retry { run }) => flow_cli::retry(socket, &run),
        Some(FlowCommand::Wait { run, timeout }) => flow_cli::wait(socket, &run, seconds(timeout)),
        Some(FlowCommand::Example) => {
            flow_cli::example();
            Ok(())
        }
    }
}

/// The directory a command about a project is given with `-C`, or the
/// current one.
fn here(dir: Option<PathBuf>) -> Result<PathBuf> {
    match dir {
        Some(dir) => Ok(std::path::absolute(dir)?),
        None => Ok(std::env::current_dir()?),
    }
}

/// Where a new session or task starts: `cwd`, or the current directory,
/// or else a new worktree on the branch `worktree` made from there.
fn start_dir(socket: &Path, cwd: Option<PathBuf>, worktree: Option<String>) -> Result<PathBuf> {
    let cwd = match cwd {
        Some(cwd) => std::path::absolute(cwd)?,
        None => std::env::current_dir()?,
    };
    match worktree {
        Some(branch) => client::add_worktree(socket, &cwd, &branch),
        None => Ok(cwd),
    }
}

/// Removes the worktree `target` names: a directory, or the branch it has
/// checked out. With `force`, though it has changes not committed.
fn remove_worktree(socket: &Path, target: &str, force: bool) -> Result<()> {
    let path = git::find_worktree(&std::env::current_dir()?, target)?;
    client::remove_worktree(socket, &path, force)
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
    let rows: Vec<[String; 9]> = sessions
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
                    .front
                    .as_ref()
                    .map_or("-".into(), |front| front.word().to_string()),
                session
                    .command
                    .iter()
                    .map(|arg| shell::quote(arg))
                    .collect::<Vec<_>>()
                    .join(" "),
                task_cell(session),
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
        "PROGRAM",
        "COMMAND",
        "TASK",
    ];
    print_table(header, &rows);
}

/// Prints `rows` under `header`, each column as wide as its widest cell.
fn print_table<const N: usize>(header: [&str; N], rows: &[[String; N]]) {
    let header = header.map(String::from);
    let mut widths = [0; N];
    for row in std::iter::once(&header).chain(rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    for row in std::iter::once(&header).chain(rows) {
        let line: Vec<String> = row
            .iter()
            .zip(widths)
            .map(|(cell, width)| format!("{cell:width$}"))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
}

/// A session's task, for `ls`: what it was asked to do while it's open,
/// and how it went once it's closed.
fn task_cell(session: &SessionInfo) -> String {
    let Some(task) = &session.task else {
        return "-".into();
    };
    let goal = task.goal.lines().next().unwrap_or("");
    match &task.outcome {
        None => goal.to_string(),
        Some(outcome) => {
            let mark = if outcome.failed { "✗" } else { "✓" };
            let said = if outcome.summary.is_empty() {
                goal
            } else {
                &outcome.summary
            };
            format!("{mark} {said}")
        }
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
