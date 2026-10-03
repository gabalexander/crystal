//! Claude Code's stream-json protocol, the part crystal speaks with a
//! background task's `claude -p`: prompts in, the conversation out, and the
//! control messages both ways that bring a permission prompt to the user
//! and take their answer back, or stop a turn.
//!
//! With `--input-format stream-json`, Claude reads one JSON object a line
//! from its standard input, and keeps reading: each user message is a turn,
//! so one process can take a prompt and every follow-up after it. With
//! `--permission-prompt-tool stdio`, a tool call that its permission mode
//! and rules don't settle comes out as a `can_use_tool` control request,
//! and the turn waits until it's answered. Control messages, both ways,
//! look like this:
//!
//! ```text
//! {"type":"control_request","request_id":"…","request":{"subtype":"…",…}}
//! {"type":"control_response","response":{"subtype":"success","request_id":"…","response":{…}}}
//! {"type":"control_response","response":{"subtype":"error","request_id":"…","error":"…"}}
//! ```
//!
//! and `{"type":"control_cancel_request","request_id":"…"}` takes back a
//! request Claude sent. Everything else on its output is the conversation,
//! which [`crate::transcript`] reads.
//!
//! Adapted from docket's `docket-claude` crate (its control and permission
//! modules), trimmed to what crystal does with it.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt;

/// What every background task's `claude` runs with: a prompt at a time on
/// its standard input, its events on its output, and its permission
/// prompts sent to crystal.
pub const ARGS: &[&str] = &[
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--permission-prompt-tool",
    "stdio",
];

/// What Claude is told when the user says no and gives no reason.
pub const DENIED: &str = "The user said no to this, from crystal.";

/// A turn's prompt, as Claude reads it.
pub fn user_message(text: &str) -> Value {
    json!({
        "type": "user",
        "message": {"role": "user", "content": text},
        "parent_tool_use_id": null,
        "session_id": "",
    })
}

/// Asks Claude to stop the turn it's in the middle of.
pub fn interrupt(request_id: &str) -> Value {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "interrupt"},
    })
}

/// The answer to a request crystal has no answer for. Claude waits for an
/// answer to every request it sends, so even these get one, in the words
/// Claude itself uses.
pub fn unsupported(request_id: &str, subtype: &str) -> Value {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "error",
            "request_id": request_id,
            "error": format!("Unsupported control request subtype: {subtype}"),
        },
    })
}

/// The user's answer to `request`.
pub fn answer(request: &PermissionRequest, decision: &Decision) -> Value {
    let response = match decision {
        // Claude wants the input back on every yes, changed or not.
        Decision::Allow { rule } => {
            let mut allow = json!({"behavior": "allow", "updatedInput": request.input});
            if let Some(rule) = rule {
                allow["updatedPermissions"] = json!([rule.update()]);
            }
            allow
        }
        Decision::Deny { message } => json!({"behavior": "deny", "message": message}),
    };
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request.request_id,
            "response": response,
        },
    })
}

/// What a line of Claude's output is.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    /// Claude asks whether it may use a tool, and waits for the answer.
    Permission(PermissionRequest),
    /// Claude takes back a request it sent: nobody waits on its answer.
    Withdrawn { request_id: String },
    /// A request crystal has no answer for, which gets an error.
    Unsupported { request_id: String, subtype: String },
    /// Claude's answer to a request crystal sent: `error` says why it
    /// refused, when it did.
    Reply {
        request_id: String,
        error: Option<String>,
    },
    /// Part of the conversation, for the transcript.
    Conversation,
    /// Not JSON, or nothing to act on, like a keep-alive.
    Nothing,
}

/// A `can_use_tool` request: the tool Claude wants to use, and how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub request_id: String,
    pub tool_name: String,
    pub input: Value,
}

/// The user's answer to a permission prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run the tool, and with a `rule`, don't ask about calls it matches
    /// again.
    Allow { rule: Option<Rule> },
    /// Don't run it: Claude is told `message`, as the tool's error, and
    /// carries on.
    Deny { message: String },
}

/// What a line of Claude's output is: see [`Line`].
pub fn read(line: &str) -> Line {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        return Line::Nothing;
    };
    let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
    match message["type"].as_str() {
        Some("control_request") => {
            let request_id = text(&message["request_id"]);
            let request = &message["request"];
            match request["subtype"].as_str().unwrap_or_default() {
                "can_use_tool" => Line::Permission(PermissionRequest {
                    request_id,
                    tool_name: text(&request["tool_name"]),
                    input: request["input"].clone(),
                }),
                subtype => Line::Unsupported {
                    request_id,
                    subtype: subtype.to_string(),
                },
            }
        }
        Some("control_cancel_request") => Line::Withdrawn {
            request_id: text(&message["request_id"]),
        },
        Some("control_response") => {
            let response = &message["response"];
            let error = (response["subtype"] == "error").then(|| text(&response["error"]));
            Line::Reply {
                request_id: text(&response["request_id"]),
                error,
            }
        }
        Some("keep_alive") | None => Line::Nothing,
        Some(_) => Line::Conversation,
    }
}

/// Commands whose first word is a family rather than a command: `cargo
/// test` and `cargo publish` are different asks, so a rule kept from one
/// keeps the second word.
const TWO_WORD_COMMANDS: &[&str] = &[
    "go", "git", "gh", "cargo", "npm", "make", "docker", "kubectl",
];

/// A permission rule, as Claude's settings spell it: `Bash(go test:*)`, or
/// a tool's name alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub tool: String,
    pub content: Option<String>,
}

impl Rule {
    /// The rule "always" keeps for a call of `tool` with `input`. For
    /// `Bash`, the command's first word as a prefix, or its first two for a
    /// family like `cargo`, unless the second is a flag: `cargo test --all`
    /// gives `Bash(cargo test:*)`, `ls -la` gives `Bash(ls:*)`. For any other
    /// tool, the tool itself. `None` for a `Bash` call without a command:
    /// the tool alone would allow every command there is.
    pub fn for_call(tool: &str, input: &Value) -> Option<Rule> {
        if tool != "Bash" {
            return Some(Rule {
                tool: tool.to_string(),
                content: None,
            });
        }
        let mut words = input["command"].as_str()?.split_whitespace();
        let first = words.next()?;
        let prefix = match words.next() {
            Some(second) if TWO_WORD_COMMANDS.contains(&first) && !second.starts_with('-') => {
                format!("{first} {second}")
            }
            _ => first.to_string(),
        };
        Some(Rule {
            tool: tool.to_string(),
            content: Some(format!("{prefix}:*")),
        })
    }

    /// The change to Claude's permissions that adds this rule: to the
    /// session, and to the checkout's `.claude/settings.local.json`, which
    /// Claude writes itself, so later sessions there have it too.
    fn update(&self) -> Value {
        let mut rule = json!({"toolName": self.tool});
        if let Some(content) = &self.content {
            rule["ruleContent"] = json!(content);
        }
        json!({
            "type": "addRules",
            "rules": [rule],
            "behavior": "allow",
            "destination": "localSettings",
        })
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.content {
            Some(content) => write!(f, "{}({content})", self.tool),
            None => f.write_str(&self.tool),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash(command: &str) -> PermissionRequest {
        PermissionRequest {
            request_id: "r1".into(),
            tool_name: "Bash".into(),
            input: json!({"command": command}),
        }
    }

    #[test]
    fn a_permission_prompt_is_read_from_its_control_request() {
        let line = r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cargo test"},"tool_use_id":"t1"}}"#;
        assert_eq!(read(line), Line::Permission(bash("cargo test")));
    }

    #[test]
    fn control_traffic_is_told_apart_from_the_conversation() {
        let hook =
            r#"{"type":"control_request","request_id":"r2","request":{"subtype":"hook_callback"}}"#;
        assert_eq!(
            read(hook),
            Line::Unsupported {
                request_id: "r2".into(),
                subtype: "hook_callback".into()
            }
        );
        assert_eq!(
            read(r#"{"type":"control_cancel_request","request_id":"r1"}"#),
            Line::Withdrawn {
                request_id: "r1".into()
            }
        );
        let refused = r#"{"type":"control_response","response":{"subtype":"error","request_id":"i1","error":"no turn"}}"#;
        assert_eq!(
            read(refused),
            Line::Reply {
                request_id: "i1".into(),
                error: Some("no turn".into())
            }
        );
        assert_eq!(
            read(r#"{"type":"assistant","message":{}}"#),
            Line::Conversation
        );
        assert_eq!(read(r#"{"type":"keep_alive"}"#), Line::Nothing);
        assert_eq!(read("[SandboxDebug] hello"), Line::Nothing);
    }

    #[test]
    fn a_yes_hands_the_input_back_and_a_no_says_why() {
        let request = bash("ls");
        let yes = answer(&request, &Decision::Allow { rule: None });
        assert_eq!(
            yes["response"],
            json!({"subtype": "success", "request_id": "r1",
                   "response": {"behavior": "allow", "updatedInput": {"command": "ls"}}})
        );
        let no = answer(
            &request,
            &Decision::Deny {
                message: "not now".into(),
            },
        );
        assert_eq!(
            no["response"]["response"],
            json!({"behavior": "deny", "message": "not now"})
        );
    }

    #[test]
    fn always_adds_its_rule_to_the_local_settings() {
        let request = bash("go test ./...");
        let rule = Rule::for_call("Bash", &request.input);
        let always = answer(&request, &Decision::Allow { rule });
        assert_eq!(
            always["response"]["response"]["updatedPermissions"],
            json!([{
                "type": "addRules",
                "rules": [{"toolName": "Bash", "ruleContent": "go test:*"}],
                "behavior": "allow",
                "destination": "localSettings"
            }])
        );
        let write = Rule::for_call("Write", &json!({"file_path": "x"})).unwrap();
        assert_eq!(write.update()["rules"], json!([{"toolName": "Write"}]));
    }

    #[test]
    fn a_rule_keeps_a_command_s_first_word_or_its_family_s_two() {
        let rule = |command: &str| {
            Rule::for_call("Bash", &json!({ "command": command })).map(|rule| rule.to_string())
        };
        assert_eq!(rule("go test ./...").as_deref(), Some("Bash(go test:*)"));
        assert_eq!(rule("ls -la").as_deref(), Some("Bash(ls:*)"));
        assert_eq!(rule("git -C x status").as_deref(), Some("Bash(git:*)"));
        assert_eq!(rule("   "), None);
        assert_eq!(Rule::for_call("Bash", &json!({})), None);
        let edit = Rule::for_call("Edit", &json!({})).unwrap();
        assert_eq!(edit.to_string(), "Edit");
    }
}
