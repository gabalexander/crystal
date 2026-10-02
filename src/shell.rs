//! Writing commands the way a shell reads them.

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
    use super::quote;

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
