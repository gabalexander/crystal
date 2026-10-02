//! What crystal knows about particular agents: how to launch one so that it
//! reports what it's doing, and how to read what it reports. Any other
//! program runs exactly as it was asked for.
//!
//! Claude Code reports through hooks: commands it runs on events like a
//! prompt being sent or a turn ending. crystal adds its own with
//! `--settings`, so the user's settings files are never touched, and its
//! hooks run alongside any the user has.

use crate::protocol::AgentEvent;
use crate::shell;
use serde_json::{Value, json};
use std::path::Path;

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
/// from `crystal`, the path of this program.
pub fn argv(command: &[String], crystal: &Path) -> Vec<String> {
    if program_name(command) != Some("claude") {
        return command.to_vec();
    }
    let mut argv = vec![
        command[0].clone(),
        "--settings".to_string(),
        claude_settings(crystal),
    ];
    argv.extend_from_slice(&command[1..]);
    argv
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
        assert_eq!(argv(&asked, Path::new("/bin/crystal")), asked);
    }

    #[test]
    fn claude_gets_hooks_ahead_of_its_own_arguments() {
        let asked = command(&["/usr/local/bin/claude", "--resume"]);
        let argv = argv(&asked, Path::new("/opt/my tools/crystal"));
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
