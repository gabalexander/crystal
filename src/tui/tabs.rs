//! Tabs: each one a space of its own, holding its own sessions. The sidebar
//! lists the sessions of the tab in front, and the panes beside it are that
//! tab's: a tree of splits, one pane following the selection and the
//! others each showing a session of its own. Going to another tab changes
//! both.
//!
//! Every session is in exactly one tab. A session no tab holds yet, like one
//! started from the command line or by another TUI, joins the tab in front,
//! or whichever tab the caller says it belongs in.
//!
//! The event loop keeps the tabs in the database, so they're there again
//! the next time the TUI opens. Nothing here does any I/O.

use super::split_tree::{Pane, SplitTree, Way};
use serde::{Deserialize, Serialize};

/// How many tabs there can be: one for each of the keys 1 to 9.
pub const MAX_TABS: usize = 9;

/// Which shape the tabs are kept in. Tabs kept in the shape before, when a
/// tab's panes were a list, are read too, the list made a tree; those of
/// any other shape, like the one tabs were kept in before they held their
/// own sessions, are ignored rather than half read.
const VERSION: u32 = 3;

/// The shape before [`VERSION`]: a tab's splits listed in the order their
/// panes were drawn.
const LISTED: u32 = 2;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
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
    /// The panes beside the sidebar, and how they split the room: one
    /// follows the selection, and each of the others shows one of the
    /// tab's own sessions, split off.
    pub panes: SplitTree,
    /// The session the selection's pane went on showing when the selection
    /// moved to one with a pane of its own: the last it showed.
    pub shown: Option<String>,
    /// Whether the selected session's pane takes all the room between the
    /// top bar and the footer, the sidebar and the other panes put away.
    pub zoomed: bool,
    /// The session floating over the panes, in a pane of its own, if one
    /// is. One of the tab's own sessions, and never one split off too.
    pub floating: Option<String>,
}

impl Tab {
    /// Names the tab. An empty name takes it back to its number.
    pub fn rename(&mut self, name: &str) {
        self.name = name.trim().to_string();
    }

    /// Whether the session called `name` is in this tab.
    pub fn holds(&self, name: &str) -> bool {
        self.sessions.iter().any(|held| held == name)
    }

    /// The sessions split off into panes of their own, in the order their
    /// panes are drawn.
    pub fn splits(&self) -> Vec<&str> {
        self.panes.sessions()
    }

    /// Whether the session called `name` is split off into a pane of its
    /// own.
    pub fn is_split(&self, name: &str) -> bool {
        self.panes.contains(&Pane::Session(name.to_string()))
    }

    /// Closes the split of the session called `name`, if it has one: the
    /// pane beside it takes the room.
    pub fn close_split(&mut self, name: &str) {
        self.panes.close(&Pane::Session(name.to_string()));
    }

    /// Takes the session called `name` out of the tab, and out of its
    /// panes and its selection with it.
    fn let_go(&mut self, name: &str) {
        self.sessions.retain(|held| held != name);
        self.close_split(name);
        for kept in [&mut self.selected, &mut self.shown, &mut self.floating] {
            if kept.as_deref() == Some(name) {
                *kept = None;
            }
        }
    }

    /// Lets go of the sessions that aren't among `names`.
    fn keep_only(&mut self, names: &[&str]) {
        self.sessions.retain(|held| names.contains(&held.as_str()));
        self.panes.retain(|split| names.contains(&split));
        for kept in [&mut self.shown, &mut self.floating] {
            if kept.as_deref().is_some_and(|name| !names.contains(&name)) {
                *kept = None;
            }
        }
    }
}

/// Every tab, and which one is in front. There's always at least one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "KeptTabs")]
pub struct Tabs {
    /// [`VERSION`] when kept by this crystal, or read in the shape before.
    /// Tabs kept without one read as 0, not as the default tabs' version,
    /// so old ones aren't taken for new.
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

    /// The tab at `index`, which has to be one of them.
    pub fn tab_mut(&mut self, index: usize) -> &mut Tab {
        &mut self.tabs[index]
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
        self.close_at(self.current)
    }

    /// Closes the tab at `index`, as [`Tabs::close`] does the one in front.
    /// Closing another, the one in front stays in front.
    pub fn close_at(&mut self, index: usize) -> bool {
        if self.tabs.len() == 1 || index >= self.tabs.len() {
            return false;
        }
        self.tabs.remove(index);
        if index < self.current {
            self.current -= 1;
        }
        self.current = self.current.min(self.tabs.len() - 1);
        true
    }

    /// Names the tab in front. An empty name takes it back to its number.
    pub fn rename(&mut self, name: &str) {
        self.current_mut().rename(name);
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
            tab.panes.replace(
                &Pane::Session(from.to_string()),
                Pane::Session(to.to_string()),
            );
            let names = tab
                .sessions
                .iter_mut()
                .chain(tab.selected.as_mut())
                .chain(tab.shown.as_mut())
                .chain(tab.floating.as_mut());
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
            tab.keep_only(names);
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

    /// Tabs as read back, if they were kept in a shape this crystal reads,
    /// and put right: see [`Tabs::checked`].
    pub fn kept(self) -> Option<Tabs> {
        (self.version == VERSION).then(|| self.checked())
    }

    /// Every session the tabs hold, by name.
    pub fn sessions(&self) -> impl Iterator<Item = &str> {
        let tabs = self.tabs.iter();
        tabs.flat_map(|tab| tab.sessions.iter().map(String::as_str))
    }

    /// Tabs as read back, put right where they couldn't have been kept that
    /// way: one tab at least and [`MAX_TABS`] at most, each
    /// with panes of its own sessions only, each in one pane, a float of its
    /// own that isn't split off too, and one pane following the selection;
    /// no session in two tabs, and the one in front among them.
    fn checked(mut self) -> Tabs {
        self.tabs.truncate(MAX_TABS);
        if self.tabs.is_empty() {
            return Tabs::default();
        }
        let mut seen: Vec<String> = Vec::new();
        for tab in &mut self.tabs {
            tab.sessions.retain(|name| !seen.contains(name));
            seen.extend(tab.sessions.iter().cloned());
            let sessions = tab.sessions.clone();
            let floating = tab.floating.take().filter(|name| sessions.contains(name));
            let mut split: Vec<String> = Vec::new();
            tab.panes.retain(|name| {
                let keep = sessions.iter().any(|held| held == name)
                    && floating.as_deref() != Some(name)
                    && !split.iter().any(|seen| seen == name);
                split.push(name.to_string());
                keep
            });
            tab.floating = floating;
        }
        self.current = self.current.min(self.tabs.len() - 1);
        self
    }
}

/// Tabs as they were kept, in this crystal's shape or the one before.
#[derive(Deserialize, Default)]
#[serde(default)]
struct KeptTabs {
    version: u32,
    tabs: Vec<KeptTab>,
    current: usize,
}

/// A tab as it was kept. Before a tab's panes were a tree, it listed
/// the sessions split off in the order their panes were drawn, and where
/// the pane that follows the selection was among them.
#[derive(Deserialize, Default)]
#[serde(default)]
struct KeptTab {
    name: String,
    sessions: Vec<String>,
    selected: Option<String>,
    panes: Option<SplitTree>,
    shown: Option<String>,
    zoomed: bool,
    floating: Option<String>,
    splits: Vec<String>,
    selection_at: usize,
}

impl From<KeptTabs> for Tabs {
    fn from(kept: KeptTabs) -> Tabs {
        let version = if kept.version == LISTED {
            VERSION
        } else {
            kept.version
        };
        Tabs {
            version,
            tabs: kept.tabs.into_iter().map(Tab::from).collect(),
            current: kept.current,
        }
    }
}

impl From<KeptTab> for Tab {
    fn from(kept: KeptTab) -> Tab {
        let panes = match kept.panes {
            Some(panes) => panes,
            None => listed(kept.splits, kept.selection_at),
        };
        Tab {
            name: kept.name,
            sessions: kept.sessions,
            selected: kept.selected,
            panes,
            shown: kept.shown,
            zoomed: kept.zoomed,
            floating: kept.floating,
        }
    }
}

/// The panes of a tab kept as a list, as a tree: the pane that follows the
/// selection put back among the splits, all evenly in a line, the way they
/// were most likely drawn. Two went side by side, on a screen wide enough
/// for both, and more were stacked.
fn listed(splits: Vec<String>, selection_at: usize) -> SplitTree {
    let mut panes: Vec<Pane> = splits.into_iter().map(Pane::Session).collect();
    panes.insert(selection_at.min(panes.len()), Pane::Selection);
    let way = if panes.len() <= 2 {
        Way::Right
    } else {
        Way::Down
    };
    SplitTree::in_line(panes, way)
}

/// The tabs kept as `json`. When there are none, or they can't be read, or
/// they were kept in another shape, it's one tab: losing a layout is no
/// reason not to start, and every session joins that tab.
pub fn read(json: Option<&str>) -> Tabs {
    json.and_then(|json| serde_json::from_str::<Tabs>(json).ok())
        .and_then(Tabs::kept)
        .unwrap_or_default()
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

    /// Splits the session called `name` off beside the selection's pane in
    /// the tab in front.
    fn split_off(tabs: &mut Tabs, name: &str) {
        let panes = &mut tabs.current_mut().panes;
        assert!(panes.split(
            &Pane::Selection,
            Way::Right,
            0.5,
            Pane::Session(name.into())
        ));
    }

    #[test]
    fn there_is_one_empty_tab_to_start_with() {
        let tabs = Tabs::default();
        assert_eq!(tabs.all().len(), 1);
        assert_eq!(tabs.current_index(), 0);
        assert!(tabs.current().sessions.is_empty());
        assert_eq!(tabs.current().panes, SplitTree::default());
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
    fn closing_another_tab_leaves_the_one_in_front_in_front() {
        let mut tabs = three_tabs();
        assert!(tabs.close_at(0));
        assert_eq!(names(&tabs), ["b", "c"]);
        assert_eq!(tabs.current().name, "c");
        assert!(tabs.close_at(1), "the one in front, as close does");
        assert_eq!(tabs.current().name, "b");
        assert!(!tabs.close_at(5));
        assert!(!tabs.close_at(0), "the only tab stays");
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
    fn a_session_moved_away_leaves_its_old_tab_s_panes_and_selection() {
        let mut tabs = Tabs::default();
        tabs.put("server", 0);
        split_off(&mut tabs, "server");
        tabs.current_mut().selected = Some("server".into());
        tabs.current_mut().shown = Some("server".into());
        tabs.add();
        tabs.put("server", 1);
        assert!(tabs.current().splits().is_empty());
        assert_eq!(tabs.current().panes, SplitTree::default());
        assert_eq!(tabs.current().selected, None);
        assert_eq!(tabs.current().shown, None);
        assert_eq!(tabs.all()[1].sessions, ["server"]);
    }

    #[test]
    fn a_renamed_session_stays_in_its_tab_split_off_and_selected() {
        let mut tabs = Tabs::default();
        tabs.put("old", 0);
        split_off(&mut tabs, "old");
        tabs.current_mut().selected = Some("old".into());
        tabs.renamed("old", "new");
        assert_eq!(tabs.current().sessions, ["new"]);
        assert_eq!(tabs.current().splits(), ["new"]);
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
    fn sessions_that_have_gone_leave_their_tabs_and_panes() {
        let mut tabs = Tabs::default();
        tabs.put("gone", 0);
        tabs.put("here", 0);
        split_off(&mut tabs, "gone");
        split_off(&mut tabs, "here");
        tabs.current_mut().shown = Some("gone".into());
        tabs.take_in(&["here"], |_| None);
        assert_eq!(in_front(&tabs), ["here"]);
        assert_eq!(tabs.current().splits(), ["here"]);
        assert_eq!(tabs.current().shown, None);
    }

    #[test]
    fn tabs_kept_come_back_as_they_were() {
        let mut tabs = three_tabs();
        tabs.put("agent", 2);
        tabs.put("server", 2);
        tabs.put("logs", 2);
        split_off(&mut tabs, "server");
        let panes = &mut tabs.current_mut().panes;
        panes.split(
            &Pane::Session("server".into()),
            Way::Down,
            0.3,
            Pane::Session("logs".into()),
        );
        tabs.current_mut().selected = Some("agent".into());
        tabs.current_mut().shown = Some("agent".into());
        tabs.current_mut().zoomed = true;
        tabs.current_mut().floating = Some("agent".into());
        tabs.go_to(1);
        let json = serde_json::to_string(&tabs).unwrap();
        assert_eq!(read(Some(&json)), tabs);
    }

    #[test]
    fn tabs_kept_before_they_could_zoom_come_back_unzoomed() {
        let kept = r#"{"version": 2, "tabs": [{"name": "a", "sessions": ["x"], "splits": []}]}"#;
        let tabs = read(Some(kept));
        assert_eq!(tabs.current().sessions, ["x"]);
        assert!(!tabs.current().zoomed);
    }

    /// The panes of the tab in front of the tabs kept as `json`, by name,
    /// `*` for the selection's, in the order drawn.
    fn panes_read(json: &str) -> Vec<String> {
        let tabs = read(Some(json));
        let panes = tabs.current().panes.panes().into_iter();
        panes
            .map(|pane| match pane {
                Pane::Selection => "*".to_string(),
                Pane::Session(name) => name.clone(),
            })
            .collect()
    }

    #[test]
    fn panes_kept_as_a_list_come_back_as_a_tree_in_the_same_order() {
        let kept = r#"{"version": 2, "tabs": [{"sessions": ["x", "y", "z"], "splits": ["x", "y"],
            "selection_at": 1}]}"#;
        assert_eq!(panes_read(kept), ["x", "*", "y"]);

        // Before panes could move, the selection's pane was always first;
        // one past the panes there are is put back after the last.
        let kept = r#"{"version": 2, "tabs": [{"sessions": ["x", "y"], "splits": ["y"]}]}"#;
        assert_eq!(panes_read(kept), ["*", "y"]);
        let kept = r#"{"version": 2, "tabs": [{"sessions": ["x", "y"], "splits": ["y"],
            "selection_at": 7}]}"#;
        assert_eq!(panes_read(kept), ["y", "*"]);
    }

    #[test]
    fn panes_kept_as_a_list_go_side_by_side_two_at_a_time_and_stacked_beyond() {
        let room = ratatui::layout::Rect::new(0, 0, 101, 40);
        let two = read(Some(
            r#"{"version": 2, "tabs": [{"sessions": ["x"], "splits": ["x"]}]}"#,
        ));
        let areas: Vec<_> = (two.current().panes.layout(room).into_iter())
            .map(|(_, area)| area)
            .collect();
        assert_eq!(areas[0], ratatui::layout::Rect::new(0, 0, 50, 40));
        assert_eq!(areas[1], ratatui::layout::Rect::new(51, 0, 50, 40));

        let three = read(Some(
            r#"{"version": 2, "tabs": [{"sessions": ["x", "y"], "splits": ["x", "y"]}]}"#,
        ));
        let heights: Vec<u16> = (three.current().panes.layout(room).into_iter())
            .map(|(_, area)| area.height)
            .collect();
        assert_eq!(heights, [13, 14, 13]);
        // Kept again, they're in this crystal's shape.
        let json = serde_json::to_string(&three).unwrap();
        assert!(json.contains(r#""version":3"#), "{json}");
        assert!(!json.contains("splits"), "{json}");
        assert_eq!(read(Some(&json)), three);
    }

    #[test]
    fn a_float_follows_its_session_through_a_rename_and_goes_with_it() {
        let mut tabs = Tabs::default();
        tabs.put("old", 0);
        tabs.current_mut().floating = Some("old".into());
        tabs.renamed("old", "new");
        assert_eq!(tabs.current().floating.as_deref(), Some("new"));
        tabs.take_in(&[], |_| None);
        assert_eq!(tabs.current().floating, None);
    }

    #[test]
    fn a_float_read_back_is_one_of_its_tabs_sessions_and_not_split_too() {
        let kept = r#"{"version": 2, "tabs": [{"sessions": ["x", "y"], "splits": ["x", "y"],
            "floating": "x"}, {"sessions": ["z"], "floating": "y"}]}"#;
        assert_eq!(panes_read(kept), ["*", "y"]);
        let tabs = read(Some(kept));
        assert_eq!(tabs.all()[0].floating.as_deref(), Some("x"));
        assert_eq!(tabs.all()[1].floating, None);
    }

    #[test]
    fn with_nothing_to_read_there_is_one_tab() {
        assert_eq!(read(None), Tabs::default());
        assert_eq!(read(Some("not json")), Tabs::default());
        assert_eq!(read(Some(r#"{"version": 3, "tabs": []}"#)), Tabs::default());
    }

    #[test]
    fn tabs_kept_before_they_held_sessions_are_ignored() {
        let old = r#"{"tabs": [{"name": "one", "selected": "claude-2", "splits": []},
            {"name": "", "selected": "claude-2", "splits": []}], "current": 1}"#;
        assert_eq!(read(Some(old)), Tabs::default());
    }

    #[test]
    fn tabs_read_back_are_put_right() {
        let tab = r#"{"name": "x", "sessions": ["a", "b", "c"], "panes": {"way": "right",
            "ratio": 0.5, "first": {"session": "d"}, "second": {"way": "down", "ratio": 0.5,
            "first": {"session": "a"}, "second": {"session": "a"}}}}"#;
        let many = [tab; 12].join(",");
        let text = format!(r#"{{"version": 3, "tabs": [{many}], "current": 20}}"#);
        let tabs = read(Some(&text));
        assert_eq!(tabs.all().len(), MAX_TABS);
        assert_eq!(tabs.current_index(), MAX_TABS - 1);
        assert_eq!(tabs.all()[0].sessions, ["a", "b", "c"]);
        // d isn't the tab's, a is in one pane only, and the selection's
        // pane, missing, is put beside them.
        assert_eq!(tabs.all()[0].splits(), ["a"]);
        assert!(tabs.all()[0].panes.contains(&Pane::Selection));
        // Each session stays in the first tab that had it.
        assert!(tabs.all()[1].sessions.is_empty());
        assert!(tabs.all()[1].splits().is_empty());
    }
}
