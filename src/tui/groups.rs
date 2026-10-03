//! How the sidebar groups sessions: under their project, then under their
//! worktree. Sessions outside any repository come last, under their
//! directory.
//!
//! Sessions waiting on the user come first without leaving their group: a
//! project with a waiting session moves to the top, and within its worktree
//! the waiting session leads. What needs the user is at the top, still next
//! to the work it belongs to. Everything else stays in the order it was
//! made in, so the list doesn't shuffle as agents work.

use crate::protocol::{Activity, SessionInfo};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// One row of the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A project's name, heading its worktrees, and its main worktree,
    /// which tells projects apart.
    Project { name: String, path: PathBuf },
    /// The heading for the sessions outside any repository.
    OutsideGit,
    /// A worktree's branch, heading its sessions. `branch` is `None` when
    /// the worktree is on no branch. `project` is the project's main
    /// worktree, where its pull requests are asked for.
    Worktree {
        project: PathBuf,
        branch: Option<String>,
        main: bool,
    },
    /// A directory outside any repository, heading its sessions.
    Directory(PathBuf),
    /// The session at this index in the ordered list.
    Session(usize),
    /// The task of the session at this index, under its row: what it was
    /// asked to do, or how that went.
    Task(usize),
}

/// Puts sessions in the sidebar's order. They come in the order they were
/// made in, which settles every tie.
pub fn order(sessions: Vec<SessionInfo>) -> Vec<SessionInfo> {
    // Where each project's and each worktree's first session is, and which
    // projects have a session waiting.
    let mut project_first: HashMap<Option<&Path>, usize> = HashMap::new();
    let mut worktree_first: HashMap<&Path, usize> = HashMap::new();
    let mut waiting_projects: HashSet<Option<&Path>> = HashSet::new();
    for (index, session) in sessions.iter().enumerate() {
        let (project, worktree) = group(session);
        project_first.entry(project).or_insert(index);
        worktree_first.entry(worktree).or_insert(index);
        if is_waiting(session) {
            waiting_projects.insert(project);
        }
    }

    let keys: Vec<SortKey> = sessions
        .iter()
        .enumerate()
        .map(|(made, session)| {
            let (project, worktree) = group(session);
            SortKey {
                outside_git: project.is_none(),
                project_not_waiting: !waiting_projects.contains(&project),
                project_first: project_first[&project],
                linked: session.worktree.as_ref().is_some_and(|w| !w.main),
                worktree_first: worktree_first[worktree],
                not_waiting: !is_waiting(session),
                made,
            }
        })
        .collect();

    let mut keyed: Vec<(SortKey, SessionInfo)> = keys.into_iter().zip(sessions).collect();
    keyed.sort_by(|(a, _), (b, _)| a.cmp(b));
    keyed.into_iter().map(|(_, session)| session).collect()
}

/// The sidebar's rows for sessions already in [`order`], those that `keep`
/// keeps by their index: a heading wherever the project or the worktree
/// changes, then each session, and under one with a task, its task. Only a
/// kept session brings its headings.
pub fn rows(sessions: &[SessionInfo], keep: impl Fn(usize) -> bool) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut previous: Option<(Option<&Path>, &Path)> = None;
    let kept = sessions
        .iter()
        .enumerate()
        .filter(|(index, _)| keep(*index));
    for (index, session) in kept {
        let (project, worktree) = group(session);
        let same_project = previous.is_some_and(|(p, _)| p == project);
        let same_worktree = same_project && previous.is_some_and(|(_, w)| w == worktree);
        if !same_project {
            rows.push(project_heading(session));
        }
        if !same_worktree {
            rows.push(worktree_heading(session));
        }
        rows.push(Row::Session(index));
        if session.task.is_some() {
            rows.push(Row::Task(index));
        }
        previous = Some((project, worktree));
    }
    rows
}

/// Where a session goes in the sidebar. Keys compare field by field, in
/// the order the fields are written, so the first field matters most.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct SortKey {
    /// Sessions outside any repository go last.
    outside_git: bool,
    /// Projects with a session waiting on the user go first…
    project_not_waiting: bool,
    /// …then projects in the order their first sessions were made.
    project_first: usize,
    /// Within a project the main worktree goes first…
    linked: bool,
    /// …then worktrees in the order their first sessions were made.
    worktree_first: usize,
    /// Within a worktree, sessions waiting on the user go first…
    not_waiting: bool,
    /// …then sessions in the order they were made.
    made: usize,
}

/// A session's project, by its main worktree's path (`None` outside any
/// repository), and its worktree, or its directory outside any repository.
fn group(session: &SessionInfo) -> (Option<&Path>, &Path) {
    match &session.worktree {
        Some(worktree) => (Some(&worktree.project_path), &worktree.path),
        None => (None, &session.cwd),
    }
}

fn project_heading(session: &SessionInfo) -> Row {
    match &session.worktree {
        Some(worktree) => Row::Project {
            name: worktree.project.clone(),
            path: worktree.project_path.clone(),
        },
        None => Row::OutsideGit,
    }
}

fn worktree_heading(session: &SessionInfo) -> Row {
    match &session.worktree {
        Some(worktree) => Row::Worktree {
            project: worktree.project_path.clone(),
            branch: worktree.branch.clone(),
            main: worktree.main,
        },
        None => Row::Directory(session.cwd.clone()),
    }
}

fn is_waiting(session: &SessionInfo) -> bool {
    session.activity == Some(Activity::Waiting)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{State, Worktree};

    /// A session in the worktree of `project` on `branch`, the main one when
    /// `branch` is "main"; or outside git, in `/tmp`, when `project` is "".
    fn session(name: &str, project: &str, branch: &str) -> SessionInfo {
        let worktree = (!project.is_empty()).then(|| Worktree {
            project: project.into(),
            project_path: PathBuf::from(format!("/code/{project}")),
            path: PathBuf::from(format!("/code/{project}/{branch}")),
            main: branch == "main",
            branch: Some(branch.into()),
        });
        SessionInfo {
            front: None,
            name: name.into(),
            id: name.into(),
            command: vec!["sh".into()],
            cwd: PathBuf::from("/tmp"),
            pid: Some(1),
            state: State::Running,
            activity: None,
            worktree,
            changed: 0,
            task: None,
        }
    }

    fn waiting(mut session: SessionInfo) -> SessionInfo {
        session.activity = Some(Activity::Waiting);
        session
    }

    fn names(sessions: &[SessionInfo]) -> Vec<&str> {
        sessions.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn sessions_are_grouped_by_project_then_worktree() {
        let sessions = order(vec![
            session("a1", "app", "feat"),
            session("w1", "web", "main"),
            session("a2", "app", "main"),
            session("a3", "app", "feat"),
        ]);
        // app's first session came first; its main worktree leads it.
        assert_eq!(names(&sessions), ["a2", "a1", "a3", "w1"]);
    }

    #[test]
    fn sessions_outside_git_go_last() {
        let sessions = order(vec![session("shell", "", ""), session("a1", "app", "main")]);
        assert_eq!(names(&sessions), ["a1", "shell"]);
    }

    #[test]
    fn a_waiting_session_brings_its_project_up_and_leads_its_worktree() {
        let sessions = order(vec![
            session("a1", "app", "main"),
            session("w1", "web", "main"),
            waiting(session("w2", "web", "main")),
        ]);
        assert_eq!(names(&sessions), ["w2", "w1", "a1"]);
    }

    #[test]
    fn sessions_left_out_take_their_headings_with_them() {
        let sessions = order(vec![
            session("a1", "app", "main"),
            session("w1", "web", "main"),
        ]);
        let rows = rows(&sessions, |index| sessions[index].name == "w1");
        assert!(matches!(&rows[0], Row::Project { name, .. } if name == "web"));
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2], Row::Session(1));
    }

    #[test]
    fn rows_head_each_project_and_worktree_once() {
        let sessions = order(vec![
            session("a1", "app", "main"),
            session("a2", "app", "main"),
            session("a3", "app", "feat"),
            session("shell", "", ""),
        ]);
        assert_eq!(
            rows(&sessions, |_| true),
            [
                Row::Project {
                    name: "app".into(),
                    path: PathBuf::from("/code/app")
                },
                Row::Worktree {
                    project: PathBuf::from("/code/app"),
                    branch: Some("main".into()),
                    main: true
                },
                Row::Session(0),
                Row::Session(1),
                Row::Worktree {
                    project: PathBuf::from("/code/app"),
                    branch: Some("feat".into()),
                    main: false
                },
                Row::Session(2),
                Row::OutsideGit,
                Row::Directory(PathBuf::from("/tmp")),
                Row::Session(3),
            ]
        );
    }
}
