//! The environment a session's program starts with: the environment of the
//! `crystal new` that asked for it, not the daemon's, which is only a copy of
//! whatever shell happened to start the daemon.

use std::collections::BTreeMap;
use std::path::Path;

/// Variables that describe where the client runs. Inside a session they'd
/// be wrong: the program runs in crystal's terminal, not the client's, and
/// it isn't a child of the agent that may have run `crystal new`.
const DROPPED: &[&str] = &[
    // The client's terminal. TERMINFO points at that terminal's own
    // terminfo entries, which needn't include the TERM we set.
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
    "TERMINFO",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "VTE_VERSION",
    "WT_SESSION",
    "TMUX",
    "TMUX_PANE",
    // Claude Code marks what it runs with these. A Claude started in a
    // session would take itself for a nested one.
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_PID",
];

/// Whole families of variables that terminals set about themselves.
const DROPPED_PREFIXES: &[&str] = &["ALACRITTY_", "GHOSTTY_", "ITERM_", "KITTY_", "WEZTERM_"];

/// This process's environment. Variables that aren't valid UTF-8 are left
/// out, since they can't be sent as JSON.
pub fn current() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (key, value) in std::env::vars_os() {
        if let (Ok(key), Ok(value)) = (key.into_string(), value.into_string()) {
            env.insert(key, value);
        }
    }
    env
}

/// The client's environment, minus what only made sense where the client
/// runs, plus what tells the program which terminal and session it's in:
/// the session's name as it is when the program starts, and its id, which
/// stays the same when the session is renamed.
pub fn for_session(
    client: &BTreeMap<String, String>,
    name: &str,
    id: &str,
    socket: &Path,
) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = client
        .iter()
        .filter(|(key, _)| !is_dropped(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    env.insert("TERM".into(), "xterm-256color".into());
    env.insert("COLORTERM".into(), "truecolor".into());
    env.insert("CRYSTAL_SESSION".into(), name.into());
    env.insert("CRYSTAL_SESSION_ID".into(), id.into());
    env.insert("CRYSTAL_SOCKET".into(), socket.display().to_string());
    env
}

/// The id of the session this process runs in, when it runs in one of the
/// daemon at `socket`'s sessions. Showing that session here would show it
/// showing itself, endlessly. It's the id rather than the name: the
/// session may have been renamed since this process started.
pub fn own_session_id(socket: &Path) -> Option<String> {
    let session = std::env::var("CRYSTAL_SESSION_ID").ok()?;
    let daemon = std::env::var_os("CRYSTAL_SOCKET")?;
    if Path::new(&daemon) == socket {
        Some(session)
    } else {
        None
    }
}

fn is_dropped(key: &str) -> bool {
    DROPPED.contains(&key)
        || DROPPED_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(vars: &[(&str, &str)]) -> BTreeMap<String, String> {
        vars.iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn the_clients_own_settings_come_through() {
        let env = for_session(
            &client(&[
                ("PATH", "/opt/bin:/usr/bin"),
                ("CLAUDE_CODE_USE_BEDROCK", "1"),
            ]),
            "agent",
            "1a2b",
            Path::new("/tmp/s.sock"),
        );
        assert_eq!(env["PATH"], "/opt/bin:/usr/bin");
        assert_eq!(env["CLAUDE_CODE_USE_BEDROCK"], "1");
    }

    #[test]
    fn what_describes_the_clients_terminal_or_agent_is_dropped() {
        let env = for_session(
            &client(&[
                ("TERMINFO", "/Applications/Ghostty.app/terminfo"),
                ("GHOSTTY_RESOURCES_DIR", "/Applications/Ghostty.app"),
                ("CLAUDECODE", "1"),
                ("CLAUDE_CODE_SESSION_ID", "abc"),
            ]),
            "agent",
            "1a2b",
            Path::new("/tmp/s.sock"),
        );
        for key in [
            "TERMINFO",
            "GHOSTTY_RESOURCES_DIR",
            "CLAUDECODE",
            "CLAUDE_CODE_SESSION_ID",
        ] {
            assert!(!env.contains_key(key), "{key} came through");
        }
    }

    #[test]
    fn the_program_learns_its_terminal_and_session() {
        let env = for_session(
            &client(&[("TERM", "xterm-ghostty")]),
            "agent",
            "1a2b",
            Path::new("/tmp/s.sock"),
        );
        assert_eq!(env["TERM"], "xterm-256color");
        assert_eq!(env["CRYSTAL_SESSION"], "agent");
        assert_eq!(env["CRYSTAL_SESSION_ID"], "1a2b");
        assert_eq!(env["CRYSTAL_SOCKET"], "/tmp/s.sock");
    }
}
