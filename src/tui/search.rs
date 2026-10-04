//! Finding things by typing a little of them, for `/` in the sidebar: a
//! session, in any tab; a project crystal knows that nothing runs in, or a
//! worktree with no sessions; a flow run, by its steps; and an open pull
//! request of any project whose forge has listed them.
//!
//! A query matches when its letters turn up in order, not necessarily side
//! by side, ignoring case: `rfx` finds `refund-fix`. Each word of the query
//! has to turn up somewhere in what's found: a session's name, project,
//! branch, command, agent, tab or flow run, so `pay fix` finds a fixer in
//! the payments project. Long texts, a directory or a flow's goal, only
//! count where a word turns up in them whole, or nearly every query would
//! find them. Where a word turns up in the name, or a pull request's
//! title, those letters are marked, so the eye can see why it matched.
//!
//! Tab narrows what's found to the sessions with one status, as the keys
//! of herdr's goto picker do: see [`StatusFilter`].

use super::groups::Row;
use super::status::Status;
use crate::flow_run::FlowRun;
use crate::forge::PullRequest;
use crate::protocol::{Front, SessionInfo, Worktree};
use crate::shell;
use std::path::{Path, PathBuf};

/// What a session can be found by beyond what it knows of itself: where
/// it is.
#[derive(Debug, Default, Clone, Copy)]
pub struct Around<'a> {
    /// The name of the tab it's in, when the tab has one.
    pub tab: Option<&'a str>,
    /// The flow run it's a step of.
    pub run: Option<&'a FlowRun>,
}

/// Whether `session` matches `query`, and if it does, which characters of
/// its name to mark, counted from 0. An empty query matches everything.
pub fn session_match(query: &str, session: &SessionInfo, around: Around) -> Option<Vec<usize>> {
    let mut also = vec![session.command.join(" ")];
    if let Some(worktree) = &session.worktree {
        also.push(worktree.project.clone());
        also.extend(worktree.branch.clone());
    }
    if let Some(Front::Agent { program, name }) = &session.front {
        also.extend([program.clone(), name.clone()]);
    }
    also.extend(around.tab.map(str::to_string));
    let mut whole = vec![shell::home_relative(&session.cwd)];
    if let Some(run) = around.run {
        also.extend([run.name.clone(), run.flow.name.clone()]);
        whole.push(run.goal.clone());
    }
    found(query, &session.name, &also, &whole)
}

/// Whether `worktree`, one with no sessions, matches `query`: by its
/// project, its branch or, for one Claude Code made for itself, `subject`,
/// the subject of the commit it's at; or by its directory, whole.
pub fn worktree_match(query: &str, worktree: &Worktree, subject: Option<&str>) -> bool {
    let mut also = vec![worktree.project.clone()];
    also.extend(worktree.branch.clone());
    also.extend(subject.map(str::to_string));
    let whole = [shell::home_relative(&worktree.path)];
    found(query, "", &also, &whole).is_some()
}

/// Whether `pull_request`, of the project called `project`, matches
/// `query`, and if it does, which characters of its title to mark. It's
/// found by its title, its number (`57`, or `#57` as its forge writes it),
/// its branch, its author and its project.
pub fn pull_request_match(
    query: &str,
    pull_request: &PullRequest,
    project: &str,
) -> Option<Vec<usize>> {
    let also = [
        pull_request.label(),
        pull_request.local_branch.clone(),
        pull_request.author.clone(),
        project.to_string(),
    ];
    found(query, &pull_request.title, &also, &[])
}

/// Whether every word of `query` turns up in `name` or in one of `also`,
/// its letters in order, or whole in one of `whole`; and if so, which of
/// `name`'s characters to mark: those of the words found in it.
fn found(query: &str, name: &str, also: &[String], whole: &[String]) -> Option<Vec<usize>> {
    let mut marked = Vec::new();
    for word in query.split_whitespace() {
        if let Some(found) = letters_in(word, name) {
            marked.extend(found);
        } else if !also.iter().any(|text| letters_in(word, text).is_some())
            && !whole.iter().any(|text| side_by_side(word, text).is_some())
        {
            return None;
        }
    }
    marked.sort_unstable();
    marked.dedup();
    Some(marked)
}

/// Where `word`'s letters turn up in `text`, ignoring case: the characters'
/// places in `text`, or `None` when they don't all turn up. Letters side by
/// side are found first, so `fix` marks the end of `refund-fix`, not its
/// first `f`; failing that, letters in order with others between them.
pub fn letters_in(word: &str, text: &str) -> Option<Vec<usize>> {
    side_by_side(word, text).or_else(|| in_order(word, text))
}

/// Where `word` turns up whole in `text`, ignoring case.
fn side_by_side(word: &str, text: &str) -> Option<Vec<usize>> {
    let word: Vec<char> = word.chars().collect();
    let text: Vec<char> = text.chars().collect();
    if word.is_empty() || word.len() > text.len() {
        return None;
    }
    let start = (0..=text.len() - word.len()).find(|&start| {
        let here = &text[start..start + word.len()];
        here.iter().zip(&word).all(|(a, b)| same_letter(*a, *b))
    })?;
    Some((start..start + word.len()).collect())
}

/// Where `word`'s letters turn up in `text`, in order, each as early as it
/// can.
fn in_order(word: &str, text: &str) -> Option<Vec<usize>> {
    let mut found = Vec::new();
    let mut letters = text.chars().enumerate();
    for wanted in word.chars() {
        let (place, _) = letters.find(|(_, letter)| same_letter(*letter, wanted))?;
        found.push(place);
    }
    Some(found)
}

fn same_letter(a: char, b: char) -> bool {
    a.to_lowercase().eq(b.to_lowercase())
}

/// The one status `/` keeps to, once Tab has picked it: only the sessions
/// with it are found, and nothing else, since nothing else has a status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFilter {
    /// Agents asking the user something.
    Waiting,
    Working,
    /// Agents that finished a turn nobody has looked at.
    Done,
    /// Agents at their prompts, and terminals running.
    Idle,
    /// Programs that have ended, well or not.
    Ended,
}

impl StatusFilter {
    /// Tab's round, after every status.
    const ROUND: [StatusFilter; 5] = [
        StatusFilter::Waiting,
        StatusFilter::Working,
        StatusFilter::Done,
        StatusFilter::Idle,
        StatusFilter::Ended,
    ];

    /// The status Tab goes on to from `from`, `None` being every status:
    /// the next, `by` 1, or the one before, `by` -1, and back round to
    /// every status past either end.
    pub fn step(from: Option<StatusFilter>, by: isize) -> Option<StatusFilter> {
        // Every status is place 0, and each status the place after it.
        let places = StatusFilter::ROUND.len() as isize + 1;
        let at = from.map_or(0, |status| {
            let index = StatusFilter::ROUND.iter().position(|each| *each == status);
            1 + index.unwrap_or_default() as isize
        });
        let to = (at + by).rem_euclid(places) as usize;
        to.checked_sub(1).map(|index| StatusFilter::ROUND[index])
    }

    pub fn name(self) -> &'static str {
        match self {
            StatusFilter::Waiting => "waiting",
            StatusFilter::Working => "working",
            StatusFilter::Done => "done",
            StatusFilter::Idle => "idle",
            StatusFilter::Ended => "ended",
        }
    }

    /// Whether a session with `status` is kept.
    pub fn keeps(self, status: Status) -> bool {
        match self {
            StatusFilter::Waiting => status == Status::Waiting,
            StatusFilter::Working => status == Status::Working,
            StatusFilter::Done => status == Status::Done,
            StatusFilter::Idle => status == Status::Running,
            StatusFilter::Ended => matches!(status, Status::Ended | Status::Failed),
        }
    }

    /// The status the sessions it keeps show, whose color it's written in.
    pub fn status(self) -> Status {
        match self {
            StatusFilter::Waiting => Status::Waiting,
            StatusFilter::Working => Status::Working,
            StatusFilter::Done => Status::Done,
            StatusFilter::Idle => Status::Running,
            StatusFilter::Ended => Status::Ended,
        }
    }
}

/// Puts what `/` found beyond the sessions into the sidebar's `rows`, under
/// the heading of the project it's in: at the end of the project's rows
/// when it has some already, or else under a heading of its own, ahead of
/// the sessions outside any repository. `found` is each project's name,
/// its main worktree and its rows, in the order new headings go in.
pub fn place_under_projects(rows: &mut Vec<Row>, found: Vec<(String, PathBuf, Vec<Row>)>) {
    for (name, path, under) in found {
        let heading = rows.iter().position(|row| is_heading_of(row, &path));
        let at = match heading {
            Some(heading) => rows[heading + 1..]
                .iter()
                .position(|row| matches!(row, Row::Project { .. } | Row::OutsideGit))
                .map_or(rows.len(), |end| heading + 1 + end),
            None => {
                let at = rows
                    .iter()
                    .position(|row| *row == Row::OutsideGit)
                    .unwrap_or(rows.len());
                rows.insert(at, Row::Project { name, path });
                at + 1
            }
        };
        rows.splice(at..at, under);
    }
}

fn is_heading_of(row: &Row, project: &Path) -> bool {
    matches!(row, Row::Project { path, .. } if path == project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_run::FlowRun;
    use crate::flows::{Flow, Step};
    use crate::forge::{Checks, Forge, Review};
    use crate::protocol::{State, Worktree};
    use std::path::PathBuf;

    fn session(name: &str, project: &str, branch: &str, command: &str) -> SessionInfo {
        SessionInfo {
            stopped_idle: false,
            front: None,
            name: name.into(),
            id: name.into(),
            command: command.split(' ').map(String::from).collect(),
            cwd: PathBuf::from("/code"),
            pid: Some(1),
            state: State::Running,
            activity: None,
            worktree: Some(Worktree {
                project: project.into(),
                project_path: PathBuf::from(format!("/code/{project}")),
                path: PathBuf::from(format!("/code/{project}")),
                main: true,
                branch: Some(branch.into()),
                in_progress: None,
            }),
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
        }
    }

    fn matched(query: &str, session: &SessionInfo) -> Option<Vec<usize>> {
        session_match(query, session, Around::default())
    }

    fn linked(project: &str, branch: &str) -> Worktree {
        Worktree {
            project: project.into(),
            project_path: PathBuf::from(format!("/code/{project}")),
            path: PathBuf::from(format!("/code/{project}.worktrees/{branch}")),
            main: false,
            branch: Some(branch.into()),
            in_progress: None,
        }
    }

    fn pull_request(number: u64, title: &str, branch: &str) -> PullRequest {
        PullRequest {
            forge: Forge::GitHub,
            number,
            title: title.into(),
            author: "ana".into(),
            branch: branch.into(),
            from_fork: false,
            local_branch: branch.into(),
            draft: false,
            conflicts: false,
            merged: false,
            checks: Checks::None,
            review: Review::None,
            updated_at: "2026-10-02T09:30:00Z".into(),
            url: format!("https://github.com/acme/app/pull/{number}"),
        }
    }

    #[test]
    fn letters_turn_up_in_order_whatever_their_case() {
        assert_eq!(letters_in("rfx", "refund-fix"), Some(vec![0, 2, 9]));
        assert_eq!(letters_in("RF", "refund-fix"), Some(vec![0, 2]));
        assert_eq!(letters_in("xr", "refund-fix"), None);
    }

    #[test]
    fn letters_side_by_side_are_found_before_scattered_ones() {
        assert_eq!(letters_in("fix", "refund-fix"), Some(vec![7, 8, 9]));
        assert_eq!(letters_in("FUND", "refund-fix"), Some(vec![2, 3, 4, 5]));
    }

    #[test]
    fn a_match_in_the_name_marks_its_letters() {
        let fixer = session("refund-fix", "payments", "main", "claude");
        assert_eq!(matched("fix", &fixer), Some(vec![7, 8, 9]));
    }

    #[test]
    fn a_session_is_found_by_its_project_branch_or_command_too() {
        let fixer = session("refund-fix", "payments", "feat/ledger", "claude");
        assert_eq!(matched("pay", &fixer), Some(vec![]));
        assert_eq!(matched("ledger", &fixer), Some(vec![]));
        assert_eq!(matched("claude", &fixer), Some(vec![]));
        assert_eq!(matched("codex", &fixer), None);
    }

    #[test]
    fn a_session_is_found_by_its_agent_tab_and_flow_run() {
        let mut fixer = session("refund-fix", "payments", "main", "zsh");
        fixer.front = Some(Front::Agent {
            program: "codex".into(),
            name: "Codex".into(),
        });
        assert_eq!(matched("codex", &fixer), Some(vec![]));

        let in_review = Around {
            tab: Some("review"),
            run: None,
        };
        assert_eq!(session_match("review", &fixer, in_review), Some(vec![]));
        assert_eq!(matched("review", &fixer), None);

        let step = Step {
            name: "plan".into(),
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
            steps: vec![step],
        };
        let run = FlowRun::new(
            "ship-1".into(),
            flow,
            &[],
            "add retries to the payment client".into(),
            PathBuf::from("/code/payments"),
            Default::default(),
            0,
        );
        let in_run = Around {
            tab: None,
            run: Some(&run),
        };
        assert_eq!(session_match("ship", &fixer, in_run), Some(vec![]));
        assert_eq!(session_match("retries", &fixer, in_run), Some(vec![]));
    }

    #[test]
    fn a_long_text_counts_only_where_a_word_turns_up_in_it_whole() {
        let mut fixer = session("refund-fix", "payments", "main", "claude");
        fixer.cwd = PathBuf::from("/srv/billing/ledger");
        assert_eq!(matched("billing", &fixer), Some(vec![]));
        assert_eq!(matched("bldg", &fixer), None, "scattered in the path");
    }

    #[test]
    fn every_word_has_to_turn_up_somewhere() {
        let fixer = session("refund-fix", "payments", "main", "claude");
        assert_eq!(matched("pay fix", &fixer), Some(vec![7, 8, 9]));
        assert_eq!(matched("pay codex", &fixer), None);
    }

    #[test]
    fn an_empty_query_matches_everything() {
        let fixer = session("refund-fix", "payments", "main", "claude");
        assert_eq!(matched("  ", &fixer), Some(vec![]));
    }

    #[test]
    fn a_worktree_is_found_by_its_project_branch_subject_or_directory() {
        let spike = linked("payments", "old-spike");
        assert!(worktree_match("pay", &spike, None));
        assert!(worktree_match("spike", &spike, None));
        assert!(worktree_match("payments.worktrees", &spike, None));
        assert!(!worktree_match("retry", &spike, None));
        assert!(worktree_match(
            "retry",
            &spike,
            Some("Retry failed charges")
        ));
    }

    #[test]
    fn a_pull_request_is_found_by_its_title_number_branch_author_or_project() {
        let fix = pull_request(57, "Fix the login redirect", "fix-login");
        assert_eq!(
            pull_request_match("login", &fix, "app"),
            Some(vec![8, 9, 10, 11, 12])
        );
        assert_eq!(pull_request_match("57", &fix, "app"), Some(vec![]));
        assert_eq!(pull_request_match("#57", &fix, "app"), Some(vec![]));
        assert_eq!(pull_request_match("fix-login", &fix, "app"), Some(vec![]));
        assert_eq!(pull_request_match("ana", &fix, "app"), Some(vec![]));
        assert!(pull_request_match("app redirect", &fix, "app").is_some());
        assert_eq!(pull_request_match("58", &fix, "app"), None);
    }

    #[test]
    fn tab_goes_round_the_statuses_and_back_to_every_one() {
        let mut status = None;
        let mut names = Vec::new();
        for _ in 0..6 {
            status = StatusFilter::step(status, 1);
            names.push(status.map_or("every", StatusFilter::name));
        }
        assert_eq!(
            names,
            ["waiting", "working", "done", "idle", "ended", "every"]
        );
        assert_eq!(StatusFilter::step(None, -1), Some(StatusFilter::Ended));
        assert_eq!(StatusFilter::step(Some(StatusFilter::Waiting), -1), None);
    }

    #[test]
    fn a_status_filter_keeps_the_sessions_with_its_status() {
        assert!(StatusFilter::Waiting.keeps(Status::Waiting));
        assert!(!StatusFilter::Waiting.keeps(Status::Done));
        assert!(StatusFilter::Idle.keeps(Status::Running));
        assert!(StatusFilter::Ended.keeps(Status::Ended));
        assert!(StatusFilter::Ended.keeps(Status::Failed));
        assert!(!StatusFilter::Ended.keeps(Status::Working));
    }

    #[test]
    fn what_is_found_goes_under_its_projects_heading_or_one_of_its_own() {
        let app = PathBuf::from("/code/app");
        let api = PathBuf::from("/code/api");
        let heading = |name: &str, path: &PathBuf| Row::Project {
            name: name.into(),
            path: path.clone(),
        };
        let mut rows = vec![
            heading("app", &app),
            Row::Session(0),
            Row::OutsideGit,
            Row::Session(1),
        ];
        let pull_request = Row::PullRequest {
            project: app.clone(),
            number: 57,
        };
        place_under_projects(
            &mut rows,
            vec![
                ("app".into(), app.clone(), vec![pull_request.clone()]),
                (
                    "api".into(),
                    api.clone(),
                    vec![Row::NoSessions(api.clone())],
                ),
            ],
        );
        assert_eq!(
            rows,
            [
                heading("app", &app),
                Row::Session(0),
                pull_request,
                heading("api", &api),
                Row::NoSessions(api.clone()),
                Row::OutsideGit,
                Row::Session(1),
            ]
        );
    }
}
