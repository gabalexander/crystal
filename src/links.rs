//! Opening a link a pane shows: in the browser, with `open` on macOS or
//! `xdg-open` on Linux. Over ssh, a browser opened on this machine would be
//! no use to the user, so the link goes on their clipboard instead, through
//! their terminal; and so it does where there's nothing to open it with.

use crate::clipboard;
use anyhow::{Result, bail};
use std::process::{Command, Stdio};
use std::thread;

/// Opens `url`, or copies it, and says which.
pub fn open(url: &str) -> Result<String> {
    if !has_scheme(url) {
        bail!("{url} isn't a link crystal can open");
    }
    if clipboard::remote() {
        clipboard::copy(url)?;
        return Ok(format!(
            "copied {url}: over ssh, crystal can't open your browser"
        ));
    }
    let program = opener(cfg!(target_os = "macos"));
    let spawned = Command::new(program)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            // Waited for off the caller's thread, so it doesn't linger.
            thread::spawn(move || child.wait());
            Ok(format!("opened {url}"))
        }
        Err(_) => {
            clipboard::copy(url)?;
            Ok(format!(
                "copied {url}: there's no {program} to open it with"
            ))
        }
    }
}

/// The program that opens a link in the user's browser.
fn opener(macos: bool) -> &'static str {
    if macos { "open" } else { "xdg-open" }
}

/// Whether `url` starts with a scheme, like `https:`: what the openers take
/// as a link rather than a file, and never an option, since it can't start
/// with a dash.
fn has_scheme(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_opens_links_with_open_and_linux_with_xdg_open() {
        assert_eq!(opener(true), "open");
        assert_eq!(opener(false), "xdg-open");
    }

    #[test]
    fn only_a_link_with_a_scheme_is_opened() {
        assert!(has_scheme("https://example.com"));
        assert!(has_scheme("file:///tmp/notes.md"));
        assert!(has_scheme("mailto:me@example.com"));
        assert!(!has_scheme("-a Calculator"));
        assert!(!has_scheme("example.com/a:b"));
        assert!(!has_scheme(":nothing"));
        let refused = open("--help").unwrap_err();
        assert!(refused.to_string().contains("isn't a link"), "{refused}");
    }
}
