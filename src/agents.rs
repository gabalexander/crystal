//! What crystal knows about particular agents: how to launch one so that it
//! reports what it's doing and hears crystal's notes, and how to read what
//! it reports. Any other program runs exactly as it was asked for.
//!
//! Claude Code reports through hooks: commands it runs on events like a
//! prompt being sent or a turn ending. crystal adds its own with
//! `--settings`, so the user's settings files are never touched, and its
//! hooks run alongside any the user has. Other agents take hooks only in
//! their own settings, where crystal puts them when the user asks it to
//! (see [`crate::agent_hooks`]), or plugins, which crystal writes for them
//! (see [`crate::agent_plugins`]); what they report is read here too, and
//! here is how each picks its conversation up again.

use crate::agent_rules;
use crate::catalog;
use crate::codex;
use crate::printable;
use crate::protocol::{AgentEvent, Conversation, Pending, PendingKind, Subagent, Wakeup};
use crate::shell;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The most a first prompt may be with crystal's notes at its top:
/// docket's limit for a starting prompt.
const FIRST_PROMPT_BYTES: usize = 16 * 1024;

/// The Claude Code hook events crystal listens to; [`hook_event`] says
/// what each one means.
pub const CLAUDE_HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "PermissionRequest",
    "Notification",
    "Stop",
    "SubagentStart",
    "SubagentStop",
];

/// The tools of Claude Code's that read or edit a file, which crystal's
/// `PreToolUse` hook is matched to: as one is about to, the agent is shown
/// what its project's memory has about the file (see [`crate::recall`]).
pub const CLAUDE_FILE_TOOLS: &[&str] = &["Read", "Edit", "Write", "MultiEdit", "NotebookEdit"];

/// The Codex hook events crystal listens to; [`codex_event`] says what each
/// one means. Codex has no `Notification`: the questions it asks while
/// it waits on the user are read off its screen.
pub const CODEX_HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
    "Interrupt",
    "SubagentStart",
    "SubagentStop",
];

/// What crystal puts in the environment of a Claude Code it starts with its
/// own hooks, `--settings`, the agent's program as its value. The hooks
/// `crystal integration` installs in the user's own settings run as well,
/// and stay quiet under it, so the agent isn't reported twice. A task's
/// `claude -p` has it too: a task knows what its Claude does from Claude's
/// own events.
pub const HOOKED: &str = "CRYSTAL_AGENT_HOOKS";

/// What a hook crystal adds runs: `crystal hook <agent>`, with crystal by
/// its path. With `installed`, it's the one `crystal integration` puts in
/// the agent's own settings.
pub fn hook_command(crystal: &Path, agent: &str, installed: bool) -> String {
    let mut command = format!("{} hook {agent}", shell::quote(&crystal.to_string_lossy()));
    if installed {
        command.push_str(" --installed");
    }
    command
}

/// What opens the notes crystal adds for an agent: where they come from.
/// An agent told to run the commands of a program it has never heard of
/// can take them for a prompt injection and ignore them.
pub const ABOUT_CRYSTAL: &str = "You're running inside crystal, the terminal workspace the \
                                 user runs their coding agents in. The user set crystal up \
                                 to add the notes below for you, and the `crystal` command \
                                 they mention is installed for you to run.";

/// What Claude Code is told about working on several things at once: to
/// start a session of crystal's for each, which the user sees, rather
/// than worktrees or subagents of its own, which they don't; and about
/// working in a worktree itself: to have crystal move its session into
/// one, rather than enter one of its own, which crystal's sidebar would
/// only find after the fact, under `.claude/worktrees`. Only Claude Code
/// makes those, and only it has a system prompt to say this in without
/// touching what the user asked.
pub const PARALLEL_WORK: &str = "To work on several things at once, start a crystal session \
                                 for each, `crystal new -d -w <branch> claude \"<task>\"` \
                                 (with `--base HEAD` when it should start from your \
                                 commits), rather than worktrees or subagents of your own: \
                                 each shows in the user's sidebar with its status, its diff \
                                 and its screen, where they can step in. When you're asked \
                                 to work in a worktree yourself, run `crystal worktree move \
                                 <branch>` once, rather than entering or making a worktree \
                                 of your own, then end your turn: crystal moves this session \
                                 into it and picks your conversation up there. The crystal \
                                 skill says more.";

/// What Claude Code is told about showing the user a file: to put it in
/// front of them with `crystal open` when they ask to see it, rather than
/// paste it into its answer, and never unasked, since it takes their
/// screen; text files only, as that's all a TUI draws. Said beside
/// [`PARALLEL_WORK`], in its system prompt. Adapted from docket's.
pub const SHOWING_FILES: &str = "When the user asks you to show them a file, or to open one in \
                                 crystal, run `crystal open <file>...` once for the files: \
                                 crystal shows them in its TUI, a markdown file as a page \
                                 with its mermaid diagrams drawn, so say in a line what you \
                                 opened rather than paste them into your answer. Never open \
                                 a file unasked: name its path and let the user ask. It \
                                 shows text files only; name an image's or a PDF's path \
                                 instead, and if the command fails, name the paths.";

/// The command line to run for `command`. For an agent crystal knows, it
/// carries the flags that make the agent report to `crystal hook`, run
/// from `crystal`, the path of this program, and with `resume`, the id of
/// a conversation to pick up again.
///
/// A conversation picked up again has been asked its first prompt already,
/// so it isn't asked again: `task`, what the session was started to do,
/// helps find that prompt on a command line written before crystal put it
/// after `--`.
///
/// `instructions` are what crystal tells the agent on top of what it was
/// asked, each a paragraph: Claude Code gets them added to its system
/// prompt, and an agent with no option for them at the top of its first
/// prompt, when it's given one. Codex gets them as its developer
/// instructions, from [`codex::with_instructions`].
pub fn argv(
    command: &[String],
    crystal: &Path,
    resume: Option<&str>,
    task: Option<&str>,
    instructions: &[String],
) -> Vec<String> {
    match program_name(command) {
        Some("claude") => {}
        Some("codex") => {
            return resume
                .and_then(|id| codex::resume_argv(command, id))
                .unwrap_or_else(|| command.to_vec());
        }
        _ => {
            return resume
                .and_then(|id| resumed(command, id, task))
                .unwrap_or_else(|| in_first_prompt(command, instructions));
        }
    }
    let mut argv = vec![
        command[0].clone(),
        "--settings".to_string(),
        claude_settings(crystal),
    ];
    let args = match resume {
        Some(id) => {
            argv.push("--resume".to_string());
            argv.push(id.to_string());
            without_resume_flags(without_first_prompt(&command[1..], task))
        }
        None => command[1..].to_vec(),
    };
    argv.extend(with_instructions(&args, instructions));
    argv
}

/// A Claude Code command line from [`argv`] with `options` right after the
/// program, ahead of `--settings`. An option that takes several values,
/// like `--mcp-config`, takes every argument up to the next option, which
/// here is always `--settings`, so it can't take a first prompt given
/// without `--`. Any other program's command line stays as it is.
pub fn with_options(mut argv: Vec<String>, options: &[String]) -> Vec<String> {
    if program_name(&argv) == Some("claude") {
        argv.splice(1..1, options.iter().cloned());
    }
    argv
}

/// A command line from [`argv`] that picks an agent's conversation up again,
/// for a session moved into another worktree, at `cwd`. Claude Code and
/// Codex take `notice` as the next prompt in their conversation, after
/// their options, so they carry on there; Codex, which would go back to the
/// directory its conversation began in, is told `cwd` as well. Whether any
/// other agent would take a prompt there isn't known, so its command line
/// stays as it is, and it picks its conversation up waiting for the user.
pub fn moved(mut argv: Vec<String>, notice: &str, cwd: &Path) -> Vec<String> {
    let program = program_name(&argv);
    if !matches!(program, Some("claude" | "codex")) || argv.iter().any(|arg| arg == "--") {
        return argv;
    }
    if program == Some("codex") {
        argv.push("--cd".to_string());
        argv.push(cwd.to_string_lossy().into_owned());
    }
    argv.push("--".to_string());
    argv.push(notice.to_string());
    argv
}

/// Claude's arguments with `value` given to the option `names` names too:
/// right after the option where the user gave it, which takes several
/// values, or else ahead of the rest.
pub fn with_value(args: &[String], names: &[&str], value: &str) -> Vec<String> {
    let before_prompt = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let mut with = args.to_vec();
    match args[..before_prompt]
        .iter()
        .position(|arg| names.contains(&arg.as_str()))
    {
        Some(at) => with.insert(at + 1, value.to_string()),
        None => {
            with.insert(0, value.to_string());
            with.insert(0, names[0].to_string());
        }
    }
    with
}

/// Claude's arguments with crystal's `instructions` added to its system
/// prompt. Claude takes `--append-system-prompt` once, so one the user gave
/// is taken out and comes first in the one crystal passes.
pub fn with_instructions(args: &[String], instructions: &[String]) -> Vec<String> {
    if instructions.is_empty() {
        return args.to_vec();
    }
    let mut kept = Vec::new();
    let mut paragraphs = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            // What follows is the first prompt, whatever it looks like.
            kept.push(arg.clone());
            kept.extend(args.by_ref().cloned());
        } else if arg == "--append-system-prompt" {
            paragraphs.extend(args.next().cloned());
        } else if let Some(text) = arg.strip_prefix("--append-system-prompt=") {
            paragraphs.push(text.to_string());
        } else {
            kept.push(arg.clone());
        }
    }
    paragraphs.extend(instructions.iter().cloned());
    // Ahead of the rest, so that it can't be taken for the first prompt.
    let mut with = vec![
        "--append-system-prompt".to_string(),
        paragraphs.join("\n\n"),
    ];
    with.extend(kept);
    with
}

/// `command` with crystal's `instructions` at the top of its agent's first
/// prompt, for an agent with no other way to be told them. Past
/// [`FIRST_PROMPT_BYTES`] they're left out from the last, what the memory
/// has first. A command with no first prompt to find, like any program
/// crystal doesn't know, stays as it is.
fn in_first_prompt(command: &[String], instructions: &[String]) -> Vec<String> {
    let mut command = command.to_vec();
    let Some(at) = catalog::first_prompt_at(&command) else {
        return command;
    };
    let size = |notes: &[String]| -> usize {
        let notes: usize = notes.iter().map(|note| note.len() + 2).sum();
        notes + command[at].len()
    };
    let mut notes = instructions;
    while !notes.is_empty() && size(notes) > FIRST_PROMPT_BYTES {
        notes = &notes[..notes.len() - 1];
    }
    // Where the notes come from says nothing on its own.
    if notes.is_empty() || notes == [ABOUT_CRYSTAL] {
        return command;
    }
    command[at] = format!("{}\n\n{}", notes.join("\n\n"), command[at]);
    command
}

/// The first of `names` a hook's input has as text: agents that copied
/// Claude Code's hooks spell their fields their own way.
fn text_field<'a>(input: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| input[name].as_str().filter(|text| !text.is_empty()))
}

/// The event a hook's input is for.
fn event_name(input: &Value) -> Option<&str> {
    text_field(input, &["hook_event_name", "hookEventName"])
}

/// The conversation a hook's input names, if it does.
pub fn hook_conversation(input: &Value) -> Option<Conversation> {
    let names = [
        "session_id",
        "sessionId",
        "conversation_id",
        "conversationId",
    ];
    let id = text_field(input, &names)?;
    let transcript = text_field(input, &["transcript_path", "transcriptPath"]).map(PathBuf::from);
    Some(Conversation {
        id: id.to_string(),
        transcript,
        prompted: false,
    })
}

/// What a Letta Code hook's input means: it hooks only a session
/// starting. Its `agent_id` is the Letta agent the conversation is with,
/// not a subagent.
pub fn letta_event(input: &Value) -> Option<AgentEvent> {
    (event_name(input)? == "SessionStart").then_some(AgentEvent::Started)
}

/// The conversation a Letta Code hook's input names: its id, or for the
/// agent's default conversation, which every agent has one of,
/// `default:<agent>`, as herdr writes it.
pub fn letta_conversation(input: &Value) -> Option<Conversation> {
    let id = text_field(input, &["conversation_id"])?;
    let id = match id {
        "default" => format!("default:{}", text_field(input, &["agent_id"])?),
        id => id.to_string(),
    };
    Some(Conversation {
        id,
        transcript: None,
        prompted: false,
    })
}

/// What the user asked, when a hook's input is for a prompt they sent.
pub fn hook_prompt(input: &Value) -> Option<String> {
    match event_name(input)? {
        "UserPromptSubmit" | "beforeSubmitPrompt" => input["prompt"].as_str().map(String::from),
        _ => None,
    }
}

/// Claude's arguments without its first prompt, for a conversation that
/// has had it already. After `--`, everything is the prompt. A command line
/// written before crystal put it there ends in the prompt instead, but a
/// last word can as well be an option's value, like `opus` in `--model
/// opus`, so it's taken for the prompt only when it's the session's `task`,
/// or when it's all there is.
fn without_first_prompt<'a>(args: &'a [String], task: Option<&str>) -> &'a [String] {
    if let Some(at) = args.iter().position(|arg| arg == "--") {
        return &args[..at];
    }
    match args.split_last() {
        Some((last, before)) if Some(last.as_str()) == task => before,
        Some((last, [])) if !last.starts_with('-') => &[],
        _ => args,
    }
}

/// Claude's arguments without the ones that choose a conversation to pick
/// up, since crystal is choosing it: `--resume` and `-r`, with the id
/// after them if there is one, and `--continue` and `-c`.
fn without_resume_flags(args: &[String]) -> Vec<String> {
    let mut kept = Vec::new();
    let mut args = args.iter().peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--continue" | "-c" => {}
            "--resume" | "-r" => {
                // The id is optional: without one, Claude offers a list.
                if args.peek().is_some_and(|next| !next.starts_with('-')) {
                    args.next();
                }
            }
            _ if arg.starts_with("--resume=") => {}
            _ => kept.push(arg.clone()),
        }
    }
    kept
}

/// What a hook's input means, or `None` if it's nothing that changes what
/// the session is doing: Claude Code's events, which Droid, Qoder, Qwen
/// Code, Devin, Kimi, MastraCode and Grok copied, adding a few of their
/// own, and Cursor's, spelled its own way. Codex's are [`codex_event`]'s.
pub fn hook_event(input: &Value) -> Option<AgentEvent> {
    let name = event_name(input)?;
    if in_subagent(input) && !matches!(name, "PostToolUse" | "PermissionRequest" | "Notification") {
        return None;
    }
    let event = match name {
        // A session starts again after its context is compacted, which can
        // happen mid-turn.
        "SessionStart" if input["source"] == "compact" => return None,
        "SessionStart" | "sessionStart" | "session_start" => AgentEvent::Started,
        "UserPromptSubmit" | "beforeSubmitPrompt" => AgentEvent::TurnStarted,
        // An answered permission puts the agent back to work, as a tool
        // finishing does.
        "PostToolUse" | "PermissionResult" => AgentEvent::ToolFinished,
        "PermissionRequest" => AgentEvent::Asking,
        "Stop" | "stop" | "AgentEnd" => AgentEvent::TurnEnded,
        // A turn the user cut short.
        "Interrupt" => AgentEvent::StillIdle,
        // Antigravity's, as it calls its model, and crystal's own, from
        // the plugins it gives agents, for a hook that only names the
        // conversation: their turns are read off their screens.
        "PreInvocation" | "SessionNamed" => AgentEvent::Named,
        "Notification" => match input["notification_type"].as_str()? {
            "permission_prompt" | "elicitation_dialog" => AgentEvent::Asking,
            "idle_prompt" => AgentEvent::StillIdle,
            _ => return None,
        },
        "SubagentStart" => AgentEvent::SubagentStarted,
        "SubagentStop" => AgentEvent::SubagentStopped,
        _ => return None,
    };
    Some(event)
}

/// Whether a hook's input comes from inside a subagent, which names itself
/// in it, rather than the agent the user talks to. A subagent's own start
/// and end aren't the agent's turn starting and ending: they come as
/// `SubagentStart` and `SubagentStop`, about it, from the agent. Its tools
/// and the permissions it asks for are the agent's work all the same.
fn in_subagent(input: &Value) -> bool {
    let name = event_name(input).unwrap_or_default();
    !name.starts_with("Subagent") && input["agent_id"].as_str().is_some()
}

/// The subagent a `SubagentStart` or `SubagentStop` hook's input is about.
pub fn subagent(input: &Value) -> Option<Subagent> {
    let id = input["agent_id"].as_str()?;
    Some(Subagent {
        id: id.to_string(),
        agent_type: input["agent_type"].as_str().map(String::from),
    })
}

/// The file a Claude Code `PreToolUse` hook's input says its agent is
/// about to read or edit with one of [`CLAUDE_FILE_TOOLS`], by its path
/// from the top, as the tool takes it. Not one a subagent reads, which only
/// what the subagent says of it reaches the agent with.
pub fn claude_file(input: &Value) -> Option<PathBuf> {
    if event_name(input)? != "PreToolUse" || in_subagent(input) {
        return None;
    }
    if !CLAUDE_FILE_TOOLS.contains(&input["tool_name"].as_str()?) {
        return None;
    }
    let tool_input = &input["tool_input"];
    let path = (tool_input["file_path"].as_str())
        .or_else(|| tool_input["notebook_path"].as_str())
        .filter(|path| !path.is_empty())?;
    let path = PathBuf::from(path);
    match path.is_absolute() {
        true => Some(path),
        false => Some(hook_cwd(input)?.join(path)),
    }
}

/// What a Codex hook's input means, or `None` if it's nothing that changes
/// what the session is doing. Codex's hooks are Claude Code's, mostly, with
/// `Interrupt` for a turn the user cut short, which is no turn ending the
/// agent's own way: it's only news if the agent was still working. What a
/// subagent does inside is left out: it may run in a conversation of its
/// own, which says nothing about where its agent runs, and the questions
/// it asks are read off the screen.
pub fn codex_event(input: &Value) -> Option<AgentEvent> {
    if in_subagent(input) {
        return None;
    }
    let event = match input["hook_event_name"].as_str()? {
        "SessionStart" if input["source"] == "compact" => return None,
        "SessionStart" => AgentEvent::Started,
        "UserPromptSubmit" => AgentEvent::TurnStarted,
        "PostToolUse" => AgentEvent::ToolFinished,
        "PermissionRequest" => AgentEvent::Asking,
        "Stop" => AgentEvent::TurnEnded,
        "Interrupt" => AgentEvent::StillIdle,
        "SubagentStart" => AgentEvent::SubagentStarted,
        "SubagentStop" => AgentEvent::SubagentStopped,
        _ => return None,
    };
    Some(event)
}

/// The conversation a Codex hook's input names, if it does: its thread,
/// and the rollout file it's recorded in, the same as Claude Code's.
pub fn codex_conversation(input: &Value) -> Option<Conversation> {
    hook_conversation(input)
}

/// What a hook's input says the agent's directory is.
pub fn hook_cwd(input: &Value) -> Option<PathBuf> {
    input["cwd"].as_str().map(PathBuf::from)
}

/// The model a hook's input says the agent runs on, as Claude Code's and
/// Codex's say as a session starts: its id, or an object with one. Not a
/// subagent's hook's, which may name the subagent's own.
pub fn hook_model(input: &Value) -> Option<String> {
    if input["agent_id"].is_string() {
        return None;
    }
    let model = &input["model"];
    let id = model
        .as_str()
        .or_else(|| model["id"].as_str())
        .or_else(|| model["display_name"].as_str())?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// Work a hook's input says the agent just scheduled to wake it later:
/// Claude Code's `ScheduleWakeup`, with how long it waits, at most the hour
/// it takes, or `CronCreate`, which may come again and again. A wakeup told
/// to stop schedules nothing.
pub fn hook_wakeup(input: &Value) -> Option<Wakeup> {
    if event_name(input)? != "PostToolUse" {
        return None;
    }
    match input["tool_name"].as_str()? {
        "ScheduleWakeup" => {
            let secs = input["tool_input"]["delaySeconds"].as_f64()?;
            Some(Wakeup::After {
                secs: secs.clamp(0.0, 3600.0) as u64,
            })
        }
        "CronCreate" => Some(Wakeup::Recurring),
        _ => None,
    }
}

/// The most of what an agent said last that's passed on: the end of it, a
/// long report's, where what it asks or waits on is.
const SAID_BYTES: usize = 16 * 1024;

/// The most of a pending task's description that's kept.
const PENDING_WHAT: usize = 120;

/// What a `Stop` hook's input says the agent said last as its turn ended:
/// Claude Code's `last_assistant_message`, the end of it at most
/// [`SAID_BYTES`].
pub fn hook_said(input: &Value) -> Option<String> {
    if event_name(input)? != "Stop" {
        return None;
    }
    let said = input["last_assistant_message"].as_str()?;
    let mut from = said.len().saturating_sub(SAID_BYTES);
    while !said.is_char_boundary(from) {
        from += 1;
    }
    Some(said[from..].to_string())
}

/// The work of the agent's own a `Stop` hook's input says is still to
/// come, which wakes it: Claude Code's `background_tasks` that haven't
/// ended, and its `session_crons`. `None` when the input has neither, as
/// from a Claude Code from before it said.
pub fn hook_pending(input: &Value) -> Option<Vec<Pending>> {
    if event_name(input)? != "Stop" {
        return None;
    }
    let (tasks, crons) = (&input["background_tasks"], &input["session_crons"]);
    if !tasks.is_array() && !crons.is_array() {
        return None;
    }
    let ended = |status: &str| {
        [
            "completed",
            "failed",
            "killed",
            "stopped",
            "cancelled",
            "done",
        ]
        .contains(&status)
    };
    let mut pending = Vec::new();
    for task in tasks.as_array().into_iter().flatten() {
        if task["status"].as_str().is_some_and(ended) {
            continue;
        }
        let kind = match task["type"].as_str().unwrap_or_default() {
            "shell" => PendingKind::Shell,
            "monitor" => PendingKind::Monitor,
            "subagent" => PendingKind::Subagent,
            _ => PendingKind::Other,
        };
        let what = (task["description"].as_str())
            .filter(|what| !what.trim().is_empty())
            .or_else(|| task["command"].as_str());
        pending.push(Pending {
            kind,
            what: what.map(|what| cut(&printable::line(what), PENDING_WHAT)),
        });
    }
    for cron in crons.as_array().into_iter().flatten() {
        let kind = match cron["recurring"].as_bool() {
            Some(true) => PendingKind::Cron,
            _ => PendingKind::Wakeup,
        };
        pending.push(Pending { kind, what: None });
    }
    Some(pending)
}

/// `text`, trimmed, with no more than `most` characters, `…` marking what
/// was cut.
fn cut(text: &str, most: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= most {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(most).collect();
    cut.push('…');
    cut
}

/// The command that picks `agent`'s conversation `id` up again, typed into
/// the shell of a session it was started in by hand: `None` for an agent
/// crystal can't resume. `agent` is as its hooks name it.
pub fn resume_typed(agent: &str, id: &str) -> Option<Vec<String>> {
    // Cursor's other program, `agent`, is too common a name to type.
    let program = match agent {
        "cursor" => "cursor-agent",
        agent => agent,
    };
    let mut argv = vec![program.to_string()];
    argv.extend(resume_args(agent, id)?);
    Some(argv)
}

/// The longest conversation id crystal puts on a command line, as herdr
/// takes them.
const LONGEST_ID: usize = 512;

/// Whether `id`, as an agent's hooks named it, can go on a command line,
/// and be typed into any shell and read the same: something, not too long,
/// not taken for an option, with no quote, and nothing a terminal takes as
/// an order (see [`printable`]).
fn fits_a_command_line(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= LONGEST_ID
        && !id.starts_with('-')
        && !id.contains('\'')
        && !id.contains(printable::is_unprintable)
}

/// What chooses `agent`'s conversation `id` on its command line, as each
/// agent takes it (herdr's list): `None` for an agent crystal can't
/// resume, or an id that can't go on a command line. Claude Code's and
/// Codex's own command lines are [`argv`]'s.
fn resume_args(agent: &str, id: &str) -> Option<Vec<String>> {
    if !fits_a_command_line(id) {
        return None;
    }
    let option = match agent {
        "codex" => "resume",
        "claude" | "cursor" | "droid" | "qodercli" | "qwen" | "devin" | "hermes" | "grok" => {
            "--resume"
        }
        "copilot" => return Some(vec![format!("--resume={id}")]),
        "kimi" | "pi" | "opencode" | "kilo" => "--session",
        "mastracode" => "--thread",
        "agy" => "--conversation",
        // An agent's default conversation is chosen with the agent.
        "letta" => match id.strip_prefix("default:") {
            Some("") => return None,
            Some(letta_agent) => {
                let args = ["--conversation", "default", "--agent", letta_agent];
                return Some(args.map(String::from).to_vec());
            }
            None => "--conversation",
        },
        _ => return None,
    };
    Some(vec![option.to_string(), id.to_string()])
}

/// The command line that picks `command`'s agent up again in its
/// conversation `id`, for an agent other than Claude Code and Codex: its
/// program, what chooses the conversation, then the arguments it was
/// started with, but for its first prompt, which the conversation has had
/// (found as for Claude, by `task` when it's last), and options that chose
/// a conversation, since crystal is choosing it. `None` for an agent
/// crystal can't resume.
fn resumed(command: &[String], id: &str, task: Option<&str>) -> Option<Vec<String>> {
    let program = program_name(command)?;
    let registry = agent_rules::current();
    let agent = registry
        .find(program)
        .map_or(program, |rules| rules.id.as_str());
    let chosen = resume_args(agent, id)?;
    let mut args = catalog::without_first_prompt(command).split_off(1);
    if task.is_some() && args.last().map(String::as_str) == task {
        args.pop();
    }
    let mut argv = vec![command[0].clone()];
    argv.extend(without_choosing(&args, &chosen));
    argv.splice(1..1, chosen);
    Some(argv)
}

/// An agent's arguments without those that choose a conversation: the
/// options in `chosen`, which crystal gives instead, and `--resume` and
/// `--continue`, which most agents choose one with, each with its value
/// if it has one. Past `--`, nothing is an option.
fn without_choosing(args: &[String], chosen: &[String]) -> Vec<String> {
    // An option in `chosen` followed by something else takes a value.
    let mut valued: Vec<&str> = Vec::new();
    for (at, arg) in chosen.iter().enumerate() {
        let next = chosen.get(at + 1);
        if arg.starts_with("--") && next.is_some_and(|next| !next.starts_with('-')) {
            valued.push(arg.split('=').next().unwrap_or(arg));
        }
    }
    let mut kept = Vec::new();
    let mut args = args.iter().peekable();
    while let Some(arg) = args.next() {
        if arg == "--" {
            kept.push(arg.clone());
            kept.extend(args.by_ref().cloned());
            break;
        }
        let name = arg.split('=').next().unwrap_or(arg);
        let chooses = name == "--continue"
            || name == "--resume"
            || valued.contains(&name)
            || chosen
                .iter()
                .any(|given| given.split('=').next() == Some(name));
        if !chooses {
            kept.push(arg.clone());
            continue;
        }
        // `--resume` may come without an id, for a list to choose from.
        let takes_value = name != "--continue" && !arg.contains('=');
        if takes_value && args.peek().is_some_and(|next| !next.starts_with('-')) {
            args.next();
        }
    }
    kept
}

/// What a Claude Code Stop hook prints to keep Claude from ending its turn:
/// it carries on, with `reason` as what it's told next.
pub fn claude_keep_going(reason: &str) -> String {
    json!({ "decision": "block", "reason": reason }).to_string()
}

/// What a Claude Code hook for `event` prints for Claude to read
/// `context` with what set it off: its `additionalContext`. With the
/// prompt the user just sent, `UserPromptSubmit`'s, Claude reads it before
/// the prompt; with a tool it's about to use, `PreToolUse`'s, beside what
/// the tool gives it.
pub fn claude_context(event: &str, context: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": context,
        }
    })
    .to_string()
}

/// Settings for Claude Code that add a hook, `crystal hook claude`, to
/// each event in [`CLAUDE_HOOK_EVENTS`], and to `PreToolUse` for
/// [`CLAUDE_FILE_TOOLS`].
fn claude_settings(crystal: &Path) -> String {
    let command = hook_command(crystal, "claude", false);
    // Each event takes a list of matcher groups; with no matcher, a group
    // matches everything.
    let groups = json!([{
        "hooks": [{ "type": "command", "command": command, "timeout": 5 }]
    }]);
    let mut hooks = serde_json::Map::new();
    for event in CLAUDE_HOOK_EVENTS {
        hooks.insert(event.to_string(), groups.clone());
    }
    // Claude waits on it before each read and edit, so it gives up on the
    // daemon sooner than this.
    let before_files = json!([{
        "matcher": CLAUDE_FILE_TOOLS.join("|"),
        "hooks": [{ "type": "command", "command": command, "timeout": 2 }]
    }]);
    hooks.insert("PreToolUse".to_string(), before_files);
    json!({ "hooks": hooks }).to_string()
}

/// The program's file name: `claude` for `/usr/local/bin/claude`.
pub fn program_name(command: &[String]) -> Option<&str> {
    let program = command.first()?;
    Path::new(program).file_name()?.to_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    fn input(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_moved_agent_picks_its_conversation_up_on_a_prompt_saying_where_it_is() {
        let cwd = Path::new("/code/app.worktrees/fix");
        let claude = command(&["claude", "--settings", "{}", "--resume", "c1"]);
        assert_eq!(
            moved(claude, "moved", cwd),
            command(&[
                "claude",
                "--settings",
                "{}",
                "--resume",
                "c1",
                "--",
                "moved"
            ])
        );
        let codex = command(&["codex", "resume", "c1", "-m", "o3"]);
        assert_eq!(
            moved(codex, "moved", cwd),
            command(&[
                "codex",
                "resume",
                "c1",
                "-m",
                "o3",
                "--cd",
                "/code/app.worktrees/fix",
                "--",
                "moved"
            ])
        );
        // Not known to take a prompt there, or given one already.
        let other = command(&["gemini", "--resume", "c1"]);
        assert_eq!(moved(other.clone(), "moved", cwd), other);
        let prompted = command(&["claude", "--resume", "c1", "--", "go on"]);
        assert_eq!(moved(prompted.clone(), "moved", cwd), prompted);
    }

    #[test]
    fn a_subagent_s_start_and_end_are_counted_not_taken_for_turns() {
        let start =
            input(r#"{"hook_event_name":"SubagentStart","agent_id":"a1","agent_type":"Explore"}"#);
        assert_eq!(hook_event(&start), Some(AgentEvent::SubagentStarted));
        assert_eq!(
            subagent(&start),
            Some(Subagent {
                id: "a1".into(),
                agent_type: Some("Explore".into())
            })
        );
        let stop = input(r#"{"hook_event_name":"SubagentStop","agent_id":"a1"}"#);
        assert_eq!(hook_event(&stop), Some(AgentEvent::SubagentStopped));
        assert_eq!(subagent(&stop).unwrap().agent_type, None);
    }

    #[test]
    fn what_happens_inside_a_subagent_is_its_agent_s_work_but_not_its_turn() {
        let inside = |event: &str| {
            hook_event(&input(&format!(
                r#"{{"hook_event_name":"{event}","agent_id":"a1","notification_type":"permission_prompt"}}"#
            )))
        };
        assert_eq!(inside("PostToolUse"), Some(AgentEvent::ToolFinished));
        assert_eq!(inside("PermissionRequest"), Some(AgentEvent::Asking));
        assert_eq!(inside("Notification"), Some(AgentEvent::Asking));
        assert_eq!(inside("Stop"), None);
        assert_eq!(inside("SessionStart"), None);
        assert_eq!(inside("UserPromptSubmit"), None);
    }

    #[test]
    fn codex_s_hooks_mean_what_claude_code_s_do_and_an_interrupt_cuts_a_turn_short() {
        let codex = |json: &str| codex_event(&input(json));
        assert_eq!(
            codex(r#"{"hook_event_name":"SessionStart","source":"startup"}"#),
            Some(AgentEvent::Started)
        );
        assert_eq!(
            codex(r#"{"hook_event_name":"SessionStart","source":"compact"}"#),
            None
        );
        assert_eq!(
            codex(r#"{"hook_event_name":"UserPromptSubmit"}"#),
            Some(AgentEvent::TurnStarted)
        );
        assert_eq!(
            codex(r#"{"hook_event_name":"PermissionRequest"}"#),
            Some(AgentEvent::Asking)
        );
        assert_eq!(
            codex(r#"{"hook_event_name":"Stop"}"#),
            Some(AgentEvent::TurnEnded)
        );
        assert_eq!(
            codex(r#"{"hook_event_name":"Interrupt"}"#),
            Some(AgentEvent::StillIdle)
        );
        assert_eq!(
            codex(r#"{"hook_event_name":"SubagentStop","agent_id":"a1"}"#),
            Some(AgentEvent::SubagentStopped)
        );
        // What a subagent does inside says nothing of where its agent runs.
        assert_eq!(
            codex(r#"{"hook_event_name":"PostToolUse","agent_id":"a1"}"#),
            None
        );
        assert_eq!(codex(r#"{"hook_event_name":"PreToolUse"}"#), None);
        let conversation = codex_conversation(&input(
            r#"{"session_id":"t-1","transcript_path":"/c/rollout.jsonl","cwd":"/app"}"#,
        ))
        .unwrap();
        assert_eq!(conversation.id, "t-1");
        assert_eq!(
            hook_cwd(&input(r#"{"cwd":"/app"}"#)),
            Some(PathBuf::from("/app"))
        );
    }

    #[test]
    fn an_agent_typed_into_a_shell_resumes_with_its_own_command() {
        let typed = |agent: &str, id: &str| resume_typed(agent, id).unwrap().join(" ");
        assert_eq!(typed("claude", "c-1"), "claude --resume c-1");
        assert_eq!(typed("codex", "t-1"), "codex resume t-1");
        assert_eq!(typed("cursor", "c-2"), "cursor-agent --resume c-2");
        assert_eq!(typed("droid", "d"), "droid --resume d");
        assert_eq!(typed("qodercli", "q"), "qodercli --resume q");
        assert_eq!(typed("qwen", "q"), "qwen --resume q");
        assert_eq!(typed("copilot", "p"), "copilot --resume=p");
        assert_eq!(typed("devin", "d"), "devin --resume d");
        assert_eq!(typed("kimi", "k"), "kimi --session k");
        assert_eq!(typed("mastracode", "m"), "mastracode --thread m");
        assert_eq!(typed("pi", "/s/p.jsonl"), "pi --session /s/p.jsonl");
        assert_eq!(typed("hermes", "h"), "hermes --resume h");
        assert_eq!(typed("opencode", "o"), "opencode --session o");
        assert_eq!(typed("kilo", "k"), "kilo --session k");
        assert_eq!(typed("agy", "a"), "agy --conversation a");
        assert_eq!(typed("grok", "g"), "grok --resume g");
        assert_eq!(typed("letta", "conv-1"), "letta --conversation conv-1");
        assert_eq!(
            typed("letta", "default:agent-9"),
            "letta --conversation default --agent agent-9"
        );
        assert_eq!(resume_typed("letta", "default:"), None);
        assert_eq!(resume_typed("gemini", "x"), None);
        // An id a shell or a terminal could take for more than an id isn't
        // typed in, or run.
        for hostile in [
            "",
            "--yolo",
            "k'1",
            "k-1\nrm -rf ~",
            "k-1\u{1b}]52;c;eA==\u{7}",
            "\u{202e}1-k",
        ] {
            assert_eq!(resume_typed("kimi", hostile), None, "{hostile:?}");
        }
        assert_eq!(resume_typed("kimi", &"k".repeat(LONGEST_ID + 1)), None);
        let typed = resume_typed("pi", "/s/my session.jsonl").unwrap();
        assert!(crate::report::check_resume(&typed).is_ok());
        let asked = command(&["kimi"]);
        let crystal = Path::new("/bin/crystal");
        assert_eq!(argv(&asked, crystal, Some("--yolo"), None, &[]), asked);
    }

    #[test]
    fn an_agent_crystal_started_resumes_without_its_first_prompt() {
        let crystal = Path::new("/bin/crystal");
        let resumed = |asked: &[&str], task: Option<&str>| {
            argv(&command(asked), crystal, Some("s-1"), task, &[]).join(" ")
        };
        assert_eq!(
            resumed(&["/opt/bin/kimi", "--yolo"], None),
            "/opt/bin/kimi --session s-1 --yolo"
        );
        assert_eq!(
            resumed(
                &["cursor-agent", "--model", "x", "--", "fix it"],
                Some("fix it")
            ),
            "cursor-agent --resume s-1 --model x"
        );
        assert_eq!(
            resumed(&["opencode", ".", "--prompt", "fix it"], None),
            "opencode --session s-1 ."
        );
        assert_eq!(resumed(&["pi", "fix it"], None), "pi --session s-1");
        assert_eq!(
            resumed(&["pi", "--model", "x", "fix it"], Some("fix it")),
            "pi --session s-1 --model x"
        );
        assert_eq!(resumed(&["copilot"], None), "copilot --resume=s-1");
        // What chose a conversation makes way for crystal's choice.
        assert_eq!(
            resumed(
                &["qwen", "--continue", "--resume", "old", "--model", "x"],
                None
            ),
            "qwen --resume s-1 --model x"
        );
        assert_eq!(
            resumed(&["kimi", "--session=old", "-y"], None),
            "kimi --session s-1 -y"
        );
        let letta = argv(
            &command(&["letta", "--agent", "old", "--conversation", "c"]),
            crystal,
            Some("default:agent-9"),
            None,
            &[],
        );
        assert_eq!(
            letta,
            command(&["letta", "--conversation", "default", "--agent", "agent-9"])
        );
        // Picked up again, the notes aren't put in a prompt that's gone.
        let notes = notes(&["About the task."]);
        let qwen = argv(
            &command(&["qwen", "-i", "fix it"]),
            crystal,
            Some("s-1"),
            None,
            &notes,
        );
        assert_eq!(qwen, command(&["qwen", "--resume", "s-1"]));
        // An agent crystal can't resume starts as it was asked.
        let gemini = command(&["gemini", "-i", "fix it"]);
        assert_eq!(argv(&gemini, crystal, Some("s-1"), None, &[]), gemini);
    }

    #[test]
    fn the_newer_agents_hooks_mean_what_claude_code_s_do() {
        let event = |json: Value| hook_event(&json);
        assert_eq!(
            event(json!({"hook_event_name": "PermissionResult"})),
            Some(AgentEvent::ToolFinished)
        );
        assert_eq!(
            event(json!({"hook_event_name": "AgentEnd"})),
            Some(AgentEvent::TurnEnded)
        );
        assert_eq!(
            event(json!({"hook_event_name": "session_start", "source": "new"})),
            Some(AgentEvent::Started)
        );
        // Antigravity's, as it calls its model, only names its
        // conversation, as crystal's own plugins' `SessionNamed` does.
        let invoked = json!({
            "hook_event_name": "PreInvocation",
            "conversationId": "a-1",
            "transcriptPath": "/t/a-1.pb",
        });
        assert_eq!(hook_event(&invoked), Some(AgentEvent::Named));
        let conversation = hook_conversation(&invoked).unwrap();
        assert_eq!(conversation.id, "a-1");
        assert_eq!(conversation.transcript, Some(PathBuf::from("/t/a-1.pb")));
        assert_eq!(
            event(json!({"hook_event_name": "SessionNamed"})),
            Some(AgentEvent::Named)
        );
    }

    #[test]
    fn letta_names_its_conversation_with_its_agent() {
        let started = json!({
            "hook_event_name": "SessionStart",
            "conversation_id": "conv-7",
            "agent_id": "agent-9",
        });
        // Its agent is no subagent.
        assert_eq!(letta_event(&started), Some(AgentEvent::Started));
        assert_eq!(letta_conversation(&started).unwrap().id, "conv-7");
        let default = json!({"conversation_id": "default", "agent_id": "agent-9"});
        assert_eq!(letta_conversation(&default).unwrap().id, "default:agent-9");
        assert_eq!(
            letta_conversation(&json!({"conversation_id": "default"})),
            None
        );
        assert_eq!(letta_event(&json!({"hook_event_name": "Stop"})), None);
    }

    #[test]
    fn the_installed_hook_is_crystal_s_own_with_a_flag() {
        let crystal = Path::new("/my apps/crystal");
        assert_eq!(
            hook_command(crystal, "claude", false),
            "'/my apps/crystal' hook claude"
        );
        assert_eq!(
            hook_command(crystal, "codex", true),
            "'/my apps/crystal' hook codex --installed"
        );
    }

    #[test]
    fn crystal_s_instructions_join_the_users_own_system_prompt() {
        let asked = command(&["claude", "--append-system-prompt", "Be brief.", "fix it"]);
        let instructions = ["Run `crystal done` when finished.".to_string()];
        let argv = argv(&asked, Path::new("/bin/crystal"), None, None, &instructions);
        assert_eq!(
            argv[3..],
            [
                "--append-system-prompt",
                "Be brief.\n\nRun `crystal done` when finished.",
                "fix it"
            ]
        );
    }

    #[test]
    fn a_value_joins_the_option_where_the_user_gave_it_or_comes_first() {
        let names = ["--allowedTools", "--allowed-tools"];
        let own = command(&["--allowed-tools", "Bash", "Edit", "--model", "opus"]);
        assert_eq!(
            with_value(&own, &names, "mcp__crystal__memory_search"),
            command(&[
                "--allowed-tools",
                "mcp__crystal__memory_search",
                "Bash",
                "Edit",
                "--model",
                "opus"
            ])
        );
        // An option that's in the prompt isn't one.
        let none = command(&["--model", "opus", "--", "--allowedTools"]);
        assert_eq!(
            with_value(&none, &names, "x"),
            command(&[
                "--allowedTools",
                "x",
                "--model",
                "opus",
                "--",
                "--allowedTools"
            ])
        );
    }

    #[test]
    fn a_first_prompt_after_the_double_dash_is_left_as_it_is() {
        let asked = command(&[
            "claude",
            "--",
            "--append-system-prompt=x, explain this flag",
        ]);
        let instructions = ["Run `crystal done` when finished.".to_string()];
        let argv = argv(&asked, Path::new("/bin/crystal"), None, None, &instructions);
        assert_eq!(
            argv[3..],
            [
                "--append-system-prompt",
                "Run `crystal done` when finished.",
                "--",
                "--append-system-prompt=x, explain this flag"
            ]
        );
    }

    #[test]
    fn without_instructions_claude_s_arguments_stay_as_they_are() {
        let asked = command(&["claude", "--append-system-prompt", "Be brief."]);
        let argv = argv(&asked, Path::new("/bin/crystal"), None, None, &[]);
        assert_eq!(argv[3..], ["--append-system-prompt", "Be brief."]);
    }

    #[test]
    fn crystal_s_options_come_ahead_of_settings_so_they_can_t_take_the_prompt() {
        let asked = command(&["claude", "fix it"]);
        let argv = argv(&asked, Path::new("/bin/crystal"), None, None, &[]);
        let options = command(&["--allowedTools", "mcp__crystal__memory_search"]);
        let with = with_options(argv, &options);
        assert_eq!(
            with[..4],
            [
                "claude",
                "--allowedTools",
                "mcp__crystal__memory_search",
                "--settings"
            ]
        );
        assert_eq!(with.last().unwrap(), "fix it");

        let other = command(&["aider", "fix it"]);
        assert_eq!(with_options(other.clone(), &options), other);
    }

    fn notes(said: &[&str]) -> Vec<String> {
        let mut notes = vec![ABOUT_CRYSTAL.to_string()];
        notes.extend(said.iter().map(|note| note.to_string()));
        notes
    }

    #[test]
    fn an_agent_with_no_option_for_notes_hears_them_atop_its_first_prompt() {
        let crystal = Path::new("/bin/crystal");
        let notes = notes(&["About the task.", "What was learned."]);
        let told = format!("{ABOUT_CRYSTAL}\n\nAbout the task.\n\nWhat was learned.\n\nfix it");
        let gemini = argv(
            &command(&["gemini", "-i", "fix it"]),
            crystal,
            None,
            None,
            &notes,
        );
        assert_eq!(gemini, ["gemini", "-i", told.as_str()]);
        let cursor = argv(
            &command(&["cursor-agent", "--", "fix it"]),
            crystal,
            None,
            None,
            &notes,
        );
        assert_eq!(cursor, ["cursor-agent", "--", told.as_str()]);

        // With no first prompt, or a program crystal doesn't know, there's
        // nowhere to say them.
        for asked in [
            command(&["gemini"]),
            command(&["aider", "--model", "o3"]),
            command(&["sh", "-c", "fix it"]),
        ] {
            assert_eq!(argv(&asked, crystal, None, None, &notes), asked);
        }
    }

    #[test]
    fn notes_too_long_for_a_first_prompt_are_left_out_from_the_last() {
        let crystal = Path::new("/bin/crystal");
        let asked = command(&["gemini", "-i", &"x".repeat(FIRST_PROMPT_BYTES - 400)]);
        let long = "~".repeat(500);
        let argv = argv(
            &asked,
            crystal,
            None,
            None,
            &notes(&["About the task.", &long]),
        );
        let prompt = &argv[2];
        assert!(prompt.starts_with(ABOUT_CRYSTAL), "{prompt}");
        assert!(prompt.contains("About the task.") && !prompt.contains('~'));

        // Where the notes come from, alone, isn't worth saying.
        let asked = command(&["gemini", "-i", &"x".repeat(FIRST_PROMPT_BYTES)]);
        assert_eq!(
            super::argv(&asked, crystal, None, None, &notes(&["About the task."])),
            asked
        );
    }

    #[test]
    fn codex_hears_its_notes_elsewhere_than_its_first_prompt() {
        let asked = command(&["codex", "--", "fix it"]);
        let notes = notes(&["About the task."]);
        assert_eq!(
            argv(&asked, Path::new("/bin/crystal"), None, None, &notes),
            asked
        );
    }

    #[test]
    fn other_programs_run_as_asked() {
        let asked = command(&["aider", "--model", "o3"]);
        assert_eq!(
            argv(&asked, Path::new("/bin/crystal"), Some("abc"), None, &[]),
            asked
        );
    }

    #[test]
    fn codex_resumes_its_conversation_with_its_own_subcommand() {
        let asked = command(&["codex", "--model", "o4", "fix it"]);
        assert_eq!(
            argv(&asked, Path::new("/bin/crystal"), Some("abc"), None, &[]),
            ["codex", "resume", "abc", "--model", "o4"]
        );
        assert_eq!(
            argv(&asked, Path::new("/bin/crystal"), None, None, &[]),
            asked
        );
    }

    #[test]
    fn claude_resumes_the_conversation_it_was_in() {
        let asked = command(&["claude", "--continue", "--model", "opus"]);
        let argv = argv(&asked, Path::new("/bin/crystal"), Some("abc"), None, &[]);
        assert_eq!(argv[3..], ["--resume", "abc", "--model", "opus"]);
    }

    #[test]
    fn a_resumed_conversation_isn_t_asked_its_first_prompt_again() {
        let asked = command(&["claude", "--model", "opus", "--", "fix it"]);
        let crystal = Path::new("/bin/crystal");
        let resumed = argv(&asked, crystal, Some("abc"), Some("fix it"), &[]);
        assert_eq!(resumed[3..], ["--resume", "abc", "--model", "opus"]);
        // After `--`, it's the prompt whether the task is known or not.
        let resumed = argv(&asked, crystal, Some("abc"), None, &[]);
        assert_eq!(resumed[3..], ["--resume", "abc", "--model", "opus"]);
    }

    #[test]
    fn an_older_command_line_loses_the_task_it_ends_in() {
        let asked = command(&["claude", "--model", "opus", "fix it"]);
        let crystal = Path::new("/bin/crystal");
        let resumed = argv(&asked, crystal, Some("abc"), Some("fix it"), &[]);
        assert_eq!(resumed[3..], ["--resume", "abc", "--model", "opus"]);
    }

    #[test]
    fn an_option_s_value_isn_t_taken_for_a_prompt() {
        let asked = command(&["claude", "--model", "opus"]);
        let crystal = Path::new("/bin/crystal");
        let resumed = argv(&asked, crystal, Some("abc"), None, &[]);
        assert_eq!(resumed[3..], ["--resume", "abc", "--model", "opus"]);
        let resumed = argv(&asked, crystal, Some("abc"), Some("fix it"), &[]);
        assert_eq!(resumed[3..], ["--resume", "abc", "--model", "opus"]);
    }

    #[test]
    fn a_prompt_given_alone_is_left_out_even_without_its_task() {
        let asked = command(&["claude", "fix it"]);
        let resumed = argv(&asked, Path::new("/bin/crystal"), Some("abc"), None, &[]);
        assert_eq!(resumed[3..], ["--resume", "abc"]);
    }

    #[test]
    fn a_new_conversation_is_asked_its_first_prompt() {
        let asked = command(&["claude", "--", "fix it"]);
        let started = argv(&asked, Path::new("/bin/crystal"), None, Some("fix it"), &[]);
        assert_eq!(started[3..], ["--", "fix it"]);
    }

    #[test]
    fn a_prompt_that_looks_like_a_resume_flag_isn_t_one() {
        let asked = command(&["claude", "--", "-c is short for --continue"]);
        let resumed = argv(&asked, Path::new("/bin/crystal"), Some("abc"), None, &[]);
        assert_eq!(resumed[3..], ["--resume", "abc"]);
    }

    #[test]
    fn resume_flags_make_way_for_crystals_own() {
        let args = command(&["-r", "old", "--resume=older", "--resume", "-c", "--verbose"]);
        assert_eq!(without_resume_flags(&args), ["--verbose"]);
    }

    #[test]
    fn a_hook_names_its_conversation() {
        let input = json!({"session_id": "abc", "transcript_path": "/t/abc.jsonl"});
        let conversation = hook_conversation(&input).unwrap();
        assert_eq!(conversation.id, "abc");
        assert_eq!(conversation.transcript, Some(PathBuf::from("/t/abc.jsonl")));
        assert_eq!(hook_conversation(&json!({})), None);
    }

    #[test]
    fn a_hook_can_name_its_model() {
        let started = json!({"hook_event_name": "SessionStart", "model": "claude-opus-5-5"});
        assert_eq!(hook_model(&started), Some("claude-opus-5-5".into()));
        let object = json!({"model": {"id": "gpt-5-codex", "display_name": "GPT-5 Codex"}});
        assert_eq!(hook_model(&object), Some("gpt-5-codex".into()));
        assert_eq!(hook_model(&json!({"model": " "})), None);
        let subagent =
            json!({"hook_event_name": "SubagentStart", "agent_id": "a1", "model": "haiku"});
        assert_eq!(hook_model(&subagent), None);
        assert_eq!(hook_model(&json!({})), None);
    }

    #[test]
    fn a_wakeup_the_agent_scheduled_is_read_from_its_tool_call() {
        let used = |tool: &str, input: Value| {
            hook_wakeup(&json!({
                "hook_event_name": "PostToolUse",
                "tool_name": tool,
                "tool_input": input,
            }))
        };
        assert_eq!(
            used(
                "ScheduleWakeup",
                json!({"delaySeconds": 1200, "prompt": "/loop"})
            ),
            Some(Wakeup::After { secs: 1200 })
        );
        assert_eq!(
            used("ScheduleWakeup", json!({"delaySeconds": 90000})),
            Some(Wakeup::After { secs: 3600 })
        );
        // Told to stop, it schedules nothing.
        assert_eq!(used("ScheduleWakeup", json!({"stop": true})), None);
        assert_eq!(
            used("CronCreate", json!({"cron": "*/5 * * * *"})),
            Some(Wakeup::Recurring)
        );
        assert_eq!(used("Bash", json!({"command": "ls"})), None);
        let asked = json!({"hook_event_name": "PreToolUse", "tool_name": "CronCreate"});
        assert_eq!(hook_wakeup(&asked), None);
    }

    #[test]
    fn claude_gets_hooks_ahead_of_its_own_arguments() {
        let asked = command(&["/usr/local/bin/claude", "--resume"]);
        let argv = argv(&asked, Path::new("/opt/my tools/crystal"), None, None, &[]);
        assert_eq!(argv[0], "/usr/local/bin/claude");
        assert_eq!(argv[1], "--settings");
        assert_eq!(argv[3], "--resume");

        let settings: Value = serde_json::from_str(&argv[2]).unwrap();
        for event in CLAUDE_HOOK_EVENTS {
            let hook = &settings["hooks"][event][0]["hooks"][0];
            assert_eq!(hook["command"], "'/opt/my tools/crystal' hook claude");
        }
        // And before the tools that read and edit files, for what the
        // memory has about each.
        let before = &settings["hooks"]["PreToolUse"][0];
        assert_eq!(before["matcher"], "Read|Edit|Write|MultiEdit|NotebookEdit");
        assert_eq!(
            before["hooks"][0]["command"],
            "'/opt/my tools/crystal' hook claude"
        );
    }

    #[test]
    fn a_file_claude_reads_or_edits_is_found_in_its_hook_s_input() {
        let read = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Read",
            "tool_input": {"file_path": "/code/app/src/ledger.rs", "offset": 10},
        });
        assert_eq!(
            claude_file(&read),
            Some(PathBuf::from("/code/app/src/ledger.rs"))
        );
        let notebook = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "NotebookEdit",
            "cwd": "/code/app",
            "tool_input": {"notebook_path": "eda.ipynb"},
        });
        assert_eq!(
            claude_file(&notebook),
            Some(PathBuf::from("/code/app/eda.ipynb"))
        );
        let after = json!({"hook_event_name": "PostToolUse", "tool_name": "Read",
                           "tool_input": {"file_path": "/code/app/a.rs"}});
        assert_eq!(claude_file(&after), None);
        let bash = json!({"hook_event_name": "PreToolUse", "tool_name": "Bash",
                          "tool_input": {"command": "cat /code/app/a.rs"}});
        assert_eq!(claude_file(&bash), None);
        let mut subagent = read.clone();
        subagent["agent_id"] = json!("a1");
        assert_eq!(
            claude_file(&subagent),
            None,
            "a subagent's reads aren't the agent's"
        );
    }

    #[test]
    fn a_stop_hook_keeps_claude_going_with_a_reason() {
        let output: Value = serde_json::from_str(&claude_keep_going("Close \"it\".")).unwrap();
        assert_eq!(
            output,
            json!({"decision": "block", "reason": "Close \"it\"."})
        );
    }

    #[test]
    fn a_prompt_hook_adds_to_what_claude_reads_with_the_prompt() {
        let output = claude_context("UserPromptSubmit", "Name it.");
        let output: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            output["hookSpecificOutput"],
            json!({"hookEventName": "UserPromptSubmit", "additionalContext": "Name it."})
        );
    }

    #[test]
    fn a_stop_hook_says_what_the_agent_said_and_what_s_still_to_come() {
        let stop = json!({
            "hook_event_name": "Stop",
            "last_assistant_message": "I'm waiting on the tests, not on you.",
            "background_tasks": [
                {"id": "b1", "type": "shell", "status": "running",
                 "description": "", "command": "cargo test\n-- --test-threads=4"},
                {"id": "m1", "type": "monitor", "status": "running", "description": "CI checks"},
                {"id": "a1", "type": "subagent", "status": "running", "description": "Review"},
                {"id": "w1", "type": "workflow", "status": "pending"},
                {"id": "b0", "type": "shell", "status": "completed", "command": "ls"}
            ],
            "session_crons": [
                {"id": "c1", "schedule": "*/5 * * * *", "recurring": true, "prompt": "check"},
                {"id": "c2", "schedule": "30 14 6 10 *", "recurring": false, "prompt": "go"}
            ]
        });
        assert_eq!(
            hook_said(&stop).as_deref(),
            Some("I'm waiting on the tests, not on you.")
        );
        let pending = hook_pending(&stop).unwrap();
        let kinds: Vec<PendingKind> = pending.iter().map(|pending| pending.kind).collect();
        assert_eq!(
            kinds,
            [
                PendingKind::Shell,
                PendingKind::Monitor,
                PendingKind::Subagent,
                PendingKind::Other,
                PendingKind::Cron,
                PendingKind::Wakeup
            ]
        );
        // A command with no description is told by its command, on a line.
        assert_eq!(
            pending[0].what.as_deref(),
            Some("cargo test -- --test-threads=4")
        );
        assert_eq!(pending[1].what.as_deref(), Some("CI checks"));

        // Nothing in flight is none; a Claude Code that doesn't say, unknown.
        let idle = json!({"hook_event_name": "Stop", "background_tasks": [], "session_crons": []});
        assert_eq!(hook_pending(&idle), Some(Vec::new()));
        let older = json!({"hook_event_name": "Stop"});
        assert_eq!((hook_pending(&older), hook_said(&older)), (None, None));
        // Only a turn's end says.
        let prompt = json!({"hook_event_name": "UserPromptSubmit", "background_tasks": []});
        assert_eq!(hook_pending(&prompt), None);
    }

    #[test]
    fn what_a_long_turn_ended_saying_is_passed_on_from_its_end() {
        let long = format!("{}Should I merge it?", "é".repeat(SAID_BYTES));
        let stop = json!({"hook_event_name": "Stop", "last_assistant_message": long});
        let said = hook_said(&stop).unwrap();
        assert!(said.len() <= SAID_BYTES);
        assert!(said.ends_with("Should I merge it?"));
    }

    #[test]
    fn claude_hooks_mean_what_they_say() {
        let cases = [
            (
                json!({"hook_event_name": "SessionStart", "source": "startup"}),
                Some(AgentEvent::Started),
            ),
            (
                json!({"hook_event_name": "SessionStart", "source": "compact"}),
                None,
            ),
            (
                json!({"hook_event_name": "UserPromptSubmit"}),
                Some(AgentEvent::TurnStarted),
            ),
            (
                json!({"hook_event_name": "PostToolUse"}),
                Some(AgentEvent::ToolFinished),
            ),
            (
                json!({"hook_event_name": "PermissionRequest"}),
                Some(AgentEvent::Asking),
            ),
            (
                json!({"hook_event_name": "Stop"}),
                Some(AgentEvent::TurnEnded),
            ),
            (
                json!({"hook_event_name": "Notification", "notification_type": "permission_prompt"}),
                Some(AgentEvent::Asking),
            ),
            (
                json!({"hook_event_name": "Notification", "notification_type": "idle_prompt"}),
                Some(AgentEvent::StillIdle),
            ),
            (
                json!({"hook_event_name": "Notification", "notification_type": "auth_success"}),
                None,
            ),
            (json!({"hook_event_name": "PreCompact"}), None),
            (json!({"not": "a hook"}), None),
        ];
        for (input, expected) in cases {
            assert_eq!(hook_event(&input), expected, "for {input}");
        }
    }

    #[test]
    fn only_a_prompt_sent_says_what_the_user_asked() {
        let sent = json!({"hook_event_name": "UserPromptSubmit", "prompt": "fix the tests"});
        assert_eq!(hook_prompt(&sent).as_deref(), Some("fix the tests"));
        let stop = json!({"hook_event_name": "Stop", "prompt": "not one"});
        assert_eq!(hook_prompt(&stop), None);
    }

    #[test]
    fn other_agents_hooks_are_read_the_way_they_spell_them() {
        // Codex, Kimi and MastraCode say when a turn is cut short.
        let interrupt = json!({"hook_event_name": "Interrupt", "session_id": "t1"});
        assert_eq!(hook_event(&interrupt), Some(AgentEvent::StillIdle));
        assert_eq!(codex_event(&interrupt), Some(AgentEvent::StillIdle));
        // Cursor's events, and its conversation's id.
        let started = json!({"hook_event_name": "sessionStart", "conversation_id": "c9"});
        assert_eq!(hook_event(&started), Some(AgentEvent::Started));
        assert_eq!(hook_conversation(&started).unwrap().id, "c9");
        let sent = json!({"hook_event_name": "beforeSubmitPrompt", "prompt": "go"});
        assert_eq!(hook_event(&sent), Some(AgentEvent::TurnStarted));
        assert_eq!(hook_prompt(&sent).as_deref(), Some("go"));
        assert_eq!(
            hook_event(&json!({"hook_event_name": "stop"})),
            Some(AgentEvent::TurnEnded)
        );
        // Copilot's, in camel case.
        let copilot = json!({"hookEventName": "SessionStart", "sessionId": "p2"});
        assert_eq!(hook_event(&copilot), Some(AgentEvent::Started));
        assert_eq!(hook_conversation(&copilot).unwrap().id, "p2");
    }
}
