//! Moving the sidebar's projects and sessions by hand: with the keys, by
//! dragging a session's row or a project's heading with the mouse, and with
//! `crystal sidebar move`. A session moves among those beside it, in its
//! worktree, agents among agents and terminals among terminals; a project
//! among the projects. Where each goes is [`ByHand`]'s, which the event
//! loop keeps in the database, so it lasts across restarts.
//!
//! [`ByHand`]: super::groups::ByHand

use super::{App, Row};
use crate::config::SidebarOrder;
use crate::tui::groups;
use std::path::PathBuf;

/// What a move by hand moves: a session, by its name, or a project, by its
/// main worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Movable {
    Session(String),
    Project(PathBuf),
}

/// Where a move by hand takes it: a place up or down among those beside
/// it, or next to one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Toward {
    Up,
    Down,
    Before(Movable),
    After(Movable),
}

/// A sidebar row the mouse took, to move what it stands for to where the
/// button comes up; or, coming up where it was taken, a click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarGrab {
    pub what: Movable,
    /// The row it was taken on.
    pub from: usize,
    /// The row the mouse is over now.
    pub over: usize,
}

impl Movable {
    fn said(&self) -> String {
        match self {
            Movable::Session(name) => name.clone(),
            Movable::Project(path) => path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        }
    }
}

impl App {
    /// `move-up` and `move-down`: the selected session, a place among
    /// those beside it; or the project whose heading the selection rests
    /// on, folded.
    pub(super) fn move_selected(&mut self, down: bool) {
        let what = match self.folded_selection() {
            Some(project) => Movable::Project(project.to_path_buf()),
            None => match self.selected().filter(|_| self.on_worktree.is_none()) {
                Some(session) => Movable::Session(session.name.clone()),
                None => {
                    self.notify("only a session or a project moves".to_string());
                    return;
                }
            },
        };
        self.move_or_say(&what, if down { Toward::Down } else { Toward::Up });
    }

    /// `move-project-up` and `move-project-down`: the selected session's
    /// project, a place among the projects.
    pub(super) fn move_selected_project(&mut self, down: bool) {
        let Some(worktree) = self.selection_worktree() else {
            self.notify("only a project in git moves".to_string());
            return;
        };
        let what = Movable::Project(worktree.project_path.clone());
        self.move_or_say(&what, if down { Toward::Down } else { Toward::Up });
    }

    fn move_or_say(&mut self, what: &Movable, toward: Toward) {
        if let Err(why) = self.move_by_hand(what, toward) {
            self.notify(why);
        }
    }

    /// Moves `what` where `toward` says among those beside it, or says why
    /// it can't. One moved that stays where it was, ordered by attention,
    /// since what waits on the user stays first, says so too.
    pub(crate) fn move_by_hand(&mut self, what: &Movable, toward: Toward) -> Result<(), String> {
        let before = self.beside(what)?;
        let at = (before.iter())
            .position(|beside| beside == what)
            .ok_or_else(|| format!("{} isn't in the tab in front", what.said()))?;
        let (next_to, after) = match toward {
            Toward::Up => match at.checked_sub(1) {
                Some(up) => (before[up].clone(), false),
                None => return Err(format!("{} is first already", what.said())),
            },
            Toward::Down => match before.get(at + 1) {
                Some(down) => (down.clone(), true),
                None => return Err(format!("{} is last already", what.said())),
            },
            Toward::Before(other) => (other, false),
            Toward::After(other) => (other, true),
        };
        if next_to == *what {
            return Ok(());
        }
        if !before.contains(&next_to) {
            let among = match what {
                Movable::Session(_) => "the sessions in its worktree",
                Movable::Project(_) => "the projects in the tab",
            };
            return Err(format!(
                "{} moves among {among}, and {} isn't one",
                what.said(),
                next_to.said()
            ));
        }
        let changed = match (what, &next_to) {
            (Movable::Session(name), Movable::Session(other)) => {
                let now = self.sessions_now();
                self.by_hand.place_session(now, name, other, after)
            }
            (Movable::Project(path), Movable::Project(other)) => {
                let now = self.projects_now();
                self.by_hand.place_project(now, path, other, after)
            }
            _ => false,
        };
        if changed {
            self.sort_again();
        }
        if self.beside(what)? == before {
            return Err(match self.order {
                SidebarOrder::Attention => "what waits on you stays first while the sidebar \
                    is ordered by attention: [sidebar] order = \"stable\" keeps it still"
                    .to_string(),
                SidebarOrder::Stable => format!("{} is where it was", what.said()),
            });
        }
        Ok(())
    }

    /// Those `what` moves among, itself too, in the order the sidebar shows
    /// them: the sessions beside it in its worktree, in the tab that holds
    /// it, agents or terminals as it is; or the projects in the tab in
    /// front.
    fn beside(&self, what: &Movable) -> Result<Vec<Movable>, String> {
        match what {
            Movable::Session(name) => {
                let session = (self.sessions.iter())
                    .find(|session| session.name == *name)
                    .ok_or_else(|| format!("no session named {name}"))?;
                let flows = self.shown_flows();
                if groups::flow_step(session, flows).is_some() {
                    return Err(format!(
                        "{name} is a step of a flow run, whose steps keep their order"
                    ));
                }
                let place = groups::place_of(session);
                let terminal = groups::is_terminal(session);
                let tab = self.tabs.tab_of(name).unwrap_or(self.tabs.current_index());
                let beside = self
                    .sessions_in(tab)
                    .into_iter()
                    .map(|at| &self.sessions[at]);
                Ok(beside
                    .filter(|other| {
                        groups::place_of(other) == place
                            && groups::is_terminal(other) == terminal
                            && groups::flow_step(other, flows).is_none()
                    })
                    .map(|other| Movable::Session(other.name.clone()))
                    .collect())
            }
            Movable::Project(_) => Ok(self
                .all_rows()
                .into_iter()
                .filter_map(|row| match row {
                    Row::Project { path, .. } => Some(Movable::Project(path)),
                    _ => None,
                })
                .collect()),
        }
    }

    /// Every session's name, in the order they stand now apart from what's
    /// waiting on the user: those placed by hand, then the rest as they
    /// were made.
    fn sessions_now(&self) -> Vec<String> {
        let mut names = self.made.clone();
        names.sort_by_key(|name| self.by_hand.session_place(name));
        names
    }

    /// Every project, the same way: those placed by hand, then the rest in
    /// the order their first sessions were made, then those crystal knows
    /// with none.
    fn projects_now(&self) -> Vec<PathBuf> {
        let mut projects: Vec<PathBuf> = Vec::new();
        let in_order = self.made.iter().filter_map(|name| {
            let session = self.sessions.iter().find(|session| session.name == *name)?;
            Some(&session.worktree.as_ref()?.project_path)
        });
        for project in in_order.chain(self.known.iter().map(|known| &known.path)) {
            if !projects.contains(project) {
                projects.push(project.clone());
            }
        }
        projects.sort_by_key(|project| self.by_hand.project_place(project));
        projects
    }

    /// The order the user put projects and sessions in by hand, to keep.
    pub fn by_hand(&self) -> &groups::ByHand {
        &self.by_hand
    }

    /// Takes the order the TUI kept.
    pub fn set_by_hand(&mut self, by_hand: groups::ByHand) {
        self.by_hand = by_hand;
        self.sort_again();
    }

    /// The mouse took the sidebar row at `row`: a session's or a project's
    /// heading is taken, to move it.
    pub(super) fn grab_row(&mut self, row: usize) {
        let what = match self.rows().get(row) {
            Some(Row::Session(index)) => {
                let session = &self.sessions[*index];
                let flows = self.shown_flows();
                let in_a_flow = groups::flow_step(session, flows).is_some();
                (!in_a_flow).then(|| Movable::Session(session.name.clone()))
            }
            Some(Row::Project { path, .. }) => Some(Movable::Project(path.clone())),
            _ => None,
        };
        self.sidebar_grab = what.map(|what| SidebarGrab {
            what,
            from: row,
            over: row,
        });
    }

    /// The row taken by the mouse, while it's held.
    pub fn sidebar_grab(&self) -> Option<&SidebarGrab> {
        self.sidebar_grab.as_ref()
    }

    /// Where what the mouse holds would go, were the button to come up:
    /// next to what the row it's over stands for, after it when that's
    /// below, and whether it's after. `None` over a row it doesn't move
    /// among, or over its own.
    pub fn drop_target(&self, grab: &SidebarGrab) -> Option<(Movable, bool)> {
        let rows = self.rows();
        let target = match (&grab.what, rows.get(grab.over)?) {
            (
                Movable::Session(_),
                Row::Session(index)
                | Row::Task(index)
                | Row::Line(index)
                | Row::More { session: index, .. },
            ) => Movable::Session(self.sessions[*index].name.clone()),
            // A project goes beside the project of any of its rows.
            (Movable::Project(_), _) => {
                let heading = rows[..=grab.over].iter().rev().find_map(|row| match row {
                    Row::Project { path, .. } => Some(Movable::Project(path.clone())),
                    Row::OutsideGit | Row::NeedsYou(_) | Row::Pinned(_) => Some(grab.what.clone()),
                    _ => None,
                })?;
                if heading == grab.what {
                    return None;
                }
                heading
            }
            _ => return None,
        };
        if target == grab.what {
            return None;
        }
        let beside = self.beside(&grab.what).ok()?;
        let from = beside.iter().position(|beside| *beside == grab.what)?;
        let to = beside.iter().position(|beside| *beside == target)?;
        Some((target, to > from))
    }

    /// The mouse let go of what it held: where it took it, a click, which
    /// folds or unfolds a project taken by its heading; elsewhere, it goes
    /// where it was let go of, if that's beside it.
    pub(super) fn let_go(&mut self) {
        let Some(grab) = self.sidebar_grab.take() else {
            return;
        };
        if grab.over == grab.from {
            if let Movable::Project(path) = &grab.what {
                self.toggle_fold(path);
            }
            return;
        }
        if let Some((target, after)) = self.drop_target(&grab) {
            let toward = if after {
                Toward::After(target)
            } else {
                Toward::Before(target)
            };
            self.move_or_say(&grab.what, toward);
        }
    }
}
