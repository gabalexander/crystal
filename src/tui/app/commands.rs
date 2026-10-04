//! The layout commands `crystal tab` and `crystal pane` send the TUI (see
//! [`crate::layout`]), carried out on its state, and the layout it answers
//! with. A command about a session works on the tab that holds it, in
//! front or not, and leaves the tab in front where it is, but for those
//! whose point is to go somewhere: making a tab, going to one, focusing a
//! session, and a layout applied that says which tab is in front.

use super::{Action, App, Slot, resize_step};
use crate::flow_run::FlowRun;
use crate::layout::{Command, Layout, NO_TUI, Order, TabLayout, Tile};
use crate::notify::Presence;
use crate::protocol::SessionInfo;
use crate::session::UNSEEN_SIZE;
use crate::tui::split_tree::{Direction, Pane, SplitTree, Way};
use crate::tui::tabs::Tabs;
use crate::tui::ui::Areas;
use ratatui::layout::Rect;

/// What came of a layout command carried out with no TUI open.
pub struct Alone {
    /// The layout it came to, as a TUI would answer with.
    pub layout: Layout,
    /// The tabs as they are now, to keep for the next TUI to open with.
    pub tabs: Tabs,
    /// The sessions a tab closed with them leaves to be killed.
    pub kill: Vec<String>,
}

impl App {
    /// Carries out a layout command with no TUI open, the way the TUI used
    /// last would have: on `tabs`, as the TUIs last kept them, with
    /// `sessions` and `flows` as they are now, on a screen the size a
    /// session has when nobody is looking at it.
    pub fn obey_alone(
        sessions: Vec<SessionInfo>,
        flows: Vec<FlowRun>,
        tabs: Tabs,
        order: Order,
    ) -> Result<Alone, String> {
        // The title is the TUI's terminal's, and there's none.
        if let Command::Title { .. } = order.command {
            return Err(format!("{NO_TUI} to show it"));
        }
        let mut app = App::new(None);
        let (rows, cols) = UNSEEN_SIZE;
        let screen = Rect::new(0, 0, cols, rows);
        app.set_screen(screen);
        app.flows = flows;
        app.set_sessions(sessions);
        app.set_tabs(tabs);
        app.set_tiles(Areas::of(&app, screen).tiles);
        let kill = match app.obey(order)? {
            Some(Action::KillAll(names)) => names,
            _ => Vec::new(),
        };
        Ok(Alone {
            layout: app.layout(),
            tabs: app.tabs_to_keep(),
            kill,
        })
    }

    /// Carries out a layout command from the command line, or says why it
    /// can't. Closing a tab with its sessions leaves killing them to the
    /// event loop.
    pub fn obey(&mut self, order: Order) -> Result<Option<Action>, String> {
        let done = self.carry_out(order.command, order.caller.as_deref());
        self.remember_shown();
        done
    }

    /// The tabs and their panes, as `crystal layout` prints them.
    pub fn layout(&self) -> Layout {
        let tabs = self.tabs.all().iter().enumerate();
        let tabs = tabs.map(|(index, tab)| {
            let shown = self.selection_shows(index);
            let panes = tab.panes.fold(
                &mut |pane| match pane {
                    Pane::Selection => Tile::Pane {
                        session: shown.clone(),
                        selection: true,
                    },
                    Pane::Session(name) => Tile::Pane {
                        session: Some(name.clone()),
                        selection: false,
                    },
                },
                &mut |way, ratio, first, second| Tile::Split {
                    way,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                },
            );
            let sessions = self.sessions_in(index).into_iter();
            TabLayout {
                number: index + 1,
                name: tab.name.clone(),
                current: index == self.tabs.current_index(),
                zoomed: tab.zoomed,
                sessions: sessions.map(|at| self.sessions[at].name.clone()).collect(),
                selected: self.selected_in(index),
                floating: tab.floating.clone(),
                panes,
            }
        });
        Layout {
            tabs: tabs.collect(),
            // Where the user is, the daemon says, from every TUI.
            presence: Presence::Unknown,
        }
    }

    fn carry_out(
        &mut self,
        command: Command,
        caller: Option<&str>,
    ) -> Result<Option<Action>, String> {
        match command {
            Command::Show => {}
            Command::NewTab { name } => self.add_tab(name.as_deref()),
            Command::SelectTab { tab } => {
                let index = self.tab_named(&tab)?;
                self.go_to_tab(index);
            }
            Command::RenameTab { tab, name } => {
                let index = self.tab_named(&tab)?;
                self.tabs.tab_mut(index).rename(&name);
            }
            Command::CloseTab { tab, kill } => return self.close_tab_named(tab.as_deref(), kill),
            Command::MoveToTab { session, tab } => {
                let name = self.session_named(&session)?;
                let to = self.tab_named(&tab)?;
                self.move_session(&name, to);
            }
            Command::ReorderTab { tab, position } => {
                let from = self.tab_named(&tab)?;
                let to = position.checked_sub(1).ok_or("tabs are numbered from 1")?;
                if !self.tabs.move_tab(from, to) {
                    let count = match self.tabs.all().len() {
                        1 => "only one tab".to_string(),
                        count => format!("{count} tabs"),
                    };
                    return Err(format!("there's no place {position} among {count}"));
                }
            }
            Command::Split {
                session,
                beside,
                way,
                ratio,
            } => {
                let name = self.session_named(&session)?;
                let beside = self.subject(beside.as_deref(), caller)?;
                self.split_beside(&name, &beside, way, ratio)?;
            }
            // Bringing the terminal to the front is the event loop's.
            Command::Focus { session, raise: _ } => {
                let name = self.session_named(&session)?;
                self.focus_on(&name);
            }
            Command::FocusToward { toward } => {
                let from = self.subject(None, caller)?;
                self.focus_toward_from(&from, toward)?;
            }
            Command::Resize {
                session,
                toward,
                cells,
            } => {
                let name = self.subject(session.as_deref(), caller)?;
                let cells = cells.unwrap_or_else(|| resize_step(toward));
                self.resize_pane_of(&name, toward, cells)?;
            }
            Command::SwapToward { session, toward } => {
                let name = self.subject(session.as_deref(), caller)?;
                self.swap_toward(&name, toward)?;
            }
            Command::Swap { session, with } => {
                let name = self.subject(session.as_deref(), caller)?;
                let with = self.session_named(&with)?;
                self.swap_with(&name, &with)?;
            }
            Command::Ratio {
                session,
                way,
                share,
            } => {
                let name = self.subject(session.as_deref(), caller)?;
                self.set_share_of(&name, way, share)?;
            }
            Command::Close { session } => {
                let name = self.subject(session.as_deref(), caller)?;
                self.close_pane_of(&name)?;
            }
            Command::Zoom { session, on: true } => {
                let name = self.subject(session.as_deref(), caller)?;
                self.zoom_on(&name);
            }
            Command::Zoom { session, on: false } => {
                let index = self.tab_for(session.as_deref(), caller)?;
                self.tabs.tab_mut(index).zoomed = false;
            }
            Command::Equalize => {
                let index = self.tab_for(None, caller)?;
                self.tabs.tab_mut(index).panes.equalize();
            }
            Command::Float { session, on: true } => {
                let name = self.subject(session.as_deref(), caller)?;
                self.float(&name)?;
            }
            Command::Float { session, on: false } => {
                let index = self.tab_for(session.as_deref(), caller)?;
                self.put_float_back_in(index);
            }
            Command::Title { text } => self.title_override = text,
            Command::Apply { tabs, replace } => self.apply(&tabs, replace)?,
        }
        Ok(None)
    }

    /// The session called `name`, if there is one.
    fn session_named(&self, name: &str) -> Result<String, String> {
        match self.position(name) {
            Some(_) => Ok(name.to_string()),
            None => Err(format!("there's no session called {name}")),
        }
    }

    /// The session a command was run in, by its id, `caller`, when it's one
    /// of the TUI's.
    fn caller_name(&self, caller: Option<&str>) -> Option<String> {
        let caller = caller.and_then(|id| self.sessions.iter().find(|session| session.id == id));
        caller.map(|session| session.name.clone())
    }

    /// The session a command is about: the one it names, or else the one it
    /// was run in, or else the one selected.
    fn subject(&self, named: Option<&str>, caller: Option<&str>) -> Result<String, String> {
        match named {
            Some(name) => self.session_named(name),
            None => (self.caller_name(caller).or_else(|| self.selected_name()))
                .ok_or_else(|| "there's no session selected: name one".to_string()),
        }
    }

    /// The tab a command is about: the one holding the session it names, or
    /// else the one it was run in, or else the one in front.
    fn tab_for(&self, named: Option<&str>, caller: Option<&str>) -> Result<usize, String> {
        let name = match named {
            Some(name) => Some(self.session_named(name)?),
            None => self.caller_name(caller),
        };
        Ok(name.map_or(self.tabs.current_index(), |name| self.tab_holding(&name)))
    }

    /// Refuses to show the session this TUI runs in: it would show itself.
    fn can_show(&self, name: &str) -> Result<(), String> {
        if self
            .sessions
            .iter()
            .any(|session| session.name == name && self.is_own(session))
        {
            return Err("crystal can't show the session it runs in".into());
        }
        Ok(())
    }

    /// The tab holding the session called `name`. Every session is in one,
    /// once the TUI has heard of it.
    fn tab_holding(&self, name: &str) -> usize {
        self.tabs.tab_of(name).unwrap_or(self.tabs.current_index())
    }

    /// The tab `tab` names: its number, from 1, or else its name.
    fn tab_named(&self, tab: &str) -> Result<usize, String> {
        let tabs = self.tabs.all();
        let numbered = tab.parse::<usize>().ok();
        let numbered = numbered.filter(|number| (1..=tabs.len()).contains(number));
        numbered
            .map(|number| number - 1)
            .or_else(|| tabs.iter().position(|held| held.name == tab))
            .ok_or_else(|| format!("there's no tab {tab}"))
    }

    /// The session selected in the tab at `index`: the one in front's now,
    /// or the one another tab will be on when it comes to the front.
    fn selected_in(&self, index: usize) -> Option<String> {
        if index == self.tabs.current_index() {
            return self.selected_name();
        }
        let tab = &self.tabs.all()[index];
        let remembered = tab.selected.clone().filter(|name| tab.holds(name));
        let first = || {
            let first = self.sessions_in(index).into_iter().next();
            first.map(|at| self.sessions[at].name.clone())
        };
        remembered.or_else(first)
    }

    /// The session the selection's pane of the tab at `index` shows: the
    /// session selected there, unless it has a pane of its own, when it's
    /// the one the pane showed last.
    fn selection_shows(&self, index: usize) -> Option<String> {
        if index == self.tabs.current_index() {
            let shown = self.pane_session(Slot::Selected)?;
            return (!self.has_own_pane(&shown.name)).then(|| shown.name.clone());
        }
        let tab = &self.tabs.all()[index];
        let own_pane = |name: &str| tab.is_split(name) || tab.floating.as_deref() == Some(name);
        let selected = self.selected_in(index)?;
        if !own_pane(&selected) {
            return Some(selected);
        }
        let shown = tab.shown.clone();
        shown.filter(|name| tab.holds(name) && !own_pane(name))
    }

    /// The pane of the tab at `index` that shows the session called `name`:
    /// its own, split off, or the selection's while that shows it. Or why
    /// none does.
    fn pane_of_session(&self, index: usize, name: &str) -> Result<Pane, String> {
        let tab = &self.tabs.all()[index];
        if tab.is_split(name) {
            return Ok(Pane::Session(name.to_string()));
        }
        if self.selection_shows(index).as_deref() == Some(name) {
            return Ok(Pane::Selection);
        }
        if tab.floating.as_deref() == Some(name) {
            return Err(format!(
                "{name} floats over the panes: `crystal pane float --off` puts it back"
            ));
        }
        Err(format!(
            "{name} isn't on screen: `crystal pane split {name}` gives it a pane"
        ))
    }

    /// Selects the session called `name` in the tab at `index`, which holds
    /// it, leaving the tab in front where it is.
    fn select_in(&mut self, index: usize, name: &str) {
        if index == self.tabs.current_index() {
            self.select(name);
        } else {
            self.tabs.tab_mut(index).selected = Some(name.to_string());
        }
    }

    /// Changes the panes of the tab at `index`: the one in front's through
    /// [`App::change_panes`], so the keyboard follows its pane.
    fn change_panes_in(&mut self, index: usize, change: impl FnOnce(&mut SplitTree)) {
        if index == self.tabs.current_index() {
            self.change_panes(change);
        } else {
            change(&mut self.tabs.tab_mut(index).panes);
        }
    }

    /// Closes the split of the session called `name` in the tab at `index`,
    /// if it has one.
    fn close_split_in(&mut self, index: usize, name: &str) {
        let split = Pane::Session(name.to_string());
        self.change_panes_in(index, |panes| {
            panes.close(&split);
        });
    }

    /// Puts back the session floating over the tab at `index`, if one is.
    fn put_float_back_in(&mut self, index: usize) {
        if index == self.tabs.current_index() {
            self.put_float_back();
        } else {
            self.tabs.tab_mut(index).floating = None;
        }
    }

    /// Makes a tab after the others, named `name` if there's one, and brings
    /// it to the front, where sessions started from then on go. Unlike `t`,
    /// it starts nothing in it.
    fn add_tab(&mut self, name: Option<&str>) {
        let index = self.tabs.add();
        self.go_to_tab(index);
        if let Some(name) = name {
            self.tabs.tab_mut(index).rename(name);
        }
    }

    /// Closes the tab `tab` names, or the one in front. One with sessions
    /// in it closes only with `kill`, and asks for them to be killed, as `&`
    /// does once it's said yes to; the session this TUI runs in is spared,
    /// and joins the tab in front.
    fn close_tab_named(&mut self, tab: Option<&str>, kill: bool) -> Result<Option<Action>, String> {
        let index = match tab {
            Some(tab) => self.tab_named(tab)?,
            None => self.tabs.current_index(),
        };
        let number = index + 1;
        if self.tabs.all().len() == 1 {
            return Err(format!("tab {number} is the only tab"));
        }
        let sessions: Vec<String> = (self.sessions_in(index).into_iter())
            .map(|at| &self.sessions[at])
            .filter(|session| !self.is_own(session))
            .map(|session| session.name.clone())
            .collect();
        if !sessions.is_empty() && !kill {
            let held = sessions.join(", ");
            return Err(format!(
                "tab {number} has sessions in it ({held}): --kill closes it and kills them"
            ));
        }
        if index == self.tabs.current_index() {
            self.close_tab_in_front();
        } else {
            self.tabs.close_at(index);
            self.place_sessions();
        }
        Ok((!sessions.is_empty()).then_some(Action::KillAll(sessions)))
    }

    /// Shows the session called `name` in a pane of its own, split off
    /// `way` from the pane that shows `beside`, which keeps `ratio` of the
    /// room, in `beside`'s tab: `name` moves there from another tab, and
    /// out of a pane it had there already. The pane split is `beside`'s own,
    /// or else the selection's, which `beside` is selected for if it doesn't
    /// show it.
    fn split_beside(
        &mut self,
        name: &str,
        beside: &str,
        way: Way,
        ratio: f32,
    ) -> Result<(), String> {
        if name == beside {
            return Err(format!("{name} can't go beside itself"));
        }
        self.can_show(name)?;
        let index = self.tab_holding(beside);
        let tab = &self.tabs.all()[index];
        if tab.floating.as_deref() == Some(beside) {
            return Err(format!(
                "{beside} floats: `crystal pane float --off` puts it back first"
            ));
        }
        let new = Pane::Session(name.to_string());
        let mut panes = tab.panes.clone();
        panes.close(&new);
        let own = Pane::Session(beside.to_string());
        let at = if panes.contains(&own) {
            own
        } else {
            Pane::Selection
        };
        if !panes.has_room(&at, way, self.tiles) {
            let place = match way {
                Way::Right => "beside",
                Way::Down => "below",
            };
            return Err(format!("no room for another pane {place} {beside}"));
        }
        panes.split(&at, way, ratio, new);

        self.move_session(name, index);
        if self.tabs.all()[index].floating.as_deref() == Some(name) {
            self.put_float_back_in(index);
        }
        self.change_panes_in(index, |tree| *tree = panes);
        if at == Pane::Selection && self.selection_shows(index).as_deref() != Some(beside) {
            self.select_in(index, beside);
        }
        Ok(())
    }

    /// Selects the session called `name`, bringing its tab to the front,
    /// and hands its pane the keyboard, if it takes keys.
    fn focus_on(&mut self, name: &str) {
        self.select(name);
        self.type_into_selected();
    }

    /// Focuses the session in the pane `toward` from the one that shows the
    /// session called `from`, as Shift and an arrow select it: the
    /// selection's pane is the session it shows.
    fn focus_toward_from(&mut self, from: &str, toward: Direction) -> Result<(), String> {
        let index = self.tab_holding(from);
        let pane = self.pane_of_session(index, from)?;
        let panes = &self.tabs.all()[index].panes;
        let next = (panes.neighbour(&pane, toward, self.tiles).cloned())
            .ok_or_else(|| format!("there's no pane {} of {from}'s", toward.word()))?;
        let name = match next {
            Pane::Session(name) => name,
            Pane::Selection => (self.selection_shows(index))
                .ok_or_else(|| "the selection's pane is empty".to_string())?,
        };
        self.focus_on(&name);
        Ok(())
    }

    /// Moves a border of the pane that shows the session called `name`
    /// `cells` columns or rows `toward`, as resize mode does.
    fn resize_pane_of(&mut self, name: &str, toward: Direction, cells: u16) -> Result<(), String> {
        let index = self.tab_holding(name);
        let pane = self.pane_of_session(index, name)?;
        let tiles = self.tiles;
        let panes = &mut self.tabs.tab_mut(index).panes;
        if !panes.resize(&pane, toward, cells, tiles) {
            let way = toward.word();
            return Err(format!("no border of {name}'s pane can move {way}"));
        }
        Ok(())
    }

    /// Swaps the pane that shows the session called `name` with the pane
    /// `toward` from it, as `H`, `J`, `K` and `L` do.
    fn swap_toward(&mut self, name: &str, toward: Direction) -> Result<(), String> {
        let index = self.tab_holding(name);
        let pane = self.pane_of_session(index, name)?;
        let panes = &self.tabs.all()[index].panes;
        let other = (panes.neighbour(&pane, toward, self.tiles).cloned())
            .ok_or_else(|| format!("there's no pane {} of {name}'s", toward.word()))?;
        self.change_panes_in(index, |panes| {
            panes.swap(&pane, &other);
        });
        Ok(())
    }

    /// Swaps the panes that show the sessions called `name` and `with`,
    /// which have to be on screen in the same tab.
    fn swap_with(&mut self, name: &str, with: &str) -> Result<(), String> {
        if name == with {
            return Err(format!("{name} can't swap with itself"));
        }
        let index = self.tab_holding(name);
        let theirs = self.tab_holding(with);
        if theirs != index {
            let number = theirs + 1;
            return Err(format!(
                "{with} is in tab {number}: panes swap within a tab, `crystal tab move` moves a session"
            ));
        }
        let pane = self.pane_of_session(index, name)?;
        let other = self.pane_of_session(index, with)?;
        self.change_panes_in(index, |panes| {
            panes.swap(&pane, &other);
        });
        Ok(())
    }

    /// Gives the side of the pane that shows the session called `name`
    /// `share` of the room of the split nearest above it, or the nearest
    /// `way`.
    fn set_share_of(&mut self, name: &str, way: Option<Way>, share: f32) -> Result<(), String> {
        if !(0.1..=0.9).contains(&share) {
            return Err("a share of the room is from 0.1 to 0.9".into());
        }
        let index = self.tab_holding(name);
        let pane = self.pane_of_session(index, name)?;
        let panes = &mut self.tabs.tab_mut(index).panes;
        if !panes.set_share(&pane, way, share) {
            let split = match way {
                None => "split",
                Some(Way::Right) => "split side by side",
                Some(Way::Down) => "split one above the other",
            };
            return Err(format!("{name}'s pane isn't in a {split}"));
        }
        Ok(())
    }

    /// Lays tabs out as `laid` has them: see [`Command::Apply`]. Nothing
    /// changes when a session they name isn't there, or is the one this
    /// TUI runs in on a pane of its own.
    fn apply(&mut self, laid: &[TabLayout], replace: bool) -> Result<(), String> {
        if laid.is_empty() {
            return Err("the layout has no tabs".into());
        }
        for tab in laid {
            for name in held(tab) {
                self.session_named(name)?;
            }
            let own = own_panes(&tab.panes);
            for name in own.iter().copied().chain(tab.floating.as_deref()) {
                self.can_show(name)?;
            }
            if let Some(name) = tab.floating.as_deref().filter(|name| own.contains(name)) {
                return Err(format!("{name} can't float and have a pane too"));
            }
        }
        let mut tabs = if replace {
            Tabs::default()
        } else {
            self.tabs_to_keep()
        };
        let mut front = None;
        for (at, tab) in laid.iter().enumerate() {
            let named = (!replace && !tab.name.is_empty())
                .then(|| tabs.all().iter().position(|held| held.name == tab.name))
                .flatten();
            let index = match named {
                Some(index) => index,
                None if replace && at == 0 => 0,
                None => tabs.add(),
            };
            for name in held(tab) {
                tabs.put(name, index);
            }
            let shows = selection_pane(&tab.panes).cloned().flatten();
            let kept = tabs.tab_mut(index);
            kept.rename(&tab.name);
            kept.panes = tree_of(&tab.panes);
            kept.panes.retain(|_| true);
            kept.floating = tab.floating.clone();
            kept.zoomed = tab.zoomed;
            kept.selected = tab.selected.clone().or_else(|| shows.clone());
            kept.shown = shows;
            if tab.current {
                front = Some(index);
            }
        }
        if let Some(index) = front {
            tabs.go_to(index);
        }
        self.set_tabs(tabs);
        Ok(())
    }

    /// Closes the pane of its own the session called `name` has: its split
    /// closes, and the pane beside it takes the room, or its float is put
    /// back.
    fn close_pane_of(&mut self, name: &str) -> Result<(), String> {
        let index = self.tab_holding(name);
        let tab = &self.tabs.all()[index];
        if tab.floating.as_deref() == Some(name) {
            self.put_float_back_in(index);
            return Ok(());
        }
        if !tab.is_split(name) {
            return Err(format!("{name} has no pane of its own to close"));
        }
        self.close_split_in(index, name);
        Ok(())
    }

    /// Zooms the tab holding the session called `name` on its pane: it's
    /// selected there, and put down among the panes if it floats.
    fn zoom_on(&mut self, name: &str) {
        let index = self.tab_holding(name);
        if self.tabs.all()[index].floating.as_deref() == Some(name) {
            self.put_float_back_in(index);
        }
        self.select_in(index, name);
        self.tabs.tab_mut(index).zoomed = true;
    }

    /// Floats the session called `name` over its tab's panes, up out of its
    /// split if it has one, in place of any session floating there.
    fn float(&mut self, name: &str) -> Result<(), String> {
        self.can_show(name)?;
        let index = self.tab_holding(name);
        self.close_split_in(index, name);
        self.put_float_back_in(index);
        self.tabs.tab_mut(index).floating = Some(name.to_string());
        Ok(())
    }
}

/// Every session a tab laid out holds: those it lists, those in its panes
/// and the one floating, each once.
fn held(tab: &TabLayout) -> Vec<&str> {
    fn gather<'a>(tile: &'a Tile, names: &mut Vec<&'a str>) {
        match tile {
            Tile::Pane { session, .. } => names.extend(session.as_deref()),
            Tile::Split { first, second, .. } => {
                gather(first, names);
                gather(second, names);
            }
        }
    }
    let mut names: Vec<&str> = tab.sessions.iter().map(String::as_str).collect();
    gather(&tab.panes, &mut names);
    names.extend(tab.floating.as_deref());
    let mut seen = Vec::new();
    names.retain(|name| {
        let new = !seen.contains(name);
        seen.push(*name);
        new
    });
    names
}

/// The sessions split off into panes of their own in `tile`.
fn own_panes(tile: &Tile) -> Vec<&str> {
    match tile {
        Tile::Pane {
            session: Some(name),
            selection: false,
        } => vec![name.as_str()],
        Tile::Pane { .. } => Vec::new(),
        Tile::Split { first, second, .. } => [own_panes(first), own_panes(second)].concat(),
    }
}

/// The first pane in `tile` that follows the selection, by the session it
/// shows, if any.
fn selection_pane(tile: &Tile) -> Option<&Option<String>> {
    match tile {
        Tile::Pane {
            session,
            selection: true,
        } => Some(session),
        Tile::Pane { .. } => None,
        Tile::Split { first, second, .. } => {
            selection_pane(first).or_else(|| selection_pane(second))
        }
    }
}

/// `tile` as a tab's panes: a pane naming no session, or marked as the
/// selection's, is the selection's. It may have none of those, or several,
/// until [`SplitTree::retain`] puts it right.
fn tree_of(tile: &Tile) -> SplitTree {
    match tile {
        Tile::Pane {
            session: Some(name),
            selection: false,
        } => SplitTree::of(Pane::Session(name.clone())),
        Tile::Pane { .. } => SplitTree::of(Pane::Selection),
        Tile::Split {
            way,
            ratio,
            first,
            second,
        } => SplitTree::joined(*way, *ratio, tree_of(first), tree_of(second)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{SessionInfo, State};
    use crate::tui::app::Focus;
    use ratatui::layout::Rect;
    use std::path::PathBuf;

    fn session(name: &str) -> SessionInfo {
        SessionInfo {
            stopped_idle: false,
            name: name.into(),
            id: format!("id-{name}"),
            command: vec!["sh".into()],
            cwd: PathBuf::from("/"),
            pid: Some(1),
            state: State::Running,
            activity: None,
            worktree: None,
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

    fn app_with(names: &[&str]) -> App {
        let mut app = App::new(None);
        app.set_sessions(names.iter().map(|name| session(name)).collect());
        app
    }

    /// Carries out `command`, as run outside any session.
    fn obey(app: &mut App, command: Command) -> Result<Option<Action>, String> {
        app.obey(Order {
            command,
            caller: None,
        })
    }

    /// Carries out `command`, as run in the session called `caller`.
    fn obey_from(app: &mut App, caller: &str, command: Command) -> Result<(), String> {
        let order = Order {
            command,
            caller: Some(format!("id-{caller}")),
        };
        app.obey(order).map(|_| ())
    }

    fn split(session: &str, beside: Option<&str>, way: Way) -> Command {
        Command::Split {
            session: session.into(),
            beside: beside.map(String::from),
            way,
            ratio: 0.5,
        }
    }

    /// The panes of tab `number`, in the order they're drawn: each by its
    /// session, `*` before the selection's.
    fn panes(app: &App, number: usize) -> Vec<String> {
        fn drawn(tile: &Tile) -> Vec<String> {
            match tile {
                Tile::Pane { session, selection } => {
                    let mark = if *selection { "*" } else { "" };
                    vec![format!("{mark}{}", session.as_deref().unwrap_or(""))]
                }
                Tile::Split { first, second, .. } => [drawn(first), drawn(second)].concat(),
            }
        }
        drawn(&app.layout().tabs[number - 1].panes)
    }

    fn selected(app: &App) -> Option<String> {
        app.selected().map(|session| session.name.clone())
    }

    #[test]
    fn a_title_given_stands_until_it_is_cleared_and_needs_a_tui() {
        let mut app = app_with(&["a"]);
        assert_eq!(app.title_override(), None);
        let title = |text: Option<&str>| Command::Title {
            text: text.map(String::from),
        };
        obey(&mut app, title(Some("deploying"))).unwrap();
        assert_eq!(app.title_override(), Some("deploying"));
        obey(&mut app, title(None)).unwrap();
        assert_eq!(app.title_override(), None);
        let order = Order {
            command: title(Some("deploying")),
            caller: None,
        };
        let alone = App::obey_alone(vec![session("a")], Vec::new(), Tabs::default(), order);
        let Err(said) = alone else {
            panic!("a title was given with no TUI to show it");
        };
        assert!(said.contains(NO_TUI), "{said}");
    }

    /// Two tabs: a and b in the first, c in the second, which is in front.
    fn two_tabs() -> App {
        let mut app = app_with(&["a", "b"]);
        obey(&mut app, Command::NewTab { name: None }).unwrap();
        app.set_sessions(["a", "b", "c"].map(session).to_vec());
        app
    }

    #[test]
    fn a_session_splits_off_beside_the_one_the_command_ran_in() {
        let mut app = app_with(&["agent", "other", "tests"]);
        app.select("other");
        obey_from(&mut app, "agent", split("tests", None, Way::Right)).unwrap();
        // The agent wasn't on screen: it's selected, to show in the pane
        // split.
        assert_eq!(panes(&app, 1), ["*agent", "tests"]);
        assert_eq!(selected(&app).as_deref(), Some("agent"));
    }

    #[test]
    fn beside_a_session_split_off_its_own_pane_splits() {
        let mut app = app_with(&["a", "b", "c"]);
        obey(&mut app, split("b", Some("a"), Way::Right)).unwrap();
        obey(&mut app, split("c", Some("b"), Way::Down)).unwrap();
        assert_eq!(panes(&app, 1), ["*a", "b", "c"]);
        let Tile::Split { way, second, .. } = &app.layout().tabs[0].panes else {
            panic!("the panes are split");
        };
        assert_eq!(*way, Way::Right);
        assert!(matches!(**second, Tile::Split { way: Way::Down, .. }));

        // Split again, a session leaves the pane it had.
        obey(&mut app, split("c", Some("a"), Way::Down)).unwrap();
        assert_eq!(panes(&app, 1), ["*a", "c", "b"]);
    }

    #[test]
    fn a_session_in_another_tab_moves_in_to_be_split_off() {
        let mut app = two_tabs();
        obey(&mut app, split("c", Some("a"), Way::Right)).unwrap();
        let layout = app.layout();
        assert_eq!(layout.tabs[0].sessions, ["a", "b", "c"]);
        assert!(layout.tabs[1].sessions.is_empty());
        // The tab in front stays in front, and gets the split once it's
        // back.
        assert_eq!(app.tabs().current_index(), 1);
        assert_eq!(panes(&app, 1), ["*a", "c"]);
        obey(&mut app, Command::SelectTab { tab: "1".into() }).unwrap();
        assert_eq!(selected(&app).as_deref(), Some("a"));
    }

    #[test]
    fn a_split_says_why_it_cant() {
        let mut app = app_with(&["a", "b"]);
        let said = |app: &mut App, command| obey(app, command).unwrap_err();
        assert_eq!(
            said(&mut app, split("gone", None, Way::Right)),
            "there's no session called gone"
        );
        assert_eq!(
            said(&mut app, split("a", Some("a"), Way::Right)),
            "a can't go beside itself"
        );
        app.set_tiles(Rect::new(29, 1, 20, 22));
        assert_eq!(
            said(&mut app, split("b", Some("a"), Way::Right)),
            "no room for another pane beside a"
        );
        assert_eq!(panes(&app, 1), ["*a"], "nothing changed");

        let mut app = App::new(None);
        app.set_sessions(vec![session("b")]);
        assert_eq!(
            said(&mut app, split("b", None, Way::Down)),
            "b can't go beside itself"
        );
        let mut empty = App::new(None);
        assert_eq!(
            said(&mut empty, Command::Close { session: None }),
            "there's no session selected: name one"
        );
    }

    #[test]
    fn the_session_the_tui_runs_in_is_never_split_off() {
        let mut app = App::new(Some("id-me".into()));
        app.set_sessions(vec![session("a"), session("me")]);
        let said = obey(&mut app, split("me", Some("a"), Way::Right)).unwrap_err();
        assert_eq!(said, "crystal can't show the session it runs in");
    }

    #[test]
    fn a_new_tab_comes_to_the_front_named() {
        let mut app = app_with(&["a"]);
        let name = Some("review".to_string());
        obey(&mut app, Command::NewTab { name }).unwrap();
        let layout = app.layout();
        let current = layout.current().unwrap();
        assert_eq!((current.number, current.name.as_str()), (2, "review"));
        assert!(current.sessions.is_empty());
        for _ in 2..12 {
            obey(&mut app, Command::NewTab { name: None }).unwrap();
        }
        assert_eq!(app.layout().current().unwrap().number, 12);
    }

    #[test]
    fn a_tab_reordered_takes_its_place_and_the_one_in_front_stays() {
        let mut app = two_tabs();
        let reorder = |tab: &str, position| Command::ReorderTab {
            tab: tab.into(),
            position,
        };
        let in_front = app.layout().current().unwrap().sessions.clone();
        obey(&mut app, reorder("1", 2)).unwrap();
        let layout = app.layout();
        assert_eq!(layout.current().unwrap().sessions, in_front);
        assert_eq!(
            layout.tabs[1].sessions,
            two_tabs().layout().tabs[0].sessions
        );
        assert_eq!(
            obey(&mut app, reorder("1", 3)).unwrap_err(),
            "there's no place 3 among 2 tabs"
        );
        assert_eq!(
            obey(&mut app, reorder("1", 0)).unwrap_err(),
            "tabs are numbered from 1"
        );
    }

    #[test]
    fn a_tab_is_found_by_its_number_or_its_name() {
        let mut app = two_tabs();
        let rename = |tab: &str, name: &str| Command::RenameTab {
            tab: tab.into(),
            name: name.into(),
        };
        obey(&mut app, rename("1", "work")).unwrap();
        obey(&mut app, rename("2", "logs")).unwrap();
        obey(&mut app, Command::SelectTab { tab: "work".into() }).unwrap();
        assert_eq!(app.tabs().current().name, "work");
        obey(&mut app, Command::SelectTab { tab: "2".into() }).unwrap();
        assert_eq!(app.tabs().current().name, "logs");
        let said = obey(&mut app, Command::SelectTab { tab: "3".into() }).unwrap_err();
        assert_eq!(said, "there's no tab 3");
    }

    #[test]
    fn a_tab_with_sessions_closes_only_to_kill_them() {
        let mut app = two_tabs();
        let close = |tab: &str, kill| Command::CloseTab {
            tab: Some(tab.into()),
            kill,
        };
        let said = obey(&mut app, close("1", false)).unwrap_err();
        assert_eq!(
            said,
            "tab 1 has sessions in it (a, b): --kill closes it and kills them"
        );
        let killing = obey(&mut app, close("1", true)).unwrap();
        assert_eq!(killing, Some(Action::KillAll(vec!["a".into(), "b".into()])));
        assert_eq!(app.tabs().all().len(), 1);
        let said = obey(
            &mut app,
            Command::CloseTab {
                tab: None,
                kill: true,
            },
        )
        .unwrap_err();
        assert_eq!(said, "tab 1 is the only tab");
    }

    #[test]
    fn an_empty_tab_closes_at_once() {
        let mut app = app_with(&["a"]);
        obey(&mut app, Command::NewTab { name: None }).unwrap();
        let closed = obey(
            &mut app,
            Command::CloseTab {
                tab: None,
                kill: false,
            },
        );
        assert_eq!(closed, Ok(None));
        assert_eq!(app.tabs().all().len(), 1);
        assert_eq!(selected(&app).as_deref(), Some("a"));
    }

    #[test]
    fn a_session_moves_to_another_tab() {
        let mut app = two_tabs();
        let to = |tab: &str| Command::MoveToTab {
            session: "a".into(),
            tab: tab.into(),
        };
        obey(&mut app, to("2")).unwrap();
        assert_eq!(app.layout().tabs[1].sessions, ["a", "c"]);
        obey(&mut app, to("2")).unwrap();
        assert_eq!(app.layout().tabs[0].sessions, ["b"]);
    }

    #[test]
    fn focus_selects_a_session_in_any_tab_and_types_into_it() {
        let mut app = two_tabs();
        obey(
            &mut app,
            Command::Focus {
                session: "b".into(),
                raise: false,
            },
        )
        .unwrap();
        assert_eq!(app.tabs().current_index(), 0);
        assert_eq!(selected(&app).as_deref(), Some("b"));
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));
    }

    #[test]
    fn focus_goes_to_the_pane_that_way_and_says_when_there_is_none() {
        let mut app = app_with(&["a", "b", "c"]);
        app.select("a");
        obey(&mut app, split("b", None, Way::Right)).unwrap();
        let toward = |toward| Command::FocusToward { toward };
        obey(&mut app, toward(Direction::Right)).unwrap();
        assert_eq!(selected(&app).as_deref(), Some("b"));
        let said = obey(&mut app, toward(Direction::Right)).unwrap_err();
        assert_eq!(said, "there's no pane right of b's");
        obey(&mut app, toward(Direction::Left)).unwrap();
        assert_eq!(selected(&app).as_deref(), Some("a"));

        // Run in a session, it goes from that session's pane.
        obey_from(&mut app, "b", toward(Direction::Left)).unwrap();
        assert_eq!(selected(&app).as_deref(), Some("a"));
        let said = obey_from(&mut app, "c", toward(Direction::Left)).unwrap_err();
        assert_eq!(
            said,
            "c isn't on screen: `crystal pane split c` gives it a pane"
        );
    }

    #[test]
    fn a_resize_moves_a_border_of_the_session_s_pane() {
        let mut app = app_with(&["a", "b", "c"]);
        app.select("a");
        obey(&mut app, split("b", None, Way::Right)).unwrap();
        let resize = |session: &str, toward, cells| Command::Resize {
            session: Some(session.into()),
            toward,
            cells,
        };
        obey(&mut app, resize("b", Direction::Left, Some(5))).unwrap();
        let ratio = |app: &App| match &app.layout().tabs[0].panes {
            Tile::Split { ratio, .. } => *ratio,
            Tile::Pane { .. } => 1.0,
        };
        // 50 columns shared, the first had 25: it has 20 now.
        assert_eq!(ratio(&app), 0.4);
        obey(&mut app, resize("a", Direction::Right, None)).unwrap();
        assert_eq!(ratio(&app), 0.48);

        let said = obey(&mut app, resize("b", Direction::Up, None)).unwrap_err();
        assert_eq!(said, "no border of b's pane can move up");
        let said = obey(&mut app, resize("c", Direction::Up, None)).unwrap_err();
        assert_eq!(
            said,
            "c isn't on screen: `crystal pane split c` gives it a pane"
        );
    }

    #[test]
    fn a_swap_trades_two_panes_places_that_way_or_by_name() {
        let mut app = app_with(&["a", "b", "c", "d"]);
        app.select("a");
        obey(&mut app, split("b", None, Way::Right)).unwrap();
        obey(&mut app, split("c", Some("b"), Way::Down)).unwrap();
        let toward = |toward| Command::SwapToward {
            session: Some("a".into()),
            toward,
        };
        obey(&mut app, toward(Direction::Right)).unwrap();
        assert_eq!(panes(&app, 1), ["b", "*a", "c"]);
        let said = obey(&mut app, toward(Direction::Right)).unwrap_err();
        assert_eq!(said, "there's no pane right of a's");

        let with = |session: &str, with: &str| Command::Swap {
            session: Some(session.into()),
            with: with.into(),
        };
        obey(&mut app, with("c", "b")).unwrap();
        assert_eq!(panes(&app, 1), ["c", "*a", "b"]);
        // Run in a session, it's that session's pane that moves.
        obey_from(
            &mut app,
            "b",
            Command::SwapToward {
                session: None,
                toward: Direction::Up,
            },
        )
        .unwrap();
        assert_eq!(panes(&app, 1), ["c", "b", "*a"]);
        // The ratios stay where they were.
        let Tile::Split { ratio, .. } = app.layout().tabs[0].panes else {
            panic!("the panes are split");
        };
        assert_eq!(ratio, 0.5);

        let said = |app: &mut App, command| obey(app, command).unwrap_err();
        assert_eq!(said(&mut app, with("a", "a")), "a can't swap with itself");
        assert_eq!(
            said(&mut app, with("a", "d")),
            "d isn't on screen: `crystal pane split d` gives it a pane"
        );
        obey(&mut app, Command::NewTab { name: None }).unwrap();
        app.set_sessions(["a", "b", "c", "d", "e"].map(session).to_vec());
        assert_eq!(
            said(&mut app, with("a", "e")),
            "e is in tab 2: panes swap within a tab, `crystal tab move` moves a session"
        );
    }

    #[test]
    fn a_ratio_gives_a_panes_side_of_its_split_a_share_of_the_room() {
        let mut app = app_with(&["a", "b", "c"]);
        app.select("a");
        obey(&mut app, split("b", None, Way::Right)).unwrap();
        obey(&mut app, split("c", Some("b"), Way::Down)).unwrap();
        let ratio = |session: &str, way, share| Command::Ratio {
            session: Some(session.into()),
            way,
            share,
        };
        let ratios = |app: &App| match &app.layout().tabs[0].panes {
            Tile::Split { ratio, second, .. } => match **second {
                Tile::Split { ratio: inner, .. } => (*ratio, inner),
                Tile::Pane { .. } => (*ratio, 1.0),
            },
            Tile::Pane { .. } => (1.0, 1.0),
        };
        // c is the second side of the split it's in.
        obey(&mut app, ratio("c", None, 0.3)).unwrap();
        assert_eq!(ratios(&app), (0.5, 0.7));
        // Side by side, its side is the second of the outer split.
        obey(&mut app, ratio("c", Some(Way::Right), 0.75)).unwrap();
        assert_eq!(ratios(&app), (0.25, 0.7));
        obey(&mut app, ratio("a", None, 0.6)).unwrap();
        assert_eq!(ratios(&app), (0.6, 0.7));

        let said = |app: &mut App, command| obey(app, command).unwrap_err();
        assert_eq!(
            said(&mut app, ratio("a", Some(Way::Down), 0.5)),
            "a's pane isn't in a split one above the other"
        );
        assert_eq!(
            said(&mut app, ratio("a", None, 0.95)),
            "a share of the room is from 0.1 to 0.9"
        );
        let mut alone = app_with(&["a"]);
        assert_eq!(
            said(&mut alone, ratio("a", None, 0.5)),
            "a's pane isn't in a split"
        );
    }

    fn laid(name: &str, current: bool, panes: Tile, sessions: &[&str]) -> TabLayout {
        TabLayout {
            number: 0,
            name: name.into(),
            current,
            zoomed: false,
            sessions: sessions.iter().map(|name| name.to_string()).collect(),
            selected: None,
            floating: None,
            panes,
        }
    }

    fn tile(session: &str, selection: bool) -> Tile {
        Tile::Pane {
            session: Some(session.into()),
            selection,
        }
    }

    fn side_by_side(first: Tile, second: Tile) -> Tile {
        Tile::Split {
            way: Way::Right,
            ratio: 0.6,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    #[test]
    fn a_layout_applied_lays_out_the_tab_of_its_name_or_a_new_one() {
        let mut app = two_tabs();
        obey(
            &mut app,
            Command::RenameTab {
                tab: "1".into(),
                name: "dev".into(),
            },
        )
        .unwrap();
        let dev = laid(
            "dev",
            false,
            side_by_side(tile("c", true), tile("a", false)),
            &[],
        );
        let logs = laid("logs", false, tile("b", true), &[]);
        let apply = |tabs| Command::Apply {
            tabs,
            replace: false,
        };
        obey(&mut app, apply(vec![dev.clone(), logs])).unwrap();
        let layout = app.layout();
        assert_eq!(layout.tabs.len(), 3);
        assert_eq!(layout.tabs[0].name, "dev");
        assert_eq!(layout.tabs[0].sessions, ["a", "c"]);
        assert_eq!(layout.tabs[0].selected.as_deref(), Some("c"));
        assert_eq!(panes(&app, 1), ["*c", "a"]);
        assert_eq!(
            (layout.tabs[2].name.as_str(), &layout.tabs[2].sessions[..]),
            ("logs", &["b".to_string()][..])
        );
        // The tab in front stays in front, empty now.
        assert_eq!(app.tabs().current_index(), 1);
        assert!(layout.tabs[1].sessions.is_empty());

        // Applied again, it changes nothing; marked current, its tab comes
        // to the front.
        let mut dev = dev;
        dev.current = true;
        obey(&mut app, apply(vec![dev])).unwrap();
        assert_eq!(app.layout().tabs.len(), 3);
        assert_eq!(app.tabs().current_index(), 0);
        assert_eq!(selected(&app).as_deref(), Some("c"));
        assert_eq!(panes(&app, 1), ["*c", "a"]);
    }

    #[test]
    fn a_layout_applied_in_place_of_every_tab_keeps_the_sessions_it_leaves_out() {
        let mut app = two_tabs();
        let mut logs = laid("logs", false, tile("b", true), &[]);
        logs.floating = Some("c".into());
        logs.zoomed = true;
        let work = laid(
            "",
            true,
            side_by_side(
                tile("a", false),
                Tile::Pane {
                    session: None,
                    selection: true,
                },
            ),
            &[],
        );
        obey(
            &mut app,
            Command::Apply {
                tabs: vec![logs, work],
                replace: true,
            },
        )
        .unwrap();
        let layout = app.layout();
        assert_eq!(layout.tabs.len(), 2);
        assert_eq!(layout.tabs[0].sessions, ["b", "c"]);
        assert_eq!(layout.tabs[0].floating.as_deref(), Some("c"));
        assert!(layout.tabs[0].zoomed);
        assert_eq!(app.tabs().current_index(), 1);
        assert_eq!(layout.tabs[1].name, "");
        assert_eq!(panes(&app, 2), ["a", "*"]);
    }

    #[test]
    fn a_layout_is_applied_whole_or_not_at_all() {
        let mut app = App::new(Some("id-me".into()));
        app.set_sessions(["a", "b", "me"].map(session).to_vec());
        let before = app.layout();
        let apply = |tab: TabLayout| Command::Apply {
            tabs: vec![tab],
            replace: false,
        };
        let said = |app: &mut App, command| obey(app, command).unwrap_err();
        let gone = laid("x", false, tile("a", true), &["gone"]);
        assert_eq!(
            said(&mut app, apply(gone)),
            "there's no session called gone"
        );
        let mine = laid(
            "x",
            false,
            side_by_side(tile("a", true), tile("me", false)),
            &[],
        );
        assert_eq!(
            said(&mut app, apply(mine)),
            "crystal can't show the session it runs in"
        );
        let mut twice = laid(
            "x",
            false,
            side_by_side(tile("a", true), tile("b", false)),
            &[],
        );
        twice.floating = Some("b".into());
        assert_eq!(
            said(&mut app, apply(twice)),
            "b can't float and have a pane too"
        );
        let none = Command::Apply {
            tabs: Vec::new(),
            replace: true,
        };
        assert_eq!(said(&mut app, none), "the layout has no tabs");
        assert_eq!(app.layout(), before);
    }

    #[test]
    fn closing_a_pane_closes_a_split_or_puts_a_float_back() {
        let mut app = app_with(&["a", "b", "c"]);
        app.select("a");
        obey(&mut app, split("b", None, Way::Right)).unwrap();
        let close = |session: &str| Command::Close {
            session: Some(session.into()),
        };
        obey(&mut app, close("b")).unwrap();
        assert_eq!(panes(&app, 1), ["*a"]);
        let said = obey(&mut app, close("a")).unwrap_err();
        assert_eq!(said, "a has no pane of its own to close");

        let float = Command::Float {
            session: Some("c".into()),
            on: true,
        };
        obey(&mut app, float).unwrap();
        assert_eq!(app.layout().tabs[0].floating.as_deref(), Some("c"));
        obey(&mut app, close("c")).unwrap();
        assert_eq!(app.layout().tabs[0].floating, None);
    }

    #[test]
    fn a_float_comes_up_out_of_its_split_and_goes_back_with_off() {
        let mut app = app_with(&["a", "b"]);
        app.select("a");
        obey(&mut app, split("b", None, Way::Right)).unwrap();
        let float = |on| Command::Float {
            session: Some("b".into()),
            on,
        };
        obey(&mut app, float(true)).unwrap();
        assert_eq!(panes(&app, 1), ["*a"]);
        assert_eq!(app.layout().tabs[0].floating.as_deref(), Some("b"));
        obey(&mut app, float(false)).unwrap();
        assert_eq!(app.layout().tabs[0].floating, None);
    }

    #[test]
    fn zoom_selects_the_session_and_off_puts_the_panes_back() {
        let mut app = app_with(&["a", "b"]);
        let zoom = |on| Command::Zoom {
            session: Some("b".into()),
            on,
        };
        obey(&mut app, zoom(true)).unwrap();
        assert!(app.zoomed());
        assert_eq!(selected(&app).as_deref(), Some("b"));
        obey(&mut app, zoom(false)).unwrap();
        assert!(!app.zoomed());
    }

    #[test]
    fn a_command_run_in_a_tab_behind_works_on_that_tab() {
        let mut app = two_tabs();
        obey(&mut app, Command::SelectTab { tab: "1".into() }).unwrap();
        // c, in tab 2, behind, splits b off beside itself there, and zooms.
        obey_from(&mut app, "c", split("b", None, Way::Down)).unwrap();
        obey_from(
            &mut app,
            "c",
            Command::Zoom {
                session: None,
                on: true,
            },
        )
        .unwrap();
        assert_eq!(app.tabs().current_index(), 0);
        let layout = app.layout();
        assert_eq!(layout.tabs[0].sessions, ["a"]);
        assert_eq!(layout.tabs[1].sessions, ["b", "c"]);
        assert!(layout.tabs[1].zoomed && !layout.tabs[0].zoomed);
        assert_eq!(panes(&app, 2), ["*c", "b"]);

        // Evening out works on the tab it was run in too.
        obey_from(&mut app, "c", Command::Equalize).unwrap();
        assert_eq!(app.tabs().current_index(), 0);
    }

    #[test]
    fn the_layout_says_what_each_tab_holds_and_shows() {
        let mut app = two_tabs();
        obey(&mut app, split("a", Some("c"), Way::Right)).unwrap();
        let layout = app.layout();
        let front = layout.current().unwrap();
        assert_eq!(front.number, 2);
        assert_eq!(front.sessions, ["a", "c"]);
        assert_eq!(front.selected.as_deref(), Some("c"));
        assert_eq!(panes(&app, 2), ["*c", "a"]);
        let behind = &layout.tabs[0];
        assert_eq!(behind.selected.as_deref(), Some("b"));
        assert_eq!(panes(&app, 1), ["*b"]);
    }
}
