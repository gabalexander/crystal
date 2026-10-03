//! The TUI, run as `crystal` with no command: a sidebar with every session,
//! the selected one live in a pane beside it, and up to two more split off
//! into panes of their own.
//!
//! Everything that happens arrives as an [`Event`] on one channel: a key,
//! the mouse, a resize, output from the session in the pane, a fresh
//! session list. The loop takes each event, updates the state, and draws.

mod app;
mod command_line;
mod diff;
mod diff_view;
mod finder;
mod fuzzy;
mod groups;
mod help;
mod issues;
mod launcher;
mod memory_view;
mod mouse;
mod pane;
mod screen_widget;
mod search;
pub(crate) mod sidebar;
mod status;
mod text_area;
mod text_input;
mod theme;
mod ui;

use crate::config::Config;
use crate::github::{self, Issue, PullRequest};
use crate::memory::{self, Listed, Memory};
use crate::protocol::{Request, Response, SessionInfo};
use crate::{catalog, keys, typing};
use crate::{client, env, git};
use anyhow::{Result, bail};
use app::{Action, App, Focus, Hit, Place, Slot};
use crossterm::event::{Event as TerminalEvent, KeyEvent, KeyEventKind, MouseEvent};
use diff_view::Against;
use pane::Pane;
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use theme::Theme;

/// How often the session list is asked for. The daemon doesn't announce
/// changes, so this is how far behind the list can be.
const POLL_EVERY: Duration = Duration::from_millis(500);

/// How often the working mark turns a quarter, while an agent works. With
/// nothing working, the TUI waits for something to happen instead.
const SPIN_EVERY: Duration = Duration::from_millis(150);

/// How often GitHub is asked again about a project's pull requests. A
/// project seen for the first time is asked about straight away.
const PULL_REQUESTS_EVERY: Duration = Duration::from_secs(60);

pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// Text pasted into the terminal, whole.
    Paste(String),
    /// The models Codex lets the user choose.
    CodexModels(Vec<String>),
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
    /// What GitHub said about the open pull requests of a project.
    PullRequests {
        project: PathBuf,
        found: Result<Vec<PullRequest>, String>,
    },
    /// What GitHub said about the open issues of a project.
    Issues {
        project: PathBuf,
        found: Result<Vec<Issue>, String>,
    },
    /// The text of one of a project's issues.
    IssueBody {
        project: PathBuf,
        number: u64,
        body: Result<String, String>,
    },
    /// Something to tell the user, from work done off the loop.
    Notice(String),
    /// A worktree's diff, read for the diff view.
    DiffRead {
        dir: PathBuf,
        against: Against,
        read: Result<diff_view::Read, String>,
    },
    /// A worktree's files, listed for the file finder.
    FilesRead {
        dir: PathBuf,
        files: Result<Vec<String>, String>,
    },
    /// The first lines of a file, for the file finder's preview.
    PreviewRead {
        dir: PathBuf,
        path: String,
        lines: Result<Vec<String>, String>,
    },
    /// A project's memory, read for the memory view.
    MemoryRead {
        dir: PathBuf,
        read: Result<Vec<Listed>, String>,
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
    let projects = Arc::new(Mutex::new(Vec::new()));
    spawn_pull_request_poller(projects.clone(), sender.clone());

    let mut tui = Tui {
        socket: socket.to_path_buf(),
        app: App::new(env::own_session_id(socket)),
        panes: Vec::new(),
        last_pane_id: 0,
        events: sender,
        screen: Rect::default(),
        projects,
        theme: Theme::from_env(config.theme),
        started: Instant::now(),
        memory_path: launcher::memory_path(socket),
        quitting: false,
    };
    tui.app.set_agents(catalog::installed());
    tui.app.set_launch_settings(&config);
    tui.app.set_memory_on(memory::enabled(&config));
    tui.app.set_memory(launcher::load_memory(&tui.memory_path));
    tui.set_sessions(sessions);

    let mut terminal = ratatui::try_init()?;
    let result = tui.run_with_modes(&mut terminal, events);
    ratatui::restore();
    result
}

/// The terminal sending the TUI what the mouse does, and pastes marked as
/// pastes, for as long as this lives. However the TUI ends, by returning,
/// failing or panicking, both go back to how the terminal had them: left
/// on, a shell would fill with the sequences the terminal sends for them.
struct TerminalModes;

impl TerminalModes {
    fn on() -> Result<TerminalModes> {
        // A panic on any thread turns them off before the panic is shown.
        let shown_before = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            modes_off();
            shown_before(info);
        }));
        // Clicks and the wheel (1000), drags (1002), written the SGR way
        // (1006). Not the mouse just moving (1003): nothing here needs it,
        // and it would wake the TUI at every move. Then bracketed paste
        // (2004): a paste comes whole, its lines kept, not as typed keys.
        let mut out = std::io::stdout();
        out.write_all(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?2004h")?;
        out.flush()?;
        Ok(TerminalModes)
    }
}

impl Drop for TerminalModes {
    fn drop(&mut self) {
        modes_off();
    }
}

fn modes_off() {
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?1000l");
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
    /// The projects the sessions are in, for the thread that asks GitHub
    /// about their pull requests.
    projects: Arc<Mutex<Vec<PathBuf>>>,
    theme: Theme,
    /// When the TUI started: the working mark turns with the time since.
    started: Instant,
    /// Where the new-session panel's memory is kept.
    memory_path: PathBuf,
    quitting: bool,
}

impl Tui {
    fn run_with_modes(
        &mut self,
        terminal: &mut DefaultTerminal,
        events: Receiver<Event>,
    ) -> Result<()> {
        // The mouse and pastes are the TUI's for as long as `_modes` lives:
        // to the end of this function, however it ends.
        let _modes = TerminalModes::on()?;
        self.run(terminal, events)
    }

    fn run(&mut self, terminal: &mut DefaultTerminal, events: Receiver<Event>) -> Result<()> {
        while !self.quitting {
            let size = terminal.size()?;
            self.screen = Rect::new(0, 0, size.width, size.height);
            let areas = ui::Areas::new(self.screen, self.app.splits().len());
            self.sync_panes(&areas);
            if let Some(view) = self.app.view() {
                let parts = ui::view_areas(view, areas.main);
                let size = |area: Rect| (area.height, area.width);
                self.app
                    .set_view_size(size(parts.list), size(parts.content));
            }
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
            self.fetch_issue_body();
        }
        Ok(())
    }

    /// Takes a fresh list of sessions, and tells the pull request poller
    /// which projects they're in.
    fn set_sessions(&mut self, sessions: Vec<SessionInfo>) {
        self.app.set_sessions(sessions);
        *self.projects.lock().unwrap() = self.app.projects();
    }

    /// Asks GitHub, off the loop, for the text of the issue the issues
    /// view's bar is on, the first time the bar is on it.
    fn fetch_issue_body(&mut self) {
        let Some((project, number)) = self.app.issue_body_to_fetch() else {
            return;
        };
        let events = self.events.clone();
        thread::spawn(move || {
            let body = github::issue_body(&project, number);
            let _ = events.send(Event::IssueBody {
                project,
                number,
                body,
            });
        });
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
            Event::Paste(text) => {
                if let Some(action) = self.app.on_paste(text) {
                    self.carry_out(action);
                }
            }
            Event::CodexModels(models) => self.app.set_codex_models(models),
            Event::Resize => {}
            Event::Sessions(sessions) => self.set_sessions(sessions),
            Event::PullRequests { project, found } => self.app.set_pull_requests(project, found),
            Event::Issues { project, found } => self.app.set_issues(&project, found),
            Event::IssueBody {
                project,
                number,
                body,
            } => self.app.set_issue_body(&project, number, body),
            Event::Notice(notice) => self.app.notify(notice),
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
            Event::DiffRead { dir, against, read } => self.app.diff_read(&dir, against, read),
            Event::FilesRead { dir, files } => {
                if let Some(action) = self.app.files_read(&dir, files) {
                    self.carry_out(action);
                }
            }
            Event::PreviewRead { dir, path, lines } => self.app.preview_read(&dir, &path, lines),
            Event::MemoryRead { dir, read } => self.app.memory_read(&dir, read),
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        if let Some(action) = self.app.on_key(key) {
            self.carry_out(action);
        }
    }

    /// Performs `action`. One that fails, say because its session has just
    /// gone, says why at the bottom rather than closing the TUI.
    fn carry_out(&mut self, action: Action) {
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
        if let Some(action) = self.app.on_mouse(mouse.kind, hit) {
            self.carry_out(action);
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
                self.start_session(None, cwd, command)?;
                launcher::save_memory(&self.memory_path, self.app.memory());
            }
            Action::Paste { to, text } => {
                if let Some(pane) = self.pane_in(to) {
                    pane.send_keys(&pasted(&text, pane.wants_paste_marked()));
                }
            }
            Action::ReadCodexModels => {
                self.read_in_background(|| Event::CodexModels(read_codex_models()));
            }
            Action::ReadDiff { dir, against } => {
                self.read_in_background(move || {
                    let read = diff_view::read(&dir, against);
                    Event::DiffRead { dir, against, read }
                });
            }
            Action::ReadFiles(dir) => {
                self.read_in_background(move || {
                    let files = finder::read_files(&dir);
                    Event::FilesRead { dir, files }
                });
            }
            Action::ReadPreview { dir, path } => {
                self.read_in_background(move || {
                    let lines = finder::read_preview(&dir, &path);
                    Event::PreviewRead { dir, path, lines }
                });
            }
            Action::ReadMemory(dir) => {
                let socket = self.socket.clone();
                self.read_in_background(move || {
                    let read = read_memory(&socket, &dir);
                    Event::MemoryRead { dir, read }
                });
            }
            Action::ForgetMemory { dir, id } => {
                let socket = self.socket.clone();
                self.read_in_background(move || {
                    let project = memory::project_of(&dir);
                    match memory::remove(&socket, &project, id) {
                        Ok(_) => Event::MemoryRead {
                            read: read_memory(&socket, &dir),
                            dir,
                        },
                        Err(err) => Event::Notice(format!("{err:#}")),
                    }
                });
            }
            Action::PromoteMemory { dir, id } => {
                let socket = self.socket.clone();
                self.read_in_background(move || match promote_memory(&socket, &dir, id) {
                    Ok(file) => {
                        let file = crate::shell::home_relative(&file);
                        Event::Notice(format!("added entry {id} to {file}"))
                    }
                    Err(err) => Event::Notice(format!("{err:#}")),
                });
            }
            Action::Edit { dir, path, name } => {
                let mut command = editor()?;
                command.push(path);
                self.start_session(Some(name), dir, command)?;
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
            Action::OpenPullRequest { project, number } => {
                // gh goes over the network: off the loop, saying only what
                // went wrong.
                let events = self.events.clone();
                thread::spawn(move || {
                    if let Err(reason) = github::open_pull_request(&project, number) {
                        let _ = events.send(Event::Notice(reason));
                    }
                });
            }
            Action::ListIssues(project) => {
                let events = self.events.clone();
                thread::spawn(move || {
                    let found = github::issues(&project);
                    let _ = events.send(Event::Issues { project, found });
                });
            }
        }
        Ok(())
    }

    /// Starts `command` in a new session in `cwd`, or the user's shell when
    /// it's empty, called `name` or after its program, then selects the
    /// session and hands it the keyboard.
    fn start_session(
        &mut self,
        name: Option<String>,
        cwd: PathBuf,
        command: Vec<String>,
    ) -> Result<()> {
        let name = client::new_session(&self.socket, name, cwd, command)?;
        self.refresh_sessions()?;
        self.app.select(&name);
        self.app.type_into_selected();
        Ok(())
    }

    /// Runs `read` on a thread of its own, since git and the disk can keep
    /// it a while, and hands what it read back to the loop as an event.
    fn read_in_background(&self, read: impl FnOnce() -> Event + Send + 'static) {
        let events = self.events.clone();
        thread::spawn(move || {
            let _ = events.send(read());
        });
    }

    /// Asks for the list now, rather than waiting for the next poll, so a
    /// key's effect shows straight away.
    fn refresh_sessions(&mut self) -> Result<()> {
        let sessions = list_sessions(&self.socket, false)?;
        self.set_sessions(sessions);
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

/// The bytes that hand `text`, pasted, to a program: marked as a paste
/// when the program asked for that. A program that didn't gets a newline
/// the way a terminal sends it for the Enter key.
fn pasted(text: &str, marked: bool) -> Vec<u8> {
    if marked {
        typing::keystrokes(text, true)
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// The memory of the project `dir` is in, for the memory view.
fn read_memory(socket: &Path, dir: &Path) -> Result<Vec<Listed>, String> {
    let project = memory::project_of(dir);
    match Memory::read(socket, &project) {
        Ok(memory) => Ok(memory.listed()),
        Err(err) => Err(format!("{err:#}")),
    }
}

/// Writes entry `id` of the memory of the project `dir` is in into its
/// CLAUDE.md or AGENTS.md, and returns which.
fn promote_memory(socket: &Path, dir: &Path, id: u64) -> Result<PathBuf> {
    let project = memory::project_of(dir);
    let memory = Memory::read(socket, &project)?;
    let Some(entry) = memory.get(id) else {
        bail!("there's no entry {id}");
    };
    memory::promote(&project, entry)
}

/// The models Codex lets the user choose, as `codex debug models` lists
/// them, or none when it can't say.
fn read_codex_models() -> Vec<String> {
    let output = std::process::Command::new("codex")
        .args(["debug", "models"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match output {
        Ok(output) if output.status.success() => {
            catalog::codex_models(&String::from_utf8_lossy(&output.stdout))
        }
        _ => Vec::new(),
    }
}

/// The user's editor, as a command line to put a file's path after:
/// `$EDITOR`, which may carry its own arguments, like `code --wait`, or
/// else `vi`.
fn editor() -> Result<Vec<String>> {
    let editor = std::env::var("EDITOR").unwrap_or_default();
    let editor = if editor.trim().is_empty() {
        "vi".to_string()
    } else {
        editor
    };
    let command = command_line::parse(&editor).map_err(|err| anyhow::anyhow!("$EDITOR: {err}"))?;
    if command.is_empty() {
        bail!("$EDITOR is empty");
    }
    Ok(command)
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
                TerminalEvent::Paste(text) => Event::Paste(text),
                TerminalEvent::Resize(..) => Event::Resize,
                _ => continue,
            };
            if events.send(event).is_err() {
                return;
            }
        }
    });
}

/// Asks GitHub about the open pull requests of each project the sessions
/// are in, on a thread of its own, since gh can take seconds to answer: a
/// project as soon as it's seen, and every one again each
/// [`PULL_REQUESTS_EVERY`].
fn spawn_pull_request_poller(projects: Arc<Mutex<Vec<PathBuf>>>, events: Sender<Event>) {
    thread::spawn(move || {
        let mut asked: HashMap<PathBuf, Instant> = HashMap::new();
        loop {
            let wanted = projects.lock().unwrap().clone();
            for project in wanted {
                let due = asked
                    .get(&project)
                    .is_none_or(|at| at.elapsed() >= PULL_REQUESTS_EVERY);
                if !due {
                    continue;
                }
                asked.insert(project.clone(), Instant::now());
                let found = github::pull_requests(&project);
                if events.send(Event::PullRequests { project, found }).is_err() {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(500));
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
