//! The pane's side of the selected session: a viewer of it, and the screen
//! that viewer has drawn so far.

use super::Event;
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
    pub fn open(
        socket: &Path,
        session: &str,
        rows: u16,
        cols: u16,
        id: u64,
        events: Sender<Event>,
    ) -> Result<Pane> {
        let (viewer, output) = Viewer::connect(socket, Some(session), rows, cols)?;
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
            screen: vt100::Parser::new(rows, cols, 0),
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

    pub fn send_keys(&self, keys: &[u8]) {
        let _ = self.viewer.send_keys(keys);
    }
}
