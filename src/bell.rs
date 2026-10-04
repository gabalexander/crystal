//! The terminal bell a session's program rings, passed on to the user's own
//! terminal, which beeps, flashes or marks its tab the way the user set it
//! up to. A viewer passes on the bells of the session it shows, and the TUI
//! those of a session out of sight that the daemon marked as having rung.
//!
//! A program that rings in a loop can't flood the user's terminal: one
//! bell is passed on at most every [`GAP`], and the rest in between are
//! let go.

use std::io::{self, Write};
use std::time::{Duration, Instant};

/// The least time between two bells passed on.
pub const GAP: Duration = Duration::from_millis(500);

/// Passes bells on, at most one every [`GAP`].
#[derive(Debug, Default)]
pub struct Ringer {
    last: Option<Instant>,
}

impl Ringer {
    /// Whether a bell rung at `now` is passed on: none has been for
    /// [`GAP`]. One that is counts from `now`.
    pub fn rings(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < GAP)
        {
            return false;
        }
        self.last = Some(now);
        true
    }

    /// Rings the user's terminal, on standard output, if a bell rung now
    /// is passed on.
    pub fn ring(&mut self) -> io::Result<()> {
        if !self.rings(Instant::now()) {
            return Ok(());
        }
        let mut out = io::stdout().lock();
        out.write_all(b"\x07")?;
        out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bell_is_passed_on_at_most_once_a_gap() {
        let mut ringer = Ringer::default();
        let start = Instant::now();
        assert!(ringer.rings(start));
        assert!(!ringer.rings(start + Duration::from_millis(10)));
        assert!(!ringer.rings(start + GAP - Duration::from_millis(1)));
        assert!(ringer.rings(start + GAP));
        // A bell let go doesn't put the next one off.
        assert!(!ringer.rings(start + GAP + Duration::from_millis(100)));
        assert!(ringer.rings(start + GAP * 2));
    }
}
