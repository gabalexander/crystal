//! Where the daemon keeps its state: the database (`db.rs`), and the files
//! kept before it, which it brings in. What it writes down of a running
//! session is here too, so that after a restart (a crash, or the machine
//! rebooting), it can start the sessions that were running again.
//!
//! A session's environment is never written down: it can hold secrets. A
//! session started again gets the environment of whoever started the
//! daemon again.

use crate::protocol::{Conversation, TaskInfo, TaskSpec};
use crate::socket;
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

/// The database of the daemon at `socket`. A server's socket lives in
/// /tmp, which a reboot empties, so its database is in its directory in the
/// user's state directory instead ([`server_dir`]), whoever starts the
/// daemon. A socket given by its path keeps its own beside it.
pub fn db_path(socket: &Path) -> PathBuf {
    kept(socket, "crystal.db", "db")
}

/// Where the sessions of the daemon at `socket` were written down before
/// the database. The rest of its state, like memory, is kept beside it.
pub fn path(socket: &Path) -> PathBuf {
    kept(socket, "sessions.json", "sessions.json")
}

/// Where the flow runs of the daemon at `socket` were written down before
/// the database: beside its sessions.
pub fn flows_path(socket: &Path) -> PathBuf {
    kept(socket, "flows.json", "flows.json")
}

/// Where the daemon at `socket` kept what it knew of the project whose main
/// worktree is `project` before the database: its backlog, and the tasks
/// done in it. A directory per project, beside where the sessions were
/// written down.
pub fn project_dir(socket: &Path, project: &Path) -> PathBuf {
    projects_dir(socket).join(project_slug(project))
}

/// The directories of every project the daemon at `socket` kept anything
/// for before the database.
pub fn project_dirs(socket: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(projects_dir(socket)) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect()
}

/// Where the daemon at `socket` keeps the files kept with the task numbered
/// `task`: a directory of its own, beside the database.
pub fn task_dir(socket: &Path, task: u64) -> PathBuf {
    kept(socket, "tasks", "tasks").join(format!("t{task}"))
}

/// Where the daemon at `socket` keeps the logs of the plugins it runs.
pub fn plugins_dir(socket: &Path) -> PathBuf {
    kept(socket, "plugins", "plugins")
}

fn projects_dir(socket: &Path) -> PathBuf {
    kept(socket, "projects", "projects")
}

/// Where the daemon at `socket` keeps `file`: in its server's directory
/// ([`socket::server_of`]), or for a socket given by its path, beside it,
/// named after it with `extension`.
fn kept(socket: &Path, file: &str, extension: &str) -> PathBuf {
    match socket::server_of(socket) {
        Some(server) => server_dir(&server).join(file),
        None => socket.with_extension(extension),
    }
}

/// Where the server called `name` keeps its state: the user's state
/// directory for the default server, where it always has, and a directory
/// of its own under [`servers_dir`] for any other.
pub fn server_dir(name: &str) -> PathBuf {
    if name == socket::DEFAULT {
        state_dir()
    } else {
        servers_dir().join(name)
    }
}

/// Where every server but the default keeps its state, a directory each.
pub fn servers_dir() -> PathBuf {
    state_dir().join("servers")
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

    #[test]
    fn a_socket_of_its_own_keeps_its_state_beside_it() {
        assert_eq!(
            db_path(Path::new("/tmp/test/crystal.sock")),
            Path::new("/tmp/test/crystal.db")
        );
        assert!(db_path(&socket::default_path()).ends_with("crystal/crystal.db"));
        assert_eq!(
            path(Path::new("/tmp/test/crystal.sock")),
            Path::new("/tmp/test/crystal.sessions.json")
        );
        assert!(path(&socket::default_path()).ends_with("crystal/sessions.json"));
        assert_eq!(
            flows_path(Path::new("/tmp/test/crystal.sock")),
            Path::new("/tmp/test/crystal.flows.json")
        );
    }

    #[test]
    fn a_server_keeps_its_state_in_a_directory_of_its_own() {
        let work = socket::of_server("work").unwrap();
        assert_eq!(db_path(&work), servers_dir().join("work/crystal.db"));
        assert_eq!(plugins_dir(&work), servers_dir().join("work/plugins"));
        assert_eq!(server_dir("work"), servers_dir().join("work"));
        assert_eq!(server_dir(socket::DEFAULT), state_dir());
        assert_eq!(
            db_path(&socket::default_path()),
            state_dir().join("crystal.db")
        );
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
