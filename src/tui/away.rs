//! "While you were away": what the event log gained while the user wasn't
//! looking, said in one line on the footer when they come back, like
//! `while you were away: 2 tasks done · 1 failed · 1 needs you`. They were
//! away since the TUI was last quit, while its terminal didn't have focus
//! for a while, or, in a terminal that doesn't say when it has focus, while
//! nothing was typed or clicked for a while ([`Presence`]). `a` then opens
//! the timeline, with what's new since that point marked.
//!
//! Where the counting starts after a quit is kept in the database: the
//! latest event the user had seen when the TUI last knew they were there
//! ([`Seen`]).

use super::needs_you::{About, Row};
use crate::events::{Event, Kind};
use crate::protocol::{TaskRecord, TaskState};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// How long the user is gone, in milliseconds, before what happened
/// meanwhile is worth a line.
pub const AWAY_FOR: u64 = 5 * 60 * 1000;

/// The document the TUI keeps: the `seq` of the latest event the user had
/// seen.
#[derive(Debug, Serialize, Deserialize)]
pub struct Seen {
    pub seq: u64,
}

/// The `seq` a kept [`Seen`] holds, when there's one to read.
pub fn read_seen(json: Option<&str>) -> Option<u64> {
    let seen: Seen = serde_json::from_str(json?).ok()?;
    Some(seen.seq)
}

/// Whether the user is there, as far as the TUI can tell, every time in
/// milliseconds since the Unix epoch.
#[derive(Debug)]
pub struct Presence {
    /// When they last typed, clicked, pasted or came back.
    last_seen: u64,
    /// When the terminal lost focus, while it hasn't got it back.
    left: Option<u64>,
    /// Whether the terminal says when it has focus. Once it has, a long
    /// while without a key is the user reading, not gone.
    tells_focus: bool,
}

impl Presence {
    pub fn new(now: u64) -> Presence {
        Presence {
            last_seen: now,
            left: None,
            tells_focus: false,
        }
    }

    /// A key, a click or a paste, at `now`. When it ends a long while
    /// without the user, gives when they went: what happened since is what
    /// they missed.
    pub fn input(&mut self, now: u64) -> Option<u64> {
        let went = self.left.or((!self.tells_focus).then_some(self.last_seen));
        self.back(now, went)
    }

    pub fn focus_lost(&mut self, now: u64) {
        self.tells_focus = true;
        self.left.get_or_insert(now);
    }

    /// The terminal has focus again, at `now`: like [`Presence::input`].
    pub fn focus_gained(&mut self, now: u64) -> Option<u64> {
        self.tells_focus = true;
        let went = self.left;
        self.back(now, went)
    }

    fn back(&mut self, now: u64, went: Option<u64>) -> Option<u64> {
        self.last_seen = now;
        self.left = None;
        went.filter(|&went| now.saturating_sub(went) >= AWAY_FOR)
    }
}

/// What happened while the user was away, counted from the event log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// The `seq` of the last event before the first one counted: what the
    /// timeline marks as new is after it.
    pub after: u64,
    done: HashSet<String>,
    failed: HashSet<String>,
    /// Sessions, by id, whose agent finished a turn.
    finished: HashSet<String>,
    /// What came to wait on the user.
    waiting: HashSet<About>,
}

impl Tally {
    /// Counts `events`, the oldest first. A task counts once, and so does a
    /// session however many turns it finished.
    pub fn of(events: &[Event]) -> Tally {
        let after = events
            .first()
            .map_or(0, |event| event.seq.saturating_sub(1));
        let mut tally = Tally {
            after,
            ..Tally::default()
        };
        for event in events {
            let session = event.session.as_ref().map(|session| session.id.clone());
            match event.kind {
                Kind::TaskClosed => {
                    let Some(task) = &event.task else {
                        continue;
                    };
                    match task.state() {
                        TaskState::Done => tally.done.insert(task_key(task)),
                        TaskState::Failed => tally.failed.insert(task_key(task)),
                        _ => false,
                    };
                }
                Kind::SessionDone => tally.finished.extend(session),
                Kind::SessionWaiting | Kind::TaskWaiting | Kind::RunAsking => {
                    tally.waiting.extend(session.map(About::Session));
                }
                Kind::FlowGate => {
                    let run = event.flow.as_ref().map(|flow| About::Run(flow.run.clone()));
                    tally.waiting.extend(run);
                }
                _ => {}
            }
        }
        tally
    }

    /// `while you were away: 2 tasks done · 1 failed · 3 sessions finished
    /// · 1 needs you`, only the parts that aren't nothing, or `None` when
    /// nothing worth saying happened. Of what came to wait on the user,
    /// only what still waits, among `now`'s rows, counts.
    pub fn line(&self, now: &[Row]) -> Option<String> {
        let need_you = now
            .iter()
            .filter(|row| row.tier.blocking() && self.waiting.contains(&row.about))
            .count();
        let parts = [
            count(self.done.len(), "task done", "tasks done"),
            count(self.failed.len(), "failed", "failed"),
            count(self.finished.len(), "session finished", "sessions finished"),
            count(need_you, "needs you", "need you"),
        ];
        let said: Vec<String> = parts.into_iter().flatten().collect();
        (!said.is_empty()).then(|| format!("while you were away: {}", said.join(" · ")))
    }
}

/// What tells a task apart: its number, or for one from before tasks had
/// one, its session and when it was made.
fn task_key(task: &TaskRecord) -> String {
    match task.id {
        Some(id) => format!("t{id}"),
        None => format!("{} {}", task.session, task.created),
    }
}

/// `2 tasks done`, or nothing at all for none.
fn count(n: usize, one: &str, many: &str) -> Option<String> {
    match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    }
}

/// The line once said, and the point it counts from, until the timeline is
/// opened there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Away {
    pub line: String,
    pub after: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_run::FlowRun;
    use crate::flows::{Flow, Step};
    use crate::protocol::{
        Activity, Asking, SessionInfo, State, TaskOutcome, TaskRecord, Worktree,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    const MINUTE: u64 = 60 * 1000;

    #[test]
    fn a_long_while_without_a_key_is_away_where_focus_isnt_told() {
        let mut presence = Presence::new(0);
        assert_eq!(presence.input(MINUTE), None);
        assert_eq!(presence.input(10 * MINUTE), Some(MINUTE));
        assert_eq!(presence.input(11 * MINUTE), None);
    }

    #[test]
    fn where_focus_is_told_only_losing_it_for_a_while_is_away() {
        let mut presence = Presence::new(0);
        presence.focus_lost(MINUTE);
        assert_eq!(presence.focus_gained(2 * MINUTE), None, "only a minute");
        // Reading without a key, with the focus, is being there.
        assert_eq!(presence.input(30 * MINUTE), None);
        presence.focus_lost(31 * MINUTE);
        assert_eq!(presence.focus_gained(40 * MINUTE), Some(31 * MINUTE));
        // A key without the focus coming back first ends it too.
        presence.focus_lost(50 * MINUTE);
        assert_eq!(presence.input(60 * MINUTE), Some(50 * MINUTE));
    }

    #[test]
    fn a_kept_seen_reads_back_and_anything_else_is_none() {
        let json = serde_json::to_string(&Seen { seq: 41 }).unwrap();
        assert_eq!(read_seen(Some(&json)), Some(41));
        assert_eq!(read_seen(Some("not json")), None);
        assert_eq!(read_seen(None), None);
    }

    fn session(name: &str, activity: Activity) -> SessionInfo {
        SessionInfo {
            stopped_idle: false,
            name: name.into(),
            id: format!("id-{name}"),
            command: vec!["claude".into()],
            cwd: PathBuf::from("/code/app"),
            pid: Some(1),
            state: State::Running,
            activity: Some(activity),
            worktree: Some(Worktree {
                project: "app".into(),
                project_path: "/code/app".into(),
                path: "/code/app".into(),
                main: true,
                branch: Some("main".into()),
                in_progress: None,
            }),
            changed: 0,
            front: None,
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
            row: Default::default(),
        }
    }

    fn closed(id: u64, session: &SessionInfo, state: TaskState) -> Event {
        let task = TaskRecord {
            id: Some(id),
            goal: "Fix it".into(),
            session: session.name.clone(),
            project: "app".into(),
            branch: None,
            background: false,
            backlog: None,
            pending: false,
            waiting: false,
            created: 1,
            outcome: Some(TaskOutcome::new(state, "", 2)),
            artifacts: Vec::new(),
            brief: Default::default(),
        };
        Event::task(Kind::TaskClosed, session, task)
    }

    fn numbered(events: Vec<Event>, first: u64) -> Vec<Event> {
        events
            .into_iter()
            .zip(first..)
            .map(|(event, seq)| Event { seq, ..event })
            .collect()
    }

    #[test]
    fn the_line_counts_each_thing_once_and_only_what_still_needs_you() {
        let worker = session("worker", Activity::Done);
        let asker = session("asker", Activity::Waiting);
        let answered = session("answered", Activity::Working);
        let events = numbered(
            vec![
                Event::activity(&worker, Some(Activity::Working), Activity::Done),
                Event::activity(&worker, Some(Activity::Working), Activity::Done),
                closed(1, &worker, TaskState::Done),
                closed(2, &worker, TaskState::Done),
                closed(3, &worker, TaskState::Failed),
                closed(4, &worker, TaskState::Cancelled),
                Event::asking(
                    &asker,
                    Asking {
                        tool: "Bash".into(),
                        gist: "ls".into(),
                    },
                ),
                Event::activity(&answered, Some(Activity::Working), Activity::Waiting),
            ],
            41,
        );
        let tally = Tally::of(&events);
        assert_eq!(tally.after, 40);
        let now = super::super::needs_you::rows(&[worker, asker, answered], &[]);
        assert_eq!(
            tally.line(&now).as_deref(),
            Some("while you were away: 2 tasks done · 1 failed · 1 session finished · 1 needs you")
        );
    }

    #[test]
    fn a_gate_that_still_waits_needs_you() {
        let step = Step {
            name: "plan".into(),
            profile: None,
            agent: None,
            model: None,
            effort: None,
            mode: None,
            background: None,
            prompt: "{goal}".into(),
            accept: Vec::new(),
            placement: None,
            worktree: false,
            gate: true,
            back_to: None,
            max_rounds: None,
        };
        let flow = Flow {
            name: "ship".into(),
            description: None,
            steps: vec![step],
        };
        let cwd = PathBuf::from("/code/app");
        let mut run = FlowRun::new(
            "ship-1".into(),
            flow,
            &[],
            "x".into(),
            cwd,
            BTreeMap::new(),
            1,
        );
        run.steps[0].state = crate::flow_run::StepState::AtGate;
        let gate = Event {
            flow: Some(crate::events::FlowAbout {
                run: "ship-1".into(),
                flow: "ship".into(),
                goal: "x".into(),
                step: Some("plan".into()),
                state: "waiting".into(),
                said: None,
                cost_usd: None,
            }),
            ..Event::new(Kind::FlowGate)
        };
        let tally = Tally::of(&[gate]);
        let now = super::super::needs_you::rows(&[], &[run]);
        assert_eq!(
            tally.line(&now).as_deref(),
            Some("while you were away: 1 needs you")
        );
        assert_eq!(tally.line(&[]), None, "once it's answered, nothing to say");
    }
}
