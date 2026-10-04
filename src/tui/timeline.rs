//! The timeline, `a` in the sidebar: crystal's event log read back, the
//! newest first and live while it's open, one line an event: when, what,
//! about what and how it went, as `crystal events` says it. Typing filters
//! the lines, Tab narrows them to a kind of event, and Enter goes to the
//! session a line is about, if it's still there. The line the bar is on is
//! read whole under the list. Events that came while the user was away are
//! marked, when it's opened after "while you were away" said so.
//!
//! The state is plain data, kept apart from I/O: the event loop reads the
//! log a page at a time and follows it as it grows, handing the events in
//! through [`TimelineView::read`] and [`TimelineView::logged`], and asks
//! [`TimelineView::wants_older`] when to read further back. The list, its
//! filter and its bar are a [`Listing`]. Events of kinds it has never heard
//! of are listed like the rest, by their name and what they say.

use super::listing::{self, Item, Listing};
use super::sidebar::fit;
use super::text_input::TextInput;
use super::theme::Theme;
use crate::events::{Event, Kind};
use crate::events_cli;
use crate::project;
use crate::protocol::TaskState;
use crate::shell;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use std::path::Path;
use std::rc::Rc;

/// How many events one read of the log takes.
pub const PAGE: usize = 500;

/// How many lines the timeline fills itself up to, reading further back on
/// its own, while the kind or the filter leaves fewer.
const FILL: usize = 100;

/// The most events the timeline reads on its own to fill itself. Past
/// that, it reads further back only as the bar reaches the end.
const MOST_ON_ITS_OWN: usize = 5000;

/// How wide the list has to be for each line to say where it happened, on
/// the right, in at most [`PLACE`] columns.
const WIDE: u16 = 110;
const PLACE: usize = 30;

/// The kinds of event the timeline can be narrowed to, by the family in
/// their names, what comes before the dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kinds {
    All,
    Sessions,
    Tasks,
    Flows,
    Memory,
    /// Every family the others don't take, new ones included.
    Other,
}

impl Kinds {
    const ALL: [Kinds; 6] = [
        Kinds::All,
        Kinds::Sessions,
        Kinds::Tasks,
        Kinds::Flows,
        Kinds::Memory,
        Kinds::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kinds::All => "all",
            Kinds::Sessions => "sessions",
            Kinds::Tasks => "tasks",
            Kinds::Flows => "flows",
            Kinds::Memory => "memory",
            Kinds::Other => "other",
        }
    }

    fn families(self) -> &'static [&'static str] {
        match self {
            Kinds::Sessions => &["session", "worktree"],
            Kinds::Tasks => &["task", "run", "handoff", "backlog"],
            Kinds::Flows => &["flow"],
            Kinds::Memory => &["memory"],
            Kinds::All | Kinds::Other => &[],
        }
    }

    fn takes(self, event: &Event) -> bool {
        let family = event.kind.name().split('.').next().unwrap_or_default();
        match self {
            Kinds::All => true,
            Kinds::Other => !Kinds::ALL
                .iter()
                .any(|kinds| kinds.families().contains(&family)),
            _ => self.families().contains(&family),
        }
    }

    /// The next, or with `by` -1 the one before, round the end.
    fn step(self, by: isize) -> Kinds {
        let count = Kinds::ALL.len() as isize;
        Kinds::ALL[(self as isize + by).rem_euclid(count) as usize]
    }
}

impl Item for Rc<Event> {
    fn number(&self) -> u64 {
        self.seq
    }

    /// What it is, about what, where, and what it says.
    fn searched(&self) -> String {
        let place = place(self).unwrap_or_default();
        format!(
            "{} {} {place} {}",
            self.kind.name(),
            self.subject(),
            self.text()
        )
    }
}

/// What a key in the view asks for beyond it.
#[derive(Debug, PartialEq)]
pub enum Step {
    Stay,
    Close,
    /// Go to what this event is about.
    Go(Rc<Event>),
}

pub struct TimelineView {
    /// Every event read so far, the newest first.
    events: Vec<Rc<Event>>,
    /// The lines shown: the events of `kinds`, filtered as typed. No items
    /// while the first page is being read.
    pub list: Listing<Rc<Event>, ()>,
    pub kinds: Kinds,
    /// The last event the user had seen before they were away: those after
    /// it are marked new.
    pub away_after: Option<u64>,
    /// Whether the log has been read back to its start.
    complete: bool,
    /// Whether a page further back is being read.
    reading: bool,
}

impl TimelineView {
    /// The timeline before anything is read, marking what came after
    /// `away_after` as new.
    pub fn new(away_after: Option<u64>) -> TimelineView {
        TimelineView {
            events: Vec::new(),
            list: Listing::new(None),
            kinds: Kinds::All,
            away_after,
            complete: false,
            reading: true,
        }
    }

    /// Takes a page of the log, the newest first: its end, or what came
    /// before the oldest event read so far. A page that couldn't be read
    /// ends the reading back.
    pub fn read(&mut self, page: Result<Vec<Event>, String>) {
        self.reading = false;
        let page = match page {
            Ok(page) => page,
            Err(reason) => {
                self.complete = true;
                if self.events.is_empty() {
                    self.list.set_items(Err(reason));
                }
                return;
            }
        };
        self.complete = page.len() < PAGE;
        let oldest = self.events.last().map(|event| event.seq);
        let older = page
            .into_iter()
            .filter(|event| oldest.is_none_or(|oldest| event.seq < oldest));
        self.events.extend(older.map(Rc::new));
        self.show();
    }

    /// Takes an event that has just happened, at the top; the bar stays on
    /// the line it was on.
    pub fn logged(&mut self, event: Event) {
        if self
            .events
            .first()
            .is_some_and(|newest| newest.seq >= event.seq)
        {
            return;
        }
        self.events.insert(0, Rc::new(event));
        self.show();
    }

    /// Where to read the log back from, when it's time to read further:
    /// the bar has just been moved onto the last line, or too few lines
    /// are left to fill the view, while it hasn't read too many on its own.
    /// `asked` says a key has just been pressed. Once asked, it counts as
    /// reading until [`TimelineView::read`] has the page.
    pub fn wants_older(&mut self, asked: bool) -> Option<u64> {
        if self.complete || self.reading {
            return None;
        }
        let oldest = self.events.last()?.seq;
        let shown = self.list.shown().len();
        let at_end = self.list.highlighted_at().is_none_or(|at| at + 1 >= shown);
        let fill = shown < FILL && self.events.len() < MOST_ON_ITS_OWN;
        if !fill && !(asked && at_end) {
            return None;
        }
        self.reading = true;
        Some(oldest)
    }

    /// How many of the events read came while the user was away.
    pub fn new_count(&self) -> usize {
        self.away_after.map_or(0, |after| {
            self.events.iter().filter(|event| event.seq > after).count()
        })
    }

    fn is_new(&self, event: &Event) -> bool {
        self.away_after.is_some_and(|after| event.seq > after)
    }

    /// Lists the events of the kinds chosen, the filter going over them.
    fn show(&mut self) {
        let kinds = self.kinds;
        let shown = self.events.iter().filter(|event| kinds.takes(event));
        self.list.set_items(Ok(shown.cloned().collect()));
    }

    /// Esc clears the filter, and closes the view once it's clear; Enter
    /// goes to what the line is about; Tab and Shift+Tab go through the
    /// kinds; the list takes the rest: the arrows move, PgUp and PgDn
    /// scroll the line read whole, and the other keys type into the filter.
    pub fn on_key(&mut self, key: &KeyEvent) -> Step {
        match key.code {
            KeyCode::Esc if !self.list.filter.text().is_empty() => {
                self.list.filter = TextInput::default();
                self.show();
            }
            KeyCode::Esc => return Step::Close,
            KeyCode::Enter => {
                if let Some(event) = self.list.highlighted() {
                    return Step::Go(event.clone());
                }
            }
            KeyCode::Tab => self.choose(self.kinds.step(1)),
            KeyCode::BackTab => self.choose(self.kinds.step(-1)),
            _ => self.list.on_key(key),
        }
        Step::Stay
    }

    pub fn on_paste(&mut self, text: &str) {
        self.list.on_paste(text);
    }

    fn choose(&mut self, kinds: Kinds) {
        self.kinds = kinds;
        self.show();
    }
}

/// Where an event happened: its project, and the branch of the session
/// it's about.
fn place(event: &Event) -> Option<String> {
    let project = project::name_of(event.project.as_ref()?);
    let branch = event.session.as_ref().and_then(|s| s.branch.as_deref());
    Some(match branch {
        Some(branch) => format!("{project} ⎇ {branch}"),
        None => project,
    })
}

/// The color that says how it went: waiting on the user, working, done or
/// failed, and muted for the rest.
fn tone(event: &Event, theme: &Theme) -> Color {
    let by_word = |word: &str| match word {
        "waiting" => theme.waiting,
        "done" | "approved" => theme.done,
        "failed" | "interrupted" => theme.failed,
        _ => theme.muted,
    };
    match event.kind {
        Kind::SessionWaiting | Kind::TaskWaiting | Kind::RunAsking | Kind::FlowGate => {
            theme.waiting
        }
        Kind::SessionWorking | Kind::TaskStarted | Kind::RunStarted | Kind::FlowStepStarted => {
            theme.working
        }
        Kind::SessionDone => theme.done,
        Kind::TaskClosed => match event.task.as_ref().map(|task| task.state()) {
            Some(TaskState::Done) => theme.done,
            Some(TaskState::Failed) => theme.failed,
            _ => theme.muted,
        },
        Kind::RunEnded => match event.run.as_ref().and_then(|run| run.failed) {
            Some(true) => theme.failed,
            _ => theme.done,
        },
        Kind::FlowStepEnded | Kind::FlowEnded => event
            .flow
            .as_ref()
            .map_or(theme.muted, |flow| by_word(&flow.state)),
        Kind::SessionEnded => match event.session.as_ref().map(|s| s.status.as_str()) {
            Some("exited 0") => theme.muted,
            _ => theme.failed,
        },
        Kind::PluginPaused | Kind::SessionStartFailed | Kind::WorktreeHookFailed => theme.failed,
        Kind::DaemonRestarted => match event.daemon.as_ref() {
            Some(daemon) if !daemon.failed.is_empty() => theme.failed,
            _ => theme.muted,
        },
        _ => theme.muted,
    }
}

/// The keys the footer offers.
pub const HINTS: &[(&str, &str)] = &[
    ("enter", "go to it"),
    ("tab", "kinds"),
    ("↑/↓", "move"),
    ("pgup/pgdn", "scroll"),
    ("esc", "clear, close"),
];

/// Draws the view in `area`: a heading with the kinds, the filter, the
/// lines, and the one the bar is on read whole. `now` is milliseconds since
/// the Unix epoch.
pub fn draw(frame: &mut Frame, view: &TimelineView, theme: &Theme, now: u64, area: Rect) {
    let [heading, filter, rest] = listing::frame_areas(frame, theme, area);
    draw_heading(frame, view, theme, heading);
    listing::draw_filter(frame, theme, &view.list.filter, true, filter);
    let events = match view.list.items() {
        None => return listing::draw_note(frame, theme, "reading the log…", rest),
        Some(Err(reason)) => return listing::draw_note(frame, theme, reason, rest),
        Some(Ok(_)) => view.list.shown(),
    };
    if events.is_empty() {
        let nothing = if view.list.filter.text().is_empty() && view.kinds == Kinds::All {
            "nothing has happened yet"
        } else {
            "nothing matches"
        };
        return listing::draw_note(frame, theme, nothing, rest);
    }
    let [list, rule, reading] = listing::list_areas(rest, events.len());
    let at = view.list.highlighted_at().unwrap_or(0);
    let first = listing::first_drawn(at, list.height);
    let shown = events.iter().enumerate().skip(first);
    for (index, event) in shown.take(usize::from(list.height)) {
        let highlighted = index == at;
        let row = listing::row_area(frame, theme, list, index - first, highlighted);
        let new = view.is_new(event);
        // Where it happened goes on the right, when there's room for it.
        let place = place(event)
            .filter(|_| row.width >= WIDE)
            .map(|place| format!("{} ", fit(&place, PLACE)));
        let right = place.as_ref().map_or(0, |place| place.chars().count() + 1);
        let width = row.width.saturating_sub(right as u16);
        frame.render_widget(event_line(event, new, theme, now, highlighted, width), row);
        if let Some(place) = place {
            let place = Line::styled(place, Style::new().fg(theme.muted));
            frame.render_widget(place.right_aligned(), row);
        }
    }
    listing::draw_rule(frame, theme, rule);
    if let Some(event) = view.list.highlighted() {
        let lines = reading_lines(event, theme, now);
        listing::draw_reading(frame, lines, view.list.scroll, reading);
    }
}

/// "timeline", the kinds with the one chosen standing out, and on the
/// right how many came while the user was away, which are marked.
fn draw_heading(frame: &mut Frame, view: &TimelineView, theme: &Theme, area: Rect) {
    let mut spans = vec![Span::styled(
        " timeline ",
        Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
    )];
    for kinds in Kinds::ALL {
        let style = if kinds == view.kinds {
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.muted)
        };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(kinds.name(), style));
    }
    frame.render_widget(Line::from(spans), area);
    let new = view.new_count();
    if new > 0 {
        let new = Line::styled(format!("{new} new "), Style::new().fg(theme.accent));
        frame.render_widget(new.right_aligned(), area);
    }
}

/// A line of the list: a dot for what's new, when it happened, what
/// happened in its color, what it's about, and what it says.
fn event_line<'a>(
    event: &Event,
    new: bool,
    theme: &Theme,
    now: u64,
    highlighted: bool,
    width: u16,
) -> Line<'a> {
    const WHEN: usize = 11;
    const KIND: usize = 18;
    const SUBJECT: usize = 16;
    let mut subject = Style::new().fg(theme.text);
    if highlighted {
        subject = subject.add_modifier(Modifier::BOLD);
    }
    let mark = if new { " • " } else { "   " };
    let room = usize::from(width).saturating_sub(3 + WHEN + 1 + KIND + 1 + SUBJECT + 2);
    Line::from(vec![
        Span::styled(mark, Style::new().fg(theme.accent)),
        Span::styled(
            format!("{:<WHEN$} ", events_cli::when(event.at, now)),
            Style::new().fg(theme.muted),
        ),
        Span::styled(
            format!("{:<KIND$} ", fit(event.kind.name(), KIND)),
            Style::new().fg(tone(event, theme)),
        ),
        Span::styled(
            format!("{:<SUBJECT$}  ", fit(&event.subject(), SUBJECT)),
            subject,
        ),
        Span::styled(fit(&event.text(), room), Style::new().fg(theme.text)),
    ])
}

/// The event the bar is on, whole: when and what, about what and where,
/// what it says, and everything it carries, as the log keeps it.
fn reading_lines<'a>(event: &Event, theme: &Theme, now: u64) -> Vec<Line<'a>> {
    // The most a field's name is padded to, so the values line up.
    const KEY: usize = 24;
    let muted = Style::new().fg(theme.muted);
    let when = events_cli::when(event.at, now);
    let mut lines = vec![
        Line::styled(
            format!(" {when} · {} · #{}", event.kind.name(), event.seq),
            muted,
        ),
        Line::from(vec![
            Span::styled(
                format!(" {}", event.subject()),
                Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", event.text()), Style::new().fg(theme.text)),
        ]),
    ];
    if let Some(place) = place(event) {
        lines.push(Line::styled(
            format!(" {place}"),
            Style::new().fg(theme.branch),
        ));
    }
    let carried = carried(event);
    let width = carried.iter().map(|(key, _)| key.chars().count()).max();
    let width = width.unwrap_or(0).min(KEY);
    if !carried.is_empty() {
        lines.push(Line::default());
    }
    for (key, value) in carried {
        lines.push(Line::from(vec![
            Span::styled(format!(" {key:<width$}  "), muted),
            Span::styled(value, Style::new().fg(theme.text)),
        ]));
    }
    lines
}

/// What the event carries beyond its number, time and name, as the log
/// keeps it: a field a line, one inside another by its path, like
/// `session.branch`, and a list of words as the words.
fn carried(event: &Event) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    if let Ok(Value::Object(json)) = serde_json::to_value(event) {
        for (key, value) in json {
            if !["seq", "at", "event"].contains(&key.as_str()) {
                flatten(key, &value, &mut fields);
            }
        }
    }
    fields
}

fn flatten(path: String, value: &Value, fields: &mut Vec<(String, String)>) {
    match value {
        Value::Null => {}
        Value::Object(object) => {
            for (key, value) in object {
                flatten(format!("{path}.{key}"), value, fields);
            }
        }
        Value::Array(items) if items.iter().all(|item| item.is_string()) => {
            let words: Vec<String> = items.iter().map(scalar).collect();
            fields.push((path, words.join(" ")));
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                flatten(format!("{path}.{index}"), item, fields);
            }
        }
        _ => fields.push((path, scalar(value))),
    }
}

/// A value on one line: a path with `~` for the home directory, any other
/// string as it is, and anything else as JSON writes it.
fn scalar(value: &Value) -> String {
    match value {
        Value::String(path) if path.starts_with('/') => shell::home_relative(Path::new(path)),
        Value::String(text) => text.replace('\n', " "),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{PluginAbout, SessionAbout};
    use crossterm::event::KeyModifiers;
    use std::path::PathBuf;

    fn event(seq: u64, kind: Kind) -> Event {
        Event {
            seq,
            at: seq * 1000,
            ..Event::about_project(kind, PathBuf::from("/code/app"))
        }
    }

    fn seqs(view: &TimelineView) -> Vec<u64> {
        view.list.shown().iter().map(|event| event.seq).collect()
    }

    fn press(view: &mut TimelineView, code: KeyCode) -> Step {
        view.on_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_text(view: &mut TimelineView, text: &str) {
        for c in text.chars() {
            press(view, KeyCode::Char(c));
        }
    }

    fn read(kinds: &[(u64, Kind)]) -> TimelineView {
        let mut view = TimelineView::new(None);
        let page = kinds.iter().map(|(seq, kind)| event(*seq, *kind)).collect();
        view.read(Ok(page));
        view
    }

    #[test]
    fn new_events_come_in_on_top_and_the_bar_stays_on_its_line() {
        let mut view = read(&[(2, Kind::BacklogClosed), (1, Kind::BacklogAdded)]);
        assert_eq!(seqs(&view), [2, 1]);
        assert_eq!(view.list.highlighted().unwrap().seq, 2);
        view.logged(event(3, Kind::MemoryAdded));
        // One the page had already is never listed twice.
        view.logged(event(2, Kind::BacklogClosed));
        assert_eq!(seqs(&view), [3, 2, 1]);
        assert_eq!(view.list.highlighted().unwrap().seq, 2);
    }

    #[test]
    fn tab_narrows_to_a_kind_and_the_rest_takes_kinds_nobody_names() {
        let mut view = read(&[
            (4, Kind::PluginPaused),
            (3, Kind::MemoryAdded),
            (2, Kind::RunAsking),
            (1, Kind::WorktreeCreated),
        ]);
        press(&mut view, KeyCode::Tab);
        assert_eq!((view.kinds, seqs(&view)), (Kinds::Sessions, vec![1]));
        press(&mut view, KeyCode::Tab);
        assert_eq!((view.kinds, seqs(&view)), (Kinds::Tasks, vec![2]));
        press(&mut view, KeyCode::BackTab);
        press(&mut view, KeyCode::BackTab);
        press(&mut view, KeyCode::BackTab);
        assert_eq!((view.kinds, seqs(&view)), (Kinds::Other, vec![4]));
        press(&mut view, KeyCode::Tab);
        assert_eq!(view.kinds, Kinds::All);
    }

    #[test]
    fn typing_filters_and_esc_clears_the_filter_before_it_closes() {
        let mut view = read(&[(2, Kind::MemoryAdded), (1, Kind::BacklogAdded)]);
        type_text(&mut view, "backlog");
        assert_eq!(seqs(&view), [1]);
        assert_eq!(press(&mut view, KeyCode::Esc), Step::Stay);
        assert_eq!(seqs(&view), [2, 1]);
        assert_eq!(press(&mut view, KeyCode::Esc), Step::Close);
    }

    #[test]
    fn enter_goes_to_what_the_line_is_about() {
        let mut view = read(&[(2, Kind::MemoryAdded), (1, Kind::BacklogAdded)]);
        press(&mut view, KeyCode::Down);
        let Step::Go(event) = press(&mut view, KeyCode::Enter) else {
            panic!("enter goes to it");
        };
        assert_eq!(event.seq, 1);
    }

    #[test]
    fn it_reads_further_back_to_fill_itself_then_as_the_bar_reaches_the_end() {
        let mut view = TimelineView::new(None);
        // Nothing more is read while the first page is.
        assert_eq!(view.wants_older(true), None);
        let full: Vec<Event> = (1..=PAGE as u64)
            .rev()
            .map(|seq| event(seq + 1000, Kind::SessionWorking))
            .collect();
        view.read(Ok(full.clone()));
        assert_eq!(view.wants_older(false), None, "a page fills the view");
        // Narrowed to a kind there are few of, it reads on to fill it, a
        // page at a time.
        press(&mut view, KeyCode::Tab);
        press(&mut view, KeyCode::Tab);
        assert_eq!(view.wants_older(true), Some(1001));
        assert_eq!(view.wants_older(true), None, "one page at a time");
        view.read(Ok(vec![event(1000, Kind::TaskClosed)]));
        assert_eq!(seqs(&view), [1000]);
        // A short page was the start of the log.
        assert_eq!(view.wants_older(true), None);
    }

    #[test]
    fn a_page_that_cant_be_read_says_why_when_theres_nothing_else() {
        let mut view = TimelineView::new(None);
        view.read(Err("no database".into()));
        assert_eq!(view.list.items(), Some(&Err("no database".to_string())));
        // The log followed from there still fills it.
        view.logged(event(7, Kind::BacklogAdded));
        assert_eq!(seqs(&view), [7]);
        assert_eq!(view.wants_older(true), None);
    }

    #[test]
    fn what_came_while_the_user_was_away_is_new() {
        let mut view = TimelineView::new(Some(5));
        view.read(Ok(vec![
            event(7, Kind::MemoryAdded),
            event(5, Kind::MemoryAdded),
        ]));
        view.logged(event(8, Kind::MemoryAdded));
        assert_eq!(view.new_count(), 2);
        assert!(view.is_new(&event(6, Kind::MemoryAdded)));
        assert!(!view.is_new(&event(5, Kind::MemoryAdded)));
    }

    #[test]
    fn a_line_is_found_by_where_it_happened_and_what_it_says() {
        let paused = Event {
            plugin: Some(PluginAbout {
                name: "notes".into(),
                why: "it kept failing".into(),
            }),
            ..event(1, Kind::PluginPaused)
        };
        let searched = Rc::new(paused).searched();
        for part in ["plugin.paused", "notes", "app", "it kept failing"] {
            assert!(searched.contains(part), "{part}: {searched}");
        }
        let started = Event {
            session: Some(SessionAbout {
                name: "fixer".into(),
                id: "s1".into(),
                command: vec!["claude".into(), "fix it".into()],
                cwd: "/code/app".into(),
                project: None,
                worktree: None,
                branch: Some("main".into()),
                activity: None,
                task: None,
                status: "running".into(),
                reporter: None,
            }),
            ..event(1, Kind::SessionStarted)
        };
        let carried = carried(&started);
        let field = |key: &str| {
            let found = carried.iter().find(|(name, _)| name == key);
            found.map(|(_, value)| value.as_str())
        };
        assert_eq!(field("project"), Some("/code/app"));
        assert_eq!(field("session.branch"), Some("main"));
        assert_eq!(field("session.command"), Some("claude fix it"));
        assert_eq!(field("session.task"), None, "what isn't there isn't said");
        assert_eq!(field("seq"), None);
    }
}
