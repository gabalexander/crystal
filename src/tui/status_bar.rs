//! What the tab bar shows at its right, after the sessions' count:
//! `[tab_bar] right`, in order, `[tab_bar] separator` between two. This
//! machine's name; the time, as `strftime` writes a format; text; and the
//! last line a shell command prints, run again on an interval and stopped
//! when it takes too long, its colors and the like taken out. Each is
//! worked out off the event loop, on threads that stop when the settings
//! change, and the event loop is told only when what's shown changes: the
//! clock, a command's answer.

use super::Event;
use super::window;
use crate::config::StatusEntry;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How often what's shown is worked out again: often enough for a clock
/// that shows seconds.
const TICK: Duration = Duration::from_millis(500);

/// The most columns a command's answer takes.
const LONGEST: usize = 60;

/// The threads working out what the bar shows, for as long as it lives.
pub struct Watch {
    stop: Arc<AtomicBool>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Starts working out `entries`, telling `events` what each shows as it
/// changes; commands run in `dir`, reaching the daemon at `socket`. `None`
/// when there's nothing to show.
pub fn watch(
    entries: &[StatusEntry],
    dir: PathBuf,
    socket: &Path,
    events: Sender<Event>,
) -> Option<Watch> {
    if entries.is_empty() {
        return None;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let answers = Arc::new(Mutex::new(vec![String::new(); entries.len()]));
    for (index, entry) in entries.iter().enumerate() {
        let (StatusEntry::Command { command, .. }, Some((every, timeout))) =
            (entry, entry.command_times())
        else {
            continue;
        };
        let run = Run {
            line: command.clone(),
            dir: dir.clone(),
            socket: socket.to_path_buf(),
            every,
            timeout,
        };
        let (stop, answers) = (stop.clone(), answers.clone());
        thread::spawn(move || run.again_and_again(index, &answers, &stop));
    }
    let entries = entries.to_vec();
    let watching = stop.clone();
    thread::spawn(move || {
        let hostname = window::hostname();
        let mut shown: Option<Vec<String>> = None;
        while !watching.load(Ordering::Relaxed) {
            let now = now();
            let answers = answers.lock().unwrap().clone();
            let texts: Vec<String> = entries
                .iter()
                .zip(answers)
                .map(|(entry, answer)| match entry {
                    StatusEntry::Hostname {} => hostname.clone(),
                    StatusEntry::Clock { format } => clock(format, now),
                    StatusEntry::Text { text } => text.clone(),
                    StatusEntry::Command { .. } => answer,
                })
                .collect();
            if shown.as_ref() != Some(&texts) {
                if events.send(Event::Status(texts.clone())).is_err() {
                    return;
                }
                shown = Some(texts);
            }
            thread::sleep(TICK);
        }
    });
    Some(Watch { stop })
}

/// A command entry, run again and again.
struct Run {
    line: String,
    dir: PathBuf,
    socket: PathBuf,
    every: Duration,
    timeout: Duration,
}

impl Run {
    /// Runs it every while, putting its answer at `index` of `answers`,
    /// until `stop` is set.
    fn again_and_again(&self, index: usize, answers: &Mutex<Vec<String>>, stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            let answer = self.once();
            if stop.load(Ordering::Relaxed) {
                return;
            }
            answers.lock().unwrap()[index] = answer;
            let next = Instant::now() + self.every;
            while Instant::now() < next {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(TICK.min(next - Instant::now()));
            }
        }
    }

    /// Its answer: the last line it printed, or nothing when it failed or
    /// took too long.
    fn once(&self) -> String {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(&self.line)
            .current_dir(&self.dir)
            .env("CRYSTAL_SOCKET", &self.socket);
        match run_within(&mut command, self.timeout) {
            Some(output) if output.status.success() => {
                last_line(&String::from_utf8_lossy(&output.stdout))
            }
            _ => String::new(),
        }
    }
}

/// Runs `command` for what it prints, stopped, with whatever it started,
/// once it has taken `timeout`; `None` when it can't start or doesn't
/// finish in time.
pub fn run_within(command: &mut Command, timeout: Duration) -> Option<Output> {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .ok()?;
    let id = child.id();
    let (sender, finished) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    match finished.recv_timeout(timeout) {
        Ok(output) => output.ok(),
        Err(_) => {
            // SAFETY: a signal to the process group the child leads, which
            // its own children are in too, so none keeps its output open.
            unsafe { libc::kill(-(id as libc::pid_t), libc::SIGKILL) };
            None
        }
    }
}

/// The last line of `output` with something on it, its escape sequences
/// and other control characters taken out, cut to [`LONGEST`] characters.
pub fn last_line(output: &str) -> String {
    let line = output
        .lines()
        .map(|line| without_escapes(line).trim().to_string())
        .rfind(|line| !line.is_empty())
        .unwrap_or_default();
    line.chars().take(LONGEST).collect()
}

/// `text` without its escape sequences, its colors and the like, and
/// without control characters.
fn without_escapes(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if !c.is_control() {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            // CSI: parameters, then a final byte from `@` to `~`.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ESC `\`.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        chars.next_if_eq(&'\\');
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Seconds since the Unix epoch.
fn now() -> libc::time_t {
    // SAFETY: time with no pointer to fill.
    unsafe { libc::time(std::ptr::null_mut()) }
}

/// The time `at`, on this machine's clock, as `strftime` writes `format`;
/// empty when it comes out too long.
pub fn clock(format: &str, at: libc::time_t) -> String {
    // SAFETY: an all-zero tm is a valid one to fill.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: valid pointers to a time and a tm to fill.
    if unsafe { libc::localtime_r(&at, &mut tm) }.is_null() {
        return String::new();
    }
    written(format, &tm)
}

/// `tm` as `strftime` writes `format`.
fn written(format: &str, tm: &libc::tm) -> String {
    let Ok(format) = std::ffi::CString::new(format) else {
        return String::new();
    };
    let mut buf = [0u8; 128];
    // SAFETY: a buffer of the length given, a nul-ended format and a tm.
    let length = unsafe { libc::strftime(buf.as_mut_ptr().cast(), buf.len(), format.as_ptr(), tm) };
    String::from_utf8_lossy(&buf[..length]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_shows_its_last_line_with_something_on_it() {
        assert_eq!(last_line("one\ntwo\n\n  \n"), "two");
        assert_eq!(last_line(""), "");
        assert_eq!(last_line("\x1b[1;32mgreen\x1b[0m ok\r\n"), "green ok");
        assert_eq!(last_line("\x1b]0;title\x07after"), "after");
        assert_eq!(
            last_line("\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"),
            "link"
        );
        assert_eq!(last_line("tab\there"), "tabhere");
        assert_eq!(last_line(&"x".repeat(100)).len(), LONGEST);
    }

    #[test]
    fn the_time_is_written_as_strftime_writes_it() {
        // SAFETY: an all-zero tm is a valid one to fill.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let at: libc::time_t = 1_790_000_000;
        // SAFETY: valid pointers to a time and a tm to fill.
        unsafe { libc::gmtime_r(&at, &mut tm) };
        assert_eq!(written("%Y-%m-%d %H:%M:%S", &tm), "2026-09-21 14:13:20");
        assert_eq!(written("%a %%", &tm), "Mon %");
        assert_eq!(written(&"%Y".repeat(100), &tm), "");
        assert_eq!(written("a\0b", &tm), "");
        assert!(!clock("%H:%M", at).is_empty());
    }

    #[test]
    fn a_command_that_takes_too_long_is_stopped_with_what_it_started() {
        let started = Instant::now();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 5 | cat; echo late"]);
        assert!(run_within(&mut command, Duration::from_millis(200)).is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo early"]);
        let output = run_within(&mut command, Duration::from_secs(5)).unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "early\n");
    }
}
