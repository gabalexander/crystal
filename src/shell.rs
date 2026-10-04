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
