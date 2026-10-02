//! The TUI's state and how keys and session lists change it. Nothing here
//! talks to the daemon or draws: when a key needs the outside world, it
//! comes back as an [`Action`] for the event loop to carry out. That keeps
//! every state change testable on its own.

use super::keys;
use crate::protocol::{Activity, SessionInfo, State};
use crossterm::event::{KeyCode, KeyEvent};

/// Where the keyboard goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Keys move through the list and act on sessions.
    Sidebar,
    /// Keys go to the selected session.
    Pane,
}

/// What a key asks the event loop to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    /// Start a shell in a new session.
    NewSession,
    Kill(String),
    /// Send the key to the selected session.
    Type(KeyEvent),
}

pub struct App {
    sessions: Vec<SessionInfo>,
    /// An index into `sessions`, kept in range while there are any.
    selected: usize,
    focus: Focus,
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
            focus: Focus::Sidebar,
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

    pub fn focus(&self) -> Focus {
        self.focus
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

    /// Takes a fresh list from the daemon, with the sessions waiting on the
    /// user moved to the top. The selected session stays selected wherever
    /// it moved to; if it's gone, the selection stays at the same place in
    /// the list, or the end of it.
    pub fn set_sessions(&mut self, mut sessions: Vec<SessionInfo>) {
        // A stable sort: false comes before true, and otherwise the order
        // the sessions were made in is kept.
        sessions.sort_by_key(|session| session.activity != Some(Activity::Waiting));
        let selected_name = self.selected().map(|session| session.name.clone());
        self.sessions = sessions;
        let still_there = selected_name.and_then(|name| self.position(&name));
        if let Some(index) = still_there {
            self.selected = index;
        }
        self.selected = self.selected.min(self.sessions.len().saturating_sub(1));
        if !self.can_type_into_selected() {
            self.focus = Focus::Sidebar;
        }
    }

    /// Selects the session called `name`, if there is one.
    pub fn select(&mut self, name: &str) {
        if let Some(index) = self.position(name) {
            self.selected = index;
        }
    }

    /// Hands the keyboard to the selected session, if it can take keys.
    pub fn type_into_selected(&mut self) {
        if self.can_type_into_selected() {
            self.focus = Focus::Pane;
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        self.notice = None;
        match self.focus {
            Focus::Sidebar => self.on_sidebar_key(key),
            Focus::Pane => self.on_pane_key(key),
        }
    }

    fn on_sidebar_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Enter => self.type_into_selected(),
            KeyCode::Char('n') => return Some(Action::NewSession),
            KeyCode::Char('x') => {
                let name = self.selected()?.name.clone();
                return Some(Action::Kill(name));
            }
            KeyCode::Char('q') => return Some(Action::Quit),
            _ => {}
        }
        None
    }

    fn on_pane_key(&mut self, key: KeyEvent) -> Option<Action> {
        if keys::is_hand_back(&key) {
            self.focus = Focus::Sidebar;
            None
        } else {
            Some(Action::Type(key))
        }
    }

    fn move_selection(&mut self, by: isize) {
        let last = self.sessions.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
    }

    /// Only a running session, other than our own, takes keys.
    fn can_type_into_selected(&self) -> bool {
        let running = self
            .selected()
            .is_some_and(|session| session.state == State::Running);
        running && !self.selected_is_own()
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
    use crossterm::event::KeyModifiers;
    use std::path::PathBuf;

    fn session(name: &str) -> SessionInfo {
        SessionInfo {
            name: name.into(),
            command: vec!["sh".into()],
            cwd: PathBuf::from("/"),
            pid: Some(1),
            state: State::Running,
            activity: None,
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
        assert_eq!(app.focus(), Focus::Pane);

        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(app.on_key(q), Some(Action::Type(q)));

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
}
