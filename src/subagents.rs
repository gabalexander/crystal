//! An agent's subagents, as its hooks tell of them, and the turn it ends
//! while they still work.
//!
//! Claude Code can run subagents in the background: the agent's turn ends,
//! its prompt comes back and its `Stop` hook runs while they go on, and as
//! each one finishes, Claude Code hands what it found to the agent, which
//! takes it up in a turn of its own. So a turn that ends with subagents
//! running isn't the agent done: the turn is held, the agent still at work,
//! until
//!
//! - a turn of its own starts, its prompt or its spinner on the screen, or
//!   a tool of its own finishes once none are running: the end of that turn
//!   is the end of its work, held again if some are running by then;
//! - they've all stopped, and the agent hasn't taken their work up within
//!   [`DRAINED_FOR`];
//! - or none has shown a sign of life, starting, stopping, a tool finishing
//!   or a permission asked for, in [`QUIET_FOR`]: their `SubagentStop`s
//!   aren't coming, and holding on would keep the agent at work for good.
//!
//! The daemon looks at a held turn each round of its keep-up, four times a
//! second. docket holds a turn the same way, looking every 30 s, and waits
//! 180 s after the last one stops; crystal reads the screen too, which
//! shows the agent's own turn starting within half a second, so a minute
//! is plenty. Nor does a subagent starting just after a turn ended open
//! it again, as docket's does within 30 s: crystal's hooks are commands
//! Claude Code waits for, so a subagent the turn started is told of before
//! the turn ends, and one starting after it is likelier a helper Claude
//! Code runs between turns, which would hold a turn that's over.
//!
//! Pure, with the time given, so it's unit-tested.

use crate::protocol::AgentEvent;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

/// How long a held turn waits, once its subagents have all stopped, for
/// the agent to take up what they found.
pub const DRAINED_FOR: Duration = Duration::from_secs(60);

/// How long a held turn waits for a sign of life from subagents still
/// counted as running: longer than Claude Code's longest command, ten
/// minutes, which a subagent waits on in silence.
pub const QUIET_FOR: Duration = Duration::from_secs(15 * 60);

/// An agent's subagents: how many are running, and the turn held for them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Subagents {
    running: u32,
    held: Option<Held>,
}

/// A turn its agent ended while subagents ran, held open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    /// The last sign of life from the subagents, or when the turn ended.
    alive: SystemTime,
    /// When the last of them stopped, with none started since.
    drained: Option<SystemTime>,
}

#[cfg(test)]
impl Held {
    /// A turn held since `alive`, its subagents all stopped at `drained`.
    pub fn at(alive: SystemTime, drained: Option<SystemTime>) -> Held {
        Held { alive, drained }
    }
}

impl Subagents {
    /// As a daemon hands them over: how many are running, and the turn
    /// held for them.
    pub fn handed(running: u32, held: Option<Held>) -> Subagents {
        Subagents { running, held }
    }

    /// How many are running, as the hooks say.
    pub fn running(&self) -> u32 {
        self.running
    }

    /// The turn held for them, if one is.
    pub fn held(&self) -> Option<Held> {
        self.held
    }

    /// Takes in what the agent did at `now`: a subagent starting or
    /// stopping, a sign of life from them, a turn of its own, which ends
    /// the hold, or starting afresh, with none.
    pub fn heard(&mut self, event: AgentEvent, now: SystemTime) {
        match event {
            AgentEvent::Started => *self = Subagents::default(),
            AgentEvent::TurnStarted => self.held = None,
            AgentEvent::SubagentStarted => {
                self.running += 1;
                if let Some(held) = &mut self.held {
                    held.alive = now;
                    held.drained = None;
                }
            }
            AgentEvent::SubagentStopped => {
                self.running = self.running.saturating_sub(1);
                if let Some(held) = &mut self.held {
                    held.alive = now;
                    if self.running == 0 {
                        held.drained.get_or_insert(now);
                    }
                }
            }
            // With none running, a tool finishing is the agent's own: it
            // has taken their work up.
            AgentEvent::ToolFinished if self.running == 0 => self.held = None,
            AgentEvent::ToolFinished | AgentEvent::Asking => {
                if let Some(held) = &mut self.held {
                    held.alive = now;
                }
            }
            AgentEvent::TurnEnded | AgentEvent::StillIdle | AgentEvent::Named => {}
        }
    }

    /// The agent's turn ended at `now`: whether it's held, with subagents
    /// still running.
    pub fn hold(&mut self, now: SystemTime) -> bool {
        if self.running == 0 {
            self.held = None;
            return false;
        }
        self.held.get_or_insert(Held {
            alive: now,
            drained: None,
        });
        true
    }

    /// Whether the held turn is over at `now`, the agent done: they've all
    /// stopped and it hasn't taken their work up, or they've gone quiet,
    /// and are taken for gone. It's let go of then.
    pub fn let_go(&mut self, now: SystemTime) -> bool {
        let Some(held) = self.held else {
            return false;
        };
        let since = |then: SystemTime| now.duration_since(then).unwrap_or_default();
        let over = match held.drained {
            Some(drained) => since(drained) >= DRAINED_FOR,
            None => since(held.alive) >= QUIET_FOR,
        };
        if over {
            *self = Subagents::default();
        }
        over
    }

    /// The agent has gone, taking its subagents with it.
    pub fn forget(&mut self) {
        *self = Subagents::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AgentEvent::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + secs)
    }

    fn running(count: u32) -> Subagents {
        let mut subagents = Subagents::default();
        for _ in 0..count {
            subagents.heard(SubagentStarted, at(0));
        }
        subagents
    }

    #[test]
    fn they_are_counted_as_they_start_and_stop() {
        let mut subagents = Subagents::default();
        subagents.heard(SubagentStarted, at(0));
        subagents.heard(SubagentStarted, at(0));
        assert_eq!(subagents.running(), 2);
        subagents.heard(SubagentStopped, at(1));
        assert_eq!(subagents.running(), 1);
        subagents.heard(TurnEnded, at(1));
        assert_eq!(subagents.running(), 1, "they can outlive a turn");
        subagents.heard(SubagentStopped, at(2));
        subagents.heard(SubagentStopped, at(2));
        assert_eq!(subagents.running(), 0, "never below none");
        let mut afresh = running(3);
        afresh.hold(at(0));
        afresh.heard(Started, at(1));
        assert_eq!(afresh, Subagents::default());
    }

    #[test]
    fn a_turn_ending_with_none_running_is_not_held() {
        let mut subagents = Subagents::default();
        assert!(!subagents.hold(at(0)));
        assert!(!subagents.let_go(at(10_000)), "nothing to let go of");
    }

    #[test]
    fn a_held_turn_waits_a_while_after_the_last_stops_for_the_agent_to_take_it_up() {
        let mut subagents = running(2);
        assert!(subagents.hold(at(0)));
        subagents.heard(SubagentStopped, at(100));
        assert!(
            !subagents.let_go(at(100 + DRAINED_FOR.as_secs())),
            "one still runs"
        );
        subagents.heard(SubagentStopped, at(200));
        assert!(!subagents.let_go(at(259)));
        assert!(subagents.let_go(at(260)));
        assert_eq!(subagents.held(), None);
        assert!(!subagents.let_go(at(261)), "let go of once");
    }

    #[test]
    fn one_starting_after_they_had_all_stopped_holds_on() {
        let mut subagents = running(1);
        subagents.hold(at(0));
        subagents.heard(SubagentStopped, at(10));
        subagents.heard(SubagentStarted, at(20));
        assert!(!subagents.let_go(at(10 + DRAINED_FOR.as_secs())));
        subagents.heard(SubagentStopped, at(30));
        assert!(subagents.let_go(at(30 + DRAINED_FOR.as_secs())));
    }

    #[test]
    fn a_turn_of_the_agent_s_own_ends_the_hold() {
        let mut subagents = running(1);
        subagents.hold(at(0));
        subagents.heard(TurnStarted, at(5));
        assert_eq!(subagents.held(), None);
        assert_eq!(subagents.running(), 1, "they run on");
        // Its end is held again while they do.
        assert!(subagents.hold(at(9)));

        // A tool of its own, once they've stopped, is it at work again.
        let mut subagents = running(1);
        subagents.hold(at(0));
        subagents.heard(SubagentStopped, at(5));
        subagents.heard(ToolFinished, at(6));
        assert_eq!(subagents.held(), None);
        assert!(!subagents.hold(at(9)));
    }

    #[test]
    fn subagents_that_go_quiet_are_given_up_on() {
        let mut subagents = running(2);
        subagents.hold(at(0));
        // Their tools and their questions are signs of life.
        subagents.heard(ToolFinished, at(600));
        subagents.heard(Asking, at(700));
        let quiet = QUIET_FOR.as_secs();
        assert!(!subagents.let_go(at(quiet)));
        assert!(!subagents.let_go(at(700 + quiet - 1)));
        assert!(subagents.let_go(at(700 + quiet)));
        assert_eq!(subagents.running(), 0, "taken for gone");
    }

    #[test]
    fn a_turn_held_again_keeps_its_clock() {
        let mut subagents = running(1);
        subagents.hold(at(0));
        // The screen and the hooks both say the turn ended.
        subagents.hold(at(QUIET_FOR.as_secs() - 1));
        assert!(subagents.let_go(at(QUIET_FOR.as_secs())));
    }

    #[test]
    fn a_clock_gone_back_lets_nothing_go() {
        let mut subagents = running(1);
        subagents.hold(at(100));
        assert!(!subagents.let_go(at(0)));
    }

    #[test]
    fn a_hold_is_handed_over_as_it_is() {
        let mut subagents = running(1);
        subagents.hold(at(0));
        let held: Held =
            serde_json::from_str(&serde_json::to_string(&subagents.held().unwrap()).unwrap())
                .unwrap();
        assert_eq!(Subagents::handed(1, Some(held)), subagents);
    }
}
