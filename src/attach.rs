//! `crystal attach`: a session fills your terminal until you detach with
//! Ctrl+\ or its program ends.
//!
//! What the session writes is drawn through a screen of our own rather than
//! passed straight through, so whatever the program does to its terminal,
//! like switching screens, stays inside the attach.

use crate::client;
use crate::env;
use crate::protocol::{Request, Response, State};
use crate::viewer::{Output, Viewer};
use anyhow::{Result, bail};
use crossterm::terminal;
use std::fs::File;
use std::io::{self, ErrorKind, IsTerminal, Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Ctrl+\, the key that hands your terminal back.
const DETACH_KEY: u8 = 0x1c;

/// How long the input loop waits on the keyboard before it checks the
/// terminal's size and whether the session is still there.
const TICK: Duration = Duration::from_millis(50);

/// Puts back every mode a session may have turned on, then leaves the
/// alternate screen.
const RESET: &[u8] = b"\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[?2004l\
\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1049l";

pub fn run(socket: &Path, name: Option<&str>) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("attach needs a terminal");
    }
    let (cols, rows) = terminal::size()?;
    let (viewer, output) = Viewer::connect(socket, name, rows, cols)?;
    let name = viewer.name.clone();
    if env::own_session(socket).as_deref() == Some(name.as_str()) {
        bail!("can't attach {name} to itself");
    }

    if !viewer.running {
        // Nothing more is coming: show how it ended, without taking over
        // the terminal.
        let mut screen = vt100::Parser::new(rows, cols, 0);
        for chunk in output {
            screen.process(&chunk);
        }
        print_screen(screen.screen())?;
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
    let screen = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
    let gone = Arc::new(AtomicBool::new(false));
    let drawer = thread::spawn({
        let screen = screen.clone();
        let gone = gone.clone();
        move || {
            for chunk in output {
                let mut screen = screen.lock().unwrap();
                let before = screen.screen().clone();
                screen.process(&chunk);
                if draw(&screen.screen().state_diff(&before)).is_err() {
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
            let detach = buf[..n].iter().position(|&byte| byte == DETACH_KEY);
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
            let mut screen = screen.lock().unwrap();
            screen.screen_mut().set_size(rows, cols);
            draw(&screen.screen().state_formatted())?;
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
fn print_screen(screen: &vt100::Screen) -> io::Result<()> {
    let (_, cols) = screen.size();
    let used = screen
        .rows(0, cols)
        .enumerate()
        .filter(|(_, row)| !row.trim().is_empty())
        .last()
        .map_or(0, |(last, _)| last + 1);
    let mut out = io::stdout().lock();
    for row in screen.rows_formatted(0, cols).take(used) {
        out.write_all(&row)?;
        out.write_all(b"\x1b[0m\n")?;
    }
    out.flush()
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
        draw(b"\x1b[?1049h\x1b[H\x1b[2J")?;
        Ok(raw)
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = draw(RESET);
        let _ = terminal::disable_raw_mode();
    }
}
