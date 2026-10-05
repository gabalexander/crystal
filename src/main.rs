mod agent_cli;
mod agent_hooks;
mod agent_plugins;
mod agent_rules;
mod agent_screen;
mod agents;
mod api;
mod artifacts;
mod attach;
mod backlog;
mod bell;
mod catalog;
mod claude_stream;
mod claude_title;
mod client;
mod clipboard;
mod codex;
mod completions;
mod config;
mod config_bundle;
mod daemon;
mod db;
mod distill;
mod drive;
mod embed;
mod emptied;
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
mod layout_file;
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
mod output;
mod output_ring;
mod plugin_cli;
mod plugin_hooks;
mod plugin_manifest;
mod plugins;
mod printable;
mod profile;
mod project;
mod project_cli;
mod project_commands;
mod protocol;
mod qwen3;
mod remote;
mod report;
mod rerank;
mod resources;
mod secrets;
mod server_cli;
mod session;
mod shell;
mod skill;
mod socket;
mod sound;
mod spending;
mod state;
mod stream;
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
mod worktree_cli;
mod worktree_hooks;

use anyhow::{Result, bail};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use client::Restart;
use output::{out, outln};
use profile::{Launch, Profile, StartIn};
use protocol::{ArchivedSession, Request, Response, SessionInfo, TaskSpec, TaskState};
use std::io::{IsTerminal, Write};
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
        /// repository in <repo>.worktrees/, or in `[worktrees] directory`.
        /// The branch is made if it doesn't exist, from origin's default
        /// branch, fetched first.
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

        /// Who sends the report, for --seq: letters, digits and `:._-`. A
        /// source that takes the session over is the one that lets go of
        /// it.
        #[arg(long, value_name = "ID")]
        source: Option<String>,

        /// The report's number from its --source: one numbered no higher
        /// than the last came late, and is passed over. What the agent is
        /// doing and what's on its row are numbered apart.
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

        /// With no command: only the items with this tag; give it more than
        /// once for items with them all.
        #[arg(short, long = "tag", value_name = "TAG")]
        tags: Vec<String>,

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

        #[command(flatten)]
        brief: BriefArgs,

        /// The prompt. Several words are joined with spaces. With --pr or
        /// --issue, it can be left out: the task is to work on it.
        #[arg(required_unless_present_any = ["pr", "issue"])]
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
    /// the room. Or write them to a layout file, or lay them out the way
    /// one says, starting what isn't there.
    #[command(args_conflicts_with_subcommands = true)]
    Layout {
        /// Print them as JSON.
        #[arg(long)]
        json: bool,

        #[command(subcommand)]
        command: Option<LayoutCommand>,
    },
    /// Show a session in this terminal; Ctrl+\ detaches, Ctrl+B v is copy
    /// mode.
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
        /// `--` before text that starts with a `-`. `-` alone reads it from
        /// standard input.
        #[arg(required = true)]
        text: Vec<String>,

        /// Type the text without pressing Enter.
        #[arg(long)]
        no_enter: bool,

        /// Type it even while the agent is asking the user something, which
        /// is refused otherwise: the text would land in the question.
        #[arg(long)]
        force: bool,

        /// Stop the run a background task is in the middle of first, and
        /// carry on from there with the text.
        #[arg(long)]
        interrupt: bool,

        /// Then wait for the turn it starts to end, and print how it ended.
        /// An agent that isn't seen starting on it within 5 seconds has
        /// stalled: that exits 3.
        #[arg(long)]
        wait: bool,

        /// With --wait, give up after this many seconds.
        #[arg(long, value_name = "SECONDS", requires = "wait")]
        timeout: Option<f64>,
    },
    /// Wait until a session's agent isn't working, or its program has
    /// ended, and print which: done, waiting, idle, exited 0… Or until a
    /// task closes, and print how it went: done, failed or cancelled.
    Wait {
        /// The session, or a task by its number, like t12.
        name: String,

        /// Wait for this instead, and print it once it's reached: working,
        /// waiting, done, idle, ended (exited), or closed, its task. Several,
        /// with commas between, wait for any of them.
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

        /// Give up after this many seconds, exiting 2.
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<f64>,

        /// Print nothing once it's there.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Print what happened, from the event log, one line each, the oldest
    /// first: sessions starting, working, waiting and ending, tasks, runs,
    /// flows, worktrees, memory and the backlog.
    Events {
        /// Only those since then: a while back, like 30m, 2h or 3d, or a
        /// time, like 14:00, 2026-10-01 or 2026-10-01T09:30.
        #[arg(long, value_name = "WHEN")]
        since: Option<String>,

        /// Only those after the event with this `seq`, like the one `api
        /// snapshot` gives.
        #[arg(long, value_name = "SEQ", conflicts_with = "since")]
        after: Option<u64>,

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

        /// Only those about a task, by its number, like t12: the task, and
        /// its session while it works on it.
        #[arg(short, long, value_name = "TASK")]
        task: Option<String>,

        /// Only the newest this many; with --follow, of those before the
        /// new ones.
        #[arg(short, long, value_name = "N")]
        limit: Option<usize>,

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

        /// Only what its program wrote since then, history and all: a while
        /// back, like 30s, 10m or 2h, or a time, like 14:00 or
        /// 2026-10-01T09:30.
        #[arg(long, value_name = "WHEN")]
        since: Option<String>,

        /// Each line its program wrote as one, however many rows it wrapped
        /// onto.
        #[arg(long)]
        unwrap: bool,

        /// Keep its colors, bold, italic and underlines, as escape codes.
        #[arg(long)]
        ansi: bool,
    },
    /// Print what runs in a session's terminal: the processes in front,
    /// the job its keys go to, its leader first, each with its command and
    /// the directory it works in.
    #[command(visible_alias = "ps")]
    ProcessInfo {
        name: String,

        /// Print it as JSON, with the session's own program and the
        /// foreground process group.
        #[arg(long)]
        json: bool,
    },
    /// Stream a session's terminal as JSON lines, for a program to watch:
    /// a `start` line with its size, then each `output` the program
    /// writes, base64, the first drawing the screen as it is, then
    /// `closed`. Doesn't resize the session, or count as you watching it.
    Observe { name: String },
    /// Stream a session's terminal as `observe` does, and drive it with
    /// JSON lines on standard input: `input` (`text`, or `data` in base64),
    /// `keys` by name as send-keys takes them, `resize` and `release`. The
    /// end of the input lets go too.
    Control {
        name: String,

        /// Resize the session to this many rows first.
        #[arg(long, requires = "cols")]
        rows: Option<u16>,

        /// And this many columns.
        #[arg(long, requires = "rows")]
        cols: Option<u16>,
    },
    /// For a client of your own: everything crystal knows as JSON (`api
    /// snapshot`), and the schema of what crystal says over its socket (`api
    /// schema`).
    Api {
        #[command(subcommand)]
        command: ApiCommand,
    },
    /// Give a session another name.
    Rename { name: String, new_name: String },
    /// Run an ended session's command again, in the same directory and
    /// under the same name. Claude Code comes back in its conversation. One
    /// that couldn't start again after a restart tries again.
    Respawn { name: String },
    /// Stop a session and remove it from the list. An archived one is
    /// taken out of the archive. Killing the last session in a linked
    /// worktree asks at the terminal whether the worktree goes too, unless
    /// `[worktrees] remove_emptied` says otherwise.
    Kill {
        name: String,

        /// Remove the linked worktree it was the last session in, without
        /// asking: not one with changes not committed, unless you say so at
        /// the terminal.
        #[arg(long, conflicts_with = "keep_worktree")]
        remove_worktree: bool,

        /// Keep the linked worktree it was the last session in, without
        /// asking.
        #[arg(long)]
        keep_worktree: bool,
    },
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

        /// Print what the release changed instead, its notes: the one
        /// given, or this crystal's.
        #[arg(long, conflicts_with = "check")]
        notes: bool,
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
    /// file would hold them; or export them, or import some.
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommand>,
    },
    /// Remember something about this project for its later sessions: a
    /// decision, a gotcha, a command that works, a note.
    Remember {
        #[command(flatten)]
        entry: EntryArgs,

        /// The project's directory [default: the current one]
        #[arg(short = 'C', long = "dir", value_name = "DIR")]
        dir: Option<PathBuf>,
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
    /// List plugins, crystal's own, yours and the project's, with whether
    /// they're on; or the events they hear; or switch, run, install, build,
    /// make or remove one, or open one of its panes.
    Plugin {
        /// The directory of the project whose plugins `--project` means,
        /// and which the list shows [default: the current one]
        #[arg(short = 'C', long = "dir", value_name = "DIR", global = true)]
        dir: Option<PathBuf>,

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

        /// Draw with ASCII rather than box drawing, as the mermaid_ascii
        /// setting does.
        #[arg(long)]
        ascii: bool,

        /// Have mermaid draw it in the browser instead: written on a page
        /// in crystal's state directory, whose path is printed.
        #[arg(long, conflicts_with_all = ["width", "ascii"])]
        open: bool,
    },
    /// List the TUI's commands, the ids `[keys]` in the config file takes,
    /// and the keys that run them, as your config has them.
    Keys,
    /// Print the guide: what to start, the keys that matter most, what
    /// agents call and where things live, on one page. The TUI shows it
    /// too: `?`, then Tab.
    Guide,
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
enum ApiCommand {
    /// Print everything at once as JSON: the sessions, the TUI's tabs and
    /// panes, the projects, the tasks not closed, the flow runs, the
    /// archive, and the latest event's `seq`, to follow on from with
    /// `crystal events --follow --after <seq>`.
    Snapshot,
    /// Print the JSON Schema of what crystal says over its socket: the
    /// requests, the responses, the events, the lines a TUI taking layout
    /// orders trades, and what `api snapshot` prints; with neither flag, a
    /// line on each.
    Schema {
        /// Print the whole schema.
        #[arg(long, conflicts_with = "output")]
        json: bool,
        /// Write the whole schema to this file.
        #[arg(short, long, value_name = "PATH")]
        output: Option<PathBuf>,
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
    /// Whether crystal's hooks are installed, for this crystal: installed,
    /// out of date (another crystal's, or an earlier one's, which
    /// `install` brings up to date), or not installed.
    Status {
        /// The agent [default: every one]
        agent: Option<integration::Agent>,

        /// Only those out of date.
        #[arg(long)]
        outdated_only: bool,
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

        #[command(flatten)]
        brief: Box<BriefArgs>,

        /// What it's to do. Several words are joined with spaces. With --pr
        /// or --issue, it can be left out: the task is to work on it.
        #[arg(required_unless_present_any = ["pr", "issue"])]
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
    /// Open a background task in a terminal: Claude Code picks its
    /// conversation up there, in its place and under its name, and its
    /// task goes on in it. Prints the session's name.
    Terminal {
        /// The task, by its number, like t12, or its session's name.
        task: String,
    },
}

/// What a task carries beside its goal, for `crystal task` and `tasks
/// new`.
#[derive(clap::Args)]
struct BriefArgs {
    /// Something that has to hold before the task is done. Its agent is
    /// told each one under its goal. Give it once for each.
    #[arg(long, value_name = "CRITERION")]
    accept: Vec<String>,

    /// Acceptance criteria from a file, a line each: a list's `-` or `[ ]`
    /// is taken off.
    #[arg(long, value_name = "FILE")]
    accept_file: Option<PathBuf>,

    /// Work on this pull request (a merge request, on GitLab), by its
    /// number: in its worktree, made if the project hasn't one. Its agent
    /// is told to read it first.
    #[arg(long, value_name = "NUMBER", conflicts_with = "worktree")]
    pr: Option<u64>,

    /// The issue it's for, by its number. Its agent is told to read it
    /// first.
    #[arg(long, value_name = "NUMBER")]
    issue: Option<u64>,
}

impl From<BriefArgs> for work::Brief {
    fn from(args: BriefArgs) -> work::Brief {
        work::Brief {
            accept: args.accept,
            accept_file: args.accept_file,
            pull_request: args.pr,
            issue: args.issue,
        }
    }
}

/// Where a task runs and what it carries: in its pull request's worktree
/// when it's on one, or else in `cwd` or a new worktree, as for a session.
fn place_task(
    socket: &Path,
    cwd: Option<PathBuf>,
    worktree: Option<NewWorktree>,
    brief: BriefArgs,
) -> Result<(PathBuf, protocol::TaskBrief)> {
    let (brief, in_pull_request) = work::read_brief(socket, &here(cwd.clone())?, brief.into())?;
    let dir = match in_pull_request {
        Some(dir) => dir,
        None => start_dir(socket, cwd, worktree)?,
    };
    Ok((dir, brief))
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

/// An entry to remember, for `crystal remember` and `memory add`.
#[derive(clap::Args)]
struct EntryArgs {
    /// What sort of thing it is.
    #[arg(short, long, value_enum, default_value_t = memory::Kind::Note)]
    kind: memory::Kind,

    /// A file it's about; once some of its files change, the entry is
    /// marked drifting, and once all of them have, stale. Give it once a
    /// file.
    #[arg(short = 'f', long = "file", value_name = "FILE")]
    files: Vec<String>,

    /// A line of its own to list it by, over what it says: what lists and
    /// agents starting are shown of it.
    #[arg(long)]
    title: Option<String>,

    /// What to remember. Several words are joined with spaces. With
    /// --title, it can be left out.
    #[arg(required_unless_present = "title")]
    text: Vec<String>,
}

#[derive(Subcommand)]
enum MemoryCommand {
    /// Remember something, as `crystal remember` does.
    Add {
        #[command(flatten)]
        entry: EntryArgs,
    },
    /// Every entry, newest first, drifting and stale ones marked: what
    /// `crystal memory` with no command does.
    #[command(visible_alias = "ls")]
    List {
        /// Only the entries of this kind.
        #[arg(short, long, value_enum)]
        kind: Option<memory::Kind>,

        /// The entries forgotten instead, the latest first: the distiller
        /// never adds one back, but remembering it again does.
        #[arg(long, visible_alias = "wrong")]
        forgotten: bool,
    },
    /// The entries that have to do with these words, the best first,
    /// those that are stale left out.
    Search {
        #[arg(required = true)]
        words: Vec<String>,

        /// Only the entries of this kind.
        #[arg(short, long, value_enum)]
        kind: Option<memory::Kind>,

        /// Only the entries about this file, or a file in this directory.
        /// Give it once a file.
        #[arg(short = 'f', long = "file", value_name = "PATH")]
        files: Vec<String>,

        /// Stale entries too, marked.
        #[arg(short, long)]
        all: bool,

        /// The most entries to print [default: 50]
        #[arg(short = 'n', long, value_name = "N")]
        limit: Option<usize>,
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
enum LayoutCommand {
    /// Print the tabs as a layout file, for `layout apply`: as `layout
    /// --json` prints them, with the command and directory that start each
    /// session again.
    Export {
        /// Only this tab: its number, from 1, or its name.
        #[arg(long)]
        tab: Option<String>,
    },
    /// Lay the tabs out the way a layout file says: each in place of the
    /// tab with its name, or else after the others. Sessions it names that
    /// aren't there start, when it says how; it prints the name of each.
    Apply {
        /// The file [default: standard input, as - is]
        file: Option<PathBuf>,

        /// Take the place of every tab, as restoring a saved layout does:
        /// the sessions the file doesn't name join the tab in front.
        #[arg(long)]
        replace: bool,
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
    /// Swap a session's pane with another, the splits and how big each is
    /// left as they are: the pane that way from it, given left, right, up
    /// or down, or the pane of another session in its tab.
    Swap {
        /// left, right, up or down, or a session's name.
        target: String,

        /// The session whose pane to swap [default: the one this runs in,
        /// or else the selected one]
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Give a session's pane a share of the room of the split it's in: the
    /// nearest split above it, or with --right or --down, the nearest that
    /// splits that way.
    Ratio {
        /// Its share of the room, from 0.1 to 0.9.
        #[arg(value_parser = share)]
        share: f32,

        /// The session [default: the one this runs in, or else the selected
        /// one]
        session: Option<String>,

        /// The nearest split side by side.
        #[arg(long, conflicts_with = "down")]
        right: bool,

        /// The nearest split one above the other.
        #[arg(long)]
        down: bool,
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
enum ConfigCommand {
    /// Write the settings, the config file and your agent rule files, as
    /// one file to keep or take to another machine: on standard output,
    /// into a file, or as crystal-settings.json into a directory.
    Export {
        /// Where to write it [default: standard output]
        path: Option<String>,
    },
    /// Merge settings into yours: what they set replaces what's here, the
    /// rest stays, and profiles and flows merge by name. From an export, a
    /// config file, a directory holding either, or - for standard input.
    Import { source: String },
}

#[derive(Subcommand)]
enum WorktreeCommand {
    /// List the project's worktrees, the main one first, with each one's
    /// label and how many sessions run in it.
    #[command(visible_alias = "ls")]
    List {
        /// A directory in the project [default: the current one]
        #[arg(short = 'C', long = "dir", value_name = "DIR")]
        dir: Option<PathBuf>,

        /// Print them as JSON, each with the names of its sessions.
        #[arg(long)]
        json: bool,
    },
    /// Make a worktree, and print its directory. A branch that exists is
    /// checked out as it is; a new one starts from origin's default
    /// branch, fetched first. It goes beside the repository in
    /// <repo>.worktrees/, or in `[worktrees] directory`.
    Create {
        /// Its branch [default: a new one with a made-up name, like
        /// brave-otter]
        branch: Option<String>,

        /// Where a new branch starts, as `new --base` says.
        #[arg(long, value_name = "REF")]
        base: Option<String>,

        /// Make it in this directory instead.
        #[arg(long, value_name = "PATH")]
        path: Option<PathBuf>,

        /// A few words on what it's for, which the sidebar shows in place
        /// of its branch.
        #[arg(long, value_name = "TEXT")]
        label: Option<String>,

        /// A directory in the project [default: the current one]
        #[arg(short = 'C', long = "dir", value_name = "DIR")]
        dir: Option<PathBuf>,
    },
    /// Start a session in a worktree, given its directory or its branch,
    /// and attach to it when run in a terminal: your shell, or the command
    /// given.
    Open {
        /// The worktree's directory, or the branch it has checked out.
        worktree: String,

        /// Give the worktree this label, as `create --label` does.
        #[arg(long, value_name = "TEXT")]
        label: Option<String>,

        /// The session's name [default: from its first prompt, or else the
        /// program's name]
        #[arg(short, long)]
        name: Option<String>,

        /// Don't attach; print the session's name instead.
        #[arg(short, long)]
        detached: bool,

        /// Set a variable in the session's environment, as `new -e` does.
        #[arg(short, long = "env", value_name = "KEY=VALUE", value_parser = variable)]
        env: Vec<(String, String)>,

        /// A directory in the project [default: the current one]
        #[arg(short = 'C', long = "dir", value_name = "DIR")]
        dir: Option<PathBuf>,

        /// The command and its arguments [default: the shell `[terminal]`
        /// says, or yours]
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Give a worktree a label, a few words on what it's for, which the
    /// sidebar shows in place of its branch; "" takes it off.
    Label {
        /// The worktree's directory, or the branch it has checked out.
        worktree: String,

        label: String,

        /// A directory in the project [default: the current one]
        #[arg(short = 'C', long = "dir", value_name = "DIR")]
        dir: Option<PathBuf>,
    },
    /// Move a session into a worktree of its project: its program stops
    /// and starts again there, an agent in its conversation, told where it
    /// is now, a background task with a follow-up. One in the middle of a
    /// turn, or a run, moves once it ends, so an agent asked to work in a
    /// worktree runs this and ends its turn.
    Move {
        /// The worktree on this branch, made if there's none [default: a
        /// new one, on a branch with a made-up name]
        branch: Option<String>,

        /// The session to move [default: the one this runs in]
        #[arg(short, long)]
        name: Option<String>,

        /// Where a new branch starts, as `new --base` says.
        #[arg(long, value_name = "REF")]
        base: Option<String>,

        /// Make a new worktree in this directory.
        #[arg(long, value_name = "PATH")]
        path: Option<PathBuf>,
    },
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
    /// Turn a plugin on. A project's shows what it runs and asks first,
    /// then builds it.
    Enable {
        name: String,

        /// The plugin the project ships in its .crystal/plugins, on for it
        /// alone.
        #[arg(long)]
        project: bool,

        /// Don't ask first.
        #[arg(long, requires = "project")]
        yes: bool,
    },
    /// Turn a plugin off.
    Disable {
        name: String,

        /// The plugin the project ships.
        #[arg(long)]
        project: bool,
    },
    /// List the events a plugin's hooks can hear, and when each happens.
    Events,
    /// Run one of a plugin's actions, or try its hooks out on an event.
    Run {
        plugin: String,

        /// The action, by its id.
        #[arg(required_unless_present_any = ["event", "json", "link"])]
        action: Option<String>,

        /// Run the plugin's hooks on a made-up event of this kind, like
        /// session.waiting, here and now, on or off, and print what they
        /// print.
        #[arg(long, value_name = "KIND", conflicts_with = "action")]
        event: Option<String>,

        /// Run its hooks on the event this JSON says, or - to read it from
        /// standard input, like a line of `crystal events --json`: what it
        /// gives over a made-up one of its kind, or of --event's.
        #[arg(long, value_name = "JSON", conflicts_with = "action")]
        json: Option<String>,

        /// Run the action the plugin's link handlers give this link, with
        /// it in CRYSTAL_LINK, as a Ctrl+click on it in a pane would.
        #[arg(long, value_name = "URL", conflicts_with_all = ["action", "event", "json"])]
        link: Option<String>,

        /// The session to run it for [default: the one this runs in, if
        /// any]
        #[arg(short, long)]
        session: Option<String>,

        /// The plugin the project ships.
        #[arg(long)]
        project: bool,
    },
    /// Open one of a plugin's panes.
    Pane {
        #[command(subcommand)]
        command: PluginPaneCommand,
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
    Build {
        name: String,

        /// The plugin the project ships.
        #[arg(long)]
        project: bool,
    },
    /// Remove a plugin you installed.
    #[command(visible_alias = "rm")]
    Remove { name: String },
    /// Make a plugin to start from, in your plugins directory.
    New { name: String },
    /// Print what a plugin's commands printed, and how they failed.
    Log {
        name: String,

        /// The plugin the project ships.
        #[arg(long)]
        project: bool,
    },
}

#[derive(Subcommand)]
enum PluginPaneCommand {
    /// Start one of a plugin's panes in a session of its own and show it
    /// where its manifest says, or --placement does: over the TUI's panes
    /// or in a popup, which take a TUI, or split off a session's pane,
    /// zoomed, or in a tab of its own. Prints the session's name.
    Open {
        plugin: String,

        /// The pane, by its id.
        pane: String,

        /// Where it goes, in place of where its manifest says.
        #[arg(long, value_enum)]
        placement: Option<plugin_manifest::Placement>,

        /// A popup's width: so many cells, or a share of the screen, like
        /// 80%.
        #[arg(long, value_name = "SIZE")]
        width: Option<String>,

        /// A popup's height.
        #[arg(long, value_name = "SIZE")]
        height: Option<String>,

        /// A split's new pane to the right of the session's.
        #[arg(long, conflicts_with = "down")]
        right: bool,

        /// A split's new pane below the session's.
        #[arg(long)]
        down: bool,

        /// The session it's about, and a split goes beside [default: the
        /// one this runs in, or else the one selected]
        #[arg(short, long)]
        session: Option<String>,

        /// The plugin the project ships.
        #[arg(long)]
        project: bool,
    },
}

#[derive(Subcommand)]
enum BacklogCommand {
    /// Put something on the backlog. Prints its number.
    Add {
        /// What to do later. Several words are joined with spaces. A text
        /// of several lines is the item's line and the start of its body.
        #[arg(required = true)]
        text: Vec<String>,

        /// More on it than its line says.
        #[arg(short, long)]
        body: Option<String>,

        /// A tag for it, like `ui`; give it more than once for more.
        #[arg(short, long = "tag", value_name = "TAG")]
        tags: Vec<String>,
    },
    /// List what's still to do, oldest first: what `crystal backlog` with
    /// no command does.
    #[command(visible_alias = "ls")]
    List {
        /// What's done too, the latest done first.
        #[arg(long)]
        all: bool,

        /// Print it as JSON.
        #[arg(long)]
        json: bool,

        /// Only the items with this tag; give it more than once for items
        /// with them all.
        #[arg(short, long = "tag", value_name = "TAG")]
        tags: Vec<String>,
    },
    /// Show an item: whether it's done, its tags, when it was added, the
    /// tasks started for it and how they went, and its body.
    Show {
        number: u64,

        /// Print it as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Change an item: its line, its body or its tags.
    Edit {
        number: u64,

        /// Its new line. Several words are joined with spaces.
        text: Vec<String>,

        /// Its new body; an empty one takes it away.
        #[arg(short, long)]
        body: Option<String>,

        /// Its tags, in place of those it has; give it once a tag.
        #[arg(short, long = "tag", value_name = "TAG", conflicts_with = "no_tags")]
        tags: Vec<String>,

        /// Take its tags away.
        #[arg(long)]
        no_tags: bool,
    },
    /// Mark an item done.
    Done { number: u64 },
    /// Mark a done item open again.
    Reopen { number: u64 },
    /// Take an item off the backlog.
    #[command(visible_alias = "remove")]
    Rm { number: u64 },
    /// Start a task for an item, with the agent the new-session panel
    /// starts first, or a profile's. Closing the task done ticks the item.
    Start {
        number: u64,

        /// In a new worktree, on a branch named after the item.
        #[arg(short, long, conflicts_with = "pr")]
        worktree: bool,

        /// Start it with this profile: its agent, options and prompt, and
        /// in a new worktree when it says so.
        #[arg(short, long, conflicts_with = "background")]
        profile: Option<String>,

        /// On this pull request, by its number: in its worktree, the
        /// project's own on its branch or else one made for it, and told
        /// of it.
        #[arg(long, value_name = "NUMBER")]
        pr: Option<u64>,

        /// Run it in the background, as `crystal task` does, and print its
        /// session's name.
        #[arg(long)]
        background: bool,

        /// The session's name [default: from the item, or else the agent's
        /// name]
        #[arg(short, long)]
        name: Option<String>,

        /// Don't attach; print the session's name instead.
        #[arg(short, long)]
        detached: bool,

        /// With --background, arguments for its `claude -p`, after `--`.
        #[arg(last = true, value_name = "CLAUDE ARGS", requires = "background")]
        claude_args: Vec<String>,
    },
    /// Print the backlog as markdown checkboxes, done items too, each one's
    /// body indented under it.
    Export,
    /// Put the items of a markdown list of checkboxes on the backlog, as
    /// `export` writes them or a README's list of things to do: `- [ ]`
    /// and `- [x]` at the start of a line, `#tags` at its end, and the
    /// indented lines under it its body. Those whose line the backlog has
    /// already are passed over.
    Import {
        /// The markdown file, or `-` for standard input.
        file: PathBuf,
    },
}

/// What `crystal` exits with when a wait gives up: 2, and only then, so a
/// script can tell "not yet" from anything else going wrong.
const TIMED_OUT: u8 = 2;

/// What `crystal send --wait` exits with when the agent was never seen
/// starting on what it was sent: 3, so a script can read it before sending
/// it again.
const STALLED: u8 = 3;

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            // A mistyped flag fails as anything else does, with 1: clap's
            // own 2 is a wait's that gave up.
            return match err.use_stderr() {
                true => ExitCode::FAILURE,
                false => ExitCode::SUCCESS,
            };
        }
    };
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        // Whatever read what it printed had what it wanted.
        Err(err) if output::closed(&err) => ExitCode::SUCCESS,
        Err(err) => {
            // It may quote what a session, an agent or the forge said; and
            // standard error may be a pipe that has gone too.
            let said = format!("{err:#}");
            let _ = writeln!(std::io::stderr(), "crystal: {}", printable::text(&said));
            if err.is::<drive::TimedOut>() {
                ExitCode::from(TIMED_OUT)
            } else if err.is::<drive::Stalled>() {
                ExitCode::from(STALLED)
            } else {
                ExitCode::FAILURE
            }
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
            if !shows && ttl.is_some() {
                bail!("--ttl goes with --line or --model");
            }
            // Who sent the report, and its number, go with what's on the row
            // and with what the agent is doing alike.
            let metadata = protocol::Metadata {
                line,
                model,
                ttl_secs: ttl.as_deref().map(report_ttl).transpose()?,
                source,
                seq,
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
            tags,
            command,
        } => backlog(&socket, here(dir)?, all, json, tags, command)?,
        Command::Task {
            name,
            cwd,
            worktree,
            base,
            wait,
            timeout,
            brief,
            prompt,
            claude_args,
        } => {
            let worktree = NewWorktree::from_args(worktree, base);
            let (cwd, brief) = place_task(&socket, cwd, worktree, brief)?;
            let spec = TaskSpec {
                prompt: work::goal(&prompt, &brief)?,
                args: claude_args,
            };
            let name = client::new_task(&socket, name, cwd, spec, None, brief)?.name;
            outln!("{name}")?;
            if wait {
                drive::wait_for_run(&socket, &name, seconds(timeout))?;
            }
        }
        Command::Flow { json, command } => flow(&socket, json, command)?,
        Command::Result { name, json } => drive::result(&socket, &name, json)?,
        Command::Worktree { command } => worktree(&socket, command)?,
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
        Command::Layout {
            command: Some(LayoutCommand::Export { tab }),
            ..
        } => layout_file::export(&socket, tab.as_deref())?,
        Command::Layout {
            command: Some(LayoutCommand::Apply { file, replace }),
            ..
        } => layout_file::apply(&socket, file.as_deref(), replace)?,
        Command::Layout {
            json,
            command: None,
        } => {
            let layout = client::lay_out(&socket, layout::Command::Show)?;
            if json {
                outln!("{}", serde_json::to_string_pretty(&layout)?)?;
            } else {
                out!("{}", printable::text(&layout.text()))?;
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
                outln!("{}", serde_json::to_string_pretty(&archived)?)?;
            } else {
                print_archived(&archived)?;
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
                print_sessions(&sessions)?;
            }
        }
        Command::Send {
            name,
            text,
            no_enter,
            force,
            interrupt,
            wait,
            timeout,
        } => {
            let text = match &text[..] {
                [dash] if dash == drive::FROM_STDIN => drive::read_stdin()?,
                words => words.join(" "),
            };
            let sending = drive::Sending {
                enter: !no_enter,
                force,
                interrupt,
                wait,
                timeout: seconds(timeout),
            };
            drive::send(&socket, &name, &text, sending)?;
        }
        Command::SendKeys {
            name,
            keys,
            wait,
            timeout,
        } => drive::send_keys(&socket, &name, keys, wait, seconds(timeout))?,
        Command::Wait {
            name,
            until,
            output,
            timeout,
            quiet,
        } => {
            let timeout = seconds(timeout);
            match output {
                Some(pattern) => drive::wait_for_output(&socket, &name, &pattern, timeout, quiet)?,
                None if until.is_empty() => drive::wait(&socket, &name, timeout, quiet)?,
                None => drive::wait_until(&socket, &name, &until, timeout, quiet)?,
            }
        }
        Command::Events {
            since,
            after,
            kinds,
            name,
            dir,
            task,
            limit,
            json,
            follow,
        } => {
            let task = task
                .map(|task| {
                    tasks::parse_id(&task)
                        .ok_or_else(|| anyhow::anyhow!("`{task}` isn't a task's number, like t12"))
                })
                .transpose()?;
            let options = events_cli::Options {
                since,
                after,
                kinds,
                session: name,
                dir: dir.map(|dir| here(Some(dir))).transpose()?,
                task,
                limit,
                json,
                follow,
            };
            events_cli::run(&socket, options)?;
        }
        Command::Read {
            name,
            lines,
            history,
            since,
            unwrap,
            ansi,
        } => {
            let reading = drive::Reading {
                lines,
                history,
                unwrap,
                ansi,
                since,
            };
            drive::read(&socket, &name, reading)?
        }
        Command::ProcessInfo { name, json } => drive::process_info(&socket, &name, json)?,
        Command::Observe { name } => stream::observe(&socket, &name)?,
        Command::Api {
            command: ApiCommand::Snapshot,
        } => outln!(
            "{}",
            serde_json::to_string_pretty(&api::snapshot(&socket)?)?
        )?,
        Command::Api {
            command: ApiCommand::Schema { json, output },
        } => match output {
            Some(path) => {
                api::schema::write(&path)?;
                outln!("wrote the API schema to {}", path.display())?;
            }
            None if json => out!("{}", api::schema::JSON)?,
            None => out!("{}", api::schema::summary()?)?,
        },
        Command::Control { name, rows, cols } => {
            let size = rows.zip(cols).filter(|&(rows, cols)| rows > 0 && cols > 0);
            stream::control(&socket, &name, size)?
        }
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
        Command::Kill {
            name,
            remove_worktree,
            keep_worktree,
        } => {
            let remove = match (remove_worktree, keep_worktree) {
                (true, _) => Some(true),
                (_, true) => Some(false),
                _ => None,
            };
            worktree_cli::kill(&socket, &name, remove)?;
        }
        Command::KillServer => {
            if !client::stop_daemon(&socket, false)? {
                no_daemon(&socket)?;
            }
        }
        Command::RestartServer { cold } => match client::restart_daemon(&socket, cold)? {
            Restart::NoDaemon => outln!("no daemon was running")?,
            Restart::HandedOver { sessions: 0 } | Restart::Cold { why: None } => {
                outln!("restarted the daemon")?;
            }
            Restart::HandedOver { .. } => {
                outln!("restarted the daemon, and its sessions carried on")?
            }
            Restart::Cold { why: Some(why) } => {
                outln!("restarted the daemon; its sessions started again, since {why}")?;
            }
        },
        Command::Update {
            version,
            check,
            notes,
        } => update::run(&socket, version, check, notes)?,
        Command::Server { json, command } => match command {
            None => server_cli::list(json)?,
            Some(ServerCommand::Stop { name }) => server_cli::stop(&name)?,
            Some(ServerCommand::Delete { name }) => server_cli::delete(&name)?,
        },
        Command::Config { command } => match command {
            None => print_config()?,
            Some(ConfigCommand::Export { path }) => config_bundle::export(path.as_deref())?,
            Some(ConfigCommand::Import { source }) => config_bundle::import(&source)?,
        },
        Command::Remember { entry, dir } => remember(&socket, dir, entry)?,
        Command::Memory { dir, command } => match command {
            None => memory_cli::list(&socket, dir, None, false)?,
            Some(MemoryCommand::Add { entry }) => remember(&socket, dir, entry)?,
            Some(MemoryCommand::List { kind, forgotten }) => {
                memory_cli::list(&socket, dir, kind, forgotten)?;
            }
            Some(MemoryCommand::Search {
                words,
                kind,
                files,
                all,
                limit,
            }) => {
                let args = memory_cli::SearchArgs {
                    kind,
                    files,
                    all,
                    limit,
                };
                memory_cli::search(&socket, dir, &words, args)?;
            }
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
                None => print_profiles(&settings.profiles)?,
                Some(ProfileCommand::Show { name }) => print_profile(&settings.profiles, &name)?,
            }
        }
        Command::Plugin { dir, command } => {
            let id = |name: &str, project: bool| plugin_cli::id(name, project, dir.clone());
            match command {
                None => plugin_cli::list(&socket, dir)?,
                Some(PluginCommand::Enable { name, project, yes }) => {
                    plugin_cli::switch(&socket, &id(&name, project)?, true, yes)?
                }
                Some(PluginCommand::Disable { name, project }) => {
                    plugin_cli::switch(&socket, &id(&name, project)?, false, false)?
                }
                Some(PluginCommand::Events) => plugin_cli::events()?,
                Some(PluginCommand::Run {
                    plugin,
                    action,
                    event,
                    json,
                    link,
                    session,
                    project,
                }) => {
                    let plugin = id(&plugin, project)?;
                    let code = match (action, event, json, link) {
                        (_, event, json, _) if event.is_some() || json.is_some() => {
                            let (event, json) = (event.as_deref(), json.as_deref());
                            plugin_cli::run_event(&socket, &plugin, event, json, session)?
                        }
                        (_, _, _, Some(link)) => {
                            plugin_cli::run_link(&socket, &plugin, &link, session)?
                        }
                        (action, ..) => {
                            let action = action.unwrap_or_default();
                            plugin_cli::run(&socket, &plugin, &action, session, None)?
                        }
                    };
                    // The action's or the hook's own exit code is crystal's.
                    std::process::exit(code);
                }
                Some(PluginCommand::Pane {
                    command:
                        PluginPaneCommand::Open {
                            plugin,
                            pane,
                            placement,
                            width,
                            height,
                            right,
                            down,
                            session,
                            project,
                        },
                }) => {
                    let split = match (right, down) {
                        (true, _) => Some(tui::keymap::SplitWay::Right),
                        (_, true) => Some(tui::keymap::SplitWay::Down),
                        _ => None,
                    };
                    let placing = plugin_cli::Placing {
                        placement,
                        width,
                        height,
                        split,
                    };
                    let plugin = id(&plugin, project)?;
                    plugin_cli::open_pane(&socket, &plugin, &pane, placing, session)?
                }
                Some(PluginCommand::Install {
                    source,
                    yes,
                    enable,
                }) => plugin_cli::install(&socket, &source, yes, enable)?,
                Some(PluginCommand::Build { name, project }) => {
                    plugin_cli::build(&socket, &id(&name, project)?)?
                }
                Some(PluginCommand::Remove { name }) => plugin_cli::remove(&name)?,
                Some(PluginCommand::New { name }) => plugin_cli::new(&name)?,
                Some(PluginCommand::Log { name, project }) => {
                    plugin_cli::log(&socket, &id(&name, project)?)?
                }
            }
        }
        Command::Mermaid {
            file,
            width,
            ascii,
            open,
        } => {
            // A config that can't be read draws no diagram the less.
            let ascii = ascii || config::Config::load().is_ok_and(|config| config.mermaid_ascii);
            mermaid_cli::run(file.as_deref(), width, ascii, open)?
        }
        Command::Guide => out!("{}", tui::page::GUIDE)?,
        Command::Keys => {
            let config = config::Config::load()?;
            let keymap = tui::keymap::Keymap::new(&config.keys).map_err(anyhow::Error::msg)?;
            out!("{}", tui::keymap::listing(&keymap))?;
        }
        Command::Skill { install, force } => {
            if install {
                skill::install(force)?;
            } else {
                skill::print()?;
            }
        }
        Command::Integration { command } => run_integration(command)?,
        Command::Completions { shell } => out!("{}", completions::script(shell, Cli::command()))?,
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
                    outln!("{}", session.name)?;
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
        outln!("# {}", path.display())?;
    } else {
        outln!("# {} (no file yet: these are the defaults)", path.display())?;
    }
    out!("{}", settings.to_toml())?;
    Ok(())
}

/// A table of the profiles: one a row, its description last, since it's
/// the one with spaces.
fn print_profiles(profiles: &[Profile]) -> Result<()> {
    if profiles.is_empty() {
        outln!(
            "no profiles yet: add one with P in the TUI, or in {}",
            shell::home_relative(&config::path())
        )?;
        return Ok(());
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
    print_table(["NAME", "AGENT", "WHERE", "DESCRIPTION"], &rows)
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
    let task = if profile.skip_task { "" } else { "<task>" };
    let command: Vec<String> = profile
        .command(task)
        .iter()
        .map(|arg| shell::quote(arg))
        .collect();
    let launch = match profile.launch {
        Launch::Either => None,
        Launch::Session => Some("a session, not a task"),
        Launch::Task => Some("a task"),
        Launch::Background => Some("a background task, Claude Code's; others a task"),
    };
    outln!("{}", profile.name)?;
    if let Some(description) = &profile.description {
        outln!("  {description}")?;
    }
    outln!("agent   {agent}")?;
    outln!("starts  {place}")?;
    if let Some(launch) = launch {
        outln!("as      {launch}")?;
    }
    if profile.skip_task {
        outln!("task    none: it starts at once")?;
    }
    outln!("runs    {}", command.join(" "))?;
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
        ..client::Purpose::default()
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
        outln!("{name}")?;
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

/// `crystal remember` and `memory add`: remembers `entry` for the project
/// `dir` is in.
fn remember(socket: &Path, dir: Option<PathBuf>, entry: EntryArgs) -> Result<()> {
    let EntryArgs {
        kind,
        files,
        title,
        text,
    } = entry;
    memory_cli::remember(socket, dir, kind, files, title, &text.join(" "))
}

/// `crystal backlog` and its commands, for the project `dir` is in.
fn backlog(
    socket: &Path,
    dir: PathBuf,
    all: bool,
    json: bool,
    tags: Vec<String>,
    command: Option<BacklogCommand>,
) -> Result<()> {
    use work::BacklogAction;
    let action = match command {
        None => BacklogAction::List { all, json, tags },
        Some(BacklogCommand::List { all, json, tags }) => BacklogAction::List { all, json, tags },
        Some(BacklogCommand::Add { text, body, tags }) => BacklogAction::Add {
            text: text.join(" "),
            body: body.unwrap_or_default(),
            tags,
        },
        Some(BacklogCommand::Show { number, json }) => BacklogAction::Show { number, json },
        Some(BacklogCommand::Edit {
            number,
            text,
            body,
            tags,
            no_tags,
        }) => BacklogAction::Edit {
            number,
            text: (!text.is_empty()).then(|| text.join(" ")),
            body,
            tags: (no_tags || !tags.is_empty()).then_some(tags),
        },
        Some(BacklogCommand::Done { number }) => BacklogAction::Mark { number, done: true },
        Some(BacklogCommand::Reopen { number }) => BacklogAction::Mark {
            number,
            done: false,
        },
        Some(BacklogCommand::Rm { number }) => BacklogAction::Remove { number },
        Some(BacklogCommand::Export) => BacklogAction::Export,
        Some(BacklogCommand::Import { file }) => BacklogAction::Import { file },
        Some(BacklogCommand::Start {
            number,
            worktree,
            profile,
            pr,
            background,
            name,
            detached,
            claude_args,
        }) => {
            let start = work::BacklogStart {
                number,
                worktree,
                profile,
                pull_request: pr,
                background,
                claude_args,
                name,
            };
            let name = work::start_from_backlog(socket, dir, start)?;
            // A task in the background has no terminal to attach to.
            if background {
                outln!("{name}")?;
                return Ok(());
            }
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
            brief,
            goal,
            claude_args,
        }) => {
            let worktree = NewWorktree::from_args(worktree, base);
            let (cwd, brief) = place_task(socket, cwd, worktree, *brief)?;
            let task = work::NewTask {
                goal: work::goal(&goal, &brief)?,
                cwd,
                name,
                background,
                claude_args,
                launch: !no_launch,
                brief,
            };
            work::new_task(socket, task)
        }
        Some(TasksCommand::Start { id }) => work::start_task(socket, &id),
        Some(TasksCommand::Show { task, json }) => work::show_task(socket, &task, json),
        Some(TasksCommand::Cancel { task }) => work::cancel_task(socket, &task),
        Some(TasksCommand::Log { task }) => work::task_log(socket, &task),
        Some(TasksCommand::Terminal { task }) => work::task_to_terminal(socket, &task),
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
        Some(FlowCommand::Example) => flow_cli::example(),
    }
}

/// `crystal integration` and its commands, for the agent named or, without
/// one, each installed here; `status` without one is about both.
fn run_integration(command: IntegrationCommand) -> Result<()> {
    let crystal = std::env::current_exe()?;
    match command {
        IntegrationCommand::Install { agent } => {
            for agent in integration::chosen(agent)? {
                // Said along the way: each agent's hooks go in whether
                // anything reads it.
                for line in integration::install(agent, &crystal)? {
                    let _ = outln!("{line}");
                }
            }
        }
        IntegrationCommand::Uninstall { agent } => {
            for agent in integration::chosen(agent)? {
                let _ = outln!("{}", integration::uninstall(agent)?);
            }
        }
        IntegrationCommand::Status {
            agent,
            outdated_only,
        } => {
            let agents = agent.map_or(integration::Agent::ALL.to_vec(), |agent| vec![agent]);
            for agent in agents {
                let outdated =
                    integration::standing_of(agent, &crystal)? == integration::Standing::OutOfDate;
                if outdated || !outdated_only {
                    outln!("{}", integration::status(agent, &crystal)?)?;
                }
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
        outln!("{}", tab.number)?;
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
                    outln!("{name}")?;
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
        PaneCommand::Swap { target, name } => match Toward::from_str(&target, false) {
            Ok(toward) => layout::Command::SwapToward {
                session: name,
                toward: toward.into(),
            },
            Err(_) => layout::Command::Swap {
                session: name,
                with: target,
            },
        },
        PaneCommand::Ratio {
            share,
            session,
            right,
            down,
        } => layout::Command::Ratio {
            session,
            way: match (right, down) {
                (true, _) => Some(Way::Right),
                (_, true) => Some(Way::Down),
                _ => None,
            },
            share,
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
            client::add_worktree(socket, &cwd, &branch, base.as_deref(), None)
        }
        None => Ok(cwd),
    }
}

/// `crystal worktree` and its commands.
fn worktree(socket: &Path, command: WorktreeCommand) -> Result<()> {
    match command {
        WorktreeCommand::List { dir, json } => worktree_cli::list(socket, &here(dir)?, json),
        WorktreeCommand::Create {
            branch,
            base,
            path,
            label,
            dir,
        } => {
            let new = worktree_cli::NewWorktree {
                branch,
                base,
                path,
                label,
            };
            worktree_cli::create(socket, &here(dir)?, new)
        }
        WorktreeCommand::Open {
            worktree,
            label,
            name,
            detached,
            env,
            dir,
            command,
        } => {
            let path = worktree_cli::find(&here(dir)?, &worktree, label.as_deref())?;
            let new = NewArgs {
                name,
                cwd: Some(path),
                worktree: None,
                detached,
                command,
                task: None,
                env,
            };
            new_session(socket, new)
        }
        WorktreeCommand::Label {
            worktree,
            label,
            dir,
        } => worktree_cli::label(&here(dir)?, &worktree, &label),
        WorktreeCommand::Move {
            branch,
            name,
            base,
            path,
        } => {
            let to = worktree_cli::MoveTo {
                session: name,
                branch,
                base,
                path,
            };
            worktree_cli::move_session(socket, to)
        }
        WorktreeCommand::Rm { worktree, force } => remove_worktree(socket, &worktree, force),
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
    outln!("{}", serde_json::to_string_pretty(&listed)?)?;
    Ok(())
}

fn print_sessions(sessions: &[SessionInfo]) -> Result<()> {
    if sessions.is_empty() {
        return Ok(());
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
    print_table(header, &rows)?;
    // Why a session couldn't start again is too long for its row, and on
    // standard error it's out of the way of a script reading the rows.
    for session in sessions {
        if let protocol::State::Failed { why } = &session.state {
            eprintln!("{} couldn't start again: {why}", session.name);
        }
    }
    Ok(())
}

/// Prints the archived sessions, the latest archived first, with how long
/// ago each was archived and whether it starts again where it was.
fn print_archived(archived: &[ArchivedSession]) -> Result<()> {
    if archived.is_empty() {
        return Ok(());
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
    print_table(header, &rows)
}

/// Prints `rows` under `header`, each column as wide as its widest cell,
/// and each cell on one line, with nothing a terminal would take as an
/// order: names, branches and commands are anyone's.
fn print_table<const N: usize>(header: [&str; N], rows: &[[String; N]]) -> Result<()> {
    let header = header.map(String::from);
    let rows: Vec<[String; N]> = rows
        .iter()
        .map(|row| row.clone().map(|cell| printable::line(&cell).into_owned()))
        .collect();
    let mut widths = [0; N];
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
        outln!("{}", line.join("  ").trim_end())?;
    }
    Ok(())
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
