//! The work an agent leaves running as its turn ends, which wakes it once
//! it's done or due, and the turn it ends while that's to come: a command it
//! runs in the background, a Monitor watching something, a wakeup it
//! scheduled, or a task of any other kind Claude Code keeps for it. Claude
//! Code's Stop hook lists them (`background_tasks` and `session_crons`), to
//! tell a session that's done from one paused until its own work wakes it.
//! Its subagents are held for apart, as they're heard starting and stopping:
//! see [`crate::subagents`].
//!
//! A turn that ends with any of it to come, and doesn't ask the user
//! anything (see [`crate::asking`]), isn't the agent done: the turn is held,
//! the agent still at work, its task neither reminded of nor waiting on the
//! user, until
//!
//! - a turn of its own starts: Claude Code wakes it as each piece ends or
//!   comes due, and with each event a Monitor sees. That turn's end is held
//!   again while some is still to come, as its Stop hook says then;
//! - or the longest any of it can take has passed with the agent never
//!   woken: Claude Code stops a command in the background after two hours
//!   at most and a Monitor after an hour, and a wakeup it schedules comes
//!   within an hour. [`WAKE_SLACK`] more is given for the agent to wake.
//!
//! Pure, with the time given, so it's unit-tested.

use crate::protocol::{Pending, PendingKind};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

/// How long after the longest its work can take a held turn waits for the
/// agent to wake.
pub const WAKE_SLACK: Duration = Duration::from_secs(60);

/// The longest Claude Code lets a command run in the background.
const LONGEST_COMMAND: Duration = Duration::from_secs(2 * 60 * 60);

/// The longest Claude Code lets a Monitor watch.
const LONGEST_WATCH: Duration = Duration::from_secs(60 * 60);

/// The furthest off a wakeup holds a turn: `ScheduleWakeup` waits an hour
/// at most, and a recurring one wakes the agent again and again, each turn
/// held anew.
const LONGEST_WAKEUP: Duration = Duration::from_secs(60 * 60);

/// What an agent's last turn ended with still to come, and the turn held
/// for it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Background {
    #[serde(default)]
    pending: Vec<Pending>,
    #[serde(default)]
    held: Option<Held>,
}

/// A turn its agent ended with work of its own to come, held open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    /// When the turn ended.
    pub since: SystemTime,
    /// When it's let go of if the agent hasn't woken by then.
    pub until: SystemTime,
}

impl Background {
    /// Takes what the agent's turn just ended with still to come, as its
    /// Stop hook said: but for its subagents, held for apart.
    pub fn ended_with(&mut self, pending: Vec<Pending>) {
        self.pending = (pending.into_iter())
            .filter(|pending| pending.kind != PendingKind::Subagent)
            .collect();
        self.held = None;
    }

    /// A turn of the agent's own has started: whatever was to come, it's
    /// at work, and its end will say what's still to come then.
    pub fn resumed(&mut self) {
        *self = Background::default();
    }

    /// The agent's turn ended at `now`: whether it's held, with work of its
    /// own to come. A wakeup is due at `wakeup`, when that's known. Held
    /// already, it stays as it was.
    pub fn hold(&mut self, now: SystemTime, wakeup: Option<SystemTime>) -> bool {
        if self.pending.is_empty() {
            self.held = None;
            return false;
        }
        if self.held.is_none() {
            let longest = (self.pending.iter())
                .map(|pending| longest(pending.kind, now, wakeup))
                .max()
                .unwrap_or_default();
            self.held = Some(Held {
                since: now,
                until: now + longest + WAKE_SLACK,
            });
        }
        true
    }

    /// Whether its last turn ended with work of its own still to come.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The turn held, if one is.
    pub fn held(&self) -> Option<Held> {
        self.held
    }

    /// Whether the held turn is over at `now`, its agent never woken by the
    /// time the longest of its work could take. It's let go of then.
    pub fn let_go(&mut self, now: SystemTime) -> bool {
        let over = self.held.is_some_and(|held| now >= held.until);
        if over {
            *self = Background::default();
        }
        over
    }

    /// What the held turn waits on, in a few words, for the session's row:
    /// `None` while none is held.
    pub fn waits_on(&self) -> Option<String> {
        self.held?;
        let first = self.pending.first()?;
        let what = first.what.as_deref();
        let mut said = match (first.kind, what) {
            (PendingKind::Shell | PendingKind::Other, Some(what)) => {
                format!("in the background: {what}")
            }
            (PendingKind::Shell, None) => "a command in the background".to_string(),
            (PendingKind::Monitor, Some(what)) => format!("watching: {what}"),
            (PendingKind::Monitor, None) => "a Monitor watching".to_string(),
            (PendingKind::Wakeup, _) => "waiting for its wakeup".to_string(),
            (PendingKind::Cron, _) => "waiting for its next scheduled turn".to_string(),
            (PendingKind::Other | PendingKind::Subagent, None) => {
                "a task in the background".to_string()
            }
            (PendingKind::Subagent, Some(what)) => format!("subagent: {what}"),
        };
        if self.pending.len() > 1 {
            said.push_str(&format!(" (and {} more)", self.pending.len() - 1));
        }
        Some(said)
    }

    /// The agent has gone, and its work with it.
    pub fn forget(&mut self) {
        *self = Background::default();
    }
}

/// The longest work of `kind` can keep the agent waiting from `now`: a
/// wakeup due at `wakeup`, when that's known and within the hour.
fn longest(kind: PendingKind, now: SystemTime, wakeup: Option<SystemTime>) -> Duration {
    match kind {
        PendingKind::Shell | PendingKind::Other | PendingKind::Subagent => LONGEST_COMMAND,
        PendingKind::Monitor => LONGEST_WATCH,
        PendingKind::Wakeup => wakeup
            .and_then(|due| due.duration_since(now).ok())
            .map_or(LONGEST_WAKEUP, |due| due.min(LONGEST_WAKEUP)),
        PendingKind::Cron => LONGEST_WAKEUP,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + secs)
    }

    fn pending(kind: PendingKind, what: Option<&str>) -> Pending {
        Pending {
            kind,
            what: what.map(String::from),
        }
    }

    #[test]
    fn a_turn_ending_with_nothing_to_come_is_not_held() {
        let mut background = Background::default();
        background.ended_with(Vec::new());
        assert!(!background.hold(at(0), None));
        assert!(!background.let_go(at(100_000)), "nothing to let go of");
        // Subagents are held for apart.
        background.ended_with(vec![pending(PendingKind::Subagent, Some("review"))]);
        assert!(!background.hold(at(0), None));
    }

    #[test]
    fn a_turn_ending_with_a_command_in_the_background_is_held_until_the_agent_wakes() {
        let mut background = Background::default();
        background.ended_with(vec![pending(PendingKind::Shell, Some("cargo test"))]);
        assert!(background.hold(at(0), None));
        assert_eq!(
            background.waits_on().as_deref(),
            Some("in the background: cargo test")
        );
        // Its end read again off the screen is the same turn, held as it was.
        assert!(background.hold(at(30), None));
        assert_eq!(background.held().unwrap().since, at(0));
        assert!(!background.let_go(at(600)));
        // Woken as the command ends, the agent is at work again.
        background.resumed();
        assert_eq!(background.held(), None);
        assert_eq!(background.waits_on(), None);
        // That turn's end says what's still to come: nothing.
        background.ended_with(Vec::new());
        assert!(!background.hold(at(700), None));
    }

    #[test]
    fn a_held_turn_whose_agent_never_wakes_is_let_go_once_its_work_can_t_still_run() {
        let mut background = Background::default();
        background.ended_with(vec![pending(PendingKind::Monitor, Some("CI checks"))]);
        background.hold(at(0), None);
        let until = LONGEST_WATCH + WAKE_SLACK;
        assert!(!background.let_go(at(until.as_secs() - 1)));
        assert!(background.let_go(at(until.as_secs())));
        assert_eq!(background, Background::default());
        assert!(
            !background.let_go(at(until.as_secs() + 1)),
            "let go of once"
        );

        // The longest of what's to come counts.
        background.ended_with(vec![
            pending(PendingKind::Monitor, None),
            pending(PendingKind::Shell, None),
        ]);
        background.hold(at(0), None);
        assert_eq!(
            background.held().unwrap().until,
            at((LONGEST_COMMAND + WAKE_SLACK).as_secs())
        );
        assert_eq!(
            background.waits_on().as_deref(),
            Some("a Monitor watching (and 1 more)")
        );
    }

    #[test]
    fn a_wakeup_holds_the_turn_until_it_s_due() {
        let mut background = Background::default();
        background.ended_with(vec![pending(PendingKind::Wakeup, None)]);
        background.hold(at(0), Some(at(270)));
        assert_eq!(
            background.held().unwrap().until,
            at(270 + WAKE_SLACK.as_secs())
        );
        assert_eq!(
            background.waits_on().as_deref(),
            Some("waiting for its wakeup")
        );
        // One that isn't known, or is far off, holds it an hour at most.
        let mut background = Background::default();
        background.ended_with(vec![pending(PendingKind::Cron, None)]);
        background.hold(at(0), Some(at(86_400)));
        assert_eq!(
            background.held().unwrap().until,
            at((LONGEST_WAKEUP + WAKE_SLACK).as_secs())
        );
    }

    #[test]
    fn what_was_to_come_goes_with_the_agent() {
        let mut background = Background::default();
        background.ended_with(vec![pending(PendingKind::Shell, None)]);
        background.hold(at(0), None);
        let handed: Background =
            serde_json::from_str(&serde_json::to_string(&background).unwrap()).unwrap();
        assert_eq!(handed, background);
        background.forget();
        assert_eq!(background, Background::default());
    }
}
