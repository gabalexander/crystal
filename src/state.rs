//! What the daemon writes down so that, after a restart (a crash, or the
//! machine rebooting), it can start the sessions that were running again.
//!
//! A session's environment is never written down: it can hold secrets. A
//! session started again gets the environment of whoever started the
//! daemon again.

use crate::protocol::Conversation;
use crate::socket;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// A running session, as much of it as it takes to start it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSession {
    pub name: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    /// The agent's conversation, to pick up where it left off.
    pub conversation: Option<Conversation>,
}

/// Where the sessions of the daemon at `socket` are written down. The
/// default socket lives in /tmp, which a reboot empties, so its sessions go
/// in the user's state directory instead. Any other socket keeps them
/// beside it.
pub fn path(socket: &Path) -> PathBuf {
    if socket == socket::default_path() {
        state_dir().join("sessions.json")
    } else {
        socket.with_extension("sessions.json")
    }
}

/// The sessions written down at `path`. A file that's missing or can't be
/// read means there's nothing to start again.
pub fn load(path: &Path) -> Vec<SavedSession> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save(path: &Path, sessions: &[SavedSession]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // Written beside it first, then moved into place in one step, so a
    // crash halfway through never leaves half a file.
    let unfinished = path.with_extension("json.unfinished");
    fs::write(&unfinished, serde_json::to_string_pretty(sessions)?)?;
    fs::rename(&unfinished, path)?;
    Ok(())
}

pub fn forget(path: &Path) {
    let _ = fs::remove_file(path);
}

/// `$XDG_STATE_HOME/crystal`, or `~/.local/state/crystal`.
fn state_dir() -> PathBuf {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(".local/state")
        }
    };
    base.join("crystal")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(name: &str) -> SavedSession {
        SavedSession {
            name: name.into(),
            command: vec!["claude".into()],
            cwd: PathBuf::from("/code/app"),
            conversation: Some(Conversation {
                id: "abc".into(),
                transcript: None,
            }),
        }
    }

    #[test]
    fn what_is_saved_loads_back_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        let sessions = vec![saved("a"), saved("b")];
        save(&path, &sessions).unwrap();
        assert_eq!(load(&path), sessions);
    }

    #[test]
    fn nothing_saved_loads_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("missing.json")).is_empty());

        let broken = dir.path().join("broken.json");
        fs::write(&broken, "not json").unwrap();
        assert!(load(&broken).is_empty());
    }

    #[test]
    fn a_socket_of_its_own_keeps_its_sessions_beside_it() {
        assert_eq!(
            path(Path::new("/tmp/test/crystal.sock")),
            Path::new("/tmp/test/crystal.sessions.json")
        );
        assert!(path(&socket::default_path()).ends_with("crystal/sessions.json"));
    }
}
