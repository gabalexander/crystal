//! What the CLI and the daemon say to each other: one JSON object per line,
//! one request and one response per connection. An attach goes on after its
//! response: the daemon sends the session's output as it comes, and the
//! client sends [`Frame`]s.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, BufRead, ErrorKind, Read, Write};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    New(NewSession),
    List,
    Kill {
        name: String,
    },
    /// Something the agent in a session did, sent by its hooks.
    Report {
        name: String,
        event: AgentEvent,
        /// The conversation the agent is in, when its hooks say.
        #[serde(default)]
        conversation: Option<Conversation>,
    },
    /// With no name, the newest session.
    Attach {
        name: Option<String>,
        rows: u16,
        cols: u16,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NewSession {
    /// `None` names the session after its program.
    pub name: Option<String>,
    pub cwd: PathBuf,
    pub command: Vec<String>,
    /// The client's environment, which the program starts from.
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Created {
        name: String,
    },
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    /// `running` is false when the session has already ended: the daemon
    /// sends its last screen and hangs up.
    Attached {
        name: String,
        running: bool,
    },
    Done,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub pid: Option<u32>,
    pub state: State,
    /// `None` for a program that doesn't report what it's doing.
    pub activity: Option<Activity>,
    /// `None` when the session's directory isn't in a git repository.
    pub worktree: Option<Worktree>,
}

/// The git worktree a session runs in, and the project it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    /// The project's name: the name of its main worktree's directory.
    pub project: String,
    /// The main worktree's directory, which tells projects apart.
    pub project_path: PathBuf,
    /// This worktree's top directory.
    pub path: PathBuf,
    /// Whether this is the repository's main worktree, rather than one
    /// linked to it with `git worktree add`.
    pub main: bool,
    /// The branch checked out, or `None` when HEAD is detached.
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    Exited { code: u32 },
    Signaled { signal: String },
}

/// An agent's conversation, as its hooks name it: what it takes to pick
/// the conversation up again after a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    /// The file the agent keeps the conversation in.
    pub transcript: Option<PathBuf>,
}

impl Conversation {
    /// Whether there's anything to pick up: an agent that was never sent a
    /// prompt hasn't written its transcript, and can't resume it.
    pub fn can_resume(&self) -> bool {
        self.transcript.as_ref().is_some_and(|path| path.is_file())
    }
}

/// What an agent's hooks report, in terms that fit any agent. The daemon
/// works out the session's [`Activity`] from these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEvent {
    /// The agent is up and waiting for its first prompt.
    Started,
    TurnStarted,
    /// A tool call finished, so the agent is back to work, say after the
    /// user allowed it.
    ToolFinished,
    /// The agent is asking the user something: a permission, a question.
    Asking,
    TurnEnded,
    /// The agent has sat at its prompt for a while.
    StillIdle,
}

/// What the agent in a session is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    /// Working on a turn.
    Working,
    /// Stopped until the user answers it, say a permission prompt.
    Waiting,
    /// Finished its turn, and nobody has looked at it since.
    Done,
    /// Finished its turn, and it has been seen.
    Idle,
}

impl fmt::Display for Activity {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let name = match self {
            Activity::Working => "working",
            Activity::Waiting => "waiting",
            Activity::Done => "done",
            Activity::Idle => "idle",
        };
        f.write_str(name)
    }
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

/// What an attached client sends: keys for the session, and its size.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Input(Vec<u8>),
    Resize { rows: u16, cols: u16 },
}

const INPUT: u8 = 0;
const RESIZE: u8 = 1;

pub fn send_frame(mut out: impl Write, frame: &Frame) -> io::Result<()> {
    let mut bytes = Vec::new();
    match frame {
        Frame::Input(input) => {
            bytes.push(INPUT);
            bytes.extend_from_slice(&(input.len() as u32).to_be_bytes());
            bytes.extend_from_slice(input);
        }
        Frame::Resize { rows, cols } => {
            bytes.push(RESIZE);
            bytes.extend_from_slice(&rows.to_be_bytes());
            bytes.extend_from_slice(&cols.to_be_bytes());
        }
    }
    out.write_all(&bytes)
}

/// The next frame, or `None` once the client has hung up.
pub fn recv_frame(mut input: impl Read) -> io::Result<Option<Frame>> {
    let mut kind = [0];
    if input.read(&mut kind)? == 0 {
        return Ok(None);
    }
    match kind[0] {
        INPUT => {
            let mut len = [0; 4];
            input.read_exact(&mut len)?;
            let mut bytes = vec![0; u32::from_be_bytes(len) as usize];
            input.read_exact(&mut bytes)?;
            Ok(Some(Frame::Input(bytes)))
        }
        RESIZE => {
            let mut size = [0; 4];
            input.read_exact(&mut size)?;
            Ok(Some(Frame::Resize {
                rows: u16::from_be_bytes([size[0], size[1]]),
                cols: u16::from_be_bytes([size[2], size[3]]),
            }))
        }
        kind => Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("unknown frame kind {kind}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conversation_resumes_only_once_its_transcript_exists() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("abc.jsonl");
        let conversation = Conversation {
            id: "abc".into(),
            transcript: Some(transcript.clone()),
        };
        assert!(!conversation.can_resume());
        std::fs::write(&transcript, "{}\n").unwrap();
        assert!(conversation.can_resume());
    }

    #[test]
    fn frames_survive_the_round_trip() {
        let frames = [
            Frame::Input(b"hello\r".to_vec()),
            Frame::Resize {
                rows: 50,
                cols: 200,
            },
            Frame::Input(Vec::new()),
        ];
        let mut wire = Vec::new();
        for frame in &frames {
            send_frame(&mut wire, frame).unwrap();
        }

        let mut wire = &wire[..];
        for frame in frames {
            assert_eq!(recv_frame(&mut wire).unwrap(), Some(frame));
        }
        assert_eq!(recv_frame(&mut wire).unwrap(), None);
    }

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
