//! Tabs: each one a space of its own, holding its own sessions. The sidebar
//! lists the sessions of the tab in front, and the panes beside it show that
//! tab's selection and splits, so going to another tab changes both.
//!
//! Every session is in exactly one tab. A session no tab holds yet, like one
//! started from the command line or by another TUI, joins the tab in front,
//! or whichever tab the caller says it belongs in.
//!
//! The tabs are kept in a file beside the daemon's state, so they're there
//! again the next time the TUI opens. Nothing else here does any I/O.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How many tabs there can be: one for each of the keys 1 to 9.
pub const MAX_TABS: usize = 9;

/// How many sessions a tab can split off into panes of their own.
pub const MAX_SPLITS: usize = 2;

/// Which shape of file [`save`] writes. A file of any other shape, like
/// the one tabs were kept in before they held their own sessions, is
/// ignored rather than half read.
const VERSION: u32 = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tab {
    /// What the user called the tab. Empty until they do: the tab bar
    /// shows only its number then.
    pub name: String,
    /// The sessions in the tab, by name. Names, not ids: a session's id
    /// changes when the daemon restarts, and its name doesn't.
    pub sessions: Vec<String>,
    /// The session that was selected when the tab was last in front, to
    /// select again when it comes back. While the tab is in front, the
    /// sidebar's selection is what counts, and this waits to be written as
    /// the tab is left.
    pub selected: Option<String>,
    /// The sessions split off into panes of their own, in the order they
    /// were split off. Only the tab's own sessions.
    pub splits: Vec<String>,
}

impl Tab {
    /// Whether the session called `name` is in this tab.
    pub fn holds(&self, name: &str) -> bool {
        self.sessions.iter().any(|held| held == name)
    }

    /// Takes the session called `name` out of the tab, and out of its
    /// splits and its selection with it.
    fn let_go(&mut self, name: &str) {
        self.sessions.retain(|held| held != name);
        self.splits.retain(|split| split != name);
        if self.selected.as_deref() == Some(name) {
            self.selected = None;
        }
    }
}

/// Every tab, and which one is in front. There's always at least one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tabs {
    /// [`VERSION`] when written by [`save`]. A file without one reads as 0,
    /// not as the default tabs' version, so an old file isn't taken for a
    /// new one.
    #[serde(default)]
    version: u32,
    tabs: Vec<Tab>,
    /// An index into `tabs`.
    current: usize,
}

impl Default for Tabs {
    fn default() -> Tabs {
        Tabs {
            version: VERSION,
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

    /// Adds an empty tab after the others, and says where it went: `None`
    /// when there's no room for another. The tab in front stays in front.
    pub fn add(&mut self) -> Option<usize> {
        if self.tabs.len() >= MAX_TABS {
            return None;
        }
        self.tabs.push(Tab::default());
        Some(self.tabs.len() - 1)
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
    /// The only tab stays open; says whether it closed. The sessions it
    /// held are in no tab now, for the caller to end.
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

    /// Where the session called `name` is: the index of the tab holding it.
    pub fn tab_of(&self, name: &str) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.holds(name))
    }

    /// Puts the session called `name` in the tab at `index`, taking it out
    /// of the tab it was in. Says whether there's a tab there.
    pub fn put(&mut self, name: &str, index: usize) -> bool {
        if index >= self.tabs.len() {
            return false;
        }
        for tab in &mut self.tabs {
            tab.let_go(name);
        }
        self.tabs[index].sessions.push(name.to_string());
        true
    }

    /// The session called `from` is called `to` now: the tab that had it
    /// keeps it, split off and selected if it was.
    pub fn renamed(&mut self, from: &str, to: &str) {
        for tab in &mut self.tabs {
            let names = tab
                .sessions
                .iter_mut()
                .chain(tab.splits.iter_mut())
                .chain(tab.selected.as_mut());
            for name in names.filter(|name| name.as_str() == from) {
                *name = to.to_string();
            }
        }
    }

    /// Takes the names of every session there is now: those that have
    /// gone leave their tabs, and those no tab holds yet join one, the tab
    /// `home` says they belong in or else the tab in front.
    pub fn take_in(&mut self, names: &[&str], home: impl Fn(&str) -> Option<usize>) {
        for tab in &mut self.tabs {
            tab.sessions.retain(|held| names.contains(&held.as_str()));
            tab.splits.retain(|split| names.contains(&split.as_str()));
        }
        for name in names {
            if self.tab_of(name).is_none() {
                let index = home(name)
                    .filter(|&index| index < self.tabs.len())
                    .unwrap_or(self.current);
                self.tabs[index].sessions.push(name.to_string());
            }
        }
    }

    /// Tabs as read from a file, put right where they couldn't have been
    /// written that way: one tab at least and [`MAX_TABS`] at most, each
    /// with [`MAX_SPLITS`] splits at most of its own sessions, no session
    /// in two tabs, and the one in front among them.
    fn checked(mut self) -> Tabs {
        self.tabs.truncate(MAX_TABS);
        if self.tabs.is_empty() {
            return Tabs::default();
        }
        let mut seen: Vec<String> = Vec::new();
        for tab in &mut self.tabs {
            tab.sessions.retain(|name| !seen.contains(name));
            seen.extend(tab.sessions.iter().cloned());
            let sessions = &tab.sessions;
            tab.splits.retain(|split| sessions.contains(split));
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

/// The tabs kept at `path`. When there are none, or they can't be read, or
/// they were written in another shape, it's one tab: losing a layout is no
/// reason not to start, and every session joins that tab.
pub fn load(path: &Path) -> Tabs {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Tabs>(&text).ok())
        .filter(|tabs| tabs.version == VERSION)
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
            let added = tabs.add().unwrap();
            tabs.go_to(added);
            tabs.rename(name);
        }
        tabs
    }

    /// The tab in front's sessions.
    fn in_front(tabs: &Tabs) -> Vec<&str> {
        let held = &tabs.current().sessions;
        held.iter().map(String::as_str).collect()
    }

    #[test]
    fn there_is_one_empty_tab_to_start_with() {
        let tabs = Tabs::default();
        assert_eq!(tabs.all().len(), 1);
        assert_eq!(tabs.current_index(), 0);
        assert!(tabs.current().sessions.is_empty());
    }

    #[test]
    fn a_new_tab_goes_after_the_others_empty_and_the_one_in_front_stays() {
        let mut tabs = Tabs::default();
        tabs.put("agent", 0);
        assert_eq!(tabs.add(), Some(1));
        assert_eq!(tabs.current_index(), 0);
        assert!(tabs.all()[1].sessions.is_empty());
        assert_eq!(in_front(&tabs), ["agent"]);
    }

    #[test]
    fn nine_tabs_at_most() {
        let mut tabs = Tabs::default();
        for _ in 1..MAX_TABS {
            assert!(tabs.add().is_some());
        }
        assert_eq!(tabs.add(), None);
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
    fn a_closed_tab_s_sessions_are_in_no_tab() {
        let mut tabs = three_tabs();
        tabs.put("server", 2);
        tabs.put("logs", 2);
        assert!(tabs.close());
        assert_eq!(tabs.tab_of("server"), None);
        assert_eq!(tabs.tab_of("logs"), None);
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
    fn a_session_is_in_one_tab_at_a_time() {
        let mut tabs = three_tabs();
        tabs.put("agent", 0);
        assert_eq!(tabs.tab_of("agent"), Some(0));
        tabs.put("agent", 1);
        assert_eq!(tabs.tab_of("agent"), Some(1));
        assert!(!tabs.all()[0].holds("agent"));
        assert!(!tabs.put("agent", 7), "there's no tab 8");
        assert_eq!(tabs.tab_of("agent"), Some(1));
    }

    #[test]
    fn a_session_moved_away_leaves_its_old_tab_s_splits_and_selection() {
        let mut tabs = Tabs::default();
        tabs.put("server", 0);
        tabs.current_mut().splits = vec!["server".into()];
        tabs.current_mut().selected = Some("server".into());
        tabs.add();
        tabs.put("server", 1);
        assert!(tabs.current().splits.is_empty());
        assert_eq!(tabs.current().selected, None);
        assert_eq!(tabs.all()[1].sessions, ["server"]);
    }

    #[test]
    fn a_renamed_session_stays_in_its_tab_split_off_and_selected() {
        let mut tabs = Tabs::default();
        tabs.put("old", 0);
        tabs.current_mut().splits = vec!["old".into()];
        tabs.current_mut().selected = Some("old".into());
        tabs.renamed("old", "new");
        assert_eq!(tabs.current().sessions, ["new"]);
        assert_eq!(tabs.current().splits, ["new"]);
        assert_eq!(tabs.current().selected.as_deref(), Some("new"));
    }

    #[test]
    fn sessions_no_tab_holds_join_the_tab_in_front() {
        let mut tabs = three_tabs();
        tabs.go_to(1);
        tabs.put("agent", 0);
        tabs.take_in(&["agent", "new"], |_| None);
        assert_eq!(tabs.tab_of("agent"), Some(0), "it stays where it was");
        assert_eq!(tabs.tab_of("new"), Some(1));
        assert_eq!(tabs.all()[1].sessions, ["new"]);
    }

    #[test]
    fn a_session_joins_the_tab_it_belongs_in_when_there_is_one() {
        let mut tabs = three_tabs();
        let home = |name: &str| (name == "step-2").then_some(0);
        tabs.take_in(&["step-2", "other"], home);
        assert_eq!(tabs.tab_of("step-2"), Some(0));
        assert_eq!(tabs.tab_of("other"), Some(2));
        // A tab that isn't there is no home.
        tabs.take_in(&["step-2", "other", "lost"], |_| Some(8));
        assert_eq!(tabs.tab_of("lost"), Some(2));
    }

    #[test]
    fn sessions_that_have_gone_leave_their_tabs_and_splits() {
        let mut tabs = Tabs::default();
        tabs.put("gone", 0);
        tabs.put("here", 0);
        tabs.current_mut().splits = vec!["gone".into(), "here".into()];
        tabs.take_in(&["here"], |_| None);
        assert_eq!(in_front(&tabs), ["here"]);
        assert_eq!(tabs.current().splits, ["here"]);
    }

    #[test]
    fn tabs_kept_on_disk_come_back_as_they_were() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/tabs.json");
        let mut tabs = three_tabs();
        tabs.put("agent", 2);
        tabs.put("server", 2);
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

        std::fs::write(&missing, r#"{"version": 2, "tabs": []}"#).unwrap();
        assert_eq!(load(&missing), Tabs::default());
    }

    #[test]
    fn tabs_kept_before_they_held_sessions_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tabs.json");
        let old = r#"{"tabs": [{"name": "one", "selected": "claude-2", "splits": []},
            {"name": "", "selected": "claude-2", "splits": []}], "current": 1}"#;
        std::fs::write(&path, old).unwrap();
        assert_eq!(load(&path), Tabs::default());
    }

    #[test]
    fn tabs_read_from_a_file_are_put_right() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tabs.json");
        let tab = r#"{"name": "x", "sessions": ["a", "b", "c"], "splits": ["a", "b", "c", "d"]}"#;
        let many = [tab; 12].join(",");
        let text = format!(r#"{{"version": 2, "tabs": [{many}], "current": 20}}"#);
        std::fs::write(&path, text).unwrap();
        let tabs = load(&path);
        assert_eq!(tabs.all().len(), MAX_TABS);
        assert_eq!(tabs.current_index(), MAX_TABS - 1);
        assert_eq!(tabs.all()[0].sessions, ["a", "b", "c"]);
        assert_eq!(tabs.all()[0].splits, ["a", "b"]);
        // Each session stays in the first tab that had it.
        assert!(tabs.all()[1].sessions.is_empty());
        assert!(tabs.all()[1].splits.is_empty());
    }
}
