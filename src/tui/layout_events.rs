//! What changed in a TUI's tabs and panes, as events for plugins: a tab
//! made, closed, named, moved or brought to the front, a tab's panes
//! changed, a session moved to another tab, and the session the user's
//! selection settled on, and its project.
//!
//! Each is found by looking at the layout before and after, not where it's
//! changed, so a change made any way, a key, the mouse, a layout command or
//! a layout put back, is told the same. The event loop looks again once the
//! layout has held still for [`SETTLE`], and tells the daemon what changed
//! since it last told it: `j` held down through the sidebar, or a border
//! dragged across the screen, comes to one event, about where it ended.
//! With no TUI open, the daemon tells what a layout command changed. Kept
//! apart from I/O, so it's unit-tested.

use crate::events::{Event, Kind};
use crate::layout::{TabLayout, Tile};
use crate::protocol::SessionInfo;
use std::path::PathBuf;
use std::time::Duration;

/// How long the layout holds still before what changed in it is told.
pub const SETTLE: Duration = Duration::from_millis(300);

/// A TUI's tabs and the session the user is on, at one moment.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Look {
    /// Each tab, as `crystal layout` has it, with its id (see
    /// [`super::tabs::Tab::id`]), in their order.
    pub tabs: Vec<(u64, TabLayout)>,
    /// The session the user is on: the one selected in the tab in front.
    pub focused: Option<Focus>,
}

/// The session the user is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Focus {
    /// Its id, which stays the same as it's renamed.
    pub id: String,
    pub name: String,
    /// Its project's main worktree, when it runs in git.
    pub project: Option<PathBuf>,
}

impl Focus {
    pub fn of(session: &SessionInfo) -> Focus {
        Focus {
            id: session.id.clone(),
            name: session.name.clone(),
            project: session
                .worktree
                .as_ref()
                .map(|worktree| worktree.project_path.clone()),
        }
    }
}

impl Look {
    fn tab(&self, id: u64) -> Option<&TabLayout> {
        self.tabs
            .iter()
            .find(|(held, _)| *held == id)
            .map(|(_, tab)| tab)
    }

    /// The id of the tab in front.
    fn current(&self) -> Option<u64> {
        self.tabs
            .iter()
            .find(|(_, tab)| tab.current)
            .map(|(id, _)| *id)
    }

    /// The tab holding the session called `name`, with its id.
    fn holding(&self, name: &str) -> Option<(u64, &TabLayout)> {
        let tabs = self.tabs.iter();
        let mut found = tabs.filter(|(_, tab)| tab.sessions.iter().any(|held| held == name));
        found.next().map(|(id, tab)| (*id, tab))
    }
}

/// What changed from `before` to `after`, as events, `sessions` being the
/// sessions as they are now; and the look to tell the next change from.
/// The user is on the session they were on until the selection rests on
/// another, not on a heading or a worktree with nothing in it.
pub fn told(before: &Look, after: Look, sessions: &[SessionInfo]) -> (Vec<Event>, Look) {
    let events = changes(before, &after, sessions);
    let focused = after.focused.or_else(|| before.focused.clone());
    (events, Look { focused, ..after })
}

/// What changed from `before` to `after`, as events, in the order a reader
/// would want them: the tabs gone, then each tab made or named in their
/// order, and the user's focus last.
pub fn changes(before: &Look, after: &Look, sessions: &[SessionInfo]) -> Vec<Event> {
    let mut events = Vec::new();
    for (id, tab) in &before.tabs {
        if after.tab(*id).is_none() {
            events.push(Event::tab(Kind::TabClosed, tab.clone(), None));
        }
    }
    for (id, tab) in &after.tabs {
        match before.tab(*id) {
            None => events.push(Event::tab(Kind::TabCreated, tab.clone(), None)),
            Some(was) if was.name != tab.name => {
                let from = Some(was.name.clone());
                events.push(Event::tab(Kind::TabRenamed, tab.clone(), from));
            }
            Some(_) => {}
        }
    }
    for id in moved(before, after) {
        let (Some(was), Some(tab)) = (before.tab(id), after.tab(id)) else {
            continue;
        };
        let from = Some(was.number.to_string());
        events.push(Event::tab(Kind::TabMoved, tab.clone(), from));
    }
    let session = |name: &str| sessions.iter().find(|session| session.name == name);
    for (id, tab) in &after.tabs {
        for name in &tab.sessions {
            let was = before.holding(name).filter(|(was, _)| was != id);
            if let (Some((_, was)), Some(session)) = (was, session(name)) {
                let from = Some(was.number.to_string());
                events.push(Event::pane(Kind::PaneMoved, session, tab.clone(), from));
            }
        }
    }
    for (id, tab) in &after.tabs {
        let was = before.tab(*id);
        if was.is_some_and(|was| shape(was) != shape(tab)) {
            events.push(Event::tab(Kind::LayoutUpdated, tab.clone(), None));
        }
    }
    let front = after
        .current()
        .filter(|front| Some(*front) != before.current());
    if let Some(tab) = front.and_then(|front| after.tab(front)) {
        let was = before.current().and_then(|was| before.tab(was));
        let from = was.map(|was| was.number.to_string());
        events.push(Event::tab(Kind::TabFocused, tab.clone(), from));
    }
    let was = before.focused.as_ref();
    let now = after
        .focused
        .as_ref()
        .filter(|now| was.is_none_or(|was| was.id != now.id));
    if let Some(now) = now {
        let was_in = was.and_then(|was| was.project.as_ref());
        if let Some(project) = now.project.as_ref().filter(|now| Some(*now) != was_in) {
            let from = was_in.map(PathBuf::as_path);
            events.push(Event::project_focused(project.clone(), from));
        }
        let holding = after.holding(&now.name);
        if let (Some(session), Some((_, tab))) = (session(&now.name), holding) {
            let from = was.map(|was| was.name.clone());
            events.push(Event::pane(Kind::PaneFocused, session, tab.clone(), from));
        }
    }
    events
}

/// The ids of the tabs both looks have that moved: those out of the
/// longest run of them still in the order they were in.
fn moved(before: &Look, after: &Look) -> Vec<u64> {
    let kept: Vec<u64> = before
        .tabs
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| after.tab(*id).is_some())
        .collect();
    let order: Vec<(u64, usize)> = after
        .tabs
        .iter()
        .filter_map(|(id, _)| Some((*id, kept.iter().position(|held| held == id)?)))
        .collect();
    // The longest run in order, by the place each had before: for each
    // tab, the longest that ends with it, and the tab before it there.
    let mut longest: Vec<(usize, Option<usize>)> = Vec::with_capacity(order.len());
    for (at, (_, place)) in order.iter().enumerate() {
        let before_it = (0..at)
            .filter(|&earlier| order[earlier].1 < *place)
            .max_by_key(|&earlier| longest[earlier].0);
        longest.push(match before_it {
            Some(earlier) => (longest[earlier].0 + 1, Some(earlier)),
            None => (1, None),
        });
    }
    let mut in_order = vec![false; order.len()];
    let mut at = (0..order.len()).max_by_key(|&at| longest[at].0);
    while let Some(here) = at {
        in_order[here] = true;
        at = longest[here].1;
    }
    order
        .iter()
        .zip(in_order)
        .filter(|(_, kept)| !kept)
        .map(|((id, _), _)| *id)
        .collect()
}

/// What a tab's panes are, but for the session the pane that follows the
/// selection shows, which changes as the selection moves: that's
/// `pane.focused`'s.
fn shape(tab: &TabLayout) -> (Tile, bool, Option<&str>) {
    (
        without_selection(&tab.panes),
        tab.zoomed,
        tab.floating.as_deref(),
    )
}

fn without_selection(tile: &Tile) -> Tile {
    match tile {
        Tile::Pane {
            selection: true, ..
        } => Tile::Pane {
            session: None,
            selection: true,
        },
        Tile::Pane { .. } => tile.clone(),
        Tile::Split {
            way,
            ratio,
            first,
            second,
        } => Tile::Split {
            way: *way,
            ratio: *ratio,
            first: Box::new(without_selection(first)),
            second: Box::new(without_selection(second)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{State, Worktree};
    use crate::tui::split_tree::Way;
    use std::path::Path;

    fn session(name: &str, project: &str) -> SessionInfo {
        SessionInfo {
            stopped_idle: false,
            name: name.into(),
            id: format!("id-{name}"),
            command: vec!["sh".into()],
            cwd: PathBuf::from(project),
            pid: None,
            state: State::Running,
            activity: None,
            worktree: Some(Worktree {
                project: project.trim_start_matches('/').into(),
                project_path: PathBuf::from(project),
                path: PathBuf::from(project),
                main: true,
                branch: None,
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
        }
    }

    fn sessions() -> Vec<SessionInfo> {
        vec![
            session("a", "/app"),
            session("b", "/app"),
            session("c", "/docs"),
        ]
    }

    fn selection(shows: Option<&str>) -> Tile {
        Tile::Pane {
            session: shows.map(String::from),
            selection: true,
        }
    }

    /// A tab numbered `number`, called `name`, holding `sessions`, the
    /// first selected, with its panes the selection's alone.
    fn tab(number: usize, name: &str, sessions: &[&str]) -> TabLayout {
        TabLayout {
            number,
            name: name.into(),
            current: false,
            zoomed: false,
            sessions: sessions.iter().map(|name| name.to_string()).collect(),
            selected: sessions.first().map(|name| name.to_string()),
            floating: None,
            panes: selection(sessions.first().copied()),
        }
    }

    /// A look at `tabs`, numbered in their order, the one with `front`'s
    /// id in front, the user on `focused`.
    fn look(tabs: &[(u64, TabLayout)], front: u64, focused: Option<&str>) -> Look {
        let tabs = tabs
            .iter()
            .enumerate()
            .map(|(at, (id, tab))| {
                let tab = TabLayout {
                    number: at + 1,
                    current: *id == front,
                    ..tab.clone()
                };
                (*id, tab)
            })
            .collect();
        let focused = focused
            .map(|name| Focus::of(&sessions().into_iter().find(|s| s.name == name).unwrap()));
        Look { tabs, focused }
    }

    fn kinds(events: &[Event]) -> Vec<&'static str> {
        events.iter().map(|event| event.kind.name()).collect()
    }

    #[test]
    fn nothing_changed_is_nothing_to_tell() {
        let before = look(&[(1, tab(1, "", &["a", "b"]))], 1, Some("a"));
        assert!(changes(&before, &before.clone(), &sessions()).is_empty());
    }

    #[test]
    fn a_tab_made_named_and_brought_to_the_front_is_told_so() {
        let before = look(&[(1, tab(1, "", &["a"]))], 1, Some("a"));
        let after = look(
            &[(1, tab(1, "main", &["a"])), (2, tab(2, "work", &[]))],
            2,
            Some("a"),
        );
        let events = changes(&before, &after, &sessions());
        assert_eq!(
            kinds(&events),
            ["tab.renamed", "tab.created", "tab.focused"]
        );
        assert_eq!(events[0].from.as_deref(), Some(""));
        assert_eq!(events[0].line(), "main: named");
        assert_eq!(events[1].tab.as_ref().unwrap().name, "work");
        assert_eq!(events[2].from.as_deref(), Some("1"));
        assert_eq!(events[2].line(), "work: in front");
    }

    #[test]
    fn a_tab_closed_is_told_as_it_was_and_the_others_dont_move() {
        let before = look(
            &[
                (1, tab(1, "", &["a"])),
                (2, tab(2, "x", &["b"])),
                (3, tab(3, "y", &["c"])),
            ],
            3,
            Some("c"),
        );
        let after = look(
            &[(1, tab(1, "", &["a"])), (3, tab(3, "y", &["c"]))],
            3,
            Some("c"),
        );
        let events = changes(&before, &after, &sessions());
        assert_eq!(kinds(&events), ["tab.closed"]);
        assert_eq!(events[0].tab.as_ref().unwrap().number, 2);
        assert_eq!(events[0].line(), "x: closed");
    }

    #[test]
    fn a_tab_moved_is_the_one_out_of_order() {
        let tabs = [
            (1, tab(1, "a", &[])),
            (2, tab(2, "b", &[])),
            (3, tab(3, "c", &[])),
        ];
        let before = look(&tabs, 1, None);
        let after = look(
            &[tabs[1].clone(), tabs[2].clone(), tabs[0].clone()],
            1,
            None,
        );
        let events = changes(&before, &after, &sessions());
        assert_eq!(kinds(&events), ["tab.moved"]);
        assert_eq!(events[0].line(), "a: from 1 to 3");
        let after = look(
            &[tabs[2].clone(), tabs[0].clone(), tabs[1].clone()],
            1,
            None,
        );
        let events = changes(&before, &after, &sessions());
        assert_eq!(events[0].subject(), "c");
    }

    #[test]
    fn a_session_moved_to_another_tab_is_told_with_the_tab_it_left() {
        let before = look(
            &[(1, tab(1, "", &["a", "b"])), (2, tab(2, "work", &[]))],
            1,
            Some("a"),
        );
        let after = look(
            &[(1, tab(1, "", &["a"])), (2, tab(2, "work", &["b"]))],
            1,
            Some("a"),
        );
        let events = changes(&before, &after, &sessions());
        assert_eq!(kinds(&events), ["pane.moved"]);
        assert_eq!(events[0].line(), "b: tab 1 → work");
        assert_eq!(events[0].session.as_ref().unwrap().id, "id-b");
    }

    #[test]
    fn panes_changed_are_told_but_not_the_selection_moving_through_them() {
        let one = tab(1, "", &["a", "b"]);
        let split = TabLayout {
            panes: Tile::Split {
                way: Way::Right,
                ratio: 0.5,
                first: Box::new(selection(Some("a"))),
                second: Box::new(Tile::Pane {
                    session: Some("b".into()),
                    selection: false,
                }),
            },
            ..one.clone()
        };
        let before = look(&[(1, one.clone())], 1, Some("a"));
        let after = look(&[(1, split.clone())], 1, Some("a"));
        let events = changes(&before, &after, &sessions());
        assert_eq!(kinds(&events), ["layout.updated"]);
        assert_eq!(events[0].line(), "tab 1: 2 panes");
        // The selection's pane showing another session is a focus, not a
        // change of panes.
        let moved_on = TabLayout {
            panes: selection(Some("b")),
            selected: Some("b".into()),
            ..one.clone()
        };
        let events = changes(&before, &look(&[(1, moved_on)], 1, Some("b")), &sessions());
        assert_eq!(kinds(&events), ["pane.focused"]);
        assert_eq!(events[0].line(), "b: focused, from a");
        assert_eq!(events[0].tab.as_ref().unwrap().number, 1);
        // Zoomed, too.
        let zoomed = TabLayout {
            zoomed: true,
            ..split
        };
        let events = changes(&after, &look(&[(1, zoomed)], 1, Some("a")), &sessions());
        assert_eq!(events[0].line(), "tab 1: 2 panes, zoomed");
    }

    #[test]
    fn the_user_going_to_another_project_is_told_before_the_session() {
        let both = [(1, tab(1, "", &["a", "c"]))];
        let before = look(&both, 1, Some("a"));
        let events = changes(&before, &look(&both, 1, Some("c")), &sessions());
        assert_eq!(kinds(&events), ["project.focused", "pane.focused"]);
        assert_eq!(events[0].project.as_deref(), Some(Path::new("/docs")));
        assert_eq!(events[0].line(), "docs: focused, from app");
        // Within a project, only the session.
        let both = [(1, tab(1, "", &["a", "b"]))];
        let events = changes(
            &look(&both, 1, Some("a")),
            &look(&both, 1, Some("b")),
            &sessions(),
        );
        assert_eq!(kinds(&events), ["pane.focused"]);
    }

    #[test]
    fn the_user_stays_on_their_session_while_the_selection_rests_on_no_session() {
        let tabs = [(1, tab(1, "", &["a", "b"]))];
        let on_a = look(&tabs, 1, Some("a"));
        let (events, kept) = told(&on_a, look(&tabs, 1, None), &sessions());
        assert!(events.is_empty());
        assert_eq!(kept.focused, on_a.focused);
        // Back on it, nothing changed; on another, it did.
        let (events, _) = told(&kept, look(&tabs, 1, Some("a")), &sessions());
        assert!(events.is_empty());
        let (events, _) = told(&kept, look(&tabs, 1, Some("b")), &sessions());
        assert_eq!(kinds(&events), ["pane.focused"]);
    }
}
