//! Moving a session into another worktree of its project, which `crystal
//! worktree move` asks the daemon for, as often as not from the agent in the
//! session, when the user asks it to work in a worktree: its program is
//! stopped and started again in the worktree, an agent in its conversation,
//! told where it is now so that it carries on there, and a background task
//! with its `claude` in its conversation, told as a follow-up. An agent in
//! the middle of a turn, or a task in the middle of a run, moves once it's
//! over: the one asking is in the middle of the turn it asks in, and
//! stopping it there would cut its answer off. Its task stays open
//! meanwhile. A handover hands the moves still to come over, and a session
//! on its way is written down as it will be there, so that a restart
//! starts it there. Adapted from docket's `docket worktree`.

use super::Daemon;
use crate::events::{Event, Kind};
use crate::git::Checkout;
use crate::handover::HandedMove;
use crate::protocol::{Activity, Response};
use crate::session::Session;
use crate::state::{MovedTo, SavedSession};
use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};

/// A session on its way into another worktree.
#[derive(Clone)]
pub(super) struct Move {
    /// The session's id, which a rename leaves as it is.
    session: String,
    /// The worktree it moves into.
    to: MovedTo,
    /// Whether its program has been stopped, to start again in the
    /// worktree once it has ended.
    stopping: bool,
}

impl Move {
    /// The move as it's handed over.
    pub(super) fn hand_over(&self) -> HandedMove {
        HandedMove {
            session: self.session.clone(),
            path: self.to.path.clone(),
            branch: self.to.branch.clone(),
            stopping: self.stopping,
        }
    }
}

impl Daemon {
    /// Moves the session called `name` into the worktree at `path`: see
    /// [`crate::protocol::Request::MoveSession`].
    pub(super) fn move_session(&self, name: &str, path: &Path) -> Result<Response> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .iter()
            .find(|session| session.name == name)
            .with_context(|| format!("no session named {name}"))?;
        ensure!(
            !session.is_unstarted(),
            "{name} is yet to start again: move it once it has"
        );
        let checkout = Checkout::find(path)
            .with_context(|| format!("{} isn't in a git repository", path.display()))?;
        let worktree = checkout.worktree();
        ensure!(
            session.project_path() == Some(checkout.project_path()),
            "{} isn't a worktree of {name}'s project",
            worktree.path.display()
        );
        if session.checkout_top() == worktree.path {
            return Ok(Response::Done);
        }
        let later = session.is_running() && !turn_over(session);
        let id = session.id.clone();
        {
            let mut moves = self.moves.lock().unwrap();
            // Asked again, it goes where it was asked last.
            moves.retain(|moving| moving.session != id);
            moves.push(Move {
                session: id,
                to: MovedTo {
                    path: worktree.path,
                    branch: worktree.branch,
                },
                stopping: false,
            });
        }
        if !later {
            self.carry_out_moves(&mut sessions);
        }
        Ok(Response::Moved { later })
    }

    /// Whether the session with the id `id` is to move into another
    /// worktree: its agent isn't reminded of its task as its turn ends,
    /// which would keep it working where it is.
    pub(super) fn is_moving(&self, id: &str) -> bool {
        let moves = self.moves.lock().unwrap();
        moves.iter().any(|moving| moving.session == id)
    }

    /// Takes each move as far as it can go: a session whose agent's turn
    /// is over, or a task between runs, is stopped, and one stopped that
    /// has ended starts again in its worktree, a task at rest at once. A
    /// move whose session has gone, killed or archived, is dropped.
    pub(super) fn carry_out_moves(&self, sessions: &mut Vec<Session>) {
        let mut moves = self.moves.lock().unwrap();
        if moves.is_empty() {
            return;
        }
        let mut ready = Vec::new();
        moves.retain_mut(|moving| {
            let found = sessions.iter_mut().find(|s| s.id == moving.session);
            let Some(session) = found else {
                return false;
            };
            if session.is_running() && !moving.stopping && turn_over(session) {
                session.stop_to_move();
                moving.stopping = true;
            }
            if session.is_running() {
                return true;
            }
            ready.push(moving.clone());
            false
        });
        drop(moves);
        for moving in ready {
            self.start_moved(sessions, &moving);
        }
    }

    /// Starts the ended session `moving` is about again in its worktree, in
    /// its place, under its name and id, an agent in its conversation and
    /// told where it is, as after a restart. One that can't start stays,
    /// failed, saying why, to be started again there.
    fn start_moved(&self, sessions: &mut Vec<Session>, moving: &Move) {
        let Some(index) = sessions
            .iter()
            .position(|session| session.id == moving.session)
        else {
            return;
        };
        let ended = sessions.remove(index);
        let saved = moved(&ended, &moving.to);
        let mut env = ended.env().clone();
        // Where its last program ran: its next works it out for itself.
        env.remove("PWD");
        env.remove("OLDPWD");
        let started = self.start_saved(sessions, saved.clone(), env, Some(ended.id.clone()));
        // Whoever was looking at it is let go, to look again at the session
        // started under its id.
        ended.term().close();
        let session = match started {
            Ok(_) => {
                let started = sessions.pop().expect("start added a session");
                self.events
                    .emit(Event::about_session(Kind::SessionStarted, &started.info()));
                started
            }
            Err(err) => {
                let why = format!("{err:#}");
                eprintln!(
                    "crystal daemon: couldn't start {} again in {}: {why}",
                    saved.name,
                    moving.to.path.display()
                );
                let failed = Session::failed_to_start(ended.id.clone(), saved, &why);
                self.events.emit(Event::start_failed(&failed.info()));
                failed
            }
        };
        sessions.insert(index, session);
    }

    /// Takes on the moves the last daemon handed over. A session stopped
    /// for one is stopped again, in case it hasn't ended: the last daemon
    /// went before it could make sure.
    pub(super) fn carry_on_moves(&self, handed: Vec<HandedMove>) {
        let mut sessions = self.sessions.lock().unwrap();
        let mut moves = self.moves.lock().unwrap();
        for handed in handed {
            let session = sessions.iter_mut().find(|s| s.id == handed.session);
            if handed.stopping
                && let Some(session) = session
            {
                session.stop_to_move();
            }
            moves.push(Move {
                session: handed.session,
                to: MovedTo {
                    path: handed.path,
                    branch: handed.branch,
                },
                stopping: handed.stopping,
            });
        }
    }
}

/// The sessions as they're written down, to start again after a restart:
/// one on its way into another worktree as it will be there, its agent to
/// be told it has moved as it starts.
pub(super) fn written_down(sessions: &[Session], moves: &[Move]) -> Vec<SavedSession> {
    let write_down = |session: &Session| match moves.iter().find(|m| m.session == session.id) {
        // Stopped for its move, it's on its way all the same.
        Some(moving) => Some(moved(session, &moving.to)),
        None => session.saved(),
    };
    sessions.iter().filter_map(write_down).collect()
}

/// What starts `session` again in the worktree `to`: in the same directory
/// under it, when it has it, or else at its top, its agent told it has
/// moved.
fn moved(session: &Session, to: &MovedTo) -> SavedSession {
    SavedSession {
        cwd: moved_cwd(session.cwd(), &session.checkout_top(), &to.path),
        moved: Some(to.clone()),
        ..session.launch()
    }
}

/// Whether `session`'s agent isn't in the middle of a turn: it has finished
/// one, or it's a program that says nothing of what it's doing; or, for a
/// background task, whether it's between runs.
fn turn_over(session: &Session) -> bool {
    if session.is_task() {
        return !session.in_a_run();
    }
    !matches!(
        session.info().activity,
        Some(Activity::Working | Activity::Waiting)
    )
}

/// Where a session that ran in `cwd`, in the worktree whose top is `from`,
/// runs in the worktree whose top is `to`: in the same directory under it,
/// when the worktree has it, or else at its top.
fn moved_cwd(cwd: &Path, from: &Path, to: &Path) -> PathBuf {
    match cwd.strip_prefix(from) {
        Ok(under) if to.join(under).is_dir() => to.join(under),
        _ => to.to_path_buf(),
    }
}

/// What an agent moved into the worktree `to` is told as it starts again
/// there: where it is now, and to carry on.
pub(super) fn notice(to: &MovedTo) -> String {
    let worktree = match &to.branch {
        Some(branch) => format!("the worktree on the branch `{branch}`"),
        None => "a worktree".to_string(),
    };
    format!(
        "[crystal] This session has moved into {worktree}, at {}: your working directory is \
         that checkout now. Carry on with what the user last asked for there.",
        to.path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_keeps_its_place_under_the_worktree_when_the_new_one_has_it() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("app"), dir.path().join("app.worktrees/fix"));
        std::fs::create_dir_all(to.join("web/src")).unwrap();
        assert_eq!(
            moved_cwd(&from.join("web/src"), &from, &to),
            to.join("web/src")
        );
        assert_eq!(moved_cwd(&from.join("gone"), &from, &to), to);
        assert_eq!(moved_cwd(&from, &from, &to), to);
    }

    #[test]
    fn a_moved_agent_is_told_where_it_is_and_to_carry_on() {
        let to = |path: &str, branch: Option<&str>| MovedTo {
            path: PathBuf::from(path),
            branch: branch.map(String::from),
        };
        let told = notice(&to("/code/app.worktrees/fix", Some("fix")));
        assert!(told.contains("`fix`"), "{told}");
        assert!(told.contains("/code/app.worktrees/fix"), "{told}");
        assert!(told.contains("Carry on"), "{told}");
        assert!(notice(&to("/x", None)).contains("a worktree"));
    }
}
