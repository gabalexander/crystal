//! Laying out the TUI from the command line: `crystal tab`, `crystal pane`
//! and `crystal layout`. A command goes through the daemon to the TUI used
//! last, which carries it out and answers with the layout it came to: see
//! [`crate::layout_relay`]. With no TUI open, the daemon carries it out
//! itself, the same way, on the tabs the TUIs keep in the database, where
//! the next TUI to open finds them. This is what goes between them, and how
//! `crystal layout` prints a layout; a layout as a file, which `crystal
//! layout export` writes and `crystal layout apply` reads, is
//! [`crate::layout_file`]'s.

use crate::notify::Presence;
use crate::tui::keymap::Extent;
use crate::tui::split_tree::{Direction, Way};
use serde::{Deserialize, Serialize};

/// Why an order can't be passed on to a TUI: none is open.
pub const NO_TUI: &str = "no TUI is running";

/// What a TUI is asked to do with its tabs and panes. A session named is
/// one of the TUI's; one that isn't named is the session the command was
/// run in, or else the one selected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "LayoutCommand"))]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Command {
    /// Nothing: the layout as it is.
    Show,
    /// Make a tab after the others, called `name` if it's given, and bring
    /// it to the front, where sessions started from then on go.
    NewTab { name: Option<String> },
    /// Bring a tab to the front: by its number, from 1, or its name.
    SelectTab { tab: String },
    /// Name a tab; an empty name takes it back to its number.
    RenameTab { tab: String, name: String },
    /// Close a tab, the one in front unless `tab` says. One that holds
    /// sessions closes only with `kill`, which kills them with it.
    CloseTab { tab: Option<String>, kill: bool },
    /// Move a session to another tab.
    MoveToTab { session: String, tab: String },
    /// Move a tab to `position`, from 1, the others making room.
    ReorderTab { tab: String, position: usize },
    /// Show `session` in a pane of its own, split off `way` from the pane
    /// of `beside`, which keeps `ratio` of the room.
    Split {
        session: String,
        beside: Option<String>,
        way: Way,
        ratio: f32,
    },
    /// Select a session, bringing its tab to the front, and hand its pane
    /// the keyboard; with `raise`, bring the TUI's terminal to the front
    /// too, as a click on a notification does.
    Focus {
        session: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        raise: bool,
    },
    /// The same for the session in the pane `toward` from the pane of the
    /// session it's about.
    FocusToward { toward: Direction },
    /// Move a border of a session's pane `cells` columns or rows `toward`,
    /// or as far as a key in resize mode does when it's `None`.
    Resize {
        session: Option<String>,
        toward: Direction,
        cells: Option<u16>,
    },
    /// Swap a session's pane with the pane `toward` from it, the splits
    /// and their ratios left as they are.
    SwapToward {
        session: Option<String>,
        toward: Direction,
    },
    /// The same with the pane of `with`, in the same tab.
    Swap {
        session: Option<String>,
        with: String,
    },
    /// Give a session's pane's side of the split nearest above it, or the
    /// nearest `way` when that's given, `share` of the split's room.
    Ratio {
        session: Option<String>,
        way: Option<Way>,
        share: f32,
    },
    /// Close a session's pane of its own: its split closes, or its float
    /// is put back.
    Close { session: Option<String> },
    /// Zoom a session's pane over its tab, selecting it there, or with
    /// `on` false, put the tab's panes back.
    Zoom { session: Option<String>, on: bool },
    /// Even out the panes of the tab the command was run in, or else the
    /// one in front.
    Equalize,
    /// Float a session over its tab's panes, or with `on` false, put the
    /// tab's float back among them.
    Float { session: Option<String>, on: bool },
    /// Show `session`, the pane of the plugin called `plugin`, over the
    /// panes with the keyboard, under `title`, or in a popup that size,
    /// until its program ends or the user closes it, which kills it. Only
    /// a TUI can.
    Overlay {
        session: String,
        plugin: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        popup: Option<Popup>,
    },
    /// Give the TUI's terminal the title `text`, in place of the one the
    /// settings make, or with none, go back to that one.
    Title { text: Option<String> },
    /// Lay tabs out as `tabs` has them, their numbers aside: each in place
    /// of the tab with its name, or after the others when it has none or
    /// there's no such tab; or with `replace`, in place of every tab, the
    /// sessions they don't hold joining the tab in front. The sessions they
    /// hold move to them, and one `current` comes to the front. The
    /// sessions are all there by now: `crystal layout apply` starts those
    /// that weren't (see [`crate::layout_file`]).
    Apply {
        tabs: Vec<TabLayout>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        replace: bool,
    },
}

/// How big a popup is: so many cells, or a share of the screen, each way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Popup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<Extent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<Extent>,
}

/// A command as a TUI gets it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "LayoutOrder"))]
pub struct Order {
    pub command: Command,
    /// The id of the session the command was run in, when it was run in
    /// one: the session it's about when it names none.
    pub caller: Option<String>,
}

/// An order the daemon passes on to a TUI, numbered for its answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "RelayedLayoutOrder"))]
pub struct Relayed {
    pub id: u64,
    pub order: Order,
}

/// What a TUI tells the daemon, a line at a time, once it takes orders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "LayoutReport"))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Report {
    /// The user did something in it, so orders go to it from now on.
    Used,
    /// Its terminal has gained the focus, or lost it.
    Focus { focused: bool },
    /// What came of the order numbered `id`: the layout it came to, or why
    /// it couldn't be carried out.
    Answer {
        id: u64,
        answer: Result<Layout, String>,
    },
}

/// A TUI's tabs and their panes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Layout {
    pub tabs: Vec<TabLayout>,
    /// Whether the user is at crystal, by what every TUI's terminal says of
    /// its focus: the daemon says, as a TUI knows only its own.
    #[serde(default)]
    pub presence: Presence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct TabLayout {
    /// Its number, from 1, as the tab bar shows it.
    pub number: usize,
    /// Empty until it's named.
    pub name: String,
    /// Whether it's the tab in front.
    pub current: bool,
    pub zoomed: bool,
    /// Its sessions, in the sidebar's order.
    pub sessions: Vec<String>,
    /// The session selected in it.
    pub selected: Option<String>,
    pub floating: Option<String>,
    pub panes: Tile,
}

/// A tab's panes: a pane, or a split of its room in two.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Tile {
    /// A session split off, or with `selection`, the pane that follows the
    /// selection, and the session it shows, if any.
    Pane {
        session: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        selection: bool,
    },
    /// The first side has `ratio` of the room, the second the rest.
    Split {
        way: Way,
        ratio: f32,
        first: Box<Tile>,
        second: Box<Tile>,
    },
}

impl Layout {
    /// The tab in front.
    pub fn current(&self) -> Option<&TabLayout> {
        self.tabs.iter().find(|tab| tab.current)
    }

    /// The layout as `crystal layout` prints it: each tab, its sessions and
    /// its float, then its panes, a split's two sides under it.
    pub fn text(&self) -> String {
        let mut lines = Vec::new();
        for tab in &self.tabs {
            let mut heading = tab.number.to_string();
            if !tab.name.is_empty() {
                heading = format!("{heading} {}", tab.name);
            }
            let about: Vec<&str> = [(tab.current, "in front"), (tab.zoomed, "zoomed")]
                .into_iter()
                .filter_map(|(is, word)| is.then_some(word))
                .collect();
            if !about.is_empty() {
                heading = format!("{heading} ({})", about.join(", "));
            }
            lines.push(heading);
            let sessions = if tab.sessions.is_empty() {
                "none".to_string()
            } else {
                tab.sessions.join(", ")
            };
            lines.push(format!("  sessions  {sessions}"));
            if let Some(floating) = &tab.floating {
                lines.push(format!("  floating  {floating}"));
            }
            lines.push("  panes".to_string());
            tab.panes.lines(2, &mut lines);
        }
        lines.iter().map(|line| format!("{line}\n")).collect()
    }
}

impl Tile {
    /// The tile's lines, indented `depth` steps: a pane's session, or a
    /// split's way and share, then each of its sides a step further in.
    fn lines(&self, depth: usize, lines: &mut Vec<String>) {
        let indent = "  ".repeat(depth);
        match self {
            Tile::Pane {
                session,
                selection: false,
            } => lines.push(format!("{indent}{}", session.as_deref().unwrap_or("?"))),
            Tile::Pane {
                session,
                selection: true,
            } => {
                let shows = session.as_deref().unwrap_or("nothing yet");
                lines.push(format!("{indent}the selection's: {shows}"));
            }
            Tile::Split {
                way,
                ratio,
                first,
                second,
            } => {
                let way = match way {
                    Way::Right => "side by side",
                    Way::Down => "one above the other",
                };
                let share = (ratio * 100.0).round();
                lines.push(format!("{indent}{way}, {share}% first"));
                first.lines(depth + 1, lines);
                second.lines(depth + 1, lines);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(session: &str) -> Box<Tile> {
        Box::new(Tile::Pane {
            session: Some(session.into()),
            selection: false,
        })
    }

    fn two_tabs() -> Layout {
        let review = TabLayout {
            number: 1,
            name: "review".into(),
            current: true,
            zoomed: false,
            sessions: vec!["alpha".into(), "beta".into(), "logs".into()],
            selected: Some("beta".into()),
            floating: Some("logs".into()),
            panes: Tile::Split {
                way: Way::Right,
                ratio: 0.4,
                first: pane("alpha"),
                second: Box::new(Tile::Pane {
                    session: Some("beta".into()),
                    selection: true,
                }),
            },
        };
        let empty = TabLayout {
            number: 2,
            name: String::new(),
            current: false,
            zoomed: true,
            sessions: Vec::new(),
            selected: None,
            floating: None,
            panes: Tile::Pane {
                session: None,
                selection: true,
            },
        };
        Layout {
            tabs: vec![review, empty],
            presence: Presence::Unknown,
        }
    }

    #[test]
    fn a_layout_prints_each_tab_and_its_panes_split_by_split() {
        let expected = "\
1 review (in front)
  sessions  alpha, beta, logs
  floating  logs
  panes
    side by side, 40% first
      alpha
      the selection's: beta
2 (zoomed)
  sessions  none
  panes
    the selection's: nothing yet
";
        assert_eq!(two_tabs().text(), expected);
    }

    #[test]
    fn a_layout_survives_the_round_trip_as_json() {
        let layout = two_tabs();
        let json = serde_json::to_value(&layout).unwrap();
        let panes = &json["tabs"][0]["panes"];
        assert_eq!(panes["kind"], "split");
        assert_eq!(panes["way"], "right");
        assert_eq!(
            panes["first"],
            serde_json::json!({"kind": "pane", "session": "alpha"})
        );
        assert_eq!(panes["second"]["selection"], true);
        let back: Layout = serde_json::from_value(json).unwrap();
        assert_eq!(back, layout);
        assert_eq!(back.current().unwrap().number, 1);
    }

    #[test]
    fn a_layout_says_where_the_user_is() {
        let mut layout = two_tabs();
        layout.presence = Presence::Away;
        let json = serde_json::to_value(&layout).unwrap();
        assert_eq!(json["presence"], "away");
        // Without it, nobody knows.
        let layout: Layout = serde_json::from_value(serde_json::json!({"tabs": []})).unwrap();
        assert_eq!(layout.presence, Presence::Unknown);
    }

    #[test]
    fn an_answer_says_what_went_wrong() {
        let report = Report::Answer {
            id: 3,
            answer: Err("there's no tab 4".into()),
        };
        let line = serde_json::to_string(&report).unwrap();
        assert_eq!(serde_json::from_str::<Report>(&line).unwrap(), report);
    }
}
