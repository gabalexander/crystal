//! The title the TUI gives the terminal it runs in, which the terminal's
//! tabs, the window's frame and the window manager show. A session's own
//! title stops at crystal, which emulates its terminal; the TUI writes one
//! of its own: `[window] title`, its tokens filled in, again each time it
//! comes out different, or the text `crystal title set` gave it until
//! `crystal title clear`. An empty title leaves the terminal's alone. The
//! title from before is saved on the terminal's stack of them as the TUI
//! starts, and put back as it ends, by a terminal that keeps one.

use crate::printable;

/// The tokens a title can have, and what each is filled with.
pub const TOKENS: &[(&str, &str)] = &[
    ("hostname", "this machine's name, up to its first dot"),
    ("session", "the selected session's name"),
    ("project", "the selected session's project"),
    ("branch", "the selected session's branch"),
    ("tab", "the tab in front's name, or its number"),
    (
        "title",
        "the title the selected session's program gave its terminal",
    ),
];

/// Saves the terminal's title on its stack.
pub const SAVE: &[u8] = b"\x1b[22;0t";

/// Puts back the title saved last.
pub const RESTORE: &[u8] = b"\x1b[23;0t";

/// What the tokens are filled with, each empty when there's nothing to
/// say: no session is selected, it's outside git, its program gave no
/// title.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Values {
    pub hostname: String,
    pub session: String,
    pub project: String,
    pub branch: String,
    pub tab: String,
    pub title: String,
}

impl Values {
    fn get(&self, token: &str) -> Option<&str> {
        Some(match token {
            "hostname" => &self.hostname,
            "session" => &self.session,
            "project" => &self.project,
            "branch" => &self.branch,
            "tab" => &self.tab,
            "title" => &self.title,
            _ => return None,
        })
    }
}

/// What can be left over at either end of a title once a token at that
/// end came out empty: the space and the marks that go between tokens.
const LEFT_OVER: &[char] = &[' ', '·', '•', ':', '-', '–', '—', '|', '/', ','];

/// `template` with its tokens filled in from `values`, `{{` and `}}`
/// written as braces, and what an empty token left at either end taken
/// off: `"crystal · {session}"` with no session is `crystal`.
pub fn fill(template: &str, values: &Values) -> String {
    let mut title = String::new();
    let mut rest = template;
    while let Some(at) = rest.find(['{', '}']) {
        title.push_str(&rest[..at]);
        let (brace, after) = rest[at..].split_at(1);
        if let Some(after) = after.strip_prefix(brace) {
            title.push_str(brace);
            rest = after;
            continue;
        }
        let token = (brace == "{")
            .then(|| after.split_once('}'))
            .flatten()
            .and_then(|(name, after)| Some((values.get(name)?, after)));
        match token {
            Some((value, after)) => {
                title.push_str(value);
                rest = after;
            }
            // `check` keeps these out of the config: a brace on its own,
            // or a token that isn't one, is written as it is.
            None => {
                title.push_str(brace);
                rest = after;
            }
        }
    }
    title.push_str(rest);
    printable::line(&title).trim_matches(LEFT_OVER).to_string()
}

/// Says what's wrong with `template`: a token that isn't one, or a brace
/// that isn't doubled.
pub fn check(template: &str) -> Result<(), String> {
    let mut rest = template;
    while let Some(at) = rest.find(['{', '}']) {
        let (brace, after) = rest[at..].split_at(1);
        if let Some(after) = after.strip_prefix(brace) {
            rest = after;
            continue;
        }
        let names: Vec<String> = TOKENS
            .iter()
            .map(|(name, _)| format!("{{{name}}}"))
            .collect();
        let Some((name, after)) = after.split_once('}').filter(|_| brace == "{") else {
            return Err(format!(
                "a lone `{brace}`: write `{brace}{brace}` for a brace, or one of {}",
                names.join(", ")
            ));
        };
        if Values::default().get(name).is_none() {
            return Err(format!(
                "`{{{name}}}` isn't a token: say one of {}",
                names.join(", ")
            ));
        }
        rest = after;
    }
    Ok(())
}

/// What sets the terminal's title to `title`.
pub fn set(title: &str) -> Vec<u8> {
    let title = printable::line(title);
    format!("\x1b]2;{title}\x07").into_bytes()
}

/// This machine's name, up to its first dot: `ann-laptop`, not
/// `ann-laptop.local`.
pub fn hostname() -> String {
    let mut name = [0u8; 256];
    // SAFETY: a buffer of the length given, which gethostname ends with a
    // nul when the name fits.
    if unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } != 0 {
        return String::new();
    }
    let end = name
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(name.len());
    let name = String::from_utf8_lossy(&name[..end]);
    name.split('.').next().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Values {
        Values {
            hostname: "box".into(),
            session: "fix-login".into(),
            project: "app".into(),
            branch: "fix/login".into(),
            tab: "2".into(),
            title: "✳ Claude".into(),
        }
    }

    #[test]
    fn the_tokens_are_filled_in() {
        assert_eq!(
            fill("crystal · {session}", &values()),
            "crystal · fix-login"
        );
        assert_eq!(
            fill("{hostname}: {project} ({branch}) {tab} {title}", &values()),
            "box: app (fix/login) 2 ✳ Claude"
        );
        assert_eq!(
            fill("{{session}} {session}", &values()),
            "{session} fix-login"
        );
        assert_eq!(fill("no tokens", &values()), "no tokens");
    }

    #[test]
    fn what_an_empty_token_leaves_at_either_end_goes_with_it() {
        let none = Values::default();
        assert_eq!(fill("crystal · {session}", &none), "crystal");
        assert_eq!(fill("{session} — {project}", &none), "");
        let only_project = Values {
            project: "app".into(),
            ..Values::default()
        };
        assert_eq!(fill("{session} | {project}", &only_project), "app");
        assert_eq!(fill("{hostname}: {project}", &only_project), "app");
    }

    #[test]
    fn what_could_end_the_title_early_is_left_out() {
        let sneaky = Values {
            title: "a\x07b\x1b]2;c".into(),
            ..values()
        };
        assert_eq!(fill("{title}", &sneaky), "ab]2;c");
        assert_eq!(set("a\x07b"), b"\x1b]2;ab\x07");
        assert_eq!(
            set("a\x1b\\b\u{9c}c\u{202e}d"),
            "\x1b]2;a\\bcd\x07".as_bytes()
        );
    }

    #[test]
    fn a_token_that_isnt_one_or_a_lone_brace_is_an_error() {
        assert_eq!(check("crystal · {session} {{x}}"), Ok(()));
        assert_eq!(check(""), Ok(()));
        let err = check("{workspace}").unwrap_err();
        assert!(err.contains("`{workspace}` isn't a token"), "{err}");
        assert!(err.contains("{session}"), "{err}");
        assert!(check("a { b").unwrap_err().contains("lone `{`"));
        assert!(check("a } b").unwrap_err().contains("lone `}`"));
        // Unchecked, they're written as they are.
        assert_eq!(fill("a } {nope} b", &values()), "a } {nope} b");
    }

    #[test]
    fn the_hostname_is_its_first_part() {
        let name = hostname();
        assert!(!name.contains('.'), "{name}");
    }
}
