//! The TUI, run as `crystal` with no command: a sidebar with every session,
//! the selected one live in a pane beside it, and up to two more split off
//! into panes of their own.
//!
//! Everything that happens arrives as an [`Event`] on one channel: a key,
//! the mouse, a resize, output from the session in the pane, a fresh
//! session list. The loop takes each event, updates the state, and draws.

mod app;
mod command_line;
mod groups;
mod help;
mod mouse;
mod pane;
mod screen_widget;
mod sidebar;
mod status;
mod text_input;
mod theme;
mod ui;

use crate::config::Config;
use crate::keys;
use crate::protocol::{Request, Response, SessionInfo};
use crate::{client, env, git};
use anyhow::{Result, bail};
use app::{Action, App, Focus, Hit, Place, Slot};
use crossterm::event::{Event as TerminalEvent, KeyEvent, KeyEventKind, MouseEvent};
use pane::Pane;
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use theme::Theme;

/// How often the session list is asked for. The daemon doesn't announce
/// changes, so this is how far behind the list can be.
const POLL_EVERY: Duration = Duration::from_millis(500);

/// How often the working mark turns a quarter, while an agent works. With
/// nothing working, the TUI waits for something to happen instead.
const SPIN_EVERY: Duration = Duration::from_millis(150);

pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
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
    let config = Config::load()?;
    // Asking for the list starts the daemon if it isn't running.
    let sessions = list_sessions(socket, true)?;

    let (sender, events) = mpsc::channel();
    spawn_input_reader(sender.clone());
    spawn_session_poller(socket.to_path_buf(), sender.clone());

    let mut tui = Tui {
        socket: socket.to_path_buf(),
        app: App::new(env::own_session_id(socket)),
        panes: Vec::new(),
        last_pane_id: 0,
        events: sender,
        screen: Rect::default(),
        theme: Theme::from_env(config.theme),
        started: Instant::now(),
        quitting: false,
    };
    tui.app.set_first_command(config.new_session);
    tui.app.set_sessions(sessions);

    let mut terminal = ratatui::try_init()?;
    let result = tui.run_with_mouse(&mut terminal, events);
    ratatui::restore();
    result
}

/// The terminal sending the TUI what the mouse does, for as long as this
/// lives. However the TUI ends, by returning, failing or panicking, the
/// mouse goes back to the terminal: left on, a shell would fill with the
/// sequences the terminal sends for it.
struct MouseCapture;

impl MouseCapture {
    fn on() -> Result<MouseCapture> {
        // A panic on any thread turns it off before the panic is shown.
        let shown_before = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            mouse_off();
            shown_before(info);
        }));
        // Clicks and the wheel (1000), drags (1002), written the SGR way
        // (1006). Not the mouse just moving (1003): nothing here needs it,
        // and it would wake the TUI at every move.
        let mut out = std::io::stdout();
        out.write_all(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h")?;
        out.flush()?;
        Ok(MouseCapture)
    }
}

impl Drop for MouseCapture {
    fn drop(&mut self) {
        mouse_off();
    }
}

fn mouse_off() {
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b[?1006l\x1b[?1002l\x1b[?1000l");
    let _ = out.flush();
}

struct Tui {
    socket: PathBuf,
    app: App,
    /// A viewer of each session a pane shows: the selected one and the
    /// split ones. No session is shown twice, so its id finds its pane.
    panes: Vec<Pane>,
    last_pane_id: u64,
    /// Handed to each pane, for its output.
    events: Sender<Event>,
    /// The whole screen as it was last drawn, to find what the mouse is on.
    screen: Rect,
    theme: Theme,
    /// When the TUI started: the working mark turns with the time since.
    started: Instant,
    quitting: bool,
}

impl Tui {
    fn run_with_mouse(
        &mut self,
        terminal: &mut DefaultTerminal,
        events: Receiver<Event>,
    ) -> Result<()> {
        // The mouse is the TUI's for as long as `_mouse` lives: to the end
        // of this function, however it ends.
        let _mouse = MouseCapture::on()?;
        self.run(terminal, events)
    }

    fn run(&mut self, terminal: &mut DefaultTerminal, events: Receiver<Event>) -> Result<()> {
        while !self.quitting {
            let size = terminal.size()?;
            self.screen = Rect::new(0, 0, size.width, size.height);
            let areas = ui::Areas::new(self.screen, self.app.splits().len());
            self.sync_panes(&areas);
            let look = ui::Look {
                theme: &self.theme,
                now: seconds_since_epoch(),
                spin: (self.started.elapsed().as_millis() / SPIN_EVERY.as_millis()) as usize,
            };
            terminal.draw(|frame| ui::draw(frame, &self.app, &self.panes, &look))?;

            // Wait for something to happen, then take whatever else has
            // happened meanwhile, so a burst of output is drawn once.
            if let Some(event) = self.next_event(&events)? {
                self.handle(event);
            }
            while let Ok(event) = events.try_recv() {
                self.handle(event);
            }
        }
        Ok(())
    }

    /// The next event. While an agent works, the wait is cut short in time
    /// to turn its mark, and there's no event: only a frame to draw.
    fn next_event(&self, events: &Receiver<Event>) -> Result<Option<Event>> {
        if !self.app.anything_working() {
            return Ok(Some(events.recv()?));
        }
        match events.recv_timeout(SPIN_EVERY) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => bail!("the TUI's events stopped"),
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.on_key(key),
            Event::Mouse(mouse) => self.on_mouse(mouse),
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

    fn on_mouse(&mut self, mouse: MouseEvent) {
        let areas = ui::Areas::new(self.screen, self.app.splits().len());
        let hit = ui::hit(&areas, &self.app, mouse.column, mouse.row);
        if self.pass_to_program(&mouse, hit) {
            return;
        }
        let Some(action) = self.app.on_mouse(mouse.kind, hit) else {
            return;
        };
        if let Err(err) = self.perform(action) {
            self.app.notify(format!("{err:#}"));
        }
    }

    /// Hands the mouse to the program in the pane that has the keyboard,
    /// if it asked for the mouse and the pane is showing it live (back in
    /// the history, the program's screen isn't what's under the mouse).
    /// Returns whether the program took it.
    fn pass_to_program(&mut self, mouse: &MouseEvent, hit: Hit) -> bool {
        let Hit::Pane {
            slot,
            cell: Some(cell),
        } = hit
        else {
            return false;
        };
        if self.app.focus() != Focus::Pane(slot) {
            return false;
        }
        let Some(pane) = self.pane_in(slot) else {
            return false;
        };
        if !pane.wants_mouse() || pane.scrolled_back() > 0 {
            return false;
        }
        let screen = pane.screen.screen();
        let mode = screen.mouse_protocol_mode();
        let encoding = screen.mouse_protocol_encoding();
        // An event the program didn't ask for, like a drag when it asked
        // only for clicks, is still the program's: it has the mouse here.
        if let Some(bytes) = mouse::encode(mouse.kind, mouse.modifiers, cell, mode, encoding) {
            pane.send_keys(&bytes);
        }
        true
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
            Action::Rename { name, new_name } => {
                client::rename(&self.socket, &name, &new_name)?;
                self.app.renamed(&name, &new_name);
                self.refresh_sessions()?;
                self.app.select(&new_name);
            }
            Action::Respawn(name) => {
                client::respawn(&self.socket, &name)?;
                self.refresh_sessions()?;
                self.app.select(&name);
                self.app.type_into_selected();
            }
            Action::RemoveWorktree(path) => {
                client::remove_worktree(&self.socket, &path)?;
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
            Action::PageBack(slot) => {
                if let Some(pane) = self.pane_in(slot) {
                    pane.page_back();
                }
            }
            Action::PageForward(slot) => {
                if let Some(pane) = self.pane_in(slot) {
                    pane.page_forward();
                }
            }
            Action::ScrollBack(slot) => {
                if let Some(pane) = self.pane_in(slot) {
                    pane.scroll_back();
                }
            }
            Action::ScrollForward(slot) => {
                if let Some(pane) = self.pane_in(slot) {
                    pane.scroll_forward();
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

            let kept = before.iter().position(|pane| pane.session_id == session.id);
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
    fn pane_in(&mut self, slot: Slot) -> Option<&mut Pane> {
        if !self.app.shows_screen(slot) {
            return None;
        }
        let session = self.app.pane_session(slot)?;
        self.panes
            .iter_mut()
            .find(|pane| pane.session_id == session.id)
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

fn seconds_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

fn list_sessions(socket: &Path, start: bool) -> Result<Vec<SessionInfo>> {
    match client::ask(socket, &Request::List, start)? {
        Some(Response::Sessions { sessions }) => Ok(sessions),
        _ => Ok(Vec::new()),
    }
}

/// Reads keys, the mouse and resizes off the terminal on a thread of its
/// own, since reading blocks.
fn spawn_input_reader(events: Sender<Event>) {
    thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            let event = match event {
                TerminalEvent::Key(key) if key.kind != KeyEventKind::Release => Event::Key(key),
                TerminalEvent::Mouse(mouse) => Event::Mouse(mouse),
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
