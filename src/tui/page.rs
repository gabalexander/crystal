//! A markdown page over everything: the guide, on the second of the `?`
//! overlay's tabs, and what's new in crystal, once, after an update. It
//! scrolls with the arrows, `j` and `k`, and the page keys; any other key
//! closes it. Its text is laid out for the room it has each time it's
//! drawn, so it's state and logic, apart from [`draw`].

use super::theme::Theme;
use crate::markdown::{self, PageLine};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

/// The guide: what to start, the keys that matter most, what agents call
/// and where things live, on one page. `crystal guide` prints it.
pub const GUIDE: &str = include_str!("../../docs/guide.md");

/// Columns kept clear on each side of a page, inside its edge, as the `?`
/// overlay keeps them.
const SIDE: u16 = 2;

/// The widest a page's text gets, so its lines stay easy to read on a wide
/// terminal.
const MOST_WIDTH: u16 = 100;

/// Which of the `?` overlay's tabs is in front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Keys,
    Guide,
}

/// A page and how far down it's read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// What its title says, like `what's new in crystal 0.4.0`.
    title: String,
    /// Its markdown.
    text: String,
    /// How many of its lines are scrolled off its top.
    scroll: usize,
}

impl Page {
    pub fn new(title: &str, text: &str) -> Page {
        Page {
            title: title.to_string(),
            text: text.to_string(),
            scroll: 0,
        }
    }

    /// The guide, from its top.
    pub fn guide() -> Page {
        Page::new("guide", GUIDE)
    }

    #[cfg(test)]
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Its lines, laid out `width` columns wide.
    fn lines(&self, width: u16) -> Vec<PageLine> {
        markdown::render(&self.text, usize::from(width).max(1))
    }

    /// Scrolls it for `key`, on a screen the size of `screen`. False when
    /// the key isn't one that scrolls, which closes it.
    pub fn on_key(&mut self, key: &KeyEvent, screen: Rect) -> bool {
        let room = text_area(screen);
        let shown = usize::from(room.height).max(1);
        let most = self.lines(room.width).len().saturating_sub(shown);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = shown.saturating_sub(1).max(1);
        self.scroll = match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.scroll + 1,
            KeyCode::Char('n') if ctrl => self.scroll + 1,
            KeyCode::Up | KeyCode::Char('k') => self.scroll.saturating_sub(1),
            KeyCode::Char('p') if ctrl => self.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll + page,
            KeyCode::Char('d') if ctrl => self.scroll + page,
            KeyCode::PageUp => self.scroll.saturating_sub(page),
            KeyCode::Char('u') if ctrl => self.scroll.saturating_sub(page),
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => most,
            _ => return false,
        }
        .min(most);
        true
    }
}

/// Where a page goes on a screen the size of `screen`: in the middle, as
/// wide as its text gets and the sides it keeps, and as tall as the screen
/// but a row above and below.
fn page_area(screen: Rect) -> Rect {
    let width = (MOST_WIDTH + 2 * SIDE).min(screen.width);
    let height = screen.height.saturating_sub(2).max(screen.height.min(3));
    Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    )
}

/// Where a page's text goes on a screen the size of `screen`: inside its
/// sides, below its title and above how to close it.
fn text_area(screen: Rect) -> Rect {
    page_area(screen).inner(Margin::new(SIDE, 1))
}

/// The title of the `?` overlay, its tabs, with the one in front marked.
pub fn tabs_title<'a>(front: Tab, theme: &Theme) -> Line<'a> {
    let tab = |name: &'static str, tab: Tab| {
        let style = if tab == front {
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.muted)
        };
        Span::styled(name, style)
    };
    let between = Span::styled(" · ", Style::new().fg(theme.muted));
    Line::from(vec![
        Span::raw(" "),
        tab("keys", Tab::Keys),
        between,
        tab("guide", Tab::Guide),
        Span::raw(" "),
    ])
}

/// Draws `page` over the middle of `area`, a panel of the theme's own or,
/// where the theme paints nothing, a thin frame, as the `?` overlay is.
/// `tab` says it's the overlay's guide, which takes the overlay's title.
pub fn draw(frame: &mut Frame, theme: &Theme, area: Rect, page: &Page, tab: Option<Tab>) {
    let overlay = page_area(area);
    let inside = text_area(area);
    let lines = page.lines(inside.width);
    let shown = usize::from(inside.height);
    let scroll = page.scroll.min(lines.len().saturating_sub(shown));

    frame.render_widget(Clear, overlay);
    let framed = theme.panel == Color::Reset;
    let block = if framed {
        Block::bordered().border_style(Style::new().fg(theme.rule))
    } else {
        Block::new()
    };
    let title = match tab {
        Some(tab) => tabs_title(tab, theme),
        None => Line::styled(
            format!(" {} ", page.title),
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
    };
    let mut keys = Vec::new();
    if lines.len() > shown {
        let read = (scroll + shown).min(lines.len()) * 100 / lines.len().max(1);
        keys.push(format!("{read}%"));
        keys.push("↑ ↓ scroll".to_string());
    }
    if tab.is_some() {
        keys.push("tab: the keys".to_string());
    }
    keys.push("any other key closes".to_string());
    let closing = format!(" {} ", keys.join(" · "));
    let block = block
        .style(Style::new().bg(theme.panel).fg(theme.text))
        .title(title)
        .title_bottom(Line::styled(closing, Style::new().fg(theme.muted)));
    frame.render_widget(block, overlay);
    let text: Vec<Line> = lines
        .iter()
        .skip(scroll)
        .take(shown)
        .map(|line| {
            let pieces = line
                .iter()
                .map(|piece| Span::styled(piece.text.clone(), theme.mark(piece.mark)));
            Line::from_iter(pieces)
        })
        .collect();
    frame.render_widget(Paragraph::new(text), inside);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeName;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// What `page` shows on a screen `width` by `height`, a line a row.
    fn shown(page: &Page, tab: Option<Tab>, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let theme = Theme::new(ThemeName::DARK, false);
        terminal
            .draw(|frame| draw(frame, &theme, frame.area(), page, tab))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn a_page_scrolls_to_its_end_and_no_further_and_another_key_closes_it() {
        let text: String = (1..=40).map(|n| format!("line {n}\n\n")).collect();
        let mut page = Page::new("what's new in crystal 0.4.0", &text);
        let screen = Rect::new(0, 0, 60, 12);
        // Eight lines show: a row above and below, the title and how to
        // close it.
        assert!(page.on_key(&key(KeyCode::Char('j')), screen));
        assert_eq!(page.scroll(), 1);
        assert!(page.on_key(&key(KeyCode::PageDown), screen));
        assert_eq!(page.scroll(), 8);
        assert!(page.on_key(&key(KeyCode::End), screen));
        let most = page.lines(text_area(screen).width).len() - 8;
        assert_eq!(page.scroll(), most);
        assert!(page.on_key(&key(KeyCode::Down), screen));
        assert_eq!(page.scroll(), most, "no further than its end");
        assert!(page.on_key(&key(KeyCode::Char('g')), screen));
        assert_eq!(page.scroll(), 0);
        assert!(!page.on_key(&key(KeyCode::Char('x')), screen));
        assert!(!page.on_key(&key(KeyCode::Esc), screen));
    }

    #[test]
    fn a_page_is_drawn_as_markdown_with_its_title_and_how_to_close_it() {
        let page = Page::new(
            "what's new in crystal 0.4.0",
            "## Added\n\n- **Phones**: one column\n",
        );
        let rows = shown(&page, None, 60, 12);
        assert!(rows[1].contains("what's new in crystal 0.4.0"), "{rows:#?}");
        assert!(rows.iter().any(|row| row.contains("Added")), "{rows:#?}");
        assert!(
            rows.iter().any(|row| row.contains("• Phones: one column")),
            "{rows:#?}"
        );
        assert!(!rows.iter().any(|row| row.contains("**")), "{rows:#?}");
        assert!(rows[10].contains("any other key closes"), "{rows:#?}");
        assert!(!rows[10].contains("scroll"), "it all fits: {rows:#?}");
    }

    #[test]
    fn the_guide_is_the_overlays_second_tab() {
        let rows = shown(&Page::guide(), Some(Tab::Guide), 100, 30);
        assert!(rows[1].contains("keys · guide"), "{rows:#?}");
        assert!(
            rows.iter().any(|row| row.contains("The crystal guide")),
            "{rows:#?}"
        );
        assert!(rows[28].contains("tab: the keys"), "{rows:#?}");
        assert!(rows[28].contains("↑ ↓ scroll"), "{rows:#?}");
    }
}
