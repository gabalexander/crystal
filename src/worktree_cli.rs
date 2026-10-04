//! `crystal worktree`: a project's worktrees and the sessions in each,
//! making one, labelling one, finding one to start a session in, and moving
//! a session into one, its agent carried on there. Removing one is the
//! daemon's: see [`client::remove_worktree`].

use crate::client::{self, Moved};
use crate::env;
use crate::git::{self, Checkout};
use crate::names;
use crate::protocol::{Request, Response, SessionInfo};
use crate::shell;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// A worktree, as `crystal worktree list --json` prints it.
#[derive(Serialize)]
struct Listed<'a> {
    path: &'a Path,
    branch: Option<&'a str>,
    main: bool,
    label: Option<String>,
    /// The sessions in it, by name.
    sessions: Vec<&'a str>,
}

/// Prints the worktrees of the project `dir` is in, the main one first,
/// with each one's label and the sessions in it.
pub fn list(socket: &Path, dir: &Path, json: bool) -> Result<()> {
    let checkout = Checkout::find(dir)
        .with_context(|| format!("{} isn't in a git repository", dir.display()))?;
    let project = checkout.project_path();
    let main = Checkout::find(project)
        .with_context(|| format!("{} isn't in a git repository", project.display()))?
        .worktree();
    let linked = git::linked_worktrees(project)?;
    let mut worktrees = vec![main];
    worktrees.extend(linked.worktrees);
    let sessions = sessions(socket)?;
    let in_it = |path: &Path| -> Vec<&str> {
        sessions
            .iter()
            .filter(|session| session.worktree.as_ref().is_some_and(|w| w.path == path))
            .map(|session| session.name.as_str())
            .collect()
    };
    let listed: Vec<Listed> = worktrees
        .iter()
        .map(|worktree| Listed {
            path: &worktree.path,
            branch: worktree.branch.as_deref(),
            main: worktree.main,
            label: linked.labels.get(&worktree.path).cloned(),
            sessions: in_it(&worktree.path),
        })
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&listed)?);
        return Ok(());
    }
    let rows: Vec<[String; 4]> = listed
        .iter()
        .map(|listed| {
            let branch = listed.branch.unwrap_or("(detached)");
            let mark = if listed.main { "⌂" } else { "⎇" };
            let label = listed.label.as_deref().unwrap_or("-");
            [
                format!("{mark} {branch}"),
                crate::printable::line(label).into_owned(),
                listed.sessions.len().to_string(),
                shell::home_relative(listed.path),
            ]
        })
        .collect();
    crate::print_table(["BRANCH", "LABEL", "SESSIONS", "DIRECTORY"], &rows);
    Ok(())
}

/// What `crystal worktree create` was asked for.
pub struct NewWorktree {
    /// A branch that exists is checked out; a new one is made. `None` makes
    /// a new branch with a made-up name.
    pub branch: Option<String>,
    /// Where a new branch starts: `--base`.
    pub base: Option<String>,
    /// Where the worktree goes: `--path`.
    pub path: Option<PathBuf>,
    pub label: Option<String>,
}

/// Makes a worktree in the project `dir` is in, as `new` says, and prints
/// its directory.
pub fn create(socket: &Path, dir: &Path, new: NewWorktree) -> Result<()> {
    let path = make(socket, dir, new)?;
    println!("{}", path.display());
    Ok(())
}

/// Makes a worktree in the project `dir` is in, as `new` says, and gives
/// back its directory.
fn make(socket: &Path, dir: &Path, new: NewWorktree) -> Result<PathBuf> {
    let NewWorktree {
        branch,
        base,
        path,
        label,
    } = new;
    let path = path.map(std::path::absolute).transpose()?;
    let made = match branch {
        Some(branch) => client::add_worktree(socket, dir, &branch, base.as_deref(), path)?,
        None => client::add_new_worktree(socket, dir, &names::random(), base.as_deref(), path)?,
    };
    // As git resolves it, so it's the path the sessions there run in.
    let made = std::fs::canonicalize(&made).unwrap_or(made);
    if let Some(label) = label {
        git::set_label(&made, &label)?;
    }
    Ok(made)
}

/// The worktree `target` names, a directory or the branch it has checked
/// out, in the project `dir` is in, labelled `label` when that's given:
/// where `crystal worktree open` starts its session.
pub fn find(dir: &Path, target: &str, label: Option<&str>) -> Result<PathBuf> {
    let path = git::find_worktree(dir, target)?;
    if let Some(label) = label {
        git::set_label(&path, label)?;
    }
    Ok(path)
}

/// Sets the label of the worktree `target` names, a directory or the
/// branch it has checked out, in the project `dir` is in, or with an empty
/// one takes it off.
pub fn label(dir: &Path, target: &str, label: &str) -> Result<()> {
    let path = git::find_worktree(dir, target)?;
    git::set_label(&path, label)
}

/// What `crystal worktree move` was asked for.
pub struct MoveTo {
    /// The session to move: the one this runs in, when it's `None`.
    pub session: Option<String>,
    /// The worktree on this branch, made if the project hasn't one. `None`
    /// makes one on a new branch with a made-up name.
    pub branch: Option<String>,
    /// Where a new branch starts: `--base`.
    pub base: Option<String>,
    /// Where a new worktree goes: `--path`.
    pub path: Option<PathBuf>,
}

/// Moves a session into a worktree of its project, as `to` says, and says
/// how that goes. An agent asking to move its own session is told to end
/// its turn, which is when it moves.
pub fn move_session(socket: &Path, to: MoveTo) -> Result<()> {
    let MoveTo {
        session,
        branch,
        base,
        path,
    } = to;
    let sessions = sessions(socket)?;
    let own = env::own_session_id(socket);
    let found = match (&session, &own) {
        (Some(name), _) => sessions.iter().find(|found| found.name == *name),
        (None, Some(id)) => sessions.iter().find(|found| found.id == *id),
        (None, None) => {
            bail!(
                "this isn't running in a crystal session: say which one to move with `-n <session>`"
            )
        }
    };
    let Some(session) = found else {
        match session {
            Some(name) => bail!("no session named {name}"),
            None => bail!("the session this runs in has gone"),
        }
    };
    let Some(worktree) = &session.worktree else {
        bail!("{} doesn't run in a git repository", session.name);
    };
    let project = worktree.project_path.clone();
    let existing = match &branch {
        Some(branch) => git::worktree_on(&project, branch)?,
        None => None,
    };
    let target = match existing {
        Some(path) => path,
        None => make(
            socket,
            &project,
            NewWorktree {
                branch,
                base,
                path,
                label: None,
            },
        )?,
    };
    let at = shell::home_relative(&target);
    let itself = own.as_deref() == Some(session.id.as_str());
    match client::move_session(socket, &session.name, &target)? {
        Moved::AlreadyThere => println!("{} is in {at} already", session.name),
        Moved::Later if itself => println!(
            "End your turn now, saying in a line that this session is moving: crystal moves it \
             into {at} once your turn ends, and picks your conversation up there."
        ),
        Moved::Later => println!(
            "{} moves into {at} once its agent's turn ends",
            session.name
        ),
        Moved::Now => println!("{} moves into {at} now", session.name),
    }
    Ok(())
}

/// The sessions the daemon runs, or none when it isn't running.
fn sessions(socket: &Path) -> Result<Vec<SessionInfo>> {
    match client::ask(socket, &Request::List, false)? {
        Some(Response::Sessions { sessions }) => Ok(sessions),
        _ => Ok(Vec::new()),
    }
}
