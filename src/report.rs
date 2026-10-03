//! `crystal report`: any agent, or a script wrapped around one, telling
//! crystal what it's doing and how to pick its session up again after a
//! restart. crystal knows Claude Code by its hooks and reads other agents
//! off their screens; an agent that reports for itself needs neither. Its
//! first report takes the session's status over, and it holds the session
//! until it lets go with `--release`, or leaves and the shell is back in
//! front.
//!
//! After a restart, the command it gave runs in the session's directory:
//! typed into the session's shell when the session runs one, or else in
//! place of the session's command. Typed into a shell, it has to read the
//! same in any of them, so it starts with a plain command name, and none of
//! its words holds a quote or a control character: quoted as
//! [`shell::quote`] does, the rest reads the same in sh, bash, zsh, fish
//! and the others.

use crate::client;
use crate::protocol::{Activity, AgentEvent, AgentReport, Request, Response};
use crate::shell;
use crate::typing;
use crate::work;
use anyhow::{Result, bail, ensure};
use std::path::Path;

/// The most words a resume command may have.
const MOST_WORDS: usize = 64;

/// The most bytes its words may take together.
const MOST_BYTES: usize = 8 * 1024;

/// The longest name an agent may give itself.
const LONGEST_AGENT: usize = 40;

/// Tells the daemon what the agent in the session called `name`, or the
/// one this runs in, says about itself.
pub fn run(socket: &Path, name: Option<String>, report: AgentReport) -> Result<()> {
    let id = work::own_session(socket, &name, "which session it's about")?;
    let request = Request::ReportAgent { id, name, report };
    match client::ask(socket, &request, false)? {
        Some(Response::Done) => Ok(()),
        Some(_) => bail!("the daemon answered something else"),
        None => bail!("no daemon is running on {}", socket.display()),
    }
}

/// Refuses a resume command that can't be typed into any shell and read
/// the same: one that doesn't start with a plain command name, has a quote
/// or a control character in it, or is too long.
pub fn check_resume(argv: &[String]) -> Result<()> {
    let Some(program) = argv.first() else {
        bail!("the resume command is empty");
    };
    ensure!(
        argv.len() <= MOST_WORDS,
        "the resume command has more than {MOST_WORDS} words"
    );
    ensure!(
        argv.iter().map(String::len).sum::<usize>() <= MOST_BYTES,
        "the resume command is longer than {MOST_BYTES} bytes"
    );
    ensure!(
        !argv.iter().any(|arg| arg.chars().any(char::is_control)),
        "the resume command has a control character in it"
    );
    ensure!(
        !argv.iter().any(|arg| arg.contains('\'')),
        "the resume command has a quote in it, which not every shell reads the same"
    );
    let plain = |c: char| c.is_ascii_alphanumeric() || "_-.".contains(c);
    ensure!(
        !program.starts_with('-') && program.chars().all(plain),
        "the resume command has to start with a command's name, found on the PATH, not {program}"
    );
    Ok(())
}

/// The name an agent gave itself, once it's one word that fits in the
/// sidebar.
pub fn checked_agent(agent: String) -> Result<String> {
    let agent = agent.trim().to_string();
    ensure!(
        !agent.is_empty() && !agent.contains(|c: char| c.is_whitespace() || c.is_control()),
        "an agent's name is one word"
    );
    ensure!(
        agent.chars().count() <= LONGEST_AGENT,
        "an agent's name is at most {LONGEST_AGENT} characters"
    );
    Ok(agent)
}

/// What an agent saying it's doing `state` means, given what it was doing:
/// `idle` after working ends a turn, which a person may not have seen yet,
/// and otherwise only says it's at its prompt.
pub fn event(state: Activity, before: Option<Activity>) -> AgentEvent {
    match state {
        Activity::Working => AgentEvent::TurnStarted,
        Activity::Waiting => AgentEvent::Asking,
        Activity::Done => AgentEvent::TurnEnded,
        Activity::Idle if before == Some(Activity::Working) => AgentEvent::TurnEnded,
        Activity::Idle => AgentEvent::Started,
    }
}

/// What's typed into a shell to run `argv`: the words quoted where they
/// need it, then Enter.
pub fn typed(argv: &[String]) -> Vec<u8> {
    let words: Vec<String> = argv.iter().map(|arg| shell::quote(arg)).collect();
    let mut line = words.join(" ").into_bytes();
    line.extend_from_slice(typing::ENTER);
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn a_resume_command_starts_with_a_plain_name() {
        assert!(check_resume(&argv(&["pi", "--session", "a b", "--model=x"])).is_ok());
        assert!(check_resume(&argv(&["my-agent.sh", "resume"])).is_ok());
        for bad in [
            argv(&[]),
            argv(&["/usr/bin/pi"]),
            argv(&["./pi"]),
            argv(&["-pi"]),
            argv(&["pi agent"]),
            argv(&["pi", "it's"]),
            argv(&["pi", "two\nlines"]),
            argv(&["pi", "\u{1b}[31m"]),
        ] {
            assert!(check_resume(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_resume_command_is_kept_short() {
        assert!(check_resume(&vec!["pi".to_string(); MOST_WORDS]).is_ok());
        assert!(check_resume(&vec!["pi".to_string(); MOST_WORDS + 1]).is_err());
        assert!(check_resume(&argv(&["pi", &"x".repeat(MOST_BYTES)])).is_err());
    }

    #[test]
    fn an_agent_s_name_is_one_short_word() {
        assert_eq!(checked_agent(" pi ".into()).unwrap(), "pi");
        assert!(checked_agent("".into()).is_err());
        assert!(checked_agent("my agent".into()).is_err());
        assert!(checked_agent("x".repeat(LONGEST_AGENT + 1)).is_err());
    }

    #[test]
    fn idle_after_working_ends_a_turn_and_otherwise_rests() {
        use Activity::*;
        assert_eq!(event(Working, None), AgentEvent::TurnStarted);
        assert_eq!(event(Waiting, Some(Working)), AgentEvent::Asking);
        assert_eq!(event(Done, Some(Idle)), AgentEvent::TurnEnded);
        assert_eq!(event(Idle, Some(Working)), AgentEvent::TurnEnded);
        assert_eq!(event(Idle, Some(Waiting)), AgentEvent::Started);
        assert_eq!(event(Idle, None), AgentEvent::Started);
    }

    #[test]
    fn a_resume_command_is_typed_quoted_then_entered() {
        assert_eq!(
            typed(&argv(&["pi", "--session", "a b"])),
            b"pi --session 'a b'\r"
        );
    }
}
