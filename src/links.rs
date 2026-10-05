//! Links a pane shows, and opening them. A URL opens in the browser, with
//! `open` on macOS or `xdg-open` on Linux. Over ssh, a browser opened on
//! this machine would be no use to the user, so the link goes on their
//! clipboard instead, through their terminal; and so it does where there's
//! nothing to open it with.
//!
//! A file's path a program writes in its text is a link too, like the
//! `src/app.rs:42` an agent prints: read out of the text here, and found
//! among the files where its session runs, for the TUI to open in the
//! user's editor at that line. Adapted from docket's `visible_file_links`.

use crate::protocol::SessionInfo;
use crate::{clipboard, shell};
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

/// What a link on a screen goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A URL: a hyperlink a program wrote (OSC 8), or one written out in
    /// its text.
    Url(String),
    /// A file's path written in the text, as it was written, and the line
    /// written after it, if there's one.
    File { path: String, line: Option<usize> },
}

impl Target {
    /// The link as the user reads it: the URL, or the path and its line.
    pub fn written(&self) -> String {
        match self {
            Target::Url(url) => url.clone(),
            Target::File { path, line: None } => path.clone(),
            Target::File {
                path,
                line: Some(line),
            } => format!("{path}:{line}"),
        }
    }
}

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

/// A file's path found in a line of text by [`path_at`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub path: String,
    pub line: Option<usize>,
    /// Its first and last characters in the line, the line number after
    /// it and a column after that among them.
    pub first: usize,
    pub last: usize,
}

/// The file's path in `text`, a line of a screen's text across the rows it
/// wrapped onto, that the character at `at` is part of, if it's in one: a
/// path with a `/`, an extension or a line after it, which is `:12`,
/// `:12:5`, `(12)`, `(12,5)` or `#L12`, its column taken in but not kept.
/// A full stop after it ends the sentence, not the path, and a bracket or
/// a quote around it isn't part of it, so `Read(src/app.rs)` is
/// `src/app.rs`. Whether there's such a file is for [`find_file`] to say.
pub fn path_at(text: &[char], at: usize) -> Option<Written> {
    let mut start = 0;
    while start <= at && start < text.len() {
        if !is_path_char(text[start]) {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < text.len() && is_path_char(text[end]) {
            end += 1;
        }
        let mut path_end = end;
        while path_end > start && text[path_end - 1] == '.' {
            path_end -= 1;
        }
        // `src/app.rs.`: what comes after the full stop isn't its line.
        let (line, suffix) = if path_end == end {
            line_after(&text[end..])
        } else {
            (None, 0)
        };
        let path: String = text[start..path_end].iter().collect();
        let last = (path_end + suffix).saturating_sub(1);
        if (start..=last).contains(&at) {
            return is_path(&path, suffix > 0).then_some(Written {
                path,
                line,
                first: start,
                last,
            });
        }
        start = end + suffix;
    }
    None
}

/// The line written at the start of `rest`, just after a path, and how
/// many characters it takes, or `(None, 0)` when there's none: `:12` (and
/// `:12:5`, a line and a column) as compilers and agents write it,
/// `(12)` (and `(12,5)`) as MSVC and TypeScript do, and `#L12` as GitHub
/// does. Digits running on into a word, `:12abc`, aren't a line.
fn line_after(rest: &[char]) -> (Option<usize>, usize) {
    let digits = |from: usize| {
        let after = rest.get(from..).unwrap_or_default();
        after.iter().take_while(|c| c.is_ascii_digit()).count()
    };
    let number = |from: usize, count: usize| {
        let written: String = rest[from..from + count].iter().collect();
        written.parse::<usize>().ok().filter(|line| *line > 0)
    };
    let word_after = |at: usize| rest.get(at).is_some_and(|c| c.is_ascii_alphanumeric());
    match rest.first() {
        Some(':') => {
            let count = digits(1);
            if count == 0 || word_after(1 + count) {
                return (None, 0);
            }
            let mut taken = 1 + count;
            if rest.get(taken) == Some(&':') {
                let column = digits(taken + 1);
                if column > 0 && !word_after(taken + 1 + column) {
                    taken += 1 + column;
                }
            }
            (number(1, count), taken)
        }
        Some('(') => {
            let count = digits(1);
            if count == 0 {
                return (None, 0);
            }
            let mut taken = 1 + count;
            if rest.get(taken) == Some(&',') {
                let column = digits(taken + 1);
                if column == 0 {
                    return (None, 0);
                }
                taken += 1 + column;
            }
            if rest.get(taken) != Some(&')') {
                return (None, 0);
            }
            (number(1, count), taken + 1)
        }
        Some('#') if rest.get(1) == Some(&'L') => match digits(2) {
            0 => (None, 0),
            count => (number(2, count), 2 + count),
        },
        _ => (None, 0),
    }
}

/// Whether `token` reads as a file's path, rather than a word: one with a
/// `/` in it (but not `//`, what's left of a URL, nor ending in one, a
/// directory), or with an extension, or with a line after it.
fn is_path(token: &str, has_line: bool) -> bool {
    if token.starts_with("//") || token.ends_with('/') {
        return false;
    }
    if !token.chars().any(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    let name = token.rsplit('/').next().unwrap_or(token);
    // At least one letter, so `1.2.3` is a version rather than a file.
    let extension = name.rsplit_once('.').is_some_and(|(_, extension)| {
        extension.chars().all(|c| c.is_ascii_alphanumeric())
            && extension.chars().any(|c| c.is_ascii_alphabetic())
    });
    token.contains('/') || extension || has_line
}

/// What a path may be made of, as programs write paths: never a quote, a
/// bracket or a colon, which come around one or after it, nor anything but
/// ASCII, so a box's border drawn beside one isn't taken for part of it.
fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '~' | '+' | '@')
}

/// Where the file a program wrote `path` for is, if there's a file there:
/// at `path` itself when it's absolute, or in the home directory when it
/// starts with `~/`, or else in the first of `dirs` it's in, the
/// directories the program runs in; there, a diff's `a/` or `b/` before it
/// is taken off when it isn't found with it. A directory isn't one.
pub fn find_file(path: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let written = shell::expand_home(Path::new(path));
    if written.is_absolute() {
        return written.is_file().then_some(written);
    }
    let in_diff = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/"));
    std::iter::once(path)
        .chain(in_diff)
        .flat_map(|path| dirs.iter().map(move |dir| dir.join(path)))
        .find(|found| found.is_file())
}

/// The directories a path `session`'s program writes is looked for in:
/// where it runs, then the top of its worktree.
pub fn dirs_of(session: &SessionInfo) -> Vec<PathBuf> {
    let mut dirs = vec![session.cwd.clone()];
    if let Some(worktree) = &session.worktree
        && worktree.path != session.cwd
    {
        dirs.push(worktree.path.clone());
    }
    dirs
}

/// Whether `target`, a link on the screen of a program running in `dirs`,
/// opens: a URL does, and a path once there's a file there.
pub fn can_open(target: &Target, dirs: &[PathBuf]) -> bool {
    match target {
        Target::Url(_) => true,
        Target::File { path, .. } => find_file(path, dirs).is_some(),
    }
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

    /// The path in `text` at the character the `|` in it is before, and
    /// the text it spans.
    fn path_in(text: &str) -> Option<(String, Option<usize>, String)> {
        let at = text.find('|').expect("a | marks where to look");
        let chars: Vec<char> = text.replace('|', "").chars().collect();
        let at = text[..at].chars().count();
        let found = path_at(&chars, at)?;
        let spans = chars[found.first..=found.last].iter().collect();
        Some((found.path, found.line, spans))
    }

    fn path(
        path: &str,
        line: Option<usize>,
        spans: &str,
    ) -> Option<(String, Option<usize>, String)> {
        Some((path.to_string(), line, spans.to_string()))
    }

    #[test]
    fn a_path_with_its_line_is_found_from_any_of_its_characters() {
        let expected = path("src/foo.rs", Some(42), "src/foo.rs:42");
        assert_eq!(path_in("see |src/foo.rs:42 for it"), expected);
        assert_eq!(path_in("see src/fo|o.rs:42 for it"), expected);
        assert_eq!(path_in("see src/foo.rs:4|2 for it"), expected);
        assert_eq!(path_in("see| src/foo.rs:42 for it"), None);
        assert_eq!(path_in("see src/foo.rs:42 |for it"), None);
    }

    #[test]
    fn a_line_is_written_in_any_of_the_ways_compilers_write_it() {
        assert_eq!(
            path_in("--> |src/main.rs:12:5"),
            path("src/main.rs", Some(12), "src/main.rs:12:5")
        );
        assert_eq!(
            path_in("|src/app.ts(12,5): error TS2322"),
            path("src/app.ts", Some(12), "src/app.ts(12,5)")
        );
        assert_eq!(
            path_in("|lib/a.cpp(7) warning"),
            path("lib/a.cpp", Some(7), "lib/a.cpp(7)")
        );
        assert_eq!(
            path_in("at |src/y.go#L7"),
            path("src/y.go", Some(7), "src/y.go#L7")
        );
        // grep's line, then the text it found.
        assert_eq!(
            path_in("|src/a.rs:3:fn main() {"),
            path("src/a.rs", Some(3), "src/a.rs:3")
        );
    }

    #[test]
    fn what_isnt_a_line_stays_out_of_the_link() {
        assert_eq!(
            path_in("|src/a.rs: error"),
            path("src/a.rs", None, "src/a.rs")
        );
        assert_eq!(
            path_in("|src/a.rs:12abc"),
            path("src/a.rs", None, "src/a.rs")
        );
        assert_eq!(path_in("|src/a.rs(x)"), path("src/a.rs", None, "src/a.rs"));
        assert_eq!(path_in("src/a.rs:|12abc"), None);
    }

    #[test]
    fn brackets_quotes_and_the_full_stop_after_a_path_arent_part_of_it() {
        assert_eq!(
            path_in("Read(|crates/app/src/main.rs)"),
            path("crates/app/src/main.rs", None, "crates/app/src/main.rs")
        );
        assert_eq!(
            path_in("edited `|src/app.rs`."),
            path("src/app.rs", None, "src/app.rs")
        );
        assert_eq!(
            path_in("edited |src/app.rs. Done"),
            path("src/app.rs", None, "src/app.rs")
        );
        assert_eq!(path_in("edited src/app.rs|. Done"), None);
        assert_eq!(
            path_in("File \"|/srv/app/main.py\", line 3"),
            path("/srv/app/main.py", None, "/srv/app/main.py")
        );
    }

    #[test]
    fn words_versions_and_whats_left_of_a_url_arent_paths() {
        for text in [
            "|and",
            "it's 2|4 hours",
            "v1.|2.3",
            "a URL: https:|//x.dev/src/a.rs",
            "the |src/ directory",
            "--- |a ---",
        ] {
            assert_eq!(path_in(text), None, "{text}");
        }
        // A word with a line after it might be a file, `Makefile:12`, as
        // might a name with an extension: whether it is is for the files to
        // say.
        assert_eq!(
            path_in("|Makefile:12"),
            path("Makefile", Some(12), "Makefile:12")
        );
        assert_eq!(
            path_in("in |Cargo.toml"),
            path("Cargo.toml", None, "Cargo.toml")
        );
        assert_eq!(path_in("|and/or"), path("and/or", None, "and/or"));
    }

    #[test]
    fn the_path_found_is_the_one_at_the_character() {
        let text = "|a.rs:1 b.rs:2";
        assert_eq!(path_in(text), path("a.rs", Some(1), "a.rs:1"));
        assert_eq!(path_in("a.rs:1 |b.rs:2"), path("b.rs", Some(2), "b.rs:2"));
    }

    #[test]
    fn a_target_is_written_the_way_the_user_reads_it() {
        let url = Target::Url("https://example.com".into());
        assert_eq!(url.written(), "https://example.com");
        let file = |line| Target::File {
            path: "src/a.rs".into(),
            line,
        };
        assert_eq!(file(None).written(), "src/a.rs");
        assert_eq!(file(Some(12)).written(), "src/a.rs:12");
    }

    #[test]
    fn a_file_is_found_where_its_session_runs_or_where_it_says() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path().join("repo");
        let cwd = worktree.join("sub");
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        std::fs::create_dir_all(worktree.join("lib")).unwrap();
        std::fs::write(cwd.join("src/a.rs"), "").unwrap();
        std::fs::write(worktree.join("lib/b.rs"), "").unwrap();
        let dirs = [cwd.clone(), worktree.clone()];
        assert_eq!(find_file("src/a.rs", &dirs), Some(cwd.join("src/a.rs")));
        // Not where the session runs, but at the top of its worktree.
        assert_eq!(
            find_file("lib/b.rs", &dirs),
            Some(worktree.join("lib/b.rs"))
        );
        // A diff's a/ and b/.
        assert_eq!(find_file("b/src/a.rs", &dirs), Some(cwd.join("src/a.rs")));
        let absolute = cwd.join("src/a.rs");
        let absolute = absolute.to_str().unwrap();
        assert_eq!(find_file(absolute, &[]), Some(cwd.join("src/a.rs")));
        assert_eq!(find_file("src/none.rs", &dirs), None);
        // A directory isn't a file to open.
        assert_eq!(find_file("src", &dirs), None);
        assert_eq!(find_file("/nowhere/at/all.rs", &dirs), None);
    }
}
