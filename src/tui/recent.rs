//! The sessions the user has been on, the latest first, for `;` to go back
//! to the one before, in whichever tab it is, as tmux's `last-pane` and
//! herdr's `last_pane` do. A session counts once the selection has stayed
//! on it a moment: one passed over with `j` or `k` on the way to another
//! isn't one the user was on, nor where `;` takes them. Sessions are kept
//! by name, as the tabs keep them, since a session's id changes when the
//! daemon restarts. Pure, with the time given, so it's unit-tested.

use std::time::{Duration, Instant};

/// How long the selection stays on a session for the user to have been on
/// it.
pub const STAYED: Duration = Duration::from_secs(1);

/// How many sessions are kept, the latest.
const KEPT: usize = 32;

#[derive(Debug, Default)]
pub struct Recent {
    /// The session the selection is on, and since when.
    on: Option<(String, Instant)>,
    /// The sessions the user was on before, the latest first.
    before: Vec<String>,
}

impl Recent {
    /// The selection is on the session called `selected` at `now`, or on
    /// none: the one it was on before is one the user was on, if it stayed
    /// there long enough.
    pub fn note(&mut self, selected: Option<&str>, now: Instant) {
        if self.on() == selected {
            return;
        }
        if let Some((name, since)) = self.on.take()
            && now >= since + STAYED
        {
            self.keep(name);
        }
        self.on = selected.map(|name| (name.to_string(), now));
    }

    /// The session `;` goes back to: the latest the user was on but the one
    /// selected, among those `exists` says are still there. The one
    /// selected is one the user was on now, however short a while, so `;`
    /// again comes back to it.
    pub fn go_back(&mut self, exists: impl Fn(&str) -> bool) -> Option<String> {
        let on = self.on();
        let back = self
            .before
            .iter()
            .find(|name| Some(name.as_str()) != on && exists(name));
        let back = back?.clone();
        if let Some((name, _)) = self.on.take() {
            self.keep(name);
        }
        Some(back)
    }

    /// The session called `from` is called `to` now.
    pub fn renamed(&mut self, from: &str, to: &str) {
        let names = self
            .before
            .iter_mut()
            .chain(self.on.as_mut().map(|(name, _)| name));
        for name in names.filter(|name| name.as_str() == from) {
            *name = to.to_string();
        }
    }

    fn on(&self) -> Option<&str> {
        self.on.as_ref().map(|(name, _)| name.as_str())
    }

    /// Puts the session called `name` first among those the user was on.
    fn keep(&mut self, name: String) {
        self.before.retain(|kept| *kept != name);
        self.before.insert(0, name);
        self.before.truncate(KEPT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `seconds` after a start.
    fn at(start: Instant, seconds: f64) -> Instant {
        start + Duration::from_secs_f64(seconds)
    }

    fn there(_: &str) -> bool {
        true
    }

    #[test]
    fn going_back_goes_to_the_session_before_and_again_comes_back() {
        let start = Instant::now();
        let mut recent = Recent::default();
        recent.note(Some("a"), at(start, 0.0));
        recent.note(Some("b"), at(start, 5.0));
        assert_eq!(recent.go_back(there).as_deref(), Some("a"));
        recent.note(Some("a"), at(start, 6.0));
        // Straight away, back to b, and then to a, as often as it's asked.
        assert_eq!(recent.go_back(there).as_deref(), Some("b"));
        recent.note(Some("b"), at(start, 6.1));
        assert_eq!(recent.go_back(there).as_deref(), Some("a"));
    }

    #[test]
    fn a_session_passed_over_on_the_way_isnt_one_the_user_was_on() {
        let start = Instant::now();
        let mut recent = Recent::default();
        recent.note(Some("a"), at(start, 0.0));
        recent.note(Some("b"), at(start, 3.0));
        recent.note(Some("c"), at(start, 3.2));
        recent.note(Some("d"), at(start, 3.4));
        assert_eq!(recent.go_back(there).as_deref(), Some("a"));
        // Gone back from d at once, d is still one to come back to.
        recent.note(Some("a"), at(start, 3.5));
        assert_eq!(recent.go_back(there).as_deref(), Some("d"));
    }

    #[test]
    fn a_session_gone_is_passed_over_and_with_none_theres_nothing_to_go_back_to() {
        let start = Instant::now();
        let mut recent = Recent::default();
        assert_eq!(recent.go_back(there), None);
        recent.note(Some("a"), at(start, 0.0));
        recent.note(Some("b"), at(start, 2.0));
        recent.note(Some("c"), at(start, 4.0));
        assert_eq!(recent.go_back(|name| name != "b").as_deref(), Some("a"));
        assert_eq!(recent.go_back(|_| false), None);
    }

    #[test]
    fn a_session_renamed_is_kept_by_its_new_name() {
        let start = Instant::now();
        let mut recent = Recent::default();
        recent.note(Some("a"), at(start, 0.0));
        recent.note(Some("b"), at(start, 2.0));
        recent.renamed("a", "fix-login");
        recent.renamed("b", "docs");
        assert_eq!(recent.go_back(there).as_deref(), Some("fix-login"));
        recent.note(Some("fix-login"), at(start, 2.5));
        assert_eq!(recent.go_back(there).as_deref(), Some("docs"));
    }

    #[test]
    fn nothing_selected_is_left_like_a_session() {
        let start = Instant::now();
        let mut recent = Recent::default();
        recent.note(Some("a"), at(start, 0.0));
        // An empty tab, then back.
        recent.note(None, at(start, 2.0));
        recent.note(Some("b"), at(start, 9.0));
        assert_eq!(recent.go_back(there).as_deref(), Some("a"));
    }
}
