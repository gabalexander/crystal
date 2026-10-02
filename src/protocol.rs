//! What the CLI and the daemon say to each other: one JSON object per line,
//! one request and one response per connection.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    New {
        name: Option<String>,
        cwd: PathBuf,
        command: Vec<String>,
    },
    List,
    Kill {
        name: String,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Created { name: String },
    Sessions { sessions: Vec<SessionInfo> },
    Done,
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub pid: Option<u32>,
    pub state: State,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    Exited { code: u32 },
    Signaled { signal: String },
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            State::Running => write!(f, "running"),
            State::Exited { code } => write!(f, "exited {code}"),
            State::Signaled { signal } => write!(f, "killed ({signal})"),
        }
    }
}

pub fn send<T: Serialize>(mut out: impl Write, message: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    out.write_all(&line)?;
    out.flush()
}

/// The next message, or `None` once the other side has hung up.
pub fn recv<T: DeserializeOwned>(mut input: impl BufRead) -> io::Result<Option<T>> {
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&line)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_survives_the_round_trip() {
        let mut wire = Vec::new();
        let request = Request::Kill {
            name: "claude".into(),
        };
        send(&mut wire, &request).unwrap();
        assert_eq!(wire.last(), Some(&b'\n'));

        let back: Request = recv(&wire[..]).unwrap().unwrap();
        assert!(matches!(back, Request::Kill { name } if name == "claude"));
    }

    #[test]
    fn recv_reports_a_hang_up_as_none() {
        let back: Option<Request> = recv(&b""[..]).unwrap();
        assert!(back.is_none());
    }
}
