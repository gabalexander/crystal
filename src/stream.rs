//! `crystal observe` and `crystal control`: a session's terminal as a
//! stream of JSON lines on standard output, for a program to watch it, and
//! with `control`, to drive it with JSON lines on standard input, as
//! herdr's `terminal session observe` and `control` do.
//!
//! Both attach as a program, not the user: the session isn't seen or
//! watched for them, and keeps its size unless `control` is given one. A
//! handover cuts them, and they attach again, saying so with a new `start`.
//!
//! What comes out, a JSON object a line:
//!
//! - `{"type":"start","session":…,"id":…,"rows":…,"cols":…,"running":…}`:
//!   the stream has begun, or begun again; a reader starts a screen of that
//!   size afresh.
//! - `{"type":"output","data":"<base64>"}`: what the program wrote, the
//!   first after a `start` drawing the screen as it is.
//! - `{"type":"error","message":…}`: a command `control` couldn't carry out.
//! - `{"type":"closed","reason":"ended"|"released"}`: the last line.
//!
//! What `control` takes:
//!
//! - `{"type":"input","text":…}` or `{"type":"input","data":"<base64>"}`:
//!   bytes for the program, as they are.
//! - `{"type":"keys","keys":["Enter","C-c"]}`: keys by name, as `crystal
//!   send-keys` takes them.
//! - `{"type":"resize","rows":…,"cols":…}`.
//! - `{"type":"release"}`, or the end of its input: the stream ends.

use crate::client;
use crate::clipboard;
use crate::protocol::Request;
use crate::viewer::{Output, Viewer};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, ErrorKind, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// A line `observe` and `control` write.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Record<'a> {
    Start {
        session: &'a str,
        id: &'a str,
        rows: u16,
        cols: u16,
        running: bool,
    },
    Output {
        data: String,
    },
    Error {
        message: String,
    },
    Closed {
        reason: &'static str,
    },
}

/// A line `control` reads.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Command {
    Input {
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        data: Option<String>,
    },
    Keys {
        keys: Vec<String>,
    },
    Resize {
        rows: u16,
        cols: u16,
    },
    Release,
}

/// Streams the session called `name` until it ends.
pub fn observe(socket: &Path, name: &str) -> Result<()> {
    run(socket, name, None, false)
}

/// Streams the session called `name`, at `size` when it's given, and
/// carries out the commands on standard input, until it ends or they say
/// to let go.
pub fn control(socket: &Path, name: &str, size: Option<(u16, u16)>) -> Result<()> {
    run(socket, name, size, true)
}

fn run(socket: &Path, name: &str, size: Option<(u16, u16)>, control: bool) -> Result<()> {
    // Asked for once: attaching again after a handover keeps whatever size
    // it has by then.
    let (viewer, mut output) = Viewer::connect_program(socket, name, size.unwrap_or((0, 0)))?;
    if !start(&viewer) {
        return Ok(());
    }
    let viewer = Arc::new(Mutex::new(viewer));
    let released = Arc::new(AtomicBool::new(false));
    if control {
        let socket = socket.to_path_buf();
        let (viewer, released) = (viewer.clone(), released.clone());
        thread::spawn(move || take_commands(&socket, &viewer, &released));
    }
    loop {
        if !pass_on(&mut output) {
            return Ok(());
        }
        if released.load(Ordering::SeqCst) {
            write(&Record::Closed { reason: "released" });
            return Ok(());
        }
        // The program ended, or the daemon was handed over to a new crystal,
        // which hangs up on every attach: attaching again says which.
        let (name, id) = {
            let viewer = viewer.lock().unwrap();
            (viewer.name.clone(), viewer.id.clone())
        };
        let again = Viewer::connect_program(socket, &name, (0, 0))
            .ok()
            .filter(|(again, _)| again.running && again.id == id);
        let Some((again, more)) = again else {
            write(&Record::Closed { reason: "ended" });
            return Ok(());
        };
        if !start(&again) {
            return Ok(());
        }
        *viewer.lock().unwrap() = again;
        output = more;
    }
}

/// Writes the `start` line for `viewer`, and says whether to go on.
fn start(viewer: &Viewer) -> bool {
    write(&Record::Start {
        session: &viewer.name,
        id: &viewer.id,
        rows: viewer.size.0,
        cols: viewer.size.1,
        running: viewer.running,
    })
}

/// Writes each chunk of `output` as it comes, until it ends; false once
/// whatever reads the lines has gone.
fn pass_on(output: &mut Output) -> bool {
    output.all(|chunk| {
        write(&Record::Output {
            data: clipboard::base64(&chunk),
        })
    })
}

/// Writes `record` as a line, and says whether to go on: not once whatever
/// reads the lines has gone.
fn write(record: &Record) -> bool {
    let Ok(mut line) = serde_json::to_string(record) else {
        return true;
    };
    line.push('\n');
    let mut out = std::io::stdout().lock();
    match out.write_all(line.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => true,
        Err(err) if err.kind() == ErrorKind::BrokenPipe => false,
        Err(_) => false,
    }
}

/// Carries out the commands on standard input, a line each, on the session
/// `viewer` is attached to, until one says to let go or the input ends;
/// then hangs up, which ends the output.
fn take_commands(socket: &Path, viewer: &Mutex<Viewer>, released: &AtomicBool) {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let command = match serde_json::from_str::<Command>(&line) {
            Ok(command) => command,
            Err(err) => {
                write(&Record::Error {
                    message: format!("`{}` isn't a command: {err}", line.trim()),
                });
                continue;
            }
        };
        if command == Command::Release {
            break;
        }
        if let Err(err) = carry_out(socket, viewer, command) {
            write(&Record::Error {
                message: format!("{err:#}"),
            });
        }
    }
    released.store(true, Ordering::SeqCst);
    viewer.lock().unwrap().hang_up();
}

fn carry_out(socket: &Path, viewer: &Mutex<Viewer>, command: Command) -> Result<()> {
    match command {
        Command::Input { text, data } => {
            let bytes = match (text, data) {
                (Some(text), None) => text.into_bytes(),
                (None, Some(data)) => unbase64(&data)?,
                _ => bail!("an input has either `text` or `data`, base64"),
            };
            viewer.lock().unwrap().send_keys(&bytes)?;
        }
        Command::Keys { keys } => {
            let name = viewer.lock().unwrap().name.clone();
            client::ask(socket, &Request::SendKeys { name, keys }, false)?;
        }
        Command::Resize { rows, cols } => {
            if rows == 0 || cols == 0 {
                bail!("a size is at least 1 by 1");
            }
            viewer.lock().unwrap().resize(rows, cols)?;
        }
        Command::Release => {}
    }
    Ok(())
}

/// The bytes base64 `text` stands for, padded or not.
fn unbase64(text: &str) -> Result<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let text = text.trim().trim_end_matches('=');
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut held) = (0u32, 0u32);
    for c in text.bytes() {
        let Some(value) = value(c) else {
            bail!("`data` isn't base64");
        };
        bits = (bits << 6) | value;
        held += 6;
        if held >= 8 {
            held -= 8;
            bytes.push((bits >> held) as u8);
            bits &= (1 << held) - 1;
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_comes_back_as_it_went() {
        for bytes in [&b""[..], b"f", b"fo", b"foo", b"foob", b"\x1b[31m\xff\x00"] {
            assert_eq!(unbase64(&clipboard::base64(bytes)).unwrap(), bytes);
        }
        assert_eq!(unbase64("Zm9v").unwrap(), b"foo");
        assert!(unbase64("Zm9v!").is_err());
    }

    #[test]
    fn commands_read_as_their_type_says() {
        let read = |line: &str| serde_json::from_str::<Command>(line).unwrap();
        assert_eq!(
            read(r#"{"type":"input","text":"ls\r"}"#),
            Command::Input {
                text: Some("ls\r".into()),
                data: None
            }
        );
        assert_eq!(
            read(r#"{"type":"keys","keys":["Enter","C-c"]}"#),
            Command::Keys {
                keys: vec!["Enter".into(), "C-c".into()]
            }
        );
        assert_eq!(
            read(r#"{"type":"resize","rows":24,"cols":80}"#),
            Command::Resize { rows: 24, cols: 80 }
        );
        assert_eq!(read(r#"{"type":"release"}"#), Command::Release);
        assert!(serde_json::from_str::<Command>(r#"{"type":"mouse"}"#).is_err());
    }

    #[test]
    fn records_say_their_type() {
        let start = Record::Start {
            session: "app",
            id: "1",
            rows: 40,
            cols: 120,
            running: true,
        };
        assert_eq!(
            serde_json::to_string(&start).unwrap(),
            r#"{"type":"start","session":"app","id":"1","rows":40,"cols":120,"running":true}"#
        );
        let closed = Record::Closed { reason: "ended" };
        assert_eq!(
            serde_json::to_string(&closed).unwrap(),
            r#"{"type":"closed","reason":"ended"}"#
        );
    }
}
