//! `crystal api snapshot`: everything a client that keeps its own picture
//! of crystal starts from, in one JSON document, as herdr's `api snapshot`
//! gives: the sessions, as `ls --json` has them, the TUI's tabs and panes,
//! the projects, the tasks not yet closed, the flow runs and the archive,
//! with the latest event's `seq`. Following the events after that seq
//! (`crystal events --follow --after <seq>`) keeps the picture up to date
//! with no gap: an event is a reason to ask again, not a change to apply.
//!
//! It's asked for piece by piece, not all at once, so a change can land
//! between two pieces; the events after `seq` cover it.

pub mod schema;

use crate::client;
use crate::db::Db;
use crate::env;
use crate::flow_run::FlowRun;
use crate::layout::{self, Layout, Order};
use crate::protocol::{self, ArchivedSession, Request, Response, SessionInfo, TaskView, Worktree};
use crate::tasks;
use anyhow::{Result, bail};
use serde::Serialize;
use std::path::Path;

/// What `crystal api snapshot` prints.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Snapshot {
    /// The crystal that took it, which is the daemon's: the two must match.
    pub version: String,
    /// The daemon's socket.
    pub socket: String,
    /// The latest event's `seq` as the snapshot began: what happens after
    /// it is news.
    pub seq: u64,
    /// The sessions, as `crystal ls --json` lists them.
    pub sessions: Vec<Listed>,
    /// The TUI's tabs and their panes, as `crystal layout --json` prints
    /// them; `null` when they can't be had.
    pub layout: Option<Layout>,
    /// The projects crystal knows.
    pub projects: Vec<Worktree>,
    /// The tasks that aren't closed, of every project: open, waiting on
    /// the user, or waiting to start. Empty while tasks are off.
    pub tasks: Vec<TaskView>,
    /// Every flow run, the oldest first.
    pub flows: Vec<FlowRun>,
    /// The archived sessions, the latest archived first.
    pub archived: Vec<ArchivedSession>,
}

/// A session as `ls --json` lists it: what the daemon knows, and the word
/// its STATE column shows.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "ListedSession"))]
pub struct Listed {
    #[serde(flatten)]
    pub session: SessionInfo,
    pub status: String,
}

/// Takes the snapshot, from a daemon that must be running already.
pub fn snapshot(socket: &Path) -> Result<Snapshot> {
    // The seq first: an event between it and the rest is news to follow,
    // not lost.
    let seq = Db::open(socket)
        .and_then(|db| db.latest_event())
        .unwrap_or(0);
    let Response::Sessions { sessions } = ask(socket, &Request::List)? else {
        bail!("the daemon didn't list the sessions");
    };
    let sessions = sessions
        .into_iter()
        .map(|session| Listed {
            status: session.status(),
            session,
        })
        .collect();
    let order = Order {
        command: layout::Command::Show,
        caller: env::own_session_id(socket),
    };
    let layout = match ask(socket, &Request::Layout(order)) {
        Ok(Response::Layout(layout)) => Some(layout),
        _ => None,
    };
    let projects = match ask(socket, &Request::Projects)? {
        Response::Projects { projects } => projects,
        _ => bail!("the daemon didn't list the projects"),
    };
    let tasks = match tasks::enabled(&crate::config::Config::load().unwrap_or_default()) {
        true => open_tasks(socket)?,
        false => Vec::new(),
    };
    let flows = match ask(socket, &Request::ListFlows)? {
        Response::Flows { runs } => runs,
        _ => bail!("the daemon didn't list the flow runs"),
    };
    let archived = match ask(socket, &Request::Archived)? {
        Response::Archived { sessions } => sessions,
        _ => bail!("the daemon didn't list the archive"),
    };
    Ok(Snapshot {
        version: protocol::version(),
        socket: socket.display().to_string(),
        seq,
        sessions,
        layout,
        projects,
        tasks,
        flows,
        archived,
    })
}

/// The tasks of every project that aren't closed.
fn open_tasks(socket: &Path) -> Result<Vec<TaskView>> {
    let request = Request::Tasks {
        dir: std::env::current_dir()?,
        all: true,
    };
    let Response::Tasks { tasks } = ask(socket, &request)? else {
        bail!("the daemon didn't list the tasks");
    };
    Ok(tasks
        .into_iter()
        .filter(|task| task.record.outcome.is_none())
        .collect())
}

/// Asks a daemon that must be running: a snapshot never starts one.
fn ask(socket: &Path, request: &Request) -> Result<Response> {
    match client::ask(socket, request, false)? {
        Some(response) => Ok(response),
        None => bail!("no daemon is running on {}", socket.display()),
    }
}
