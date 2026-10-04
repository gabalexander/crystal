//! Layouts as files, the way herdr's `layout.export` and `layout.apply` take
//! them: `crystal layout export` writes the TUI's tabs as JSON, each with how
//! its panes split the room and what starts each of its sessions again, its
//! command and directory; `crystal layout apply` reads one, from a file or
//! standard input, starts the sessions it names that aren't there, and has
//! the tabs laid out that way, as [`Command::Apply`] says.
//!
//! A file is in the shape `crystal layout --json` prints, which applies as
//! it is, with more said about a session where it's named: a pane's `cwd`,
//! `command` and `env`, and in a tab's `sessions` and its `floating`, the
//! same in an object, its name as `session`. A session that's there, running
//! or ended, is laid out as it is. One that isn't is started under its name
//! when the file says how: its command (a shell, when it gives none), in its
//! directory (where `apply` runs, when it gives none), with its variables
//! over the environment `apply` runs with. A pane naming no session starts a
//! new one the same way, named by the daemon, but for the selection's pane,
//! which with nothing said shows whatever's selected. What can't be started
//! is left out, and said so.
//!
//! All but reading the file and asking the daemon is pure, so it's
//! unit-tested.

use crate::client::{self, Purpose};
use crate::layout::{self, Command, Layout, TabLayout};
use crate::printable;
use crate::protocol::{Request, Response, SessionInfo};
use crate::shell;
use crate::tui::layouts::Program;
use crate::tui::split_tree::Way;
use anyhow::{Context, Result, bail};
use serde::de::{self, MapAccess, Visitor, value::MapAccessDeserializer};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

/// How deep a tab's splits may go, and how many panes it may have, as
/// herdr has it: past that, a file is more likely a mistake than a layout.
const DEEPEST: usize = 16;
const MOST_PANES: usize = 24;

/// A layout file: its tabs, in order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct File {
    pub tabs: Vec<Tab>,
}

/// A tab in a file. Anything may be left out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tab {
    /// Named, it's laid out in place of the tab with its name, if there's
    /// one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Whether it comes to the front.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub current: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub zoomed: bool,
    /// The session selected in it, when it isn't the one the selection's
    /// pane shows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    /// Its panes; none for the selection's pane alone, showing what's
    /// selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panes: Option<Tile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub floating: Option<Start>,
    /// Its sessions with no pane of their own. Those in its panes and its
    /// float are in it too, listed here or not.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<Start>,
}

/// A tab's panes in a file, as `crystal layout --json` has them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Tile {
    /// A pane: the session it shows, and how to start it; with
    /// `selection`, the pane that follows the selection.
    Pane {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        selection: bool,
        #[serde(flatten)]
        start: Start,
    },
    /// The first side has `ratio` of the room, the second the rest.
    Split {
        way: Way,
        ratio: f32,
        first: Box<Tile>,
        second: Box<Tile>,
    },
}

/// A session as a file names it, and what starts it when it isn't there.
/// Read from its name alone too. (`remote = "Self"` derives the object's
/// reading as `Start::deserialize`, for the reading that takes a name too
/// to fall back on.)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", default)]
pub struct Start {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl Serialize for Start {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Start::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for Start {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Start, D::Error> {
        struct Named;
        impl<'de> Visitor<'de> for Named {
            type Value = Start;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a session's name, or an object with its session, cwd, command and env")
            }

            fn visit_str<E: de::Error>(self, name: &str) -> Result<Start, E> {
                Ok(Start::named(name))
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Start, A::Error> {
                Start::deserialize(MapAccessDeserializer::new(map))
            }
        }
        deserializer.deserialize_any(Named)
    }
}

impl Start {
    fn named(name: &str) -> Start {
        Start {
            session: Some(name.to_string()),
            ..Start::default()
        }
    }

    /// Whether it says how to start the session: a command, a directory or
    /// variables.
    fn says_how(&self) -> bool {
        self.command.is_some() || self.cwd.is_some() || !self.env.is_empty()
    }

    /// The session it names, or else a new one.
    fn who(&self) -> String {
        match &self.session {
            Some(name) => name.clone(),
            None => "a new session".to_string(),
        }
    }

    /// What starts the session called `name` again, if it's among
    /// `sessions` and runs in a terminal: as a saved layout keeps it.
    fn of(name: &str, sessions: &[SessionInfo]) -> Start {
        let session = sessions.iter().find(|session| session.name == name);
        let program = session.and_then(Program::of);
        Start {
            session: Some(name.to_string()),
            cwd: program.as_ref().map(|program| program.cwd.clone()),
            command: program.map(|program| program.command),
            env: BTreeMap::new(),
        }
    }
}

impl Tab {
    /// How a message names it: by its name, or its place in the file.
    fn called(&self, index: usize) -> String {
        match self.name.as_str() {
            "" => format!("tab {}", index + 1),
            name => format!("tab {name}"),
        }
    }

    /// The sessions on its screen, its panes' and its float's, as the file
    /// names them.
    fn shown(&self) -> Vec<&Start> {
        let mut shown = Vec::new();
        if let Some(panes) = &self.panes {
            panes.gather(&mut shown);
        }
        shown.extend(&self.floating);
        shown
    }

    /// Takes what the tab's `sessions` say about a session on its screen
    /// into its pane or its float, where that says nothing about how to
    /// start it, and leaves it off the list.
    fn take_in_listed(&mut self) {
        let mut shown: Vec<&mut Start> = Vec::new();
        if let Some(panes) = &mut self.panes {
            panes.gather_mut(&mut shown);
        }
        shown.extend(self.floating.as_mut());
        self.sessions.retain(|listed| {
            let Some(on_screen) = shown
                .iter_mut()
                .find(|start| start.session.is_some() && start.session == listed.session)
            else {
                return true;
            };
            if !on_screen.says_how() {
                on_screen.cwd = listed.cwd.clone();
                on_screen.command = listed.command.clone();
                on_screen.env = listed.env.clone();
            }
            false
        });
    }

    /// Every session it names, once each.
    fn names(&self) -> Vec<&str> {
        let shown = self.shown().into_iter().chain(&self.sessions);
        let mut names: Vec<&str> = Vec::new();
        for name in shown.filter_map(|start| start.session.as_deref()) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        names
    }
}

impl Tile {
    /// What each pane names, in the order they're drawn.
    fn gather<'a>(&'a self, starts: &mut Vec<&'a Start>) {
        match self {
            Tile::Pane { start, .. } => starts.push(start),
            Tile::Split { first, second, .. } => {
                first.gather(starts);
                second.gather(starts);
            }
        }
    }

    fn gather_mut<'a>(&'a mut self, starts: &mut Vec<&'a mut Start>) {
        match self {
            Tile::Pane { start, .. } => starts.push(start),
            Tile::Split { first, second, .. } => {
                first.gather_mut(starts);
                second.gather_mut(starts);
            }
        }
    }

    /// How many panes deep it goes.
    fn depth(&self) -> usize {
        match self {
            Tile::Pane { .. } => 0,
            Tile::Split { first, second, .. } => 1 + first.depth().max(second.depth()),
        }
    }

    /// How many of its panes are marked as the selection's.
    fn selections(&self) -> usize {
        match self {
            Tile::Pane { selection, .. } => usize::from(*selection),
            Tile::Split { first, second, .. } => first.selections() + second.selections(),
        }
    }

    /// Whether every split's ratio is a share of its room.
    fn ratios_fit(&self) -> bool {
        match self {
            Tile::Pane { .. } => true,
            Tile::Split {
                ratio,
                first,
                second,
                ..
            } => *ratio > 0.0 && *ratio < 1.0 && first.ratios_fit() && second.ratios_fit(),
        }
    }

    /// The file's tile for `tile`, each session with what starts it again.
    fn of(tile: &layout::Tile, sessions: &[SessionInfo]) -> Tile {
        match tile {
            layout::Tile::Pane { session, selection } => Tile::Pane {
                selection: *selection,
                start: (session.as_deref())
                    .map(|name| Start::of(name, sessions))
                    .unwrap_or_default(),
            },
            layout::Tile::Split {
                way,
                ratio,
                first,
                second,
            } => Tile::Split {
                way: *way,
                ratio: *ratio,
                first: Box::new(Tile::of(first, sessions)),
                second: Box::new(Tile::of(second, sessions)),
            },
        }
    }

    /// The tile with the sessions `name_of` names for what each pane says,
    /// those it names none for left out, the other side of their split
    /// taking the room; nothing when it names none. The selection's pane
    /// with nothing said stays, showing what's selected.
    fn resolve(&self, name_of: &mut dyn FnMut(&Start) -> Option<String>) -> Option<layout::Tile> {
        match self {
            Tile::Pane {
                selection: true,
                start,
            } if start.session.is_none() && !start.says_how() => Some(layout::Tile::Pane {
                session: None,
                selection: true,
            }),
            Tile::Pane { selection, start } => name_of(start).map(|name| layout::Tile::Pane {
                session: Some(name),
                selection: *selection,
            }),
            Tile::Split {
                way,
                ratio,
                first,
                second,
            } => match (first.resolve(name_of), second.resolve(name_of)) {
                (Some(first), Some(second)) => Some(layout::Tile::Split {
                    way: *way,
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (one, other) => one.or(other),
            },
        }
    }
}

/// Marks the first pane of `tile` as the selection's, unless one is.
fn mark_selection(tile: &mut layout::Tile) {
    fn marked(tile: &layout::Tile) -> bool {
        match tile {
            layout::Tile::Pane { selection, .. } => *selection,
            layout::Tile::Split { first, second, .. } => marked(first) || marked(second),
        }
    }
    fn mark_first(tile: &mut layout::Tile) {
        match tile {
            layout::Tile::Pane { selection, .. } => *selection = true,
            layout::Tile::Split { first, .. } => mark_first(first),
        }
    }
    if !marked(tile) {
        mark_first(tile);
    }
}

/// The sessions a resolved tile shows, in the order they're drawn.
fn shown_in(tile: &layout::Tile, names: &mut Vec<String>) {
    match tile {
        layout::Tile::Pane { session, .. } => names.extend(session.clone()),
        layout::Tile::Split { first, second, .. } => {
            shown_in(first, names);
            shown_in(second, names);
        }
    }
}

/// What applying a file comes to.
#[derive(Debug, Default, PartialEq)]
pub struct Planned {
    /// The tabs to lay out, every session in them there by now.
    pub tabs: Vec<TabLayout>,
    /// The sessions started for them, by name.
    pub started: Vec<String>,
    /// What was left out, and why.
    pub left_out: Vec<String>,
}

impl File {
    /// The layout file `text`, checked, its directories made whole from
    /// `here`, `~` the home directory; or why it can't be applied.
    pub fn read(text: &str, here: &Path) -> Result<File, String> {
        let mut file: File =
            serde_json::from_str(text).map_err(|err| format!("couldn't read the layout: {err}"))?;
        file.check()?;
        for tab in &mut file.tabs {
            tab.take_in_listed();
            let mut starts: Vec<&mut Start> = Vec::new();
            if let Some(panes) = &mut tab.panes {
                panes.gather_mut(&mut starts);
            }
            starts.extend(tab.floating.as_mut());
            starts.extend(tab.sessions.iter_mut());
            for cwd in starts.into_iter().filter_map(|start| start.cwd.as_mut()) {
                *cwd = here.join(shell::expand_home(cwd));
            }
        }
        Ok(file)
    }

    /// Says why the file can't be applied, if it can't: no tabs, two
    /// current or of the same name, splits too deep or too many, a ratio
    /// that isn't a share, two panes marked as the selection's, a session
    /// on screen twice or in two tabs, one selected that the tab doesn't
    /// hold, an empty name, or a variable's name that isn't one.
    fn check(&self) -> Result<(), String> {
        if self.tabs.is_empty() {
            return Err("the layout has no tabs".into());
        }
        if self.tabs.iter().filter(|tab| tab.current).count() > 1 {
            return Err("only one tab can be current".into());
        }
        let mut named: Vec<&str> = Vec::new();
        let mut held: Vec<&str> = Vec::new();
        for (index, tab) in self.tabs.iter().enumerate() {
            let what = tab.called(index);
            if !tab.name.is_empty() {
                if named.contains(&tab.name.as_str()) {
                    return Err(format!("two tabs are called {}", tab.name));
                }
                named.push(&tab.name);
            }
            if let Some(panes) = &tab.panes {
                if panes.depth() > DEEPEST {
                    return Err(format!("{what}'s splits go more than {DEEPEST} deep"));
                }
                let mut starts = Vec::new();
                panes.gather(&mut starts);
                if starts.len() > MOST_PANES {
                    return Err(format!("{what} has more than {MOST_PANES} panes"));
                }
                if panes.selections() > 1 {
                    return Err(format!("{what} has two panes marked as the selection's"));
                }
                if !panes.ratios_fit() {
                    return Err(format!(
                        "{what} has a split whose ratio isn't a share of its room, between 0 and 1"
                    ));
                }
            }
            let mut shown: Vec<&str> = Vec::new();
            for name in tab
                .shown()
                .into_iter()
                .filter_map(|start| start.session.as_deref())
            {
                if shown.contains(&name) {
                    return Err(format!("{name} is on screen twice in {what}"));
                }
                shown.push(name);
            }
            let names = tab.names();
            if let Some(name) = names.iter().find(|name| held.contains(name)) {
                return Err(format!("{name} is in two tabs"));
            }
            if let Some(selected) = &tab.selected
                && !names.contains(&selected.as_str())
            {
                return Err(format!("{what} selects {selected}, which isn't in it"));
            }
            held.extend(names);
            let starts = tab.shown().into_iter().chain(&tab.sessions);
            for start in starts {
                if start.session.as_deref() == Some("") {
                    return Err(format!("{what} names a session with an empty name"));
                }
                let bad = start.env.keys().find(|key| {
                    key.is_empty() || key.contains('=') || key.contains(char::is_whitespace)
                });
                if let Some(key) = bad {
                    return Err(format!("{key:?} isn't a variable's name"));
                }
            }
        }
        Ok(())
    }

    /// Works out the tabs to lay out with the sessions `there` are, using
    /// those, and having `start` start the others the file says how to:
    /// it gives the name of the session it started, or why it couldn't.
    pub fn plan(
        &self,
        there: &[String],
        mut start: impl FnMut(&Start) -> Result<String, String>,
    ) -> Planned {
        let mut planned = Planned::default();
        let mut name_of = |wanted: &Start, planned: &mut Planned| match &wanted.session {
            Some(name) if there.contains(name) => Some(name.clone()),
            Some(name) if !wanted.says_how() => {
                planned.left_out.push(format!(
                    "left out {name}: it isn't there, and the layout doesn't say how to start it"
                ));
                None
            }
            _ => match start(wanted) {
                Ok(name) => {
                    planned.started.push(name.clone());
                    Some(name)
                }
                Err(why) => {
                    let who = wanted.who();
                    planned
                        .left_out
                        .push(format!("left out {who}: couldn't start it: {why}"));
                    None
                }
            },
        };
        for (index, tab) in self.tabs.iter().enumerate() {
            let panes = tab
                .panes
                .as_ref()
                .and_then(|panes| panes.resolve(&mut |wanted| name_of(wanted, &mut planned)));
            let mut panes = panes.unwrap_or(layout::Tile::Pane {
                session: None,
                selection: true,
            });
            mark_selection(&mut panes);
            let floating = (tab.floating.as_ref()).and_then(|wanted| name_of(wanted, &mut planned));
            let mut sessions = Vec::new();
            shown_in(&panes, &mut sessions);
            sessions.extend(floating.clone());
            for wanted in &tab.sessions {
                if let Some(name) = name_of(wanted, &mut planned) {
                    sessions.push(name);
                }
            }
            let selected = tab.selected.clone().filter(|name| sessions.contains(name));
            planned.tabs.push(TabLayout {
                number: index + 1,
                name: tab.name.clone(),
                current: tab.current,
                zoomed: tab.zoomed,
                sessions,
                selected,
                floating,
                panes,
            });
        }
        planned
    }

    /// The tabs of `layout` as a file, or only the one `tab` names, by its
    /// number or its name; each session with what starts it again, as
    /// `sessions` has them: one in the background is named alone.
    pub fn export(
        layout: &Layout,
        sessions: &[SessionInfo],
        tab: Option<&str>,
    ) -> Result<File, String> {
        let chosen: Vec<&TabLayout> = match tab {
            None => layout.tabs.iter().collect(),
            Some(tab) => {
                let count = layout.tabs.len();
                let numbered = tab.parse::<usize>().ok();
                let numbered = numbered.filter(|number| (1..=count).contains(number));
                let found = numbered
                    .map(|number| &layout.tabs[number - 1])
                    .or_else(|| layout.tabs.iter().find(|held| held.name == tab));
                vec![found.ok_or_else(|| format!("there's no tab {tab}"))?]
            }
        };
        let tabs = chosen.into_iter().map(|tab| {
            let panes = match &tab.panes {
                layout::Tile::Pane { session: None, .. } => None,
                tile => Some(Tile::of(tile, sessions)),
            };
            let mut shown = Vec::new();
            shown_in(&tab.panes, &mut shown);
            let mut selection = None;
            find_selection(&tab.panes, &mut selection);
            let others = (tab.sessions.iter())
                .filter(|name| !shown.contains(name) && tab.floating.as_ref() != Some(*name));
            Tab {
                name: tab.name.clone(),
                current: tab.current,
                zoomed: tab.zoomed,
                selected: tab
                    .selected
                    .clone()
                    .filter(|name| selection.as_ref() != Some(name)),
                panes,
                floating: (tab.floating.as_deref()).map(|name| Start::of(name, sessions)),
                sessions: others.map(|name| Start::of(name, sessions)).collect(),
            }
        });
        Ok(File {
            tabs: tabs.collect(),
        })
    }
}

/// The session the pane of `tile` that follows the selection shows.
fn find_selection(tile: &layout::Tile, found: &mut Option<String>) {
    match tile {
        layout::Tile::Pane {
            session,
            selection: true,
        } if found.is_none() => *found = session.clone(),
        layout::Tile::Pane { .. } => {}
        layout::Tile::Split { first, second, .. } => {
            find_selection(first, found);
            find_selection(second, found);
        }
    }
}

/// `crystal layout export`: prints the TUI's tabs, or the one `tab` names,
/// as a layout file.
pub fn export(socket: &Path, tab: Option<&str>) -> Result<()> {
    let layout = client::lay_out(socket, Command::Show)?;
    let sessions = sessions(socket, false)?;
    let file = File::export(&layout, &sessions, tab).map_err(anyhow::Error::msg)?;
    println!("{}", serde_json::to_string_pretty(&file)?);
    Ok(())
}

/// `crystal layout apply`: lays the TUI's tabs out the way the layout file
/// at `path` says, or the one on standard input, starting the sessions it
/// says how to that aren't there: see [`Command::Apply`]. Prints the name
/// of each session it started, and says what it left out.
pub fn apply(socket: &Path, path: Option<&Path>, replace: bool) -> Result<()> {
    let text = match path.filter(|path| *path != Path::new("-")) {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("couldn't read {}", path.display()))?,
        None => {
            if std::io::stdin().is_terminal() {
                bail!(
                    "give a layout file, or one on standard input: `crystal layout export` writes one"
                );
            }
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            text
        }
    };
    let here = std::env::current_dir()?;
    let file = File::read(&text, &here).map_err(anyhow::Error::msg)?;
    let there: Vec<String> = (sessions(socket, true)?.into_iter())
        .map(|session| session.name)
        .collect();
    let planned = file.plan(&there, |wanted| {
        let cwd = wanted.cwd.clone().unwrap_or_else(|| here.clone());
        let command = wanted.command.clone().unwrap_or_default();
        let env: Vec<(String, String)> = wanted.env.clone().into_iter().collect();
        let name = wanted.session.clone();
        let started =
            client::new_session_with(socket, name, cwd, command, Purpose::default(), &env);
        started
            .map(|started| started.name)
            .map_err(|err| format!("{err:#}"))
    });
    for why in &planned.left_out {
        eprintln!("{}", printable::line(why));
    }
    for name in &planned.started {
        println!("{name}");
    }
    client::lay_out(
        socket,
        Command::Apply {
            tabs: planned.tabs,
            replace,
        },
    )?;
    Ok(())
}

/// The sessions there are, starting the daemon first with `start`.
fn sessions(socket: &Path, start: bool) -> Result<Vec<SessionInfo>> {
    match client::ask(socket, &Request::List, start)? {
        Some(Response::Sessions { sessions }) => Ok(sessions),
        _ => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::Presence;
    use crate::protocol::State;

    fn session(name: &str, command: &[&str], cwd: &str) -> SessionInfo {
        SessionInfo {
            stopped_idle: false,
            name: name.into(),
            id: format!("id-{name}"),
            command: command.iter().map(|arg| arg.to_string()).collect(),
            cwd: PathBuf::from(cwd),
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

    fn read(json: serde_json::Value) -> Result<File, String> {
        File::read(&json.to_string(), Path::new("/work"))
    }

    fn there(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// Plans `file` with the sessions `names` there, each one started
    /// named as asked, or `new-1`, `new-2`, … when it isn't.
    fn plan(file: &File, names: &[&str]) -> (Planned, Vec<Start>) {
        let mut asked = Vec::new();
        let planned = file.plan(&there(names), |wanted| {
            asked.push(wanted.clone());
            match &wanted.session {
                Some(name) if name == "broken" => Err("its directory is gone".into()),
                Some(name) => Ok(name.clone()),
                None => Ok(format!("new-{}", asked.len())),
            }
        });
        (planned, asked)
    }

    fn pane(session: &str, selection: bool) -> layout::Tile {
        layout::Tile::Pane {
            session: Some(session.into()),
            selection,
        }
    }

    #[test]
    fn a_file_names_a_session_or_says_how_to_start_it() {
        let file = read(serde_json::json!({"tabs": [{
            "name": "dev",
            "current": true,
            "panes": {
                "kind": "split", "way": "right", "ratio": 0.65,
                "first": {"kind": "pane", "session": "editor", "cwd": "~/repo", "command": ["nvim"]},
                "second": {"kind": "pane", "cwd": "tests", "env": {"ROLE": "tests"}}
            },
            "floating": "logs",
            "sessions": [{"session": "notes", "cwd": "/notes"}, "scratch"]
        }]}))
        .unwrap();
        let tab = &file.tabs[0];
        let home = shell::expand_home(Path::new("~/repo"));
        let Some(Tile::Split { first, second, .. }) = &tab.panes else {
            panic!("the panes are split");
        };
        let Tile::Pane { start, selection } = &**first else {
            panic!("a pane");
        };
        assert!(!selection);
        assert_eq!(start.cwd.as_deref(), Some(home.as_path()));
        assert_eq!(start.command.as_deref(), Some(&["nvim".to_string()][..]));
        let Tile::Pane { start, .. } = &**second else {
            panic!("a pane");
        };
        // A directory is made whole from where it's applied.
        assert_eq!(start.cwd.as_deref(), Some(Path::new("/work/tests")));
        assert_eq!(start.env["ROLE"], "tests");
        assert_eq!(tab.floating, Some(Start::named("logs")));
        assert_eq!(tab.sessions[1], Start::named("scratch"));

        let said = read(serde_json::json!({"tabs": [{"sessions": [3]}]})).unwrap_err();
        assert!(said.starts_with("couldn't read the layout"), "{said}");
        assert!(said.contains("a session's name, or an object"), "{said}");
    }

    #[test]
    fn a_file_says_why_it_cant_be_applied() {
        let said = |json| read(json).unwrap_err();
        let split = |first, second| {
            serde_json::json!({"kind": "split", "way": "down", "ratio": 0.5,
                "first": first, "second": second})
        };
        let pane = |name: &str| serde_json::json!({"kind": "pane", "session": name});
        assert_eq!(
            said(serde_json::json!({"tabs": []})),
            "the layout has no tabs"
        );
        assert_eq!(
            said(serde_json::json!({"tabs": [{"current": true}, {"current": true}]})),
            "only one tab can be current"
        );
        assert_eq!(
            said(serde_json::json!({"tabs": [{"name": "dev"}, {"name": "dev"}]})),
            "two tabs are called dev"
        );
        assert_eq!(
            said(serde_json::json!({"tabs": [{"panes": split(pane("a"), pane("a"))}]})),
            "a is on screen twice in tab 1"
        );
        assert_eq!(
            said(serde_json::json!({"tabs": [{"panes": pane("a"), "floating": "a"}]})),
            "a is on screen twice in tab 1"
        );
        assert_eq!(
            said(
                serde_json::json!({"tabs": [{"sessions": ["a"]}, {"name": "x", "panes": pane("a")}]})
            ),
            "a is in two tabs"
        );
        assert_eq!(
            said(serde_json::json!({"tabs": [{"name": "x", "selected": "b", "sessions": ["a"]}]})),
            "tab x selects b, which isn't in it"
        );
        let both =
            |name: &str| serde_json::json!({"kind": "pane", "session": name, "selection": true});
        assert_eq!(
            said(serde_json::json!({"tabs": [{"panes": split(both("a"), both("b"))}]})),
            "tab 1 has two panes marked as the selection's"
        );
        let whole = serde_json::json!({"kind": "split", "way": "down", "ratio": 1.0,
            "first": pane("a"), "second": pane("b")});
        assert_eq!(
            said(serde_json::json!({"tabs": [{"panes": whole}]})),
            "tab 1 has a split whose ratio isn't a share of its room, between 0 and 1"
        );
        let mut deep = pane("a");
        for _ in 0..=DEEPEST {
            deep = split(deep, serde_json::json!({"kind": "pane"}));
        }
        assert_eq!(
            said(serde_json::json!({"tabs": [{"panes": deep}]})),
            "tab 1's splits go more than 16 deep"
        );
        assert_eq!(
            said(
                serde_json::json!({"tabs": [{"sessions": [{"session": "a", "env": {"A B": "1"}}]}]})
            ),
            "\"A B\" isn't a variable's name"
        );
        assert_eq!(
            said(serde_json::json!({"tabs": [{"sessions": [""]}]})),
            "tab 1 names a session with an empty name"
        );
    }

    #[test]
    fn a_plan_starts_what_isnt_there_and_leaves_out_what_it_cant() {
        let file = read(serde_json::json!({"tabs": [
            {
                "name": "dev",
                "panes": {
                    "kind": "split", "way": "right", "ratio": 0.6,
                    "first": {"kind": "pane", "session": "editor"},
                    "second": {"kind": "split", "way": "down", "ratio": 0.5,
                        "first": {"kind": "pane", "session": "tests", "command": ["cargo", "test"]},
                        "second": {"kind": "pane", "session": "gone"}}
                },
                "sessions": ["notes", {"session": "broken", "cwd": "/x"}]
            },
            {"current": true, "panes": {"kind": "pane", "cwd": "/tmp"}, "floating": {"session": "logs", "cwd": "/var/log"}}
        ]}))
        .unwrap();
        let (planned, asked) = plan(&file, &["editor", "notes"]);
        let asked: Vec<String> = asked.iter().map(Start::who).collect();
        assert_eq!(asked, ["tests", "broken", "a new session", "logs"]);
        assert_eq!(planned.started, ["tests", "new-3", "logs"]);
        assert_eq!(
            planned.left_out,
            [
                "left out gone: it isn't there, and the layout doesn't say how to start it",
                "left out broken: couldn't start it: its directory is gone",
            ]
        );
        let dev = &planned.tabs[0];
        assert_eq!(dev.name, "dev");
        assert_eq!(dev.sessions, ["editor", "tests", "notes"]);
        // Gone, its pane is left out, and the first pane follows the
        // selection, there being no other marked.
        assert_eq!(
            dev.panes,
            layout::Tile::Split {
                way: Way::Right,
                ratio: 0.6,
                first: Box::new(pane("editor", true)),
                second: Box::new(pane("tests", false)),
            }
        );
        let second = &planned.tabs[1];
        assert!(second.current);
        assert_eq!(second.panes, pane("new-3", true));
        assert_eq!(second.floating.as_deref(), Some("logs"));
        assert_eq!(second.sessions, ["new-3", "logs"]);
    }

    #[test]
    fn the_selections_pane_with_nothing_said_shows_whats_selected() {
        let file = read(serde_json::json!({"tabs": [{
            "selected": "b",
            "panes": {"kind": "split", "way": "right", "ratio": 0.5,
                "first": {"kind": "pane", "selection": true},
                "second": {"kind": "pane", "session": "b"}},
            "sessions": ["a"]
        }]}))
        .unwrap();
        let (planned, asked) = plan(&file, &["a", "b"]);
        assert!(asked.is_empty());
        let tab = &planned.tabs[0];
        assert_eq!(tab.selected.as_deref(), Some("b"));
        let layout::Tile::Split { first, .. } = &tab.panes else {
            panic!("the panes are split");
        };
        assert_eq!(
            **first,
            layout::Tile::Pane {
                session: None,
                selection: true
            }
        );
    }

    #[test]
    fn what_the_list_says_of_a_session_on_screen_goes_to_its_pane() {
        let file = read(serde_json::json!({"tabs": [{
            "panes": {"kind": "pane", "session": "a", "selection": true},
            "sessions": [{"session": "a", "command": ["top"]}, "b"]
        }]}))
        .unwrap();
        let tab = &file.tabs[0];
        assert_eq!(tab.sessions, [Start::named("b")]);
        let Some(Tile::Pane { start, .. }) = &tab.panes else {
            panic!("a pane");
        };
        assert_eq!(start.command.as_deref(), Some(&["top".to_string()][..]));
    }

    /// A tab whose panes show a, b split off, and logs floating, with c in
    /// it too; and an empty one in front.
    fn tabs() -> Layout {
        let dev = TabLayout {
            number: 1,
            name: "dev".into(),
            current: false,
            zoomed: true,
            sessions: ["a", "b", "c", "logs", "task"].map(String::from).to_vec(),
            selected: Some("b".into()),
            floating: Some("logs".into()),
            panes: layout::Tile::Split {
                way: Way::Down,
                ratio: 0.3,
                first: Box::new(pane("a", true)),
                second: Box::new(pane("b", false)),
            },
        };
        let empty = TabLayout {
            number: 2,
            name: String::new(),
            current: true,
            zoomed: false,
            sessions: Vec::new(),
            selected: None,
            floating: None,
            panes: layout::Tile::Pane {
                session: None,
                selection: true,
            },
        };
        Layout {
            tabs: vec![dev, empty],
            presence: Presence::Unknown,
        }
    }

    fn running() -> Vec<SessionInfo> {
        let mut task = session("task", &["claude", "-p"], "/repo");
        task.front = Some(crate::protocol::Front::Task);
        vec![
            session("a", &["claude", "fix the login"], "/repo"),
            session("b", &["cargo", "test"], "/repo"),
            session("c", &["zsh"], "/home"),
            session("logs", &["tail", "-f", "log"], "/var/log"),
            task,
        ]
    }

    #[test]
    fn an_export_says_what_starts_each_session_again() {
        let file = File::export(&tabs(), &running(), None).unwrap();
        let json = serde_json::to_value(&file).unwrap();
        let dev = &json["tabs"][0];
        assert_eq!(dev["name"], "dev");
        assert_eq!(dev["zoomed"], true);
        assert!(dev.get("current").is_none());
        assert_eq!(dev["selected"], "b");
        assert_eq!(
            dev["panes"]["first"],
            serde_json::json!({"kind": "pane", "selection": true, "session": "a",
                "cwd": "/repo", "command": ["claude"]})
        );
        assert_eq!(
            dev["panes"]["second"]["command"],
            serde_json::json!(["cargo", "test"])
        );
        assert_eq!(
            dev["floating"]["command"],
            serde_json::json!(["tail", "-f", "log"])
        );
        // A task in the background has no terminal to start again.
        assert_eq!(
            dev["sessions"],
            serde_json::json!([{"session": "c", "cwd": "/home", "command": ["zsh"]},
                {"session": "task"}])
        );
        assert_eq!(json["tabs"][1], serde_json::json!({"current": true}));

        let one = File::export(&tabs(), &running(), Some("dev")).unwrap();
        assert_eq!(one.tabs, file.tabs[..1]);
        assert_eq!(
            File::export(&tabs(), &running(), Some("2")).unwrap().tabs,
            file.tabs[1..]
        );
        assert_eq!(
            File::export(&tabs(), &running(), Some("3")).unwrap_err(),
            "there's no tab 3"
        );
    }

    #[test]
    fn an_export_applied_lays_the_tabs_out_as_they_were() {
        let text =
            serde_json::to_string(&File::export(&tabs(), &running(), None).unwrap()).unwrap();
        let file = File::read(&text, Path::new("/elsewhere")).unwrap();
        let names: Vec<&str> = ["a", "b", "c", "logs", "task"].to_vec();
        let (planned, asked) = plan(&file, &names);
        assert!(asked.is_empty());
        let mut expected = tabs().tabs;
        expected[0].sessions = ["a", "b", "logs", "c", "task"].map(String::from).to_vec();
        assert_eq!(planned.tabs, expected);

        // And the output of `crystal layout --json` applies as it is.
        let json = serde_json::to_string(&tabs()).unwrap();
        let (planned, asked) = plan(&File::read(&json, Path::new("/")).unwrap(), &names);
        assert!(asked.is_empty());
        assert_eq!(planned.tabs[0].panes, tabs().tabs[0].panes);
        assert_eq!(planned.tabs[0].selected.as_deref(), Some("b"));
    }
}
