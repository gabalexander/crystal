//! The menu a right click opens: on a session, a worktree or a project in
//! the sidebar, on a tab, or on a pane. Each item is a key the sidebar
//! takes, said in words, so choosing one does just what pressing its key
//! would, on what was clicked; the key is shown beside it, to learn.
//!
//! In the menu, `j`/`k` or the arrows move the bar, Enter chooses, an
//! item's own key chooses it straight away, and Esc closes it; the mouse
//! chooses with a click, and a click anywhere else closes it. Its state and
//! keys are here, and its drawing; the App says what's in it.

use super::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear};

/// One thing the menu offers: what it does, and the sidebar key that does
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: &'static str,
    pub key: KeyCode,
    /// It can't be taken back, like a kill: drawn in the failed color.
    pub danger: bool,
}

impl Item {
    pub fn new(label: &'static str, key: char) -> Item {
        Item {
            label,
            key: KeyCode::Char(key),
            danger: false,
        }
    }

    pub fn enter(label: &'static str) -> Item {
        Item {
            label,
            key: KeyCode::Enter,
            danger: false,
        }
    }

    pub fn danger(label: &'static str, key: char) -> Item {
        Item {
            danger: true,
            ..Item::new(label, key)
        }
    }

    /// How its key is written beside it.
    fn key_label(&self) -> String {
        match self.key {
            KeyCode::Enter => "enter".to_string(),
            KeyCode::Char(c) => c.to_string(),
            _ => String::new(),
        }
    }
}

/// What a key or a click in the menu asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Stay,
    Close,
    /// Close, and press this key in the sidebar.
    Press(KeyCode),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    /// Where it was opened: the screen's column and row of the click.
    pub at: (u16, u16),
    pub items: Vec<Item>,
    /// The item the bar is on.
    pub highlighted: usize,
}

impl Menu {
    pub fn new(at: (u16, u16), items: Vec<Item>) -> Menu {
        Menu {
            at,
            items,
            highlighted: 0,
        }
    }

    pub fn on_key(&mut self, key: &KeyEvent) -> Step {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Step::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_by(-1);
                Step::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_by(1);
                Step::Stay
            }
            KeyCode::Char('p') if ctrl => {
                self.move_by(-1);
                Step::Stay
            }
            KeyCode::Char('n') if ctrl => {
                self.move_by(1);
                Step::Stay
            }
            KeyCode::Enter => self.choose(self.highlighted),
            code => match self.items.iter().position(|item| item.key == code) {
                Some(index) => self.choose(index),
                None => Step::Stay,
            },
        }
    }

    /// The bar goes onto the item at `index`, as the mouse moves over it.
    pub fn highlight(&mut self, index: usize) {
        if index < self.items.len() {
            self.highlighted = index;
        }
    }

    pub fn choose(&self, index: usize) -> Step {
        match self.items.get(index) {
            Some(item) => Step::Press(item.key),
            None => Step::Close,
        }
    }

    fn move_by(&mut self, by: isize) {
        let last = self.items.len().saturating_sub(1);
        self.highlighted = self.highlighted.saturating_add_signed(by).min(last);
    }

    /// Where it's drawn on `screen`: at the click, its corner there, moved
    /// left or up as far as it must to fit, a frame around its items.
    pub fn area(&self, screen: Rect) -> Rect {
        let widest = self
            .items
            .iter()
            .map(|item| item.label.chars().count() + item.key_label().chars().count() + 3)
            .max()
            .unwrap_or(0);
        let width = (widest as u16 + 4).min(screen.width);
        let height = (self.items.len() as u16 + 2).min(screen.height);
        let (column, row) = self.at;
        let x = column.min(screen.right().saturating_sub(width));
        // Below the click when it fits, or else above it.
        let y = if row + height <= screen.bottom() {
            row
        } else {
            (row + 1).saturating_sub(height).max(screen.y)
        };
        Rect::new(x, y, width, height)
    }

    /// The item at the screen's `column` and `row`, when they're on one of
    /// its rows, in the menu drawn on `screen`.
    pub fn item_at(&self, screen: Rect, column: u16, row: u16) -> Option<usize> {
        let inner = Block::bordered().inner(self.area(screen));
        if !inner.contains((column, row).into()) {
            return None;
        }
        let index = usize::from(row - inner.y);
        (index < self.items.len()).then_some(index)
    }

    /// Whether the screen's `column` and `row` are on the menu, its frame
    /// included.
    pub fn covers(&self, screen: Rect, column: u16, row: u16) -> bool {
        self.area(screen).contains((column, row).into())
    }
}

/// Draws `menu` over everything on `screen`.
pub fn draw(frame: &mut Frame, menu: &Menu, theme: &Theme, screen: Rect) {
    let area = menu.area(screen);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .style(Style::new().bg(theme.panel).fg(theme.text));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    for (index, item) in menu
        .items
        .iter()
        .enumerate()
        .take(usize::from(inner.height))
    {
        let line_area = Rect::new(inner.x, inner.y + index as u16, inner.width, 1);
        let highlighted = index == menu.highlighted;
        let color = if item.danger {
            theme.failed
        } else {
            theme.text
        };
        let mut label = Style::new().fg(color);
        if highlighted {
            frame.buffer_mut().set_style(line_area, theme.selection);
            label = label.add_modifier(Modifier::BOLD);
        }
        let key = item.key_label();
        let used = 1 + item.label.chars().count() + key.chars().count() + 1;
        let gap = usize::from(inner.width).saturating_sub(used);
        let line = Line::from(vec![
            Span::raw(" "),
            Span::styled(item.label, label),
            Span::raw(" ".repeat(gap)),
            Span::styled(key, Style::new().fg(theme.muted)),
        ]);
        frame.render_widget(line, line_area);
    }
}

/// The keys while a menu is open.
pub const HINTS: &[(&str, &str)] = &[
    ("j/k", "move"),
    ("enter", "choose, or an item's key"),
    ("esc", "close"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn press(menu: &mut Menu, code: KeyCode) -> Step {
        menu.on_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn menu() -> Menu {
        Menu::new(
            (10, 5),
            vec![
                Item::enter("type into it"),
                Item::new("zoom", 'z'),
                Item::danger("kill", 'x'),
            ],
        )
    }

    #[test]
    fn enter_chooses_the_item_the_bar_is_on_and_a_key_its_own_item() {
        let mut menu = menu();
        assert_eq!(
            press(&mut menu, KeyCode::Enter),
            Step::Press(KeyCode::Enter)
        );
        press(&mut menu, KeyCode::Down);
        press(&mut menu, KeyCode::Down);
        press(&mut menu, KeyCode::Down);
        assert_eq!(menu.highlighted, 2, "the bar stops at the last");
        assert_eq!(
            press(&mut menu, KeyCode::Enter),
            Step::Press(KeyCode::Char('x'))
        );
        assert_eq!(
            press(&mut menu, KeyCode::Char('z')),
            Step::Press(KeyCode::Char('z'))
        );
        assert_eq!(press(&mut menu, KeyCode::Char('w')), Step::Stay);
        assert_eq!(press(&mut menu, KeyCode::Esc), Step::Close);
    }

    #[test]
    fn it_opens_at_the_click_and_moves_to_fit_on_the_screen() {
        let screen = Rect::new(0, 0, 80, 24);
        let menu = menu();
        let area = menu.area(screen);
        assert_eq!((area.x, area.y), (10, 5));
        assert_eq!(area.height, 5);
        // The bar on its first item is the row below the frame's top.
        assert_eq!(menu.item_at(screen, 12, 6), Some(0));
        assert_eq!(menu.item_at(screen, 12, 8), Some(2));
        assert_eq!(menu.item_at(screen, 12, 5), None);
        assert!(menu.covers(screen, 12, 5));
        assert!(!menu.covers(screen, 9, 6));

        let corner = Menu::new((79, 23), menu.items.clone());
        let area = corner.area(screen);
        assert_eq!(area.right(), 80);
        assert_eq!(area.bottom(), 24);
    }
}
