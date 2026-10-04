//! How the sidebar groups sessions: under their project, then under their
//! worktree. Sessions outside any repository come last, under their
//! directory. The steps of a flow run go together, under the run, after
//! their project's worktrees, whichever worktree each step ran in.
//!
//! Sessions waiting on the user come first without leaving their group: a
//! project with a waiting session moves to the top, and within its worktree
//! the waiting session leads. What needs the user is at the top, still next
//! to the work it belongs to. Within a worktree, its agents come before
//! its terminals, the shells and other programs beside them, with a line
//! between the two, so an agent never passes for a shell at a glance.
//! Everything else stays in the order it was made in, so the list doesn't
//! shuffle as agents work.
//!
//! A linked worktree with no sessions left stays at the end of its
//! project, with a row saying so, until it's removed: it's still on disk,
//! maybe with work in it, and the sidebar is where it's removed from.

use crate::flow_run::FlowRun;
use crate::front;
use crate::protocol::{Activity, Front, InProgress, SessionInfo, Worktree};
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
    /// worktree, where its pull requests are asked for; `path`, the
    /// worktree's own directory.
    Worktree {
        project: PathBuf,
        path: PathBuf,
        branch: Option<String>,
        main: bool,
        /// What git is in the middle of there, if anything.
        in_progress: Option<InProgress>,
    },
    /// A directory outside any repository, heading its sessions.
    Directory(PathBuf),
    /// The session at this index in the ordered list.
    Session(usize),
    /// The line between a worktree's agents and its terminals, when it has
    /// both.
    Terminals,
    /// The row under a linked worktree with no sessions, the one at this
    /// directory: the selection can be on it, to start something there or
    /// remove the worktree.
    NoSessions(PathBuf),
    /// The task of the session at this index, under its row: what it was
    /// asked to do, or how that went.
    Task(usize),
    /// The flow run at this index, heading its steps.
    Flow(usize),
    /// A step of a flow run with no session to show: one still to come, or
    /// whose session has gone. A step with a session is that session's row.
    Step { run: usize, step: usize },
}

/// Puts sessions in the sidebar's order. They come in the order they were
/// made in, which settles every tie. The sessions of `runs`' steps go after
/// their project's worktrees, a run at a time, each run's in step order.
pub fn order(sessions: Vec<SessionInfo>, runs: &[FlowRun]) -> Vec<SessionInfo> {
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
            let flow_step = flow_step(session, runs);
            SortKey {
                outside_git: project.is_none(),
                project_not_waiting: !waiting_projects.contains(&project),
                project_first: project_first[&project],
                in_a_flow: flow_step.is_some(),
                flow_step: flow_step.unwrap_or_default(),
                linked: session.worktree.as_ref().is_some_and(|w| !w.main),
                worktree_first: worktree_first[worktree],
                terminal: is_terminal(session),
                not_waiting: !is_waiting(session),
                made,
            }
        })
        .collect();

    let mut keyed: Vec<(SortKey, SessionInfo)> = keys.into_iter().zip(sessions).collect();
    keyed.sort_by(|(a, _), (b, _)| a.cmp(b));
    keyed.into_iter().map(|(_, session)| session).collect()
}

/// The sidebar's rows for sessions already in [`order`] with `runs`, those
/// that `keep` keeps by their index: a heading wherever the project or the
/// worktree changes, then each session, and under one with a task, its
/// task. Where a worktree's terminals follow its agents, a line goes
/// between them. A flow run is a heading of its own, then a row for each
/// step: its session's, or one for the step alone when it has none. Only a
/// kept session brings its headings, and a run's.
///
/// `empty` are the linked worktrees with no sessions: each goes at the end
/// of its project, a heading and a [`Row::NoSessions`], wherever the
/// project has a kept session.
pub fn rows(
    sessions: &[SessionInfo],
    runs: &[FlowRun],
    empty: &[Worktree],
    keep: impl Fn(usize) -> bool,
) -> Vec<Row> {
    let mut rows = Vec::new();
    // Where the session given a row last went, and whether it's a terminal.
    let mut previous: Option<(Option<&Path>, Under, bool)> = None;
    let kept = sessions
        .iter()
        .enumerate()
        .filter(|(index, _)| keep(*index));
    for (index, session) in kept {
        let (project, worktree) = group(session);
        let under = match flow_step(session, runs) {
            Some((run, _)) => Under::Run(run),
            None => Under::Worktree(worktree),
        };
        let terminal = is_terminal(session);
        let same_project = previous.is_some_and(|(p, _, _)| p == project);
        let same_group = same_project && previous.is_some_and(|(_, u, _)| u == under);
        let after_an_agent =
            same_group && previous.is_some_and(|(_, _, was_terminal)| !was_terminal);
        if !same_project {
            // The project before this one has ended.
            if let Some((Some(ended), _, _)) = previous {
                rows.extend(empty_rows(empty, ended));
            }
            rows.push(project_heading(session));
        }
        previous = Some((project, under, terminal));
        match under {
            // A run's rows all came with its first session.
            Under::Run(_) if same_group => {}
            Under::Run(run) => {
                rows.push(Row::Flow(run));
                rows.extend(step_rows(sessions, runs, run, &keep));
            }
            Under::Worktree(_) => {
                if !same_group {
                    rows.push(worktree_heading(session));
                }
                // A worktree's agents come first, so its first terminal
                // after an agent is where the line between them goes.
                if terminal && after_an_agent {
                    rows.push(Row::Terminals);
                }
                rows.push(Row::Session(index));
                if session.task.is_some() {
                    rows.push(Row::Task(index));
                }
            }
        }
    }
    if let Some((Some(last), _, _)) = previous {
        rows.extend(empty_rows(empty, last));
    }
    rows
}

/// The rows of the worktrees in `empty` that are `project`'s, in the order
/// they come, but for those Claude Code made for itself, which come last:
/// each one's heading, and the row saying it has no sessions.
pub fn empty_rows(empty: &[Worktree], project: &Path) -> Vec<Row> {
    let mut rows = Vec::new();
    let of_project = empty.iter().filter(|w| w.project_path == project);
    let (claude_codes, others): (Vec<&Worktree>, Vec<&Worktree>) =
        of_project.partition(|w| w.claude_codes_own());
    for worktree in others.into_iter().chain(claude_codes) {
        rows.push(Row::Worktree {
            project: worktree.project_path.clone(),
            path: worktree.path.clone(),
            branch: worktree.branch.clone(),
            main: false,
            in_progress: worktree.in_progress,
        });
        rows.push(Row::NoSessions(worktree.path.clone()));
    }
    rows
}

/// The worktrees in `linked` that no session in `sessions` is in, ended
/// ones included: those the sidebar would otherwise lose.
pub fn empty_worktrees(linked: &[Worktree], sessions: &[SessionInfo]) -> Vec<Worktree> {
    linked
        .iter()
        .filter(|worktree| {
            !sessions.iter().any(|session| {
                let in_it = session.worktree.as_ref();
                in_it.is_some_and(|w| w.path == worktree.path)
            })
        })
        .cloned()
        .collect()
}

/// What a session goes under in its project: its worktree, or the flow run
/// at this index, when it's one of its steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Under<'a> {
    Worktree(&'a Path),
    Run(usize),
}

/// A row for each step of the run at `run`, in order: its session's, when
/// it has one that `keep` keeps; none when `keep` leaves its session out;
/// and one for the step alone when it has no session.
fn step_rows(
    sessions: &[SessionInfo],
    runs: &[FlowRun],
    run: usize,
    keep: impl Fn(usize) -> bool,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for (step, state) in runs[run].steps.iter().enumerate() {
        let session = state
            .session
            .as_ref()
            .and_then(|name| sessions.iter().position(|session| session.name == *name));
        match session {
            Some(index) if keep(index) => rows.push(Row::Session(index)),
            Some(_) => {}
            None => rows.push(Row::Step { run, step }),
        }
    }
    rows
}

/// The flow run `session` is a step of, by its index in `runs`, and which
/// step.
pub fn flow_step(session: &SessionInfo, runs: &[FlowRun]) -> Option<(usize, usize)> {
    runs.iter().enumerate().find_map(|(index, run)| {
        let step = run
            .steps
            .iter()
            .position(|step| step.session.as_ref() == Some(&session.name))?;
        Some((index, step))
    })
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
    /// Within a project, the steps of flow runs go after the worktrees…
    in_a_flow: bool,
    /// …a run at a time, in the order they started, each run's steps in
    /// their order: (run, step).
    flow_step: (usize, usize),
    /// Within a project the main worktree goes first…
    linked: bool,
    /// …then worktrees in the order their first sessions were made.
    worktree_first: usize,
    /// Within a worktree, agents go before terminals. Only an agent can be
    /// waiting on the user, so a waiting session still leads its worktree…
    terminal: bool,
    /// …and among the agents, those waiting on the user go first…
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
            path: worktree.path.clone(),
            branch: worktree.branch.clone(),
            main: worktree.main,
            in_progress: worktree.in_progress,
        },
        None => Row::Directory(session.cwd.clone()),
    }
}

/// Whether a session is a terminal rather than an agent: what's in front
/// in it is a shell or some other program. Until the daemon has looked,
/// a moment after it starts, or once it has ended, its command says: a
/// session started as `claude` is an agent from its first moment.
pub fn is_terminal(session: &SessionInfo) -> bool {
    let front = session
        .front
        .clone()
        .or_else(|| front::of_command(&session.command));
    !matches!(front, Some(Front::Agent { .. } | Front::Task))
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
            in_progress: None,
        });
        SessionInfo {
            stopped_idle: false,
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
            asking: None,
            reporter: None,
            subagents: 0,
        }
    }

    fn waiting(mut session: SessionInfo) -> SessionInfo {
        session.activity = Some(Activity::Waiting);
        session
    }

    /// `session` with Claude Code in front.
    fn agent(mut session: SessionInfo) -> SessionInfo {
        session.front = Some(Front::Agent {
            program: "claude".into(),
            name: "Claude Code".into(),
        });
        session
    }

    /// `session` with zsh in front, at its prompt.
    fn shell(mut session: SessionInfo) -> SessionInfo {
        session.front = Some(Front::Shell { name: "zsh".into() });
        session
    }

    fn names(sessions: &[SessionInfo]) -> Vec<&str> {
        sessions.iter().map(|s| s.name.as_str()).collect()
    }

    /// Most tests have no flow runs.
    fn order(sessions: Vec<SessionInfo>) -> Vec<SessionInfo> {
        super::order(sessions, &[])
    }

    /// Most tests have no worktrees without sessions either.
    fn rows(sessions: &[SessionInfo], keep: impl Fn(usize) -> bool) -> Vec<Row> {
        super::rows(sessions, &[], &[], keep)
    }

    /// The linked worktree of `project` on `branch`, as git lists it.
    fn linked(project: &str, branch: &str) -> Worktree {
        Worktree {
            project: project.into(),
            project_path: PathBuf::from(format!("/code/{project}")),
            path: PathBuf::from(format!("/code/{project}/{branch}")),
            main: false,
            branch: Some(branch.into()),
            in_progress: None,
        }
    }

    /// A run of a flow of three steps, plan, build and review, whose first
    /// two ran in the sessions called `plan` and `build`.
    fn run() -> FlowRun {
        use crate::flows::{Flow, Step};
        let step = |name: &str| Step {
            name: name.into(),
            profile: None,
            prompt: "x".into(),
            placement: None,
            worktree: false,
            gate: false,
            back_to: None,
            max_rounds: None,
        };
        let flow = Flow {
            name: "ship".into(),
            description: None,
            steps: vec![step("plan"), step("build"), step("review")],
        };
        let mut run = FlowRun::new(
            "ship-1".into(),
            flow,
            &[],
            "goal".into(),
            PathBuf::from("/code/app"),
            Default::default(),
            0,
        );
        run.steps[0].session = Some("plan".into());
        run.steps[1].session = Some("build".into());
        run
    }

    #[test]
    fn a_flow_runs_steps_go_together_after_their_projects_worktrees() {
        let runs = [run()];
        let sessions = super::order(
            vec![
                session("build", "app", "feat"),
                session("a1", "app", "main"),
                session("plan", "app", "main"),
                session("a2", "app", "feat"),
            ],
            &runs,
        );
        assert_eq!(names(&sessions), ["a1", "a2", "plan", "build"]);
        let rows = super::rows(&sessions, &runs, &[], |_| true);
        assert_eq!(
            rows[5..],
            [
                Row::Flow(0),
                Row::Session(2),
                Row::Session(3),
                Row::Step { run: 0, step: 2 },
            ]
        );
    }

    #[test]
    fn a_step_whose_session_is_left_out_has_no_row() {
        let runs = [run()];
        let sessions = super::order(
            vec![
                session("plan", "app", "main"),
                session("build", "app", "main"),
            ],
            &runs,
        );
        let rows = super::rows(&sessions, &runs, &[], |index| {
            sessions[index].name == "build"
        });
        assert_eq!(
            rows[1..],
            [Row::Flow(0), Row::Session(1), Row::Step { run: 0, step: 2 }]
        );
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
    fn within_a_worktree_agents_come_before_terminals() {
        let sessions = order(vec![
            shell(session("zsh", "app", "main")),
            agent(session("claude", "app", "main")),
            session("server", "app", "main"),
            waiting(agent(session("claude-2", "app", "main"))),
        ]);
        assert_eq!(names(&sessions), ["claude-2", "claude", "zsh", "server"]);
    }

    #[test]
    fn a_line_goes_between_a_worktrees_agents_and_its_terminals() {
        let sessions = order(vec![
            agent(session("claude", "app", "main")),
            shell(session("zsh", "app", "main")),
            shell(session("zsh-2", "app", "main")),
            shell(session("only-a-shell", "app", "feat")),
        ]);
        let rows = rows(&sessions, |_| true);
        assert_eq!(
            rows[2..6],
            [
                Row::Session(0),
                Row::Terminals,
                Row::Session(1),
                Row::Session(2),
            ]
        );
        // A worktree of terminals alone has nothing to tell them from.
        assert_eq!(rows[6..].len(), 2);
        assert_eq!(rows[7], Row::Session(3));
    }

    #[test]
    fn a_task_counts_as_an_agent_and_a_shell_as_a_terminal() {
        let mut task = session("task", "app", "main");
        task.front = Some(Front::Task);
        assert!(!is_terminal(&task));
        assert!(!is_terminal(&agent(session("claude", "app", "main"))));
        assert!(is_terminal(&shell(session("zsh", "app", "main"))));
        assert!(is_terminal(&session("new", "app", "main")));
    }

    #[test]
    fn a_session_not_looked_at_yet_goes_by_its_command() {
        let mut claude = session("claude", "app", "main");
        claude.command = vec!["claude".into(), "fix the login".into()];
        assert!(!is_terminal(&claude));
        let mut server = session("server", "app", "main");
        server.command = vec!["npm".into(), "run".into(), "dev".into()];
        assert!(is_terminal(&server));
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
                    path: PathBuf::from("/code/app/main"),
                    branch: Some("main".into()),
                    main: true,
                    in_progress: None,
                },
                Row::Session(0),
                Row::Session(1),
                Row::Worktree {
                    project: PathBuf::from("/code/app"),
                    path: PathBuf::from("/code/app/feat"),
                    branch: Some("feat".into()),
                    main: false,
                    in_progress: None,
                },
                Row::Session(2),
                Row::OutsideGit,
                Row::Directory(PathBuf::from("/tmp")),
                Row::Session(3),
            ]
        );
    }

    #[test]
    fn a_worktree_with_no_sessions_stays_at_the_end_of_its_project() {
        let sessions = order(vec![
            session("a1", "app", "main"),
            session("w1", "web", "main"),
        ]);
        let empty = [linked("app", "old"), linked("web", "spike")];
        let rows = super::rows(&sessions, &[], &empty, |_| true);
        let old = Row::Worktree {
            project: PathBuf::from("/code/app"),
            path: PathBuf::from("/code/app/old"),
            branch: Some("old".into()),
            main: false,
            in_progress: None,
        };
        assert_eq!(
            rows[2..5],
            [
                Row::Session(0),
                old,
                Row::NoSessions(PathBuf::from("/code/app/old")),
            ]
        );
        assert!(matches!(&rows[5], Row::Project { name, .. } if name == "web"));
        assert_eq!(
            rows.last(),
            Some(&Row::NoSessions(PathBuf::from("/code/web/spike")))
        );
    }

    #[test]
    fn claude_code_s_own_worktrees_come_after_the_others() {
        let mut own = linked("app", "worktree-agent-1");
        own.path = PathBuf::from("/code/app/.claude/worktrees/agent-1");
        let empty = [own.clone(), linked("app", "old")];
        let rows = empty_rows(&empty, Path::new("/code/app"));
        assert_eq!(rows.len(), 4);
        assert!(
            matches!(&rows[0], Row::Worktree { path, .. } if path == Path::new("/code/app/old"))
        );
        assert!(matches!(&rows[2], Row::Worktree { path, .. } if *path == own.path));
        assert_eq!(rows[3], Row::NoSessions(own.path));
    }

    #[test]
    fn a_worktree_with_no_sessions_shows_only_where_its_project_does() {
        let sessions = order(vec![
            session("a1", "app", "main"),
            session("w1", "web", "main"),
        ]);
        let empty = [linked("app", "old")];
        let rows = super::rows(&sessions, &[], &empty, |index| index == 1);
        assert!(!rows.iter().any(|row| matches!(row, Row::NoSessions(_))));
    }

    #[test]
    fn a_worktree_with_a_session_in_it_ended_or_not_isn_t_empty() {
        let mut ended = session("a", "app", "feat");
        ended.state = State::Exited { code: 0 };
        let worktrees = [linked("app", "feat"), linked("app", "old")];
        let empty = empty_worktrees(&worktrees, &[ended]);
        assert_eq!(empty, [linked("app", "old")]);
    }
}
