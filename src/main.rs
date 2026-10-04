mod agent_cli;
mod agent_hooks;
mod agent_plugins;
mod agent_rules;
mod agent_screen;
mod agents;
mod artifacts;
mod attach;
mod backlog;
mod bell;
mod catalog;
mod claude_stream;
mod client;
mod clipboard;
mod codex;
mod completions;
mod config;
mod daemon;
mod db;
mod distill;
mod drive;
mod embed;
mod env;
mod event_log;
mod events;
mod events_cli;
mod flow_cli;
mod flow_run;
mod flows;
mod forge;
mod front;
mod git;
mod handoff;
mod handover;
mod hook;
mod integration;
mod keys;
mod layout;
mod layout_relay;
mod links;
mod markdown;
mod mcp;
mod memory;
mod memory_cli;
mod mermaid;
mod mermaid_cli;
mod messages;
mod model;
mod names;
mod notify;
mod plugin_cli;
mod plugin_hooks;
mod plugin_manifest;
mod plugins;
mod profile;
mod project;
mod project_cli;
mod project_commands;
mod protocol;
mod qwen3;
mod remote;
mod report;
mod rerank;
mod secrets;
mod server_cli;
mod session;
mod shell;
mod skill;
mod socket;
mod sound;
mod spending;
mod state;
mod syntax;
mod task;
mod tasks;
mod transcript;
mod tui;
mod typing;
mod update;
mod viewer;
mod vt;
mod work;

use anyhow::{Result, bail};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use client::Restart;
use profile::{Profile, StartIn};
use protocol::{ArchivedSession, Request, Response, SessionInfo, TaskSpec, TaskState};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;
use tui::split_tree::{Direction, Way};

/// One terminal for all your coding agents. With no command, opens the
/// TUI: every session in a sidebar, the selected one live beside it.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// The server to use: a daemon of its own, with its own sessions and
    /// state, made the first time it's named [env: CRYSTAL_SERVER]
    /// [default: default]
    #[arg(short = 'L', long, global = true, value_name = "NAME")]
    server: Option<String>,

    /// The daemon's socket, in place of a server's [env: CRYSTAL_SOCKET]
    /// [default: $XDG_RUNTIME_DIR/crystal/default.sock, or
    /// /tmp/crystal-<uid>/default.sock]
    #[arg(short = 'S', long, global = true, conflicts_with = "server")]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start a session running a command, or your shell if there's none,
    /// and attach to it when run in a terminal.
    New {
        /// The session's name [default: from its first prompt, or else the
        /// program's name]
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
        /// doesn't exist, from origin's default branch, fetched first.
        #[arg(short, long, value_name = "BRANCH")]
        worktree: Option<String>,

        /// With -w, where a new branch starts: a branch (origin's copy, when
        /// it has one, fetched first), a tag, a commit, or HEAD for the one
        /// you're on [default: `[worktrees] base`, or else origin's default
        /// branch]
        #[arg(long, value_name = "REF", requires = "worktree")]
        base: Option<String>,

        /// Give the session this to do, which makes it a task: open until
        /// it's closed with `crystal done`. An agent crystal knows gets it
        /// as its first prompt. A prompt given as the agent's only
        /// argument, like `claude "fix it"`, makes a task too.
        #[arg(short, long, value_name = "TEXT")]
        task: Option<String>,

        /// Set a variable in the session's environment, over the one this
        /// runs with. Can be given more than once.
        #[arg(short, long = "env", value_name = "KEY=VALUE", value_parser = variable)]
        env: Vec<(String, String)>,

        /// The command and its arguments [default: the shell `[terminal]`
        /// says, or yours]
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

        /// A file in the session's worktree to keep with the task, copied
        /// into crystal's state directory. Can be given more than once.
        #[arg(long = "artifact", value_name = "PATH")]
        artifacts: Vec<PathBuf>,

        /// What was done, or why it couldn't be. Several words are joined
        /// with spaces.
        summary: Vec<String>,
    },
    /// Leave a note for the sessions that work in this worktree after
    /// this one, in its `.crystal/handoff.md`: every agent started there is
    /// told to read it.
    Handoff {
        /// The session whose worktree it's for [default: the one this runs
        /// in]
        #[arg(short, long)]
        name: Option<String>,

        /// What the next session should know. Several words are joined
        /// with spaces.
        #[arg(required = true)]
        note: Vec<String>,
    },
    /// Tell the user something with a notification, the way crystal tells
    /// them a session needs them; a click on it takes them to the session.
    /// The notification settings count, `unfocused_only` among them.
    Notify {
        /// The session it's about, which a click takes the user to
        /// [default: the one this runs in, if any]
        #[arg(short, long)]
        name: Option<String>,

        /// What to tell them. Several words are joined with spaces.
        #[arg(required = true)]
        message: Vec<String>,
    },
    /// Say what the agent in this session is doing, for an agent crystal
    /// doesn't know or a script wrapped around one, and how to resume it
    /// after a restart: the command after `--`. Its reports are the
    /// session's status until `--release`. Or, with `--line` and
    /// `--model` alone, only put a line on its row in the sidebar, or its
    /// model.
    Report {
        /// What it's doing: working, waiting (on you, which `blocked` says
        /// too), idle (at its prompt) or done (with a turn).
        #[arg(
            value_enum,
            required_unless_present_any = ["session_only", "release", "line", "model"],
            conflicts_with_all = ["session_only", "release"]
        )]
        state: Option<ReportedState>,

        /// A short line under the session's row in the sidebar, like
        /// "indexing 40%"; "" takes it off. It doesn't take the status
        /// over.
        #[arg(long)]
        line: Option<String>,

        /// The model the agent runs on, shown on its row in place of what
        /// crystal reads; "" gives that back.
        #[arg(long)]
        model: Option<String>,

        /// How long the --line and --model go on showing unless they're
        /// reported again, like 30s, 5m or 2h, a day at most [default:
        /// until they're replaced]
        #[arg(long, value_name = "WHILE")]
        ttl: Option<String>,

        /// Who reports the --line and --model, for --seq: letters, digits
        /// and `:._-`.
        #[arg(long, value_name = "ID")]
        source: Option<String>,

        /// The report's number from its --source: one numbered no higher
        /// than the last came late, and is passed over.
        #[arg(long, value_name = "N")]
        seq: Option<u64>,

        /// The agent's name, as the sidebar and `ls` show it [default: the
        /// one it gave before, or what's in front in the session]
        #[arg(long, conflicts_with = "release")]
        agent: Option<String>,

        /// A line on what it's doing, like what it's waiting on you for.
        #[arg(short, long, requires = "state", conflicts_with_all = ["session_only", "release"])]
        message: Option<String>,

        /// The session [default: the one this runs in]
        #[arg(short, long)]
        name: Option<String>,

        /// Only say how to resume it: the command after `--`.
        #[arg(long, requires = "resume", conflicts_with = "release")]
        session_only: bool,

        /// Give the session's status back to crystal, and forget the resume
        /// command: the agent is leaving.
        #[arg(long)]
        release: bool,

        /// The command that resumes the agent's session after a restart,
        /// its first word a command on the PATH: `-- my-agent --resume 42`.
        #[arg(last = true, value_name = "COMMAND", conflicts_with = "release")]
        resume: Vec<String>,
    },
    /// List the tasks of the project this directory is in: those still
    /// open, then those waiting to start, then those closed, the latest
    /// first. Or make, start, show or cancel one.
    Tasks {
        /// With no command: every project's tasks.
        #[arg(long)]
        all: bool,

        /// With no command: the project's directory [default: the current
        /// one]
        #[arg(short = 'C', long = "dir", value_name = "DIR")]
        dir: Option<PathBuf>,

        /// With no command: print them as JSON.
        #[arg(long)]
        json: bool,

        #[command(subcommand)]
        command: Option<TasksCommand>,
    },
    /// Answer the permission a background task is waiting on you for: `y`
    /// lets the tool run, `always` lets it and keeps a rule for calls like
    /// it, so they aren't asked about again, and `n` says no.
    Answer {
        /// The task, by its number, like t12, or its session's name.
        task: String,

        #[arg(value_enum)]
        answer: Reply,

        /// With `n`, what Claude is told.
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Stop the run a background task is in the middle of. Its task stays
    /// open, waiting on you.
    Interrupt {
        /// The task, by its number, like t12, or its session's name.
        task: String,
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
        /// The task's name [default: from its prompt, or task, task-2…]
        #[arg(short, long)]
        name: Option<String>,

        /// The directory to start in [default: the current one]
        #[arg(short = 'c', long)]
        cwd: Option<PathBuf>,

        /// Start in a new git worktree on this branch, as `new -w` does.
        #[arg(short, long, value_name = "BRANCH")]
        worktree: Option<String>,

        /// With -w, where a new branch starts, as `new --base` says.
        #[arg(long, value_name = "REF", requires = "worktree")]
        base: Option<String>,

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
    /// Run flows: chains of tasks on one goal, from the config file's
    /// `[[flow]]` tables and the project's `.crystal/flows.toml`. With no
    /// command, lists the runs.
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
    /// The projects crystal knows, which the TUI lists even with no session
    /// running in them: those sessions have run in, and those added. Or add
    /// one, take one off the list, or run or open the project here with
    /// its own commands.
    Project {
        /// With no command: print them as JSON.
        #[arg(long)]
        json: bool,

        #[command(subcommand)]
        command: Option<ProjectCommand>,
    },
    /// List the sessions.
    #[command(visible_alias = "list")]
    Ls {
        /// Print them as a JSON array, for scripts and agents.
        #[arg(long)]
        json: bool,

        /// List the archived sessions instead.
        #[arg(long)]
        archived: bool,
    },
    /// Lay out the TUI's tabs: make one, go to one, name one, close one, or
    /// move a session to one. The TUI used last does it.
    Tab {
        #[command(subcommand)]
        command: TabCommand,
    },
    /// Lay out the panes of the TUI's tabs: split a session off beside
    /// another, focus one, resize, close, zoom or float one, or even them
    /// out. The TUI used last does it.
    Pane {
        #[command(subcommand)]
        command: PaneCommand,
    },
    /// Give the terminal the TUI runs in a title, in place of the one
    /// `[window] title` makes, or clear it to go back to that one. The TUI
    /// used last does it.
    Title {
        #[command(subcommand)]
        command: TitleCommand,
    },
    /// Print the TUI's tabs: each one's sessions, and how its panes split
    /// the room.
    Layout {
        /// Print them as JSON.
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

        /// Type it even while the agent is asking the user something, which
        /// is refused otherwise: the text would land in the question.
        #[arg(long)]
        force: bool,

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

        /// Wait for this instead, and print it once it's reached: working,
        /// waiting, done, idle, or ended (exited). Several, with commas
        /// between, wait for any of them.
        #[arg(
            long,
            value_enum,
            value_delimiter = ',',
            value_name = "STATUS",
            conflicts_with = "output"
        )]
        until: Vec<drive::Until>,

        /// Wait until a line on its screen, or just scrolled off it,
        /// matches this regular expression, and print the line.
        #[arg(long, value_name = "REGEX")]
        output: Option<String>,

        /// Give up after this many seconds, and fail.
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<f64>,
    },
    /// Print what happened, from the event log, one line each, the oldest
    /// first: sessions starting, working, waiting and ending, tasks, runs,
    /// flows, worktrees, memory and the backlog.
    Events {
        /// Only those since then: a while back, like 30m, 2h or 3d, or a
        /// time, like 14:00, 2026-10-01 or 2026-10-01T09:30.
        #[arg(long, value_name = "WHEN")]
        since: Option<String>,

        /// Only events of this kind, or family, like session.waiting or
        /// task.*; give it more than once for more.
        #[arg(short, long = "kind", value_name = "KIND")]
        kinds: Vec<String>,

        /// Only those about this session, through its renames.
        #[arg(short, long)]
        name: Option<String>,

        /// Only those about the project this directory is in.
        #[arg(short = 'C', long = "dir", value_name = "DIR")]
        dir: Option<PathBuf>,

        /// Print them as JSON, one object a line, as the log keeps them.
        #[arg(long)]
        json: bool,

        /// Then keep printing each new one as it happens; without --since,
        /// only new ones.
        #[arg(short, long)]
        follow: bool,
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
    /// under the same name. Claude Code comes back in its conversation. One
    /// that couldn't start again after a restart tries again.
    Respawn { name: String },
    /// Stop a session and remove it from the list. An archived one is
    /// taken out of the archive.
    Kill { name: String },
    /// Stop sessions and keep them in the archive, out of the list, to
    /// start again where they were with `unarchive`: an agent in its
    /// conversation. `ls --archived` lists them.
    Archive {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Start an archived session again, under its name or the next one
    /// free, and take it out of the archive. Attaches to it when run in a
    /// terminal.
    Unarchive {
        name: String,

        /// Don't attach: print the session's name.
        #[arg(short, long)]
        detached: bool,
    },
    /// Stop every session and the daemon.
    KillServer,
    /// Restart the daemon on this crystal, say after installing a new one.
    /// The daemon hands its sessions over to it, and they carry on running,
    /// their screens and all.
    RestartServer {
        /// Stop the daemon and start it again instead: running sessions
        /// come back, Claude Code in its conversation, other programs from
        /// the start.
        #[arg(long)]
        cold: bool,
    },
    /// Update crystal to its latest release, or to the one given: download
    /// it, check it against its checksum, put it in place of this one, and
    /// restart every running daemon on it, their sessions carrying on.
    Update {
        /// The release to install, like 0.4.0, rather than the latest.
        #[arg(conflicts_with = "check")]
        version: Option<String>,

        /// Only say whether a newer crystal is out.
        #[arg(long)]
        check: bool,
    },
    /// List the servers, each a daemon with its own sessions and state: the
    /// default one and those named with --server, with whether each is
    /// running and how many sessions it has. Or stop one, or delete one.
    #[command(visible_alias = "servers")]
    Server {
        /// With no command: print them as JSON.
        #[arg(long)]
        json: bool,

        #[command(subcommand)]
        command: Option<ServerCommand>,
    },
    /// Show where the config file is, and the settings in effect, as the
    /// file would hold them.
    Config,
    /// Remember something about this project for its later sessions: a
    /// decision, a gotcha, a command that works, a note.
    Remember {
        /// What sort of thing it is.
        #[arg(short, long, value_enum, default_value_t = memory::Kind::Note)]
        kind: memory::Kind,

        /// A file it's about; once some of its files change, the entry is
        /// marked drifting, and once all of them have, stale. Give it once
        /// a file.
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
    /// switch, run, install, build, make or remove one.
    Plugin {
        #[command(subcommand)]
        command: Option<PluginCommand>,
    },
    /// List the agents crystal reads the screens of, where their rules
    /// come from and their hooks; or show why it reads a session the way
    /// it does, print an agent's rules, or add hooks to an agent.
    Agent {
        #[command(subcommand)]
        command: Option<AgentCommand>,
    },
    /// Draw a mermaid diagram as text, the way crystal's previews draw it:
    /// a diagram, or each ```mermaid fence of a markdown file. One that
    /// can't be drawn is printed as it is, and the command fails saying
    /// why.
    Mermaid {
        /// The file [default: standard input]
        file: Option<String>,

        /// How many columns to draw in [default: the terminal's, or 80]
        #[arg(short, long, value_name = "COLUMNS")]
        width: Option<usize>,

        /// Draw with ASCII rather than box drawing.
        #[arg(long)]
        ascii: bool,
    },
    /// List the TUI's commands, the ids `[keys]` in the config file takes,
    /// and the keys that run them, as your config has them.
    Keys,
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
    /// Put crystal's hooks in an agent's own settings, or its plugin in its
    /// plugins: Claude Code, Codex, Cursor, Droid, Qoder, Qwen Code, Copilot,
    /// Devin, Kimi, Letta, MastraCode, Grok, Antigravity, Pi, OpenCode, Kilo
    /// or Hermes. Then the agent says what it's doing, as far as it can,
    /// and which conversation it's in, which a restart picks up again.
    Integration {
        #[command(subcommand)]
        command: IntegrationCommand,
    },
    /// Print the script that completes crystal's commands in your shell:
    /// see the README for where each shell wants it.
    Completions {
        #[arg(value_enum)]
        shell: completions::Target,
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
    Daemon {
        /// Carry on from the daemon that ran this crystal in its place,
        /// reading what it handed over from this descriptor.
        #[arg(long, value_name = "FD")]
        handover: Option<i32>,
    },
    /// Tell the daemon about an agent's event; what the agent's hooks run.
    #[command(hide = true)]
    Hook {
        agent: String,

        /// Run by the hooks `crystal integration` installed, which leave an
        /// agent crystal started with hooks of its own to those.
        #[arg(long)]
        installed: bool,

        /// The event to take the hook for, in place of the one its input
        /// names.
        #[arg(long)]
        event: Option<String>,
    },
    /// Print the running sessions' names, a line each, for a shell
    /// completing one: never starts the daemon, and says nothing when it
    /// can't ask.
    #[command(hide = true)]
    CompleteSessions,
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
enum IntegrationCommand {
    /// Add crystal's hooks, beside your own, to every event crystal
    /// listens to. Codex runs them once you've reviewed them in its
    /// `/hooks`.
    Install {
        /// The agent [default: each one installed here]
        agent: Option<integration::Agent>,
    },
    /// Take crystal's hooks out again, and only those.
    Uninstall {
        /// The agent [default: each one installed here]
        agent: Option<integration::Agent>,
    },
    /// Whether crystal's hooks are installed, for this crystal.
    Status {
        /// The agent [default: every one]
        agent: Option<integration::Agent>,
    },
}

#[derive(Subcommand)]
enum AgentCommand {
    /// List the agents crystal has rules for: where the rules come from,
    /// whether the agent is installed, and its hooks.
    List {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show why crystal reads a session's agent the way it does: what's in
    /// front, the rules tried on its screen, and the one that decided.
    Explain {
        /// The session [required without --file]
        #[arg(required_unless_present = "file")]
        session: Option<String>,

        /// Try the rules on a screen saved in a file instead, a row a line.
        #[arg(
            long,
            value_name = "PATH",
            conflicts_with = "session",
            requires = "agent"
        )]
        file: Option<PathBuf>,

        /// The agent whose rules to try [default: the one in front]
        #[arg(long, value_name = "AGENT")]
        agent: Option<String>,

        /// With --file, the title the agent gave its terminal.
        #[arg(long, requires = "file", default_value = "")]
        title: String,

        /// With --file, the progress it reported (OSC 9;4), like `4;3`.
        #[arg(long, requires = "file", default_value = "")]
        progress: String,

        /// Show the text each rule looked at.
        #[arg(short, long)]
        verbose: bool,

        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the rules crystal comes with for an agent, to start a file of
    /// your own from.
    Rules { agent: String },
}

#[derive(Subcommand)]
enum TasksCommand {
    /// Make a task, and start it: the agent the new-session panel starts
    /// first, given the goal as its first prompt, or Claude in the
    /// background with --background. Prints the task's number.
    New {
        /// The session's name [default: from its goal, or else the
        /// program's]
        #[arg(short, long)]
        name: Option<String>,

        /// The directory to start in [default: the current one]
        #[arg(short = 'c', long)]
        cwd: Option<PathBuf>,

        /// Start in a new git worktree on this branch, as `new -w` does,
        /// made now.
        #[arg(short, long, value_name = "BRANCH")]
        worktree: Option<String>,

        /// With -w, where a new branch starts, as `new --base` says.
        #[arg(long, value_name = "REF", requires = "worktree")]
        base: Option<String>,

        /// Run it in the background, as `crystal task` does.
        #[arg(long)]
        background: bool,

        /// Don't start it: it waits, pending, until `crystal tasks start`.
        #[arg(long)]
        no_launch: bool,

        /// What it's to do. Several words are joined with spaces.
        #[arg(required = true)]
        goal: Vec<String>,

        /// With --background, arguments for its `claude -p`, after `--`.
        #[arg(last = true, value_name = "CLAUDE ARGS", requires = "background")]
        claude_args: Vec<String>,
    },
    /// Start a task made with --no-launch. Prints its session's name.
    Start {
        /// The task's number, like t12.
        id: String,
    },
    /// Show a task: how it stands, its session, what it's asking for and
    /// has cost, and how it went.
    Show {
        /// The task, by its number, like t12, or its session's name.
        task: String,

        /// Print it as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Cancel a task, and stop the session working on it.
    Cancel {
        /// The task, by its number, like t12, or its session's name.
        task: String,
    },
    /// What happened to a task: how it stands, then its session's
    /// transcript, while the session is still there.
    Log {
        /// The task, by its number, like t12, or its session's name.
        task: String,
    },
}

/// What an agent says it's doing with `crystal report`.
#[derive(Clone, Copy, clap::ValueEnum)]
enum ReportedState {
    Working,
    #[value(alias = "blocked")]
    Waiting,
    Idle,
    Done,
}

impl ReportedState {
    fn activity(self) -> protocol::Activity {
        match self {
            ReportedState::Working => protocol::Activity::Working,
            ReportedState::Waiting => protocol::Activity::Waiting,
            ReportedState::Idle => protocol::Activity::Idle,
            ReportedState::Done => protocol::Activity::Done,
        }
    }
}

/// An answer to a permission a background task asks for.
#[derive(Clone, Copy, clap::ValueEnum)]
enum Reply {
    #[value(name = "y", alias = "yes")]
    Yes,
    #[value(name = "n", alias = "no")]
    No,
    Always,
}

#[derive(Subcommand)]
enum MemoryCommand {
    /// The entries that have to do with these words, the best first.
    Search {
        #[arg(required = true)]
        words: Vec<String>,
    },
    /// An entry in full, by its id: its text, its files, where it came from
    /// and how often it was said.
    Show { id: u64 },
    /// Print every entry as markdown, newest first.
    Export,
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
        /// The flow, by its name: the project's own, or the config file's.
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
    /// Cancel a run: the task of the step it's at is cancelled and its
    /// session stopped, and the run goes no further.
    Cancel { run: String },
    /// List the flows a run started here would find, and where each is
    /// written: the config file, or the project's `.crystal/flows.toml`.
    Defs {
        /// The project's directory [default: the current one]
        #[arg(short = 'C', long = "dir", value_name = "DIR")]
        dir: Option<PathBuf>,
    },
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
enum TabCommand {
    /// Make a tab after the others and go to it: sessions started from then
    /// on go in it. Prints its number.
    New {
        /// What to call it [default: its number]
        name: Option<String>,
    },
    /// Go to a tab.
    Select {
        /// The tab: its number, from 1, or its name.
        tab: String,
    },
    /// Name a tab. An empty name takes it back to its number.
    Rename { tab: String, name: String },
    /// Close a tab.
    Close {
        /// The tab [default: the one in front]
        tab: Option<String>,

        /// Kill its sessions with it: a tab with sessions in it doesn't
        /// close without.
        #[arg(long)]
        kill: bool,
    },
    /// Move a session to another tab.
    Move { session: String, tab: String },
    /// Move a tab to another place among the tabs, the others making room.
    Reorder {
        /// The tab: its number, from 1, or its name.
        tab: String,

        /// The number it takes, from 1.
        #[arg(value_parser = clap::value_parser!(u64).range(1..))]
        position: u64,
    },
}

#[derive(Subcommand)]
enum TitleCommand {
    /// Set the title.
    Set {
        /// The title. Several words are joined with spaces.
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Go back to the title `[window] title` makes.
    Clear,
}

#[derive(Subcommand)]
enum PaneCommand {
    /// Show a session in a pane of its own, split off to the right of, or
    /// below, the pane of the session this runs in, or else the selected
    /// one's; with no session, a new shell, whose name it prints.
    Split {
        /// The session to show. One in another tab moves to this one.
        /// [default: a new shell, in the current directory]
        session: Option<String>,

        /// The new shell's directory [default: the current one]
        #[arg(short = 'c', long, conflicts_with = "session")]
        cwd: Option<PathBuf>,

        /// Set a variable in the new shell's environment, over the one
        /// this runs with. Can be given more than once.
        #[arg(
            short,
            long = "env",
            value_name = "KEY=VALUE",
            value_parser = variable,
            conflicts_with = "session"
        )]
        env: Vec<(String, String)>,

        /// The session whose pane to split [default: the one this runs in,
        /// or else the selected one]
        #[arg(long, value_name = "SESSION")]
        beside: Option<String>,

        /// To the right of it: the default.
        #[arg(long, conflicts_with = "down")]
        right: bool,

        /// Below it.
        #[arg(long)]
        down: bool,

        /// The share of the room the pane split keeps, from 0.1 to 0.9.
        #[arg(long, default_value_t = 0.5, value_parser = share)]
        ratio: f32,
    },
    /// Select a session, bringing its tab to the front, and type into it;
    /// or, given left, right, up or down, the session in the pane that way
    /// from the pane of the session this runs in, or else the selected
    /// one's.
    Focus {
        /// A session's name, or left, right, up or down.
        target: String,

        /// Bring the TUI's terminal to the front too, as a click on a
        /// notification does.
        #[arg(long)]
        raise: bool,
    },
    /// Move a border of a session's pane: the one on that side, which it
    /// grows into, or else the one on its other side, which it shrinks
    /// from.
    Resize {
        #[arg(value_enum)]
        direction: Toward,

        /// How many columns or rows [default: 4 columns or 2 rows, as resize
        /// mode moves]
        cells: Option<u16>,

        /// The session [default: the one this runs in, or else the selected
        /// one]
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Close a session's pane of its own: its split, the pane beside it
    /// taking the room, or its float.
    Close {
        /// The session [default: the one this runs in, or else the selected
        /// one]
        session: Option<String>,
    },
    /// Zoom a session's pane over the whole of its tab, selecting it there.
    Zoom {
        /// The session [default: the one this runs in, or else the selected
        /// one]
        session: Option<String>,

        /// Put the tab's panes back instead.
        #[arg(long)]
        off: bool,
    },
    /// Even out the panes of the tab this runs in, or else the one in front.
    Equalize,
    /// Float a session over its tab's panes, in a pane of its own.
    Float {
        /// The session [default: the one this runs in, or else the selected
        /// one]
        session: Option<String>,

        /// Put the session floating over the tab back among the panes
        /// instead.
        #[arg(long)]
        off: bool,
    },
}

/// A way to go from a pane, as the command line says it.
#[derive(Clone, Copy, clap::ValueEnum)]
enum Toward {
    Left,
    Right,
    Up,
    Down,
}

impl From<Toward> for Direction {
    fn from(toward: Toward) -> Direction {
        match toward {
            Toward::Left => Direction::Left,
            Toward::Right => Direction::Right,
            Toward::Up => Direction::Up,
            Toward::Down => Direction::Down,
        }
    }
}

/// A variable for a session's environment, as `--env` takes it:
/// `KEY=VALUE`, the value maybe empty or with `=` in it.
fn variable(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((key, value)) if !key.is_empty() && !key.contains(char::is_whitespace) => {
            Ok((key.to_string(), value.to_string()))
        }
        _ => Err("write it KEY=VALUE".into()),
    }
}

/// A share of a pane's room, as `--ratio` takes it.
fn share(text: &str) -> Result<f32, String> {
    let share: f32 = text.parse().map_err(|_| format!("{text} isn't a number"))?;
    if (0.1..=0.9).contains(&share) {
        Ok(share)
    } else {
        Err("it's a share of the room, from 0.1 to 0.9".into())
    }
}

#[derive(Subcommand)]
enum ServerCommand {
    /// Stop a server and every session in it, as kill-server does.
    Stop { name: String },
    /// Delete a stopped server: its saved sessions, tabs, layouts, backlog,
    /// tasks and memory. Refuses while it's running, and for the default
    /// server.
    #[command(visible_alias = "rm")]
    Delete { name: String },
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
enum ProjectCommand {
    /// Put a project on the list: the repository a directory is in.
    Add {
        /// A directory in the project [default: the current one]
        dir: Option<PathBuf>,
    },
    /// Take a project off the list. Its backlog, tasks and memory stay,
    /// and it's back once a session runs there. Refuses while one does.
    #[command(visible_alias = "remove")]
    Rm {
        /// A directory in the project [default: the current one]
        dir: Option<PathBuf>,
    },
    /// Run the project in this worktree, with the `run` command from its
    /// `.crystal/project.toml` or the config's `[[project]]`: a session of
    /// its own, called `run-` and the worktree's directory. Attaches to it
    /// when run in a terminal.
    Run {
        /// A directory in the worktree [default: the current one]
        #[arg(short = 'C', long = "dir", value_name = "DIR")]
        dir: Option<PathBuf>,

        /// Stop the session running it instead.
        #[arg(long)]
        stop: bool,

        /// Don't attach: print the session's name.
        #[arg(short, long)]
        detached: bool,
    },
    /// Open this worktree with the project's `open` command, like `code .`.
    Open {
        /// A directory in the worktree [default: the current one]
        #[arg(short = 'C', long = "dir", value_name = "DIR")]
        dir: Option<PathBuf>,
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
    /// Run one of a plugin's actions, or try its hooks out on an event.
    Run {
        plugin: String,

        /// The action, by its id.
        #[arg(required_unless_present_any = ["event", "link"])]
        action: Option<String>,

        /// Run the plugin's hooks on a made-up event of this kind, like
        /// session.waiting, here and now, on or off, and print what they
        /// print.
        #[arg(long, value_name = "KIND", conflicts_with = "action")]
        event: Option<String>,

        /// Run the action the plugin's link handlers give this link, with
        /// it in CRYSTAL_LINK, as a Ctrl+click on it in a pane would.
        #[arg(long, value_name = "URL", conflicts_with_all = ["action", "event"])]
        link: Option<String>,

        /// The session to run it for [default: the one this runs in, if
        /// any]
        #[arg(short, long)]
        session: Option<String>,
    },
    /// Install a plugin from a git repository or a directory, once you've
    /// seen what it runs and said yes, and build it. It starts off.
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
    /// Run a plugin's build commands again. One that fails turns it off
    /// until a build works.
    Build { name: String },
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

        /// The session's name [default: from the item, or else the agent's
        /// name]
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
    let socket = std::path::absolute(socket::chosen(cli.socket, cli.server.clone())?)?;
    let Some(command) = cli.command else {
        return tui::run(&socket);
    };
    match command {
        Command::New {
            name,
            cwd,
            detached,
            worktree,
            base,
            task,
            env,
            command,
        } => {
            let worktree = NewWorktree::from_args(worktree, base);
            let new = NewArgs {
                name,
                cwd,
                worktree,
                detached,
                command,
                task,
                env,
            };
            new_session(&socket, new)?
        }
        Command::Done {
            name,
            failed,
            artifacts,
            summary,
        } => work::done(&socket, name, failed, &summary.join(" "), artifacts)?,
        Command::Handoff { name, note } => work::handoff(&socket, name, &note.join(" "))?,
        Command::Notify { name, message } => {
            let id = name
                .is_none()
                .then(|| env::own_session_id(&socket))
                .flatten();
            let request = Request::Notify {
                text: message.join(" "),
                id,
                name,
            };
            match client::ask(&socket, &request, true)? {
                Some(Response::Done) => {}
                _ => bail!("the daemon answered something else"),
            }
        }
        Command::Report {
            state,
            line,
            model,
            ttl,
            source,
            seq,
            agent,
            message,
            name,
            session_only: _,
            release,
            resume,
        } => {
            let shows = line.is_some() || model.is_some();
            if !shows && (ttl.is_some() || source.is_some() || seq.is_some()) {
                bail!("--ttl, --source and --seq go with --line or --model");
            }
            let metadata = if shows {
                Some(protocol::Metadata {
                    line,
                    model,
                    ttl_secs: ttl.as_deref().map(report_ttl).transpose()?,
                    source,
                    seq,
                })
            } else {
                None
            };
            let resume = (!resume.is_empty()).then_some(resume);
            let report = match (state, resume) {
                _ if release => Some(protocol::AgentReport::Release),
                (Some(state), resume) => Some(protocol::AgentReport::State {
                    agent,
                    state: state.activity(),
                    message,
                    resume,
                }),
                (None, Some(argv)) => Some(protocol::AgentReport::Resume { agent, argv }),
                (None, None) if shows => None,
                (None, None) => bail!("say what the agent is doing"),
            };
            report::run(&socket, name, metadata, report)?;
        }
        Command::Tasks {
            all,
            dir,
            json,
            command,
        } => tasks(&socket, all, dir, json, command)?,
        Command::Answer {
            task,
            answer,
            message,
        } => {
            let answer = match answer {
                Reply::Yes => protocol::Answer::Allow,
                Reply::No => protocol::Answer::Deny,
                Reply::Always => protocol::Answer::Always,
            };
            drive::answer(&socket, &task, answer, message)?;
        }
        Command::Interrupt { task } => drive::interrupt(&socket, &task)?,
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
            base,
            wait,
            timeout,
            prompt,
            claude_args,
        } => {
            let spec = TaskSpec {
                prompt: prompt.join(" "),
                args: claude_args,
            };
            let cwd = start_dir(&socket, cwd, NewWorktree::from_args(worktree, base))?;
            let name = client::new_task(&socket, name, cwd, spec, None)?.name;
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
        Command::Project { json, command } => project(&socket, json, command)?,
        Command::Tab { command } => tab(&socket, command)?,
        Command::Pane { command } => pane(&socket, command)?,
        Command::Title { command } => {
            let text = match command {
                TitleCommand::Set { text } => Some(text.join(" ")),
                TitleCommand::Clear => None,
            };
            client::lay_out(&socket, layout::Command::Title { text })?;
        }
        Command::Layout { json } => {
            let layout = client::lay_out(&socket, layout::Command::Show)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&layout)?);
            } else {
                print!("{}", layout.text());
            }
        }
        Command::Attach { name } => attach::run(&socket, name.as_deref())?,
        Command::Ls {
            json,
            archived: true,
        } => {
            let archived = match client::ask(&socket, &Request::Archived, false)? {
                Some(Response::Archived { sessions }) => sessions,
                _ => Vec::new(),
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&archived)?);
            } else {
                print_archived(&archived);
            }
        }
        Command::Ls { json, .. } => {
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
            force,
            wait,
            timeout,
        } => {
            drive::send(&socket, &name, &text.join(" "), !no_enter, force)?;
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
        Command::Wait {
            name,
            until,
            output,
            timeout,
        } => {
            let timeout = seconds(timeout);
            match output {
                Some(pattern) => drive::wait_for_output(&socket, &name, &pattern, timeout)?,
                None if until.is_empty() => drive::wait(&socket, &name, timeout)?,
                None => drive::wait_until(&socket, &name, &until, timeout)?,
            }
        }
        Command::Events {
            since,
            kinds,
            name,
            dir,
            json,
            follow,
        } => {
            let options = events_cli::Options {
                since,
                kinds,
                session: name,
                dir: dir.map(|dir| here(Some(dir))).transpose()?,
                json,
                follow,
            };
            events_cli::run(&socket, options)?;
        }
        Command::Read {
            name,
            lines,
            history,
        } => drive::read(&socket, &name, lines, history)?,
        Command::Rename { name, new_name } => client::rename(&socket, &name, &new_name)?,
        Command::Respawn { name } => client::respawn(&socket, &name)?,
        Command::Archive { names } => {
            for name in names {
                if client::ask(&socket, &Request::Archive { name }, false)?.is_none() {
                    no_daemon(&socket)?;
                }
            }
        }
        Command::Unarchive { name, detached } => {
            let request = Request::Unarchive {
                name,
                env: env::current(),
            };
            let name = match client::ask(&socket, &request, true)? {
                Some(Response::Created { name, .. }) => name,
                _ => bail!("the daemon didn't start it"),
            };
            attach_or_print(&socket, &name, detached)?;
        }
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
        Command::RestartServer { cold } => match client::restart_daemon(&socket, cold)? {
            Restart::NoDaemon => println!("no daemon was running"),
            Restart::HandedOver { sessions: 0 } | Restart::Cold { why: None } => {
                println!("restarted the daemon");
            }
            Restart::HandedOver { .. } => {
                println!("restarted the daemon, and its sessions carried on")
            }
            Restart::Cold { why: Some(why) } => {
                println!("restarted the daemon; its sessions started again, since {why}");
            }
        },
        Command::Update { version, check } => update::run(&socket, version, check)?,
        Command::Server { json, command } => match command {
            None => server_cli::list(json)?,
            Some(ServerCommand::Stop { name }) => server_cli::stop(&name)?,
            Some(ServerCommand::Delete { name }) => server_cli::delete(&name)?,
        },
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
            Some(MemoryCommand::Show { id }) => memory_cli::show(&socket, dir, id)?,
            Some(MemoryCommand::Export) => memory_cli::export(&socket, dir)?,
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
                event,
                link,
                session,
            }) => {
                let code = match (action, event, link) {
                    (_, Some(event), _) => {
                        plugin_cli::run_event(&socket, &plugin, &event, session)?
                    }
                    (_, None, Some(link)) => {
                        plugin_cli::run_link(&socket, &plugin, &link, session)?
                    }
                    (action, None, None) => {
                        let action = action.unwrap_or_default();
                        plugin_cli::run(&socket, &plugin, &action, session, None)?
                    }
                };
                // The action's or the hook's own exit code is crystal's.
                std::process::exit(code);
            }
            Some(PluginCommand::Install {
                source,
                yes,
                enable,
            }) => plugin_cli::install(&socket, &source, yes, enable)?,
            Some(PluginCommand::Build { name }) => plugin_cli::build(&socket, &name)?,
            Some(PluginCommand::Remove { name }) => plugin_cli::remove(&name)?,
            Some(PluginCommand::New { name }) => plugin_cli::new(&name)?,
            Some(PluginCommand::Log { name }) => plugin_cli::log(&socket, &name)?,
        },
        Command::Mermaid { file, width, ascii } => mermaid_cli::run(file.as_deref(), width, ascii)?,
        Command::Keys => {
            let config = config::Config::load()?;
            let keymap = tui::keymap::Keymap::new(&config.keys).map_err(anyhow::Error::msg)?;
            print!("{}", tui::keymap::listing(&keymap));
        }
        Command::Skill { install, force } => {
            if install {
                skill::install(force)?;
            } else {
                skill::print();
            }
        }
        Command::Integration { command } => run_integration(command)?,
        Command::Completions { shell } => print!("{}", completions::script(shell, Cli::command())),
        Command::Ssh {
            install,
            destination,
            mut args,
        } => {
            // A server named for `crystal ssh` is one over there.
            if let Some(server) = cli.server {
                args.splice(0..0, ["--server".to_string(), server]);
            }
            let code = remote::run(&destination, &args, install)?;
            // The remote command's own exit code is crystal's.
            std::process::exit(code);
        }
        Command::Agent { command } => match command {
            None | Some(AgentCommand::List { json: false }) => agent_cli::list(false)?,
            Some(AgentCommand::List { json: true }) => agent_cli::list(true)?,
            Some(AgentCommand::Explain {
                session,
                file,
                agent,
                title,
                progress,
                verbose,
                json,
            }) => match (file, session) {
                (Some(file), _) => {
                    let agent = agent.as_deref().unwrap_or(agent_rules::DEFAULT);
                    agent_cli::explain_file(&file, agent, &title, &progress, verbose, json)?;
                }
                (None, Some(session)) => {
                    agent_cli::explain(&socket, &session, agent.as_deref(), verbose, json)?;
                }
                (None, None) => unreachable!("clap asks for a session or a file"),
            },
            Some(AgentCommand::Rules { agent }) => agent_cli::rules(&agent)?,
        },
        Command::Daemon { handover } => daemon::run(&socket, handover)?,
        Command::Hook {
            agent,
            installed,
            event,
        } => hook::run(&socket, &agent, installed, event.as_deref()),
        Command::CompleteSessions => {
            if let Ok(Some(Response::Sessions { sessions })) =
                client::ask(&socket, &Request::List, false)
            {
                for session in sessions {
                    println!("{}", session.name);
                }
            }
        }
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

/// What `crystal new` was asked for.
struct NewArgs {
    name: Option<String>,
    cwd: Option<PathBuf>,
    worktree: Option<NewWorktree>,
    detached: bool,
    command: Vec<String>,
    task: Option<String>,
    env: Vec<(String, String)>,
}

fn new_session(socket: &Path, new: NewArgs) -> Result<()> {
    let NewArgs {
        name,
        cwd,
        worktree,
        detached,
        mut command,
        task,
        env,
    } = new;
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
    let name = client::new_session_with(socket, name, cwd, command, purpose, &env)?.name;
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

/// `crystal project` and its commands.
fn project(socket: &Path, json: bool, command: Option<ProjectCommand>) -> Result<()> {
    match command {
        None => project_cli::list(socket, json),
        Some(ProjectCommand::Add { dir }) => project_cli::change(socket, &here(dir)?, true),
        Some(ProjectCommand::Rm { dir }) => project_cli::change(socket, &here(dir)?, false),
        Some(ProjectCommand::Run {
            dir,
            stop,
            detached,
        }) => match project_cli::run(socket, &here(dir)?, stop)? {
            Some(name) => attach_or_print(socket, &name, detached),
            None => Ok(()),
        },
        Some(ProjectCommand::Open { dir }) => project_cli::open(&here(dir)?),
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

/// `crystal tasks` and its commands.
fn tasks(
    socket: &Path,
    all: bool,
    dir: Option<PathBuf>,
    json: bool,
    command: Option<TasksCommand>,
) -> Result<()> {
    match command {
        None => work::list_tasks(socket, here(dir)?, all, json),
        Some(TasksCommand::New {
            name,
            cwd,
            worktree,
            base,
            background,
            no_launch,
            goal,
            claude_args,
        }) => {
            let task = work::NewTask {
                goal: goal.join(" "),
                cwd: start_dir(socket, cwd, NewWorktree::from_args(worktree, base))?,
                name,
                background,
                claude_args,
                launch: !no_launch,
            };
            work::new_task(socket, task)
        }
        Some(TasksCommand::Start { id }) => work::start_task(socket, &id),
        Some(TasksCommand::Show { task, json }) => work::show_task(socket, &task, json),
        Some(TasksCommand::Cancel { task }) => work::cancel_task(socket, &task),
        Some(TasksCommand::Log { task }) => work::task_log(socket, &task),
    }
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
        Some(FlowCommand::Cancel { run }) => flow_cli::cancel(socket, &run),
        Some(FlowCommand::Defs { dir }) => flow_cli::defs(&here(dir)?),
        Some(FlowCommand::Wait { run, timeout }) => flow_cli::wait(socket, &run, seconds(timeout)),
        Some(FlowCommand::Example) => {
            flow_cli::example();
            Ok(())
        }
    }
}

/// `crystal integration` and its commands, for the agent named or, without
/// one, each installed here; `status` without one is about both.
fn run_integration(command: IntegrationCommand) -> Result<()> {
    let crystal = std::env::current_exe()?;
    match command {
        IntegrationCommand::Install { agent } => {
            for agent in integration::chosen(agent)? {
                for line in integration::install(agent, &crystal)? {
                    println!("{line}");
                }
            }
        }
        IntegrationCommand::Uninstall { agent } => {
            for agent in integration::chosen(agent)? {
                println!("{}", integration::uninstall(agent)?);
            }
        }
        IntegrationCommand::Status { agent } => {
            let agents = agent.map_or(integration::Agent::ALL.to_vec(), |agent| vec![agent]);
            for agent in agents {
                println!("{}", integration::status(agent, &crystal)?);
            }
        }
    }
    Ok(())
}

/// `crystal tab` and its commands. A new tab's number is printed.
fn tab(socket: &Path, command: TabCommand) -> Result<()> {
    let new = matches!(command, TabCommand::New { .. });
    let command = match command {
        TabCommand::New { name } => layout::Command::NewTab { name },
        TabCommand::Select { tab } => layout::Command::SelectTab { tab },
        TabCommand::Rename { tab, name } => layout::Command::RenameTab { tab, name },
        TabCommand::Close { tab, kill } => layout::Command::CloseTab { tab, kill },
        TabCommand::Move { session, tab } => layout::Command::MoveToTab { session, tab },
        TabCommand::Reorder { tab, position } => layout::Command::ReorderTab {
            tab,
            position: position as usize,
        },
    };
    let layout = client::lay_out(socket, command)?;
    if new && let Some(tab) = layout.current() {
        println!("{}", tab.number);
    }
    Ok(())
}

/// `crystal pane` and its commands.
fn pane(socket: &Path, command: PaneCommand) -> Result<()> {
    let command = match command {
        PaneCommand::Split {
            session,
            cwd,
            env,
            beside,
            right: _,
            down,
            ratio,
        } => {
            let session = match session {
                Some(session) => session,
                None => {
                    let cwd = here(cwd)?;
                    let purpose = client::Purpose::default();
                    let new =
                        client::new_session_with(socket, None, cwd, Vec::new(), purpose, &env);
                    let name = new?.name;
                    println!("{name}");
                    name
                }
            };
            layout::Command::Split {
                session,
                beside,
                way: if down { Way::Down } else { Way::Right },
                ratio,
            }
        }
        // A direction's word is a direction, even where a session has it
        // for its name.
        PaneCommand::Focus { target, raise } => match Toward::from_str(&target, false) {
            Ok(toward) if !raise => layout::Command::FocusToward {
                toward: toward.into(),
            },
            _ => layout::Command::Focus {
                session: target,
                raise,
            },
        },
        PaneCommand::Resize {
            direction,
            cells,
            name,
        } => layout::Command::Resize {
            session: name,
            toward: direction.into(),
            cells,
        },
        PaneCommand::Close { session } => layout::Command::Close { session },
        PaneCommand::Zoom { session, off } => layout::Command::Zoom { session, on: !off },
        PaneCommand::Equalize => layout::Command::Equalize,
        PaneCommand::Float { session, off } => layout::Command::Float { session, on: !off },
    };
    client::lay_out(socket, command)?;
    Ok(())
}

/// The directory a command about a project is given with `-C`, or the
/// current one.
fn here(dir: Option<PathBuf>) -> Result<PathBuf> {
    match dir {
        Some(dir) => Ok(std::path::absolute(dir)?),
        None => Ok(std::env::current_dir()?),
    }
}

/// A new worktree to start in, as `-w` and `--base` ask for it.
struct NewWorktree {
    branch: String,
    /// Where the branch starts, when it's a new one: `--base`.
    base: Option<String>,
}

impl NewWorktree {
    fn from_args(branch: Option<String>, base: Option<String>) -> Option<NewWorktree> {
        Some(NewWorktree {
            branch: branch?,
            base,
        })
    }
}

/// Where a new session or task starts: `cwd`, or the current directory,
/// or else a new worktree made from there.
fn start_dir(
    socket: &Path,
    cwd: Option<PathBuf>,
    worktree: Option<NewWorktree>,
) -> Result<PathBuf> {
    let cwd = match cwd {
        Some(cwd) => std::path::absolute(cwd)?,
        None => std::env::current_dir()?,
    };
    match worktree {
        Some(NewWorktree { branch, base }) => {
            client::add_worktree(socket, &cwd, &branch, base.as_deref())
        }
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

/// `crystal report --ttl`, in seconds: a while written the way the
/// settings write one, a day at most.
fn report_ttl(ttl: &str) -> Result<u64> {
    let ttl = config::duration(ttl)?
        .filter(|ttl| *ttl <= report::LONGEST_TTL)
        .ok_or_else(|| anyhow::anyhow!("a --ttl is from a second to a day, like 30s, 5m or 2h"))?;
    Ok(ttl.as_secs())
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
            status: session.status(),
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
                session.status(),
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
    // Why a session couldn't start again is too long for its row, and on
    // standard error it's out of the way of a script reading the rows.
    for session in sessions {
        if let protocol::State::Failed { why } = &session.state {
            eprintln!("{} couldn't start again: {why}", session.name);
        }
    }
}

/// Prints the archived sessions, the latest archived first, with how long
/// ago each was archived and whether it starts again where it was.
fn print_archived(archived: &[ArchivedSession]) {
    if archived.is_empty() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let rows: Vec<[String; 6]> = archived
        .iter()
        .map(|archived| {
            let (project, branch) = match &archived.worktree {
                Some(worktree) => (
                    worktree.project.clone(),
                    worktree
                        .branch
                        .clone()
                        .unwrap_or_else(|| "(detached)".into()),
                ),
                None => ("-".into(), "-".into()),
            };
            let command = archived.session.command.iter().map(|arg| shell::quote(arg));
            [
                archived.name().to_string(),
                tui::sidebar::ago(archived.archived, now),
                project,
                branch,
                if archived.resumes() { "yes" } else { "no" }.to_string(),
                command.collect::<Vec<_>>().join(" "),
            ]
        })
        .collect();
    let header = [
        "NAME", "ARCHIVED", "PROJECT", "BRANCH", "RESUMES", "COMMAND",
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
            let mark = match outcome.state() {
                TaskState::Failed => "✗",
                TaskState::Cancelled => "–",
                _ => "✓",
            };
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
