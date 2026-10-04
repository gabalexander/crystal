//! The model a session's agent runs on, for its row in the sidebar: the
//! `--model` its command gave, what its hooks say as it starts, and after
//! that the newest switch in its conversation's transcript. Claude Code's
//! `/model` fires no hook, but writes the switch into the transcript at
//! once, as the output of a local command:
//!
//! ```text
//! {"type":"user","message":{"content":"<local-command-stdout>Set model to `Opus 5 (1M context)` and saved as your default…</local-command-stdout>"},…}
//! ```
//!
//! and with the next prompt, as with every first prompt, an attachment that
//! names it:
//!
//! ```text
//! {"type":"attachment","attachment":{"type":"model","identity":{"modelId":"claude-opus-5-5","marketingName":"Opus 5.5",…}},…}
//! ```
//!
//! Codex writes each turn's model into its rollout, in the turn's context:
//! `{"type":"turn_context","payload":{"model":"gpt-5-codex",…}}`, so a
//! `/model` there shows from the next turn.
//!
//! A transcript runs to megabytes, and a switch is one line somewhere in
//! it: the watch reads only what was written since it last looked, at most
//! [`MOST_READ`] at a time, and an unchanged file costs one `stat`. What a
//! transcript held when it was first seen is history, read only when
//! nothing else has said the model: a resumed agent may have been started
//! on another. Adapted from docket's `session_model`.

use crate::front;
use crate::protocol::Front;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// The most a look at a transcript reads of what was written since the
/// last: the rest waits for the next look.
const MOST_READ: u64 = 1024 * 1024;

/// How much of the end of a transcript is read for its model, when it's
/// first seen and nothing has said the model.
const TAIL: u64 = 256 * 1024;

/// What's known of the model a session's agent runs on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watch {
    /// The `--model` the session's command gave its agent.
    #[serde(default)]
    flag: Option<String>,
    /// What the agent's hooks or its transcript said since.
    #[serde(default)]
    said: Option<String>,
    /// The transcript read, and how far: to the end of the last whole line
    /// read.
    #[serde(default)]
    read: Option<(PathBuf, u64)>,
}

impl Watch {
    /// A watch for an agent started with `command`.
    pub fn new(command: &[String]) -> Watch {
        Watch {
            flag: from_command(command),
            ..Watch::default()
        }
    }

    /// The model, once something has said.
    pub fn model(&self) -> Option<&str> {
        self.said.as_deref().or(self.flag.as_deref())
    }

    /// Takes the model the agent's hooks name. Whether that changed it.
    pub fn heard(&mut self, model: &str) -> bool {
        let model = model.trim();
        if model.is_empty() || self.said.as_deref() == Some(model) {
            return false;
        }
        self.said = Some(model.to_string());
        true
    }

    /// The agent has left: what it said goes with it, and the transcript is
    /// read afresh if it comes back.
    pub fn forget(&mut self) {
        self.said = None;
        self.read = None;
    }

    /// Reads what `transcript` was added to since the last look. Whether
    /// that changed the model.
    pub fn look(&mut self, transcript: &Path) -> bool {
        let len = std::fs::metadata(transcript).map(|meta| meta.len()).ok();
        let read_to = self
            .read
            .as_ref()
            .filter(|(seen, _)| seen == transcript)
            .map(|(_, at)| *at);
        let seen = match (read_to, len) {
            // Claude Code writes no transcript before the first prompt:
            // all of it will be new.
            (None, None) => {
                self.read = Some((transcript.to_path_buf(), 0));
                return false;
            }
            (Some(_), None) => return false,
            // Written before it was seen: history, read only when nothing
            // has said the model.
            (None, Some(len)) => {
                self.read = Some((transcript.to_path_buf(), len));
                if self.model().is_some() {
                    return false;
                }
                read_tail(transcript, len)
            }
            (Some(at), Some(len)) if len == at => return false,
            // Written again, shorter: nothing in it is news.
            (Some(at), Some(len)) if len < at => {
                self.read = Some((transcript.to_path_buf(), len));
                return false;
            }
            (Some(at), Some(_)) => {
                let Some((seen, next)) = read_from(transcript, at) else {
                    return false;
                };
                self.read = Some((transcript.to_path_buf(), next));
                seen
            }
        };
        match seen {
            Some(model) => self.heard(&model),
            None => false,
        }
    }
}

/// The `--model` an agent's command gives it: `--model opus`,
/// `--model=opus`, or Codex's `-m`. Only an agent's: a program's `--model`
/// is its own business.
pub fn from_command(command: &[String]) -> Option<String> {
    let Some(Front::Agent { program, .. }) = front::of_command(command) else {
        return None;
    };
    let mut words = command.iter().skip(1);
    while let Some(word) = words.next() {
        if word == "--" {
            break;
        }
        if word == "--model" || (word == "-m" && program == "codex") {
            return words.next().cloned().filter(|model| !model.is_empty());
        }
        if let Some(model) = word.strip_prefix("--model=") {
            return Some(model.to_string()).filter(|model| !model.is_empty());
        }
    }
    None
}

/// The newest switch among the whole lines written from byte `from` on,
/// at most [`MOST_READ`] of them, and where the next look starts: past the
/// last whole line, so that a line still being written is read whole next
/// time. A line longer than all that is passed over.
fn read_from(path: &Path, from: u64) -> Option<(Option<String>, u64)> {
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.take(MOST_READ).read_to_end(&mut bytes).ok()?;
    let whole = match bytes.iter().rposition(|&b| b == b'\n') {
        Some(at) => at + 1,
        None if bytes.len() as u64 == MOST_READ => return Some((None, from + MOST_READ)),
        None => 0,
    };
    Some((newest(&bytes[..whole]), from + whole as u64))
}

/// The newest switch in the last [`TAIL`] of a transcript `len` long.
fn read_tail(path: &Path, len: u64) -> Option<String> {
    let start = len.saturating_sub(TAIL);
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(len - start).read_to_end(&mut bytes).ok()?;
    // Started in the middle of a line, the part of it read isn't one.
    let first = match start {
        0 => 0,
        _ => bytes
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |at| at + 1),
    };
    newest(&bytes[first..])
}

/// The model the newest line in `bytes` that says one switches to.
fn newest(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .rev()
        .find_map(switched_to)
}

/// The model one line of a transcript switches to, if it says one.
fn switched_to(line: &str) -> Option<String> {
    // Nearly every line says none: they go without being read as JSON.
    let says = ["Set model to ", r#""type":"model""#, r#""turn_context""#];
    if !says.iter().any(|said| line.contains(said)) {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    // A subagent's lines are about its own model.
    if value["isSidechain"] == true {
        return None;
    }
    let model = match value["type"].as_str()? {
        "user" => {
            let said = value["message"]["content"].as_str()?;
            let named = said
                .strip_prefix("<local-command-stdout>")?
                .strip_prefix("Set model to ")?;
            display_name(named)
        }
        "attachment" if value["attachment"]["type"] == "model" => {
            let identity = &value["attachment"]["identity"];
            let id = identity["modelId"].as_str();
            id.or(identity["marketingName"].as_str())?
                .trim()
                .to_string()
        }
        "turn_context" => value["payload"]["model"].as_str()?.trim().to_string(),
        _ => return None,
    };
    (!model.is_empty()).then_some(model)
}

/// `/model`'s name for the model, out of the rest of what it said:
/// `` `Opus 5 (1M context)` and saved as your default… `` is
/// `Opus 5 (1M context)`. Backticks and the codes that make it bold go.
fn display_name(said: &str) -> String {
    let name = said.split(" and saved").next().unwrap_or(said);
    let name = name.split("</local-command-stdout>").next().unwrap_or(name);
    let mut out = String::new();
    let mut chars = name.chars();
    while let Some(c) = chars.next() {
        match c {
            // An escape sequence, through its final letter.
            '\u{1b}' => {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            '`' => {}
            c => out.push(c),
        }
    }
    out.trim().to_string()
}

/// A model's name as a row in the sidebar has room for: Claude's ids and
/// names read the same, `claude-opus-5-5` and `Opus 5.5` both `opus 5.5`,
/// with `[1m]` for the 1M context window; others as they're given, in
/// lowercase, like `gpt-5-codex`.
pub fn short(model: &str) -> String {
    let lower = model.trim().to_lowercase();
    // `/model`'s default names the model it is in brackets.
    let lower = match lower.strip_prefix("default (") {
        Some(inner) => inner
            .split([')', '·'])
            .next()
            .unwrap_or(inner)
            .trim()
            .to_string(),
        None => lower,
    };
    let lower = lower.replace(" (1m context)", "[1m]");
    let Some(id) = lower.strip_prefix("claude-") else {
        return lower;
    };
    let (id, window) = match id.find('[') {
        Some(at) => id.split_at(at),
        None => (id, ""),
    };
    let mut parts: Vec<&str> = id.split('-').filter(|part| !part.is_empty()).collect();
    // A dated id's date says nothing a row needs.
    if parts
        .last()
        .is_some_and(|last| last.len() == 8 && last.chars().all(|c| c.is_ascii_digit()))
    {
        parts.pop();
    }
    // A version's numbers go together, dotted: `opus 5.5`, `3.5 haiku`.
    let mut words: Vec<String> = Vec::new();
    let mut was_number = false;
    for part in parts {
        let number = part.chars().all(|c| c.is_ascii_digit());
        match words.last_mut() {
            Some(last) if number && was_number => {
                last.push('.');
                last.push_str(part);
            }
            _ => words.push(part.to_string()),
        }
        was_number = number;
    }
    format!("{}{window}", words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn words(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn an_agent_s_command_gives_its_model() {
        assert_eq!(
            from_command(&words(&["claude", "--model", "opus"])),
            Some("opus".into())
        );
        assert_eq!(
            from_command(&words(&["claude", "--model=sonnet"])),
            Some("sonnet".into())
        );
        assert_eq!(
            from_command(&words(&["codex", "-m", "gpt-5-codex"])),
            Some("gpt-5-codex".into())
        );
        // `-m` is Codex's alone, and a program's `--model` its own.
        assert_eq!(from_command(&words(&["aider", "-m", "fix it"])), None);
        assert_eq!(from_command(&words(&["python", "--model", "x"])), None);
        assert_eq!(from_command(&words(&["claude", "--", "--model"])), None);
    }

    #[test]
    fn a_switch_is_read_from_claude_code_s_and_codex_s_lines() {
        let set = r#"{"type":"user","message":{"content":"<local-command-stdout>Set model to `Opus 5 (1M context)` and saved as your default for new sessions</local-command-stdout>"}}"#;
        assert_eq!(switched_to(set), Some("Opus 5 (1M context)".into()));
        let bold = "{\"type\":\"user\",\"message\":{\"content\":\"<local-command-stdout>Set model to \\u001b[1mSonnet 5\\u001b[22m</local-command-stdout>\"}}";
        assert_eq!(switched_to(bold), Some("Sonnet 5".into()));
        let attachment = r#"{"type":"attachment","attachment":{"type":"model","identity":{"modelId":"claude-opus-5-5","marketingName":"Opus 5.5"}}}"#;
        assert_eq!(switched_to(attachment), Some("claude-opus-5-5".into()));
        let codex = r#"{"type":"turn_context","payload":{"cwd":"/app","model":"gpt-5-codex"}}"#;
        assert_eq!(switched_to(codex), Some("gpt-5-codex".into()));
        // A subagent's, and a tool's output that only quotes one, say none.
        let sidechain = attachment.replace("{\"type\"", "{\"isSidechain\":true,\"type\"");
        assert_eq!(switched_to(&sidechain), None);
        let quoted = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"Set model to x"}]}}"#;
        assert_eq!(switched_to(quoted), None);
    }

    #[test]
    fn models_are_shown_short() {
        assert_eq!(short("claude-opus-5-5"), "opus 5.5");
        assert_eq!(short("claude-opus-5-5[1m]"), "opus 5.5[1m]");
        assert_eq!(short("claude-sonnet-4-5-20250929"), "sonnet 4.5");
        assert_eq!(short("claude-3-5-haiku-20241022"), "3.5 haiku");
        assert_eq!(short("Opus 5.5"), "opus 5.5");
        assert_eq!(short("Opus 5 (1M context)"), "opus 5[1m]");
        assert_eq!(short("Default (Opus 5.5 · most capable)"), "opus 5.5");
        assert_eq!(short("gpt-5-codex"), "gpt-5-codex");
    }

    fn append(path: &Path, line: &str) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{line}").unwrap();
    }

    fn attachment(id: &str) -> String {
        format!(
            r#"{{"type":"attachment","attachment":{{"type":"model","identity":{{"modelId":"{id}"}}}}}}"#
        )
    }

    #[test]
    fn a_transcript_is_followed_as_it_s_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversation.jsonl");
        let mut watch = Watch::new(&words(&["claude", "--model", "opus"]));
        assert_eq!(watch.model(), Some("opus"));
        // Not written yet: everything in it will be new.
        assert!(!watch.look(&path));
        append(&path, r#"{"type":"user","message":{"content":"hi"}}"#);
        append(&path, &attachment("claude-opus-5-5"));
        assert!(watch.look(&path));
        assert_eq!(watch.model(), Some("claude-opus-5-5"));
        // Nothing new, nothing read.
        assert!(!watch.look(&path));
        // A line half written waits to be whole.
        let switch = attachment("claude-sonnet-5");
        let (start, end) = switch.split_at(20);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(file, "{start}").unwrap();
        assert!(!watch.look(&path));
        writeln!(file, "{end}").unwrap();
        assert!(watch.look(&path));
        assert_eq!(watch.model(), Some("claude-sonnet-5"));
    }

    #[test]
    fn a_transcript_s_history_counts_only_when_nothing_said_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversation.jsonl");
        append(&path, &attachment("claude-haiku-5"));
        append(&path, r#"{"type":"assistant","message":{"content":[]}}"#);
        let mut started_on = Watch::default();
        assert!(started_on.heard("claude-opus-5-5"));
        assert!(!started_on.look(&path));
        assert_eq!(started_on.model(), Some("claude-opus-5-5"));
        let mut unknown = Watch::default();
        assert!(unknown.look(&path));
        assert_eq!(unknown.model(), Some("claude-haiku-5"));
        // Once it has left, what was said goes, but not the command's.
        let mut flagged = Watch::new(&words(&["claude", "--model", "opus"]));
        flagged.heard("claude-sonnet-5");
        flagged.forget();
        assert_eq!(flagged.model(), Some("opus"));
    }
}
