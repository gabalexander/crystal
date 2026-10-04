//! `crystal attach`: a session fills your terminal until you detach with
//! Ctrl+\ or its program ends.
//!
//! What the session writes is drawn through a screen of our own rather than
//! passed straight through, so whatever the program does to its terminal,
//! like switching screens, stays inside the attach. Your terminal is asked
//! for what the program asked of its keyboard and mouse, the wheel sending
//! arrow keys only while the program is on its alternate screen, though
//! the attach is on your terminal's all along. With `[mouse]
//! attach_capture`, the attach takes the mouse from your terminal instead:
//! a program that asked for it gets it, written its way; the wheel sends a
//! program on its alternate screen that didn't the arrow keys, and anywhere
//! else scrolls the session's history, until you type. Its bell is passed
//! on to your terminal, as often as [`crate::bell`] lets it, and what it
//! copies goes on your clipboard, as [`crate::clipboard`] puts it there.

use crate::bell::Ringer;
use crate::client;
use crate::clipboard;
use crate::config::Config;
use crate::env;
use crate::protocol::{Request, Response, State};
use crate::tui::mouse;
use crate::tui::screen_widget::ScreenWidget;
use crate::viewer::{Output, Viewer};
use crate::vt;
use anyhow::{Result, bail};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
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

/// Ctrl+\, the key that hands your terminal back, the old way. In the
/// Kitty keyboard protocol it's an escape: see [`detach_key`].
const DETACH_KEY: u8 = 0x1c;

/// Ctrl+\'s key in the Kitty keyboard protocol: the backslash.
const BACKSLASH: u32 = 92;

/// How long the input loop waits on the keyboard before it checks the
/// terminal's size and whether the session is still there.
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
    // The history from before is for the wheel to scroll through, once the
    // attach takes the mouse; your terminal's own wheel can't reach it.
    let history = config.mouse.attach_capture;
    let (viewer, output) = Viewer::connect(socket, name, (rows, cols), history)?;
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
        println!("{}", ending(socket, &name));
        return Ok(());
    }

    let options = Options {
        copies: config.clipboard.allow_programs && !in_background(socket, &viewer.id),
        wheel: history.then_some(config.mouse.scroll_lines),
    };
    let detached = {
        let _raw = RawTerminal::enter()?;
        relay(socket, viewer, output, (rows, cols), options)?
    };
    if detached {
        println!("[detached from {name}]");
    } else {
        println!("{}", ending(socket, &name));
    }
    Ok(())
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
}

/// Draws the session and sends it the keyboard, and the mouse as
/// `options` say, until the user detaches (`true`) or the session goes
/// (`false`).
fn relay(
    socket: &Path,
    viewer: Viewer,
    output: Output,
    (rows, cols): (u16, u16),
    options: Options,
) -> Result<bool> {
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
            let history = options.wheel.is_some();
            let Some((again, output)) = attach_again(socket, &viewer, size, history) else {
                break false;
            };
            viewer = again;
            drawn.lock().unwrap().screen = vt::Screen::new(size.0, size.1);
            drawing = Drawing::start(&drawn, output, options.copies);
        }
        // The start of a mouse report waits only a moment for its end.
        let wait = if reader.holds() { HELD } else { TICK };
        if readable(&keyboard, wait)? {
            let n = (&keyboard).read(&mut buf)?;
            let inputs = reader.read(&buf[..n]);
            if n == 0 || take(inputs, &viewer, &drawn, options.wheel)? {
                break true;
            }
        } else if let Some(held) = reader.give_up()
            && take(vec![held], &viewer, &drawn, options.wheel)?
        {
            break true;
        }
        let (cols, rows) = terminal::size()?;
        if (rows, cols) != size {
            size = (rows, cols);
            let _ = viewer.resize(rows, cols);
            let mut drawn = drawn.lock().unwrap();
            drawn.screen.resize(rows, cols);
            drawn.draw()?;
        }
    };
    // Hanging up ends the drawer's output, and the drawer must be done
    // before the terminal is put back.
    drop(viewer);
    drawing.finish();
    Ok(detached)
}

/// The session `viewer` showed, attached again at `(rows, cols)`, while its
/// program still runs, and with its history, for the wheel to scroll.
fn attach_again(
    socket: &Path,
    viewer: &Viewer,
    (rows, cols): (u16, u16),
    history: bool,
) -> Option<(Viewer, Output)> {
    let name = Some(viewer.name.as_str());
    let (again, output) = Viewer::connect(socket, name, (rows, cols), history).ok()?;
    (again.running && again.id == viewer.id).then_some((again, output))
}

/// Hands on what came from your terminal: keys to the session, up to a
/// Ctrl+\, and the mouse as [`on_mouse`] says, while the attach takes it
/// and a notch of the `wheel` is so many lines; then the keys bring the
/// screen back to live from the history first. Returns whether a Ctrl+\
/// detaches.
fn take(
    inputs: Vec<Input>,
    viewer: &Viewer,
    drawn: &Mutex<Drawn>,
    wheel: Option<u16>,
) -> io::Result<bool> {
    for input in inputs {
        match input {
            Input::Keys(keys) => {
                let detach = detach_key(&keys);
                let keys = &keys[..detach.unwrap_or(keys.len())];
                if wheel.is_some() && typed(keys) {
                    let mut drawn = drawn.lock().unwrap();
                    if drawn.screen.scrolled_back() > 0 {
                        drawn.screen.scroll_to_live();
                        drawn.draw()?;
                    }
                }
                if !keys.is_empty() {
                    let _ = viewer.send_keys(keys);
                }
                if detach.is_some() {
                    return Ok(true);
                }
            }
            Input::Mouse(event) => {
                let mut drawn = drawn.lock().unwrap();
                match on_mouse(&mut drawn.screen, event, wheel.unwrap_or(0)) {
                    Mouse::Keys(keys) => {
                        let _ = viewer.send_keys(&keys);
                    }
                    Mouse::Scrolled => drawn.draw()?,
                    Mouse::Nothing => {}
                }
            }
        }
    }
    Ok(false)
}

/// Whether `keys` are something typed: not your terminal saying it gained
/// or lost the focus, which it does for a program that asked, and which
/// leaves the screen as far back in the history as it was.
fn typed(keys: &[u8]) -> bool {
    !keys.is_empty() && !matches!(keys, b"\x1b[I" | b"\x1b[O")
}

/// What a mouse event did on the screen of the attach.
#[derive(Debug, PartialEq, Eq)]
enum Mouse {
    /// It's for the session's program, as these keys.
    Keys(Vec<u8>),
    /// It scrolled the screen through the history.
    Scrolled,
    Nothing,
}

/// What `event` does on `screen`, which fills your terminal: a program that
/// asked for the mouse gets it, written its way, while the screen is live
/// (back in the history, the program's screen isn't what's under the
/// mouse). Otherwise a notch of the wheel is `lines` presses of an arrow
/// key for a program on its alternate screen, as [`mouse::alternate_scroll`]
/// has it, or scrolls the history `lines` rows. Nothing else does anything.
fn on_mouse(screen: &mut vt::Screen, event: MouseEvent, lines: u16) -> Mouse {
    let protocol = mouse::Protocol::of(screen).filter(|_| screen.scrolled_back() == 0);
    if let Some(protocol) = protocol {
        let cell = (event.row, event.column);
        let keys = mouse::encode(event.kind, event.modifiers, cell, protocol);
        return keys.map_or(Mouse::Nothing, Mouse::Keys);
    }
    let back = match event.kind {
        MouseEventKind::ScrollUp => true,
        MouseEventKind::ScrollDown => false,
        _ => return Mouse::Nothing,
    };
    if let Some(arrows) = mouse::alternate_scroll(screen, back, lines) {
        return Mouse::Keys(arrows);
    }
    let was = screen.scrolled_back();
    let rows = usize::from(lines) as isize;
    screen.scroll_back(if back { rows } else { -rows });
    if screen.scrolled_back() == was {
        Mouse::Nothing
    } else {
        Mouse::Scrolled
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
                    drawn.screen.process(&chunk);
                    if drawn.draw().is_err() {
                        break;
                    }
                    if drawn.screen.take_bells() > 0 {
                        let _ = ringer.ring();
                    }
                    if let Some(text) = drawn.screen.take_copied()
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

/// Where the first Ctrl+\ in `keys` starts, the old way or as the Kitty
/// keyboard protocol writes it: `CSI 92 ; 5 u`, with any alternate keys,
/// Caps Lock or Num Lock, and maybe saying it's a press. The keys before
/// it go to the session.
fn detach_key(keys: &[u8]) -> Option<usize> {
    (0..keys.len()).find(|&at| keys[at] == DETACH_KEY || kitty_detach_key(&keys[at..]))
}

/// Whether `keys` starts with Ctrl+\ pressed, in the Kitty protocol.
fn kitty_detach_key(keys: &[u8]) -> bool {
    let Some(rest) = keys.strip_prefix(b"\x1b[") else {
        return false;
    };
    let Some(end) = rest
        .iter()
        .position(|byte| !matches!(byte, b'0'..=b'9' | b';' | b':'))
    else {
        return false;
    };
    if rest[end] != b'u' {
        return false;
    }
    let params = String::from_utf8_lossy(&rest[..end]);
    let mut params = params.split(';');
    let number = |field: Option<&str>, at: usize, default: u32| {
        field
            .unwrap_or("")
            .split(':')
            .nth(at)
            .filter(|number| !number.is_empty())
            .map_or(Some(default), |number| number.parse().ok())
    };
    let key = params.next();
    let modifiers = params.next();
    // Modifiers are written one more than their bits: 4 is Ctrl, and Caps
    // Lock (64) and Num Lock (128) don't count. A press is 1.
    let ctrl = number(modifiers, 0, 1)
        .and_then(|held| held.checked_sub(1))
        .is_some_and(|bits| bits & !(64 | 128) == 4);
    let press = number(modifiers, 1, 1) == Some(1);
    number(key, 0, 0) == Some(BACKSLASH) && ctrl && press
}

/// The session's screen, and your terminal, which it's drawn on: ratatui
/// keeps what it drew last, and writes only the cells that changed.
struct Drawn {
    screen: vt::Screen,
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
            screen: vt::Screen::new(rows, cols),
            terminal: Terminal::new(CrosstermBackend::new(io::stdout()))?,
            modes: vt::InputModes::default(),
            mouse,
        })
    }

    /// Draws the screen, as far back in the history as it's scrolled,
    /// which its top right says, with no cursor.
    fn draw(&mut self) -> io::Result<()> {
        let screen = &self.screen;
        self.terminal.draw(|frame| {
            frame.render_widget(ScreenWidget::new(screen), frame.area());
            match screen.scrolled_back() {
                0 => {
                    if let Some((row, col)) = screen.cursor() {
                        frame.set_cursor_position((col, row));
                    }
                }
                back => draw_back(frame, back),
            }
        })?;
        let modes = self.screen.input_modes();
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

/// Says how far back into the history the screen is, `↑ 120 lines` as a
/// pane's title has it, at the right of its top row.
fn draw_back(frame: &mut Frame, back: usize) {
    let text = format!(" ↑ {back} lines ");
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
        assert_eq!(on_mouse(&mut screen, up, 3), Mouse::Scrolled);
        assert_eq!(screen.scrolled_back(), 3);
        assert_eq!(on_mouse(&mut screen, up, 3), Mouse::Scrolled);
        assert_eq!(screen.scrolled_back(), 4, "as far back as there is");
        assert_eq!(on_mouse(&mut screen, up, 3), Mouse::Nothing);
        let down = wheel(MouseEventKind::ScrollDown, 0, 0);
        assert_eq!(on_mouse(&mut screen, down, 3), Mouse::Scrolled);
        assert_eq!(screen.scrolled_back(), 1);
        // A click there is nobody's.
        let click = wheel(MouseEventKind::Down(MouseButton::Left), 0, 0);
        assert_eq!(on_mouse(&mut screen, click, 3), Mouse::Nothing);
    }

    #[test]
    fn a_program_that_asked_gets_the_mouse_its_way_while_the_screen_is_live() {
        let mut screen = vt::Screen::new(3, 10);
        screen.process(b"1\r\n2\r\n3\r\n4\r\n5\x1b[?1000h");
        let up = wheel(MouseEventKind::ScrollUp, 1, 2);
        assert_eq!(
            on_mouse(&mut screen, up, 3),
            Mouse::Keys(b"\x1b[M`#\"".to_vec())
        );
        screen.process(b"\x1b[?1006h");
        let click = wheel(MouseEventKind::Down(MouseButton::Left), 1, 2);
        assert_eq!(
            on_mouse(&mut screen, click, 3),
            Mouse::Keys(b"\x1b[<0;3;2M".to_vec())
        );
        // Back in the history, the wheel is the attach's.
        screen.scroll_back(1);
        assert_eq!(on_mouse(&mut screen, up, 3), Mouse::Scrolled);
        assert_eq!(on_mouse(&mut screen, click, 3), Mouse::Nothing);
    }

    #[test]
    fn the_wheel_sends_a_pager_the_arrow_keys() {
        let mut screen = vt::Screen::new(3, 10);
        screen.process(b"\x1b[?1049hless");
        let down = wheel(MouseEventKind::ScrollDown, 0, 0);
        assert_eq!(
            on_mouse(&mut screen, down, 2),
            Mouse::Keys(b"\x1b[B\x1b[B".to_vec())
        );
        // Unless it turned that off: there's no history to scroll either.
        screen.process(b"\x1b[?1007l");
        assert_eq!(on_mouse(&mut screen, down, 2), Mouse::Nothing);
    }

    #[test]
    fn focus_reports_aren_t_typing() {
        assert!(typed(b"a"));
        assert!(!typed(b"\x1b[I"));
        assert!(!typed(b"\x1b[O"));
        assert!(!typed(b""));
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
            assert_eq!(
                detach_key(keys),
                None,
                "{:?}",
                String::from_utf8_lossy(keys)
            );
        }
    }
}
