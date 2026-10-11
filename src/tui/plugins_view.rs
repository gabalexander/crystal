//! The plugins view, `X`: crystal's own plugins, the ones installed and
//! those the selected session's project ships, each on or off, with the
//! actions, panes and link handlers of the installed ones under them.
//! Space switches the plugin the bar is on, but for a project's that's
//! off, which `crystal plugin enable --project` turns on once it has shown
//! what it runs; and Enter runs the action, or opens the pane, the bar is
//! on. The event loop does the writing and the running (see
//! [`crate::plugins`]).
//!
//! The view is state and logic only, apart from [`draw`] at the end.

use super::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use std::path::PathBuf;

/// A plugin as the view lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub name: String,
    pub description: String,
    pub built_in: bool,
    /// The main worktree of the project that ships it, for a project's.
    pub project: Option<PathBuf>,
    pub on: bool,
    /// Why it doesn't run, though it may be on: paused for failing, or a
    /// manifest that doesn't make sense.
    pub trouble: Option<String>,
    pub actions: Vec<Item>,
    pub panes: Vec<Item>,
    pub links: Vec<LinkItem>,
}

/// One of a plugin's link handlers: the links it takes, and the title of
/// the action it runs on them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkItem {
    pub pattern: String,
    pub action: String,
}

/// One of a plugin's actions or panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub title: String,
    /// The sidebar key that runs an action, if it has one.
    pub key: Option<String>,
}

/// What a key in the view leads to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    /// Turn the plugin called `name`, the project's when it's given, on,
    /// or off.
    Switch {
        name: String,
        project: Option<PathBuf>,
        on: bool,
    },
    /// Run one of a plugin's actions.
    Run {
        plugin: String,
        project: Option<PathBuf>,
        action: String,
    },
    /// Open one of a plugin's panes.
    Open {
        plugin: String,
        project: Option<PathBuf>,
        pane: String,
    },
}

/// A row of the list: a heading, a plugin, or one of its actions or panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Heading(&'static str),
    /// The heading over the plugins of the project of the plugin it names.
    ProjectHeading(usize),
    Plugin(usize),
    Action(usize, usize),
    Pane(usize, usize),
    Link(usize, usize),
}

pub struct PluginsView {
    plugins: Vec<Listed>,
    rows: Vec<Row>,
    /// The row the bar is on: never a heading.
    selected: usize,
    /// Why the last thing asked for couldn't be done.
    problem: Option<String>,
}

impl PluginsView {
    pub fn new(plugins: Vec<Listed>) -> PluginsView {
        let mut view = PluginsView {
            plugins: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            problem: None,
        };
        view.set_plugins(plugins);
        view
    }

    /// Takes the plugins as they are now, keeping the bar on the row it
    /// was on.
    pub fn set_plugins(&mut self, plugins: Vec<Listed>) {
        let was = self.rows.get(self.selected).copied();
        self.plugins = plugins;
        self.rows = rows(&self.plugins);
        self.selected = was
            .and_then(|was| self.rows.iter().position(|row| *row == was))
            .or_else(|| self.rows.iter().position(|row| !is_heading(row)))
            .unwrap_or(0);
    }

    pub fn set_problem(&mut self, problem: String) {
        self.problem = Some(problem);
    }

    #[cfg(test)]
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        self.problem = None;
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return Outcome::Stay;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 'X') => Outcome::Close,
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_by(1);
                Outcome::Stay
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_by(-1);
                Outcome::Stay
            }
            KeyCode::Char(' ') => self.switch(),
            KeyCode::Enter => self.go(),
            _ => Outcome::Stay,
        }
    }

    /// Moves the bar `by` rows, over the headings.
    fn move_by(&mut self, by: isize) {
        let mut at = self.selected;
        loop {
            let Some(next) = at
                .checked_add_signed(by)
                .filter(|&next| next < self.rows.len())
            else {
                return;
            };
            at = next;
            if !is_heading(&self.rows[at]) {
                self.selected = at;
                return;
            }
        }
    }

    /// The plugin the bar's row belongs to.
    fn plugin(&self) -> Option<&Listed> {
        let index = match self.rows.get(self.selected)? {
            Row::Heading(_) | Row::ProjectHeading(_) => return None,
            Row::Plugin(index)
            | Row::Action(index, _)
            | Row::Pane(index, _)
            | Row::Link(index, _) => *index,
        };
        self.plugins.get(index)
    }

    fn switch(&mut self) -> Outcome {
        let Some(plugin) = self.plugin() else {
            return Outcome::Stay;
        };
        // Code a repository ships runs once its commands have been seen.
        if plugin.project.is_some() && !plugin.on {
            self.problem = Some(format!(
                "`crystal plugin enable {} --project` shows what it runs, then turns it on",
                plugin.name
            ));
            return Outcome::Stay;
        }
        Outcome::Switch {
            name: plugin.name.clone(),
            project: plugin.project.clone(),
            on: !plugin.on,
        }
    }

    fn go(&mut self) -> Outcome {
        let Some(plugin) = self.plugin() else {
            return Outcome::Stay;
        };
        let (plugin_name, on) = (plugin.name.clone(), plugin.on);
        let project = plugin.project.clone();
        let outcome = match self.rows[self.selected] {
            Row::Action(index, action) => Outcome::Run {
                plugin: plugin_name.clone(),
                project,
                action: self.plugins[index].actions[action].id.clone(),
            },
            Row::Pane(index, pane) => Outcome::Open {
                plugin: plugin_name.clone(),
                project,
                pane: self.plugins[index].panes[pane].id.clone(),
            },
            _ => return Outcome::Stay,
        };
        if !on {
            self.problem = Some(format!("{plugin_name} is off: space turns it on"));
            return Outcome::Stay;
        }
        outcome
    }
}

fn is_heading(row: &Row) -> bool {
    matches!(row, Row::Heading(_) | Row::ProjectHeading(_))
}

/// The list's rows: crystal's own plugins, then the installed ones, then
/// the project's, each with its actions and panes.
fn rows(plugins: &[Listed]) -> Vec<Row> {
    let mut rows = vec![Row::Heading("crystal's own")];
    let own = plugins
        .iter()
        .enumerate()
        .filter(|(_, plugin)| plugin.built_in);
    rows.extend(own.map(|(index, _)| Row::Plugin(index)));
    rows.push(Row::Heading("installed"));
    let installed = |plugin: &&Listed| !plugin.built_in && plugin.project.is_none();
    let projects = |plugin: &&Listed| plugin.project.is_some();
    for (shipped, kept) in [(false, installed as fn(&&Listed) -> bool), (true, projects)] {
        let mut listed = plugins
            .iter()
            .enumerate()
            .filter(|(_, p)| kept(p))
            .peekable();
        if let (true, Some((first, _))) = (shipped, listed.peek()) {
            rows.push(Row::ProjectHeading(*first));
        }
        for (index, plugin) in listed {
            rows.push(Row::Plugin(index));
            rows.extend((0..plugin.actions.len()).map(|action| Row::Action(index, action)));
            rows.extend((0..plugin.panes.len()).map(|pane| Row::Pane(index, pane)));
            rows.extend((0..plugin.links.len()).map(|link| Row::Link(index, link)));
        }
    }
    rows
}

/// The keys while the view is open.
pub const HINTS: &[(&str, &str)] = &[
    ("space", "on/off"),
    ("enter", "run or open"),
    ("j/k", "move"),
    ("esc", "close"),
];

/// Draws the view over the middle of `area`, the rest dimmed behind it.
pub fn draw(frame: &mut Frame, view: &PluginsView, theme: &Theme, area: Rect) {
    frame
        .buffer_mut()
        .set_style(area, Style::new().add_modifier(Modifier::DIM));
    let mut lines: Vec<Line> = view
        .rows
        .iter()
        .enumerate()
        .map(|(at, row)| row_line(view, *row, at == view.selected, theme))
        .collect();
    if view
        .plugins
        .iter()
        .all(|plugin| plugin.built_in || plugin.project.is_some())
    {
        lines.push(Line::styled(
            "  none yet: `crystal plugin new <name>` makes one",
            Style::new().fg(theme.muted),
        ));
    }
    if let Some(problem) = &view.problem {
        lines.push(Line::from(""));
        lines.push(Line::styled(problem.clone(), Style::new().fg(theme.failed)));
    }

    let width = area.width.saturating_sub(4).clamp(40.min(area.width), 90);
    let height = (lines.len() as u16 + 2).min(area.height);
    let top = if area.height > height { 1 } else { 0 };
    let panel = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + top,
        width,
        height,
    );
    frame.render_widget(Clear, panel);
    let framed = theme.panel == Color::Reset;
    let block = if framed {
        Block::bordered().border_style(Style::new().fg(theme.rule))
    } else {
        Block::new()
    };
    let block = block.style(Style::new().bg(theme.panel).fg(theme.text));
    let inside = block
        .inner(panel)
        .inner(Margin::new(if framed { 1 } else { 2 }, 1));
    frame.render_widget(block, panel);
    // The bar's row is kept in sight.
    let skip = (view.selected + 1).saturating_sub(usize::from(inside.height));
    frame.render_widget(Paragraph::new(lines).scroll((skip as u16, 0)), inside);
}

fn row_line(view: &PluginsView, row: Row, selected: bool, theme: &Theme) -> Line<'static> {
    let muted = Style::new().fg(theme.muted);
    let line = match row {
        Row::Heading(heading) => {
            return Line::styled(heading, Style::new().add_modifier(Modifier::BOLD));
        }
        Row::ProjectHeading(index) => {
            let project = view.plugins[index].project.as_deref();
            let name = project.map(crate::project::name_of).unwrap_or_default();
            let heading = format!("{name}'s own, on for it alone");
            return Line::styled(heading, Style::new().add_modifier(Modifier::BOLD));
        }
        Row::Plugin(index) => {
            let plugin = &view.plugins[index];
            let (mark, color) = if plugin.on {
                ("●", theme.done)
            } else {
                ("○", theme.muted)
            };
            let mut spans = vec![
                Span::styled(format!("{mark} "), Style::new().fg(color)),
                Span::styled(format!("{:<14}", plugin.name), Style::new().fg(theme.text)),
                Span::styled(plugin.description.clone(), muted),
            ];
            if let Some(trouble) = &plugin.trouble {
                let trouble = format!("  {trouble}");
                spans.push(Span::styled(trouble, Style::new().fg(theme.failed)));
            }
            Line::from(spans)
        }
        Row::Action(index, action) => {
            let action = &view.plugins[index].actions[action];
            let key = action
                .key
                .as_ref()
                .map(|key| format!("  key {key}"))
                .unwrap_or_default();
            item_line("run", &action.title, &key, view.plugins[index].on, theme)
        }
        Row::Pane(index, pane) => {
            let pane = &view.plugins[index].panes[pane];
            item_line("open", &pane.title, "", view.plugins[index].on, theme)
        }
        Row::Link(index, link) => {
            let link = &view.plugins[index].links[link];
            let runs = format!("  → {}", link.action);
            item_line("link", &link.pattern, &runs, view.plugins[index].on, theme)
        }
    };
    if selected {
        line.style(theme.selection)
    } else {
        line
    }
}

/// An action's or a pane's row, under its plugin; dim while the plugin is
/// off.
fn item_line(verb: &str, title: &str, after: &str, on: bool, theme: &Theme) -> Line<'static> {
    let text = if on { theme.text } else { theme.muted };
    Line::from(vec![
        Span::raw("    "),
        Span::styled(format!("{verb:<5}"), Style::new().fg(theme.accent)),
        Span::styled(title.to_string(), Style::new().fg(text)),
        Span::styled(after.to_string(), Style::new().fg(theme.muted)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(view: &mut PluginsView, code: KeyCode) -> Outcome {
        view.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn plugin(name: &str, built_in: bool, on: bool) -> Listed {
        let item = |id: &str| Item {
            id: id.into(),
            title: id.to_uppercase(),
            key: None,
        };
        Listed {
            name: name.into(),
            description: String::new(),
            built_in,
            project: None,
            on,
            trouble: None,
            actions: if built_in { vec![] } else { vec![item("note")] },
            panes: if built_in {
                vec![]
            } else {
                vec![item("board")]
            },
            links: Vec::new(),
        }
    }

    fn view() -> PluginsView {
        PluginsView::new(vec![
            plugin("memory", true, true),
            plugin("notes", false, true),
            plugin("todo", false, false),
        ])
    }

    #[test]
    fn space_switches_the_plugin_the_bar_is_on() {
        let mut view = view();
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Switch {
                name: "memory".into(),
                project: None,
                on: false
            }
        );
        // Down over the heading, to notes, then its action: still notes.
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Switch {
                name: "notes".into(),
                project: None,
                on: false
            }
        );
    }

    #[test]
    fn enter_runs_an_action_or_opens_a_pane() {
        let mut view = view();
        press(&mut view, KeyCode::Down);
        assert_eq!(press(&mut view, KeyCode::Enter), Outcome::Stay);
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Outcome::Run {
                plugin: "notes".into(),
                project: None,
                action: "note".into()
            }
        );
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Outcome::Open {
                plugin: "notes".into(),
                project: None,
                pane: "board".into()
            }
        );
    }

    #[test]
    fn a_plugin_thats_off_runs_nothing_and_says_so() {
        let mut view = view();
        for _ in 0..5 {
            press(&mut view, KeyCode::Down);
        }
        assert_eq!(press(&mut view, KeyCode::Enter), Outcome::Stay);
        assert_eq!(view.problem(), Some("todo is off: space turns it on"));
    }

    #[test]
    fn a_link_handler_is_listed_under_its_plugin_and_enter_on_it_does_nothing() {
        let mut notes = plugin("notes", false, true);
        notes.links.push(LinkItem {
            pattern: "^https://".into(),
            action: "NOTE".into(),
        });
        let mut view = PluginsView::new(vec![plugin("memory", true, true), notes]);
        for _ in 0..4 {
            press(&mut view, KeyCode::Down);
        }
        assert_eq!(view.rows[view.selected], Row::Link(1, 0));
        assert_eq!(press(&mut view, KeyCode::Enter), Outcome::Stay);
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Switch {
                name: "notes".into(),
                project: None,
                on: false
            }
        );
    }

    #[test]
    fn a_projects_plugins_come_under_its_name_and_only_the_command_line_turns_one_on() {
        let project = |plugin: Listed| Listed {
            project: Some(PathBuf::from("/code/app")),
            ..plugin
        };
        let mut view = PluginsView::new(vec![
            plugin("memory", true, true),
            project(plugin("lint", false, false)),
            plugin("notes", false, true),
            project(plugin("deploy", false, true)),
        ]);
        let headings: Vec<Row> = view.rows.iter().copied().filter(is_heading).collect();
        assert_eq!(
            headings,
            [
                Row::Heading("crystal's own"),
                Row::Heading("installed"),
                Row::ProjectHeading(1)
            ]
        );
        // memory, notes and its two, then the project's lint.
        for _ in 0..4 {
            press(&mut view, KeyCode::Down);
        }
        assert_eq!(view.rows[view.selected], Row::Plugin(1));
        assert_eq!(press(&mut view, KeyCode::Char(' ')), Outcome::Stay);
        assert!(
            view.problem()
                .unwrap()
                .contains("crystal plugin enable lint --project")
        );
        // One that's on goes off here, and runs for its project: past
        // lint's action and pane, to deploy's action.
        for _ in 0..4 {
            press(&mut view, KeyCode::Down);
        }
        assert_eq!(view.rows[view.selected], Row::Action(3, 0));
        let app = Some(PathBuf::from("/code/app"));
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Outcome::Run {
                plugin: "deploy".into(),
                project: app.clone(),
                action: "note".into()
            }
        );
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Switch {
                name: "deploy".into(),
                project: app,
                on: false
            }
        );
    }

    #[test]
    fn the_bar_stays_on_its_row_when_the_list_comes_again() {
        let mut view = view();
        press(&mut view, KeyCode::Down);
        let mut plugins = view.plugins.clone();
        plugins[1].on = false;
        view.set_plugins(plugins);
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Switch {
                name: "notes".into(),
                project: None,
                on: true
            }
        );
        // The bar never goes past the ends.
        for _ in 0..20 {
            press(&mut view, KeyCode::Up);
        }
        assert_eq!(view.selected, 1);
        assert_eq!(press(&mut view, KeyCode::Esc), Outcome::Close);
    }
}
