//! `crystal attach`: a session fills your terminal until you detach with
//! Ctrl+\ or its program ends.
//!
//! What the session writes is drawn through a screen of our own rather than
//! passed straight through, so whatever the program does to its terminal,
//! like switching screens, stays inside the attach.

use crate::client;
use crate::env;
use crate::protocol::{Request, Response, State};
use crate::tui::screen_widget::ScreenWidget;
use crate::viewer::{Output, Viewer};
use crate::vt;
use anyhow::{Result, bail};
use crossterm::terminal;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
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

/// Puts back every mode a session may have turned on, pops the Kitty
/// keyboard flags the attach pushed, then leaves the alternate screen.
const RESET: &[u8] = b"\x1b[<u\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[?2004l\x1b[?1004l\
\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1016l\x1b[?1049l";

pub fn run(socket: &Path, name: Option<&str>) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("attach needs a terminal");
    }
    let (cols, rows) = terminal::size()?;
    // Your own terminal keeps what scrolls by while you're attached; the
    // history from before is for the TUI's panes and `crystal read`.
    let (viewer, output) = Viewer::connect(socket, name, (rows, cols), false)?;
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

    let detached = {
        let _raw = RawTerminal::enter()?;
        relay(viewer, output, rows, cols)?
    };
    if detached {
        println!("[detached from {name}]");
    } else {
        println!("{}", ending(socket, &name));
    }
    Ok(())
}

/// Draws the session and sends it the keyboard until the user detaches
/// (`true`) or the session goes (`false`).
fn relay(viewer: Viewer, output: Output, rows: u16, cols: u16) -> Result<bool> {
    let drawn = Arc::new(Mutex::new(Drawn::new(rows, cols)?));
    let gone = Arc::new(AtomicBool::new(false));
    let drawer = thread::spawn({
        let drawn = drawn.clone();
        let gone = gone.clone();
        move || {
            for chunk in output {
                let mut drawn = drawn.lock().unwrap();
                drawn.screen.process(&chunk);
                if drawn.draw().is_err() {
                    break;
                }
            }
            gone.store(true, Ordering::SeqCst);
        }
    });

    // Our own handle on the keyboard, unbuffered, so that waiting on it
    // and reading from it agree.
    let keyboard = File::from(io::stdin().as_fd().try_clone_to_owned()?);
    let mut size = (rows, cols);
    let mut buf = [0; 4096];
    let detached = loop {
        if gone.load(Ordering::SeqCst) {
            break false;
        }
        if readable(&keyboard, TICK)? {
            let n = (&keyboard).read(&mut buf)?;
            let detach = detach_key(&buf[..n]);
            let keys = &buf[..detach.unwrap_or(n)];
            if !keys.is_empty() {
                let _ = viewer.send_keys(keys);
            }
            if detach.is_some() || n == 0 {
                break true;
            }
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
    let _ = drawer.join();
    Ok(detached)
}

/// How the session ended, as the daemon tells it.
fn ending(socket: &Path, name: &str) -> String {
    // The output can end a moment before the daemon has seen the exit.
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let sessions = match client::ask(socket, &Request::List, false) {
            Ok(Some(Response::Sessions { sessions })) => sessions,
            _ => return "[the daemon stopped]".into(),
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
}

impl Drawn {
    fn new(rows: u16, cols: u16) -> io::Result<Drawn> {
        Ok(Drawn {
            screen: vt::Screen::new(rows, cols),
            terminal: Terminal::new(CrosstermBackend::new(io::stdout()))?,
            modes: vt::InputModes::default(),
        })
    }

    fn draw(&mut self) -> io::Result<()> {
        let screen = &self.screen;
        self.terminal.draw(|frame| {
            frame.render_widget(ScreenWidget::new(screen), frame.area());
            if let Some((row, col)) = screen.cursor() {
                frame.set_cursor_position((col, row));
            }
        })?;
        let modes = self.screen.input_modes();
        let changes = modes.changes_from(&self.modes);
        if !changes.is_empty() {
            draw(&changes)?;
            self.modes = modes;
        }
        Ok(())
    }
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
        // An entry of the attach's own on the terminal's stack of Kitty
        // keyboard flags, for the session's flags to go in.
        draw(b"\x1b[?1049h\x1b[H\x1b[2J\x1b[>0u")?;
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
