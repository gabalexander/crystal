//! `crystal server`: the servers, each a daemon with its own sessions and
//! state, listed with whether each is running and how many sessions it
//! has; and stopping one, or deleting a stopped one's state.

use crate::client;
use crate::db;
use crate::output::{errln, outln};
use crate::protocol::{Request, Response};
use crate::socket::{self, DEFAULT};
use crate::state;
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, ErrorKind};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// A server, as `crystal server` lists it.
#[derive(Debug, Serialize)]
struct Server {
    name: String,
    running: bool,
    /// While it runs, the sessions `ls` lists. While it's stopped, those it
    /// starts again when it next starts: none after `stop`, and those that
    /// were running after a crash or a reboot. `None` when it can't say.
    sessions: Option<usize>,
    socket: PathBuf,
    /// The directory it keeps its state in.
    state: PathBuf,
    /// Why a server that's running couldn't say how many sessions it has:
    /// its daemon is another crystal's, say.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Prints every server, the default first.
pub fn list(json: bool) -> Result<()> {
    let servers = names()
        .iter()
        .map(|name| look_at(name))
        .collect::<Result<Vec<Server>>>()?;
    if json {
        outln!("{}", serde_json::to_string_pretty(&servers)?)?;
        return Ok(());
    }
    let rows: Vec<[String; 3]> = servers
        .iter()
        .map(|server| {
            let state = if server.running { "running" } else { "stopped" };
            let sessions = server
                .sessions
                .map_or("-".into(), |count| count.to_string());
            [server.name.clone(), state.to_string(), sessions]
        })
        .collect();
    crate::print_table(["NAME", "STATE", "SESSIONS"], &rows)?;
    for server in &servers {
        if let Some(error) = &server.error {
            errln!("crystal: {}: {error}", server.name);
        }
    }
    Ok(())
}

/// Every server's name: the default's, then those with a directory of
/// state or a socket.
pub fn names() -> Vec<String> {
    let name = |path: PathBuf, suffix: &str| -> Option<String> {
        let file = path.file_name()?.to_str()?;
        Some(file.strip_suffix(suffix)?.to_string())
    };
    let kept = entries(state::servers_dir())
        .filter(|path| path.is_dir())
        .filter_map(|path| name(path, ""));
    let sockets = entries(socket::dir()).filter_map(|path| name(path, ".sock"));
    in_order(kept.chain(sockets))
}

/// The default server, then the other names `found`, in order, once each,
/// leaving out what can't name a server.
fn in_order(found: impl Iterator<Item = String>) -> Vec<String> {
    let others: BTreeSet<String> = found
        .filter(|name| name != DEFAULT && socket::check_name(name).is_ok())
        .collect();
    std::iter::once(DEFAULT.to_string()).chain(others).collect()
}

/// What's in the directory `dir`, or nothing when it isn't there.
fn entries(dir: PathBuf) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
}

/// How the server called `name` stands: its daemon asked, while it runs,
/// or else its database read.
fn look_at(name: &str) -> Result<Server> {
    let socket = socket::of_server(name)?;
    let (running, sessions, error) = match client::ask(&socket, &Request::List, false) {
        Ok(None) => (false, db::saved_session_count(&socket).ok(), None),
        Ok(Some(Response::Sessions { sessions })) => (true, Some(sessions.len()), None),
        Ok(Some(_)) => (true, None, None),
        Err(err) => (true, None, Some(format!("{err:#}"))),
    };
    Ok(Server {
        name: name.to_string(),
        running,
        sessions,
        socket,
        state: state::server_dir(name),
        error,
    })
}

/// Stops the server called `name` and its sessions, as `kill-server` does.
pub fn stop(name: &str) -> Result<()> {
    let socket = socket::of_server(name)?;
    ensure!(
        client::stop_daemon(&socket, false)?,
        "the server {name} isn't running"
    );
    Ok(())
}

/// Deletes what the server called `name` keeps: its state, its log, and a
/// socket a crash left behind. Refuses while it runs, since its daemon
/// would go on with state that's gone, and for the default server.
pub fn delete(name: &str) -> Result<()> {
    let socket = socket::of_server(name)?;
    ensure!(name != DEFAULT, "the default server can't be deleted");
    ensure!(
        UnixStream::connect(&socket).is_err(),
        "the server {name} is running: `crystal server stop {name}` stops it first"
    );
    let dir = state::server_dir(name);
    let log = socket::log_path(&socket);
    let mut found = removed(fs::remove_dir_all(&dir), &dir)?;
    found |= removed(fs::remove_file(&log), &log)?;
    found |= removed(fs::remove_file(&socket), &socket)?;
    ensure!(
        found,
        "there's no server called {name}; `crystal server` lists them"
    );
    Ok(())
}

/// Whether removing `path` removed something: a path that wasn't there is
/// no error.
fn removed(result: io::Result<()>, path: &Path) -> Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("couldn't remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_server_comes_first_then_the_others_by_name_once_each() {
        let found = [
            "work",
            "agents",
            "work",
            "default",
            "default.log",
            "..",
            "side",
        ];
        let names = in_order(found.iter().map(|name| name.to_string()));
        assert_eq!(names, ["default", "agents", "side", "work"]);
    }

    #[test]
    fn the_default_server_is_listed_with_nothing_else_there() {
        assert_eq!(in_order(std::iter::empty()), ["default"]);
    }
}
