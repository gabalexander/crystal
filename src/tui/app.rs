//! The TUI's state and how keys, the mouse and session lists change it.
//! Nothing here talks to the daemon or draws: when a key needs the outside
//! world, it comes back as an [`Action`] for the event loop to carry out.
//! That keeps every state change testable on its own.

use super::command_line;
use super::groups::{self, Row};
use super::keys;
use super::text_input::TextInput;
use crate::config::Config;
use crate::protocol::{Activity, SessionInfo, State};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
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

/// What the mouse is over, worked out from the layout by `ui::hit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// A row of the sidebar, by its place in [`App::rows`].
    SidebarRow(usize),
    /// The sidebar, but none of its rows: its border, or below the last.
    Sidebar,
    /// The pane at `slot`. `cell` is the `(row, column)` on its session's
    /// screen, counted from 0, when the mouse is inside the pane's border.
    Pane {
        slot: Slot,
        cell: Option<(u16, u16)>,
    },
    /// The footer, or anywhere else.
    Elsewhere,
}

/// Which way Tab goes round the panes: Tab forward, Shift+Tab back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Forward,
    Back,
}

/// Where a new session starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// In this directory, or the TUI's own when it's `None`.
    Directory(Option<PathBuf>),
    /// In a new worktree on `branch`, made in the repository at `base`, or
    /// the TUI's own directory's when that's `None`.
    NewWorktree {
        branch: String,
        base: Option<PathBuf>,
    },
}

/// A question asked on the footer line, and the answer typed so far.
#[derive(Debug)]
pub struct Prompt {
    pub question: Question,
    pub input: TextInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// The branch for a new worktree. The command to run there is asked
    /// for next.
    Branch,
    /// The command line for a new session, which starts at the place.
    Command(Place),
    /// A new name for the session now called this.
    Rename(String),
}

/// A question on the footer line that `y` answers yes and any other key
/// no, asked before something that can't be taken back, or that starts a
/// program again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    Kill(String),
    /// Start this ended session's command again.
    Respawn(String),
    /// Remove the linked worktree at `path`, which is on `branch`.
    RemoveWorktree {
        path: PathBuf,
        branch: String,
    },
}

impl Confirm {
    /// The question, the way the footer asks it.
    pub fn question(&self) -> String {
        match self {
            Confirm::Kill(name) => format!("kill {name}? y/n"),
            Confirm::Respawn(name) => format!("start {name} again? y/n"),
            Confirm::RemoveWorktree { branch, .. } => format!("remove worktree {branch}? y/n"),
        }
    }

    /// What a yes asks for.
    fn action(self) -> Action {
        match self {
            Confirm::Kill(name) => Action::Kill(name),
            Confirm::Respawn(name) => Action::Respawn(name),
            Confirm::RemoveWorktree { path, .. } => Action::RemoveWorktree(path),
        }
    }
}

/// What a key asks the event loop to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    /// Start `command` in a new session at `place`. An empty command starts
    /// the user's shell.
    Start {
        place: Place,
        command: Vec<String>,
    },
    Kill(String),
    Rename {
        name: String,
        new_name: String,
    },
    /// Start this ended session's command again.
    Respawn(String),
    /// Remove the linked worktree at this path.
    RemoveWorktree(PathBuf),
    /// Send the key to the session in the pane at `to`.
    Type {
        to: Slot,
        key: KeyEvent,
    },
    /// Show a page further back into the history of the pane at this slot.
    PageBack(Slot),
    /// Show a page further toward live in the pane at this slot.
    PageForward(Slot),
    /// Show a few lines further back into the history of the pane at this
    /// slot: a notch of the mouse wheel.
    ScrollBack(Slot),
    /// Show a few lines further toward live in the pane at this slot.
    ScrollForward(Slot),
}

pub struct App {
    /// In the sidebar's order: see [`groups`].
    sessions: Vec<SessionInfo>,
    /// An index into `sessions`, kept in range while there are any.
    selected: usize,
    /// The question on the footer line, while one is being answered.
    prompt: Option<Prompt>,
    /// The command line the last new session was started with, which the
    /// next one starts out with.
    last_command: String,
    /// A yes-or-no question on the footer line, until it's answered.
    confirm: Option<Confirm>,
    /// Sessions split off into panes of their own, by name, in the order
    /// they were split off. A split stays on its session while the
    /// selection moves.
    splits: Vec<String>,
    focus: Focus,
    /// The pane the keyboard was in last, so that Tab in the sidebar goes
    /// on to the next one.
    last_pane: Option<Slot>,
    /// The id of the session this TUI runs in, if it runs in one. The pane
    /// never shows it: it would be showing itself.
    own_id: Option<String>,
    /// Something to tell the user, like why a key didn't work. It stays
    /// until the next key.
    notice: Option<String>,
}

impl App {
    /// A TUI with no sessions yet. `own_id` is the id of the session it
    /// runs in, if it runs in one.
    pub fn new(own_id: Option<String>) -> App {
        App {
            sessions: Vec::new(),
            selected: 0,
            prompt: None,
            last_command: Config::default().new_session,
            confirm: None,
            splits: Vec::new(),
            focus: Focus::Sidebar,
            last_pane: None,
            own_id,
            notice: None,
        }
    }

    /// Sets what the new-session prompt starts out with, until a session
    /// has been started from it: the config's `new_session`.
    pub fn set_first_command(&mut self, command: String) {
        self.last_command = command;
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

    /// The question on the footer line and its answer so far, while one
    /// is being answered.
    pub fn prompt(&self) -> Option<&Prompt> {
        self.prompt.as_ref()
    }

    /// The yes-or-no question waiting on its answer, if there is one.
    pub fn confirm(&self) -> Option<&Confirm> {
        self.confirm.as_ref()
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

    /// Whether the selected session is the one this TUI runs in. Ids tell,
    /// since the session may have been renamed since the TUI started.
    pub fn selected_is_own(&self) -> bool {
        match (&self.own_id, self.selected()) {
            (Some(own), Some(selected)) => selected.id == *own,
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

    /// The session called `from` is called `to` now: a split of it stays
    /// open under its new name.
    pub fn renamed(&mut self, from: &str, to: &str) {
        for split in &mut self.splits {
            if split == from {
                *split = to.to_string();
            }
        }
    }

    /// Hands the keyboard to the selected session, in whichever pane shows
    /// it, if it can take keys.
    pub fn type_into_selected(&mut self) {
        let Some(slot) = self.selected_slot() else {
            return;
        };
        if self.can_type_into(slot) {
            self.focus_pane(slot);
        }
    }

    /// The pane that shows the selected session: its split, if it has one,
    /// or else the pane that follows the selection.
    fn selected_slot(&self) -> Option<Slot> {
        let selected = self.selected()?;
        let slot = match self.splits.iter().position(|split| *split == selected.name) {
            Some(index) => Slot::Split(index),
            None => Slot::Selected,
        };
        Some(slot)
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        self.notice = None;
        // Only `y` says yes; any other key says no.
        if let Some(confirm) = self.confirm.take() {
            if key.code == KeyCode::Char('y') {
                return Some(confirm.action());
            }
            return None;
        }
        if self.prompt.is_some() {
            return self.on_prompt_key(key);
        }
        match self.focus {
            Focus::Sidebar => self.on_sidebar_key(key),
            Focus::Pane(slot) => self.on_pane_key(slot, key),
        }
    }

    /// What the mouse does, when no program in a pane has taken it: a click
    /// selects a session or hands a pane the keyboard, and the wheel moves
    /// the selection, or scrolls a pane through its history.
    pub fn on_mouse(&mut self, kind: MouseEventKind, hit: Hit) -> Option<Action> {
        // A question on the footer waits for its answer from the keyboard.
        if self.prompt.is_some() || self.confirm.is_some() {
            return None;
        }
        let click = kind == MouseEventKind::Down(MouseButton::Left);
        if click {
            self.notice = None;
        }
        match (kind, hit) {
            (_, Hit::SidebarRow(row)) if click => self.click_row(row),
            (_, Hit::Pane { slot, .. }) if click => {
                if self.can_type_into(slot) {
                    self.focus_pane(slot);
                }
            }
            (MouseEventKind::ScrollUp, Hit::SidebarRow(_) | Hit::Sidebar) => {
                self.move_selection(-1);
            }
            (MouseEventKind::ScrollDown, Hit::SidebarRow(_) | Hit::Sidebar) => {
                self.move_selection(1);
            }
            (MouseEventKind::ScrollUp, Hit::Pane { slot, .. }) => {
                return Some(Action::ScrollBack(slot));
            }
            (MouseEventKind::ScrollDown, Hit::Pane { slot, .. }) => {
                return Some(Action::ScrollForward(slot));
            }
            _ => {}
        }
        None
    }

    /// A click on a sidebar row: on a session, selects it and gives the
    /// sidebar the keyboard. Headings don't do anything.
    fn click_row(&mut self, row: usize) {
        if let Some(Row::Session(index)) = self.rows().get(row) {
            self.selected = *index;
            self.focus = Focus::Sidebar;
        }
    }

    fn on_sidebar_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Enter => self.enter(),
            KeyCode::Tab => self.move_to_pane(Direction::Forward),
            KeyCode::BackTab => self.move_to_pane(Direction::Back),
            KeyCode::Char('s') => self.toggle_split(),
            KeyCode::PageUp => return Some(Action::PageBack(self.selected_slot()?)),
            KeyCode::PageDown => return Some(Action::PageForward(self.selected_slot()?)),
            KeyCode::Char('n') => self.ask_for_command(self.selected_place()),
            KeyCode::Char('w') => self.ask(Question::Branch, ""),
            KeyCode::Char('W') => self.ask_to_remove_worktree(),
            KeyCode::Char('r') => self.ask_for_name(),
            KeyCode::Char('x') => self.confirm = Some(Confirm::Kill(self.selected()?.name.clone())),
            KeyCode::Char('u') => self.select_next_needing_user(),
            KeyCode::Char('q') => return Some(Action::Quit),
            _ => {}
        }
        None
    }

    /// Enter on a session: types into it while it runs, or, once it has
    /// ended, offers to start it again.
    fn enter(&mut self) {
        let Some(selected) = self.selected() else {
            return;
        };
        if selected.state == State::Running {
            self.type_into_selected();
        } else {
            self.confirm = Some(Confirm::Respawn(selected.name.clone()));
        }
    }

    /// Asks for a new name for the selected session, starting from the
    /// one it has.
    fn ask_for_name(&mut self) {
        if let Some(selected) = self.selected() {
            let name = selected.name.clone();
            self.ask(Question::Rename(name.clone()), &name);
        }
    }

    /// Asks before removing the selected session's worktree. Only a linked
    /// worktree goes, and only once nothing runs in it any more: removing
    /// it would pull the directory out from under them.
    fn ask_to_remove_worktree(&mut self) {
        let Some(selected) = self.selected() else {
            return;
        };
        let name = selected.name.clone();
        let Some(worktree) = selected.worktree.clone() else {
            self.notify(format!("{name} isn't in a git worktree"));
            return;
        };
        if worktree.main {
            self.notify("the main worktree can't be removed".into());
            return;
        }
        let branch = worktree.branch.unwrap_or_else(|| "(detached)".into());
        let running: Vec<&str> = self
            .sessions
            .iter()
            .filter(|session| session.state == State::Running)
            .filter(|session| {
                let in_it = session.worktree.as_ref();
                in_it.is_some_and(|w| w.path == worktree.path)
            })
            .map(|session| session.name.as_str())
            .collect();
        if running.is_empty() {
            self.confirm = Some(Confirm::RemoveWorktree {
                path: worktree.path,
                branch,
            });
        } else {
            let notice = format!("{} still running in {branch}", running.join(", "));
            self.notify(notice);
        }
    }

    fn ask(&mut self, question: Question, answer: &str) {
        self.prompt = Some(Prompt {
            question,
            input: TextInput::with_text(answer),
        });
    }

    /// Asks what to run in a new session at `place`, starting out with the
    /// command line used last.
    fn ask_for_command(&mut self, place: Place) {
        let last = self.last_command.clone();
        self.ask(Question::Command(place), &last);
    }

    /// Keys while a question is asked: Enter answers it, Esc gives up, and
    /// every other key edits the answer.
    fn on_prompt_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.prompt = None;
                None
            }
            KeyCode::Enter => {
                let prompt = self.prompt.take()?;
                self.answer(prompt)
            }
            _ => {
                if let Some(prompt) = &mut self.prompt {
                    prompt.input.on_key(&key);
                }
                None
            }
        }
    }

    /// What an answered question leads to: a branch, to asking what to run
    /// there; a command line, to a new session.
    fn answer(&mut self, prompt: Prompt) -> Option<Action> {
        let answer = prompt.input.text().trim().to_string();
        match prompt.question {
            Question::Branch => {
                if !answer.is_empty() {
                    let base = self.worktree_base();
                    self.ask_for_command(Place::NewWorktree {
                        branch: answer,
                        base,
                    });
                }
                None
            }
            Question::Command(place) => {
                // Kept even when it doesn't read, so that the next `n`
                // brings it back to put right.
                self.last_command = answer.clone();
                match command_line::parse(&answer) {
                    Ok(command) => Some(Action::Start { place, command }),
                    Err(err) => {
                        self.notify(err);
                        None
                    }
                }
            }
            // An empty answer, or the name it already has, changes nothing.
            Question::Rename(name) => {
                if answer.is_empty() || answer == name {
                    None
                } else {
                    Some(Action::Rename {
                        name,
                        new_name: answer,
                    })
                }
            }
        }
    }

    /// Where `n` starts a session: where the selected session runs, or the
    /// TUI's own directory when nothing is selected.
    fn selected_place(&self) -> Place {
        let dir = self.selected().map(|session| session.cwd.clone());
        Place::Directory(dir)
    }

    /// Where a new worktree is made from: the selected session's project,
    /// or `None` for the TUI's own directory when nothing in a repository
    /// is selected.
    fn worktree_base(&self) -> Option<PathBuf> {
        let worktree = self.selected()?.worktree.as_ref()?;
        Some(worktree.project_path.clone())
    }

    /// Every key goes to the pane's session, Tab too, since shells and
    /// agents need it. Kept back are Ctrl+\, which returns to the sidebar,
    /// and Shift+PageUp and Shift+PageDown, how terminals have always
    /// scrolled back: they page through the pane's history. Unshifted, the
    /// page keys go to the session like any other.
    fn on_pane_key(&mut self, slot: Slot, key: KeyEvent) -> Option<Action> {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            _ if keys::is_hand_back(&key) => {
                self.focus = Focus::Sidebar;
                None
            }
            KeyCode::PageUp if shift => Some(Action::PageBack(slot)),
            KeyCode::PageDown if shift => Some(Action::PageForward(slot)),
            _ => Some(Action::Type { to: slot, key }),
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

    /// Selects the next session that needs the user, or says there's none.
    fn select_next_needing_user(&mut self) {
        match self.next_needing_user() {
            Some(index) => self.selected = index,
            None => self.notify("nothing needs you".to_string()),
        }
    }

    /// The next session that needs the user, going down the sidebar from
    /// the selected one and round again: one waiting on them comes before
    /// one that's done. Starting after the selection means that pressing
    /// the key again moves on to the next.
    fn next_needing_user(&self) -> Option<usize> {
        let count = self.sessions.len();
        let in_turn: Vec<usize> = (1..=count)
            .map(|step| (self.selected + step) % count)
            .collect();
        for wanted in [Activity::Waiting, Activity::Done] {
            let found = in_turn.iter().copied().find(|&index| {
                let session = &self.sessions[index];
                session.state == State::Running && session.activity == Some(wanted)
            });
            if found.is_some() {
                return found;
            }
        }
        None
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
            id: name.into(),
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

    fn doing(name: &str, activity: Activity) -> SessionInfo {
        SessionInfo {
            activity: Some(activity),
            ..session(name)
        }
    }

    #[test]
    fn u_goes_to_a_session_waiting_on_the_user_before_one_thats_done() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            doing("finished", Activity::Done),
            session("quiet"),
            doing("asking", Activity::Waiting),
        ]);
        app.select("quiet");
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(selected_name(&app), Some("asking"));
    }

    #[test]
    fn u_again_moves_on_to_the_next_and_round() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            doing("one", Activity::Waiting),
            session("rest"),
            doing("two", Activity::Waiting),
        ]);
        app.select("rest");
        let mut visited = Vec::new();
        for _ in 0..3 {
            press(&mut app, KeyCode::Char('u'));
            visited.push(selected_name(&app).unwrap().to_string());
        }
        assert_eq!(visited, ["one", "two", "one"]);
    }

    #[test]
    fn done_sessions_are_next_once_nothing_is_waiting() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            doing("a", Activity::Done),
            session("b"),
            doing("c", Activity::Done),
        ]);
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(selected_name(&app), Some("c"));
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(selected_name(&app), Some("a"));
    }

    #[test]
    fn with_nothing_needing_the_user_u_says_so() {
        let mut app = App::new(None);
        let gone = SessionInfo {
            activity: Some(Activity::Done),
            ..ended("gone")
        };
        app.set_sessions(vec![session("a"), gone]);
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(selected_name(&app), Some("a"));
        assert_eq!(app.notice(), Some("nothing needs you"));
    }

    #[test]
    fn the_config_sets_what_a_new_session_starts_out_with() {
        let mut app = app_with(&["a"]);
        app.set_first_command("codex".to_string());
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.prompt().unwrap().input.text(), "codex");
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
    fn q_asks_to_quit() {
        let mut app = app_with(&["a"]);
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

    /// Clears the answer the prompt started out with, and types `text`.
    fn answer(app: &mut App, text: &str) {
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(app, text);
    }

    fn prompt_text(app: &App) -> Option<&str> {
        app.prompt().map(|prompt| prompt.input.text())
    }

    fn start(place: Place, command: &[&str]) -> Option<Action> {
        let command = command.iter().map(|word| word.to_string()).collect();
        Some(Action::Start { place, command })
    }

    #[test]
    fn n_starts_out_with_claude_and_enter_starts_it_where_the_selection_runs() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(prompt_text(&app), Some("claude"));

        let place = Place::Directory(Some(PathBuf::from("/")));
        assert_eq!(press(&mut app, KeyCode::Enter), start(place, &["claude"]));
        assert!(app.prompt().is_none());
    }

    #[test]
    fn an_agents_first_prompt_goes_as_one_argument() {
        let mut app = App::new(None);
        press(&mut app, KeyCode::Char('n'));
        type_text(&mut app, " fix the login bug");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(Place::Directory(None), &["claude", "fix the login bug"])
        );
    }

    #[test]
    fn the_next_n_starts_out_with_the_command_used_last() {
        let mut app = App::new(None);
        press(&mut app, KeyCode::Char('n'));
        answer(&mut app, "codex --model o3");
        press(&mut app, KeyCode::Enter);

        press(&mut app, KeyCode::Char('n'));
        assert_eq!(prompt_text(&app), Some("codex --model o3"));
    }

    #[test]
    fn an_empty_line_starts_the_shell() {
        let mut app = App::new(None);
        press(&mut app, KeyCode::Char('n'));
        answer(&mut app, "");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(Place::Directory(None), &[])
        );
    }

    #[test]
    fn a_line_that_does_not_read_says_why_and_comes_back_to_put_right() {
        let mut app = App::new(None);
        press(&mut app, KeyCode::Char('n'));
        answer(&mut app, "echo 'oops");
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert_eq!(app.notice(), Some("a quote isn't closed"));

        press(&mut app, KeyCode::Char('n'));
        assert_eq!(prompt_text(&app), Some("echo 'oops"));
    }

    #[test]
    fn keys_go_to_the_prompt_while_it_is_open() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('w'));
        // q and x would quit and kill on the list; here they're letters.
        type_text(&mut app, "qx");
        assert_eq!(prompt_text(&app), Some("qx"));
        assert_eq!(selected_name(&app), Some("a"));
    }

    #[test]
    fn w_asks_for_a_branch_then_what_to_run_in_the_new_worktree() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_project("agent", "app")]);
        press(&mut app, KeyCode::Char('w'));
        type_text(&mut app, "fix/typo");
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert_eq!(prompt_text(&app), Some("claude"));

        let place = Place::NewWorktree {
            branch: "fix/typo".into(),
            base: Some(PathBuf::from("/code/app")),
        };
        answer(&mut app, "npm test");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(place, &["npm", "test"])
        );
        assert!(app.prompt().is_none());
    }

    #[test]
    fn esc_or_an_empty_branch_gives_up_on_the_new_worktree() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('w'));
        type_text(&mut app, "feat");
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert!(app.prompt().is_none());

        press(&mut app, KeyCode::Char('w'));
        type_text(&mut app, "  ");
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert!(app.prompt().is_none());
    }

    #[test]
    fn outside_a_repository_the_worktree_is_made_from_the_tuis_directory() {
        let mut app = app_with(&["shell"]);
        press(&mut app, KeyCode::Char('w'));
        type_text(&mut app, "feat");
        press(&mut app, KeyCode::Enter);
        let place = Place::NewWorktree {
            branch: "feat".into(),
            base: None,
        };
        assert_eq!(press(&mut app, KeyCode::Enter), start(place, &["claude"]));
    }

    #[test]
    fn x_asks_first_and_only_y_kills() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        assert_eq!(app.confirm(), Some(&Confirm::Kill("b".into())));
        assert_eq!(press(&mut app, KeyCode::Char('n')), None);
        assert_eq!(app.confirm(), None);
        assert!(app.prompt().is_none(), "the n answered the question");

        press(&mut app, KeyCode::Char('x'));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::Kill("b".into()))
        );
    }

    #[test]
    fn x_with_nothing_selected_asks_nothing() {
        let mut app = App::new(None);
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        assert_eq!(app.confirm(), None);
    }

    #[test]
    fn r_asks_for_a_new_name_starting_from_the_old_one() {
        let mut app = app_with(&["a", "fixer"]);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(prompt_text(&app), Some("fixer"));

        answer(&mut app, "login-fixer");
        let renamed = Action::Rename {
            name: "fixer".into(),
            new_name: "login-fixer".into(),
        };
        assert_eq!(press(&mut app, KeyCode::Enter), Some(renamed));
    }

    #[test]
    fn a_rename_to_nothing_or_the_same_name_changes_nothing() {
        let mut app = app_with(&["fixer"]);
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(press(&mut app, KeyCode::Enter), None);

        press(&mut app, KeyCode::Char('r'));
        answer(&mut app, "");
        assert_eq!(press(&mut app, KeyCode::Enter), None);
    }

    #[test]
    fn a_split_stays_open_when_its_session_is_renamed() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('s'));
        app.renamed("a", "c");
        app.set_sessions(vec![session("c"), session("b")]);
        assert_eq!(app.splits(), ["c"]);
    }

    #[test]
    fn the_tuis_own_session_is_told_by_its_id_whatever_its_name() {
        let mut renamed = session("renamed");
        renamed.id = "me".into();
        let mut app = App::new(Some("me".into()));
        app.set_sessions(vec![renamed]);
        assert!(app.selected_is_own());
    }

    #[test]
    fn enter_on_an_ended_session_offers_to_start_it_again() {
        let mut app = App::new(None);
        app.set_sessions(vec![ended("done")]);
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert_eq!(app.confirm(), Some(&Confirm::Respawn("done".into())));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::Respawn("done".into()))
        );
    }

    /// A session in the worktree of the `app` project on `branch`, the main
    /// one when `branch` is "main".
    fn in_worktree(name: &str, branch: &str, state: State) -> SessionInfo {
        SessionInfo {
            state,
            worktree: Some(Worktree {
                project: "app".into(),
                project_path: PathBuf::from("/code/app"),
                path: PathBuf::from(format!("/code/app.worktrees/{branch}")),
                main: branch == "main",
                branch: Some(branch.into()),
            }),
            ..session(name)
        }
    }

    #[test]
    fn shift_w_asks_before_removing_a_worktree_nothing_runs_in() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            in_worktree("fixer", "fix", State::Exited { code: 0 }),
            in_worktree("other", "main", State::Running),
        ]);
        app.select("fixer");
        press(&mut app, KeyCode::Char('W'));
        let removal = Confirm::RemoveWorktree {
            path: PathBuf::from("/code/app.worktrees/fix"),
            branch: "fix".into(),
        };
        assert_eq!(app.confirm(), Some(&removal));
        assert_eq!(
            app.confirm().unwrap().question(),
            "remove worktree fix? y/n"
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::RemoveWorktree("/code/app.worktrees/fix".into()))
        );
    }

    #[test]
    fn shift_w_refuses_while_a_session_runs_in_the_worktree() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            in_worktree("fixer", "fix", State::Exited { code: 0 }),
            in_worktree("tests", "fix", State::Running),
        ]);
        app.select("fixer");
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(app.confirm(), None);
        assert_eq!(app.notice(), Some("tests still running in fix"));
    }

    #[test]
    fn shift_w_leaves_the_main_worktree_and_sessions_outside_git_alone() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_worktree("planner", "main", State::Running)]);
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(app.notice(), Some("the main worktree can't be removed"));

        app.set_sessions(vec![session("shell")]);
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(app.notice(), Some("shell isn't in a git worktree"));
        assert_eq!(app.confirm(), None);
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
    fn page_keys_in_the_sidebar_page_the_selected_sessions_pane() {
        let mut app = app_with_splits(&["a", "b"], 1);
        app.select("b");
        assert_eq!(
            press(&mut app, KeyCode::PageUp),
            Some(Action::PageBack(Slot::Selected))
        );
        // A session split off is paged in its split.
        app.select("a");
        assert_eq!(
            press(&mut app, KeyCode::PageDown),
            Some(Action::PageForward(Slot::Split(0)))
        );
        assert_eq!(press(&mut App::new(None), KeyCode::PageUp), None);
    }

    #[test]
    fn in_a_pane_only_shifted_page_keys_page_its_history() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Enter);
        let shifted = |code| KeyEvent::new(code, KeyModifiers::SHIFT);
        assert_eq!(
            app.on_key(shifted(KeyCode::PageUp)),
            Some(Action::PageBack(Slot::Selected))
        );
        assert_eq!(
            app.on_key(shifted(KeyCode::PageDown)),
            Some(Action::PageForward(Slot::Selected))
        );
        let page_up = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
        let typed = Action::Type {
            to: Slot::Selected,
            key: page_up,
        };
        assert_eq!(app.on_key(page_up), Some(typed));
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

    const CLICK: MouseEventKind = MouseEventKind::Down(MouseButton::Left);

    /// The sidebar row `name` is drawn on.
    fn row_of(app: &App, name: &str) -> usize {
        let index = app.sessions().iter().position(|s| s.name == name).unwrap();
        app.rows()
            .iter()
            .position(|row| *row == Row::Session(index))
            .unwrap()
    }

    #[test]
    fn clicking_a_session_row_selects_it_and_a_heading_does_nothing() {
        let mut app = app_with(&["a", "b", "c"]);
        app.on_mouse(CLICK, Hit::SidebarRow(row_of(&app, "c")));
        assert_eq!(selected_name(&app), Some("c"));

        // Sessions outside git sit under two headings, on the first rows.
        assert!(matches!(app.rows()[0], Row::OutsideGit));
        app.on_mouse(CLICK, Hit::SidebarRow(0));
        assert_eq!(selected_name(&app), Some("c"));
    }

    #[test]
    fn clicking_a_row_gives_the_sidebar_the_keyboard() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Enter);
        app.on_mouse(CLICK, Hit::SidebarRow(row_of(&app, "b")));
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn clicking_a_pane_hands_it_the_keyboard_if_it_takes_keys() {
        let mut app = app_with(&["a"]);
        let pane = Hit::Pane {
            slot: Slot::Selected,
            cell: Some((2, 3)),
        };
        app.on_mouse(CLICK, pane);
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));

        let mut app = App::new(None);
        app.set_sessions(vec![ended("done")]);
        app.on_mouse(CLICK, pane);
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn the_wheel_over_the_sidebar_moves_the_selection() {
        let mut app = app_with(&["a", "b", "c"]);
        app.on_mouse(MouseEventKind::ScrollDown, Hit::Sidebar);
        app.on_mouse(MouseEventKind::ScrollDown, Hit::SidebarRow(0));
        assert_eq!(selected_name(&app), Some("c"));
        app.on_mouse(MouseEventKind::ScrollUp, Hit::Sidebar);
        assert_eq!(selected_name(&app), Some("b"));
    }

    #[test]
    fn the_wheel_over_a_pane_scrolls_its_history() {
        let mut app = app_with(&["a"]);
        let over = |slot| Hit::Pane { slot, cell: None };
        assert_eq!(
            app.on_mouse(MouseEventKind::ScrollUp, over(Slot::Selected)),
            Some(Action::ScrollBack(Slot::Selected))
        );
        assert_eq!(
            app.on_mouse(MouseEventKind::ScrollDown, over(Slot::Selected)),
            Some(Action::ScrollForward(Slot::Selected))
        );
    }

    #[test]
    fn the_mouse_waits_while_a_question_is_asked() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('x'));
        app.on_mouse(CLICK, Hit::SidebarRow(row_of(&app, "b")));
        assert_eq!(selected_name(&app), Some("a"));
        assert_eq!(
            app.confirm(),
            Some(&Confirm::Kill("a".into())),
            "the question is still asked"
        );
    }
}
