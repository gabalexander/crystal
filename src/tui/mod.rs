//! The TUI, run as `crystal` with no command: a sidebar with every session,
//! the selected one live in a pane beside it, and up to two more split off
//! into panes of their own.
//!
//! Everything that happens arrives as an [`Event`] on one channel: a key,
//! the mouse, a resize, output from the session in the pane, a fresh
//! session list. The loop takes each event, updates the state, and draws.

mod app;
mod backlog_view;
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
mod plugins_view;
mod profiles;
mod screen_widget;
mod search;
pub(crate) mod sidebar;
mod status;
mod text_area;
mod text_input;
mod theme;
mod ui;

use crate::config::{self, Config};
use crate::github::{self, Issue, PullRequest};
use crate::memory::{self, Listed, Memory};
use crate::plugins::{self, Context};
use crate::profile;
use crate::protocol::{Backlog, NewSession, Request, Response, SessionInfo};
use crate::{catalog, keys, typing};
use crate::{client, env};
use anyhow::{Context as _, Result, bail};
use app::{Action, App, Focus, Hit, Place, PluginKey, PluginPane, Slot};
use backlog_view::BacklogChange;
use crossterm::event::{Event as TerminalEvent, KeyEvent, KeyEventKind, MouseEvent};
use diff_view::Against;
use pane::Pane;
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// The backlog of the project `dir` is in, for the backlog view.
    Backlog {
        dir: PathBuf,
        found: Result<Backlog, String>,
    },
    /// How many backlog items each project has to do.
    BacklogCounts(HashMap<PathBuf, usize>),
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
    let count_backlog = Arc::new(AtomicBool::new(crate::backlog::enabled(&config)));
    spawn_session_poller(socket.to_path_buf(), sender.clone(), count_backlog.clone());
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
        overlay: None,
        count_backlog,
    };
    tui.app.set_agents(catalog::installed());
    tui.app.set_launch_settings(&config);
    tui.app.set_features(&config);
    tui.app.set_plugin_keys(plugin_keys(&config));
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
    /// The viewer of the session of a plugin's pane, while one is open.
    overlay: Option<Pane>,
    /// Whether the session poller asks how many backlog items each project
    /// has, which follows the backlog plugin being switched.
    count_backlog: Arc<AtomicBool>,
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
            if let Some(overlay) = &mut self.overlay {
                let screen = ui::plugin_pane_screen(&areas);
                let size = (screen.height.max(1), screen.width.max(1));
                if overlay.size() != size {
                    overlay.resize(size.0, size.1);
                }
            }
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
            let overlay = self.overlay.as_ref();
            terminal.draw(|frame| ui::draw(frame, &self.app, &self.panes, overlay, &look))?;

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
        // With the github plugin off, the poller has nothing to ask about.
        let asked = if self.app.github_on() {
            self.app.projects()
        } else {
            Vec::new()
        };
        *self.projects.lock().unwrap() = asked;
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
                // A plugin's pane closes as its program ends.
                if self
                    .overlay
                    .as_ref()
                    .is_some_and(|overlay| overlay.id == pane)
                {
                    self.close_plugin_pane();
                } else if let Some(pane) = self.pane_with_id(pane) {
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
            Event::Backlog { dir, found } => self.app.set_backlog(&dir, found),
            Event::BacklogCounts(counts) => self.app.set_backlog_counts(counts),
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
        // A plugin's pane takes the keyboard, not the mouse.
        if self.overlay.is_some() {
            return;
        }
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
            Action::Start {
                place,
                command,
                purpose,
            } => {
                let cwd = directory_for(&self.socket, place)?;
                let name = client::new_session_for(&self.socket, None, cwd, command, purpose)?;
                self.show_new_session(&name)?;
                launcher::save_memory(&self.memory_path, self.app.memory());
            }
            Action::StartInBackground {
                place,
                spec,
                backlog,
            } => {
                let cwd = directory_for(&self.socket, place)?;
                let name = client::new_task(&self.socket, None, cwd, spec, backlog)?;
                // A background task takes no keys: the sidebar keeps them.
                self.refresh_sessions()?;
                self.app.select(&name);
                launcher::save_memory(&self.memory_path, self.app.memory());
            }
            Action::CloseTask {
                name,
                failed,
                summary,
            } => {
                client::close_task(&self.socket, &name, failed, &summary)?;
                self.refresh_sessions()?;
            }
            Action::ListBacklog(dir) => self.list_backlog(dir),
            Action::ChangeBacklog { dir, change } => {
                let request = match change {
                    BacklogChange::Add(text) => Request::BacklogAdd {
                        dir: dir.clone(),
                        text,
                        tags: Vec::new(),
                    },
                    BacklogChange::Mark { number, done } => Request::BacklogMark {
                        dir: dir.clone(),
                        number,
                        done,
                    },
                    BacklogChange::Remove(number) => Request::BacklogRemove {
                        dir: dir.clone(),
                        number,
                    },
                };
                client::ask(&self.socket, &request, false)?;
                self.list_backlog(dir);
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
            Action::SaveProfile { replacing, profile } => {
                let saved = profile::save(&config::path(), replacing.as_deref(), &profile);
                self.profiles_changed(saved, Some(&profile.name));
            }
            Action::DeleteProfile(name) => {
                let deleted = profile::delete(&config::path(), &name);
                self.profiles_changed(deleted, None);
            }
            Action::ListPlugins => {
                let config = Config::load()?;
                self.app.show_plugins(listed_plugins(&config, &self.socket));
            }
            Action::SwitchPlugin { name, on } => {
                let path = config::path();
                let switched = plugins::set_enabled(&path, &self.socket, &name, on)
                    .and_then(|()| Config::load());
                match switched {
                    Ok(config) => self.plugins_changed(&config),
                    Err(err) => self.app.plugin_failed(format!("{err:#}")),
                }
            }
            Action::RunPlugin {
                plugin,
                action,
                context,
            } => self.run_plugin(&plugin, &action, context)?,
            Action::OpenPluginPane {
                plugin,
                pane,
                context,
            } => self.open_plugin_pane(&plugin, &pane, context)?,
            Action::TypeInPluginPane(key) => {
                if let Some(pane) = &mut self.overlay {
                    let application_cursor = pane.screen.screen().application_cursor();
                    if let Some(bytes) = keys::encode(&key, application_cursor) {
                        pane.send_keys(&bytes);
                    }
                }
            }
            Action::PasteInPluginPane(text) => {
                if let Some(pane) = &mut self.overlay {
                    pane.send_keys(&pasted(&text, pane.wants_paste_marked()));
                }
            }
            Action::ClosePluginPane => self.close_plugin_pane(),
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
        self.show_new_session(&name)
    }

    /// Selects the session just started, called `name`, and hands it the
    /// keyboard.
    fn show_new_session(&mut self, name: &str) -> Result<()> {
        self.refresh_sessions()?;
        self.app.select(name);
        self.app.type_into_selected();
        Ok(())
    }

    /// After a profile was written to the config file or taken out of it:
    /// reads the file again, so the panel and the profiles view show what
    /// it now says, or has the view say why the file wasn't changed.
    fn profiles_changed(&mut self, changed: Result<()>, select: Option<&str>) {
        match changed.and_then(|()| Config::load()) {
            Ok(config) => self.app.profiles_saved(&config, select),
            Err(error) => self.app.profile_failed(format!("{error:#}")),
        }
    }

    /// After a plugin was switched on or off: everything that shows what
    /// the plugins add follows what the config file now says.
    fn plugins_changed(&mut self, config: &Config) {
        self.app.set_features(config);
        self.app.set_plugin_keys(plugin_keys(config));
        self.app.show_plugins(listed_plugins(config, &self.socket));
        let backlog = crate::backlog::enabled(config);
        self.count_backlog.store(backlog, Ordering::Relaxed);
        self.set_sessions(self.app.sessions().to_vec());
    }

    /// Runs one of a plugin's actions, off the loop, with what it prints in
    /// the plugin's log, and says how it went at the bottom.
    fn run_plugin(&mut self, plugin: &str, action: &str, context: Context) -> Result<()> {
        plugins::ensure_enabled(&Config::load()?, plugin)?;
        let (dir, manifest) = installed_plugin(plugin)?;
        let action = manifest
            .actions
            .into_iter()
            .find(|candidate| candidate.id == action)
            .with_context(|| format!("{plugin} has no action {action}"))?;
        let context = placed(context)?;
        plugins::log(
            &self.socket,
            plugin,
            &format!("{}: {}", action.id, action.command.join(" ")),
        );
        let log = plugins::open_log(&self.socket, plugin)?;
        let mut child = plugins::command(&dir, &action.command, &self.socket, &context)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .with_context(|| format!("couldn't run {}", action.command.join(" ")))?;
        let what = format!("{plugin}: {}", action.title);
        let plugin = plugin.to_string();
        let events = self.events.clone();
        thread::spawn(move || {
            let notice = match child.wait() {
                Ok(status) if status.success() => format!("ran {what}"),
                Ok(status) => format!("{what} failed ({status}): `crystal plugin log {plugin}`"),
                Err(err) => format!("{what}: {err}"),
            };
            let _ = events.send(Event::Notice(notice));
        });
        Ok(())
    }

    /// Starts one of a plugin's panes in a session of its own, and shows it
    /// over the panes, with the keyboard.
    fn open_plugin_pane(&mut self, plugin: &str, pane: &str, context: Context) -> Result<()> {
        plugins::ensure_enabled(&Config::load()?, plugin)?;
        let (dir, manifest) = installed_plugin(plugin)?;
        let spec = manifest
            .panes
            .into_iter()
            .find(|candidate| candidate.id == pane)
            .with_context(|| format!("{plugin} has no pane {pane}"))?;
        let context = placed(context)?;
        let mut env = env::current();
        for (key, said) in plugins::env(&self.socket, &context) {
            match said {
                Some(value) => env.insert(key.to_string(), value),
                None => env.remove(key),
            };
        }
        let taken = list_sessions(&self.socket, false)?;
        let name = free_name(&format!("{plugin}-{pane}"), &taken);
        let request = Request::New(NewSession {
            name: Some(name),
            cwd: dir.clone(),
            command: plugins::argv(&dir, &spec.command),
            env,
            task: None,
            backlog: None,
        });
        let Some(Response::Created { name }) = client::ask(&self.socket, &request, true)? else {
            bail!("the daemon didn't start {plugin}'s pane");
        };
        let areas = ui::Areas::new(self.screen, self.app.splits().len());
        let screen = ui::plugin_pane_screen(&areas);
        self.last_pane_id += 1;
        let (id, events) = (self.last_pane_id, self.events.clone());
        let rows = screen.height.max(1);
        let cols = screen.width.max(1);
        self.overlay = Some(Pane::open(&self.socket, &name, rows, cols, id, events)?);
        self.app.plugin_pane_opened(PluginPane {
            plugin: plugin.to_string(),
            title: spec.title,
            session: name,
        });
        self.refresh_sessions()
    }

    /// Closes the plugin's pane that's open, and ends its session, which
    /// was only ever the pane's.
    fn close_plugin_pane(&mut self) {
        self.overlay = None;
        let Some(pane) = self.app.plugin_pane().cloned() else {
            return;
        };
        self.app.plugin_pane_closed();
        let kill = Request::Kill { name: pane.session };
        if let Err(err) = client::ask(&self.socket, &kill, false) {
            self.app.notify(format!("{err:#}"));
        }
        let _ = self.refresh_sessions();
    }

    /// Asks the daemon, off the loop, for the backlog of the project `dir`
    /// is in, done items too.
    fn list_backlog(&self, dir: PathBuf) {
        let socket = self.socket.clone();
        self.read_in_background(move || {
            let found =
                client::backlog(&socket, dir.clone(), true).map_err(|err| format!("{err:#}"));
            Event::Backlog { dir, found }
        });
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
        self.panes
            .iter_mut()
            .chain(self.overlay.as_mut())
            .find(|pane| pane.id == id)
    }
}

/// The plugins as the plugins view lists them: crystal's own, then the
/// installed ones, each with whether it's on and what keeps it from
/// running.
fn listed_plugins(config: &Config, socket: &Path) -> Vec<plugins_view::Listed> {
    let own = plugins::BUILT_IN.iter().map(|plugin| plugins_view::Listed {
        name: plugin.name.to_string(),
        description: plugin.description.to_string(),
        built_in: true,
        on: plugins::enabled(config, plugin.name),
        trouble: None,
        actions: Vec::new(),
        panes: Vec::new(),
    });
    let installed = plugins::installed().into_iter().map(|plugin| {
        let on = plugins::enabled(config, &plugin.name);
        let paused = plugins::paused(socket, &plugin.name).filter(|_| on);
        let item = |id: &str, title: &str, key: Option<&String>| plugins_view::Item {
            id: id.to_string(),
            title: title.to_string(),
            key: key.cloned(),
        };
        match plugin.manifest {
            Ok(manifest) => plugins_view::Listed {
                name: plugin.name,
                description: manifest.description,
                built_in: false,
                on,
                trouble: paused.map(|_| "paused after failing: space off and on again".to_string()),
                actions: (manifest.actions.iter())
                    .map(|action| item(&action.id, &action.title, action.key.as_ref()))
                    .collect(),
                panes: (manifest.panes.iter())
                    .map(|pane| item(&pane.id, &pane.title, None))
                    .collect(),
            },
            Err(why) => plugins_view::Listed {
                name: plugin.name,
                description: String::new(),
                built_in: false,
                on,
                trouble: Some(why),
                actions: Vec::new(),
                panes: Vec::new(),
            },
        }
    });
    own.chain(installed).collect()
}

/// The sidebar keys taken by the actions of the installed plugins that are
/// on. Installing or switching on a plugin refuses a key another has, so
/// where two plugins' files were changed to share one, the first by name
/// keeps it.
fn plugin_keys(config: &Config) -> Vec<PluginKey> {
    let mut keys: Vec<PluginKey> = Vec::new();
    let on = plugins::installed()
        .into_iter()
        .filter(|plugin| plugins::enabled(config, &plugin.name));
    for plugin in on {
        let Ok(manifest) = plugin.manifest else {
            continue;
        };
        for action in manifest.actions {
            let Some(key) = action.key.as_ref().and_then(|key| key.chars().next()) else {
                continue;
            };
            if keys.iter().all(|taken| taken.key != key) {
                keys.push(PluginKey {
                    key,
                    plugin: plugin.name.clone(),
                    action: action.id,
                    title: action.title,
                });
            }
        }
    }
    keys
}

/// The installed plugin called `name`: its directory and manifest.
fn installed_plugin(name: &str) -> Result<(PathBuf, crate::plugin_manifest::Manifest)> {
    let plugin = plugins::find(name).with_context(|| format!("there's no plugin called {name}"))?;
    let manifest = plugin
        .manifest
        .map_err(|why| anyhow::anyhow!("{name}'s plugin.toml: {why}"))?;
    Ok((plugin.dir, manifest))
}

/// `context`, or, with no session selected to say where, the TUI's own
/// directory.
fn placed(context: Context) -> Result<Context> {
    if context.worktree.is_some() {
        return Ok(context);
    }
    Ok(Context::of_dir(&std::env::current_dir()?))
}

/// `base`, or `base-2`, `base-3`, …, whichever no session has yet.
fn free_name(base: &str, sessions: &[SessionInfo]) -> String {
    let taken = |name: &str| sessions.iter().any(|session| session.name == name);
    let mut name = base.to_string();
    let mut number = 1;
    while taken(&name) {
        number += 1;
        name = format!("{base}-{number}");
    }
    name
}

/// The directory a new session at `place` starts in, making the worktree
/// first when it's a new one. Where a place says nothing, it's the TUI's
/// own directory.
fn directory_for(socket: &Path, place: Place) -> Result<PathBuf> {
    match place {
        Place::Directory(Some(dir)) => Ok(dir),
        Place::Directory(None) => Ok(std::env::current_dir()?),
        Place::NewWorktree { branch, base } => {
            let base = match base {
                Some(base) => base,
                None => std::env::current_dir()?,
            };
            client::add_worktree(socket, &base, &branch)
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

/// Asks for the session list every [`POLL_EVERY`], and with
/// `count_backlog`, how many backlog items each of their projects has to
/// do.
fn spawn_session_poller(socket: PathBuf, events: Sender<Event>, count_backlog: Arc<AtomicBool>) {
    thread::spawn(move || {
        loop {
            thread::sleep(POLL_EVERY);
            // A daemon that has gone away has no sessions left.
            let sessions = list_sessions(&socket, false).unwrap_or_default();
            let projects = projects_of(&sessions);
            if events.send(Event::Sessions(sessions)).is_err() {
                return;
            }
            if count_backlog.load(Ordering::Relaxed)
                && let Some(counts) = backlog_counts(&socket, projects)
                && events.send(Event::BacklogCounts(counts)).is_err()
            {
                return;
            }
        }
    });
}

/// The projects `sessions` are in, by their main worktrees.
fn projects_of(sessions: &[SessionInfo]) -> Vec<PathBuf> {
    let mut projects: Vec<PathBuf> = sessions
        .iter()
        .filter_map(|session| session.worktree.as_ref())
        .map(|worktree| worktree.project_path.clone())
        .collect();
    projects.sort();
    projects.dedup();
    projects
}

/// How many backlog items each of `projects` has to do, or `None` when
/// the daemon can't say.
fn backlog_counts(socket: &Path, projects: Vec<PathBuf>) -> Option<HashMap<PathBuf, usize>> {
    match client::ask(socket, &Request::BacklogCounts { projects }, false) {
        Ok(Some(Response::BacklogCounts { open })) => Some(open.into_iter().collect()),
        _ => None,
    }
}
