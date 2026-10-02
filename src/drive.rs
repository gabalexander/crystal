//! `crystal send`, `wait` and `read`: how one agent drives another, or a
//! script drives an agent. They work from inside a session as well, since
//! every session knows its daemon's socket.

use crate::client;
use crate::protocol::{Activity, Request, Response, SessionInfo, State};
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// How often `wait` asks for the session again.
const POLL_EVERY: Duration = Duration::from_millis(100);

/// How long `send --wait` gives an agent to start on what it was sent. A
/// turn can also be over before it's seen starting, so after this the wait
/// goes on to the end of the turn either way.
const START_GRACE: Duration = Duration::from_secs(5);

/// Types `text` into the session called `name`, and presses Enter after it
/// when `enter` is set.
pub fn send(socket: &Path, name: &str, text: &str, enter: bool) -> Result<()> {
    let request = Request::Send {
        name: name.to_string(),
        text: text.to_string(),
        enter,
    };
    ask(socket, &request)?;
    Ok(())
}

/// Waits until the session's agent isn't working, or its program has
/// ended, and prints which. Gives up after `timeout`, if there is one.
pub fn wait(socket: &Path, name: &str, timeout: Option<Duration>) -> Result<()> {
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    match poll(socket, name, deadline, settled)? {
        Some(settled) => {
            println!("{settled}");
            Ok(())
        }
        None => timed_out(name, timeout),
    }
}

/// Waits for the turn that a `send` has just started: first for the agent
/// to start working, then as [`wait`] does. Right after the send the agent
/// may not have started yet, and whatever it said about the turn before
/// would end the wait at once.
pub fn wait_for_turn(socket: &Path, name: &str, timeout: Option<Duration>) -> Result<()> {
    let start = Instant::now();
    let grace_over = start + START_GRACE;
    let start_deadline = match timeout {
        Some(timeout) => grace_over.min(start + timeout),
        None => grace_over,
    };
    // Seen starting or not, what's left is to wait for the turn to end.
    let _seen_starting = poll(socket, name, Some(start_deadline), |session| {
        started(session).then_some(())
    })?;

    let remaining = timeout.map(|timeout| timeout.saturating_sub(start.elapsed()));
    wait(socket, name, remaining)
}

/// Prints what's on the session's screen, after its history with
/// `history`. With `lines`, only that many of the last rows that aren't
/// blank.
pub fn read(socket: &Path, name: &str, lines: Option<usize>, history: bool) -> Result<()> {
    let request = Request::Read {
        name: name.to_string(),
        history,
    };
    let Response::Screen { rows } = ask(socket, &request)? else {
        bail!("the daemon didn't send the screen");
    };
    print!("{}", screen_text(&rows, lines));
    Ok(())
}

/// What a session has settled on, once it isn't busy: how its program
/// ended, or what its agent is doing when that isn't working. A program
/// that doesn't say what it's doing is busy until it ends.
fn settled(session: &SessionInfo) -> Option<String> {
    if session.state != State::Running {
        return Some(session.state.to_string());
    }
    match session.activity {
        Some(Activity::Working) | None => None,
        Some(activity) => Some(activity.to_string()),
    }
}

/// Whether a session's agent has started on a turn, or its program has
/// ended, which is as good as an answer.
fn started(session: &SessionInfo) -> bool {
    session.state != State::Running || session.activity == Some(Activity::Working)
}

/// The screen as text: each row without its trailing spaces, and no blank
/// rows after the last one with something on it. With `lines`, only that
/// many of the last rows that aren't blank.
fn screen_text(rows: &[String], lines: Option<usize>) -> String {
    let rows: Vec<&str> = rows.iter().map(|row| row.trim_end()).collect();
    let kept: Vec<&str> = match lines {
        Some(lines) => {
            let filled: Vec<&str> = rows.into_iter().filter(|row| !row.is_empty()).collect();
            let first = filled.len().saturating_sub(lines);
            filled[first..].to_vec()
        }
        None => {
            let end = rows
                .iter()
                .rposition(|row| !row.is_empty())
                .map_or(0, |last| last + 1);
            rows[..end].to_vec()
        }
    };
    kept.iter().map(|row| format!("{row}\n")).collect()
}

/// Asks for the session called `name` again and again, until `check`
/// gives an answer, and returns it; or `None` once `deadline` has passed.
fn poll<T>(
    socket: &Path,
    name: &str,
    deadline: Option<Instant>,
    check: impl Fn(&SessionInfo) -> Option<T>,
) -> Result<Option<T>> {
    loop {
        if let Some(answer) = check(&session(socket, name)?) {
            return Ok(Some(answer));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(None);
        }
        thread::sleep(POLL_EVERY);
    }
}

fn session(socket: &Path, name: &str) -> Result<SessionInfo> {
    let Response::Sessions { sessions } = ask(socket, &Request::List)? else {
        bail!("the daemon didn't send the sessions");
    };
    sessions
        .into_iter()
        .find(|session| session.name == name)
        .with_context(|| format!("no session named {name}"))
}

/// Asks the daemon, which must be running already: these commands only
/// make sense for a session that exists.
fn ask(socket: &Path, request: &Request) -> Result<Response> {
    match client::ask(socket, request, false)? {
        Some(response) => Ok(response),
        None => bail!("no daemon is running on {}", socket.display()),
    }
}

fn timed_out(name: &str, timeout: Option<Duration>) -> Result<()> {
    let seconds = timeout.unwrap_or_default().as_secs_f64();
    bail!("{name} was still busy after {seconds}s")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn session(state: State, activity: Option<Activity>) -> SessionInfo {
        SessionInfo {
            name: "agent".into(),
            command: vec!["claude".into()],
            cwd: PathBuf::from("/"),
            pid: Some(1),
            state,
            activity,
            worktree: None,
        }
    }

    fn rows(text: &[&str]) -> Vec<String> {
        text.iter().map(|row| row.to_string()).collect()
    }

    #[test]
    fn an_agent_settles_on_anything_but_working() {
        let running = |activity| settled(&session(State::Running, activity));
        assert_eq!(running(Some(Activity::Working)), None);
        assert_eq!(running(Some(Activity::Done)), Some("done".into()));
        assert_eq!(running(Some(Activity::Waiting)), Some("waiting".into()));
        assert_eq!(running(Some(Activity::Idle)), Some("idle".into()));
    }

    #[test]
    fn a_program_that_says_nothing_settles_when_it_ends() {
        assert_eq!(settled(&session(State::Running, None)), None);
        let ended = session(State::Exited { code: 4 }, Some(Activity::Working));
        assert_eq!(settled(&ended), Some("exited 4".into()));
    }

    #[test]
    fn a_turn_has_started_once_the_agent_works_or_the_program_ends() {
        assert!(!started(&session(State::Running, Some(Activity::Done))));
        assert!(started(&session(State::Running, Some(Activity::Working))));
        assert!(started(&session(State::Exited { code: 0 }, None)));
    }

    #[test]
    fn the_screen_loses_its_trailing_blanks() {
        let screen = rows(&["one   ", "", "two", "", ""]);
        assert_eq!(screen_text(&screen, None), "one\n\ntwo\n");
        assert_eq!(screen_text(&rows(&["", ""]), None), "");
    }

    #[test]
    fn lines_keeps_the_last_rows_that_say_something() {
        let screen = rows(&["one", "", "two", "three", "", ""]);
        assert_eq!(screen_text(&screen, Some(2)), "two\nthree\n");
        assert_eq!(screen_text(&screen, Some(10)), "one\ntwo\nthree\n");
    }
}
