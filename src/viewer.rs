//! A client's connection to one session: what an attach opens. `crystal
//! attach` and the TUI's pane are both viewers; the daemon treats them the
//! same.

use crate::protocol::{self, Frame, Request, Response};
use anyhow::{Context, Result, bail};
use std::io::{self, BufRead, BufReader, ErrorKind};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;

/// The sending half of an attach: keys and size go to the session.
pub struct Viewer {
    /// The session's name, as the daemon resolved it.
    pub name: String,
    /// False when the session had already ended: the daemon sends its last
    /// screen and hangs up.
    pub running: bool,
    conn: UnixStream,
}

/// The receiving half of an attach: the session's screen as it was, then
/// everything its program writes. Read it on a thread of its own; it ends
/// when the session does or the [`Viewer`] is dropped.
pub struct Output {
    reader: BufReader<UnixStream>,
}

impl Viewer {
    /// Attaches to the session called `name`, or the newest one, at a size
    /// of `rows` by `cols`.
    pub fn connect(
        socket: &Path,
        name: Option<&str>,
        rows: u16,
        cols: u16,
    ) -> Result<(Viewer, Output)> {
        let conn = UnixStream::connect(socket)
            .with_context(|| format!("no daemon is running on {}", socket.display()))?;
        let request = Request::Attach {
            name: name.map(String::from),
            rows,
            cols,
        };
        protocol::send(&conn, &request)?;

        // The same reader goes on to read the output: it may already hold
        // the first of it.
        let mut reader = BufReader::new(conn.try_clone()?);
        let (name, running) = match protocol::recv(&mut reader)? {
            Some(Response::Attached { name, running }) => (name, running),
            Some(Response::Error { message }) => bail!(message),
            _ => bail!("the daemon hung up without answering"),
        };
        let viewer = Viewer {
            name,
            running,
            conn,
        };
        Ok((viewer, Output { reader }))
    }

    pub fn send_keys(&self, keys: &[u8]) -> io::Result<()> {
        protocol::send_frame(&self.conn, &Frame::Input(keys.to_vec()))
    }

    pub fn resize(&self, rows: u16, cols: u16) -> io::Result<()> {
        protocol::send_frame(&self.conn, &Frame::Resize { rows, cols })
    }
}

impl Drop for Viewer {
    /// Hangs up, which also ends the [`Output`] on whatever thread reads it.
    fn drop(&mut self) {
        let _ = self.conn.shutdown(Shutdown::Both);
    }
}

impl Iterator for Output {
    type Item = Vec<u8>;

    /// The next bytes from the session, or `None` once it has ended or the
    /// viewer has hung up.
    fn next(&mut self) -> Option<Vec<u8>> {
        loop {
            match self.reader.fill_buf() {
                Ok([]) => return None,
                Ok(bytes) => {
                    let chunk = bytes.to_vec();
                    self.reader.consume(chunk.len());
                    return Some(chunk);
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return None,
            }
        }
    }
}
