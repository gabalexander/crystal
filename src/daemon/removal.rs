//! Removing a worktree, which `W` in the TUI and `crystal worktree rm` ask
//! the daemon for: refused while a session runs there; git removes it;
//! everyone's told it's gone, and the sessions that had ended in it leave
//! the list with it, since their directory is gone; then whoever asked is
//! answered. The client going doesn't stop it. A handover doesn't either:
//! git is left running, still this process's child, and the next daemon
//! waits for it to end, has git remove the worktree again if it's still
//! there, and answers the clients that asked.

use super::Daemon;
use crate::events::Event;
use crate::git::{self, Checkout};
use crate::handover::{self, HandedRemoval, Ticket};
use crate::protocol::{self, Response};
use crate::session::Session;
use anyhow::{Result, bail};
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

/// A worktree the daemon is removing.
pub(super) struct Removal {
    path: PathBuf,
    /// Its repository's main worktree, where git is run on it.
    project: PathBuf,
    branch: Option<String>,
    force: bool,
    /// git's process, while it removes the worktree.
    git: Option<u32>,
    /// The connections waiting to hear it's done, each counted in at the
    /// gate until it's answered.
    asking: Vec<(UnixStream, Ticket)>,
}

impl Removal {
    /// The removal as it's handed over, its connections kept open across
    /// the exec.
    pub(super) fn hand_over(&self) -> io::Result<HandedRemoval> {
        let asking = self.asking.iter().map(|(conn, _)| conn.as_fd());
        Ok(HandedRemoval {
            path: self.path.clone(),
            project: self.project.clone(),
            branch: self.branch.clone(),
            force: self.force,
            git: self.git,
            asking: asking
                .map(handover::keep_across_exec)
                .collect::<io::Result<_>>()?,
        })
    }
}

impl Daemon {
    /// Removes the worktree at `path` for `conn`, which `ticket` let in,
    /// and answers it once that's done, or with why not: see
    /// [`protocol::Request::RemoveWorktree`].
    pub(super) fn remove_worktree(
        &self,
        conn: &UnixStream,
        ticket: Ticket,
        path: PathBuf,
        force: bool,
    ) -> Result<()> {
        let refuse = |message: String| protocol::send(conn, &Response::Error { message });
        let Some(checkout) = Checkout::find(&path) else {
            let message = format!("{} isn't in a git repository", path.display());
            return Ok(refuse(message)?);
        };
        if let Err(err) = self.check_nothing_runs_in(&path) {
            return Ok(refuse(format!("{err:#}"))?);
        }
        {
            let mut removals = self.removals.lock().unwrap();
            let asking = (conn.try_clone()?, ticket);
            if let Some(removal) = removals.iter_mut().find(|removal| removal.path == path) {
                removal.asking.push(asking);
                return Ok(());
            }
            removals.push(Removal {
                path: path.clone(),
                project: checkout.project_path().to_path_buf(),
                branch: checkout.worktree().branch,
                force,
                git: None,
                asking: vec![asking],
            });
        }
        let removed = self.run_git(&path);
        self.removed(&path, removed);
        Ok(())
    }

    /// Finishes the removals the last daemon handed over, each on a thread
    /// of its own.
    pub(super) fn carry_on_removals(self: &Arc<Self>, handed: Vec<HandedRemoval>) {
        for handed in handed {
            let asking = handed
                .asking
                .into_iter()
                .filter_map(|conn| handover::inherit(conn).ok())
                .filter_map(|conn| self.gate.admit(UnixStream::from(conn)))
                .collect();
            let (path, project, git) = (handed.path.clone(), handed.project.clone(), handed.git);
            self.removals.lock().unwrap().push(Removal {
                path: handed.path,
                project: handed.project,
                branch: handed.branch,
                force: handed.force,
                git,
                asking,
            });
            let daemon = self.clone();
            thread::spawn(move || daemon.carry_on_removal(&path, &project, git));
        }
    }

    /// Finishes removing the worktree at `path`, of the repository whose
    /// main worktree is `project`, which the last daemon was removing:
    /// waits for its `git`, when it was still running, then has git remove
    /// the worktree again, if it's still there.
    fn carry_on_removal(&self, path: &Path, project: &Path, git: Option<u32>) {
        if let Some(pid) = git {
            // Waited for, then reaped while the removals are held, as one
            // started here is.
            let _ = handover::wait_for_end(pid);
            let mut removals = self.removals.lock().unwrap();
            let _ = handover::reap(pid);
            if let Some(removal) = removals.iter_mut().find(|removal| removal.path == path) {
                removal.git = None;
            }
        }
        let removed = match git::still_has_worktree(project, path) {
            Ok(true) => self
                .check_nothing_runs_in(path)
                .and_then(|()| self.run_git(path)),
            Ok(false) => Ok(()),
            Err(err) => Err(err),
        };
        self.removed(path, removed);
    }

    /// Refuses while a session runs in the worktree at `path`: removing it
    /// would pull the directory out from under the program.
    fn check_nothing_runs_in(&self, path: &Path) -> Result<()> {
        let sessions = self.sessions.lock().unwrap();
        let running: Vec<&str> = sessions
            .iter()
            .filter(|session| session.is_running() && in_worktree(session, path))
            .map(|session| session.name.as_str())
            .collect();
        if !running.is_empty() {
            bail!("{} still running in {}", running.join(", "), path.display());
        }
        Ok(())
    }

    /// Has git remove the worktree the daemon is removing at `path`, and
    /// waits for it. It's started, and reaped, while the removals are
    /// held, so a handover finds it either running, for the next daemon to
    /// wait for, or done.
    fn run_git(&self, path: &Path) -> Result<()> {
        let mut git = {
            let mut removals = self.removals.lock().unwrap();
            let Some(removal) = removals.iter_mut().find(|removal| removal.path == path) else {
                bail!("{} isn't being removed", path.display());
            };
            let git = git::start_removing_worktree(&removal.project, path, removal.force)?;
            removal.git = Some(git.id());
            git
        };
        let mut said = String::new();
        if let Some(mut stderr) = git.stderr.take() {
            let _ = stderr.read_to_string(&mut said);
        }
        handover::wait_for_end(git.id())?;
        let ended = {
            let mut removals = self.removals.lock().unwrap();
            if let Some(removal) = removals.iter_mut().find(|removal| removal.path == path) {
                removal.git = None;
            }
            git.wait()?
        };
        if !ended.success() {
            bail!("{}", said.trim());
        }
        Ok(())
    }

    /// The worktree at `path` is removed, or `removed` says why not. Once
    /// it's gone, everyone's told, and the sessions that had ended in it
    /// leave the list with it. Then whoever asked is answered. All of it
    /// happens while the sessions are held, so a handover finds the
    /// removal either all done and answered, or still to do, for the next
    /// daemon to finish.
    fn removed(&self, path: &Path, removed: Result<()>) {
        let mut sessions = self.sessions.lock().unwrap();
        let removal = {
            let mut removals = self.removals.lock().unwrap();
            let index = removals.iter().position(|removal| removal.path == path);
            index.map(|index| removals.remove(index))
        };
        let Some(removal) = removal else {
            return;
        };
        if removed.is_ok() {
            let branch = removal.branch.as_deref();
            self.events.emit(Event::worktree(false, path, branch));
            while let Some(index) = sessions
                .iter()
                .position(|session| !session.is_running() && in_worktree(session, path))
            {
                self.kill_at(&mut sessions, index);
            }
        }
        let answer = match removed {
            Ok(()) => Response::Done,
            Err(err) => Response::from(err),
        };
        // One short line, which goes in the socket's buffer whether the
        // client reads it or has gone, so it never holds the sessions up.
        for (conn, _ticket) in &removal.asking {
            let _ = protocol::send(conn, &answer);
        }
    }
}

/// Whether `session` runs in the worktree at `path`.
fn in_worktree(session: &Session, path: &Path) -> bool {
    session.project_path().is_some() && session.checkout_top() == path
}
