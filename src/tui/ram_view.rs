//! The resources view, `#` in the sidebar: the memory and CPU each session
//! takes, its program and every process under it, the biggest first by
//! memory or, after `s`, by CPU, with how many processes that is and a bar
//! of its share of all of it; under them what crystal takes itself, the
//! daemon, each program it runs that isn't a session's (its helpers: the
//! distiller's `claude -p`, git, a plugin's hook) and this TUI, then the
//! agent kept warm while there's one (`[sessions] warm_agent`); and in its
//! heading all of it, with its share of the machine's memory and cores. The
//! daemon looks at the processes (see [`crate::resources`]), asked off the
//! event loop every second while the view is open, and every few seconds
//! while it's closed, for the footer's readout. Enter goes to the session
//! the bar is on.
//!
//! It was the RAM view, and `[keys] ram` still names its key. The view is
//! state and keys, kept apart from I/O, and its drawing.

use super::listing;
use super::needs_you;
use super::sidebar::fit;
use super::theme::Theme;
use crate::protocol::SessionInfo;
use crate::resources::{self, Resources, Total, Usage};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear};

/// A row: what a session's processes take and where it runs, or what one of
/// crystal's own takes and what it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: String,
    /// A session's project and branch, or its directory.
    pub place: String,
    pub usage: Usage,
}

/// What the rows go by, the biggest first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Order {
    #[default]
    Memory,
    Cpu,
}

impl Order {
    fn other(self) -> Order {
        match self {
            Order::Memory => Order::Cpu,
            Order::Cpu => Order::Memory,
        }
    }

    /// How much of `total` `usage` is, by this, from 0 to 1.
    fn share(self, usage: &Usage, total: &Total) -> f64 {
        let (part, whole) = match self {
            Order::Memory => (usage.bytes as f64, total.bytes as f64),
            Order::Cpu => (usage.cpu, total.cpu),
        };
        if whole > 0.0 { part / whole } else { 0.0 }
    }
}

/// The sessions' rows, in the daemon's order, from what it found they
/// take, and where each runs, from `sessions`.
pub fn rows(resources: &Resources, sessions: &[SessionInfo]) -> Vec<Row> {
    resources
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
        .collect()
}

/// `rows` the biggest first by `order`, then by name.
fn sort(rows: &mut [Row], order: Order) {
    rows.sort_by(|a, b| {
        let by = match order {
            Order::Memory => b.usage.bytes.cmp(&a.usage.bytes),
            Order::Cpu => {
                (b.usage.cpu.total_cmp(&a.usage.cpu)).then(b.usage.bytes.cmp(&a.usage.bytes))
            }
        };
        by.then(a.name.cmp(&b.name))
    });
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
    order: Order,
}

impl RamView {
    pub fn new(rows: Vec<Row>) -> RamView {
        let mut view = RamView::default();
        view.refresh(rows);
        view
    }

    pub fn highlighted(&self) -> Option<&Row> {
        self.rows.get(self.at)
    }

    /// Takes the rows as they are now, which a new look at the processes
    /// orders anew: the bar stays on the session it was on, or where it
    /// was when that one has gone.
    pub fn refresh(&mut self, mut rows: Vec<Row>) {
        sort(&mut rows, self.order);
        let on = self.highlighted().map(|row| row.name.clone());
        self.rows = rows;
        let found = on.and_then(|on| self.rows.iter().position(|row| row.name == on));
        self.at = found
            .unwrap_or(self.at)
            .min(self.rows.len().saturating_sub(1));
    }

    /// `j`/`k` and the arrows move; `s` orders the rows by the other of
    /// memory and CPU; Enter goes to the row's session; Esc or `q` closes.
    pub fn on_key(&mut self, key: &KeyEvent) -> Step {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Step::Close,
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            KeyCode::Char('s') => {
                self.order = self.order.other();
                self.refresh(self.rows.clone());
            }
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
pub fn hints(view: &RamView) -> &'static [(&'static str, &'static str)] {
    match view.order {
        Order::Memory => &[
            ("enter", "go to it"),
            ("j/k", "move"),
            ("s", "by CPU"),
            ("esc", "close"),
        ],
        Order::Cpu => &[
            ("enter", "go to it"),
            ("j/k", "move"),
            ("s", "by RAM"),
            ("esc", "close"),
        ],
    }
}

/// What the footer shows of it at its right once the daemon has said: all
/// crystal takes, its memory short and its CPU, like `2.1G 35%`.
pub fn readout(resources: &Resources) -> String {
    let all = resources.all();
    match resources.cpu_over_ms {
        0 => resources::short_size(all.bytes),
        _ => format!(
            "{} {}",
            resources::short_size(all.bytes),
            resources::cpu(all.cpu)
        ),
    }
}

/// What crystal takes itself, a row each: the daemon, its helpers, those of
/// one program together, this TUI, and the agent kept warm.
fn own_rows(resources: &Resources) -> Vec<Row> {
    let row = |name: &str, place: &str, usage: Usage| Row {
        name: name.to_string(),
        place: place.to_string(),
        usage,
    };
    let mut rows = vec![row("the daemon", "", resources.daemon)];
    let mut helpers: Vec<(Row, usize)> = Vec::new();
    for helper in &resources.helpers {
        match helpers.iter_mut().find(|(row, _)| row.name == helper.name) {
            Some((row, count)) => {
                row.usage.bytes += helper.usage.bytes;
                row.usage.cpu += helper.usage.cpu;
                row.usage.processes += helper.usage.processes;
                *count += 1;
            }
            None => helpers.push((row(&helper.name, "", helper.usage), 1)),
        }
    }
    for (mut helper, count) in helpers {
        helper.place = match count {
            1 => "run by the daemon".to_string(),
            count => format!("{count} run by the daemon"),
        };
        rows.push(helper);
    }
    if let Some(client) = resources.client {
        rows.push(row("this TUI", "", client));
    }
    if let Some(warm) = resources.warm {
        rows.push(row("the agent kept warm", "for a new session", warm));
    }
    rows
}

/// Draws the view in `area`: a heading with all of it, a row each session
/// under the columns' names, and what crystal takes itself at the bottom.
/// `None` until the daemon has said.
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
        frame.render_widget(heading_line(String::new(), theme), heading);
        return listing::draw_note(frame, theme, "looking at the processes…", rest);
    };
    frame.render_widget(heading_line(said(resources, heading.width), theme), heading);
    let own = own_rows(resources);
    let own_height = u16::try_from(own.len() + 1)
        .unwrap_or(u16::MAX)
        .min(rest.height / 2);
    let [columns, list, rule, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(own_height),
    ])
    .areas(rest);
    frame.render_widget(columns_line(view.order, theme, columns.width), columns);
    let look = RowLook {
        all: resources.all(),
        order: view.order,
        counted: resources.cpu_over_ms > 0,
        theme,
    };
    if view.rows.is_empty() {
        listing::draw_note(frame, theme, "no session's program is running", list);
    }
    let first = listing::first_drawn(view.at, list.height);
    let shown = view.rows.iter().enumerate().skip(first);
    for (index, row) in shown.take(usize::from(list.height)) {
        let highlighted = index == view.at;
        let area = listing::row_area(frame, theme, list, index - first, highlighted);
        frame.render_widget(row_line(row, &look, highlighted, area.width), area);
    }
    listing::draw_rule(frame, theme, rule);
    let lines = (own
        .iter()
        .map(|row| row_line(row, &look, false, bottom.width)))
    .chain([own_line(resources, theme)]);
    for (line, y) in lines.zip(bottom.rows()) {
        frame.render_widget(line, y);
    }
}

fn heading_line<'a>(said: String, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            " Resources",
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(said, Style::new().fg(theme.muted)),
    ])
}

/// What the heading says after its name in `width`: all crystal takes and
/// its CPU in percent of a core, each with its share of the machine's
/// memory or cores where there's room.
fn said(resources: &Resources, width: u16) -> String {
    let all = resources.all();
    let machine = resources.machine;
    let (mut memory, mut cpu) = (resources::size(all.bytes), String::new());
    let (mut of_memory, mut of_cores) = (String::new(), String::new());
    if machine.bytes > 0 {
        let of = share(all.bytes, machine.bytes);
        of_memory = format!(", {of} of {}", resources::size(machine.bytes));
    }
    if resources.cpu_over_ms > 0 {
        cpu = format!(" · CPU {} of a core", resources::cpu(all.cpu));
        let of = resources::cpu(all.cpu / f64::from(machine.cores.max(1)));
        of_cores = match machine.cores {
            0 => String::new(),
            1 => format!(", {of} of 1 core"),
            cores => format!(", {of} of {cores} cores"),
        };
    }
    let whole = format!(" · RAM {memory}{of_memory}{cpu}{of_cores}");
    if " Resources".len() + whole.chars().count() <= usize::from(width) {
        return whole;
    }
    memory.insert_str(0, " · RAM ");
    cpu.insert_str(0, &memory);
    cpu
}

/// How wide a row's columns are: the name's, the place's up to its share,
/// and the numbers'.
const NAME: usize = 22;
const PLACE: usize = 26;
const PROCESSES: usize = 13;
const RAM: usize = 7;
const CPU: usize = 6;
const BAR: usize = 10;

/// How much room the place gets in `width`: what the name, its spaces and
/// the numbers leave, up to its share.
fn place_room(width: u16) -> usize {
    let numbers = 1 + PROCESSES + 1 + RAM + 1 + CPU + 1 + BAR;
    let room = usize::from(width).saturating_sub(1 + NAME + 1 + numbers);
    PLACE.min(room)
}

/// The columns' names over the rows, the one they go by standing out with
/// an arrow.
fn columns_line<'a>(order: Order, theme: &Theme, width: u16) -> Line<'a> {
    let place = place_room(width);
    let muted = Style::new().fg(theme.muted);
    let column = |word: &str, width: usize, by: Order| match by == order {
        true => Span::styled(
            format!(" {:>width$}", format!("{word}↓")),
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        false => Span::styled(format!(" {word:>width$}"), muted),
    };
    Line::from(vec![
        Span::styled(format!(" {:<NAME$} {:<place$}", "session", "where"), muted),
        Span::styled(format!(" {:>PROCESSES$}", "processes"), muted),
        column("RAM", RAM, Order::Memory),
        column("CPU", CPU, Order::Cpu),
    ])
}

/// What every row is drawn against: all of it, for the bar, what the rows
/// go by, and whether CPU was counted.
struct RowLook<'a> {
    all: Total,
    order: Order,
    counted: bool,
    theme: &'a Theme,
}

/// A row: its name, where it runs, its processes, the memory and CPU they
/// take, and a bar of its share of all of it by what the rows go by.
fn row_line<'a>(row: &Row, look: &RowLook, highlighted: bool, width: u16) -> Line<'a> {
    let theme = look.theme;
    let mut name = Style::new().fg(theme.text);
    if highlighted {
        name = name.add_modifier(Modifier::BOLD);
    }
    let processes = match row.usage.processes {
        1 => "1 process".to_string(),
        count => format!("{count} processes"),
    };
    let place = place_room(width);
    let cpu = match look.counted {
        true => resources::cpu(row.usage.cpu),
        false => "-".to_string(),
    };
    let share = look.order.share(&row.usage, &look.all);
    let filled = ((share * BAR as f64).ceil() as usize).min(BAR);
    Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("{:<NAME$} ", fit(&row.name, NAME)), name),
        Span::styled(
            format!("{:<place$}", fit(&row.place, place)),
            Style::new().fg(theme.branch),
        ),
        Span::styled(
            format!(" {processes:>PROCESSES$} "),
            Style::new().fg(theme.muted),
        ),
        Span::styled(
            format!("{:>RAM$} {cpu:>CPU$}", resources::size(row.usage.bytes)),
            Style::new().fg(theme.text),
        ),
        Span::raw(" "),
        Span::styled("█".repeat(filled), Style::new().fg(theme.accent)),
        Span::styled("░".repeat(BAR - filled), Style::new().fg(theme.rule)),
    ])
}

/// What crystal takes itself, the rows above but the agent kept warm.
fn own_line<'a>(resources: &Resources, theme: &Theme) -> Line<'a> {
    let own = resources.own();
    let mut said = format!(" crystal itself {}", resources::size(own.bytes));
    if resources.cpu_over_ms > 0 {
        said += &format!(", {} of a core", resources::cpu(own.cpu));
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
    use crate::resources::{Helper, Machine, SessionUsage};

    fn usage(pid: u32, mb: u64, cpu: f64) -> Usage {
        Usage {
            pid,
            bytes: mb << 20,
            cpu,
            processes: 2,
        }
    }

    /// Each session's name, its megabytes and its CPU.
    fn taken(sessions: &[(&str, u64, f64)]) -> Resources {
        Resources {
            daemon: usage(1, 30, 1.0),
            sessions: sessions
                .iter()
                .enumerate()
                .map(|(at, &(name, mb, cpu))| SessionUsage {
                    name: name.to_string(),
                    usage: usage(10 + at as u32, mb, cpu),
                })
                .collect(),
            cpu_over_ms: 1000,
            ..Resources::default()
        }
    }

    fn press(view: &mut RamView, code: KeyCode) -> Step {
        view.on_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn names(view: &RamView) -> Vec<&str> {
        view.rows.iter().map(|row| row.name.as_str()).collect()
    }

    /// The view drawn `width` wide, a row a line.
    fn drawn(view: &RamView, resources: &Resources, width: u16, height: u16) -> Vec<String> {
        let theme = super::super::theme::Theme::new(crate::config::ThemeName::DARK, false);
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw(frame, view, Some(resources), &theme, frame.area()))
            .unwrap();
        let cells = terminal.backend().buffer().content();
        let rows = cells.chunks(usize::from(width));
        rows.map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    #[test]
    fn the_biggest_session_comes_first_and_enter_goes_to_it() {
        let rows = rows(
            &taken(&[("small", 10, 0.0), ("big", 400, 0.0), ("mid", 90, 0.0)]),
            &[],
        );
        let mut view = RamView::new(rows);
        assert_eq!(names(&view), ["big", "mid", "small"]);
        press(&mut view, KeyCode::Char('j'));
        assert_eq!(press(&mut view, KeyCode::Enter), Step::Go("mid".into()));
        assert_eq!(press(&mut view, KeyCode::Esc), Step::Close);
    }

    #[test]
    fn s_orders_the_rows_by_cpu_and_back_the_bar_staying_on_its_session() {
        let resources = taken(&[("idle", 900, 0.2), ("busy", 100, 87.5), ("some", 300, 4.0)]);
        let mut view = RamView::new(rows(&resources, &[]));
        assert_eq!(names(&view), ["idle", "some", "busy"]);
        press(&mut view, KeyCode::Char('j'));
        press(&mut view, KeyCode::Char('s'));
        assert_eq!(view.order, Order::Cpu);
        assert_eq!(names(&view), ["busy", "some", "idle"]);
        assert_eq!(view.highlighted().unwrap().name, "some");
        assert_eq!(hints(&view)[2], ("s", "by RAM"));
        // A new look keeps the order.
        view.refresh(rows(
            &taken(&[("idle", 900, 50.0), ("busy", 100, 1.0)]),
            &[],
        ));
        assert_eq!(names(&view), ["idle", "busy"]);
        press(&mut view, KeyCode::Char('s'));
        assert_eq!(view.order, Order::Memory);
    }

    #[test]
    fn the_bar_stays_on_its_session_as_the_order_changes() {
        let mut view = RamView::new(rows(&taken(&[("a", 300, 0.0), ("b", 200, 0.0)]), &[]));
        press(&mut view, KeyCode::Down);
        assert_eq!(view.highlighted().unwrap().name, "b");
        view.refresh(rows(&taken(&[("a", 100, 0.0), ("b", 500, 0.0)]), &[]));
        assert_eq!(view.highlighted().unwrap().name, "b");
        view.refresh(rows(&taken(&[("a", 100, 0.0)]), &[]));
        assert_eq!(view.highlighted().unwrap().name, "a");
    }

    #[test]
    fn crystal_s_own_rows_are_the_daemon_its_helpers_by_program_the_tui_and_the_warm_agent() {
        let mut resources = taken(&[("a", 100, 0.0)]);
        let helper = |name: &str, pid, mb, cpu| Helper {
            name: name.to_string(),
            usage: usage(pid, mb, cpu),
        };
        resources.helpers = vec![
            helper("git", 20, 10, 2.0),
            helper("claude", 21, 300, 40.0),
            helper("git", 22, 12, 3.0),
        ];
        resources.client = Some(usage(30, 40, 2.5));
        resources.warm = Some(usage(99, 250, 0.0));
        let own: Vec<(String, String, u64, f64)> = (own_rows(&resources).into_iter())
            .map(|row| (row.name, row.place, row.usage.bytes >> 20, row.usage.cpu))
            .collect();
        let row = |name: &str, place: &str, mb, cpu| (name.into(), place.into(), mb, cpu);
        assert_eq!(
            own,
            [
                row("the daemon", "", 30, 1.0),
                row("git", "2 run by the daemon", 22, 5.0),
                row("claude", "run by the daemon", 300, 40.0),
                row("this TUI", "", 40, 2.5),
                row("the agent kept warm", "for a new session", 250, 0.0),
            ]
        );
        // Crystal itself leaves the agent kept warm out; all of it doesn't.
        assert_eq!(resources.own().bytes, (30 + 322 + 40) << 20);
        assert_eq!(resources.own().cpu, 48.5);
        assert_eq!(resources.all().bytes, (392 + 250 + 100) << 20);
    }

    #[test]
    fn the_view_shows_memory_and_cpu_and_the_totals_in_its_heading() {
        let mut resources = taken(&[("agent", 600, 35.0), ("shell", 4, 0.0)]);
        resources.helpers = vec![Helper {
            name: "git".to_string(),
            usage: usage(5, 10, 3.2),
        }];
        resources.machine = Machine {
            bytes: 16 << 30,
            cores: 10,
        };
        let view = RamView::new(rows(&resources, &[]));
        let lines = drawn(&view, &resources, 100, 12);
        let heading = &lines[0];
        assert!(
            heading.starts_with(" Resources · RAM 644 MB, 3% of 16.0 GB"),
            "{heading}"
        );
        assert!(
            heading
                .trim_end()
                .ends_with("CPU 39% of a core, 3.9% of 10 cores"),
            "{heading}"
        );
        assert!(
            lines[1].contains("session") && lines[1].contains("RAM↓    CPU"),
            "{}",
            lines[1]
        );
        let agent = &lines[2];
        assert!(
            agent.starts_with(" agent ") && agent.contains("600 MB    35% ███"),
            "{agent}"
        );
        assert!(lines[3].contains("4 MB     0%"), "{}", lines[3]);
        let at = |text: &str| lines.iter().position(|line| line.contains(text));
        let daemon = at("the daemon").unwrap();
        assert!(lines[daemon].contains("30 MB   1.0%"), "{}", lines[daemon]);
        assert!(
            lines[daemon + 1].contains("git") && lines[daemon + 1].contains("run by the daemon")
        );
        assert!(lines[daemon + 2].contains("crystal itself 40 MB, 4.2% of a core"));
        // Narrower, the heading leaves the machine out.
        let narrow = drawn(&view, &resources, 50, 12);
        let heading = " Resources · RAM 644 MB · CPU 39% of a core";
        assert_eq!(narrow[0].trim_end(), heading);
        // Before CPU could be counted, it says so.
        resources.cpu_over_ms = 0;
        let lines = drawn(&view, &resources, 100, 12);
        assert!(!lines[0].contains("CPU"), "{}", lines[0]);
        assert!(lines[2].contains("600 MB      - "), "{}", lines[2]);
    }

    #[test]
    fn the_footer_reads_memory_short_and_cpu() {
        let mut resources = taken(&[("a", 2000, 33.0)]);
        assert_eq!(readout(&resources), "2.0G 34%");
        resources.cpu_over_ms = 0;
        assert_eq!(readout(&resources), "2.0G");
    }

    #[test]
    fn a_share_reads_in_whole_percent() {
        assert_eq!(share(4, 100), "4%");
        assert_eq!(share(1, 1000), "<1%");
        assert_eq!(share(0, 1000), "0%");
    }
}
