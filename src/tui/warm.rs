//! Asking the daemon to keep an agent warm where the selection is, while
//! `[sessions] warm_agent` is on (see `daemon/spare.rs`): what the
//! new-session panel would start there, once that has held for a moment,
//! so that going through the sidebar asks for nothing on the way; again
//! every few minutes while it's the same, which keeps it; and again once a
//! session has taken it over. Pure, so it's unit-tested; the event loop
//! asks.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long what the agent would be holds before it's asked for.
const HOLD: Duration = Duration::from_secs(1);

/// How often the same agent is asked for again: well within the quarter of
/// an hour the daemon keeps one nobody asks for.
const AGAIN: Duration = Duration::from_secs(5 * 60);

/// What an agent kept warm would be: where it starts, its command with no
/// first prompt, and whether the session that takes it over is a task.
pub type Wanted = (PathBuf, Vec<String>, bool);

#[derive(Debug, Default)]
pub struct Warming {
    /// What it would be now, and since when.
    seen: Option<(Wanted, Instant)>,
    /// What was asked for last, and when.
    asked: Option<(Wanted, Instant)>,
}

impl Warming {
    /// Takes what the agent would be at `now`, `None` for none, and gives
    /// back what to ask the daemon for, when it's time to.
    pub fn follow(&mut self, wanted: Option<Wanted>, now: Instant) -> Option<Wanted> {
        let Some(wanted) = wanted else {
            self.seen = None;
            return None;
        };
        if self.seen.as_ref().is_none_or(|(seen, _)| *seen != wanted) {
            self.seen = Some((wanted, now));
        }
        let (seen, since) = self.seen.as_ref()?;
        let fresh = |(asked, at): &(Wanted, Instant)| asked == seen && now < *at + AGAIN;
        if now < *since + HOLD || self.asked.as_ref().is_some_and(fresh) {
            return None;
        }
        self.asked = Some((seen.clone(), now));
        Some(seen.clone())
    }

    /// When to look again, for what it would be to be asked for in time.
    pub fn due(&self) -> Option<Instant> {
        let (seen, since) = self.seen.as_ref()?;
        match &self.asked {
            Some((asked, at)) if asked == seen => Some(*at + AGAIN),
            _ => Some(*since + HOLD),
        }
    }

    /// A session was started, which may have taken the agent over: the
    /// next look asks for another.
    pub fn taken(&mut self) {
        self.asked = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(dir: &str) -> Option<Wanted> {
        Some((PathBuf::from(dir), vec!["claude".into()], true))
    }

    #[test]
    fn an_agent_is_asked_for_once_where_the_selection_rests_and_kept_after() {
        let start = Instant::now();
        let mut warming = Warming::default();
        assert_eq!(warming.follow(None, start), None);
        assert_eq!(warming.due(), None);
        // Going past a worktree asks for nothing there.
        assert_eq!(warming.follow(wanted("/a"), start), None);
        assert_eq!(warming.due(), Some(start + HOLD));
        let later = start + HOLD / 2;
        assert_eq!(warming.follow(wanted("/b"), later), None);
        assert_eq!(warming.follow(wanted("/b"), start + HOLD), None);
        // Resting there does, once, then again a while after.
        let rested = later + HOLD;
        assert_eq!(warming.follow(wanted("/b"), rested), wanted("/b"));
        assert_eq!(warming.follow(wanted("/b"), rested + HOLD), None);
        assert_eq!(warming.due(), Some(rested + AGAIN));
        assert_eq!(warming.follow(wanted("/b"), rested + AGAIN), wanted("/b"));
        // A session started may have taken it: another is asked for.
        warming.taken();
        let after = rested + AGAIN + HOLD;
        assert_eq!(warming.follow(wanted("/b"), after), wanted("/b"));
        // Where nothing can be kept warm, nothing is asked, and coming
        // back holds again first.
        assert_eq!(warming.follow(None, after), None);
        assert_eq!(warming.due(), None);
        assert_eq!(warming.follow(wanted("/b"), after), None);
    }
}
