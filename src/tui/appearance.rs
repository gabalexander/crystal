//! Light or dark: the appearance the theme follows while `[appearance]
//! auto_switch` is on. On the user's own machine it's the system's: a
//! Mac's from its defaults, elsewhere the desktop's from its settings
//! portal, or GNOME's, asked again every two seconds, so the theme switches
//! when the system does. Over ssh the system isn't the user's, and some
//! systems can't say: there it's the terminal's, from the background color
//! it answers with as the TUI starts.
//!
//! A terminal can also tell a program its appearance each time it changes
//! (mode 2031), but crystal's input reader, crossterm's, takes that report
//! for the start of a sequence it waits to see the end of, and would
//! swallow the keys after it.

use super::Event;
use std::io::Write;
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

/// How often the system is asked again.
const ASK_EVERY: Duration = Duration::from_secs(2);

/// The longest the system's programs are given to answer.
const SYSTEM_TIMEOUT: Duration = Duration::from_secs(1);

/// The longest the terminal is given to answer, as the TUI starts.
const TERMINAL_TIMEOUT: Duration = Duration::from_millis(300);

impl Appearance {
    /// The appearance of a background `(r, g, b)`: light when it's
    /// lighter than the middle grey, by how bright each channel looks.
    pub fn of_background((r, g, b): (u8, u8, u8)) -> Appearance {
        let brightness = 299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b);
        if brightness >= 128_000 {
            Appearance::Light
        } else {
            Appearance::Dark
        }
    }
}

/// The appearance as the TUI starts: the system's, or where that can't
/// be had, the terminal's. Asking the terminal reads its input, so this
/// must come before anything else does.
pub fn at_start() -> Option<Appearance> {
    if !over_ssh()
        && let Some(appearance) = of_system()
    {
        return Some(appearance);
    }
    of_terminal()
}

/// Has `events` told the system's appearance each time it changes from
/// `last`, while `on` is set, for as long as the TUI runs. Over ssh there's
/// nothing to follow: the system there isn't the user's.
pub fn follow(events: Sender<Event>, on: Arc<AtomicBool>, mut last: Option<Appearance>) {
    if over_ssh() {
        return;
    }
    thread::spawn(move || {
        loop {
            thread::sleep(ASK_EVERY);
            if !on.load(Ordering::Relaxed) {
                continue;
            }
            if let Some(now) = of_system()
                && Some(now) != last
            {
                last = Some(now);
                if events.send(Event::Appearance(now)).is_err() {
                    return;
                }
            }
        }
    });
}

/// Whether the TUI runs on another machine than the user's: set and not
/// empty, as an empty one is how a test says it isn't.
fn over_ssh() -> bool {
    ["SSH_CONNECTION", "SSH_TTY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// The system's appearance, or `None` when it can't say.
pub fn of_system() -> Option<Appearance> {
    if cfg!(target_os = "macos") {
        let output = run(Command::new("defaults").args(["read", "-g", "AppleInterfaceStyle"]))?;
        let said = String::from_utf8_lossy(&output.stdout);
        let why = String::from_utf8_lossy(&output.stderr);
        return mac_says(output.status.success(), &said, &why);
    }
    let portal = run(Command::new("dbus-send").args([
        "--session",
        "--print-reply=literal",
        "--reply-timeout=1000",
        "--dest=org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings.Read",
        "string:org.freedesktop.appearance",
        "string:color-scheme",
    ]));
    let from_portal = portal
        .filter(|output| output.status.success())
        .and_then(|output| portal_says(&String::from_utf8_lossy(&output.stdout)));
    if from_portal.is_some() {
        return from_portal;
    }
    let gnome = run(Command::new("gsettings").args([
        "get",
        "org.gnome.desktop.interface",
        "color-scheme",
    ]))?;
    if !gnome.status.success() {
        return None;
    }
    gnome_says(&String::from_utf8_lossy(&gnome.stdout))
}

/// What `defaults read -g AppleInterfaceStyle` says: `Dark` while it's
/// dark; while it's light, the setting isn't there at all, and it fails
/// saying so.
fn mac_says(succeeded: bool, said: &str, why: &str) -> Option<Appearance> {
    match (succeeded, said.trim()) {
        (true, "Dark") => Some(Appearance::Dark),
        (true, _) => Some(Appearance::Light),
        (false, _) if why.contains("does not exist") => Some(Appearance::Light),
        (false, _) => None,
    }
}

/// What the desktop portal's `color-scheme` says, its number last: 1 for
/// dark, 2 for light, 0 for no preference, which says nothing.
fn portal_says(said: &str) -> Option<Appearance> {
    match said.split_whitespace().last()? {
        "1" => Some(Appearance::Dark),
        "2" => Some(Appearance::Light),
        _ => None,
    }
}

/// What GNOME's `color-scheme` says: `'prefer-dark'`, or `'default'` and
/// `'prefer-light'`, which are both light.
fn gnome_says(said: &str) -> Option<Appearance> {
    match said.trim().trim_matches('\'') {
        "prefer-dark" => Some(Appearance::Dark),
        "prefer-light" | "default" => Some(Appearance::Light),
        _ => None,
    }
}

/// Runs `command` for what it prints; `None` when it isn't there or
/// doesn't finish in [`SYSTEM_TIMEOUT`].
fn run(command: &mut Command) -> Option<Output> {
    super::status_bar::run_within(command, SYSTEM_TIMEOUT)
}

/// The terminal's appearance, by the background color it says it has, or
/// `None` when it doesn't say in time.
fn of_terminal() -> Option<Appearance> {
    crossterm::terminal::enable_raw_mode().ok()?;
    let heard = ask_terminal();
    let _ = crossterm::terminal::disable_raw_mode();
    background_in(&heard?).map(Appearance::of_background)
}

/// Asks the terminal for its background, then for its attributes, which
/// every terminal answers: once that answer is in, one that hasn't said
/// its background won't. Gives back what it answered.
fn ask_terminal() -> Option<Vec<u8>> {
    let mut out = std::io::stdout();
    out.write_all(b"\x1b]11;?\x1b\\\x1b[c").ok()?;
    out.flush().ok()?;
    let deadline = Instant::now() + TERMINAL_TIMEOUT;
    let mut heard = Vec::new();
    let mut buf = [0u8; 512];
    while !answered_attributes(&heard) {
        let left = deadline.saturating_duration_since(Instant::now());
        let mut poll = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, and a count of one.
        let ready = unsafe { libc::poll(&mut poll, 1, left.as_millis() as libc::c_int) };
        if ready <= 0 {
            break;
        }
        // Read from the descriptor itself: standard input's buffer would
        // keep what comes after, from the reader that takes the keys.
        // SAFETY: reading into a buffer of the length given.
        let read = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
        if read <= 0 {
            break;
        }
        heard.extend_from_slice(&buf[..read as usize]);
    }
    Some(heard)
}

/// Whether `heard` holds the terminal's answer about its attributes:
/// `ESC [ ?` … `c`.
fn answered_attributes(heard: &[u8]) -> bool {
    heard
        .windows(3)
        .position(|window| window == b"\x1b[?")
        .is_some_and(|at| heard[at..].contains(&b'c'))
}

/// The background color in a terminal's answer to `OSC 11 ; ?`:
/// `ESC ] 11 ; rgb:RRRR/GGGG/BBBB`, ended by `ESC \` or `BEL`, each channel
/// one to four hex digits.
fn background_in(heard: &[u8]) -> Option<(u8, u8, u8)> {
    let text = String::from_utf8_lossy(heard);
    let start = text.find("\x1b]11;")? + "\x1b]11;".len();
    let rest = &text[start..];
    let end = rest.find(['\x1b', '\x07'])?;
    let color = rest[..end].strip_prefix("rgb:")?;
    let mut channels = color.split('/').map(channel);
    let rgb = (channels.next()??, channels.next()??, channels.next()??);
    channels.next().is_none().then_some(rgb)
}

/// One channel, of one to four hex digits, scaled to a byte.
fn channel(digits: &str) -> Option<u8> {
    if digits.is_empty() || digits.len() > 4 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    let most = (1u32 << (4 * digits.len())) - 1;
    Some(((value * 255 + most / 2) / most) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mac_is_dark_when_it_says_so_and_light_when_it_has_no_style() {
        assert_eq!(mac_says(true, "Dark\n", ""), Some(Appearance::Dark));
        let missing = "The domain/default pair of (kCFPreferencesAnyApplication, \
                       AppleInterfaceStyle) does not exist";
        assert_eq!(mac_says(false, "", missing), Some(Appearance::Light));
        assert_eq!(mac_says(false, "", "defaults: killed"), None);
    }

    #[test]
    fn the_portal_says_dark_light_or_nothing() {
        assert_eq!(
            portal_says("   variant       variant          uint32 1\n"),
            Some(Appearance::Dark)
        );
        assert_eq!(portal_says("variant uint32 2"), Some(Appearance::Light));
        assert_eq!(
            portal_says("   variant       variant          uint32 0"),
            None
        );
        assert_eq!(portal_says(""), None);
    }

    #[test]
    fn gnome_prefers_dark_or_is_light() {
        assert_eq!(gnome_says("'prefer-dark'\n"), Some(Appearance::Dark));
        assert_eq!(gnome_says("'default'\n"), Some(Appearance::Light));
        assert_eq!(gnome_says("'prefer-light'"), Some(Appearance::Light));
        assert_eq!(gnome_says("No such key"), None);
    }

    #[test]
    fn a_background_is_light_or_dark_by_how_bright_it_looks() {
        assert_eq!(Appearance::of_background((30, 30, 46)), Appearance::Dark);
        assert_eq!(
            Appearance::of_background((239, 241, 245)),
            Appearance::Light
        );
        // Pure blue looks dark, pure green light.
        assert_eq!(Appearance::of_background((0, 0, 255)), Appearance::Dark);
        assert_eq!(Appearance::of_background((0, 255, 0)), Appearance::Light);
    }

    #[test]
    fn the_terminals_background_is_read_from_its_answer() {
        let answer = b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[?62;22c";
        assert!(answered_attributes(answer));
        assert_eq!(background_in(answer), Some((30, 30, 46)));
        assert_eq!(
            background_in(b"\x1b]11;rgb:ef/f1/f5\x07"),
            Some((239, 241, 245))
        );
        assert_eq!(
            background_in(b"\x1b]11;rgb:f/f/f\x07"),
            Some((255, 255, 255))
        );
        // A terminal that only says its attributes says nothing of it.
        assert!(answered_attributes(b"\x1b[?1;2c"));
        assert_eq!(background_in(b"\x1b[?1;2c"), None);
        assert!(!answered_attributes(b"\x1b]11;rgb:0/0/0\x1b\\"));
        for broken in [
            &b"\x1b]11;rgb:12/34\x07"[..],
            b"\x1b]11;rgb:12/34/56/78\x07",
            b"\x1b]11;#123456\x07",
            b"\x1b]11;rgb:zz/00/00\x07",
            b"\x1b]11;rgb:00/00/00",
        ] {
            assert_eq!(background_in(broken), None, "{broken:?}");
        }
    }
}
