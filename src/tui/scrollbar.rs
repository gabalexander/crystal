//! A pane's scrollbar: the column beside its screen that shows where in its
//! history the pane is, a thumb as long, against the track, as the screen
//! is against everything it can show, which the mouse drags to scroll.
//! Pure, apart from [`draw`] at the end.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

/// The thumb on a track as tall as the screen beside it: its top, counted
/// from the track's, and how many rows it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thumb {
    pub top: u16,
    pub len: u16,
}

impl Thumb {
    /// The thumb for a screen of `rows` rows with `history` rows behind
    /// it, showing `back` rows back: at the bottom while it's live, at the
    /// top at the start of the history. `None` when there's no history.
    pub fn of(history: usize, back: usize, rows: u16) -> Option<Thumb> {
        if history == 0 || rows == 0 {
            return None;
        }
        let rows = usize::from(rows);
        let total = history + rows;
        let len = ((rows * rows + total / 2) / total).clamp(1, rows);
        let room = rows - len;
        let from_top = history - back.min(history);
        let top = (from_top * room + history / 2) / history;
        Some(Thumb {
            top: top as u16,
            len: len as u16,
        })
    }

    /// Whether the thumb covers row `row` of the track.
    pub fn covers(self, row: u16) -> bool {
        (self.top..self.top + self.len).contains(&row)
    }

    /// Where on the thumb the mouse holds it when it goes down on row
    /// `row` of the track: there, on the thumb; elsewhere, by its middle,
    /// as the thumb jumps to put that under the mouse.
    pub fn grab(self, row: u16) -> u16 {
        if self.covers(row) {
            row - self.top
        } else {
            self.len / 2
        }
    }
}

/// How far back a screen of `rows` rows with `history` rows behind it is
/// to show for its thumb's top to be on row `top` of the track, which may
/// be past either end. `None` when the thumb fills the track and can't
/// move, or there's no history.
pub fn back_at(history: usize, rows: u16, top: i32) -> Option<usize> {
    let thumb = Thumb::of(history, 0, rows)?;
    let room = rows - thumb.len;
    if room == 0 {
        return None;
    }
    let top = top.clamp(0, i32::from(room)) as usize;
    let room = usize::from(room);
    let from_top = (top * history + room / 2) / room;
    Some(history - from_top.min(history))
}

/// Draws the scrollbar in `track`, one column wide: a thin line down it,
/// and the thumb over that.
pub fn draw(buffer: &mut Buffer, track: Rect, thumb: Thumb, line: Style, held: Style) {
    for row in 0..track.height {
        let (symbol, style) = if thumb.covers(row) {
            ("▐", held)
        } else {
            ("▕", line)
        };
        if let Some(cell) = buffer.cell_mut((track.x, track.y + row)) {
            cell.set_symbol(symbol).set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_history_has_no_thumb() {
        assert_eq!(Thumb::of(0, 0, 20), None);
        assert_eq!(back_at(0, 20, 3), None);
    }

    #[test]
    fn the_thumb_is_as_long_against_the_track_as_the_screen_against_all_of_it() {
        // 20 rows of 40: half the track.
        let live = Thumb::of(20, 0, 20).unwrap();
        assert_eq!(live, Thumb { top: 10, len: 10 });
        assert_eq!(Thumb::of(20, 20, 20).unwrap(), Thumb { top: 0, len: 10 });
        assert_eq!(Thumb::of(20, 10, 20).unwrap(), Thumb { top: 5, len: 10 });
        // However long the history, the thumb is a row at least.
        assert_eq!(
            Thumb::of(100_000, 0, 20).unwrap(),
            Thumb { top: 19, len: 1 }
        );
    }

    #[test]
    fn dragging_the_thumb_to_a_row_shows_that_far_into_the_history() {
        assert_eq!(back_at(20, 20, 0), Some(20));
        assert_eq!(back_at(20, 20, 5), Some(10));
        assert_eq!(back_at(20, 20, 10), Some(0));
        // Past either end it stops there.
        assert_eq!(back_at(20, 20, -4), Some(20));
        assert_eq!(back_at(20, 20, 30), Some(0));
        // Wherever it's dragged, the thumb ends up there.
        for top in 0..=19 {
            let back = back_at(10_000, 20, top).unwrap();
            assert_eq!(Thumb::of(10_000, back, 20).unwrap().top, top as u16);
        }
    }

    #[test]
    fn a_thumb_that_fills_the_track_doesnt_move() {
        // A row of history behind a one-row screen.
        assert_eq!(Thumb::of(1, 0, 1).unwrap(), Thumb { top: 0, len: 1 });
        assert_eq!(back_at(1, 1, 0), None);
    }

    #[test]
    fn the_mouse_holds_the_thumb_where_it_went_down_or_by_its_middle() {
        let thumb = Thumb { top: 5, len: 4 };
        assert_eq!(thumb.grab(6), 1);
        assert_eq!(thumb.grab(0), 2);
        assert_eq!(thumb.grab(15), 2);
    }

    #[test]
    fn the_thumb_is_drawn_over_a_line_down_the_track() {
        let track = Rect::new(2, 1, 1, 4);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 6));
        let thumb = Thumb { top: 1, len: 2 };
        draw(&mut buffer, track, thumb, Style::new(), Style::new());
        let column: Vec<&str> = (0..6).map(|y| buffer[(2, y)].symbol()).collect();
        assert_eq!(column, [" ", "▕", "▐", "▐", "▕", " "]);
    }
}
