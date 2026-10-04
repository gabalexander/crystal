//! What the issues and pull requests views share: the list their forge
//! gave, filtered as you type, with a bar that keeps to its item, by
//! number, while the filter changes; each item read whole once, the first
//! time the bar comes to it; and the reading pane under the list, which
//! scrolls. Then how both are drawn: a heading, the filter, the list, a
//! rule, and the reading pane.
//!
//! The state is plain data: what the forge answered arrives through
//! [`Listing::set_items`] and [`Listing::set_detail`], and the event loop
//! asks [`Listing::detail_to_fetch`] what to read next.

use super::search::letters_in;
use super::text_input::TextInput;
use super::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use std::collections::{HashMap, HashSet};

/// The most of the room below the filter the list takes, in thirds: the
/// rest is the reading pane.
const LIST_THIRDS: u16 = 2;

/// How many lines PgUp and PgDn scroll the reading pane.
const PAGE: u16 = 10;

/// Something a forge lists: an issue, or a pull request.
pub trait Item {
    fn number(&self) -> u64;
    /// The text the filter looks through for what's typed.
    fn searched(&self) -> String;
}

/// A forge's list, the bar on it, and its items read whole: `T` is an item
/// as listed, and `D` one read whole.
pub struct Listing<T, D> {
    /// What the forge answered: `None` while it's still being asked.
    items: Option<Result<Vec<T>, String>>,
    pub filter: TextInput,
    /// The item the bar is on, by number. It stays on it while the filter
    /// changes, as long as the item is still shown.
    highlighted: Option<u64>,
    /// Items read whole, by number, as the forge gave them.
    details: HashMap<u64, Result<D, String>>,
    /// The items that have been asked for whole, answered or not.
    asked: HashSet<u64>,
    /// How many lines the reading pane is scrolled down.
    pub scroll: u16,
}

impl<T: Item, D> Listing<T, D> {
    /// A list still being asked for, or with `known` in it until the
    /// forge's fresh answer comes.
    pub fn new(known: Option<Vec<T>>) -> Listing<T, D> {
        let mut listing = Listing {
            items: None,
            filter: TextInput::default(),
            highlighted: None,
            details: HashMap::new(),
            asked: HashSet::new(),
            scroll: 0,
        };
        if let Some(known) = known {
            listing.set_items(Ok(known));
        }
        listing
    }

    /// Takes what the forge listed, or why it couldn't.
    pub fn set_items(&mut self, found: Result<Vec<T>, String>) {
        self.items = Some(found);
        self.keep_highlight_shown();
    }

    /// What the forge listed: `None` while it's being asked.
    pub fn items(&self) -> Option<&Result<Vec<T>, String>> {
        self.items.as_ref()
    }

    pub fn item_mut(&mut self, number: u64) -> Option<&mut T> {
        let Some(Ok(items)) = &mut self.items else {
            return None;
        };
        items.iter_mut().find(|item| item.number() == number)
    }

    pub fn set_detail(&mut self, number: u64, detail: Result<D, String>) {
        self.details.insert(number, detail);
    }

    /// Item `number` read whole, once it has been.
    pub fn detail(&self, number: u64) -> Option<&Result<D, String>> {
        self.details.get(&number)
    }

    pub fn detail_mut(&mut self, number: u64) -> Option<&mut D> {
        self.details.get_mut(&number)?.as_mut().ok()
    }

    /// Has item `number` read again, the next time the bar is on it: after
    /// a comment, say. What it was read as shows until then.
    pub fn read_again(&mut self, number: u64) {
        self.asked.remove(&number);
    }

    /// The items that match the filter, in the order they're listed.
    pub fn shown(&self) -> Vec<&T> {
        let Some(Ok(items)) = &self.items else {
            return Vec::new();
        };
        let query = self.filter.text();
        items.iter().filter(|item| matches(query, *item)).collect()
    }

    pub fn highlighted(&self) -> Option<&T> {
        let number = self.highlighted?;
        self.shown()
            .into_iter()
            .find(|item| item.number() == number)
    }

    /// Where the bar is among the items shown.
    pub fn highlighted_at(&self) -> Option<usize> {
        let number = self.highlighted?;
        self.shown().iter().position(|item| item.number() == number)
    }

    /// The highlighted item's number, when it hasn't been asked for whole
    /// yet; it counts as asked from then on.
    pub fn detail_to_fetch(&mut self) -> Option<u64> {
        let number = self.highlighted()?.number();
        self.asked.insert(number).then_some(number)
    }

    /// ↑ and ↓ (or Ctrl+P and Ctrl+N) move the bar, PgUp and PgDn scroll
    /// the reading pane, and every other key edits the filter.
    pub fn on_key(&mut self, key: &KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(PAGE),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(PAGE),
            _ => {
                self.filter.on_key(key);
                self.keep_highlight_shown();
            }
        }
    }

    /// Pasted text goes into the filter, as if typed.
    pub fn on_paste(&mut self, text: &str) {
        self.filter.insert_str(text);
        self.keep_highlight_shown();
    }

    fn move_by(&mut self, by: isize) {
        let shown = self.shown();
        let Some(at) = self.highlighted_at() else {
            return;
        };
        let to = at.saturating_add_signed(by).min(shown.len() - 1);
        self.highlight(shown[to].number());
    }

    /// Puts the bar on `number`, the reading pane at the top of it.
    pub fn highlight(&mut self, number: u64) {
        if self.highlighted != Some(number) {
            self.highlighted = Some(number);
            self.scroll = 0;
        }
    }

    /// Puts the bar on the first item shown when the one it was on isn't
    /// shown any more.
    fn keep_highlight_shown(&mut self) {
        if self.highlighted_at().is_none()
            && let Some(first) = self.shown().first().map(|item| item.number())
        {
            self.highlight(first);
        }
    }
}

/// Whether every word of `query` turns up in what's searched of `item`,
/// its letters in order. With no words, everything matches, and nothing is
/// searched.
fn matches(query: &str, item: &impl Item) -> bool {
    let mut words = query.split_whitespace().peekable();
    if words.peek().is_none() {
        return true;
    }
    let text = item.searched();
    words.all(|word| letters_in(word, &text).is_some())
}

/// The parts of a view, top to bottom: its heading, the filter, and the
/// rest, for the list and the reading pane. Clears `area` first, so that
/// nothing drawn before, the sidebar and the panes, shows through.
pub fn frame_areas(frame: &mut Frame, theme: &Theme, area: Rect) -> [Rect; 3] {
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().style(theme.base()), area);
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area)
}

/// The list's place, the rule's under it, and the reading pane's, in
/// `rest`, for `count` items: the list as long as it needs, but no more
/// than its share.
pub fn list_areas(rest: Rect, count: usize) -> [Rect; 3] {
    let list_height = u16::try_from(count)
        .unwrap_or(u16::MAX)
        .min(rest.height * LIST_THIRDS / 3)
        .max(1);
    Layout::vertical([
        Constraint::Length(list_height),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(rest)
}

/// "pull requests · app", and on the right how many are open, once the
/// forge has said.
pub fn draw_heading(
    frame: &mut Frame,
    theme: &Theme,
    title: &str,
    project: &str,
    open: Option<usize>,
    area: Rect,
) {
    let line = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            title.to_string(),
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" · {project}"), Style::new().fg(theme.muted)),
    ]);
    frame.render_widget(line, area);
    if let Some(open) = open {
        let open = Line::styled(format!("{open} open "), Style::new().fg(theme.muted));
        frame.render_widget(open.right_aligned(), area);
    }
}

/// The filter, with the cursor in it while it takes the keys.
pub fn draw_filter(frame: &mut Frame, theme: &Theme, filter: &TextInput, cursor: bool, area: Rect) {
    let label = " filter: ";
    let line = Line::from(vec![
        Span::styled(
            label,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(filter.text().to_string(), Style::new().fg(theme.text)),
    ]);
    frame.render_widget(line, area);
    if cursor {
        let column = area.x + (label.len() + filter.cursor()) as u16;
        frame.set_cursor_position((column.min(area.right().saturating_sub(1)), area.y));
    }
}

pub fn draw_note(frame: &mut Frame, theme: &Theme, note: &str, area: Rect) {
    let line = Line::styled(format!(" {note}"), Style::new().fg(theme.muted));
    frame.render_widget(line, area);
}

pub fn draw_rule(frame: &mut Frame, theme: &Theme, area: Rect) {
    let rule = "─".repeat(usize::from(area.width));
    frame.render_widget(Line::styled(rule, Style::new().fg(theme.rule)), area);
}

/// The first of the items shown that's drawn, in a list `height` rows high
/// with the bar at `at`: the list scrolls to keep the bar in sight.
pub fn first_drawn(at: usize, height: u16) -> usize {
    (at + 1).saturating_sub(usize::from(height.max(1)))
}

/// Row `row` of the list drawn in `area`, with the bar's colors on it when
/// it's the highlighted item's.
pub fn row_area(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    row: usize,
    highlighted: bool,
) -> Rect {
    let row = Rect::new(area.x, area.y + row as u16, area.width, 1);
    if highlighted {
        frame.buffer_mut().set_style(row, theme.selection);
    }
    row
}

/// The reading pane: `lines`, wrapped to its width, `scroll` lines down,
/// though never so far that nothing's left.
pub fn draw_reading(frame: &mut Frame, lines: Vec<Line>, scroll: u16, area: Rect) {
    let last = u16::try_from(lines.len().saturating_sub(1)).unwrap_or(u16::MAX);
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll.min(last), 0));
    frame.render_widget(paragraph, area);
}

/// Text from the forge as lines of the reading pane, each a column in.
pub fn text_lines<'a>(text: &str, style: Style) -> Vec<Line<'a>> {
    text.lines()
        .map(|line| Line::styled(format!(" {line}"), style))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Thing(u64, &'static str);

    impl Item for Thing {
        fn number(&self) -> u64 {
            self.0
        }
        fn searched(&self) -> String {
            format!("#{} {}", self.0, self.1)
        }
    }

    fn listing() -> Listing<Thing, String> {
        Listing::new(Some(vec![Thing(42, "Login loops"), Thing(7, "Dark mode")]))
    }

    fn press(listing: &mut Listing<Thing, String>, code: KeyCode) {
        listing.on_key(&KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn type_text(listing: &mut Listing<Thing, String>, text: &str) {
        for c in text.chars() {
            press(listing, KeyCode::Char(c));
        }
    }

    fn numbers(listing: &Listing<Thing, String>) -> Vec<u64> {
        listing.shown().iter().map(|thing| thing.0).collect()
    }

    #[test]
    fn the_bar_starts_on_the_first_item_and_follows_the_filter() {
        let mut listing = listing();
        assert_eq!(listing.highlighted().map(|thing| thing.0), Some(42));
        type_text(&mut listing, "dark");
        assert_eq!(numbers(&listing), [7]);
        assert_eq!(listing.highlighted().map(|thing| thing.0), Some(7));
        for _ in 0..4 {
            press(&mut listing, KeyCode::Backspace);
        }
        // Still shown, it stays where it was.
        assert_eq!(listing.highlighted().map(|thing| thing.0), Some(7));
        type_text(&mut listing, "#42");
        assert_eq!(numbers(&listing), [42]);
    }

    #[test]
    fn the_arrows_move_the_bar_and_stop_at_the_ends() {
        let mut listing = listing();
        press(&mut listing, KeyCode::Down);
        press(&mut listing, KeyCode::Down);
        assert_eq!(listing.highlighted().map(|thing| thing.0), Some(7));
        listing.on_key(&KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(listing.highlighted().map(|thing| thing.0), Some(42));
    }

    #[test]
    fn each_item_is_read_once_until_it_is_to_be_read_again() {
        let mut listing = listing();
        assert_eq!(listing.detail_to_fetch(), Some(42));
        assert_eq!(listing.detail_to_fetch(), None);
        press(&mut listing, KeyCode::Down);
        assert_eq!(listing.detail_to_fetch(), Some(7));
        listing.set_detail(7, Ok("dark".to_string()));
        listing.read_again(7);
        assert_eq!(listing.detail_to_fetch(), Some(7));
        // What it was read as shows until it's read again.
        assert_eq!(listing.detail(7), Some(&Ok("dark".to_string())));
    }

    #[test]
    fn the_reading_pane_scrolls_and_starts_over_on_another_item() {
        let mut listing = listing();
        press(&mut listing, KeyCode::PageDown);
        assert_eq!(listing.scroll, PAGE);
        press(&mut listing, KeyCode::Down);
        assert_eq!(listing.scroll, 0);
    }
}
