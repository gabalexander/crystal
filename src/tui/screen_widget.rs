//! Draws a session's screen, as vt100 keeps it, into a ratatui area: each
//! vt100 cell becomes a ratatui cell with the same character, colors and
//! attributes.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

pub struct ScreenWidget<'a> {
    screen: &'a vt100::Screen,
}

impl<'a> ScreenWidget<'a> {
    pub fn new(screen: &'a vt100::Screen) -> ScreenWidget<'a> {
        ScreenWidget { screen }
    }
}

impl Widget for ScreenWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (rows, cols) = self.screen.size();
        for row in 0..rows.min(area.height) {
            for col in 0..cols.min(area.width) {
                let Some(cell) = self.screen.cell(row, col) else {
                    continue;
                };
                // The right half of a wide character: ratatui leaves it
                // alone once the left half holds the character.
                if cell.is_wide_continuation() {
                    continue;
                }
                let Some(target) = buf.cell_mut((area.x + col, area.y + row)) else {
                    continue;
                };
                if cell.has_contents() {
                    target.set_symbol(cell.contents());
                } else {
                    target.set_symbol(" ");
                }
                target.set_style(style(cell));
            }
        }
    }
}

fn style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default()
        .fg(color(cell.fgcolor()))
        .bg(color(cell.bgcolor()));
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.dim() {
        style = style.add_modifier(Modifier::DIM);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

fn color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(index) => Color::Indexed(index),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(rows: u16, cols: u16, output: &[u8]) -> vt100::Parser {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(output);
        parser
    }

    fn render(parser: &vt100::Parser, area: Rect) -> Buffer {
        let mut buf = Buffer::empty(area);
        ScreenWidget::new(parser.screen()).render(area, &mut buf);
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
    fn a_screen_bigger_than_the_area_is_cut_off() {
        let parser = screen(2, 6, b"abcdef\r\nghijkl");
        let buf = render(&parser, Rect::new(0, 0, 3, 1));
        assert_eq!(buf[(2, 0)].symbol(), "c");
    }
}
