//! What crystal knows about particular agents: how to launch one so that it
//! reports what it's doing, and how to read what it reports. Any other
//! program runs exactly as it was asked for.
//!
//! Claude Code reports through hooks: commands it runs on events like a
//! prompt being sent or a turn ending. crystal adds its own with
//! `--settings`, so the user's settings files are never touched, and its
//! hooks run alongside any the user has.

use crate::codex;
use crate::protocol::{AgentEvent, Conversation};
use crate::shell;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The Claude Code hook events crystal listens to; [`claude_event`] says
/// what each one means.
const CLAUDE_HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "PermissionRequest",
    "Notification",
    "Stop",
];

/// What opens the notes crystal adds for an agent: where they come from.
/// An agent told to run the commands of a program it has never heard of
/// can take them for a prompt injection and ignore them.
pub const ABOUT_CRYSTAL: &str = "You're running inside crystal, the terminal workspace the \
                                 user runs their coding agents in. The user set crystal up \
                                 to add the notes below for you, and the `crystal` command \
                                 they mention is installed for you to run.";

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
/// prompt. Other agents have no such option, and go without.
pub fn argv(
    command: &[String],
    crystal: &Path,
    resume: Option<&str>,
    task: Option<&str>,
    instructions: &[String],
) -> Vec<String> {
    if program_name(command) == Some("codex")
        && let Some(id) = resume
        && let Some(argv) = codex::resume_argv(command, id)
    {
        return argv;
    }
    if program_name(command) != Some("claude") {
        return command.to_vec();
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

/// Claude's arguments with crystal's `instructions` added to its system
/// prompt. Claude takes `--append-system-prompt` once, so one the user gave
/// is taken out and comes first in the one crystal passes.
fn with_instructions(args: &[String], instructions: &[String]) -> Vec<String> {
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

/// The conversation a Claude Code hook's input names, if it does.
pub fn claude_conversation(input: &Value) -> Option<Conversation> {
    let id = input["session_id"].as_str()?;
    let transcript = input["transcript_path"].as_str().map(PathBuf::from);
    Some(Conversation {
        id: id.to_string(),
        transcript,
    })
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
    let event = match input["hook_event_name"].as_str()? {
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
        _ => return None,
    };
    Some(event)
}

/// Settings for Claude Code that add a hook, `crystal hook claude`, to
/// each event in [`CLAUDE_HOOK_EVENTS`].
fn claude_settings(crystal: &Path) -> String {
    let command = format!("{} hook claude", shell::quote(&crystal.to_string_lossy()));
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
}
