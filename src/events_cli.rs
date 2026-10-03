//! `crystal events`: the event log in a shell, one line an event, the
//! oldest first, or followed as it grows.

use crate::client;
use crate::event_log;
use crate::events::{self, Event, Filter, Since, now_ms};
use crate::project;
use crate::protocol::{Request, Response};
use anyhow::{Result, bail};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// What `crystal events` was asked for.
pub struct Options {
    /// A while back, like `2h`, or a time, like `14:00`.
    pub since: Option<String>,
    pub kinds: Vec<String>,
    pub session: Option<String>,
    /// The project this directory is in.
    pub dir: Option<PathBuf>,
    pub json: bool,
    pub follow: bool,
}

/// Prints the events in the log that `options` asks for, and with
/// `follow`, each new one as it happens: after those since `--since`, or
/// without it, only new ones.
pub fn run(socket: &Path, options: Options) -> Result<()> {
    for kind in &options.kinds {
        events::check_pattern(kind)?;
    }
    let since = match &options.since {
        Some(when) => Some(Since::At(parse_since(when, now_ms())?)),
        None => None,
    };
    let filter = Filter {
        kinds: options.kinds,
        session: options.session.map(|name| session_key(socket, name)),
        project: options.dir.map(|dir| project::of(&dir).path),
    };
    let json = options.json;
    if options.follow {
        for event in client::subscribe(socket, filter, since)? {
            if !print(&event?, json)? {
                break;
            }
        }
        return Ok(());
    }
    for event in event_log::read(socket, &filter, since.unwrap_or(Since::Seq(0)))? {
        if !print(&event, json)? {
            break;
        }
    }
    Ok(())
}

/// Prints `event`, as a line or as JSON, and says whether to go on: not
/// once whatever reads what's printed, like `head`, has had enough.
fn print(event: &Event, json: bool) -> Result<bool> {
    let text = if json {
        serde_json::to_string(event)?
    } else {
        line(event, now_ms())
    };
    match writeln!(std::io::stdout(), "{text}") {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == ErrorKind::BrokenPipe => Ok(false),
        Err(err) => Err(err.into()),
    }
}

/// What `-n name` looks for: the id of the session called `name` now, so
/// its events from before a rename count too, or else the name, for one
/// that has gone.
fn session_key(socket: &Path, name: String) -> String {
    let Ok(Some(Response::Sessions { sessions })) = client::ask(socket, &Request::List, false)
    else {
        return name;
    };
    sessions
        .into_iter()
        .find(|session| session.name == name)
        .map_or(name, |session| session.id)
}

/// An event as `crystal events` prints it: when, what, about what, and
/// how it went.
fn line(event: &Event, now: u64) -> String {
    let line = format!(
        "{:<11}  {:<18}  {}  {}",
        when(event.at, now),
        event.kind.name(),
        event.subject(),
        event.text()
    );
    line.trim_end().to_string()
}

/// When something happened, in milliseconds since the Unix epoch, as
/// short as it can be said from `now`: `14:03:07` today, `09-24 14:03`
/// earlier this year, and the date before that. The TUI's timeline says it
/// the same way.
pub fn when(at: u64, now: u64) -> String {
    let at = local(at);
    let now = local(now);
    let (year, month, day) = (at.tm_year + 1900, at.tm_mon + 1, at.tm_mday);
    if at.tm_year != now.tm_year {
        format!("{year:04}-{month:02}-{day:02}")
    } else if (at.tm_mon, at.tm_mday) != (now.tm_mon, now.tm_mday) {
        format!("{month:02}-{day:02} {:02}:{:02}", at.tm_hour, at.tm_min)
    } else {
        format!("{:02}:{:02}:{:02}", at.tm_hour, at.tm_min, at.tm_sec)
    }
}

/// When `--since` means, in milliseconds since the Unix epoch, from `now`:
/// a while back, like `90s`, `30m`, `2h`, `3d` or `1w`, or a time on this
/// machine's clock, like `14:00` today, `2026-10-01` or
/// `2026-10-01T09:30`.
fn parse_since(when: &str, now: u64) -> Result<u64> {
    if let Some(back) = parse_while(when) {
        return Ok(now.saturating_sub(back));
    }
    if let Some(at) = parse_time(when, now) {
        return Ok(at);
    }
    bail!(
        "`{when}` is neither a while back, like 30m, 2h or 3d, \
         nor a time, like 14:00, 2026-10-01 or 2026-10-01T09:30"
    )
}

/// A while, like `30m`, in milliseconds.
fn parse_while(text: &str) -> Option<u64> {
    let unit = text.chars().last()?;
    let count: u64 = text[..text.len() - unit.len_utf8()].parse().ok()?;
    let seconds = match unit {
        's' => 1,
        'm' => 60,
        'h' => 60 * 60,
        'd' => 24 * 60 * 60,
        'w' => 7 * 24 * 60 * 60,
        _ => return None,
    };
    Some(count * seconds * 1000)
}

/// A time on this machine's clock, in milliseconds since the Unix epoch:
/// a date, a time of day today, or both, with a `T` or a space between.
fn parse_time(text: &str, now: u64) -> Option<u64> {
    let (date, time) = match text.split_once(['T', ' ']) {
        Some((date, time)) => (Some(date), Some(time)),
        None if text.contains('-') => (Some(text), None),
        None => (None, Some(text)),
    };
    let today = local(now);
    let (year, month, day) = match date {
        Some(date) => {
            let parts = numbers(date, '-')?;
            let [year, month, day] = parts[..] else {
                return None;
            };
            (year, month, day)
        }
        None => (today.tm_year + 1900, today.tm_mon + 1, today.tm_mday),
    };
    let (hour, minute, second) = match time {
        Some(time) => match numbers(time, ':')?[..] {
            [hour, minute] => (hour, minute, 0),
            [hour, minute, second] => (hour, minute, second),
            _ => return None,
        },
        None => (0, 0, 0),
    };
    let fits = (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && (0..24).contains(&hour)
        && (0..60).contains(&minute)
        && (0..60).contains(&second);
    fits.then(|| from_local(year, month, day, hour, minute, second))
        .flatten()
}

/// The numbers `text` holds between `separator`s, or `None` if anything
/// else is there.
fn numbers(text: &str, separator: char) -> Option<Vec<i32>> {
    text.split(separator)
        .map(|part| part.parse().ok())
        .collect()
}

/// `ms` since the Unix epoch, on this machine's clock.
fn local(ms: u64) -> libc::tm {
    let seconds = (ms / 1000) as libc::time_t;
    // SAFETY: an all-zero tm is a valid value for localtime_r to fill in,
    // and both pointers live for the whole call.
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&seconds, &mut local) };
    local
}

/// A time on this machine's clock, in milliseconds since the Unix epoch.
fn from_local(year: i32, month: i32, day: i32, hour: i32, minute: i32, second: i32) -> Option<u64> {
    // SAFETY: an all-zero tm is a valid value to fill in, and mktime only
    // reads it and fixes it up.
    let mut time: libc::tm = unsafe { std::mem::zeroed() };
    time.tm_year = year - 1900;
    time.tm_mon = month - 1;
    time.tm_mday = day;
    time.tm_hour = hour;
    time.tm_min = minute;
    time.tm_sec = second;
    // Whether daylight saving was on then is for mktime to work out.
    time.tm_isdst = -1;
    let seconds = unsafe { libc::mktime(&mut time) };
    u64::try_from(seconds).ok().map(|seconds| seconds * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_while_back_is_counted_from_now() {
        let now = 10_000_000_000;
        assert_eq!(parse_since("90s", now).unwrap(), now - 90_000);
        assert_eq!(parse_since("2h", now).unwrap(), now - 7_200_000);
        assert_eq!(parse_since("1w", now).unwrap(), now - 604_800_000);
    }

    #[test]
    fn a_time_is_on_this_machines_clock() {
        let now = from_local(2026, 10, 3, 15, 30, 0).unwrap();
        let today = |hour, minute| from_local(2026, 10, 3, hour, minute, 0).unwrap();
        assert_eq!(parse_since("14:00", now).unwrap(), today(14, 0));
        assert_eq!(parse_since("2026-10-03", now).unwrap(), today(0, 0));
        assert_eq!(parse_since("2026-10-03T09:30", now).unwrap(), today(9, 30));
        assert_eq!(
            parse_since("2026-10-03 09:30:15", now).unwrap(),
            today(9, 30) + 15_000
        );
    }

    #[test]
    fn what_isnt_a_while_or_a_time_says_what_would_be() {
        for nonsense in ["yesterday", "5x", "25:00", "2026-13-01", "m", ""] {
            let err = parse_since(nonsense, 0).unwrap_err();
            assert!(err.to_string().contains("nor a time"), "{nonsense}: {err}");
        }
    }

    #[test]
    fn when_is_as_short_as_it_can_be() {
        let now = from_local(2026, 10, 3, 15, 30, 0).unwrap();
        assert_eq!(
            when(from_local(2026, 10, 3, 9, 5, 7).unwrap(), now),
            "09:05:07"
        );
        assert_eq!(
            when(from_local(2026, 9, 24, 14, 3, 0).unwrap(), now),
            "09-24 14:03"
        );
        assert_eq!(
            when(from_local(2025, 9, 24, 14, 3, 0).unwrap(), now),
            "2025-09-24"
        );
    }
}
