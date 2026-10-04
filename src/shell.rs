//! Writing commands and paths the way a shell reads them.

use std::path::{Path, PathBuf};

/// `path` with the home directory written as `~`, as you'd type it.
pub fn home_relative(path: &Path) -> String {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => relative_to(path, Path::new(&home)),
        _ => path.display().to_string(),
    }
}

/// `path` with a leading `~` made the home directory, as a shell would.
pub fn expand_home(path: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(rest)
        }
        Err(_) => path.to_path_buf(),
    }
}

/// `path` with `home` written as `~`. Paths compare whole directory names,
/// so `/home/ann` isn't under `/home/an`.
fn relative_to(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// What Tab makes of `typed`, a directory being typed, as a shell
/// completes it: the last name finished, and a `/` after it, when one
/// directory in its parent starts with it, or as far as all those that do
/// agree. Hidden ones count only once a dot is typed. `subdirectories`
/// lists the names of a directory's directories.
pub fn complete_dir(typed: &str, subdirectories: impl Fn(&Path) -> Vec<String>) -> String {
    let (parent, start) = match typed.rfind('/') {
        Some(at) => typed.split_at(at + 1),
        None => ("", typed),
    };
    let listed = match parent {
        "" => PathBuf::from("."),
        parent => expand_home(Path::new(parent)),
    };
    let mut found: Vec<String> = subdirectories(&listed)
        .into_iter()
        .filter(|name| name.starts_with(start))
        .filter(|name| start.starts_with('.') || !name.starts_with('.'))
        .collect();
    found.sort();
    let Some(first) = found.first() else {
        return typed.to_string();
    };
    if found.len() == 1 {
        return format!("{parent}{first}/");
    }
    let mut agreed = first.clone();
    for name in &found[1..] {
        let same = agreed
            .chars()
            .zip(name.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum();
        agreed.truncate(same);
    }
    format!("{parent}{agreed}")
}

/// The names of the directories in `dir`, symbolic links to them too.
pub fn subdirectories(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect()
}

/// `arg` as you'd type it into a shell: as it is when that's safe, or else
/// in single quotes.
pub fn quote(arg: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c);
    if !arg.is_empty() && arg.chars().all(plain) {
        return arg.to_string();
    }
    // Inside single quotes nothing is special but the quote itself, which
    // has to close the quotes, be escaped, and open them again.
    format!("'{}'", arg.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_finishes_a_directory_or_goes_as_far_as_the_ones_it_could_be_agree() {
        let listed = |dir: &Path| -> Vec<String> {
            match dir.to_str().unwrap() {
                "/code/" => vec![
                    "crystal".into(),
                    "crate".into(),
                    "docs".into(),
                    ".git".into(),
                ],
                "/code/docs/" => vec!["guide".into()],
                _ => Vec::new(),
            }
        };
        assert_eq!(complete_dir("/code/d", listed), "/code/docs/");
        assert_eq!(complete_dir("/code/c", listed), "/code/cr");
        assert_eq!(complete_dir("/code/cry", listed), "/code/crystal/");
        assert_eq!(complete_dir("/code/docs/", listed), "/code/docs/guide/");
        // Hidden ones once a dot is typed; nothing that fits, nothing new.
        assert_eq!(complete_dir("/code/.", listed), "/code/.git/");
        assert_eq!(complete_dir("/code/x", listed), "/code/x");
        assert_eq!(complete_dir("/code/", listed), "/code/");
    }

    #[test]
    fn the_home_directory_is_written_as_a_tilde() {
        let home = Path::new("/home/ann");
        assert_eq!(
            relative_to(Path::new("/home/ann/code/app"), home),
            "~/code/app"
        );
        assert_eq!(relative_to(Path::new("/home/ann"), home), "~");
        assert_eq!(relative_to(Path::new("/home/anna"), home), "/home/anna");
        assert_eq!(relative_to(Path::new("/tmp"), home), "/tmp");
    }

    #[test]
    fn plain_words_are_left_alone() {
        assert_eq!(quote("--model=opus"), "--model=opus");
    }

    #[test]
    fn what_a_shell_would_split_is_quoted() {
        assert_eq!(quote("exit 3"), "'exit 3'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote(""), "''");
    }
}
