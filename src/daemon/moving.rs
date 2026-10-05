//! Moving a session into another worktree of its project, which `crystal
//! worktree move` asks the daemon for, as often as not from the agent in the
//! session, when the user asks it to work in a worktree: its program is
//! stopped and started again in the worktree, an agent in its conversation,
//! told where it is now so that it carries on there. An agent in the middle
//! of a turn moves once the turn ends: the one asking is in the middle of
//! the turn it asks in, and stopping it there would cut its answer off. A
//! handover hands the moves still to come over. Adapted from docket's
//! `docket worktree`.

use super::{Daemon, start_as};
use crate::events::{Event, Kind};
use crate::git::Checkout;
use crate::handover::HandedMove;
use crate::protocol::{Activity, NewSession, Response};
use crate::session::Session;
use anyhow::{Context, Result, bail, ensure};
use std::path::{Path, PathBuf};

/// A session on its way into another worktree.
pub(super) struct Move {
    /// The session's id, which a rename leaves as it is.
    session: String,
    /// The worktree's top directory.
    path: PathBuf,
    /// The branch it's on, which the agent is told.
    branch: Option<String>,
    /// Whether its program has been stopped, to start again in the
    /// worktree once it has ended.
    stopping: bool,
}

impl Move {
    /// The move as it's handed over.
    pub(super) fn hand_over(&self) -> HandedMove {
        HandedMove {
            session: self.session.clone(),
            path: self.path.clone(),
            branch: self.branch.clone(),
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
        if session.is_task() {
            bail!(
                "{name} is a background task, which can't move: `crystal tasks terminal {name}` \
                 opens it in a terminal, which can"
            );
        }
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
                path: worktree.path,
                branch: worktree.branch,
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
    /// is over is stopped, and one stopped that has ended starts again in
    /// its worktree. A move whose session has gone, killed or archived, is
    /// dropped.
    pub(super) fn carry_out_moves(&self, sessions: &mut Vec<Session>) {
        let mut moves = self.moves.lock().unwrap();
        if moves.is_empty() {
            return;
        }
        let mut ready = Vec::new();
        moves.retain_mut(|moving| {
            let Some(session) = sessions.iter().find(|session| session.id == moving.session) else {
                return false;
            };
            if !session.is_running() {
                ready.push(Move {
                    session: moving.session.clone(),
                    path: moving.path.clone(),
                    branch: moving.branch.clone(),
                    stopping: true,
                });
                return false;
            }
            if !moving.stopping && turn_over(session) {
                session.stop();
                moving.stopping = true;
            }
            true
        });
        drop(moves);
        for moving in ready {
            self.start_moved(sessions, &moving);
        }
    }

    /// Starts the ended session `moving` is about again in its worktree, in
    /// its place, under its name and id, an agent in its conversation and
    /// told where it is. One that can't start stays, failed, saying why,
    /// to be started again there.
    fn start_moved(&self, sessions: &mut Vec<Session>, moving: &Move) {
        let Some(index) = sessions
            .iter()
            .position(|session| session.id == moving.session)
        else {
            return;
        };
        let ended = sessions.remove(index);
        let mut saved = ended.launch();
        saved.cwd = moved_cwd(ended.cwd(), &ended.checkout_top(), &moving.path);
        let mut env = ended.env().clone();
        // Where its last program ran: its next works it out for itself.
        env.remove("PWD");
        env.remove("OLDPWD");
        let goal = saved.goal.clone();
        let new = NewSession {
            name: Some(saved.name.clone()),
            cwd: saved.cwd.clone(),
            command: saved.command.clone(),
            env,
            task: goal.as_ref().map(|goal| goal.goal.clone()),
            backlog: goal.as_ref().and_then(|goal| goal.backlog),
            brief: goal
                .as_ref()
                .map(|goal| goal.brief.clone())
                .unwrap_or_default(),
        };
        let notice = notice(&moving.path, moving.branch.as_deref());
        let started = start_as(
            ended.id.clone(),
            sessions,
            &self.socket,
            new,
            saved.conversation.clone(),
            saved.resume.clone(),
            Some(&notice),
            None,
        );
        // Whoever was looking at it is let go, to look again at the session
        // started under its id.
        ended.term().close();
        let session = match started {
            Ok(_) => {
                let mut started = sessions.pop().expect("start added a session");
                if let Some(goal) = goal {
                    started.give_task(goal);
                }
                if saved.name_given {
                    started.keep_given_name();
                }
                self.events
                    .emit(Event::about_session(Kind::SessionStarted, &started.info()));
                started
            }
            Err(err) => {
                let why = format!("{err:#}");
                eprintln!(
                    "crystal daemon: couldn't start {} again in {}: {why}",
                    saved.name,
                    moving.path.display()
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
        let sessions = self.sessions.lock().unwrap();
        let mut moves = self.moves.lock().unwrap();
        for handed in handed {
            let session = sessions.iter().find(|session| session.id == handed.session);
            if handed.stopping
                && let Some(session) = session
            {
                session.stop();
            }
            moves.push(Move {
                session: handed.session,
                path: handed.path,
                branch: handed.branch,
                stopping: handed.stopping,
            });
        }
    }
}

/// Whether `session`'s agent isn't in the middle of a turn: it has finished
/// one, or it's a program that says nothing of what it's doing.
fn turn_over(session: &Session) -> bool {
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

/// What an agent moved into the worktree at `path`, on `branch`, is told as
/// it starts again there: where it is now, and to carry on.
fn notice(path: &Path, branch: Option<&str>) -> String {
    let worktree = match branch {
        Some(branch) => format!("the worktree on the branch `{branch}`"),
        None => "a worktree".to_string(),
    };
    format!(
        "[crystal] This session has moved into {worktree}, at {}: your working directory is \
         that checkout now. Carry on with what the user last asked for there.",
        path.display()
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
        let told = notice(Path::new("/code/app.worktrees/fix"), Some("fix"));
        assert!(told.contains("`fix`"), "{told}");
        assert!(told.contains("/code/app.worktrees/fix"), "{told}");
        assert!(told.contains("Carry on"), "{told}");
        assert!(notice(Path::new("/x"), None).contains("a worktree"));
    }
}
