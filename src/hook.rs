//! `crystal hook <agent>`: what an agent's hooks run inside a session. It
//! reads the event the agent passes on stdin and tells the daemon.
//!
//! A hook must never get in its agent's way. This one prints nothing, since
//! an agent may take a hook's output as input, but for the one thing meant
//! as input: the reminder the daemon sends back when an agent ends a turn
//! with its task still open. And it always succeeds: a failing hook can
//! hold the agent up.

use crate::agents;
use crate::client;
use crate::protocol::{Request, Response};
use anyhow::Result;
use std::io::Read;
use std::path::Path;

pub fn run(socket: &Path, agent: &str) {
    // There's nowhere to say what went wrong, and nothing to be done about
    // it: the agent carries on either way.
    let _ = report(socket, agent);
}

fn report(socket: &Path, agent: &str) -> Result<()> {
    // Run outside a session, say by a hook the user copied elsewhere,
    // there's no one to tell.
    let Ok(name) = std::env::var("CRYSTAL_SESSION") else {
        return Ok(());
    };
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let input = serde_json::from_str(&input)?;

    // The name is the one the session had when this program started; the
    // id finds the session even after a rename. Only programs started
    // before sessions had ids go without one.
    let id = std::env::var("CRYSTAL_SESSION_ID").ok();
    let (event, conversation) = match agent {
        "claude" => (
            agents::claude_event(&input),
            agents::claude_conversation(&input),
        ),
        _ => (None, None),
    };
    let Some(event) = event else {
        return Ok(());
    };
    let report = Request::Report {
        name,
        id,
        event,
        conversation,
    };
    if let Some(Response::Remind { text }) = client::ask(socket, &report, false)?
        && agent == "claude"
    {
        println!("{}", agents::claude_keep_going(&text));
    }
    Ok(())
}
