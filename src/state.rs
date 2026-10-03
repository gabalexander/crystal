//! What the daemon writes down so that, after a restart (a crash, or the
//! machine rebooting), it can start the sessions that were running again.
//!
//! A session's environment is never written down: it can hold secrets. A
//! session started again gets the environment of whoever started the
//! daemon again.

use crate::protocol::{Conversation, TaskInfo, TaskSpec};
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
    /// For a task, what it was asked to do. A task comes back at rest,
    /// ready to carry its conversation on, rather than running its prompt
    /// again.
    #[serde(default)]
    pub task: Option<TaskSpec>,
    /// What the session's agent was asked to do, and how that went so far.
    #[serde(default)]
    pub goal: Option<TaskInfo>,
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

/// Where the daemon at `socket` keeps what it knows of the project whose
/// main worktree is `project`: its backlog, and the tasks done in it. A
/// directory per project, beside where the sessions are written down.
pub fn project_dir(socket: &Path, project: &Path) -> PathBuf {
    projects_dir(socket).join(project_slug(project))
}

/// The directories of every project the daemon at `socket` keeps anything
/// for.
pub fn project_dirs(socket: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(projects_dir(socket)) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect()
}

/// Where the daemon at `socket` keeps the logs of the plugins it runs.
pub fn plugins_dir(socket: &Path) -> PathBuf {
    if socket == socket::default_path() {
        state_dir().join("plugins")
    } else {
        socket.with_extension("plugins")
    }
}

fn projects_dir(socket: &Path) -> PathBuf {
    if socket == socket::default_path() {
        state_dir().join("projects")
    } else {
        socket.with_extension("projects")
    }
}

/// A directory name for a project: its own name, so a person can find it,
/// and a hash of its whole path, since two projects can share a name.
fn project_slug(project: &Path) -> String {
    let name = project
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".to_string());
    let name: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!(
        "{name}-{:016x}",
        fnv1a(project.to_string_lossy().as_bytes())
    )
}

/// The FNV-1a hash of `bytes`: small, and the same from one build of crystal
/// to the next, which the standard library's hash doesn't promise.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
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
            task: None,
            goal: None,
        }
    }

    #[test]
    fn what_is_saved_loads_back_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        let mut task = saved("b");
        task.task = Some(TaskSpec {
            prompt: "fix the tests".into(),
            args: vec!["--permission-mode".into(), "acceptEdits".into()],
        });
        let sessions = vec![saved("a"), task];
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

    #[test]
    fn each_project_has_a_directory_of_its_own_named_after_it() {
        let socket = Path::new("/tmp/test/crystal.sock");
        let app = project_dir(socket, Path::new("/code/app"));
        let other_app = project_dir(socket, Path::new("/elsewhere/app"));
        assert!(app.starts_with("/tmp/test/crystal.projects"));
        let name = app.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("app-"), "{name}");
        assert_ne!(app, other_app);
        assert_eq!(app, project_dir(socket, Path::new("/code/app")));
    }
}
