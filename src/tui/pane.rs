//! The pane's side of the selected session: a viewer of it, and the screen
//! that viewer has drawn so far, history and all.

use super::Event;
use crate::viewer::Viewer;
use crate::vt;
use anyhow::Result;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::thread;

pub struct Pane {
    /// Tells this pane's output apart from that of panes already closed,
    /// whose last chunks may still be on their way.
    pub id: u64,
    /// The id of the session the pane shows, which finds the pane. It stays
    /// the same when the session is renamed, and a session started again
    /// has a new one, so its pane starts afresh.
    pub session_id: String,
    pub screen: vt::Screen,
    /// The session has ended: `screen` is the last it showed.
    pub ended: bool,
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
    /// type shows there.
    pub fn send_keys(&mut self, keys: &[u8]) {
        self.screen.scroll_to_live();
        let _ = self.viewer.send_keys(keys);
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

    /// Shows a few lines further back into the history: a notch of the
    /// mouse wheel.
    pub fn scroll_back(&mut self) {
        scroll(&mut self.screen, Way::Back, WHEEL_LINES);
    }

    /// Shows a few lines further toward live.
    pub fn scroll_forward(&mut self) {
        scroll(&mut self.screen, Way::Forward, WHEEL_LINES);
    }

    /// Whether the program has asked for pastes to be marked as pastes.
    pub fn wants_paste_marked(&self) -> bool {
        self.screen.bracketed_paste()
    }
}

/// How far a notch of the mouse wheel scrolls: what most terminals do.
const WHEEL_LINES: usize = 3;

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
        scroll(&mut screen, Way::Back, WHEEL_LINES);
        assert_eq!(screen.scrolled_back(), 3);
        scroll(&mut screen, Way::Forward, WHEEL_LINES);
        assert_eq!(screen.scrolled_back(), 0);
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
