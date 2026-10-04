//! The RAM view, `#` in the sidebar: the memory each session takes, its
//! program and every process under it, the biggest first, how many
//! processes that is and what share of the machine's; then what crystal
//! takes itself, the daemon and this TUI, and all of it together. The
//! daemon looks at the processes (see [`crate::resources`]), asked off the
//! event loop every second while the view is open, and every few seconds
//! while it's closed, for the footer's readout. Enter goes to the session
//! the bar is on.
//!
//! The view is state and keys, kept apart from I/O, and its drawing.

use super::listing;
use super::needs_you;
use super::sidebar::fit;
use super::theme::Theme;
use crate::protocol::SessionInfo;
use crate::resources::{self, Resources, Usage};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear};

/// A session's row: what its processes take, and where it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    /// Its project and branch, or its directory.
    pub place: String,
    pub usage: Usage,
}

/// The sessions' rows, from what the daemon found they take, the biggest
/// first, and where each runs, from `sessions`.
pub fn rows(resources: &Resources, sessions: &[SessionInfo]) -> Vec<Row> {
    let mut rows: Vec<Row> = resources
        .sessions
        .iter()
        .map(|taken| Row {
            name: taken.name.clone(),
            place: sessions
                .iter()
                .find(|session| session.name == taken.name)
                .map(needs_you::place)
                .unwrap_or_default(),
            usage: taken.usage,
        })
        .collect();
    rows.sort_by(|a, b| b.usage.bytes.cmp(&a.usage.bytes).then(a.name.cmp(&b.name)));
    rows
}

/// What a key in the view asks for beyond it.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Stay,
    Close,
    /// Go to the session with this name.
    Go(String),
}

#[derive(Debug, Default)]
pub struct RamView {
    rows: Vec<Row>,
    /// Where the bar is among the rows.
    at: usize,
}

impl RamView {
    pub fn new(rows: Vec<Row>) -> RamView {
        RamView { rows, at: 0 }
    }

    pub fn highlighted(&self) -> Option<&Row> {
        self.rows.get(self.at)
    }

    /// Takes the rows as they are now, which a new look at the processes
    /// orders anew: the bar stays on the session it was on, or where it
    /// was when that one has gone.
    pub fn refresh(&mut self, rows: Vec<Row>) {
        let on = self.highlighted().map(|row| row.name.clone());
        self.rows = rows;
        let found = on.and_then(|on| self.rows.iter().position(|row| row.name == on));
        self.at = found
            .unwrap_or(self.at)
            .min(self.rows.len().saturating_sub(1));
    }

    /// `j`/`k` and the arrows move; Enter goes to the row's session; Esc or
    /// `q` closes.
    pub fn on_key(&mut self, key: &KeyEvent) -> Step {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Step::Close,
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            KeyCode::Enter => {
                if let Some(row) = self.highlighted() {
                    return Step::Go(row.name.clone());
                }
            }
            _ => {}
        }
        Step::Stay
    }

    fn move_by(&mut self, by: isize) {
        let last = self.rows.len().saturating_sub(1);
        self.at = self.at.saturating_add_signed(by).min(last);
    }
}

/// The keys the footer offers.
pub const HINTS: &[(&str, &str)] = &[("enter", "go to it"), ("j/k", "move"), ("esc", "close")];

/// What the footer shows of it at its right: all crystal takes, once the
/// daemon has said.
pub fn readout(resources: &Resources) -> String {
    resources::size(resources.all())
}

/// Draws the view in `area`: a heading with all of it, a row each session,
/// and what crystal takes itself at the bottom. `None` until the daemon has
/// said.
pub fn draw(
    frame: &mut Frame,
    view: &RamView,
    resources: Option<&Resources>,
    theme: &Theme,
    area: Rect,
) {
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().style(theme.base()), area);
    let [heading, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let Some(resources) = resources else {
        frame.render_widget(heading_line("RAM", String::new(), theme), heading);
        return listing::draw_note(frame, theme, "looking at the processes…", rest);
    };
    let all = resources.all();
    let of_machine = match resources.total {
        0 => String::new(),
        total => format!(", {} of {}", share(all, total), resources::size(total)),
    };
    let said = format!(" · {}{of_machine}", resources::size(all));
    frame.render_widget(heading_line("RAM", said, theme), heading);
    let [list, rule, own] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(rest);
    if view.rows.is_empty() {
        listing::draw_note(frame, theme, "no session's program is running", list);
    }
    let first = listing::first_drawn(view.at, list.height);
    let shown = view.rows.iter().enumerate().skip(first);
    for (index, row) in shown.take(usize::from(list.height)) {
        let highlighted = index == view.at;
        let area = listing::row_area(frame, theme, list, index - first, highlighted);
        frame.render_widget(row_line(row, all, theme, highlighted, area.width), area);
    }
    listing::draw_rule(frame, theme, rule);
    frame.render_widget(own_line(resources, theme), own);
}

fn heading_line<'a>(title: &str, said: String, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!(" {title}"),
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(said, Style::new().fg(theme.muted)),
    ])
}

/// A session's row: its name, where it runs, its processes, the memory
/// they take, and a bar of its share of all crystal takes.
fn row_line<'a>(row: &Row, all: u64, theme: &Theme, highlighted: bool, width: u16) -> Line<'a> {
    const NAME: usize = 22;
    const PLACE: usize = 26;
    const BAR: usize = 10;
    let mut name = Style::new().fg(theme.text);
    if highlighted {
        name = name.add_modifier(Modifier::BOLD);
    }
    let processes = match row.usage.processes {
        1 => "1 process".to_string(),
        count => format!("{count} processes"),
    };
    let size = format!("{:>7}", resources::size(row.usage.bytes));
    // The name, its spaces and the numbers come first; the place gets what's
    // left, up to its share.
    let numbers = 1 + 13 + 1 + size.len() + 1 + BAR + 1;
    let room = usize::from(width).saturating_sub(1 + NAME + 1 + numbers);
    let place = PLACE.min(room);
    let filled = match all {
        0 => 0,
        all => (row.usage.bytes * BAR as u64).div_ceil(all) as usize,
    };
    Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("{:<NAME$} ", fit(&row.name, NAME)), name),
        Span::styled(
            format!("{:<place$}", fit(&row.place, place)),
            Style::new().fg(theme.branch),
        ),
        Span::styled(format!(" {processes:>13} "), Style::new().fg(theme.muted)),
        Span::styled(size, Style::new().fg(theme.text)),
        Span::raw(" "),
        Span::styled("█".repeat(filled.min(BAR)), Style::new().fg(theme.accent)),
        Span::styled(
            "░".repeat(BAR - filled.min(BAR)),
            Style::new().fg(theme.rule),
        ),
    ])
}

/// What crystal takes itself: the daemon, and this TUI.
fn own_line<'a>(resources: &Resources, theme: &Theme) -> Line<'a> {
    let mut said = format!(
        " crystal itself {}: the daemon {}",
        resources::size(resources.own()),
        resources::size(resources.daemon.bytes)
    );
    if let Some(client) = resources.client {
        said.push_str(&format!(", this TUI {}", resources::size(client.bytes)));
    }
    Line::styled(said, Style::new().fg(theme.muted))
}

/// `part` of `whole`, in percent, as a person reads it: `4%`, or `<1%`.
fn share(part: u64, whole: u64) -> String {
    match part * 100 / whole.max(1) {
        0 if part > 0 => "<1%".to_string(),
        percent => format!("{percent}%"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::SessionUsage;

    fn taken(sessions: &[(&str, u64)]) -> Resources {
        Resources {
            daemon: Usage {
                pid: 1,
                bytes: 30 << 20,
                processes: 1,
            },
            client: None,
            sessions: sessions
                .iter()
                .enumerate()
                .map(|(at, (name, mb))| SessionUsage {
                    name: name.to_string(),
                    usage: Usage {
                        pid: 10 + at as u32,
                        bytes: mb << 20,
                        processes: 2,
                    },
                })
                .collect(),
            total: 0,
        }
    }

    fn press(view: &mut RamView, code: KeyCode) -> Step {
        view.on_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn the_biggest_session_comes_first_and_enter_goes_to_it() {
        let rows = rows(&taken(&[("small", 10), ("big", 400), ("mid", 90)]), &[]);
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["big", "mid", "small"]);
        let mut view = RamView::new(rows);
        press(&mut view, KeyCode::Char('j'));
        assert_eq!(press(&mut view, KeyCode::Enter), Step::Go("mid".into()));
        assert_eq!(press(&mut view, KeyCode::Esc), Step::Close);
    }

    #[test]
    fn the_bar_stays_on_its_session_as_the_order_changes() {
        let mut view = RamView::new(rows(&taken(&[("a", 300), ("b", 200)]), &[]));
        press(&mut view, KeyCode::Down);
        assert_eq!(view.highlighted().unwrap().name, "b");
        view.refresh(rows(&taken(&[("a", 100), ("b", 500)]), &[]));
        assert_eq!(view.highlighted().unwrap().name, "b");
        view.refresh(rows(&taken(&[("a", 100)]), &[]));
        assert_eq!(view.highlighted().unwrap().name, "a");
    }

    #[test]
    fn a_share_reads_in_whole_percent() {
        assert_eq!(share(4, 100), "4%");
        assert_eq!(share(1, 1000), "<1%");
        assert_eq!(share(0, 1000), "0%");
    }
}
