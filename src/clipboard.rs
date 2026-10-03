//! Putting text on the user's clipboard. On their own machine, that's the
//! system's clipboard, through `pbcopy`, `wl-copy`, `xclip` or `xsel`. Over
//! ssh, or where none of those works, crystal asks the terminal it's drawn
//! in, with OSC 52, which puts the text on the clipboard of the machine the
//! terminal runs on. Most terminals do it; a few, like macOS's Terminal,
//! don't, and some, like iTerm2, only once the user allows it.

use anyhow::{Result, bail};
use std::ffi::OsStr;
use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How long a clipboard program gets before the terminal is asked instead.
const PROGRAM_TIMEOUT: Duration = Duration::from_secs(2);

/// Puts `text` on the clipboard.
pub fn copy(text: &str) -> Result<()> {
    if !over_ssh(
        std::env::var_os("SSH_CONNECTION").as_deref(),
        std::env::var_os("SSH_TTY").as_deref(),
    ) {
        for program in programs(cfg!(target_os = "macos"), &has) {
            if run(program, text).is_ok() {
                return Ok(());
            }
        }
    }
    let mut out = std::io::stdout();
    out.write_all(osc52(text).as_bytes())?;
    out.flush()?;
    Ok(())
}

/// Whether crystal runs on another machine than the user's, over ssh: its
/// clipboard programs would fill that machine's clipboard, not theirs.
fn over_ssh(connection: Option<&OsStr>, tty: Option<&OsStr>) -> bool {
    connection.is_some() || tty.is_some()
}

/// Whether the environment variable `name` is set to something.
fn has(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// The programs that put text on the system's clipboard, in the order to
/// try them: on macOS `pbcopy`, and on Linux Wayland's, then X's, for the
/// displays `has` says there are.
fn programs(macos: bool, has: &dyn Fn(&str) -> bool) -> Vec<&'static [&'static str]> {
    if macos {
        return vec![&["pbcopy"]];
    }
    let mut programs: Vec<&'static [&'static str]> = Vec::new();
    if has("WAYLAND_DISPLAY") {
        programs.push(&["wl-copy"]);
    }
    if has("DISPLAY") {
        programs.push(&["xclip", "-selection", "clipboard"]);
        programs.push(&["xsel", "--clipboard", "--input"]);
    }
    programs
}

/// Hands `text` to `program`, and waits for it to take it. Its output goes
/// nowhere: `xclip` and `wl-copy` leave a process behind to hold the
/// clipboard, which would keep a pipe open.
fn run(program: &[&str], text: &str) -> Result<()> {
    let (name, args) = program.split_first().expect("a program has a name");
    let mut child = Command::new(name)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Dropping stdin once it's written closes it, which tells the program
    // that's all.
    let written = child
        .stdin
        .take()
        .map(|mut stdin| stdin.write_all(text.as_bytes()));
    if let Some(Err(err)) = written {
        let _ = child.kill();
        let _ = child.wait();
        return Err(err.into());
    }
    let deadline = Instant::now() + PROGRAM_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            bail!("{name} {status}");
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{name} took too long");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// The OSC 52 sequence that asks a terminal to put `text` on its
/// clipboard. It ends with BEL, which more terminals know than ST.
fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// `bytes` in base64, padded, as OSC 52 takes them.
fn base64(bytes: &[u8]) -> String {
    const LETTERS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &byte)| n | u32::from(byte) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(LETTERS[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_is_padded_to_whole_groups() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64("中".as_bytes()), "5Lit");
        assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn osc52_asks_for_the_clipboard_and_ends_with_bel() {
        assert_eq!(osc52("hello"), "\x1b]52;c;aGVsbG8=\x07");
    }

    #[test]
    fn over_ssh_the_terminal_is_asked_rather_than_the_machine() {
        let set = Some(OsStr::new("1"));
        assert!(over_ssh(set, None));
        assert!(over_ssh(None, set));
        assert!(!over_ssh(None, None));
    }

    #[test]
    fn macos_has_pbcopy_and_linux_has_its_displays_programs() {
        assert_eq!(programs(true, &|_| false), [&["pbcopy"][..]]);
        assert!(programs(false, &|_| false).is_empty());
        let wayland = programs(false, &|name| name == "WAYLAND_DISPLAY");
        assert_eq!(wayland, [&["wl-copy"][..]]);
        let both = programs(false, &|_| true);
        let names: Vec<&str> = both.iter().map(|program| program[0]).collect();
        assert_eq!(names, ["wl-copy", "xclip", "xsel"]);
    }
}
