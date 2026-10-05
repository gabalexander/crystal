//! The memory view, `m` in the sidebar: what the selected session's
//! project has remembered, newest first, with the entry the bar is on in
//! full beside the list. `/` filters the list as you type; Enter opens the
//! entry's file in the user's editor; `x` forgets the entry and `p` writes
//! it into the project's CLAUDE.md or AGENTS.md, each once the user says
//! `y`.
//!
//! The state here is plain data: the entries arrive through
//! [`MemoryView::read_done`], and what the user asks for goes out as an
//! [`Action`] for the event loop to carry out.

use super::app::{Action, Hit, Loading, Outcome};
use super::sidebar::{ago, fit};
use super::text_input::TextInput;
use super::ui::{self, Look, ViewAreas};
use crate::memory::{self, Freshness, Listed};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use std::path::{Path, PathBuf};

/// The rows the filter takes at the top of the list while it's shown: the
/// filter, and a rule under it.
const FILTER_ROWS: u16 = 2;

/// How wide the kind column is: the longest kind, `decision`, and a space.
const KIND_WIDTH: usize = 9;

pub struct MemoryView {
    /// The directory the memory is for: the project's main worktree, or a
    /// session's directory outside git.
    pub dir: PathBuf,
    /// What the header calls it.
    pub place: String,
    entries: Loading<Vec<Listed>>,
    filter: TextInput,
    /// Whether the filter has the keyboard. What it holds keeps filtering
    /// once Enter hands the keyboard back to the list.
    filtering: bool,
    /// The entry the bar is on, by id. It stays on it while the list
    /// changes, as long as the entry is still shown.
    selected: Option<u64>,
    /// A yes-or-no question about an entry, until it's answered.
    asking: Option<Ask>,
    /// How many rows the list has, which is what a page is.
    rows: u16,
}

/// What the user is asked before something is done to an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    Forget(u64),
    Promote(u64),
}

impl MemoryView {
    /// The view for the memory of `dir`, waiting for its entries.
    pub fn new(dir: PathBuf, place: String) -> MemoryView {
        MemoryView {
            dir,
            place,
            entries: Loading::Reading,
            filter: TextInput::default(),
            filtering: false,
            selected: None,
            asking: None,
            rows: 1,
        }
    }

    /// Asks for the entries to be read.
    pub fn read(&self) -> Action {
        Action::ReadMemory(self.dir.clone())
    }

    /// Takes the entries read for `dir`, if they're still the ones wanted.
    pub fn read_done(&mut self, dir: &Path, read: Result<Vec<Listed>, String>) {
        if dir != self.dir {
            return;
        }
        self.entries = match read {
            Ok(entries) => Loading::Read(entries),
            Err(why) => Loading::Failed(why),
        };
        self.keep_selection_shown();
    }

    pub fn set_size(&mut self, list: (u16, u16)) {
        self.rows = list.0;
    }

    /// The entries that match the filter, newest first. Every word typed
    /// has to turn up in the entry's kind, text, files or source.
    pub fn shown(&self) -> Vec<&Listed> {
        let Loading::Read(entries) = &self.entries else {
            return Vec::new();
        };
        let words: Vec<String> = self
            .filter
            .text()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        entries
            .iter()
            .filter(|item| {
                let entry = &item.entry;
                let haystack = format!(
                    "{} {} {} {}",
                    entry.kind,
                    entry.text,
                    entry.files.join(" "),
                    entry.source
                )
                .to_lowercase();
                words.iter().all(|word| haystack.contains(word))
            })
            .collect()
    }

    pub fn selected(&self) -> Option<&Listed> {
        let id = self.selected?;
        self.shown().into_iter().find(|item| item.entry.id == id)
    }

    /// The question being asked, for the footer, if there is one.
    pub fn question(&self) -> Option<String> {
        match self.asking? {
            Ask::Forget(id) => Some(format!("forget entry {id}?")),
            Ask::Promote(id) => Some(format!("add entry {id} to the project's CLAUDE.md?")),
        }
    }

    /// Whether a key typed is a character, not a move: while the filter
    /// has the keyboard, or a question waits.
    pub fn typing(&self) -> bool {
        self.filtering || self.asking.is_some()
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        // Only `y` says yes; any other key says no, and does nothing else.
        if let Some(ask) = self.asking.take() {
            if key.code == KeyCode::Char('y') {
                return Outcome::Do(self.action_for(ask));
            }
            return Outcome::Stay;
        }
        if self.filtering {
            return self.on_filter_key(key);
        }
        let page = self.rows.max(1) as isize;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Char('/') => self.filtering = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::PageDown => self.move_by(page),
            KeyCode::PageUp => self.move_by(-page),
            KeyCode::Char('x') => self.asking = self.selected.map(Ask::Forget),
            KeyCode::Char('p') => self.asking = self.selected.map(Ask::Promote),
            KeyCode::Enter => {
                if let Some((_, path)) = self.file_to_open() {
                    return Outcome::Edit { path, line: None };
                }
            }
            _ => {}
        }
        Outcome::Stay
    }

    /// The file Enter opens of the entry the bar is on, and the worktree
    /// it's opened in: the one the entry's files were said in, while it's
    /// there, or else the project's; the first of its files that's there,
    /// or else its first. `None` for an entry about no file.
    pub fn file_to_open(&self) -> Option<(PathBuf, String)> {
        let entry = &self.selected()?.entry;
        let checkout = entry
            .checkout
            .clone()
            .filter(|checkout| checkout.is_dir())
            .unwrap_or_else(|| self.dir.clone());
        let file = entry
            .files
            .iter()
            .find(|file| checkout.join(file).is_file())
            .or(entry.files.first())?;
        Some((checkout, file.clone()))
    }

    /// Pasted text goes into the filter, which opens for it.
    pub fn on_paste(&mut self, text: &str) -> Outcome {
        self.filtering = true;
        self.filter.insert_str(text);
        self.keep_selection_shown();
        Outcome::Stay
    }

    /// A click on an entry puts the bar on it; the wheel moves the bar.
    pub fn on_mouse(&mut self, kind: MouseEventKind, hit: Hit) -> Outcome {
        match (kind, hit) {
            (MouseEventKind::Down(MouseButton::Left), Hit::ViewList(index)) => {
                if let Some(item) = self.shown().get(index) {
                    self.selected = Some(item.entry.id);
                }
            }
            (MouseEventKind::ScrollUp, Hit::ViewList(_)) => self.move_by(-1),
            (MouseEventKind::ScrollDown, Hit::ViewList(_)) => self.move_by(1),
            _ => {}
        }
        Outcome::Stay
    }

    /// Keys while the filter has the keyboard: Enter keeps what's typed and
    /// goes back to the list, Esc empties it, the arrows still move the
    /// bar, and the rest edit it.
    fn on_filter_key(&mut self, key: KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => self.filtering = false,
            KeyCode::Esc => {
                self.filtering = false;
                self.filter = TextInput::default();
            }
            KeyCode::Down => self.move_by(1),
            KeyCode::Up => self.move_by(-1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            _ => self.filter.on_key(&key),
        }
        self.keep_selection_shown();
        Outcome::Stay
    }

    fn action_for(&self, ask: Ask) -> Action {
        let dir = self.dir.clone();
        match ask {
            Ask::Forget(id) => Action::ForgetMemory { dir, id },
            Ask::Promote(id) => Action::PromoteMemory { dir, id },
        }
    }

    fn move_by(&mut self, by: isize) {
        let shown = self.shown();
        let Some(at) = shown
            .iter()
            .position(|item| Some(item.entry.id) == self.selected)
        else {
            return;
        };
        let to = at.saturating_add_signed(by).min(shown.len() - 1);
        self.selected = Some(shown[to].entry.id);
    }

    /// Puts the bar on the first entry shown when the one it was on isn't
    /// shown any more.
    fn keep_selection_shown(&mut self) {
        let shown = self.shown();
        let still_shown = shown
            .iter()
            .any(|item| Some(item.entry.id) == self.selected);
        if !still_shown {
            self.selected = shown.first().map(|item| item.entry.id);
        }
    }

    /// The rows the filter takes at the top of the list: none until
    /// something's typed or it has the keyboard.
    fn filter_rows(&self) -> u16 {
        if self.filtering || !self.filter.text().is_empty() {
            FILTER_ROWS
        } else {
            0
        }
    }
}

/// How wide the list is, for a view `width` columns wide.
pub fn list_width(width: u16) -> u16 {
    (width / 2).clamp(30, 72).min(width / 2)
}

/// The keys the footer shows, or the question being asked.
pub fn hints(view: &MemoryView) -> Vec<(String, String)> {
    if let Some(question) = view.question() {
        return vec![(question, String::new()), ("y".into(), "yes".into())];
    }
    if view.filtering {
        return vec![
            ("enter".into(), "keep the filter".into()),
            ("esc".into(), "clear it".into()),
        ];
    }
    let mut hints = vec![("j/k", "move"), ("/", "filter")];
    if view.file_to_open().is_some() {
        hints.push(("enter", "edit its file"));
    }
    hints.extend([("x", "forget"), ("p", "add to CLAUDE.md"), ("esc", "close")]);
    hints
        .into_iter()
        .map(|(key, does)| (key.to_string(), does.to_string()))
        .collect()
}

/// Which entry's row is on screen `row`, in a list drawn in `area`.
pub fn list_hit(view: &MemoryView, area: Rect, row: u16) -> Hit {
    let Some(row) = (row - area.y).checked_sub(view.filter_rows()) else {
        return Hit::Elsewhere;
    };
    let shown = view.shown();
    let height = area.height.saturating_sub(view.filter_rows());
    let index = list_offset(view, &shown, height) + usize::from(row);
    if index < shown.len() {
        Hit::ViewList(index)
    } else {
        Hit::Elsewhere
    }
}

/// The first entry on screen: the list scrolls to keep the bar in sight.
fn list_offset(view: &MemoryView, shown: &[&Listed], height: u16) -> usize {
    let at = shown
        .iter()
        .position(|item| Some(item.entry.id) == view.selected)
        .unwrap_or(0);
    (at + 1).saturating_sub(usize::from(height.max(1)))
}

pub fn draw(frame: &mut Frame, view: &MemoryView, look: &Look, areas: &ViewAreas) {
    frame.render_widget(header(view, look, areas.header.width), areas.header);
    ui::draw_rule(frame, look, areas.rule);
    let filter_rows = view.filter_rows();
    if filter_rows > 0 {
        draw_filter(frame, view, look, areas.list);
    }
    let list = Rect::new(
        areas.list.x,
        areas.list.y + filter_rows,
        areas.list.width,
        areas.list.height.saturating_sub(filter_rows),
    );
    match &view.entries {
        Loading::Reading => ui::draw_message(frame, look, "reading…", list),
        Loading::Failed(why) => {
            let line = Line::styled(format!(" {why}"), Style::new().fg(look.theme.failed));
            frame.render_widget(Paragraph::new(line), list);
        }
        Loading::Read(entries) if entries.is_empty() => ui::draw_message(
            frame,
            look,
            "nothing remembered yet: crystal remember \"…\"",
            list,
        ),
        Loading::Read(_) => draw_list(frame, view, look, list),
    }
    draw_entry(frame, view, look, areas.content);
}

/// "✎ memory · 12 entries", and where on the right.
fn header<'a>(view: &MemoryView, look: &Look, width: u16) -> Line<'a> {
    let mut notes = Vec::new();
    if let Loading::Read(entries) = &view.entries {
        notes.push(match entries.len() {
            1 => "1 entry".to_string(),
            count => format!("{count} entries"),
        });
    }
    ui::view_header("✎", "memory", &notes, &view.place, look, width)
}

/// The filter, with the cursor in it while it has the keyboard, and a rule
/// under it.
fn draw_filter(frame: &mut Frame, view: &MemoryView, look: &Look, list: Rect) {
    let theme = look.theme;
    let prompt = " / ";
    let line = Line::from(vec![
        Span::styled(
            prompt,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(view.filter.text().to_string(), Style::new().fg(theme.text)),
    ]);
    frame.render_widget(line, Rect::new(list.x, list.y, list.width, 1));
    let rule = "─".repeat(usize::from(list.width));
    frame.render_widget(
        Line::styled(rule, Style::new().fg(theme.rule)),
        Rect::new(list.x, list.y + 1, list.width, 1),
    );
    if view.filtering {
        // The prompt is three columns wide.
        let column = list.x + 3 + view.filter.cursor() as u16;
        frame.set_cursor_position((column.min(list.right().saturating_sub(1)), list.y));
    }
}

/// One row an entry: its kind, its text on one line, and how long ago it
/// was added on the right. A drifting entry says so; a stale or an expired
/// one is muted too.
fn draw_list(frame: &mut Frame, view: &MemoryView, look: &Look, area: Rect) {
    let theme = look.theme;
    let shown = view.shown();
    if shown.is_empty() {
        return ui::draw_message(frame, look, "nothing matches", area);
    }
    let now = super::seconds_since_epoch();
    let first = list_offset(view, &shown, area.height);
    for (row, item) in shown
        .iter()
        .skip(first)
        .take(area.height.into())
        .enumerate()
    {
        let line_area = Rect::new(area.x, area.y + row as u16, area.width, 1);
        let entry = &item.entry;
        if Some(entry.id) == view.selected {
            frame.buffer_mut().set_style(line_area, theme.selection);
        }
        let age = ago(entry.created, now);
        let expired = entry.expired(now);
        let marks = item.freshness.mark().into_iter();
        let marks = marks.chain(expired.then_some("expired"));
        let mark: String = marks.map(|mark| format!(" {mark}")).collect();
        let room = usize::from(area.width)
            .saturating_sub(1 + KIND_WIDTH + age.chars().count() + mark.len() + 2);
        let text_color = match item.freshness {
            Freshness::Stale => theme.muted,
            _ if expired => theme.muted,
            _ => theme.text,
        };
        let line = Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("{:<KIND_WIDTH$}", entry.kind.to_string()),
                Style::new().fg(theme.muted),
            ),
            Span::styled(
                fit(&memory::title(&entry.text), room),
                Style::new().fg(text_color),
            ),
        ]);
        frame.render_widget(line, line_area);
        let right = Line::from(vec![
            Span::styled(mark, Style::new().fg(theme.waiting)),
            Span::styled(format!(" {age} "), Style::new().fg(theme.muted)),
        ]);
        frame.render_widget(right.right_aligned(), line_area);
    }
}

/// The entry the bar is on, in full: its kind, when and from whom, the
/// files it's about, whether it's drifting, stale or expired, and its text.
fn draw_entry(frame: &mut Frame, view: &MemoryView, look: &Look, area: Rect) {
    let theme = look.theme;
    let Some(item) = view.selected() else {
        return;
    };
    let entry = &item.entry;
    let now = super::seconds_since_epoch();
    let added = match ago(entry.created, now).as_str() {
        "now" => "just now".to_string(),
        age => format!("{age} ago"),
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!(" {} {}", entry.kind, entry.id),
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · {added} · from {}", entry.source),
            Style::new().fg(theme.muted),
        ),
    ])];
    if !entry.files.is_empty() {
        lines.push(Line::styled(
            format!(" about {}", entry.files.join(", ")),
            Style::new().fg(theme.branch),
        ));
    }
    let holds = match item.freshness {
        Freshness::Fresh => None,
        Freshness::Drifting => Some(
            " drifting: some of the files it's about have changed since, so it may hold only in \
             part",
        ),
        Freshness::Stale => {
            Some(" stale: the files it's about have changed since, so it may no longer hold")
        }
    };
    if let Some(holds) = holds {
        lines.push(Line::styled(holds, Style::new().fg(theme.waiting)));
    }
    if entry.expired(now) {
        lines.push(Line::styled(
            " expired: nobody has found it again, so searches and agents starting leave it out",
            Style::new().fg(theme.waiting),
        ));
    }
    lines.push(Line::raw(""));
    for text_line in entry.text.lines() {
        lines.push(Line::styled(
            format!(" {text_line}"),
            Style::new().fg(theme.text),
        ));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Entry, Kind, Source};

    fn item(id: u64, kind: Kind, text: &str) -> Listed {
        Listed {
            entry: Entry {
                id,
                kind,
                text: text.into(),
                files: Vec::new(),
                source: Source::User,
                created: 1_000,
                seen: 1,
                last_seen: 1_000,
                anchors: Default::default(),
                checkout: None,
                used: None,
            },
            freshness: Freshness::Fresh,
        }
    }

    fn view_of(entries: Vec<Listed>) -> MemoryView {
        let mut view = MemoryView::new(PathBuf::from("/code/app"), "app".into());
        view.read_done(Path::new("/code/app"), Ok(entries));
        view
    }

    fn press(view: &mut MemoryView, code: KeyCode) -> Outcome {
        view.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_text(view: &mut MemoryView, text: &str) {
        for c in text.chars() {
            press(view, KeyCode::Char(c));
        }
    }

    fn shown_ids(view: &MemoryView) -> Vec<u64> {
        view.shown().iter().map(|item| item.entry.id).collect()
    }

    #[test]
    fn the_bar_starts_on_the_first_entry_and_moves_within_the_list() {
        let mut view = view_of(vec![item(3, Kind::Note, "c"), item(2, Kind::Note, "b")]);
        assert_eq!(view.selected().map(|i| i.entry.id), Some(3));
        press(&mut view, KeyCode::Char('j'));
        press(&mut view, KeyCode::Char('j'));
        assert_eq!(view.selected().map(|i| i.entry.id), Some(2));
    }

    #[test]
    fn slash_filters_by_what_is_typed_and_enter_keeps_it() {
        let mut view = view_of(vec![
            item(2, Kind::Gotcha, "the ledger test needs the db"),
            item(1, Kind::Note, "fees are in cents"),
        ]);
        press(&mut view, KeyCode::Char('/'));
        type_text(&mut view, "ledger");
        assert_eq!(shown_ids(&view), [2]);
        press(&mut view, KeyCode::Enter);
        // Back on the list, letters are keys again, and the filter stays.
        press(&mut view, KeyCode::Char('j'));
        assert_eq!(shown_ids(&view), [2]);

        press(&mut view, KeyCode::Char('/'));
        press(&mut view, KeyCode::Esc);
        assert_eq!(shown_ids(&view), [2, 1], "esc clears the filter");
    }

    #[test]
    fn x_forgets_the_entry_only_after_y() {
        let mut view = view_of(vec![item(4, Kind::Note, "old news")]);
        assert_eq!(press(&mut view, KeyCode::Char('x')), Outcome::Stay);
        assert_eq!(view.question().as_deref(), Some("forget entry 4?"));
        assert_eq!(
            press(&mut view, KeyCode::Char('y')),
            Outcome::Do(Action::ForgetMemory {
                dir: PathBuf::from("/code/app"),
                id: 4
            })
        );

        press(&mut view, KeyCode::Char('x'));
        assert_eq!(press(&mut view, KeyCode::Char('n')), Outcome::Stay);
        assert_eq!(view.question(), None);
    }

    #[test]
    fn p_promotes_the_entry_after_y() {
        let mut view = view_of(vec![item(5, Kind::Gotcha, "run the ledger first")]);
        press(&mut view, KeyCode::Char('p'));
        assert_eq!(
            press(&mut view, KeyCode::Char('y')),
            Outcome::Do(Action::PromoteMemory {
                dir: PathBuf::from("/code/app"),
                id: 5
            })
        );
    }

    #[test]
    fn enter_opens_the_entry_s_file_where_it_was_said_while_that_s_there() {
        let worktree = tempfile::tempdir().unwrap();
        std::fs::write(worktree.path().join("there.rs"), "").unwrap();
        let mut about = item(6, Kind::Gotcha, "the ledger needs redis");
        about.entry.files = vec!["gone.rs".into(), "there.rs".into()];
        about.entry.checkout = Some(worktree.path().to_path_buf());
        let mut view = view_of(vec![about, item(5, Kind::Note, "about nothing")]);
        assert_eq!(
            view.file_to_open(),
            Some((worktree.path().to_path_buf(), "there.rs".into()))
        );
        let edit = Outcome::Edit {
            path: "there.rs".into(),
            line: None,
        };
        assert_eq!(press(&mut view, KeyCode::Enter), edit);
        assert!(hints(&view).iter().any(|(key, _)| key == "enter"));

        // An entry about no file has nothing to open.
        press(&mut view, KeyCode::Down);
        assert_eq!(press(&mut view, KeyCode::Enter), Outcome::Stay);
        assert!(!hints(&view).iter().any(|(key, _)| key == "enter"));

        // With its worktree gone, its first file, in the project.
        let mut moved = item(7, Kind::Note, "x");
        moved.entry.files = vec!["src/a.rs".into()];
        moved.entry.checkout = Some(PathBuf::from("/nowhere/at/all"));
        let view = view_of(vec![moved]);
        assert_eq!(
            view.file_to_open(),
            Some((PathBuf::from("/code/app"), "src/a.rs".into()))
        );
    }

    #[test]
    fn a_list_row_is_an_entry_s_title() {
        let view = view_of(vec![item(
            1,
            Kind::Note,
            "Fees are in cents\n\nNever floats.",
        )]);
        let theme = super::super::theme::Theme::new(crate::config::ThemeName::DARK, false);
        let look = Look {
            theme: &theme,
            now: 2_000,
            spin: 0,
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 4)).unwrap();
        terminal
            .draw(|frame| draw_list(frame, &view, &look, frame.area()))
            .unwrap();
        let row: String = terminal.backend().buffer().content()[..60]
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(row.contains("Fees are in cents"), "{row}");
        assert!(!row.contains("Never"), "{row}");
    }

    #[test]
    fn a_note_nobody_found_again_is_marked_expired_and_a_lesson_never_is() {
        // Both said long ago, and neither found since.
        let view = view_of(vec![
            item(2, Kind::Note, "Fees are in cents"),
            item(1, Kind::Gotcha, "The ledger needs redis"),
        ]);
        let theme = super::super::theme::Theme::new(crate::config::ThemeName::DARK, false);
        let look = Look {
            theme: &theme,
            now: 2_000,
            spin: 0,
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 4)).unwrap();
        terminal
            .draw(|frame| draw_list(frame, &view, &look, frame.area()))
            .unwrap();
        let rows: Vec<String> = terminal.backend().buffer().content()[..120]
            .chunks(60)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect();
        assert!(rows[0].contains("Fees are in cents") && rows[0].contains(" expired"));
        assert!(!rows[1].contains("expired"), "{}", rows[1]);
    }

    #[test]
    fn esc_closes_the_view() {
        let mut view = view_of(vec![]);
        assert_eq!(press(&mut view, KeyCode::Esc), Outcome::Close);
    }

    #[test]
    fn entries_read_for_another_directory_are_left_alone() {
        let mut view = MemoryView::new(PathBuf::from("/code/app"), "app".into());
        view.read_done(Path::new("/code/other"), Ok(vec![item(1, Kind::Note, "x")]));
        assert!(view.shown().is_empty());
    }
}
