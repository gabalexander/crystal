//! Tabs: each one a way of laying out the panes beside the sidebar, the
//! session the first pane was on and the sessions split off beside it, so
//! that a few of them can be kept and switched between. A tab only arranges
//! sessions: making or closing one never starts or stops any.
//!
//! The tabs are kept in a file beside the daemon's state, so they're there
//! again the next time the TUI opens. Nothing else here does any I/O.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How many tabs there can be: one for each of the keys 1 to 9.
pub const MAX_TABS: usize = 9;

/// How many sessions a tab can split off into panes of their own.
pub const MAX_SPLITS: usize = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tab {
    /// What the user called the tab. Empty until they do: the tab bar
    /// shows only its number then.
    pub name: String,
    /// The session that was selected when the tab was last in front, by
    /// name, to select again when it comes back. While the tab is in front,
    /// the sidebar's selection is what counts, and this waits to be written
    /// as the tab is left.
    pub selected: Option<String>,
    /// The sessions split off into panes of their own, by name, in the
    /// order they were split off.
    pub splits: Vec<String>,
}

/// Every tab, and which one is in front. There's always at least one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tabs {
    tabs: Vec<Tab>,
    /// An index into `tabs`.
    current: usize,
}

impl Default for Tabs {
    fn default() -> Tabs {
        Tabs {
            tabs: vec![Tab::default()],
            current: 0,
        }
    }
}

impl Tabs {
    pub fn all(&self) -> &[Tab] {
        &self.tabs
    }

    /// Where the tab in front is among them all, counted from 0.
    pub fn current_index(&self) -> usize {
        self.current
    }

    pub fn current(&self) -> &Tab {
        &self.tabs[self.current]
    }

    pub fn current_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.current]
    }

    /// Adds a tab after the others, on the session called `selected` with
    /// nothing split off, and brings it to the front. Says whether there
    /// was room for it.
    pub fn add(&mut self, selected: Option<String>) -> bool {
        if self.tabs.len() >= MAX_TABS {
            return false;
        }
        self.tabs.push(Tab {
            selected,
            ..Tab::default()
        });
        self.current = self.tabs.len() - 1;
        true
    }

    /// Brings the tab at `index` to the front, if there is one there. Says
    /// whether there was.
    pub fn go_to(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return false;
        }
        self.current = index;
        true
    }

    /// The tab after the one in front, round from the last to the first.
    pub fn next(&self) -> usize {
        (self.current + 1) % self.tabs.len()
    }

    /// The tab before the one in front, round from the first to the last.
    pub fn previous(&self) -> usize {
        (self.current + self.tabs.len() - 1) % self.tabs.len()
    }

    /// Closes the tab in front, and brings the one that takes its place to
    /// the front: the one after it, or, if it was the last, the one before.
    /// The only tab stays open; says whether it closed.
    pub fn close(&mut self) -> bool {
        if self.tabs.len() == 1 {
            return false;
        }
        self.tabs.remove(self.current);
        self.current = self.current.min(self.tabs.len() - 1);
        true
    }

    /// Names the tab in front. An empty name takes it back to its number.
    pub fn rename(&mut self, name: &str) {
        self.current_mut().name = name.trim().to_string();
    }

    /// The session called `from` is called `to` now: every tab that had it
    /// keeps it.
    pub fn renamed(&mut self, from: &str, to: &str) {
        for tab in &mut self.tabs {
            let names = tab.splits.iter_mut().chain(tab.selected.as_mut());
            for name in names.filter(|name| name.as_str() == from) {
                *name = to.to_string();
            }
        }
    }

    /// Closes, in every tab, the splits of sessions that `exists` says are
    /// gone.
    pub fn forget_gone(&mut self, exists: impl Fn(&str) -> bool) {
        for tab in &mut self.tabs {
            tab.splits.retain(|name| exists(name));
        }
    }

    /// Tabs as read from a file, put right where they couldn't have been
    /// written that way: one tab at least and [`MAX_TABS`] at most, each
    /// with [`MAX_SPLITS`] splits at most, and the one in front among them.
    fn checked(mut self) -> Tabs {
        self.tabs.truncate(MAX_TABS);
        if self.tabs.is_empty() {
            return Tabs::default();
        }
        for tab in &mut self.tabs {
            tab.splits.truncate(MAX_SPLITS);
        }
        self.current = self.current.min(self.tabs.len() - 1);
        self
    }
}

/// Where the TUI of the daemon at `socket` keeps its tabs: beside what the
/// new-session panel remembers.
pub fn path(socket: &Path) -> PathBuf {
    crate::state::path(socket).with_file_name("tabs.json")
}

/// The tabs kept at `path`. When there are none, or they can't be read, it's
/// one tab with nothing split off: losing a layout is no reason not to start.
pub fn load(path: &Path) -> Tabs {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Tabs>(&text).ok())
        .map(Tabs::checked)
        .unwrap_or_default()
}

/// Keeps `tabs` at `path`, if it can.
pub fn save(path: &Path, tabs: &Tabs) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(tabs) {
        let _ = std::fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(tabs: &Tabs) -> Vec<&str> {
        tabs.all().iter().map(|tab| tab.name.as_str()).collect()
    }

    /// Three tabs, named a, b and c, with the last one in front.
    fn three_tabs() -> Tabs {
        let mut tabs = Tabs::default();
        tabs.rename("a");
        for name in ["b", "c"] {
            tabs.add(None);
            tabs.rename(name);
        }
        tabs
    }

    #[test]
    fn there_is_one_tab_to_start_with_and_nothing_split_off_in_it() {
        let tabs = Tabs::default();
        assert_eq!(tabs.all().len(), 1);
        assert_eq!(tabs.current_index(), 0);
        assert!(tabs.current().splits.is_empty());
    }

    #[test]
    fn a_new_tab_goes_after_the_others_on_the_session_given_and_comes_to_the_front() {
        let mut tabs = Tabs::default();
        tabs.current_mut().splits.push("helper".into());
        assert!(tabs.add(Some("agent".into())));
        assert_eq!(tabs.current_index(), 1);
        assert_eq!(tabs.current().selected.as_deref(), Some("agent"));
        assert!(tabs.current().splits.is_empty());
        assert_eq!(tabs.all()[0].splits, ["helper"], "the first keeps its own");
    }

    #[test]
    fn nine_tabs_at_most() {
        let mut tabs = Tabs::default();
        for _ in 1..MAX_TABS {
            assert!(tabs.add(None));
        }
        assert!(!tabs.add(None));
        assert_eq!(tabs.all().len(), MAX_TABS);
    }

    #[test]
    fn going_to_a_tab_that_isnt_there_stays_put() {
        let mut tabs = three_tabs();
        assert!(tabs.go_to(0));
        assert_eq!(tabs.current().name, "a");
        assert!(!tabs.go_to(3));
        assert_eq!(tabs.current().name, "a");
    }

    #[test]
    fn next_and_previous_go_round() {
        let mut tabs = three_tabs();
        assert_eq!(tabs.next(), 0);
        assert_eq!(tabs.previous(), 1);
        tabs.go_to(0);
        assert_eq!(tabs.previous(), 2);
        assert_eq!(tabs.next(), 1);
    }

    #[test]
    fn closing_a_tab_brings_the_next_to_the_front_or_else_the_one_before() {
        let mut tabs = three_tabs();
        tabs.go_to(1);
        assert!(tabs.close());
        assert_eq!(names(&tabs), ["a", "c"]);
        assert_eq!(tabs.current().name, "c");

        assert!(tabs.close());
        assert_eq!(tabs.current().name, "a");
    }

    #[test]
    fn the_only_tab_stays_open() {
        let mut tabs = Tabs::default();
        tabs.rename("only");
        assert!(!tabs.close());
        assert_eq!(names(&tabs), ["only"]);
    }

    #[test]
    fn a_name_is_trimmed_and_an_empty_one_takes_the_tab_back_to_its_number() {
        let mut tabs = Tabs::default();
        tabs.rename("  review ");
        assert_eq!(tabs.current().name, "review");
        tabs.rename("  ");
        assert_eq!(tabs.current().name, "");
    }

    #[test]
    fn a_renamed_session_stays_in_every_tab_that_had_it() {
        let mut tabs = Tabs::default();
        tabs.current_mut().splits = vec!["old".into(), "other".into()];
        tabs.add(Some("old".into()));
        tabs.renamed("old", "new");
        assert_eq!(tabs.all()[0].splits, ["new", "other"]);
        assert_eq!(tabs.all()[1].selected.as_deref(), Some("new"));
    }

    #[test]
    fn a_session_that_has_gone_leaves_the_splits_of_every_tab() {
        let mut tabs = Tabs::default();
        tabs.current_mut().splits = vec!["gone".into(), "here".into()];
        tabs.add(None);
        tabs.current_mut().splits = vec!["gone".into()];
        tabs.forget_gone(|name| name == "here");
        assert_eq!(tabs.all()[0].splits, ["here"]);
        assert!(tabs.all()[1].splits.is_empty());
    }

    #[test]
    fn tabs_kept_on_disk_come_back_as_they_were() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/tabs.json");
        let mut tabs = three_tabs();
        tabs.current_mut().splits = vec!["server".into()];
        tabs.current_mut().selected = Some("agent".into());
        tabs.go_to(1);
        save(&path, &tabs);
        assert_eq!(load(&path), tabs);
    }

    #[test]
    fn with_nothing_to_read_there_is_one_tab() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("tabs.json");
        assert_eq!(load(&missing), Tabs::default());

        std::fs::write(&missing, "not json").unwrap();
        assert_eq!(load(&missing), Tabs::default());

        std::fs::write(&missing, r#"{"tabs": []}"#).unwrap();
        assert_eq!(load(&missing), Tabs::default());
    }

    #[test]
    fn tabs_read_from_a_file_are_put_right() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tabs.json");
        let tab = r#"{"name": "x", "splits": ["a", "b", "c"]}"#;
        let many = [tab; 12].join(",");
        std::fs::write(&path, format!(r#"{{"tabs": [{many}], "current": 20}}"#)).unwrap();
        let tabs = load(&path);
        assert_eq!(tabs.all().len(), MAX_TABS);
        assert_eq!(tabs.current_index(), MAX_TABS - 1);
        assert_eq!(tabs.current().splits, ["a", "b"]);
    }
}
