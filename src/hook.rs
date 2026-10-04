//! `crystal hook <agent>`: what an agent's hooks run inside a session. It
//! reads the event the agent passes on stdin and tells the daemon.
//!
//! A hook must never get in its agent's way. This one prints nothing, since
//! an agent may take a hook's output as input, but for the one thing meant
//! as input: the reminder the daemon sends back when an agent ends a turn
//! with its task still open. And it always succeeds: a failing hook can
//! hold the agent up.
//!
//! The hooks crystal adds as it starts Claude Code run `crystal hook
//! claude`; those `crystal integration` puts in an agent's own settings run
//! it with `--installed`, for an agent typed into a session's shell, or one
//! like Codex that takes hooks from nowhere else.

use crate::agents;
use crate::client;
use crate::protocol::{AgentEvent, Request, Response};
use anyhow::Result;
use std::io::Read;
use std::path::Path;

pub fn run(socket: &Path, agent: &str, installed: bool) {
    // There's nowhere to say what went wrong, and nothing to be done about
    // it: the agent carries on either way.
    let _ = report(socket, agent, installed);
}

fn report(socket: &Path, agent: &str, installed: bool) -> Result<()> {
    // Run outside a session, say by a hook the user copied elsewhere, or
    // one installed in their settings for an agent they started outside
    // crystal, there's no one to tell.
    let Ok(name) = std::env::var("CRYSTAL_SESSION") else {
        return Ok(());
    };
    // An agent crystal started with hooks of its own reports through
    // those.
    if installed && std::env::var(agents::HOOKED).is_ok_and(|hooked| hooked == agent) {
        return Ok(());
    }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let input = serde_json::from_str(&input)?;

    // The name is the one the session had when this program started; the
    // id finds the session even after a rename. Only programs started
    // before sessions had ids go without one.
    let id = std::env::var("CRYSTAL_SESSION_ID").ok();
    let (event, conversation) = match agent {
        "codex" => (
            agents::codex_event(&input),
            agents::codex_conversation(&input),
        ),
        _ => (
            agents::hook_event(&input),
            agents::hook_conversation(&input),
        ),
    };
    let Some(event) = event else {
        return Ok(());
    };
    let about_subagent = matches!(
        event,
        AgentEvent::SubagentStarted | AgentEvent::SubagentStopped
    );
    let report = Request::Report {
        name,
        id,
        event,
        conversation,
        prompt: agents::hook_prompt(&input),
        agent: Some(agent.to_string()),
        cwd: agents::hook_cwd(&input),
        subagent: agents::subagent(&input).filter(|_| about_subagent),
        model: agents::hook_model(&input),
    };
    // Codex's Stop hook takes the same answer as Claude Code's.
    if let Some(Response::Remind { text }) = client::ask(socket, &report, false)? {
        println!("{}", agents::claude_keep_going(&text));
    }
    Ok(())
}
