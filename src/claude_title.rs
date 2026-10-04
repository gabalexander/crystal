//! Keeping a session's name and the name Claude Code gives its conversation
//! in step. Claude Code names a conversation with `/rename` (or `--name`),
//! shows the name in its prompt box and `/resume`, and keeps it beside its
//! transcript, in `<the transcript's directory>/<the conversation's
//! id>/custom-title.json` as `{"customTitle": "…"}`, the file taken away
//! when the name is cleared. No hook says it changed. So:
//!
//! - **Claude Code to crystal**: with each check of a session whose agent's
//!   hooks named its transcript, the daemon looks at that file, a `stat`
//!   unless it has changed, and a name Claude Code didn't have before names
//!   the session, unless the user or a script named it. The first look at a
//!   conversation only takes in the name it has: what's followed is a
//!   rename, so a conversation picked up again, after a restart or with
//!   `--resume`, renames nothing.
//! - **crystal to Claude Code**: the hook that reports a prompt the user
//!   sent can answer with `hookSpecificOutput.sessionTitle`, which names the
//!   conversation as `/rename` would; it's the only way in. A rename in
//!   crystal goes with the next prompt, once. The name a session started
//!   with doesn't, nor one crystal made up: Claude Code has its own.
//!
//! Checked against Claude Code 2.1.289. Adapted from docket's
//! `session_title`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The file Claude Code keeps a conversation's name in.
const SIDECAR: &str = "custom-title.json";

/// The most of a name that's taken: Claude Code's are a few words.
const LONGEST: usize = 200;

/// What's known of the name Claude Code gives a session's conversation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watch {
    /// The name Claude Code was last seen to have for the conversation, or
    /// was last given.
    #[serde(default)]
    seen: Option<String>,
    /// The file last looked at, and how it was then: when it last changed
    /// and how long it was, or `None` while it wasn't there.
    #[serde(default)]
    looked: Option<(PathBuf, Option<(SystemTime, u64)>)>,
    /// The name to give Claude Code with the next prompt the user sends.
    #[serde(default)]
    giving: Option<String>,
}

impl Watch {
    /// Looks at the name Claude Code keeps for the conversation `id`, whose
    /// transcript is `transcript`: the name, when it's one Claude Code was
    /// given since the last look.
    pub fn look(&mut self, transcript: &Path, id: &str) -> Option<String> {
        let file = sidecar(transcript, id)?;
        let meta = std::fs::metadata(&file).ok();
        let state = meta.and_then(|meta| Some((meta.modified().ok()?, meta.len())));
        let first = match &self.looked {
            Some((looked, before)) if *looked == file => {
                if *before == state {
                    return None;
                }
                false
            }
            // Another conversation, or the first: what it's called now is
            // where it starts.
            _ => {
                self.seen = None;
                true
            }
        };
        self.looked = Some((file.clone(), state));
        let Some(name) = state.and_then(|_| read(&file)) else {
            // Cleared: a name given again later is a rename again.
            self.seen = None;
            return None;
        };
        if self.seen.as_deref() == Some(name.as_str()) {
            return None;
        }
        self.seen = Some(name.clone());
        (!first).then_some(name)
    }

    /// The user renamed the session `name` in crystal: Claude Code is given
    /// it with the next prompt, unless that's what it has already, written
    /// as a session's name is.
    pub fn give(&mut self, name: &str) {
        let has = self.seen.as_deref().and_then(crate::names::from_title);
        if has.as_deref() == Some(name) {
            self.giving = None;
            return;
        }
        self.giving = Some(name.to_string());
    }

    /// The name to give Claude Code now, as the user sends a prompt: once,
    /// after which it's what Claude Code has.
    pub fn take_giving(&mut self) -> Option<String> {
        let name = self.giving.take()?;
        self.seen = Some(name.clone());
        Some(name)
    }
}

/// Where Claude Code keeps the name of the conversation `id`, from its
/// transcript's path.
fn sidecar(transcript: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains(['/', '\\']) || id == ".." {
        return None;
    }
    Some(transcript.parent()?.join(id).join(SIDECAR))
}

/// The name in the file at `path`, if it has one.
fn read(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let name = crate::printable::line(value["customTitle"].as_str()?);
    let name: String = name.trim().chars().take(LONGEST).collect();
    (!name.is_empty()).then_some(name)
}

/// The answer to the hook reporting a prompt the user sent that gives the
/// conversation `name`.
pub fn hook_answer(name: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "sessionTitle": name,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Conversation {
        _dir: tempfile::TempDir,
        transcript: PathBuf,
        file: PathBuf,
    }

    fn conversation() -> Conversation {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("abc.jsonl");
        let file = dir.path().join("abc").join(SIDECAR);
        Conversation {
            _dir: dir,
            transcript,
            file,
        }
    }

    fn rename(conversation: &Conversation, name: &str) {
        std::fs::create_dir_all(conversation.file.parent().unwrap()).unwrap();
        let text = serde_json::json!({ "customTitle": name }).to_string();
        std::fs::write(&conversation.file, text).unwrap();
    }

    #[test]
    fn a_rename_is_seen_once_and_the_first_look_only_takes_in_the_name() {
        let talk = conversation();
        let mut watch = Watch::default();
        assert_eq!(watch.look(&talk.transcript, "abc"), None);
        rename(&talk, "Fix refund rounding");
        assert_eq!(
            watch.look(&talk.transcript, "abc").as_deref(),
            Some("Fix refund rounding")
        );
        assert_eq!(watch.look(&talk.transcript, "abc"), None);

        // Picked up with a name already, the conversation renames nothing
        // until it's renamed.
        let mut resumed = Watch::default();
        assert_eq!(resumed.look(&talk.transcript, "abc"), None);
        rename(&talk, "Ship the refunds");
        assert_eq!(
            resumed.look(&talk.transcript, "abc").as_deref(),
            Some("Ship the refunds")
        );
    }

    #[test]
    fn a_name_cleared_and_given_again_is_a_rename_again() {
        let talk = conversation();
        let mut watch = Watch::default();
        watch.look(&talk.transcript, "abc");
        rename(&talk, "Payments");
        assert!(watch.look(&talk.transcript, "abc").is_some());
        std::fs::remove_file(&talk.file).unwrap();
        assert_eq!(watch.look(&talk.transcript, "abc"), None);
        rename(&talk, "Payments");
        assert_eq!(
            watch.look(&talk.transcript, "abc").as_deref(),
            Some("Payments")
        );
    }

    #[test]
    fn a_name_given_goes_once_unless_claude_has_it_already() {
        let talk = conversation();
        let mut watch = Watch::default();
        watch.look(&talk.transcript, "abc");
        rename(&talk, "Payments refactor");
        watch.look(&talk.transcript, "abc");
        watch.give("payments-refactor");
        assert_eq!(watch.take_giving(), None);
        watch.give("api");
        assert_eq!(watch.take_giving().as_deref(), Some("api"));
        assert_eq!(watch.take_giving(), None);
        // Claude Code writing it back is no rename.
        rename(&talk, "api");
        assert_eq!(watch.look(&talk.transcript, "abc"), None);
    }

    #[test]
    fn the_answer_is_the_hook_output_claude_code_reads() {
        let answer: serde_json::Value = serde_json::from_str(&hook_answer("api")).unwrap();
        assert_eq!(answer["hookSpecificOutput"]["sessionTitle"], "api");
        assert_eq!(
            answer["hookSpecificOutput"]["hookEventName"],
            "UserPromptSubmit"
        );
        assert!(sidecar(Path::new("/t/abc.jsonl"), "../x").is_none());
    }
}
