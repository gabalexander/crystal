//! What `crystal send` carries from one session to another: the text tidied
//! and cut to a size, a line ahead of it saying which session sent it, the
//! guard that keeps a session from sending more than so many a minute, and
//! why an agent that's asking the user something takes nothing typed into
//! it. Adapted from docket's `docket send`.
//!
//! Only a message from another session is tidied, headed and counted: one
//! from the user, a script or the TUI goes as it was typed, the way `send`
//! has always typed it.

use crate::protocol::TaskInfo;
use anyhow::{Result, bail, ensure};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The most a message from another session may be, in bytes: docket's.
pub const MOST_BYTES: usize = 8 * 1024;

/// How many messages a session may send in a minute. Two agents answering
/// each other would otherwise never stop.
pub const SENDS_PER_MINUTE: usize = 20;

/// The window [`SENDS_PER_MINUTE`] is counted over, sliding.
const WINDOW: Duration = Duration::from_secs(60);

/// The session a message comes from, as it is when it sends it.
pub struct Sender {
    pub id: String,
    pub name: String,
    /// What it was asked to do, while its task is open.
    pub task: Option<String>,
}

impl Sender {
    /// The session called `name`, with id `id`, and its task, if it has one
    /// still open.
    pub fn new(id: &str, name: &str, task: Option<&TaskInfo>) -> Sender {
        let task = task.filter(|task| task.is_open());
        Sender {
            id: id.to_string(),
            name: name.to_string(),
            task: task.map(|task| first_line(&task.goal)),
        }
    }
}

/// `text` from another session, ready to send: control characters taken
/// out but for line breaks, a tab made a space, and cut to [`MOST_BYTES`]
/// with `…` where it was cut. The rest is left as it was written. Nothing
/// left to send is refused.
pub fn tidy(text: &str) -> Result<String> {
    let text = text.replace("\r\n", "\n");
    let tidied: String = text
        .chars()
        .filter_map(|c| match c {
            '\n' => Some('\n'),
            '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    let tidied = cut(tidied.trim(), MOST_BYTES);
    ensure!(
        !tidied.is_empty(),
        "the message is empty: `crystal send <session> \"<what it needs to know>\"`"
    );
    Ok(tidied)
}

/// `text` cut to at most `most` bytes, on a character's edge, with `…`
/// where it was cut.
fn cut(text: &str, most: usize) -> String {
    if text.len() <= most {
        return text.to_string();
    }
    let mark = '…';
    let mut end = most - mark.len_utf8();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{mark}", &text[..end])
}

/// The line ahead of a message that says which session sent it, and what
/// it's working on: `[crystal] Message from session "scout", working on
/// task "Port the codec":`.
pub fn header(sender: &Sender) -> String {
    let mut line = format!("[crystal] Message from session \"{}\"", sender.name);
    if let Some(task) = &sender.task {
        line.push_str(&format!(", working on task \"{task}\""));
    }
    line.push(':');
    line
}

/// A message from `sender`, its header on a line of its own ahead of it.
pub fn compose(sender: &Sender, text: &str) -> String {
    format!("{}\n{text}", header(sender))
}

/// Why `name`, whose agent is asking the user something (`why`, as
/// `Session::blocked` says it), takes nothing typed into it, and what to do
/// instead: a background task's permission is answered with `crystal
/// answer`, a terminal's question in its pane or with `send-keys`.
pub fn blocked(name: &str, why: &str, task: bool) -> String {
    let answer = if task {
        format!("with `crystal answer {name} y|n|always`, or y or n in the TUI")
    } else {
        format!("in its pane, or with `crystal send-keys {name} …`")
    };
    format!("agent_blocked: {name} is {why}. Answer it first, {answer}, or send again with --force")
}

/// When each session sent the messages it sent in the last minute, kept
/// only while the daemon runs.
#[derive(Default)]
pub struct Guard {
    sent: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Guard {
    /// Counts a message from the session with id `from`, sent at `now`, or
    /// refuses it when the session has sent [`SENDS_PER_MINUTE`] in the
    /// minute before.
    pub fn admit(&self, from: &str, now: Instant) -> Result<()> {
        let mut sent = self.sent.lock().unwrap();
        let recent = sent.entry(from.to_string()).or_default();
        while recent
            .front()
            .is_some_and(|at| now.duration_since(*at) >= WINDOW)
        {
            recent.pop_front();
        }
        if recent.len() >= SENDS_PER_MINUTE {
            bail!(
                "not sent: this session has sent {SENDS_PER_MINUTE} messages in the last minute, \
                 the most crystal allows; two sessions answering each other are probably in a \
                 loop. Stop sending and carry on with your own work"
            );
        }
        recent.push_back(now);
        Ok(())
    }

    /// Takes back the last message counted for the session with id `from`,
    /// which couldn't be sent after all.
    pub fn give_back(&self, from: &str) {
        if let Some(recent) = self.sent.lock().unwrap().get_mut(from) {
            recent.pop_back();
        }
    }
}

/// The first line of `text`.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(goal: &str) -> TaskInfo {
        TaskInfo {
            id: Some(3),
            goal: goal.into(),
            background: false,
            backlog: None,
            waiting: false,
            created: 0,
            outcome: None,
            brief: Default::default(),
        }
    }

    fn sender(goal: Option<&str>) -> Sender {
        Sender::new("k3x9", "scout", goal.map(task).as_ref())
    }

    #[test]
    fn a_message_says_which_session_sent_it_and_what_it_works_on() {
        assert_eq!(
            compose(&sender(None), "the codec moved"),
            "[crystal] Message from session \"scout\":\nthe codec moved"
        );
        assert_eq!(
            header(&sender(Some("Port the codec\nto the new crate"))),
            "[crystal] Message from session \"scout\", working on task \"Port the codec\":"
        );
    }

    #[test]
    fn a_closed_task_isn_t_what_a_session_works_on() {
        use crate::protocol::{TaskOutcome, TaskState};
        let closed = TaskInfo {
            outcome: Some(TaskOutcome::new(TaskState::Done, "done", 0)),
            ..task("Port the codec")
        };
        assert_eq!(Sender::new("k3x9", "scout", Some(&closed)).task, None);
    }

    #[test]
    fn a_message_keeps_its_lines_and_loses_its_control_characters() {
        assert_eq!(
            tidy("  look at\r\nsrc/a.rs\t now\u{1b}[31m  \u{7}").unwrap(),
            "look at\nsrc/a.rs  now[31m"
        );
        assert!(tidy(" \u{1b}\n ").is_err());
    }

    #[test]
    fn a_long_message_is_cut_where_it_says_so() {
        let long = "é".repeat(MOST_BYTES);
        let cut = tidy(&long).unwrap();
        assert!(cut.len() <= MOST_BYTES, "{}", cut.len());
        assert!(cut.ends_with("é…"));
        let fits = "x".repeat(MOST_BYTES);
        assert_eq!(tidy(&fits).unwrap(), fits);
    }

    #[test]
    fn a_session_sends_so_many_a_minute_and_no_more() {
        let guard = Guard::default();
        let start = Instant::now();
        for _ in 0..SENDS_PER_MINUTE {
            guard.admit("a", start).unwrap();
        }
        let err = guard.admit("a", start).unwrap_err().to_string();
        assert!(err.contains("20 messages in the last minute"), "{err}");
        // Another session has its own count.
        guard.admit("b", start).unwrap();
        // One that couldn't be sent after all is given back.
        guard.give_back("a");
        guard.admit("a", start).unwrap();
        // A minute on, the first have gone out of the window.
        guard.admit("a", start + WINDOW).unwrap();
    }

    #[test]
    fn a_blocked_agent_says_how_to_answer_it() {
        let task = blocked("fixer", "asking to use Bash: cargo test", true);
        assert!(task.starts_with("agent_blocked: fixer is asking to use Bash: cargo test"));
        assert!(task.contains("crystal answer fixer"), "{task}");
        assert!(task.ends_with("or send again with --force"), "{task}");
        let term = blocked("claude", "asking the user something", false);
        assert!(term.contains("crystal send-keys claude"), "{term}");
    }
}
