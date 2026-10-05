//! The worktrees killing sessions leaves with nothing in them, which the TUI
//! asks about removing too once `x` or closing a tab has killed the last
//! session in one, and `crystal kill` does on its terminal; `[worktrees]
//! remove_emptied` says whether to ask (see [`EmptiedWorktree`]). Archived
//! sessions that ran in one are counted in the question: they couldn't
//! start there again once it's gone. Adapted from docket's
//! `delete_empty_worktree`. Pure, so it's unit-tested.

use crate::config::EmptiedWorktree;
use crate::protocol::{ArchivedSession, SessionInfo, Worktree};
use std::path::{Path, PathBuf};

/// A linked worktree killing sessions has left with nothing in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emptied {
    pub path: PathBuf,
    /// What it's called: its label, or else its branch.
    pub name: String,
    /// How many archived sessions ran in it.
    pub archived: usize,
}

/// What to do about the worktrees killing sessions emptied, as the settings
/// say.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Those to remove without asking.
    pub remove: Vec<Emptied>,
    /// Those to ask about, in one question.
    pub ask: Vec<Emptied>,
}

/// The linked worktrees killing the sessions called `killed` leaves with
/// nothing in them, each once, in the order the sessions are in: not the
/// main worktree, nor one Claude Code made for itself, and none another of
/// `sessions`, running or not, is in.
pub fn by_killing(killed: &[String], sessions: &[SessionInfo]) -> Vec<Worktree> {
    let in_it = |session: &SessionInfo, path: &Path| {
        (session.worktree.as_ref()).is_some_and(|worktree| worktree.path == path)
    };
    let mut emptied: Vec<Worktree> = Vec::new();
    for session in sessions.iter().filter(|s| killed.contains(&s.name)) {
        let Some(worktree) = &session.worktree else {
            continue;
        };
        let seen = emptied.iter().any(|w| w.path == worktree.path);
        if seen || worktree.main || worktree.claude_codes_own() {
            continue;
        }
        let others = (sessions.iter())
            .any(|other| !killed.contains(&other.name) && in_it(other, &worktree.path));
        if !others {
            emptied.push(worktree.clone());
        }
    }
    emptied
}

/// The worktree's name in the question: its label, or else its branch.
pub fn name(worktree: &Worktree, label: Option<&str>) -> String {
    match label {
        Some(label) => label.to_string(),
        None => (worktree.branch.clone()).unwrap_or_else(|| "(detached)".to_string()),
    }
}

/// How many of the `archived` sessions ran in the worktree at `path`.
pub fn archived_in(path: &Path, archived: &[ArchivedSession]) -> usize {
    let ran_there = |session: &&ArchivedSession| {
        (session.worktree.as_ref()).is_some_and(|worktree| worktree.path == path)
    };
    archived.iter().filter(ran_there).count()
}

/// What `setting` has done with the worktrees in `emptied`: removed
/// without asking, asked about, or kept. One archived sessions ran in is
/// asked about even when the settings say to remove it.
pub fn plan(emptied: Vec<Emptied>, setting: EmptiedWorktree) -> Plan {
    match setting {
        EmptiedWorktree::Never => Plan::default(),
        EmptiedWorktree::Ask => Plan {
            remove: Vec::new(),
            ask: emptied,
        },
        EmptiedWorktree::Always => {
            let (ask, remove) = emptied.into_iter().partition(|e| e.archived > 0);
            Plan { remove, ask }
        }
    }
}

/// The question whether the worktrees in `emptied` go too, without how
/// it's answered.
pub fn question(emptied: &[Emptied]) -> String {
    let names: Vec<&str> = emptied.iter().map(|e| e.name.as_str()).collect();
    let (worktrees, them) = match names.len() {
        1 => ("worktree", "it"),
        _ => ("worktrees", "them"),
    };
    let names = words(&names);
    let archived: usize = emptied.iter().map(|e| e.archived).sum();
    let but = match archived {
        0 => String::new(),
        1 => " but an archived session, which won't start there again".to_string(),
        count => format!(" but {count} archived sessions, which won't start there again"),
    };
    format!("nothing else is in {worktrees} {names}{but}: remove {them} too?")
}

/// `a`, `a and b`, `a, b and c`.
fn words(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::State;
    use crate::state::SavedSession;

    fn worktree(branch: &str) -> Worktree {
        let main = branch == "main";
        Worktree {
            project: "app".into(),
            project_path: PathBuf::from("/code/app"),
            path: if main {
                PathBuf::from("/code/app")
            } else {
                PathBuf::from(format!("/code/app.worktrees/{branch}"))
            },
            main,
            branch: Some(branch.into()),
            in_progress: None,
        }
    }

    fn in_worktree(name: &str, branch: &str, state: State) -> SessionInfo {
        let worktree = worktree(branch);
        SessionInfo {
            name: name.into(),
            id: name.into(),
            command: vec!["sh".into()],
            cwd: worktree.path.clone(),
            pid: Some(1),
            state,
            activity: None,
            worktree: Some(worktree),
            changed: 0,
            task: None,
            asking: None,
            reporter: None,
            subagents: 0,
            model: None,
            line: None,
            bell: false,
            unseen_copies: 0,
            context: None,
            output_waits: 0,
            stopped_idle: false,
            front: None,
        }
    }

    fn archived_in_worktree(name: &str, branch: &str) -> ArchivedSession {
        let worktree = worktree(branch);
        ArchivedSession {
            id: name.into(),
            session: SavedSession {
                name: name.into(),
                command: vec!["claude".into()],
                cwd: worktree.path.clone(),
                conversation: None,
                task: None,
                goal: None,
                resume: None,
                about: Default::default(),
                moved: None,
            },
            worktree: Some(worktree),
            archived: 0,
        }
    }

    fn branches(worktrees: &[Worktree]) -> Vec<&str> {
        (worktrees.iter())
            .map(|w| w.branch.as_deref().unwrap_or(""))
            .collect()
    }

    fn killing(names: &[&str], sessions: &[SessionInfo]) -> Vec<Worktree> {
        let names: Vec<String> = names.iter().map(|name| name.to_string()).collect();
        by_killing(&names, sessions)
    }

    #[test]
    fn a_worktree_is_emptied_once_every_session_in_it_is_killed() {
        let sessions = [
            in_worktree("planner", "main", State::Running),
            in_worktree("fixer", "fix", State::Running),
            in_worktree("tests", "fix", State::Exited { code: 0 }),
            in_worktree("spiker", "spike", State::Running),
        ];
        assert!(killing(&["fixer"], &sessions).is_empty(), "tests is there");
        assert_eq!(
            branches(&killing(&["fixer", "tests", "spiker"], &sessions)),
            ["fix", "spike"]
        );
        assert!(killing(&["planner"], &sessions).is_empty(), "the main one");
        assert!(killing(&["nobody"], &sessions).is_empty());
    }

    #[test]
    fn claude_codes_own_worktrees_are_never_emptied() {
        let mut session = in_worktree("helper", "fix", State::Running);
        let own = PathBuf::from("/code/app/.claude/worktrees/agent-1a2b");
        session.worktree.as_mut().unwrap().path = own;
        assert!(killing(&["helper"], &[session]).is_empty());
    }

    #[test]
    fn a_worktree_is_named_by_its_label_or_else_its_branch() {
        let mut fix = worktree("fix");
        assert_eq!(name(&fix, None), "fix");
        assert_eq!(name(&fix, Some("try sqlite")), "try sqlite");
        fix.branch = None;
        assert_eq!(name(&fix, None), "(detached)");
    }

    #[test]
    fn the_archived_sessions_that_ran_in_a_worktree_are_counted() {
        let archived = [
            archived_in_worktree("old", "fix"),
            archived_in_worktree("older", "fix"),
            archived_in_worktree("planner", "main"),
        ];
        assert_eq!(archived_in(&worktree("fix").path, &archived), 2);
        assert_eq!(archived_in(&worktree("spike").path, &archived), 0);
    }

    fn emptied(name: &str, archived: usize) -> Emptied {
        Emptied {
            path: PathBuf::from(format!("/code/app.worktrees/{name}")),
            name: name.into(),
            archived,
        }
    }

    #[test]
    fn the_question_names_the_worktrees_and_counts_what_was_archived_there() {
        assert_eq!(
            question(&[emptied("fix", 0)]),
            "nothing else is in worktree fix: remove it too?"
        );
        assert_eq!(
            question(&[emptied("fix", 1)]),
            "nothing else is in worktree fix but an archived session, which won't start \
             there again: remove it too?"
        );
        assert_eq!(
            question(&[emptied("fix", 2), emptied("spike", 0), emptied("docs", 1)]),
            "nothing else is in worktrees fix, spike and docs but 3 archived sessions, \
             which won't start there again: remove them too?"
        );
    }

    #[test]
    fn the_settings_say_whether_to_ask_and_archived_sessions_always_are() {
        let both = || vec![emptied("fix", 0), emptied("spike", 2)];
        assert_eq!(
            plan(both(), EmptiedWorktree::Ask),
            Plan {
                remove: Vec::new(),
                ask: both(),
            }
        );
        assert_eq!(
            plan(both(), EmptiedWorktree::Always),
            Plan {
                remove: vec![emptied("fix", 0)],
                ask: vec![emptied("spike", 2)],
            }
        );
        assert_eq!(plan(both(), EmptiedWorktree::Never), Plan::default());
    }
}
