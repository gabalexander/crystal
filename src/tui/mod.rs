//! The TUI, run as `crystal` with no command: a sidebar with every session,
//! and the selected one live in a pane beside it.
//!
//! Everything that happens arrives as an [`Event`] on one channel: a key,
//! a resize, output from the session in the pane, a fresh session list.
//! The loop takes each event, updates the state, and draws.

mod app;
mod keys;
mod pane;
mod screen_widget;
mod ui;

use crate::protocol::{Request, Response, SessionInfo};
use crate::{client, env};
use anyhow::{Result, bail};
use app::{Action, App};
use crossterm::event::{Event as TerminalEvent, KeyEvent, KeyEventKind};
use pane::Pane;
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

/// How often the session list is asked for. The daemon doesn't announce
/// changes, so this is how far behind the list can be.
const POLL_EVERY: Duration = Duration::from_millis(500);

pub enum Event {
    Key(KeyEvent),
    /// The terminal changed size. The next draw lays everything out again
    /// and resizes the pane's session to fit.
    Resize,
    Sessions(Vec<SessionInfo>),
    /// Output from the session in the pane with this id.
    Output {
        pane: u64,
        bytes: Vec<u8>,
    },
    /// The session in the pane with this id has ended.
    OutputEnded {
        pane: u64,
    },
}

pub fn run(socket: &Path) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("crystal needs a terminal; see crystal --help for the commands");
    }
    // Asking for the list starts the daemon if it isn't running.
    let sessions = list_sessions(socket, true)?;

    let (sender, events) = mpsc::channel();
    spawn_input_reader(sender.clone());
    spawn_session_poller(socket.to_path_buf(), sender.clone());

    let mut tui = Tui {
        socket: socket.to_path_buf(),
        app: App::new(env::own_session(socket)),
        pane: None,
        last_pane_id: 0,
        events: sender,
        quitting: false,
    };
    tui.app.set_sessions(sessions);

    let mut terminal = ratatui::try_init()?;
    let result = tui.run(&mut terminal, events);
    ratatui::restore();
    result
}

struct Tui {
    socket: PathBuf,
    app: App,
    /// A viewer of the selected session, once there is one to show.
    pane: Option<Pane>,
    last_pane_id: u64,
    /// Handed to each pane, for its output.
    events: Sender<Event>,
    quitting: bool,
}

impl Tui {
    fn run(&mut self, terminal: &mut DefaultTerminal, events: Receiver<Event>) -> Result<()> {
        while !self.quitting {
            let size = terminal.size()?;
            let areas = ui::Areas::new(Rect::new(0, 0, size.width, size.height));
            self.sync_pane(areas.session_screen());
            terminal.draw(|frame| ui::draw(frame, &self.app, self.pane.as_ref()))?;

            // Wait for something to happen, then take whatever else has
            // happened meanwhile, so a burst of output is drawn once.
            let event = events.recv()?;
            self.handle(event);
            while let Ok(event) = events.try_recv() {
                self.handle(event);
            }
        }
        Ok(())
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.on_key(key),
            Event::Resize => {}
            Event::Sessions(sessions) => self.app.set_sessions(sessions),
            Event::Output { pane, bytes } => {
                if let Some(pane) = self.pane_with_id(pane) {
                    pane.screen.process(&bytes);
                }
            }
            Event::OutputEnded { pane } => {
                if let Some(pane) = self.pane_with_id(pane) {
                    pane.ended = true;
                }
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        let Some(action) = self.app.on_key(key) else {
            return;
        };
        // A key that fails, say because its session has just gone, says
        // why at the bottom rather than closing the TUI.
        if let Err(err) = self.perform(action) {
            self.app.notify(format!("{err:#}"));
        }
    }

    fn perform(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Quit => self.quitting = true,
            Action::NewSession => {
                let cwd = std::env::current_dir()?;
                let name = client::new_session(&self.socket, None, cwd, Vec::new())?;
                self.refresh_sessions()?;
                self.app.select(&name);
                self.app.type_into_selected();
            }
            Action::Kill(name) => {
                client::ask(&self.socket, &Request::Kill { name }, false)?;
                self.refresh_sessions()?;
            }
            Action::Type(key) => {
                if let Some(pane) = &self.pane {
                    let application_cursor = pane.screen.screen().application_cursor();
                    if let Some(bytes) = keys::encode(&key, application_cursor) {
                        pane.send_keys(&bytes);
                    }
                }
            }
        }
        Ok(())
    }

    /// Asks for the list now, rather than waiting for the next poll, so a
    /// key's effect shows straight away.
    fn refresh_sessions(&mut self) -> Result<()> {
        let sessions = list_sessions(&self.socket, false)?;
        self.app.set_sessions(sessions);
        Ok(())
    }

    /// Keeps the pane on the selected session at the size it's drawn at:
    /// attaches to another session when the selection moves, and resizes
    /// when the layout changes.
    fn sync_pane(&mut self, area: Rect) {
        let rows = area.height.max(1);
        let cols = area.width.max(1);
        let wanted = match self.app.selected() {
            Some(session) if !self.app.selected_is_own() => session.name.clone(),
            _ => {
                self.pane = None;
                return;
            }
        };

        let showing_wanted = self
            .pane
            .as_ref()
            .is_some_and(|pane| pane.session == wanted);
        if !showing_wanted {
            self.last_pane_id += 1;
            let id = self.last_pane_id;
            let events = self.events.clone();
            // If it fails, the session has likely just gone; the next list
            // will catch up.
            self.pane = Pane::open(&self.socket, &wanted, rows, cols, id, events).ok();
        }
        if let Some(pane) = &mut self.pane
            && pane.size() != (rows, cols)
        {
            pane.resize(rows, cols);
        }
    }

    fn pane_with_id(&mut self, id: u64) -> Option<&mut Pane> {
        self.pane.as_mut().filter(|pane| pane.id == id)
    }
}

fn list_sessions(socket: &Path, start: bool) -> Result<Vec<SessionInfo>> {
    match client::ask(socket, &Request::List, start)? {
        Some(Response::Sessions { sessions }) => Ok(sessions),
        _ => Ok(Vec::new()),
    }
}

/// Reads keys and resizes off the terminal on a thread of its own, since
/// reading blocks.
fn spawn_input_reader(events: Sender<Event>) {
    thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            let event = match event {
                TerminalEvent::Key(key) if key.kind != KeyEventKind::Release => Event::Key(key),
                TerminalEvent::Resize(..) => Event::Resize,
                _ => continue,
            };
            if events.send(event).is_err() {
                return;
            }
        }
    });
}

fn spawn_session_poller(socket: PathBuf, events: Sender<Event>) {
    thread::spawn(move || {
        loop {
            thread::sleep(POLL_EVERY);
            // A daemon that has gone away has no sessions left.
            let sessions = list_sessions(&socket, false).unwrap_or_default();
            if events.send(Event::Sessions(sessions)).is_err() {
                return;
            }
        }
    });
}
