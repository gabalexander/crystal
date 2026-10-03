//! The pane's side of the selected session: a viewer of it, and the screen
//! that viewer has drawn so far, history and all.

use super::Event;
use crate::history::HISTORY_LINES;
use crate::viewer::Viewer;
use anyhow::Result;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::thread;

pub struct Pane {
    /// Tells this pane's output apart from that of panes already closed,
    /// whose last chunks may still be on their way.
    pub id: u64,
    pub session: String,
    pub screen: vt100::Parser,
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
            session: viewer.name.clone(),
            screen: vt100::Parser::new(rows, cols, HISTORY_LINES),
            ended: false,
            viewer,
        })
    }

    /// The pane's size, as `(rows, cols)`.
    pub fn size(&self) -> (u16, u16) {
        self.screen.screen().size()
    }

    /// Fits the session to a new pane size. The program redraws for it.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.screen.screen_mut().set_size(rows, cols);
        let _ = self.viewer.resize(rows, cols);
    }

    /// Types into the session, which brings the pane back to live: what you
    /// type shows there.
    pub fn send_keys(&mut self, keys: &[u8]) {
        self.screen.screen_mut().set_scrollback(0);
        let _ = self.viewer.send_keys(keys);
    }

    /// How many rows back into the history the pane is showing, or 0 when
    /// it's live.
    pub fn scrolled_back(&self) -> usize {
        self.screen.screen().scrollback()
    }

    /// Shows a page further back into the history.
    pub fn page_back(&mut self) {
        page(self.screen.screen_mut(), Way::Back);
    }

    /// Shows a page further toward live.
    pub fn page_forward(&mut self) {
        page(self.screen.screen_mut(), Way::Forward);
    }

    /// Shows a few lines further back into the history: a notch of the
    /// mouse wheel.
    pub fn scroll_back(&mut self) {
        scroll(self.screen.screen_mut(), Way::Back, WHEEL_LINES);
    }

    /// Shows a few lines further toward live.
    pub fn scroll_forward(&mut self) {
        scroll(self.screen.screen_mut(), Way::Forward, WHEEL_LINES);
    }

    /// Whether the program has asked to hear about the mouse.
    pub fn wants_mouse(&self) -> bool {
        self.screen.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
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
/// row, so the row at the edge stays in sight to read on from. vt100 stops
/// at either end. While the view is back, vt100 keeps it on the same rows
/// as new output comes in, so reading isn't pulled away.
fn page(screen: &mut vt100::Screen, way: Way) {
    let (rows, _) = screen.size();
    let page = usize::from(rows.saturating_sub(1).max(1));
    scroll(screen, way, page);
}

/// Moves the screen's view `lines` through its history.
fn scroll(screen: &mut vt100::Screen, way: Way, lines: usize) {
    let back = match way {
        Way::Back => screen.scrollback().saturating_add(lines),
        Way::Forward => screen.scrollback().saturating_sub(lines),
    };
    screen.set_scrollback(back);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 5-row screen with 20 numbered lines behind it.
    fn screen_with_history() -> vt100::Parser {
        let mut parser = vt100::Parser::new(5, 20, HISTORY_LINES);
        for line in 0..24 {
            parser.process(format!("line {line}\r\n").as_bytes());
        }
        parser
    }

    #[test]
    fn a_page_is_a_screenful_less_a_row() {
        let mut parser = screen_with_history();
        page(parser.screen_mut(), Way::Back);
        assert_eq!(parser.screen().scrollback(), 4);
        page(parser.screen_mut(), Way::Back);
        assert_eq!(parser.screen().scrollback(), 8);
        page(parser.screen_mut(), Way::Forward);
        assert_eq!(parser.screen().scrollback(), 4);
    }

    #[test]
    fn paging_stops_at_both_ends_of_the_history() {
        let mut parser = screen_with_history();
        for _ in 0..10 {
            page(parser.screen_mut(), Way::Back);
        }
        assert_eq!(parser.screen().scrollback(), 20);
        assert!(parser.screen().contents().starts_with("line 0"));

        for _ in 0..10 {
            page(parser.screen_mut(), Way::Forward);
        }
        assert_eq!(parser.screen().scrollback(), 0);
    }

    #[test]
    fn a_notch_of_the_wheel_scrolls_a_few_lines() {
        let mut parser = screen_with_history();
        scroll(parser.screen_mut(), Way::Back, WHEEL_LINES);
        assert_eq!(parser.screen().scrollback(), 3);
        scroll(parser.screen_mut(), Way::Forward, WHEEL_LINES);
        assert_eq!(parser.screen().scrollback(), 0);
    }

    #[test]
    fn new_output_keeps_a_view_into_the_history_where_it_was() {
        let mut parser = screen_with_history();
        page(parser.screen_mut(), Way::Back);
        let shown = parser.screen().contents();

        parser.process(b"line 24\r\nline 25\r\n");
        assert_eq!(parser.screen().contents(), shown);
    }
}
