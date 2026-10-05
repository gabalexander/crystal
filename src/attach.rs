//! `crystal attach`: a session fills your terminal until you detach with
//! Ctrl+\ or its program ends.
//!
//! What the session writes is drawn through a screen of our own rather than
//! passed straight through, so whatever the program does to its terminal,
//! like switching screens, stays inside the attach. Your terminal is asked
//! for what the program asked of its keyboard and mouse, the wheel sending
//! arrow keys only while the program is on its alternate screen, though
//! the attach is on your terminal's all along. A few keys are the attach's,
//! as they are in the TUI's panes: the prefix (`Ctrl+B`) then copy mode's
//! key (`v`) puts the screen in [copy mode](crate::tui::copy_mode), its
//! search typed on the bottom row, and the prefix then `PageUp` or
//! `PageDown`, or those with `Shift`, page through the history until you
//! type. With `[mouse] attach_capture`, the attach takes the mouse from
//! your terminal instead: a program that asked for it gets it, written its
//! way; anywhere else a drag selects, copied as you let go or held in copy
//! mode, and the wheel sends a program on its alternate screen that didn't
//! ask the arrow keys, and scrolls the session's history otherwise. Its
//! bell is passed on to your terminal, as often as [`crate::bell`] lets it,
//! and what it copies goes on your clipboard, as [`crate::clipboard`] puts
//! it there.

use crate::bell::Ringer;
use crate::client;
use crate::clipboard;
use crate::config::Config;
use crate::env;
use crate::keys::{self, Typed};
use crate::links;
use crate::output::outln;
use crate::plugins::{self, Context, Id};
use crate::protocol::{Request, Response, State};
use crate::tui::copy_mode::{CopyMode, Outcome, SearchPrompt};
use crate::tui::keymap::{Bound, Chord, Command, Keymap};
use crate::tui::mouse;
use crate::tui::pane::{self, Clicks};
use crate::tui::screen_widget::ScreenWidget;
use crate::viewer::{Output, Viewer};
use crate::vt;
use anyhow::{Result, bail};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use std::fs::File;
use std::io::{self, ErrorKind, IsTerminal, Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How long the input loop waits on the keyboard before it checks the
/// terminal's size and whether the session is still there, and scrolls
/// under a drag held on the top or bottom row.
const TICK: Duration = Duration::from_millis(50);

/// How long the start of a mouse report at the end of what was read waits
/// for the rest of it, before it's taken for keys after all.
const HELD: Duration = Duration::from_millis(20);

/// Puts back every mode a session may have turned on, pops the Kitty
/// keyboard flags the attach pushed, then leaves the alternate screen. The
/// wheel's alternate scroll is turned back on, as most terminals have it,
/// then put back as [`ENTER`] saved it, in a terminal that saves modes.
const RESET: &[u8] = b"\x1b[<u\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[?2004l\x1b[?1004l\
\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1016l\
\x1b[?1007h\x1b[?1007r\x1b[?1049l";

/// Saves your terminal's alternate scroll (XTSAVE) and turns it off, since
/// a session starts on the main screen, then goes to the alternate screen,
/// clears it, and pushes an entry of the attach's own on the terminal's
/// stack of Kitty keyboard flags, for the session's flags to go in.
const ENTER: &[u8] = b"\x1b[?1007s\x1b[?1007l\x1b[?1049h\x1b[H\x1b[2J\x1b[>0u";

pub fn run(socket: &Path, name: Option<&str>) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("attach needs a terminal");
    }
    // A config that can't be read is for the TUI to say; attaching goes on.
    let config = Config::load().unwrap_or_default();
    vt::set_history_lines(config.scrollback_lines);
    let (cols, rows) = terminal::size()?;
    // The history from before, for copy mode and the wheel to go back
    // through: your terminal's own scrolling can't reach it.
    let (mut viewer, mut output) = Viewer::connect(socket, name, (rows, cols), true)?;
    // One crystal stopped as it sat idle starts again where it was, as
    // going to it in the TUI has it do.
    if !viewer.running && stopped_idle(socket, &viewer.id) {
        client::respawn(socket, &viewer.name)?;
        (viewer, output) = Viewer::connect(socket, Some(&viewer.name), (rows, cols), true)?;
    }
    let name = viewer.name.clone();
    if env::own_session_id(socket).as_deref() == Some(viewer.id.as_str()) {
        bail!("can't attach {name} to itself");
    }

    if !viewer.running {
        // Nothing more is coming: show how it ended, without taking over
        // the terminal.
        let mut screen = vt::Screen::new(rows, cols);
        for chunk in output {
            screen.process(&chunk);
        }
        print_screen(&screen)?;
        outln!("{}", ending(socket, &name))?;
        return Ok(());
    }

    let options = Options {
        copies: config.clipboard.allow_programs && !in_background(socket, &viewer.id),
        wheel: config
            .mouse
            .attach_capture
            .then_some(config.mouse.scroll_lines),
        copy_on_select: config.mouse.copy_on_select,
    };
    // The keys come from the TUI's keymap; one the config gets wrong is
    // for the TUI to say too.
    let keymap = Keymap::new(&config.keys).unwrap_or_default();
    let controls = Controls::new(keymap, options);
    let detached = {
        let _raw = RawTerminal::enter()?;
        relay(socket, viewer, output, (rows, cols), controls)?
    };
    if detached {
        outln!("[detached from {name}]")?;
    } else {
        outln!("{}", ending(socket, &name))?;
    }
    Ok(())
}

/// Whether crystal stopped the session with the id `id` after it sat idle.
fn stopped_idle(socket: &Path, id: &str) -> bool {
    let Ok(Some(Response::Sessions { sessions })) = client::ask(socket, &Request::List, false)
    else {
        return false;
    };
    sessions
        .iter()
        .any(|session| session.id == id && session.stopped_idle)
}

/// Whether the session with the id `id` is a background task, whose screen
/// shows what crystal draws of Claude's work rather than a program's own
/// output.
fn in_background(socket: &Path, id: &str) -> bool {
    let Ok(Some(Response::Sessions { sessions })) = client::ask(socket, &Request::List, false)
    else {
        return false;
    };
    let session = sessions.iter().find(|session| session.id == id);
    let task = session.and_then(|session| session.task.as_ref());
    task.is_some_and(|task| task.background)
}

/// What the settings have the attach do.
#[derive(Debug, Clone, Copy)]
struct Options {
    /// What the session's program copies goes on your clipboard.
    copies: bool,
    /// The attach takes the mouse, and a notch of the wheel is this many
    /// lines; `None` leaves the mouse to your terminal.
    wheel: Option<u16>,
    /// What the mouse selects goes on your clipboard as you let go; or
    /// else it's held in copy mode, for `y`.
    copy_on_select: bool,
}

/// Draws the session and hands what comes from your terminal to
/// `controls`, until the user detaches (`true`) or the session goes
/// (`false`).
fn relay(
    socket: &Path,
    viewer: Viewer,
    output: Output,
    (rows, cols): (u16, u16),
    mut controls: Controls,
) -> Result<bool> {
    let options = controls.options;
    let drawn = Drawn::new(rows, cols, options.wheel.is_some())?;
    let drawn = Arc::new(Mutex::new(drawn));
    let mut viewer = viewer;
    let mut drawing = Drawing::start(&drawn, output, options.copies);

    // Our own handle on the keyboard, unbuffered, so that waiting on it
    // and reading from it agree.
    let keyboard = File::from(io::stdin().as_fd().try_clone_to_owned()?);
    let mut reader = Reader::new(options.wheel.is_some());
    let mut size = (rows, cols);
    let mut buf = [0; 4096];
    let detached = loop {
        if drawing.is_done() {
            drawing.finish();
            // The output ended: the session's program has, or the daemon
            // was handed over to a new crystal, which hangs up on every
            // attach. Attaching again says which.
            let Some((again, output)) = attach_again(socket, &viewer, size) else {
                break false;
            };
            viewer = again;
            drawn.lock().unwrap().shown = Shown::new(size.0, size.1);
            controls.start_over();
            drawing = Drawing::start(&drawn, output, options.copies);
        }
        // The start of a mouse report waits only a moment for its end.
        let wait = if reader.holds() { HELD } else { TICK };
        if readable(&keyboard, wait)? {
            let n = (&keyboard).read(&mut buf)?;
            let inputs = reader.read(&buf[..n]);
            if n == 0 || controls.take(inputs, socket, &viewer, &drawn)? {
                break true;
            }
        } else if let Some(held) = reader.give_up()
            && controls.take(vec![held], socket, &viewer, &drawn)?
        {
            break true;
        }
        if controls.dragging.is_some() {
            let mut drawn = drawn.lock().unwrap();
            if controls.scroll_past_edge(&mut drawn.shown, Instant::now()) {
                drawn.draw()?;
            }
        }
        let (cols, rows) = terminal::size()?;
        if (rows, cols) != size {
            size = (rows, cols);
            let _ = viewer.resize(rows, cols);
            let mut drawn = drawn.lock().unwrap();
            drawn.shown.screen.resize(rows, cols);
            drawn.draw()?;
        }
    };
    // Hanging up ends the drawer's output, and the drawer must be done
    // before the terminal is put back.
    drop(viewer);
    drawing.finish();
    Ok(detached)
}

/// The session `viewer` showed, attached again at `(rows, cols)`, with its
/// history, while its program still runs.
fn attach_again(
    socket: &Path,
    viewer: &Viewer,
    (rows, cols): (u16, u16),
) -> Option<(Viewer, Output)> {
    let name = Some(viewer.name.as_str());
    let (again, output) = Viewer::connect(socket, name, (rows, cols), true).ok()?;
    (again.running && again.id == viewer.id).then_some((again, output))
}

/// What the attach does with your keys and the mouse: the keys it takes
/// for itself, which the TUI's keymap gives, as they work in a pane; the
/// prefix waiting for the key after it; and the mouse, while the attach has
/// it, selecting. Kept apart from I/O: it changes what's [`Shown`], and
/// says what's to be sent, copied or opened.
struct Controls {
    keymap: Keymap,
    options: Options,
    /// The prefix that was pressed, the next key being the attach's.
    prefixed: Option<Chord>,
    /// Whether the last key pressed was the attach's: its release, which a
    /// terminal reports in the Kitty protocol to a program that asks, isn't
    /// the program's either.
    took_last: bool,
    clicks: Clicks<()>,
    /// The drag selecting, while the button is down.
    dragging: Option<Drag>,
}

/// A drag selecting on the screen of the attach.
#[derive(Debug, Clone, Copy)]
struct Drag {
    /// The cell the mouse is on.
    cell: (u16, u16),
    /// Whether it has moved off the cell it went down on.
    moved: bool,
    /// When it last scrolled the history, held on the top or bottom row.
    scrolled: Option<Instant>,
}

/// What became of a key, or a paste.
#[derive(Debug, PartialEq, Eq)]
enum Took {
    /// It's typed into the session: its bytes go there, and the screen
    /// comes back to live.
    Typed,
    /// Its bytes go to the session, but nothing was typed: a key coming
    /// up, or your terminal saying it gained the focus.
    Passed,
    /// The attach took it, and the screen shows what it did.
    Taken,
    /// Nobody takes it.
    Dropped,
    /// Copy mode copies this, and is over.
    Copy(String),
    /// Copy mode opens this link, and is over.
    Open(String),
    /// Ctrl+\: the attach is over.
    Detach,
}

impl Controls {
    fn new(keymap: Keymap, options: Options) -> Controls {
        Controls {
            keymap,
            options,
            prefixed: None,
            took_last: false,
            clicks: Clicks::default(),
            dragging: None,
        }
    }

    /// Forgets what was under way, for a screen that starts again.
    fn start_over(&mut self) {
        self.prefixed = None;
        self.dragging = None;
    }

    /// Hands on what came from your terminal, as [`Controls::key`] and
    /// [`Controls::mouse`] say, sending the session its part and drawing
    /// what changed. Returns whether a Ctrl+\ detaches.
    fn take(
        &mut self,
        inputs: Vec<Input>,
        socket: &Path,
        viewer: &Viewer,
        drawn: &Mutex<Drawn>,
    ) -> io::Result<bool> {
        let mut drawn = drawn.lock().unwrap();
        let shown = &mut drawn.shown;
        // What goes to the session, in the order it came.
        let mut keys = Vec::new();
        let mut changed = false;
        let mut detached = false;
        'inputs: for input in inputs {
            match input {
                Input::Keys(bytes) => {
                    for (typed, range) in keys::decode(&bytes) {
                        match self.key(typed, shown) {
                            Took::Typed => {
                                changed |= shown.back_to_live();
                                keys.extend_from_slice(&bytes[range]);
                            }
                            Took::Passed => keys.extend_from_slice(&bytes[range]),
                            Took::Taken => changed = true,
                            Took::Dropped => {}
                            Took::Copy(text) => {
                                shown.said = Some(copy_to_clipboard(&text));
                                changed = true;
                            }
                            Took::Open(url) => {
                                shown.said = Some(open_link(socket, &viewer.id, url));
                                changed = true;
                            }
                            Took::Detach => {
                                detached = true;
                                break 'inputs;
                            }
                        }
                    }
                }
                Input::Mouse(event) => match self.mouse(event, shown) {
                    Mouse::Keys(bytes) => keys.extend(bytes),
                    Mouse::Changed | Mouse::Select => changed = true,
                    Mouse::Copy(text) => {
                        shown.said = Some(copy_to_clipboard(&text));
                        changed = true;
                    }
                    Mouse::Nothing => {}
                },
            }
        }
        if !keys.is_empty() {
            let _ = viewer.send_keys(&keys);
        }
        if changed {
            drawn.draw()?;
        }
        Ok(detached)
    }

    /// What a key, or a paste, does: Ctrl+\ detaches; in copy mode it's
    /// copy mode's; after the prefix, it's the attach's if it's a key of
    /// copy mode or paging, and the program's if it's the prefix again;
    /// the prefix waits for the key after it, `Shift+PageUp` and
    /// `Shift+PageDown` page, and the rest are the program's.
    fn key(&mut self, typed: Typed, shown: &mut Shown) -> Took {
        let key = match typed {
            Typed::Key(key) => key,
            Typed::Paste(text) => {
                self.prefixed = None;
                if let Some(copy) = &mut shown.copy {
                    copy.on_paste(&mut shown.screen, &text);
                    return Took::Taken;
                }
                return Took::Typed;
            }
            Typed::Other if shown.copy.is_some() => return Took::Dropped,
            Typed::Other => return Took::Passed,
        };
        if key.kind == KeyEventKind::Release {
            let theirs = !self.took_last && self.prefixed.is_none() && shown.copy.is_none();
            return if theirs { Took::Passed } else { Took::Dropped };
        }
        if detaches(&key) {
            return Took::Detach;
        }
        self.took_last = true;
        if shown.copy.is_some() {
            return copy_key(key, shown);
        }
        if let Some(prefix) = self.prefixed.take() {
            shown.said = None;
            return self.after_prefix(prefix, key, shown);
        }
        if let Some(Bound::Command(command)) = self.keymap.direct(&key)
            && run_command(command, shown)
        {
            return Took::Taken;
        }
        if self.keymap.is_prefix(&key) {
            let prefix = Chord::of(&key);
            self.prefixed = Some(prefix);
            shown.said = Some(self.after_prefix_hint(prefix));
            return Took::Taken;
        }
        if key.modifiers == KeyModifiers::SHIFT {
            let command = match key.code {
                KeyCode::PageUp => Some(Command::PageUp),
                KeyCode::PageDown => Some(Command::PageDown),
                _ => None,
            };
            if let Some(command) = command
                && run_command(command, shown)
            {
                return Took::Taken;
            }
        }
        self.took_last = false;
        Took::Typed
    }

    /// The key after the prefix: the prefix again is the program's, and a
    /// key of copy mode or paging the attach's. Any other does nothing, and
    /// says so.
    fn after_prefix(&mut self, prefix: Chord, key: KeyEvent, shown: &mut Shown) -> Took {
        if self.keymap.is_prefix(&key) {
            self.took_last = false;
            return Took::Typed;
        }
        if key.code == KeyCode::Esc {
            return Took::Taken;
        }
        if let Some(Bound::Command(command)) = self.keymap.bound(&key)
            && run_command(command, shown)
        {
            return Took::Taken;
        }
        let (prefix, written) = (prefix.hint(), Chord::of(&key).hint());
        shown.said = Some(format!("{prefix} {written} does nothing in attach"));
        Took::Taken
    }

    /// What the top right says while the prefix waits for its key: the keys
    /// that can follow it.
    fn after_prefix_hint(&self, prefix: Chord) -> String {
        let prefix = prefix.hint();
        let mut hint = format!("{prefix} …");
        for (command, does) in [
            (Command::Copy, "copy mode"),
            (Command::PageUp, "back"),
            (Command::PageDown, "forward"),
        ] {
            if let Some(key) = self.keymap.hint(command) {
                hint.push_str(&format!(" {key} {does} ·"));
            }
        }
        hint.push_str(&format!(" {prefix} sends it"));
        hint
    }

    /// What a mouse event does, while the attach has the mouse: a drag
    /// selecting goes on until the button comes up, the wheel scrolling
    /// the history under it meanwhile; and else it's as [`on_mouse`] says,
    /// a press of the left button starting a selection there, of a word on
    /// a double-click and a line on a triple-click.
    fn mouse(&mut self, event: MouseEvent, shown: &mut Shown) -> Mouse {
        let cell = (event.row, event.column);
        // A click or the wheel puts away what was said, for the top right
        // to say how far back the screen is.
        if matches!(
            event.kind,
            MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            shown.said = None;
        }
        if let Some(drag) = &mut self.dragging {
            match event.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    if cell != drag.cell {
                        drag.moved = true;
                        self.clicks.dragged_to(cell);
                    }
                    drag.cell = cell;
                    shown.select_to(cell);
                    return Mouse::Changed;
                }
                MouseEventKind::Up(_) => {
                    let cell = drag.cell;
                    self.dragging = None;
                    return self.let_go(cell, shown);
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let back = event.kind == MouseEventKind::ScrollUp;
                    scroll(&mut shown.screen, back, lines(self.options));
                    shown.select_to(drag.cell);
                    return Mouse::Changed;
                }
                // The button came up somewhere nothing heard it: this is a
                // new click.
                MouseEventKind::Down(_) => self.dragging = None,
                _ => return Mouse::Nothing,
            }
        }
        let copying = shown.copy.is_some();
        let did = on_mouse(&mut shown.screen, copying, event, lines(self.options));
        if did == Mouse::Select {
            let clicks = self.clicks.click((), cell, Instant::now());
            shown.select_from(cell, clicks);
            self.dragging = Some(Drag {
                cell,
                moved: false,
                scrolled: None,
            });
        }
        did
    }

    /// The mouse let go of what it selected at `cell`: it's copied, or held
    /// in copy mode for `y`, as `copy_on_select` says. In copy mode, it
    /// stays there either way.
    fn let_go(&self, cell: (u16, u16), shown: &mut Shown) -> Mouse {
        if !shown.screen.selecting() {
            return Mouse::Nothing;
        }
        if self.options.copy_on_select {
            return shown
                .screen
                .selected_text()
                .map_or(Mouse::Nothing, Mouse::Copy);
        }
        if shown.copy.is_some() {
            return Mouse::Nothing;
        }
        shown.screen.start_copying_at(cell);
        shown.copy = Some(CopyMode::default());
        Mouse::Changed
    }

    /// A drag selecting that has moved onto the top row, or the bottom one,
    /// scrolls the history that way a row at a time, as often as a pane's
    /// does past its edge, for as long as it's held there, the selection
    /// taken along. Returns whether it scrolled.
    fn scroll_past_edge(&mut self, shown: &mut Shown, now: Instant) -> bool {
        let Some(drag) = &mut self.dragging else {
            return false;
        };
        let (rows, _) = shown.screen.size();
        let rows = if drag.cell.0 == 0 {
            1
        } else if drag.cell.0 >= rows.saturating_sub(1) {
            -1
        } else {
            return false;
        };
        let due = drag.scrolled.is_none_or(|scrolled| {
            now.saturating_duration_since(scrolled) >= pane::EDGE_SCROLL_EVERY
        });
        if !drag.moved || !due {
            return false;
        }
        let was = shown.screen.scrolled_back();
        shown.screen.scroll_back(rows);
        if shown.screen.scrolled_back() == was {
            return false;
        }
        drag.scrolled = Some(now);
        shown.select_to(drag.cell);
        true
    }
}

/// How many lines a notch of the wheel scrolls, while the attach has it.
fn lines(options: Options) -> u16 {
    options.wheel.unwrap_or(0)
}

/// Whether `key` is Ctrl+\, which detaches, written the old way (as
/// Ctrl+4) or in the Kitty protocol.
fn detaches(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('\\' | '4')) && key.modifiers == KeyModifiers::CONTROL
}

/// Runs `command`, one of the TUI's that the attach has: copy mode, and
/// paging through the history. Returns whether it's one of those.
fn run_command(command: Command, shown: &mut Shown) -> bool {
    let back = match command {
        Command::Copy => {
            shown.start_copying();
            return true;
        }
        Command::PageUp => true,
        Command::PageDown => false,
        _ => return false,
    };
    shown.said = None;
    page(&mut shown.screen, back);
    true
}

/// A key in copy mode, which ends it when it copies, opens a link or
/// leaves.
fn copy_key(key: KeyEvent, shown: &mut Shown) -> Took {
    let Some(copy) = &mut shown.copy else {
        return Took::Dropped;
    };
    shown.said = None;
    let took = match copy.on_key(&mut shown.screen, key) {
        Outcome::Stay => return Took::Taken,
        Outcome::Say(said) => {
            shown.said = Some(said);
            return Took::Taken;
        }
        Outcome::Leave => Took::Taken,
        Outcome::Copy(text) => Took::Copy(text),
        Outcome::Open(url) => Took::Open(url),
    };
    shown.stop_copying();
    took
}

/// Moves the screen's view a page back into its history, or toward live:
/// a screenful less a row, so the row at the edge stays in sight.
fn page(screen: &mut vt::Screen, back: bool) {
    let (rows, _) = screen.size();
    scroll(screen, back, rows.saturating_sub(1).max(1));
}

/// Moves the screen's view `lines` back into its history, or toward live.
fn scroll(screen: &mut vt::Screen, back: bool, lines: u16) {
    let rows = usize::from(lines) as isize;
    screen.scroll_back(if back { rows } else { -rows });
}

/// Puts `text` on your clipboard, and says so, or why it couldn't.
fn copy_to_clipboard(text: &str) -> String {
    match clipboard::copy(text) {
        Ok(()) => {
            let lines = text.lines().count().max(1);
            let noun = if lines == 1 { "line" } else { "lines" };
            format!("copied {lines} {noun}")
        }
        Err(err) => format!("couldn't copy: {err:#}"),
    }
}

/// Opens `url`, a link on the screen of the session with the id `id`, as a
/// Ctrl+click in the TUI does: with the action of the first plugin that
/// handles links like it, in the background, or else in your browser. Says
/// which, or why it couldn't.
fn open_link(socket: &Path, id: &str, url: String) -> String {
    let config = Config::load().unwrap_or_default();
    let Some((plugin, action)) = plugins::link_handler(&config, socket, &url) else {
        return links::open(&url).unwrap_or_else(|err| format!("{err:#}"));
    };
    let context = Context {
        link: Some(url),
        ..session_context(socket, id)
    };
    match plugins::start_action(socket, &Id::own(&plugin), &action, &context) {
        Ok((mut child, what)) => {
            // Waited for off the input loop, so it doesn't linger.
            thread::spawn(move || child.wait());
            format!("ran {what}")
        }
        Err(err) => format!("{err:#}"),
    }
}

/// What a plugin is told about the session with the id `id`: where it
/// runs, or the directory the attach runs in, if it's gone.
fn session_context(socket: &Path, id: &str) -> Context {
    if let Ok(Some(Response::Sessions { sessions })) = client::ask(socket, &Request::List, false)
        && let Some(session) = sessions.iter().find(|session| session.id == id)
    {
        return Context::of_session(session);
    }
    Context::of_dir(&std::env::current_dir().unwrap_or_default())
}

/// What a mouse event does on the screen of the attach.
#[derive(Debug, PartialEq, Eq)]
enum Mouse {
    /// It's for the session's program, as these keys.
    Keys(Vec<u8>),
    /// It starts a selection where it went down.
    Select,
    /// It changed what's shown: scrolled the history, or the selection.
    Changed,
    /// What it selected goes on your clipboard.
    Copy(String),
    Nothing,
}

/// What `event` does on `screen`, which fills your terminal: a program that
/// asked for the mouse gets it, written its way, while the screen is live
/// (back in the history, the program's screen isn't what's under the
/// mouse) and out of copy mode, which has the mouse. Otherwise the left
/// button starts a selection, and a notch of the wheel is `lines` presses
/// of an arrow key for a program on its alternate screen, as
/// [`mouse::alternate_scroll`] has it, out of copy mode, or scrolls the
/// history `lines` rows. Nothing else does anything.
fn on_mouse(screen: &mut vt::Screen, copying: bool, event: MouseEvent, lines: u16) -> Mouse {
    let live = screen.scrolled_back() == 0;
    let protocol = mouse::Protocol::of(screen).filter(|_| live && !copying);
    if let Some(protocol) = protocol {
        let cell = (event.row, event.column);
        let keys = mouse::encode(event.kind, event.modifiers, cell, protocol);
        return keys.map_or(Mouse::Nothing, Mouse::Keys);
    }
    let back = match event.kind {
        MouseEventKind::Down(MouseButton::Left) => return Mouse::Select,
        MouseEventKind::ScrollUp => true,
        MouseEventKind::ScrollDown => false,
        _ => return Mouse::Nothing,
    };
    if !copying && let Some(arrows) = mouse::alternate_scroll(screen, back, lines) {
        return Mouse::Keys(arrows);
    }
    let was = screen.scrolled_back();
    scroll(screen, back, lines);
    if screen.scrolled_back() == was {
        Mouse::Nothing
    } else {
        Mouse::Changed
    }
}

/// What came from your terminal: keys for the session, or the mouse.
#[derive(Debug, PartialEq, Eq)]
enum Input {
    Keys(Vec<u8>),
    Mouse(MouseEvent),
}

/// Reads what comes from your terminal into [`Input`]s: all keys, unless
/// the attach takes the mouse, when its reports are picked out of them.
/// The start of one at the end of a read is held for the next to end; if
/// nothing comes, it was keys after all, like an `Esc` on its own.
struct Reader {
    mouse: bool,
    held: Vec<u8>,
}

impl Reader {
    fn new(mouse: bool) -> Reader {
        Reader {
            mouse,
            held: Vec::new(),
        }
    }

    fn read(&mut self, bytes: &[u8]) -> Vec<Input> {
        if !self.mouse {
            return vec![Input::Keys(bytes.to_vec())];
        }
        self.held.extend_from_slice(bytes);
        let (inputs, held) = split(&self.held);
        self.held.drain(..self.held.len() - held);
        inputs
    }

    /// Whether the start of a mouse report is waiting for its end.
    fn holds(&self) -> bool {
        !self.held.is_empty()
    }

    /// What was held, as keys, once nothing came to end it.
    fn give_up(&mut self) -> Option<Input> {
        self.holds()
            .then(|| Input::Keys(std::mem::take(&mut self.held)))
    }
}

/// How SGR mouse reports start: `CSI <`, then the button, the column and
/// the row, and `M` for a press or `m` for a release.
const SGR_MOUSE: &[u8] = b"\x1b[<";

/// The most a report's numbers take: a button, and a column and a row of
/// up to five digits each, with the semicolons between.
const SGR_LONGEST: usize = 15;

/// `bytes` split into keys and the mouse reports among them, and how many
/// bytes at their end may be the start of one the next read ends.
fn split(bytes: &[u8]) -> (Vec<Input>, usize) {
    let mut inputs = Vec::new();
    // Where the keys not yet handed on start.
    let mut keys = 0;
    let mut at = 0;
    while at < bytes.len() {
        let report = sgr_report(&bytes[at..]);
        if matches!(report, Report::Not) {
            at += 1;
            continue;
        }
        if keys < at {
            inputs.push(Input::Keys(bytes[keys..at].to_vec()));
        }
        let Report::Whole { len, event } = report else {
            return (inputs, bytes.len() - at);
        };
        inputs.extend(event.map(Input::Mouse));
        at += len;
        keys = at;
    }
    if keys < at {
        inputs.push(Input::Keys(bytes[keys..].to_vec()));
    }
    (inputs, 0)
}

/// What the start of some bytes is, as a mouse report.
enum Report {
    Not,
    /// The start of one, cut off.
    Cut,
    /// One `len` bytes long, saying `event`, or nothing crossterm has words
    /// for, like a button past the wheel's.
    Whole {
        len: usize,
        event: Option<MouseEvent>,
    },
}

/// Whether `bytes` start with an SGR mouse report.
fn sgr_report(bytes: &[u8]) -> Report {
    let Some(numbers) = bytes.strip_prefix(SGR_MOUSE) else {
        let cut = bytes.len() < SGR_MOUSE.len() && SGR_MOUSE.starts_with(bytes);
        return if cut { Report::Cut } else { Report::Not };
    };
    let number = |byte: &u8| matches!(byte, b'0'..=b'9' | b';');
    match numbers.iter().position(|byte| !number(byte)) {
        Some(end) if end <= SGR_LONGEST && matches!(numbers[end], b'M' | b'm') => Report::Whole {
            len: SGR_MOUSE.len() + end + 1,
            event: sgr_event(&numbers[..end], numbers[end] == b'm'),
        },
        None if numbers.len() <= SGR_LONGEST => Report::Cut,
        _ => Report::Not,
    }
}

/// The event an SGR report's `numbers` say, ended with `m` for a
/// `release`: the button's bits (the wheel's, a move's, the keys held),
/// then the column and the row, counted from 1.
fn sgr_event(numbers: &[u8], release: bool) -> Option<MouseEvent> {
    let numbers = std::str::from_utf8(numbers).ok()?;
    let mut numbers = numbers.split(';').map(|number| number.parse::<u16>().ok());
    let (Some(Some(code)), Some(Some(column)), Some(Some(row)), None) = (
        numbers.next(),
        numbers.next(),
        numbers.next(),
        numbers.next(),
    ) else {
        return None;
    };
    let button = match code & 3 {
        0 => Some(MouseButton::Left),
        1 => Some(MouseButton::Middle),
        2 => Some(MouseButton::Right),
        _ => None,
    };
    let kind = if code & 128 != 0 {
        return None;
    } else if code & 64 != 0 {
        match code & 3 {
            0 => MouseEventKind::ScrollUp,
            1 => MouseEventKind::ScrollDown,
            2 => MouseEventKind::ScrollLeft,
            _ => MouseEventKind::ScrollRight,
        }
    } else if code & 32 != 0 {
        button.map_or(MouseEventKind::Moved, MouseEventKind::Drag)
    } else if release {
        MouseEventKind::Up(button?)
    } else {
        MouseEventKind::Down(button?)
    };
    let mut modifiers = KeyModifiers::NONE;
    for (bit, held) in [
        (4, KeyModifiers::SHIFT),
        (8, KeyModifiers::ALT),
        (16, KeyModifiers::CONTROL),
    ] {
        if code & bit != 0 {
            modifiers |= held;
        }
    }
    Some(MouseEvent {
        kind,
        column: column.checked_sub(1)?,
        row: row.checked_sub(1)?,
        modifiers,
    })
}

/// The thread that draws a session's output as it comes, until it ends,
/// and passes on its bell and, with `copies`, what it copies.
struct Drawing {
    drawer: Option<thread::JoinHandle<()>>,
    done: Arc<AtomicBool>,
}

impl Drawing {
    fn start(drawn: &Arc<Mutex<Drawn>>, output: Output, copies: bool) -> Drawing {
        let done = Arc::new(AtomicBool::new(false));
        let drawer = thread::spawn({
            let drawn = drawn.clone();
            let done = done.clone();
            move || {
                let mut ringer = Ringer::default();
                for chunk in output {
                    let mut drawn = drawn.lock().unwrap();
                    drawn.shown.screen.process(&chunk);
                    if drawn.draw().is_err() {
                        break;
                    }
                    if drawn.shown.screen.take_bells() > 0 {
                        let _ = ringer.ring();
                    }
                    if let Some(text) = drawn.shown.screen.take_copied()
                        && copies
                    {
                        let _ = clipboard::copy(&text);
                    }
                }
                done.store(true, Ordering::SeqCst);
            }
        });
        Drawing {
            drawer: Some(drawer),
            done,
        }
    }

    fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    /// Waits until the drawer is done, which it is once the output ends.
    fn finish(&mut self) {
        if let Some(drawer) = self.drawer.take() {
            let _ = drawer.join();
        }
    }
}

/// How the session ended, as the daemon tells it.
fn ending(socket: &Path, name: &str) -> String {
    // The output can end a moment before the daemon has seen the exit.
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let sessions = match client::ask(socket, &Request::List, false) {
            Ok(Some(Response::Sessions { sessions })) => sessions,
            // Say a newer crystal took the daemon over.
            Err(err) => return format!("[{err:#}]"),
            Ok(_) => return "[the daemon stopped]".into(),
        };
        match sessions.iter().find(|session| session.name == name) {
            None => return format!("[{name} was killed]"),
            Some(session) if session.state != State::Running => {
                return format!("[{name} {}]", session.state);
            }
            Some(_) if Instant::now() > deadline => return format!("[lost {name}]"),
            Some(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// The screen's rows down to its last non-blank one, in their colors.
fn print_screen(screen: &vt::Screen) -> io::Result<()> {
    let printed = screen.styled();
    let mut out = io::stdout().lock();
    if !printed.is_empty() {
        out.write_all(printed.as_bytes())?;
        out.write_all(b"\x1b[0m\n")?;
    }
    out.flush()
}

/// What the attach shows: the session's screen, copy mode over it while
/// that's on, and what it said last, at the top right.
struct Shown {
    screen: vt::Screen,
    /// Copy mode, while it's on.
    copy: Option<CopyMode>,
    /// What a key or the mouse said last, until the next key: what copy
    /// mode found, what was copied, or the keys that can follow the prefix.
    said: Option<String>,
}

impl Shown {
    fn new(rows: u16, cols: u16) -> Shown {
        Shown {
            screen: vt::Screen::new(rows, cols),
            copy: None,
            said: None,
        }
    }

    /// Puts the screen in copy mode, its cursor where the program's is.
    fn start_copying(&mut self) {
        self.screen.start_copying();
        self.copy = Some(CopyMode::default());
        self.said = None;
    }

    fn stop_copying(&mut self) {
        self.screen.stop_copying();
        self.copy = None;
    }

    /// The mouse went down on the cell at `(row, col)`, the first click,
    /// the second or the third of `clicks` in a row there: where a
    /// selection starts, and copy mode's cursor goes.
    fn select_from(&mut self, cell: (u16, u16), clicks: u8) {
        self.screen.select_from(cell, pane::selection_kind(clicks));
        if self.copy.is_some() {
            self.screen.put_copy_cursor(cell);
        }
    }

    /// The mouse dragged to the cell at `(row, col)`, which the selection,
    /// and copy mode's cursor, go to.
    fn select_to(&mut self, cell: (u16, u16)) {
        self.screen.select_to(cell);
        if self.copy.is_some() {
            self.screen.put_copy_cursor(cell);
        }
    }

    /// Something typed into the session brings the screen back to live,
    /// lets go of what the mouse selected, and puts away what was said.
    /// Returns whether that changed anything.
    fn back_to_live(&mut self) -> bool {
        let changed =
            self.screen.scrolled_back() > 0 || self.screen.selecting() || self.said.is_some();
        self.screen.scroll_to_live();
        self.screen.clear_selection();
        self.said = None;
        changed
    }
}

/// What's shown, and your terminal, which it's drawn on: ratatui keeps
/// what it drew last, and writes only the cells that changed.
struct Drawn {
    shown: Shown,
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    /// What your terminal has been asked to send: what the session's
    /// program asked for, as of the last draw. Your keys go to it as they
    /// come, so your terminal has to write them its way.
    modes: vt::InputModes,
    /// Whether the attach takes the mouse, which your terminal is asked
    /// for besides.
    mouse: bool,
}

impl Drawn {
    fn new(rows: u16, cols: u16, mouse: bool) -> io::Result<Drawn> {
        Ok(Drawn {
            shown: Shown::new(rows, cols),
            terminal: Terminal::new(CrosstermBackend::new(io::stdout()))?,
            modes: vt::InputModes::default(),
            mouse,
        })
    }

    /// Draws the screen, as far back in the history as it's scrolled, with
    /// copy mode's marks, and what the attach says at its top right; the
    /// cursor is the program's while the screen is live, the search's
    /// while one is typed, and none in copy mode.
    fn draw(&mut self) -> io::Result<()> {
        let shown = &self.shown;
        let screen = &shown.screen;
        let copying = shown.copy.is_some();
        let prompt = shown.copy.as_ref().and_then(|copy| copy.prompt.as_ref());
        let label = label(shown.said.as_deref(), copying, screen.scrolled_back());
        self.terminal.draw(|frame| {
            frame.render_widget(ScreenWidget::new(screen), frame.area());
            if let Some(label) = &label {
                draw_label(frame, label);
            }
            if let Some(prompt) = prompt {
                draw_prompt(frame, prompt, screen);
            } else if !copying
                && screen.scrolled_back() == 0
                && let Some((row, col)) = screen.cursor()
            {
                frame.set_cursor_position((col, row));
            }
        })?;
        let modes = self.shown.screen.input_modes();
        let modes = if self.mouse {
            modes.taking_the_mouse()
        } else {
            modes
        };
        let changes = modes.changes_from(&self.modes);
        if !changes.is_empty() {
            draw(&changes)?;
            self.modes = modes;
        }
        Ok(())
    }
}

/// What the attach says at its top right, if anything: that the screen is
/// in copy mode, then what was said last, or else how far back into the
/// history the screen is, as a pane's title says it (`↑ 120 lines`).
fn label(said: Option<&str>, copying: bool, back: usize) -> Option<String> {
    let back = (back > 0).then(|| format!("↑ {back} lines"));
    let after = said.map(str::to_string).or(back);
    match (copying, after) {
        (true, Some(after)) => Some(format!("copy mode · {after}")),
        (true, None) => Some("copy mode".to_string()),
        (false, after) => after,
    }
}

/// Draws `text` at the right of the top row, reversed.
fn draw_label(frame: &mut Frame, text: &str) {
    let text = format!(" {text} ");
    let area = frame.area();
    let width = u16::try_from(text.chars().count()).map_or(area.width, |w| w.min(area.width));
    let corner = Rect {
        x: area.right() - width,
        y: area.y,
        width,
        height: area.height.min(1),
    };
    let reversed = Style::new().add_modifier(Modifier::REVERSED);
    frame.render_widget(Paragraph::new(text).style(reversed), corner);
}

/// The search being typed in copy mode, across the bottom row, or the top
/// one while copy mode's cursor is on the bottom row, with what it has
/// found so far, and the cursor in it.
fn draw_prompt(frame: &mut Frame, prompt: &SearchPrompt, screen: &vt::Screen) {
    let area = frame.area();
    if area.height == 0 || area.width == 0 {
        return;
    }
    let bottom = area.height - 1;
    let on_bottom = screen.copy_cursor().is_some_and(|(row, _)| row == bottom);
    let row = area.y + if on_bottom { 0 } else { bottom };
    let mut text = format!("{}{}", prompt.label(), prompt.text());
    if let Some(count) = prompt.count() {
        text.push_str(&format!("  {count}"));
    }
    let line = Rect {
        y: row,
        height: 1,
        ..area
    };
    let reversed = Style::new().add_modifier(Modifier::REVERSED);
    frame.render_widget(Paragraph::new(text).style(reversed), line);
    // The label is plain ASCII, so its length in bytes is its width.
    let column = area.x + (prompt.label().len() + prompt.cursor()) as u16;
    frame.set_cursor_position((column.min(area.right() - 1), row));
}

fn draw(bytes: &[u8]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(bytes)?;
    out.flush()
}

fn readable(fd: &impl AsRawFd, timeout: Duration) -> io::Result<bool> {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd, and a count of one.
    match unsafe { libc::poll(&mut poll, 1, timeout.as_millis() as libc::c_int) } {
        -1 => {
            let err = io::Error::last_os_error();
            match err.kind() {
                ErrorKind::Interrupted => Ok(false),
                _ => Err(err),
            }
        }
        ready => Ok(ready > 0),
    }
}

/// Raw mode in the alternate screen, put back however the attach ends.
struct RawTerminal;

impl RawTerminal {
    fn enter() -> Result<RawTerminal> {
        terminal::enable_raw_mode()?;
        let raw = RawTerminal;
        draw(ENTER)?;
        Ok(raw)
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = draw(RESET);
        let _ = terminal::disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wheel(kind: MouseEventKind, row: u16, column: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_reports_are_picked_out_of_the_keys() {
        let mut reader = Reader::new(true);
        let inputs = reader.read(b"ls\x1b[<64;3;2Mx\x1b[<0;10;5m");
        assert_eq!(
            inputs,
            [
                Input::Keys(b"ls".to_vec()),
                Input::Mouse(wheel(MouseEventKind::ScrollUp, 1, 2)),
                Input::Keys(b"x".to_vec()),
                Input::Mouse(wheel(MouseEventKind::Up(MouseButton::Left), 4, 9)),
            ]
        );
        assert!(!reader.holds());
        // Left to your terminal, the mouse's reports are all keys.
        let mut keys = Reader::new(false);
        let inputs = keys.read(b"\x1b[<64;3;2M");
        assert_eq!(inputs, [Input::Keys(b"\x1b[<64;3;2M".to_vec())]);
    }

    #[test]
    fn a_report_cut_off_is_held_for_the_next_read() {
        let mut reader = Reader::new(true);
        assert_eq!(reader.read(b"a\x1b[<65;1"), [Input::Keys(b"a".to_vec())]);
        assert!(reader.holds());
        let ended = reader.read(b";1M");
        assert_eq!(
            ended,
            [Input::Mouse(wheel(MouseEventKind::ScrollDown, 0, 0))]
        );

        // An Esc on its own, with nothing after it, was a key after all.
        assert_eq!(reader.read(b"\x1b"), []);
        assert_eq!(reader.give_up(), Some(Input::Keys(b"\x1b".to_vec())));
        assert_eq!(reader.give_up(), None);
    }

    #[test]
    fn what_isn_t_a_report_stays_keys() {
        let mut reader = Reader::new(true);
        // Alt+[ then <, a Kitty key, and numbers too long for a report.
        for keys in [
            &b"\x1b[<x"[..],
            b"\x1b[92;5u",
            b"\x1b[<1234567890123456;1;1M",
        ] {
            assert_eq!(reader.read(keys), [Input::Keys(keys.to_vec())]);
            assert!(!reader.holds());
        }
    }

    #[test]
    fn a_report_says_its_button_and_the_keys_held() {
        let event = |report: &[u8]| match sgr_report(report) {
            Report::Whole { event, .. } => event,
            _ => panic!("not a report"),
        };
        let drag = event(b"\x1b[<52;7;3M").unwrap();
        assert_eq!(drag.kind, MouseEventKind::Drag(MouseButton::Left));
        assert_eq!(drag.modifiers, KeyModifiers::SHIFT | KeyModifiers::CONTROL);
        assert_eq!((drag.row, drag.column), (2, 6));
        let moved = event(b"\x1b[<35;1;1M").unwrap();
        assert_eq!(moved.kind, MouseEventKind::Moved);
        let right = event(b"\x1b[<2;1;1M").unwrap();
        assert_eq!(right.kind, MouseEventKind::Down(MouseButton::Right));
        // A button past the wheel's, and a cell at 0, say nothing.
        assert_eq!(event(b"\x1b[<128;1;1M"), None);
        assert_eq!(event(b"\x1b[<0;0;1M"), None);
    }

    #[test]
    fn the_wheel_scrolls_the_history_on_the_main_screen() {
        let mut screen = vt::Screen::new(3, 10);
        screen.process(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7");
        let up = wheel(MouseEventKind::ScrollUp, 0, 0);
        assert_eq!(on_mouse(&mut screen, false, up, 3), Mouse::Changed);
        assert_eq!(screen.scrolled_back(), 3);
        assert_eq!(on_mouse(&mut screen, false, up, 3), Mouse::Changed);
        assert_eq!(screen.scrolled_back(), 4, "as far back as there is");
        assert_eq!(on_mouse(&mut screen, false, up, 3), Mouse::Nothing);
        let down = wheel(MouseEventKind::ScrollDown, 0, 0);
        assert_eq!(on_mouse(&mut screen, false, down, 3), Mouse::Changed);
        assert_eq!(screen.scrolled_back(), 1);
        // A click there selects, and another button does nothing.
        let click = wheel(MouseEventKind::Down(MouseButton::Left), 0, 0);
        assert_eq!(on_mouse(&mut screen, false, click, 3), Mouse::Select);
        let right = wheel(MouseEventKind::Down(MouseButton::Right), 0, 0);
        assert_eq!(on_mouse(&mut screen, false, right, 3), Mouse::Nothing);
    }

    #[test]
    fn a_program_that_asked_gets_the_mouse_its_way_while_the_screen_is_live() {
        let mut screen = vt::Screen::new(3, 10);
        screen.process(b"1\r\n2\r\n3\r\n4\r\n5\x1b[?1000h");
        let up = wheel(MouseEventKind::ScrollUp, 1, 2);
        assert_eq!(
            on_mouse(&mut screen, false, up, 3),
            Mouse::Keys(b"\x1b[M`#\"".to_vec())
        );
        screen.process(b"\x1b[?1006h");
        let click = wheel(MouseEventKind::Down(MouseButton::Left), 1, 2);
        assert_eq!(
            on_mouse(&mut screen, false, click, 3),
            Mouse::Keys(b"\x1b[<0;3;2M".to_vec())
        );
        // Copy mode has the mouse, and so does the attach back in the
        // history.
        assert_eq!(on_mouse(&mut screen, true, click, 3), Mouse::Select);
        screen.scroll_back(1);
        assert_eq!(on_mouse(&mut screen, false, up, 3), Mouse::Changed);
        assert_eq!(on_mouse(&mut screen, false, click, 3), Mouse::Select);
    }

    #[test]
    fn the_wheel_sends_a_pager_the_arrow_keys() {
        let mut screen = vt::Screen::new(3, 10);
        screen.process(b"\x1b[?1049hless");
        let down = wheel(MouseEventKind::ScrollDown, 0, 0);
        assert_eq!(
            on_mouse(&mut screen, false, down, 2),
            Mouse::Keys(b"\x1b[B\x1b[B".to_vec())
        );
        // Not in copy mode, whose cursor the arrows would move.
        assert_eq!(on_mouse(&mut screen, true, down, 2), Mouse::Nothing);
        // Unless it turned that off: there's no history to scroll either.
        screen.process(b"\x1b[?1007l");
        assert_eq!(on_mouse(&mut screen, false, down, 2), Mouse::Nothing);
    }

    /// The attach's controls, with the TUI's keys and the mouse taken, and
    /// what the mouse selects copied as it lets go or not.
    fn controls(copy_on_select: bool) -> Controls {
        let options = Options {
            copies: false,
            wheel: Some(3),
            copy_on_select,
        };
        Controls::new(Keymap::default(), options)
    }

    /// A screen three rows high showing `four`, `five` and `six seven`,
    /// with two rows of history behind it.
    fn shown() -> Shown {
        let mut shown = Shown::new(3, 20);
        shown
            .screen
            .process(b"one two\r\nthree\r\nfour\r\nfive\r\nsix seven");
        shown
    }

    /// What became of each thing typed in `bytes`.
    fn press(controls: &mut Controls, shown: &mut Shown, bytes: &[u8]) -> Vec<Took> {
        let typed = keys::decode(bytes).into_iter();
        typed.map(|(typed, _)| controls.key(typed, shown)).collect()
    }

    fn copied(text: &str) -> Took {
        Took::Copy(text.to_string())
    }

    #[test]
    fn the_prefix_then_v_is_copy_mode_which_takes_every_key() {
        let (mut controls, mut shown) = (controls(true), shown());
        assert_eq!(press(&mut controls, &mut shown, b"\x02"), [Took::Taken]);
        let said = shown.said.clone().unwrap();
        assert!(said.starts_with("ctrl+b … v copy mode"), "{said}");
        assert_eq!(press(&mut controls, &mut shown, b"v"), [Took::Taken]);
        assert!(shown.copy.is_some() && shown.said.is_none());

        // Up two rows, and that line copied, which ends copy mode.
        let took = press(&mut controls, &mut shown, b"kkY");
        assert_eq!(took, [Took::Taken, Took::Taken, copied("four")]);
        assert!(shown.copy.is_none());
        assert!(!shown.screen.copying());
        assert_eq!(press(&mut controls, &mut shown, b"k"), [Took::Typed]);
    }

    #[test]
    fn the_prefix_twice_is_the_prefix_and_another_key_after_it_does_nothing() {
        let (mut controls, mut shown) = (controls(true), shown());
        let took = press(&mut controls, &mut shown, b"\x02\x02");
        assert_eq!(took, [Took::Taken, Took::Typed]);
        let took = press(&mut controls, &mut shown, b"\x02x");
        assert_eq!(took, [Took::Taken, Took::Taken]);
        assert_eq!(
            shown.said.as_deref(),
            Some("ctrl+b x does nothing in attach")
        );
        // Esc after it does nothing, and says nothing.
        let took = press(&mut controls, &mut shown, b"\x02\x1b");
        assert_eq!(took, [Took::Taken, Took::Taken]);
        assert_eq!(shown.said, None);
        assert_eq!(press(&mut controls, &mut shown, b"x"), [Took::Typed]);
    }

    #[test]
    fn ctrl_backslash_detaches_from_copy_mode_too() {
        let (mut controls, mut shown) = (controls(true), shown());
        let took = press(&mut controls, &mut shown, b"\x02v\x1b[92;5u");
        assert_eq!(took, [Took::Taken, Took::Taken, Took::Detach]);
        assert_eq!(press(&mut controls, &mut shown, b"\x1c"), [Took::Detach]);
    }

    #[test]
    fn a_paste_in_copy_mode_goes_into_the_search() {
        let (mut controls, mut shown) = (controls(true), shown());
        press(&mut controls, &mut shown, b"\x02v?");
        let took = press(&mut controls, &mut shown, b"\x1b[200~three\x1b[201~");
        assert_eq!(took, [Took::Taken]);
        assert_eq!(shown.screen.copy_cursor_line(), "three");
        assert_eq!(shown.screen.scrolled_back(), 1);
    }

    #[test]
    fn a_key_written_direct_works_without_the_prefix() {
        let config = crate::config::from_text("[keys]\ncopy = [\"v\", \"direct+alt+v\"]").unwrap();
        let keymap = Keymap::new(&config.keys).unwrap();
        let mut controls = Controls::new(keymap, controls(true).options);
        let mut shown = shown();
        assert_eq!(press(&mut controls, &mut shown, b"v"), [Took::Typed]);
        assert_eq!(press(&mut controls, &mut shown, b"\x1bv"), [Took::Taken]);
        assert!(shown.copy.is_some());
    }

    #[test]
    fn shift_or_the_prefix_and_the_page_keys_page_through_the_history() {
        let (mut controls, mut shown) = (controls(true), shown());
        let took = press(&mut controls, &mut shown, b"\x1b[5;2~");
        assert_eq!(took, [Took::Taken]);
        assert_eq!(shown.screen.scrolled_back(), 2);
        press(&mut controls, &mut shown, b"\x1b[6;2~");
        assert_eq!(shown.screen.scrolled_back(), 0);
        press(&mut controls, &mut shown, b"\x02\x1b[5~");
        assert_eq!(shown.screen.scrolled_back(), 2);
        // Without either, the page keys are the program's.
        assert_eq!(press(&mut controls, &mut shown, b"\x1b[5~"), [Took::Typed]);
    }

    #[test]
    fn the_releases_of_the_attachs_own_keys_are_not_the_programs() {
        let (mut controls, mut shown) = (controls(true), shown());
        // Ctrl+B, and Ctrl+B again, pressed and let go, in the Kitty
        // protocol with its events: the second is the program's.
        let took = press(
            &mut controls,
            &mut shown,
            b"\x1b[98;5u\x1b[98;5:3u\x1b[98;5u\x1b[98;5:3u",
        );
        assert_eq!(
            took,
            [Took::Taken, Took::Dropped, Took::Typed, Took::Passed]
        );
        // Copy mode's last key, q, comes up after copy mode is over.
        let took = press(&mut controls, &mut shown, b"\x02v\x1b[113u\x1b[113;1:3u");
        assert_eq!(took[2..], [Took::Taken, Took::Dropped]);
        assert!(shown.copy.is_none());
        let took = press(&mut controls, &mut shown, b"\x1b[97u\x1b[97;1:3u");
        assert_eq!(took, [Took::Typed, Took::Passed]);
    }

    /// The mouse doing `kind` on the cell at `(row, column)`.
    fn mouse(
        controls: &mut Controls,
        shown: &mut Shown,
        kind: MouseEventKind,
        cell: (u16, u16),
    ) -> Mouse {
        controls.mouse(wheel(kind, cell.0, cell.1), shown)
    }

    const DOWN: MouseEventKind = MouseEventKind::Down(MouseButton::Left);
    const DRAG: MouseEventKind = MouseEventKind::Drag(MouseButton::Left);
    const UP: MouseEventKind = MouseEventKind::Up(MouseButton::Left);

    #[test]
    fn a_drag_selects_and_letting_go_copies_what_it_covers() {
        let (mut controls, mut shown) = (controls(true), shown());
        assert_eq!(
            mouse(&mut controls, &mut shown, DOWN, (2, 0)),
            Mouse::Select
        );
        assert_eq!(
            mouse(&mut controls, &mut shown, DRAG, (2, 2)),
            Mouse::Changed
        );
        let up = mouse(&mut controls, &mut shown, UP, (2, 2));
        assert_eq!(up, Mouse::Copy("six".into()));
        // It stays marked until you type.
        assert!(shown.screen.selecting());
        assert!(shown.back_to_live());
        assert!(!shown.screen.selecting());
        assert!(!shown.back_to_live());
    }

    #[test]
    fn without_copy_on_select_letting_go_holds_it_in_copy_mode() {
        let (mut controls, mut shown) = (controls(false), shown());
        mouse(&mut controls, &mut shown, DOWN, (1, 0));
        mouse(&mut controls, &mut shown, DRAG, (1, 2));
        assert_eq!(mouse(&mut controls, &mut shown, UP, (1, 2)), Mouse::Changed);
        assert!(shown.copy.is_some());
        // Copy mode's keys take its end on from where the mouse let go.
        let took = press(&mut controls, &mut shown, b"jy");
        assert_eq!(took, [Took::Taken, copied("five\nsix")]);
    }

    #[test]
    fn a_double_click_selects_a_word() {
        let (mut controls, mut shown) = (controls(true), shown());
        mouse(&mut controls, &mut shown, DOWN, (2, 5));
        assert_eq!(mouse(&mut controls, &mut shown, UP, (2, 5)), Mouse::Nothing);
        mouse(&mut controls, &mut shown, DOWN, (2, 5));
        let up = mouse(&mut controls, &mut shown, UP, (2, 5));
        assert_eq!(up, Mouse::Copy("seven".into()));
    }

    #[test]
    fn in_copy_mode_a_click_puts_the_cursor_there() {
        let (mut controls, mut shown) = (controls(true), shown());
        shown.screen.process(b"\x1b[?1000h\x1b[?1006h");
        let click = mouse(&mut controls, &mut shown, DOWN, (0, 1));
        assert_eq!(click, Mouse::Keys(b"\x1b[<0;2;1M".to_vec()));
        press(&mut controls, &mut shown, b"\x02v");
        assert_eq!(
            mouse(&mut controls, &mut shown, DOWN, (0, 1)),
            Mouse::Select
        );
        assert_eq!(mouse(&mut controls, &mut shown, UP, (0, 1)), Mouse::Nothing);
        assert_eq!(shown.screen.copy_cursor(), Some((0, 1)));
        assert_eq!(press(&mut controls, &mut shown, b"Y"), [copied("four")]);
    }

    #[test]
    fn a_drag_held_on_the_top_row_scrolls_back_through_the_history() {
        let (mut controls, mut shown) = (controls(true), shown());
        let now = Instant::now();
        // Down on the top row, it hasn't dragged yet.
        mouse(&mut controls, &mut shown, DOWN, (0, 0));
        assert!(!controls.scroll_past_edge(&mut shown, now));
        mouse(&mut controls, &mut shown, DRAG, (0, 2));
        assert!(controls.scroll_past_edge(&mut shown, now));
        assert_eq!(shown.screen.scrolled_back(), 1);
        assert!(!controls.scroll_past_edge(&mut shown, now), "not yet");
        assert!(controls.scroll_past_edge(&mut shown, now + pane::EDGE_SCROLL_EVERY));
        assert!(
            !controls.scroll_past_edge(&mut shown, now + pane::EDGE_SCROLL_EVERY * 2),
            "the top"
        );
        assert_eq!(shown.screen.scrolled_back(), 2);
        let up = mouse(&mut controls, &mut shown, UP, (0, 2));
        assert_eq!(up, Mouse::Copy("e two\nthree\nf".into()));
    }

    #[test]
    fn the_top_right_says_copy_mode_what_was_said_and_how_far_back() {
        assert_eq!(label(None, false, 0), None);
        assert_eq!(label(None, false, 12).as_deref(), Some("↑ 12 lines"));
        assert_eq!(label(None, true, 0).as_deref(), Some("copy mode"));
        let both = label(None, true, 12);
        assert_eq!(both.as_deref(), Some("copy mode · ↑ 12 lines"));
        let said = label(Some("of: 3 of 12"), true, 12);
        assert_eq!(said.as_deref(), Some("copy mode · of: 3 of 12"));
        let said = label(Some("copied 1 line"), false, 12);
        assert_eq!(said.as_deref(), Some("copied 1 line"));
    }

    /// Where the first key in `keys` that detaches starts.
    fn detach_key(keys: &[u8]) -> Option<usize> {
        keys::decode(keys)
            .into_iter()
            .find(|(typed, _)| matches!(typed, Typed::Key(key) if detaches(key)))
            .map(|(_, range)| range.start)
    }

    #[test]
    fn ctrl_backslash_detaches_the_old_way() {
        assert_eq!(detach_key(b"ls\x1c"), Some(2));
        assert_eq!(detach_key(b"ls\r"), None);
    }

    #[test]
    fn ctrl_backslash_detaches_in_the_kitty_protocol() {
        assert_eq!(detach_key(b"a\x1b[92;5u"), Some(1));
        // A press said so, alternate keys, and Caps Lock on.
        assert_eq!(detach_key(b"\x1b[92;5:1u"), Some(0));
        assert_eq!(detach_key(b"\x1b[92:124;5u"), Some(0));
        assert_eq!(detach_key(b"\x1b[92;69u"), Some(0));
    }

    #[test]
    fn other_keys_in_the_kitty_protocol_dont_detach() {
        // Its release, Ctrl+Shift+\, a plain backslash, Ctrl+], Shift+Enter.
        for keys in [
            &b"\x1b[92;5:3u"[..],
            b"\x1b[92;6u",
            b"\x1b[92u",
            b"\x1b[93;5u",
            b"\x1b[13;2u",
            b"\x1b[92;5",
            b"\x1b[92;0u",
        ] {
            let detaches = keys::decode(keys).iter().any(
                |(typed, _)| matches!(typed, Typed::Key(key) if detaches(key) && key.kind != KeyEventKind::Release),
            );
            assert!(!detaches, "{:?}", String::from_utf8_lossy(keys));
        }
    }
}
