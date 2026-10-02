//! What crystal knows about particular agents: how to launch one so that it
//! reports what it's doing, and how to read what it reports. Any other
//! program runs exactly as it was asked for.
//!
//! Claude Code reports through hooks: commands it runs on events like a
//! prompt being sent or a turn ending. crystal adds its own with
//! `--settings`, so the user's settings files are never touched, and its
//! hooks run alongside any the user has.

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

/// The command line to run for `command`. For an agent crystal knows, it
/// carries the flags that make the agent report to `crystal hook`, run
/// from `crystal`, the path of this program, and with `resume`, the id of
/// a conversation to pick up again.
pub fn argv(command: &[String], crystal: &Path, resume: Option<&str>) -> Vec<String> {
    if program_name(command) != Some("claude") {
        return command.to_vec();
    }
    let mut argv = vec![
        command[0].clone(),
        "--settings".to_string(),
        claude_settings(crystal),
    ];
    match resume {
        Some(id) => {
            argv.push("--resume".to_string());
            argv.push(id.to_string());
            argv.extend(without_resume_flags(&command[1..]));
        }
        None => argv.extend_from_slice(&command[1..]),
    }
    argv
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
fn program_name(command: &[String]) -> Option<&str> {
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
    fn other_programs_run_as_asked() {
        let asked = command(&["codex", "--model", "o3"]);
        assert_eq!(argv(&asked, Path::new("/bin/crystal"), Some("abc")), asked);
    }

    #[test]
    fn claude_resumes_the_conversation_it_was_in() {
        let asked = command(&["claude", "--continue", "--model", "opus"]);
        let argv = argv(&asked, Path::new("/bin/crystal"), Some("abc"));
        assert_eq!(argv[3..], ["--resume", "abc", "--model", "opus"]);
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
        let argv = argv(&asked, Path::new("/opt/my tools/crystal"), None);
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
