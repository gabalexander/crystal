//! Draws a session's screen into a ratatui area: each cell of the screen
//! becomes a ratatui cell with the same character, colors and attributes,
//! with what copy mode marks on it laid over them, and the link under the
//! mouse underlined.

use crate::vt::{self, CellColor, CellStyle, Link, Mark};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

pub struct ScreenWidget<'a> {
    screen: &'a vt::Screen,
    /// What a program's "default" colors stand for: the terminal's own,
    /// unless the theme paints its own.
    default_fg: Color,
    default_bg: Color,
    marks: Marks,
    /// The link the mouse is over, with Ctrl held.
    link: Option<Link>,
}

/// How each of copy mode's marks looks, laid over the cell's own style.
#[derive(Debug, Clone, Copy)]
pub struct Marks {
    pub selected: Style,
    pub found: Style,
    pub current: Style,
    pub cursor: Style,
}

impl Default for Marks {
    /// Marks any terminal can show, whatever its colors.
    fn default() -> Marks {
        Marks {
            selected: Style::new().add_modifier(Modifier::REVERSED),
            found: Style::new().add_modifier(Modifier::UNDERLINED),
            current: Style::new().add_modifier(Modifier::UNDERLINED | Modifier::BOLD),
            cursor: Style::new().add_modifier(Modifier::REVERSED),
        }
    }
}

impl<'a> ScreenWidget<'a> {
    pub fn new(screen: &'a vt::Screen) -> ScreenWidget<'a> {
        ScreenWidget {
            screen,
            default_fg: Color::Reset,
            default_bg: Color::Reset,
            marks: Marks::default(),
            link: None,
        }
    }

    /// Draws the program's default colors as `fg` and `bg`, so that a
    /// session sits on the theme's background rather than the terminal's.
    pub fn with_defaults(self, fg: Color, bg: Color) -> ScreenWidget<'a> {
        ScreenWidget {
            default_fg: fg,
            default_bg: bg,
            ..self
        }
    }

    /// Draws copy mode's marks as `marks` has them.
    pub fn with_marks(self, marks: Marks) -> ScreenWidget<'a> {
        ScreenWidget { marks, ..self }
    }

    /// Underlines `link`, to say a Ctrl+click opens it.
    pub fn with_link(self, link: Option<Link>) -> ScreenWidget<'a> {
        ScreenWidget { link, ..self }
    }

    /// What `mark` lays over a cell's style.
    fn mark(&self, mark: Mark) -> Style {
        match mark {
            Mark::None => Style::new(),
            Mark::Found => self.marks.found,
            Mark::Current => self.marks.current,
            Mark::Selected => self.marks.selected,
            Mark::Cursor => self.marks.cursor,
        }
    }

    fn style(&self, cell: &CellStyle) -> Style {
        let mut fg = cell.fg_color.map_or(self.default_fg, color);
        let bg = cell.bg_color.map_or(self.default_bg, color);
        if cell.invisible {
            fg = bg;
        }
        let mut style = Style::default().fg(fg).bg(bg);
        let attributes = [
            (cell.bold, Modifier::BOLD),
            (cell.faint, Modifier::DIM),
            (cell.italic, Modifier::ITALIC),
            (cell.underlined, Modifier::UNDERLINED),
            (cell.inverse, Modifier::REVERSED),
            (cell.strikethrough, Modifier::CROSSED_OUT),
        ];
        for (on, modifier) in attributes {
            if on {
                style = style.add_modifier(modifier);
            }
        }
        style
    }
}

impl Widget for ScreenWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.screen.each_cell(|row, col, cell| {
            // A wide character in the last column would spill out.
            let width = if cell.wide { 2 } else { 1 };
            if row >= area.height || col + width > area.width {
                return;
            }
            // The right half of a wide character is left out: ratatui
            // leaves it alone once the left half holds the character.
            let Some(target) = buf.cell_mut((area.x + col, area.y + row)) else {
                return;
            };
            target.set_symbol(cell.text);
            let mut style = self.style(&cell.style).patch(self.mark(cell.mark));
            if self
                .link
                .as_ref()
                .is_some_and(|link| link.covers((row, col)))
            {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            target.set_style(style);
        });
    }
}

/// The color a cell asks for.
fn color(color: CellColor) -> Color {
    match color {
        CellColor::Palette(index) => Color::Indexed(index),
        CellColor::Rgb(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(rows: u16, cols: u16, output: &[u8]) -> vt::Screen {
        let mut screen = vt::Screen::new(rows, cols);
        screen.process(output);
        screen
    }

    fn render(screen: &vt::Screen, area: Rect) -> Buffer {
        let mut buf = Buffer::empty(area);
        ScreenWidget::new(screen).render(area, &mut buf);
        buf
    }

    #[test]
    fn text_lands_where_the_program_put_it() {
        let parser = screen(3, 10, b"hi\r\n  there");
        let buf = render(&parser, Rect::new(0, 0, 10, 3));
        assert_eq!(buf[(0, 0)].symbol(), "h");
        assert_eq!(buf[(1, 0)].symbol(), "i");
        assert_eq!(buf[(2, 1)].symbol(), "t");
    }

    #[test]
    fn the_screen_is_drawn_from_the_areas_corner() {
        let parser = screen(2, 4, b"ab");
        let buf = render(&parser, Rect::new(5, 3, 4, 2));
        assert_eq!(buf[(5, 3)].symbol(), "a");
        assert_eq!(buf[(6, 3)].symbol(), "b");
    }

    #[test]
    fn colors_and_attributes_come_through() {
        // Red on 256-color 21, bold; then a truecolor foreground, inverse.
        let parser = screen(1, 4, b"\x1b[1;31;48;5;21mR\x1b[0;7;38;2;1;2;3mT");
        let buf = render(&parser, Rect::new(0, 0, 4, 1));

        let red = buf[(0, 0)].style();
        assert_eq!(red.fg, Some(Color::Indexed(1)));
        assert_eq!(red.bg, Some(Color::Indexed(21)));
        assert!(red.add_modifier.contains(Modifier::BOLD));

        let truecolor = buf[(1, 0)].style();
        assert_eq!(truecolor.fg, Some(Color::Rgb(1, 2, 3)));
        assert!(truecolor.add_modifier.contains(Modifier::REVERSED));
        assert!(!truecolor.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn default_colors_take_the_themes_and_others_stay_the_programs() {
        let parser = screen(1, 4, b"a\x1b[31mb");
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::empty(area);
        let ink = Color::Rgb(1, 1, 1);
        let paper = Color::Rgb(9, 9, 9);
        ScreenWidget::new(&parser)
            .with_defaults(ink, paper)
            .render(area, &mut buf);

        assert_eq!(buf[(0, 0)].style().fg, Some(ink));
        assert_eq!(buf[(0, 0)].style().bg, Some(paper));
        assert_eq!(buf[(1, 0)].style().fg, Some(Color::Indexed(1)));
        assert_eq!(buf[(1, 0)].style().bg, Some(paper));
    }

    #[test]
    fn a_wide_character_takes_two_cells() {
        let parser = screen(1, 6, "中x".as_bytes());
        let buf = render(&parser, Rect::new(0, 0, 6, 1));
        assert_eq!(buf[(0, 0)].symbol(), "中");
        assert_eq!(buf[(2, 0)].symbol(), "x");
    }

    #[test]
    fn copy_modes_marks_are_laid_over_the_cells() {
        let mut parser = screen(2, 10, b"find me\r\n");
        parser.start_copying();
        parser.search("me", false);
        let selected = Style::new().bg(Color::Rgb(1, 2, 3));
        let marks = Marks {
            selected,
            ..Marks::default()
        };
        let area = Rect::new(0, 0, 10, 2);
        let mut buf = Buffer::empty(area);
        ScreenWidget::new(&parser)
            .with_marks(marks)
            .render(area, &mut buf);
        // The search put the cursor on "me": its first cell is the cursor's,
        // the next the match's.
        assert!(buf[(5, 0)].modifier.contains(Modifier::REVERSED));
        assert!(
            buf[(6, 0)]
                .modifier
                .contains(Modifier::UNDERLINED | Modifier::BOLD)
        );
        assert!(buf[(0, 0)].modifier.is_empty());

        parser.toggle_selection(vt::SelectionKind::Lines);
        let mut buf = Buffer::empty(area);
        ScreenWidget::new(&parser)
            .with_marks(marks)
            .render(area, &mut buf);
        assert_eq!(buf[(0, 0)].bg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn the_link_under_the_mouse_is_underlined() {
        let parser = screen(1, 30, b"at https://example.com now");
        let area = Rect::new(0, 0, 30, 1);
        let mut buf = Buffer::empty(area);
        ScreenWidget::new(&parser)
            .with_link(parser.link_at((0, 8)))
            .render(area, &mut buf);
        let underlined = |col: u16| buf[(col, 0)].modifier.contains(Modifier::UNDERLINED);
        assert!(!underlined(2));
        assert!(underlined(3) && underlined(21));
        assert!(!underlined(22));
    }

    #[test]
    fn a_screen_bigger_than_the_area_is_cut_off() {
        let parser = screen(2, 6, b"abcdef\r\nghijkl");
        let buf = render(&parser, Rect::new(0, 0, 3, 1));
        assert_eq!(buf[(2, 0)].symbol(), "c");
    }
}
