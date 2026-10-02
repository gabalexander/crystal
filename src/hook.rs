//! `crystal hook <agent>`: what an agent's hooks run inside a session. It
//! reads the event the agent passes on stdin and tells the daemon.
//!
//! A hook must never get in its agent's way. This one prints nothing, since
//! an agent may take a hook's output as input, and it always succeeds: a
//! failing hook can hold the agent up.

use crate::agents;
use crate::client;
use crate::protocol::Request;
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

    let event = match agent {
        "claude" => agents::claude_event(&input),
        _ => None,
    };
    if let Some(event) = event {
        client::ask(socket, &Request::Report { name, event }, false)?;
    }
    Ok(())
}
