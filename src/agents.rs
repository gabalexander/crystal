//! What crystal knows about particular agents: how to launch one so that it
//! reports what it's doing and hears crystal's notes, and how to read what
//! it reports. Any other program runs exactly as it was asked for.
//!
//! Claude Code reports through hooks: commands it runs on events like a
//! prompt being sent or a turn ending. crystal adds its own with
//! `--settings`, so the user's settings files are never touched, and its
//! hooks run alongside any the user has.

use crate::catalog;
use crate::codex;
use crate::protocol::{AgentEvent, Conversation, Subagent};
use crate::shell;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The most a first prompt may be with crystal's notes at its top:
/// docket's limit for a starting prompt.
const FIRST_PROMPT_BYTES: usize = 16 * 1024;

/// The Claude Code hook events crystal listens to; [`claude_event`] says
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
/// than worktrees or subagents of its own, which they don't. Only Claude
/// Code makes those, and only it has a system prompt to say this in
/// without touching what the user asked.
pub const PARALLEL_WORK: &str = "To work on several things at once, start a crystal session \
                                 for each, `crystal new -d -w <branch> claude \"<task>\"` \
                                 (with `--base HEAD` when it should start from your \
                                 commits), rather than worktrees or subagents of your own: \
                                 each shows in the user's sidebar with its status, its diff \
                                 and its screen, where they can step in. The crystal skill \
                                 says more.";

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
        _ => return in_first_prompt(command, instructions),
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

/// The conversation a Claude Code hook's input names, if it does.
pub fn claude_conversation(input: &Value) -> Option<Conversation> {
    let id = input["session_id"].as_str()?;
    let transcript = input["transcript_path"].as_str().map(PathBuf::from);
    Some(Conversation {
        id: id.to_string(),
        transcript,
    })
}

/// What the user asked, when a Claude Code hook's input is for a prompt
/// they sent.
pub fn claude_prompt(input: &Value) -> Option<String> {
    if input["hook_event_name"] != "UserPromptSubmit" {
        return None;
    }
    input["prompt"].as_str().map(String::from)
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

/// What a Claude Code hook's input means, or `None` if it's nothing that
/// changes what the session is doing.
pub fn claude_event(input: &Value) -> Option<AgentEvent> {
    let name = input["hook_event_name"].as_str()?;
    if in_subagent(input) && !matches!(name, "PostToolUse" | "PermissionRequest" | "Notification") {
        return None;
    }
    let event = match name {
        // A session starts again after its context is compacted, which can
        // happen mid-turn.
        "SessionStart" if input["source"] == "compact" => return None,
        "SessionStart" => AgentEvent::Started,
        "UserPromptSubmit" => AgentEvent::TurnStarted,
        "PostToolUse" => AgentEvent::ToolFinished,
        "PermissionRequest" => AgentEvent::Asking,
        "Stop" => AgentEvent::TurnEnded,
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
    let name = input["hook_event_name"].as_str().unwrap_or_default();
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
    claude_conversation(input)
}

/// What a hook's input says the agent's directory is.
pub fn hook_cwd(input: &Value) -> Option<PathBuf> {
    input["cwd"].as_str().map(PathBuf::from)
}

/// The command that picks `agent`'s conversation `id` up again, typed into
/// the shell of a session it was started in by hand: `None` for an agent
/// crystal can't resume.
pub fn resume_typed(agent: &str, id: &str) -> Option<Vec<String>> {
    let argv = match agent {
        "claude" => ["claude", "--resume", id],
        "codex" => ["codex", "resume", id],
        _ => return None,
    };
    Some(argv.map(String::from).to_vec())
}

/// What a Claude Code Stop hook prints to keep Claude from ending its turn:
/// it carries on, with `reason` as what it's told next.
pub fn claude_keep_going(reason: &str) -> String {
    json!({ "decision": "block", "reason": reason }).to_string()
}

/// Settings for Claude Code that add a hook, `crystal hook claude`, to
/// each event in [`CLAUDE_HOOK_EVENTS`].
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
    fn a_subagent_s_start_and_end_are_counted_not_taken_for_turns() {
        let start =
            input(r#"{"hook_event_name":"SubagentStart","agent_id":"a1","agent_type":"Explore"}"#);
        assert_eq!(claude_event(&start), Some(AgentEvent::SubagentStarted));
        assert_eq!(
            subagent(&start),
            Some(Subagent {
                id: "a1".into(),
                agent_type: Some("Explore".into())
            })
        );
        let stop = input(r#"{"hook_event_name":"SubagentStop","agent_id":"a1"}"#);
        assert_eq!(claude_event(&stop), Some(AgentEvent::SubagentStopped));
        assert_eq!(subagent(&stop).unwrap().agent_type, None);
    }

    #[test]
    fn what_happens_inside_a_subagent_is_its_agent_s_work_but_not_its_turn() {
        let inside = |event: &str| {
            claude_event(&input(&format!(
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
        assert_eq!(
            resume_typed("claude", "c-1").unwrap(),
            ["claude", "--resume", "c-1"]
        );
        assert_eq!(
            resume_typed("codex", "t-1").unwrap(),
            ["codex", "resume", "t-1"]
        );
        assert_eq!(resume_typed("gemini", "x"), None);
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
        let conversation = claude_conversation(&input).unwrap();
        assert_eq!(conversation.id, "abc");
        assert_eq!(conversation.transcript, Some(PathBuf::from("/t/abc.jsonl")));
        assert_eq!(claude_conversation(&json!({})), None);
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
            assert_eq!(claude_event(&input), expected, "for {input}");
        }
    }

    #[test]
    fn only_a_prompt_sent_says_what_the_user_asked() {
        let sent = json!({"hook_event_name": "UserPromptSubmit", "prompt": "fix the tests"});
        assert_eq!(claude_prompt(&sent).as_deref(), Some("fix the tests"));
        let stop = json!({"hook_event_name": "Stop", "prompt": "not one"});
        assert_eq!(claude_prompt(&stop), None);
    }
}
