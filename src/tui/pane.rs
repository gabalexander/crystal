//! The pane's side of the selected session: a viewer of it, and the screen
//! that viewer has drawn so far, history and all, with copy mode over it
//! while that's on; what the mouse selects on it, by characters, words or
//! lines, and its scrollbar.

use super::Event;
use super::app::Slot;
use super::copy_mode::{self, CopyMode};
use super::scrollbar::{self, Thumb};
use crate::viewer::Viewer;
use crate::vt::{self, SelectionKind};
use anyhow::Result;
use crossterm::event::KeyEvent;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

pub struct Pane {
    /// Tells this pane's output apart from that of panes already closed,
    /// whose last chunks may still be on their way.
    pub id: u64,
    /// The id of the session the pane shows, which finds the pane. It stays
    /// the same when the session is renamed, and a session started again
    /// has a new one, so its pane starts afresh.
    pub session_id: String,
    pub screen: vt::Screen,
    /// Its output has ended: the session has, and `screen` is the last it
    /// showed, or the daemon was handed over to a new crystal, and the pane
    /// attaches again.
    pub ended: bool,
    /// Copy mode, while it's on.
    pub copy: Option<CopyMode>,
    /// The cell of the screen the mouse last took the selection's end to:
    /// where it is while a drag scrolls the history under it.
    selected_to: Option<(u16, u16)>,
    /// Where on the scrollbar's thumb the mouse holds it, while it does.
    holding_thumb: Option<u16>,
    viewer: Viewer,
}

impl Pane {
    /// Attaches to `session` at the given size. Its output arrives as
    /// [`Event::Output`]s tagged with `id`, then an [`Event::OutputEnded`].
    /// The output starts with the session's history, so the pane can
    /// scroll back to before it opened.
    pub fn open(
        socket: &Path,
        session: &str,
        rows: u16,
        cols: u16,
        id: u64,
        events: Sender<Event>,
    ) -> Result<Pane> {
        let (viewer, output) = Viewer::connect(socket, Some(session), (rows, cols), true)?;
        thread::spawn(move || {
            for bytes in output {
                if events.send(Event::Output { pane: id, bytes }).is_err() {
                    return;
                }
            }
            let _ = events.send(Event::OutputEnded { pane: id });
        });
        Ok(Pane {
            id,
            session_id: viewer.id.clone(),
            screen: vt::Screen::new(rows, cols),
            ended: false,
            copy: None,
            selected_to: None,
            holding_thumb: None,
            viewer,
        })
    }

    /// The pane's size, as `(rows, cols)`.
    pub fn size(&self) -> (u16, u16) {
        self.screen.size()
    }

    /// Fits the session to a new pane size. The program redraws for it.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.screen.resize(rows, cols);
        let _ = self.viewer.resize(rows, cols);
    }

    /// Types into the session, which brings the pane back to live: what you
    /// type shows there. What the mouse selected is let go.
    pub fn send_keys(&mut self, keys: &[u8]) {
        self.screen.scroll_to_live();
        self.screen.clear_selection();
        let _ = self.viewer.send_keys(keys);
    }

    /// Turns copy mode on or off, to follow whether the keyboard is in it.
    pub fn set_copying(&mut self, on: bool) {
        match (on, self.copy.is_some()) {
            (true, false) => {
                self.screen.start_copying();
                self.copy = Some(CopyMode::default());
            }
            (false, true) => {
                self.screen.stop_copying();
                self.copy = None;
            }
            _ => {}
        }
    }

    /// A key in copy mode, which it turns on first if it isn't yet.
    pub fn copy_key(&mut self, key: KeyEvent) -> copy_mode::Outcome {
        self.set_copying(true);
        let copy = self.copy.get_or_insert_default();
        copy.on_key(&mut self.screen, key)
    }

    /// The mouse went down on the cell at `(row, col)`, the first click,
    /// the second or the third of `clicks` in a row there: where a
    /// selection starts, of characters if it drags, or the word there, or
    /// the line; and where copy mode's cursor goes.
    pub fn select_from(&mut self, cell: (u16, u16), clicks: u8) {
        self.screen.select_from(cell, selection_kind(clicks));
        self.selected_to = Some(cell);
        if self.copy.is_some() {
            self.screen.put_copy_cursor(cell);
        }
    }

    /// The mouse dragged to the cell at `(row, col)`. In copy mode, its
    /// cursor goes there too, and takes the selection's end on from there.
    pub fn select_to(&mut self, cell: (u16, u16)) {
        self.screen.select_to(cell);
        self.selected_to = Some(cell);
        if self.copy.is_some() {
            self.screen.put_copy_cursor(cell);
        }
    }

    /// The history scrolled under a drag that's selecting: the selection's
    /// end goes on to what's under the mouse now.
    pub fn follow_drag(&mut self) {
        if let Some(cell) = self.selected_to {
            self.select_to(cell);
        }
    }

    /// The mouse let go of what it selected, to be copied with copy mode's
    /// keys rather than at once: copy mode comes on, the selection kept,
    /// its cursor where the mouse let go. Returns whether anything was
    /// selected to keep.
    pub fn hold_selection(&mut self) -> bool {
        if !self.screen.selecting() {
            return false;
        }
        let cell = self.selected_to.unwrap_or_default();
        self.screen.start_copying_at(cell);
        self.copy.get_or_insert_default();
        true
    }

    /// The drag selecting in this pane has gone `past` rows above its
    /// screen, when that's below 0, or below it: scrolls the history that
    /// way, further the further past, and takes the selection along.
    pub fn scroll_past_edge(&mut self, past: i32) {
        self.screen.scroll_back(edge_lines(past));
        self.follow_drag();
    }

    /// The pane's scrollbar's thumb, while there's history to scroll.
    pub fn thumb(&self) -> Option<Thumb> {
        let (rows, _) = self.size();
        Thumb::of(self.screen.history(), self.scrolled_back(), rows)
    }

    /// The mouse went down on row `row` of the scrollbar: on the thumb it
    /// takes it there; elsewhere the thumb jumps to put its middle there.
    pub fn grab_thumb(&mut self, row: u16) {
        let Some(thumb) = self.thumb() else {
            self.holding_thumb = None;
            return;
        };
        self.holding_thumb = Some(thumb.grab(row));
        self.drag_thumb(row);
    }

    /// The mouse dragged the thumb it holds to row `row` of the scrollbar,
    /// or past either end: the view goes as far into the history.
    pub fn drag_thumb(&mut self, row: u16) {
        let (Some(held), Some(thumb)) = (self.holding_thumb, self.thumb()) else {
            return;
        };
        let top = i32::from(row) - i32::from(held);
        // A row of the track stands for many rows of history: the view
        // moves only as the thumb does, so taking it where it is moves
        // nothing.
        if top == i32::from(thumb.top) {
            return;
        }
        let (rows, _) = self.size();
        if let Some(back) = scrollbar::back_at(self.screen.history(), rows, top) {
            let by = back as isize - self.scrolled_back() as isize;
            self.screen.scroll_back(by);
        }
    }

    /// How many rows back into the history the pane is showing, or 0 when
    /// it's live.
    pub fn scrolled_back(&self) -> usize {
        self.screen.scrolled_back()
    }

    /// Shows a page further back into the history.
    pub fn page_back(&mut self) {
        page(&mut self.screen, Way::Back);
    }

    /// Shows a page further toward live.
    pub fn page_forward(&mut self) {
        page(&mut self.screen, Way::Forward);
    }

    /// Shows `lines` further back into the history: a notch of the mouse
    /// wheel.
    pub fn scroll_back(&mut self, lines: usize) {
        scroll(&mut self.screen, Way::Back, lines);
    }

    /// Shows `lines` further toward live.
    pub fn scroll_forward(&mut self, lines: usize) {
        scroll(&mut self.screen, Way::Forward, lines);
    }

    /// Whether the program has asked for pastes to be marked as pastes.
    pub fn wants_paste_marked(&self) -> bool {
        self.screen.bracketed_paste()
    }
}

/// What the clicks in a row select: characters from the first, the word
/// from a double-click, the line from a triple-click.
fn selection_kind(clicks: u8) -> SelectionKind {
    match clicks {
        2 => SelectionKind::Words,
        3 => SelectionKind::Lines,
        _ => SelectionKind::Chars,
    }
}

/// How many rows a drag `past` rows beyond the edge of a pane's screen
/// scrolls its history each time: back, above the top, and toward live,
/// below the bottom; a row for each row past, up to [`EDGE_LINES_MOST`].
fn edge_lines(past: i32) -> isize {
    let lines = past.clamp(-EDGE_LINES_MOST, EDGE_LINES_MOST);
    -lines as isize
}

/// The most rows a drag past the edge scrolls each time.
const EDGE_LINES_MOST: i32 = 10;

/// How often a drag held past the edge of a pane's screen scrolls again.
pub const EDGE_SCROLL_EVERY: Duration = Duration::from_millis(50);

/// The longest between the clicks of a double- or triple-click.
const CLICKS_WITHIN: Duration = Duration::from_millis(400);

/// Counts the clicks on a pane's screen that come one quickly after
/// another on the same cell, or one beside it: a double-click selects a
/// word, and a triple-click a line.
#[derive(Debug, Default)]
pub struct Clicks {
    last: Option<Click>,
}

#[derive(Debug, Clone, Copy)]
struct Click {
    slot: Slot,
    cell: (u16, u16),
    at: Instant,
    count: u8,
}

impl Clicks {
    /// A click on `cell` of the screen of the pane at `slot`, at `at`: the
    /// first in a row, the second or the third, and a fourth is a first
    /// again.
    pub fn click(&mut self, slot: Slot, cell: (u16, u16), at: Instant) -> u8 {
        let near = |(row, col): (u16, u16)| row.abs_diff(cell.0) <= 1 && col.abs_diff(cell.1) <= 1;
        let count = match self.last {
            Some(last)
                if last.slot == slot
                    && near(last.cell)
                    && at.saturating_duration_since(last.at) <= CLICKS_WITHIN =>
            {
                last.count % 3 + 1
            }
            _ => 1,
        };
        self.last = Some(Click {
            slot,
            cell,
            at,
            count,
        });
        count
    }

    /// The mouse dragged to `cell` with the button down: off the cell it
    /// went down on, it's selecting, and the next click is a first.
    pub fn dragged_to(&mut self, cell: (u16, u16)) {
        if self.last.is_some_and(|last| last.cell != cell) {
            self.last = None;
        }
    }
}

/// Which way through the history a page goes.
#[derive(Debug, Clone, Copy)]
enum Way {
    Back,
    Forward,
}

/// Moves the screen's view a page through its history: a screenful less a
/// row, so the row at the edge stays in sight to read on from. It stops at
/// either end. While the view is back, it stays on the same rows as new
/// output comes in, so reading isn't pulled away.
fn page(screen: &mut vt::Screen, way: Way) {
    let (rows, _) = screen.size();
    let page = usize::from(rows.saturating_sub(1).max(1));
    scroll(screen, way, page);
}

/// Moves the screen's view `lines` through its history.
fn scroll(screen: &mut vt::Screen, way: Way, lines: usize) {
    let lines = isize::try_from(lines).unwrap_or(isize::MAX);
    match way {
        Way::Back => screen.scroll_back(lines),
        Way::Forward => screen.scroll_back(-lines),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 5-row screen with 20 numbered lines behind it.
    fn screen_with_history() -> vt::Screen {
        let mut screen = vt::Screen::new(5, 20);
        for line in 0..24 {
            screen.process(format!("line {line}\r\n").as_bytes());
        }
        screen
    }

    /// The rows showing.
    fn showing(screen: &vt::Screen) -> Vec<String> {
        let mut rows = vec![String::new(); 5];
        screen.each_cell(|row, _, cell| rows[usize::from(row)].push_str(cell.text));
        rows.iter().map(|row| row.trim_end().to_string()).collect()
    }

    #[test]
    fn a_page_is_a_screenful_less_a_row() {
        let mut screen = screen_with_history();
        page(&mut screen, Way::Back);
        assert_eq!(screen.scrolled_back(), 4);
        page(&mut screen, Way::Back);
        assert_eq!(screen.scrolled_back(), 8);
        page(&mut screen, Way::Forward);
        assert_eq!(screen.scrolled_back(), 4);
    }

    #[test]
    fn paging_stops_at_both_ends_of_the_history() {
        let mut screen = screen_with_history();
        for _ in 0..10 {
            page(&mut screen, Way::Back);
        }
        assert_eq!(screen.scrolled_back(), 20);
        assert_eq!(showing(&screen)[0], "line 0");

        for _ in 0..10 {
            page(&mut screen, Way::Forward);
        }
        assert_eq!(screen.scrolled_back(), 0);
    }

    #[test]
    fn a_notch_of_the_wheel_scrolls_a_few_lines() {
        let mut screen = screen_with_history();
        scroll(&mut screen, Way::Back, 3);
        assert_eq!(screen.scrolled_back(), 3);
        scroll(&mut screen, Way::Forward, 3);
        assert_eq!(screen.scrolled_back(), 0);
    }

    #[test]
    fn a_drag_past_the_edge_scrolls_further_the_further_past() {
        assert_eq!(edge_lines(-1), 1);
        assert_eq!(edge_lines(-3), 3);
        assert_eq!(edge_lines(2), -2);
        assert_eq!(edge_lines(-40), 10);
        assert_eq!(edge_lines(0), 0);
    }

    #[test]
    fn clicks_in_a_row_on_one_cell_count_up_to_three_then_start_again() {
        let mut clicks = Clicks::default();
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let pane = Slot::Selected;
        assert_eq!(clicks.click(pane, (2, 5), at(0)), 1);
        assert_eq!(clicks.click(pane, (2, 5), at(200)), 2);
        // A hand that moved a cell meanwhile still counts.
        assert_eq!(clicks.click(pane, (2, 6), at(400)), 3);
        assert_eq!(clicks.click(pane, (2, 6), at(500)), 1);
        // Too slow, too far, or in another pane, it's a first.
        assert_eq!(clicks.click(pane, (2, 6), at(1000)), 1);
        assert_eq!(clicks.click(pane, (4, 6), at(1100)), 1);
        assert_eq!(clicks.click(Slot::Split(0), (4, 6), at(1200)), 1);
        // A click that dragged away selected: the next is a first.
        clicks.dragged_to((4, 6));
        assert_eq!(clicks.click(Slot::Split(0), (4, 6), at(1300)), 2);
        clicks.dragged_to((4, 9));
        assert_eq!(clicks.click(Slot::Split(0), (4, 6), at(1400)), 1);
    }

    #[test]
    fn a_double_click_selects_a_word_and_a_triple_click_a_line() {
        assert_eq!(selection_kind(1), SelectionKind::Chars);
        assert_eq!(selection_kind(2), SelectionKind::Words);
        assert_eq!(selection_kind(3), SelectionKind::Lines);
    }

    #[test]
    fn new_output_keeps_a_view_into_the_history_where_it_was() {
        let mut screen = screen_with_history();
        page(&mut screen, Way::Back);
        let shown = showing(&screen);

        screen.process(b"line 24\r\nline 25\r\n");
        assert_eq!(showing(&screen), shown);
    }
}
