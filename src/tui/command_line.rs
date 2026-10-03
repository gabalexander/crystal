//! Reading the line typed at the TUI's "new session:" prompt into the
//! command to run.

use std::path::Path;

/// Agents that take their first prompt as an argument, and so can be given
/// one right on the line: `claude fix the login bug`.
const AGENTS: &[&str] = &["claude", "codex"];

/// The command `line` asks for. After an agent's name, words that don't
/// start with `-` or a quote are its first prompt, all of them one
/// argument, so `claude fix the login bug` asks Claude to fix the login
/// bug. Any other line is split into words the way a shell splits them. An
/// empty line is an empty command, which starts the user's shell.
pub fn parse(line: &str) -> Result<Vec<String>, String> {
    let line = line.trim();
    if let Some((program, rest)) = line.split_once(char::is_whitespace) {
        let rest = rest.trim_start();
        if is_agent(program) && !rest.starts_with(['-', '\'', '"']) {
            return Ok(vec![program.to_string(), rest.to_string()]);
        }
    }
    split_words(line)
}

/// Whether `program` names an agent in [`AGENTS`], by name or by path.
fn is_agent(program: &str) -> bool {
    let name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str());
    name.is_some_and(|name| AGENTS.contains(&name))
}

/// Splits `line` into words the way a shell does: at spaces, except inside
/// single or double quotes. Outside single quotes, a backslash takes the
/// character after it as it is.
pub fn split_words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    // Whether a word has begun, which `""` does even though it adds nothing.
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('"'), '\\') | (None, '\\') => {
                let escaped = chars.next().ok_or("the line ends in a backslash")?;
                word.push(escaped);
                in_word = true;
            }
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err("a quote isn't closed".to_string());
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        parse(line).unwrap()
    }

    #[test]
    fn an_agent_takes_the_rest_of_the_line_as_its_first_prompt() {
        assert_eq!(
            words("claude fix the login bug"),
            ["claude", "fix the login bug"]
        );
        assert_eq!(
            words("codex   write the docs  "),
            ["codex", "write the docs"]
        );
        assert_eq!(
            words("/usr/local/bin/claude it's broken"),
            ["/usr/local/bin/claude", "it's broken"]
        );
    }

    #[test]
    fn an_agent_given_flags_or_quotes_is_split_like_any_command() {
        assert_eq!(words("claude --model opus"), ["claude", "--model", "opus"]);
        assert_eq!(
            words("claude \"fix it\" --verbose"),
            ["claude", "fix it", "--verbose"]
        );
    }

    #[test]
    fn other_commands_are_split_the_way_a_shell_splits_them() {
        assert_eq!(words("npm run dev"), ["npm", "run", "dev"]);
        assert_eq!(
            words(r#"sh -c 'echo "hi there"'"#),
            ["sh", "-c", r#"echo "hi there""#]
        );
        assert_eq!(words(r"echo a\ b \'c"), ["echo", "a b", "'c"]);
        assert_eq!(words(r#"echo "" x"#), ["echo", "", "x"]);
    }

    #[test]
    fn an_empty_line_is_an_empty_command() {
        assert!(words("").is_empty());
        assert!(words("   ").is_empty());
        assert_eq!(words("claude"), ["claude"]);
    }

    #[test]
    fn a_quote_left_open_is_an_error() {
        assert_eq!(parse("echo 'oops"), Err("a quote isn't closed".to_string()));
        assert!(parse(r"echo \").is_err());
    }
}
