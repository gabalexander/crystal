//! The TUI's state and how keys and session lists change it. Nothing here
//! talks to the daemon or draws: when a key needs the outside world, it
//! comes back as an [`Action`] for the event loop to carry out. That keeps
//! every state change testable on its own.

use super::groups::{self, Row};
use super::keys;
use super::text_input::TextInput;
use crate::protocol::{SessionInfo, State};
use crossterm::event::{KeyCode, KeyEvent};
use std::path::PathBuf;

/// How many sessions can be split off into panes of their own at once.
pub const MAX_SPLITS: usize = 2;

/// Where a pane sits beside the sidebar: the one that follows the
/// selection, or one of the splits, counted in the order they were made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Selected,
    Split(usize),
}

/// Where the keyboard goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Keys move through the list and act on sessions.
    Sidebar,
    /// Keys go to the session in this pane.
    Pane(Slot),
}

/// Which way Tab goes round the panes: Tab forward, Shift+Tab back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Forward,
    Back,
}

/// What a key asks the event loop to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    /// Start a shell in a new session.
    NewSession,
    /// Start a shell in a new worktree on `branch`, made in the repository
    /// at `base`, or the TUI's own directory when that's `None`.
    NewWorktree {
        branch: String,
        base: Option<PathBuf>,
    },
    Kill(String),
    /// Send the key to the session in the pane at `to`.
    Type {
        to: Slot,
        key: KeyEvent,
    },
}

pub struct App {
    /// In the sidebar's order: see [`groups`].
    sessions: Vec<SessionInfo>,
    /// An index into `sessions`, kept in range while there are any.
    selected: usize,
    /// The branch name being typed for a new worktree, while it's asked for.
    branch_prompt: Option<TextInput>,
    /// Sessions split off into panes of their own, by name, in the order
    /// they were split off. A split stays on its session while the
    /// selection moves.
    splits: Vec<String>,
    focus: Focus,
    /// The pane the keyboard was in last, so that Tab in the sidebar goes
    /// on to the next one.
    last_pane: Option<Slot>,
    /// The session this TUI runs in, if it runs in one. The pane never
    /// shows it: it would be showing itself.
    own_session: Option<String>,
    /// Something to tell the user, like why a key didn't work. It stays
    /// until the next key.
    notice: Option<String>,
}

impl App {
    pub fn new(own_session: Option<String>) -> App {
        App {
            sessions: Vec::new(),
            selected: 0,
            branch_prompt: None,
            splits: Vec::new(),
            focus: Focus::Sidebar,
            last_pane: None,
            own_session,
            notice: None,
        }
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn notify(&mut self, notice: String) {
        self.notice = Some(notice);
    }

    pub fn sessions(&self) -> &[SessionInfo] {
        &self.sessions
    }

    /// The sidebar's rows: the sessions under their projects and worktrees.
    pub fn rows(&self) -> Vec<Row> {
        groups::rows(&self.sessions)
    }

    /// The branch name typed so far, while a new worktree's is asked for.
    pub fn branch_prompt(&self) -> Option<&TextInput> {
        self.branch_prompt.as_ref()
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// The names of the sessions split off, in the order they were.
    pub fn splits(&self) -> &[String] {
        &self.splits
    }

    /// The panes beside the sidebar, in the order they're drawn: the one
    /// that follows the selection, then each split.
    pub fn slots(&self) -> Vec<Slot> {
        let splits = (0..self.splits.len()).map(Slot::Split);
        std::iter::once(Slot::Selected).chain(splits).collect()
    }

    /// The session the pane at `slot` is about: the selected one, or the
    /// one split off there.
    pub fn pane_session(&self, slot: Slot) -> Option<&SessionInfo> {
        match slot {
            Slot::Selected => self.selected(),
            Slot::Split(index) => {
                let name = self.splits.get(index)?;
                self.sessions.iter().find(|session| session.name == *name)
            }
        }
    }

    /// Whether the pane at `slot` shows its session's screen. The pane that
    /// follows the selection doesn't when the selected session is split
    /// off, so that no session is drawn twice at two sizes, nor when it's
    /// the session this TUI runs in.
    pub fn shows_screen(&self, slot: Slot) -> bool {
        let Some(session) = self.pane_session(slot) else {
            return false;
        };
        match slot {
            Slot::Selected => !self.selected_is_own() && !self.is_split(&session.name),
            Slot::Split(_) => true,
        }
    }

    /// Whether the session called `name` has a pane of its own.
    pub fn is_split(&self, name: &str) -> bool {
        self.splits.iter().any(|split| split == name)
    }

    /// The index of the selected session, or `None` when there are none.
    pub fn selected_index(&self) -> Option<usize> {
        if self.sessions.is_empty() {
            None
        } else {
            Some(self.selected)
        }
    }

    pub fn selected(&self) -> Option<&SessionInfo> {
        self.sessions.get(self.selected)
    }

    /// Whether the selected session is the one this TUI runs in.
    pub fn selected_is_own(&self) -> bool {
        match (&self.own_session, self.selected()) {
            (Some(own), Some(selected)) => selected.name == *own,
            _ => false,
        }
    }

    /// Takes a fresh list from the daemon and puts it in the sidebar's
    /// order. The selected session stays selected wherever it moved to; if
    /// it's gone, the selection stays at the same place in the list, or the
    /// end of it.
    pub fn set_sessions(&mut self, sessions: Vec<SessionInfo>) {
        let selected_name = self.selected().map(|session| session.name.clone());
        self.sessions = groups::order(sessions);
        let still_there = selected_name.and_then(|name| self.position(&name));
        if let Some(index) = still_there {
            self.selected = index;
        }
        self.selected = self.selected.min(self.sessions.len().saturating_sub(1));
        // A split whose session has gone closes. Going from the end keeps
        // the splits still to check where they were.
        for index in (0..self.splits.len()).rev() {
            if self.position(&self.splits[index]).is_none() {
                self.close_split(index);
            }
        }
        if let Focus::Pane(slot) = self.focus
            && !self.can_type_into(slot)
        {
            self.focus = Focus::Sidebar;
        }
    }

    /// Selects the session called `name`, if there is one.
    pub fn select(&mut self, name: &str) {
        if let Some(index) = self.position(name) {
            self.selected = index;
        }
    }

    /// Hands the keyboard to the selected session, in whichever pane shows
    /// it, if it can take keys.
    pub fn type_into_selected(&mut self) {
        let Some(selected) = self.selected() else {
            return;
        };
        let slot = match self.splits.iter().position(|split| *split == selected.name) {
            Some(index) => Slot::Split(index),
            None => Slot::Selected,
        };
        if self.can_type_into(slot) {
            self.focus_pane(slot);
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        self.notice = None;
        if self.branch_prompt.is_some() {
            return self.on_prompt_key(key);
        }
        match self.focus {
            Focus::Sidebar => self.on_sidebar_key(key),
            Focus::Pane(slot) => self.on_pane_key(slot, key),
        }
    }

    fn on_sidebar_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Enter => self.type_into_selected(),
            KeyCode::Tab => self.move_to_pane(Direction::Forward),
            KeyCode::BackTab => self.move_to_pane(Direction::Back),
            KeyCode::Char('s') => self.toggle_split(),
            KeyCode::Char('n') => return Some(Action::NewSession),
            KeyCode::Char('w') => self.branch_prompt = Some(TextInput::default()),
            KeyCode::Char('x') => {
                let name = self.selected()?.name.clone();
                return Some(Action::Kill(name));
            }
            KeyCode::Char('q') => return Some(Action::Quit),
            _ => {}
        }
        None
    }

    /// Keys while a branch name is asked for: Enter makes the worktree, Esc
    /// gives up, and every other key edits the name.
    fn on_prompt_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.branch_prompt = None;
                None
            }
            KeyCode::Enter => {
                let input = self.branch_prompt.take()?;
                let branch = input.text().trim();
                if branch.is_empty() {
                    return None;
                }
                Some(Action::NewWorktree {
                    branch: branch.to_string(),
                    base: self.worktree_base(),
                })
            }
            _ => {
                if let Some(input) = &mut self.branch_prompt {
                    input.on_key(&key);
                }
                None
            }
        }
    }

    /// Where a new worktree is made from: the selected session's project,
    /// or `None` for the TUI's own directory when nothing in a repository
    /// is selected.
    fn worktree_base(&self) -> Option<PathBuf> {
        let worktree = self.selected()?.worktree.as_ref()?;
        Some(worktree.project_path.clone())
    }

    /// Every key goes to the pane's session, Tab too, since shells and
    /// agents need it. Only Ctrl+\ is kept back: it returns to the sidebar.
    fn on_pane_key(&mut self, slot: Slot, key: KeyEvent) -> Option<Action> {
        if keys::is_hand_back(&key) {
            self.focus = Focus::Sidebar;
            None
        } else {
            Some(Action::Type { to: slot, key })
        }
    }

    /// Splits the selected session off into a pane of its own, or closes
    /// its split if it has one.
    fn toggle_split(&mut self) {
        let Some(selected) = self.selected() else {
            return;
        };
        let name = selected.name.clone();
        if let Some(index) = self.splits.iter().position(|split| *split == name) {
            self.close_split(index);
        } else if self.selected_is_own() {
            self.notify("crystal can't show the session it runs in".into());
        } else if self.splits.len() >= MAX_SPLITS {
            let notice = format!("{MAX_SPLITS} splits at most: press s on one to close it");
            self.notify(notice);
        } else {
            self.splits.push(name);
        }
    }

    /// Closes the split at `index`. The splits after it move up a place,
    /// and the keyboard moves with its pane, or goes back to the sidebar
    /// if its pane is the one that closed.
    fn close_split(&mut self, index: usize) {
        self.splits.remove(index);
        self.focus = match self.focus {
            Focus::Pane(Slot::Split(at)) if at == index => Focus::Sidebar,
            Focus::Pane(Slot::Split(at)) if at > index => Focus::Pane(Slot::Split(at - 1)),
            focus => focus,
        };
    }

    /// Moves the keyboard from the sidebar to the next pane that takes
    /// keys, going on from the one it was in last.
    fn move_to_pane(&mut self, direction: Direction) {
        if let Some(slot) = self.next_pane(direction) {
            self.focus_pane(slot);
        }
    }

    /// The next pane that takes keys, going round the panes in the order
    /// they're drawn, from just past the one used last. With none used
    /// yet, going forward starts at the first pane, and going back at the
    /// last.
    fn next_pane(&self, direction: Direction) -> Option<Slot> {
        let slots = self.slots();
        let count = slots.len();
        let last = self
            .last_pane
            .and_then(|last| slots.iter().position(|slot| *slot == last));
        (1..=count)
            .map(|step| match (direction, last) {
                (Direction::Forward, Some(last)) => (last + step) % count,
                (Direction::Back, Some(last)) => (last + count - step) % count,
                (Direction::Forward, None) => step - 1,
                (Direction::Back, None) => count - step,
            })
            .map(|index| slots[index])
            .find(|slot| self.can_type_into(*slot))
    }

    fn focus_pane(&mut self, slot: Slot) {
        self.focus = Focus::Pane(slot);
        self.last_pane = Some(slot);
    }

    fn move_selection(&mut self, by: isize) {
        let last = self.sessions.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
    }

    /// Only a pane that shows a running session takes keys.
    fn can_type_into(&self, slot: Slot) -> bool {
        let running = self
            .pane_session(slot)
            .is_some_and(|session| session.state == State::Running);
        running && self.shows_screen(slot)
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| session.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Activity, Worktree};
    use crossterm::event::KeyModifiers;

    fn session(name: &str) -> SessionInfo {
        SessionInfo {
            name: name.into(),
            command: vec!["sh".into()],
            cwd: PathBuf::from("/"),
            pid: Some(1),
            state: State::Running,
            activity: None,
            worktree: None,
        }
    }

    fn ended(name: &str) -> SessionInfo {
        SessionInfo {
            state: State::Exited { code: 0 },
            ..session(name)
        }
    }

    fn app_with(names: &[&str]) -> App {
        let mut app = App::new(None);
        app.set_sessions(names.iter().map(|name| session(name)).collect());
        app
    }

    fn press(app: &mut App, code: KeyCode) -> Option<Action> {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn selected_name(app: &App) -> Option<&str> {
        app.selected().map(|session| session.name.as_str())
    }

    #[test]
    fn j_and_k_move_the_selection_and_stop_at_the_ends() {
        let mut app = app_with(&["a", "b", "c"]);
        assert_eq!(selected_name(&app), Some("a"));
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(selected_name(&app), Some("a"));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(selected_name(&app), Some("c"));
        press(&mut app, KeyCode::Up);
        assert_eq!(selected_name(&app), Some("b"));
    }

    #[test]
    fn the_selected_session_stays_selected_when_the_list_changes() {
        let mut app = app_with(&["a", "b", "c"]);
        app.select("b");
        app.set_sessions(vec![session("new"), session("a"), session("b")]);
        assert_eq!(selected_name(&app), Some("b"));
    }

    #[test]
    fn when_the_selected_session_goes_the_selection_stays_in_range() {
        let mut app = app_with(&["a", "b", "c"]);
        app.select("c");
        app.set_sessions(vec![session("a"), session("b")]);
        assert_eq!(selected_name(&app), Some("b"));

        app.set_sessions(Vec::new());
        assert_eq!(app.selected_index(), None);
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
    }

    #[test]
    fn enter_hands_the_keyboard_to_the_pane_and_ctrl_backslash_takes_it_back() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));

        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let typed = Action::Type {
            to: Slot::Selected,
            key: q,
        };
        assert_eq!(app.on_key(q), Some(typed));

        let ctrl_backslash = KeyEvent::new(KeyCode::Char('4'), KeyModifiers::CONTROL);
        assert_eq!(app.on_key(ctrl_backslash), None);
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn an_ended_session_takes_no_keys() {
        let mut app = App::new(None);
        app.set_sessions(vec![ended("done")]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn the_keyboard_comes_back_when_the_session_being_typed_into_ends() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Enter);
        app.set_sessions(vec![ended("a")]);
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn the_tuis_own_session_is_never_typed_into() {
        let mut app = App::new(Some("me".into()));
        app.set_sessions(vec![session("me")]);
        assert!(app.selected_is_own());
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn a_notice_lasts_until_the_next_key() {
        let mut app = app_with(&["a"]);
        app.notify("no session named b".into());
        assert_eq!(app.notice(), Some("no session named b"));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.notice(), None);
    }

    #[test]
    fn sessions_waiting_on_the_user_come_first() {
        let mut waiting = session("asks");
        waiting.activity = Some(Activity::Waiting);
        let mut app = App::new(None);
        app.set_sessions(vec![session("a"), session("b"), waiting]);
        let names: Vec<&str> = app.sessions().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["asks", "a", "b"]);
    }

    #[test]
    fn sidebar_keys_ask_for_their_actions() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            press(&mut app, KeyCode::Char('n')),
            Some(Action::NewSession)
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('x')),
            Some(Action::Kill("b".into()))
        );
        assert_eq!(press(&mut app, KeyCode::Char('q')), Some(Action::Quit));
    }

    fn in_project(name: &str, project: &str) -> SessionInfo {
        SessionInfo {
            worktree: Some(Worktree {
                project: project.into(),
                project_path: PathBuf::from(format!("/code/{project}")),
                path: PathBuf::from(format!("/code/{project}")),
                main: true,
                branch: Some("main".into()),
            }),
            ..session(name)
        }
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn w_asks_for_a_branch_and_enter_makes_the_worktree_in_the_selected_project() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_project("agent", "app")]);
        press(&mut app, KeyCode::Char('w'));
        type_text(&mut app, "fix/typo");
        assert_eq!(app.branch_prompt().unwrap().text(), "fix/typo");

        let action = press(&mut app, KeyCode::Enter);
        assert_eq!(
            action,
            Some(Action::NewWorktree {
                branch: "fix/typo".into(),
                base: Some(PathBuf::from("/code/app")),
            })
        );
        assert!(app.branch_prompt().is_none());
    }

    #[test]
    fn keys_go_to_the_branch_prompt_while_it_is_open() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('w'));
        // q and x would quit and kill on the list; here they're letters.
        type_text(&mut app, "qx");
        assert_eq!(app.branch_prompt().unwrap().text(), "qx");
        assert_eq!(selected_name(&app), Some("a"));
    }

    #[test]
    fn esc_or_an_empty_name_gives_up_on_the_new_worktree() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('w'));
        type_text(&mut app, "feat");
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert!(app.branch_prompt().is_none());

        press(&mut app, KeyCode::Char('w'));
        type_text(&mut app, "  ");
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert!(app.branch_prompt().is_none());
    }

    #[test]
    fn outside_a_repository_the_worktree_is_made_from_the_tuis_directory() {
        let mut app = app_with(&["shell"]);
        press(&mut app, KeyCode::Char('w'));
        type_text(&mut app, "feat");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::NewWorktree {
                branch: "feat".into(),
                base: None,
            })
        );
    }

    fn hand_back(app: &mut App) {
        app.on_key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL));
    }

    /// An app with `names`, the first `split` of them split off, and the
    /// selection on the last one.
    fn app_with_splits(names: &[&str], split: usize) -> App {
        let mut app = app_with(names);
        for _ in 0..split {
            press(&mut app, KeyCode::Char('s'));
            press(&mut app, KeyCode::Char('j'));
        }
        app.select(names[names.len() - 1]);
        app
    }

    #[test]
    fn s_splits_the_selected_session_off_and_again_closes_it() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.splits(), ["a"]);
        assert_eq!(app.slots(), [Slot::Selected, Slot::Split(0)]);

        press(&mut app, KeyCode::Char('s'));
        assert!(app.splits().is_empty());
    }

    #[test]
    fn a_split_stays_on_its_session_while_the_selection_moves() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('j'));
        let shown = |slot| app.pane_session(slot).map(|session| session.name.as_str());
        assert_eq!(shown(Slot::Selected), Some("b"));
        assert_eq!(shown(Slot::Split(0)), Some("a"));
    }

    #[test]
    fn two_splits_at_most_and_a_third_says_so() {
        let mut app = app_with_splits(&["a", "b", "c"], 2);
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.splits(), ["a", "b"]);
        assert!(app.notice().unwrap().contains("2 splits at most"));
    }

    #[test]
    fn a_session_with_a_split_is_not_shown_again_in_the_selections_pane() {
        let mut app = app_with(&["a"]);
        assert!(app.shows_screen(Slot::Selected));
        press(&mut app, KeyCode::Char('s'));
        assert!(!app.shows_screen(Slot::Selected));
        assert!(app.shows_screen(Slot::Split(0)));
    }

    #[test]
    fn enter_on_a_split_session_types_into_its_split() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(0)));
    }

    #[test]
    fn the_tuis_own_session_is_never_split_off() {
        let mut app = App::new(Some("me".into()));
        app.set_sessions(vec![session("me")]);
        press(&mut app, KeyCode::Char('s'));
        assert!(app.splits().is_empty());
        assert!(app.notice().is_some());
    }

    #[test]
    fn tab_in_the_sidebar_goes_on_to_the_next_pane_each_time() {
        let mut app = app_with_splits(&["a", "b", "c"], 2);
        let mut visited = Vec::new();
        for _ in 0..4 {
            press(&mut app, KeyCode::Tab);
            visited.push(app.focus());
            hand_back(&mut app);
        }
        let pane = Focus::Pane;
        assert_eq!(
            visited,
            [
                pane(Slot::Selected),
                pane(Slot::Split(0)),
                pane(Slot::Split(1)),
                pane(Slot::Selected),
            ]
        );
    }

    #[test]
    fn shift_tab_goes_round_the_other_way_starting_at_the_last_pane() {
        let mut app = app_with_splits(&["a", "b", "c"], 2);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(1)));
        hand_back(&mut app);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(0)));
    }

    #[test]
    fn tab_skips_panes_that_take_no_keys() {
        let mut app = App::new(None);
        app.set_sessions(vec![ended("done"), session("live")]);
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('s'));
        // The selection is on `live`, which has a split of its own, so its
        // pane shows nothing; `done` has ended. Only `live`'s split is left.
        for _ in 0..3 {
            press(&mut app, KeyCode::Tab);
            assert_eq!(app.focus(), Focus::Pane(Slot::Split(1)));
            hand_back(&mut app);
        }
    }

    #[test]
    fn tab_inside_a_pane_goes_to_its_session() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Enter);
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        let typed = Action::Type {
            to: Slot::Selected,
            key: tab,
        };
        assert_eq!(app.on_key(tab), Some(typed));
    }

    #[test]
    fn a_split_closes_when_its_session_goes_and_the_keyboard_follows_its_pane() {
        let mut app = app_with_splits(&["a", "b", "c"], 2);
        app.select("b");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(1)));

        app.set_sessions(vec![session("b"), session("c")]);
        assert_eq!(app.splits(), ["b"]);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(0)));
    }

    #[test]
    fn the_keyboard_leaves_a_split_whose_session_ends() {
        let mut app = app_with_splits(&["a", "b"], 1);
        app.select("a");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(0)));

        app.set_sessions(vec![ended("a"), session("b")]);
        assert_eq!(app.splits(), ["a"], "it stays, to show how it ended");
        assert_eq!(app.focus(), Focus::Sidebar);
    }
}
