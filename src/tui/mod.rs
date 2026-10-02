//! The TUI, run as `crystal` with no command: a sidebar with every session,
//! the selected one live in a pane beside it, and up to two more split off
//! into panes of their own.
//!
//! Everything that happens arrives as an [`Event`] on one channel: a key,
//! a resize, output from the session in the pane, a fresh session list.
//! The loop takes each event, updates the state, and draws.

mod app;
mod command_line;
mod groups;
mod keys;
mod pane;
mod screen_widget;
mod text_input;
mod ui;

use crate::protocol::{Request, Response, SessionInfo};
use crate::{client, env, git};
use anyhow::{Result, bail};
use app::{Action, App, Place, Slot};
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
        panes: Vec::new(),
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
    /// A viewer of each session a pane shows: the selected one and the
    /// split ones. No session is shown twice, so its name finds its pane.
    panes: Vec<Pane>,
    last_pane_id: u64,
    /// Handed to each pane, for its output.
    events: Sender<Event>,
    quitting: bool,
}

impl Tui {
    fn run(&mut self, terminal: &mut DefaultTerminal, events: Receiver<Event>) -> Result<()> {
        while !self.quitting {
            let size = terminal.size()?;
            let screen = Rect::new(0, 0, size.width, size.height);
            let areas = ui::Areas::new(screen, self.app.splits().len());
            self.sync_panes(&areas);
            terminal.draw(|frame| ui::draw(frame, &self.app, &self.panes))?;

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
            Action::Start { place, command } => {
                let cwd = directory_for(place)?;
                self.start_session(cwd, command)?;
            }
            Action::Kill(name) => {
                client::ask(&self.socket, &Request::Kill { name }, false)?;
                self.refresh_sessions()?;
            }
            Action::Type { to, key } => {
                if let Some(pane) = self.pane_in(to) {
                    let application_cursor = pane.screen.screen().application_cursor();
                    if let Some(bytes) = keys::encode(&key, application_cursor) {
                        pane.send_keys(&bytes);
                    }
                }
            }
        }
        Ok(())
    }

    /// Starts `command` in a new session in `cwd`, or the user's shell when
    /// it's empty, then selects the session and hands it the keyboard.
    fn start_session(&mut self, cwd: PathBuf, command: Vec<String>) -> Result<()> {
        let name = client::new_session(&self.socket, None, cwd, command)?;
        self.refresh_sessions()?;
        self.app.select(&name);
        self.app.type_into_selected();
        Ok(())
    }

    /// Asks for the list now, rather than waiting for the next poll, so a
    /// key's effect shows straight away.
    fn refresh_sessions(&mut self) -> Result<()> {
        let sessions = list_sessions(&self.socket, false)?;
        self.app.set_sessions(sessions);
        Ok(())
    }

    /// Keeps a viewer on each session a pane shows, at the size it's drawn
    /// at: attaches to a session as it comes on screen, resizes when the
    /// layout changes, and lets go of sessions no pane shows any more.
    fn sync_panes(&mut self, areas: &ui::Areas) {
        let mut before = std::mem::take(&mut self.panes);
        for (slot, area) in self.app.slots().into_iter().zip(&areas.panes) {
            if !self.app.shows_screen(slot) {
                continue;
            }
            let Some(session) = self.app.pane_session(slot) else {
                continue;
            };
            let name = session.name.clone();
            let screen = ui::screen_area(*area);
            let (rows, cols) = (screen.height.max(1), screen.width.max(1));

            let kept = before.iter().position(|pane| pane.session == name);
            let pane = match kept {
                Some(index) => Some(before.swap_remove(index)),
                None => self.open_pane(&name, rows, cols),
            };
            if let Some(mut pane) = pane {
                if pane.size() != (rows, cols) {
                    pane.resize(rows, cols);
                }
                self.panes.push(pane);
            }
        }
        // What's left in `before` is on no pane now. Dropping a viewer
        // hangs up.
    }

    /// Attaches a new pane to `session`. If that fails, the session has
    /// likely just gone, and the next list will catch up.
    fn open_pane(&mut self, session: &str, rows: u16, cols: u16) -> Option<Pane> {
        self.last_pane_id += 1;
        let id = self.last_pane_id;
        let events = self.events.clone();
        Pane::open(&self.socket, session, rows, cols, id, events).ok()
    }

    /// The viewer of the session the pane at `slot` shows.
    fn pane_in(&self, slot: Slot) -> Option<&Pane> {
        if !self.app.shows_screen(slot) {
            return None;
        }
        let session = self.app.pane_session(slot)?;
        self.panes.iter().find(|pane| pane.session == session.name)
    }

    fn pane_with_id(&mut self, id: u64) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|pane| pane.id == id)
    }
}

/// The directory a new session at `place` starts in, making the worktree
/// first when it's a new one. Where a place says nothing, it's the TUI's
/// own directory.
fn directory_for(place: Place) -> Result<PathBuf> {
    match place {
        Place::Directory(Some(dir)) => Ok(dir),
        Place::Directory(None) => Ok(std::env::current_dir()?),
        Place::NewWorktree { branch, base } => {
            let base = match base {
                Some(base) => base,
                None => std::env::current_dir()?,
            };
            git::add_worktree(&base, &branch)
        }
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
