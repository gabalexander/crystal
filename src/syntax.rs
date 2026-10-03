//! Highlighting code well enough to read at a glance: comments, strings,
//! numbers and each language's keywords, found a line at a time without a
//! highlighter crate. A block comment carries on to the lines after it; a
//! string left open runs to the end of its line and stops there. What color
//! each kind is drawn in is the theme's.
//!
//! Adapted from docket's `syntax.rs`.

/// What a run of a line is, for its color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    String,
    Comment,
    Number,
    Text,
}

/// A line of code cut into runs, each with what it is.
pub type Runs = Vec<(TokenKind, String)>;

/// How one language is read.
struct Language {
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// The characters a string starts and ends with; a backslash escapes
    /// the next one.
    quotes: &'static [char],
    keywords: &'static [&'static str],
    /// Whether a keyword is one in any case, as in SQL.
    any_case: bool,
}

const RUST: Language = Language {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    // No `'`: a lifetime, `'a`, would take the rest of the line.
    quotes: &['"'],
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
        "true", "type", "unsafe", "use", "where", "while",
    ],
    any_case: false,
};

const JAVASCRIPT: Language = Language {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\'', '`'],
    keywords: &[
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "null",
        "of",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "type",
        "typeof",
        "undefined",
        "var",
        "void",
        "while",
        "yield",
    ],
    any_case: false,
};

const PYTHON: Language = Language {
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
        "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "self",
        "True", "try", "while", "with", "yield",
    ],
    any_case: false,
};

const GO: Language = Language {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '`'],
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "fallthrough",
        "false",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "map",
        "nil",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "true",
        "type",
        "var",
    ],
    any_case: false,
};

const C: Language = Language {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    keywords: &[
        "auto",
        "bool",
        "break",
        "case",
        "char",
        "class",
        "const",
        "continue",
        "default",
        "delete",
        "do",
        "double",
        "else",
        "enum",
        "extern",
        "false",
        "float",
        "for",
        "goto",
        "if",
        "inline",
        "int",
        "long",
        "namespace",
        "new",
        "nullptr",
        "private",
        "protected",
        "public",
        "return",
        "short",
        "signed",
        "sizeof",
        "static",
        "struct",
        "switch",
        "template",
        "true",
        "typedef",
        "typename",
        "union",
        "unsigned",
        "using",
        "virtual",
        "void",
        "volatile",
        "while",
    ],
    any_case: false,
};

/// Java and the languages on the JVM, and Swift, whose keywords are much
/// the same.
const JAVA: Language = Language {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    keywords: &[
        "abstract",
        "boolean",
        "break",
        "byte",
        "case",
        "catch",
        "char",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "double",
        "else",
        "enum",
        "extends",
        "false",
        "final",
        "finally",
        "float",
        "for",
        "fun",
        "if",
        "implements",
        "import",
        "instanceof",
        "int",
        "interface",
        "long",
        "native",
        "new",
        "null",
        "object",
        "override",
        "package",
        "private",
        "protected",
        "public",
        "record",
        "return",
        "short",
        "static",
        "super",
        "switch",
        "synchronized",
        "this",
        "throw",
        "throws",
        "true",
        "try",
        "val",
        "var",
        "void",
        "volatile",
        "when",
        "while",
    ],
    any_case: false,
};

const RUBY: Language = Language {
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    keywords: &[
        "and", "begin", "break", "case", "class", "def", "do", "else", "elsif", "end", "ensure",
        "false", "for", "if", "in", "module", "next", "nil", "not", "or", "raise", "require",
        "rescue", "return", "self", "then", "true", "unless", "until", "when", "while", "yield",
    ],
    any_case: false,
};

const SHELL: Language = Language {
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    keywords: &[
        "alias", "case", "do", "done", "echo", "elif", "else", "esac", "exit", "export", "fi",
        "for", "function", "if", "in", "local", "return", "set", "source", "then", "unset",
        "until", "while",
    ],
    any_case: false,
};

const SQL: Language = Language {
    line_comments: &["--"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    keywords: &[
        "all",
        "alter",
        "and",
        "as",
        "by",
        "constraint",
        "create",
        "default",
        "delete",
        "distinct",
        "drop",
        "exists",
        "foreign",
        "from",
        "group",
        "having",
        "in",
        "index",
        "inner",
        "insert",
        "into",
        "is",
        "join",
        "key",
        "left",
        "limit",
        "not",
        "null",
        "offset",
        "on",
        "or",
        "order",
        "outer",
        "primary",
        "references",
        "right",
        "select",
        "table",
        "union",
        "unique",
        "update",
        "values",
        "view",
        "where",
    ],
    any_case: true,
};

const TOML: Language = Language {
    line_comments: &["#", ";"],
    block_comment: None,
    quotes: &['"', '\''],
    keywords: &["true", "false"],
    any_case: false,
};

const YAML: Language = Language {
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    keywords: &["true", "false", "null"],
    any_case: false,
};

const JSON: Language = Language {
    // For JSON with comments; plain JSON never has one.
    line_comments: &["//"],
    block_comment: None,
    quotes: &['"'],
    keywords: &["true", "false", "null"],
    any_case: false,
};

const CSS: Language = Language {
    // For Sass and Less; plain CSS never has one.
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    keywords: &[],
    any_case: false,
};

const DOCKERFILE: Language = Language {
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    keywords: &[
        "add",
        "arg",
        "cmd",
        "copy",
        "entrypoint",
        "env",
        "expose",
        "from",
        "healthcheck",
        "label",
        "run",
        "shell",
        "user",
        "volume",
        "workdir",
    ],
    any_case: true,
};

/// Highlights a file a line at a time. A block comment's state carries
/// from one line to the next, so the lines go in in order, from the top.
pub struct Highlighter {
    language: Option<&'static Language>,
    in_block_comment: bool,
}

impl Highlighter {
    /// A highlighter for the file at `path`, by its name; a file of a
    /// language not known here gets the plain one.
    pub fn for_path(path: &str) -> Highlighter {
        let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
        let language = match name.as_str() {
            "makefile" | "gnumakefile" => Some(&SHELL),
            "dockerfile" => Some(&DOCKERFILE),
            _ => name
                .rsplit_once('.')
                .and_then(|(_, extension)| by_extension(extension)),
        };
        Highlighter {
            language,
            in_block_comment: false,
        }
    }

    /// A highlighter for a fenced code block, by its info string: `rust`,
    /// `js`, `console`, `dockerfile`. A language not known here gets the
    /// plain one.
    pub fn for_fence(info: &str) -> Highlighter {
        let name = info
            .split([',', ' ', '{'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let extension = match name.as_str() {
            "rust" => "rs",
            "javascript" => "js",
            "typescript" => "ts",
            "python" => "py",
            "golang" => "go",
            "shell" | "console" | "bash" | "zsh" | "fish" => "sh",
            "ruby" => "rb",
            "yaml" => "yml",
            "c++" => "cpp",
            "objective-c" | "objc" => "m",
            "kotlin" => "kt",
            "dockerfile" | "docker" => return Highlighter::for_path("dockerfile"),
            "makefile" | "make" => return Highlighter::for_path("makefile"),
            other => other,
        };
        Highlighter::for_path(&format!("fence.{extension}"))
    }

    /// No language: every line is one run of text.
    pub fn plain() -> Highlighter {
        Highlighter {
            language: None,
            in_block_comment: false,
        }
    }

    /// `line` cut into runs that cover it all, each with what it is.
    pub fn line(&mut self, line: &str) -> Runs {
        let Some(language) = self.language else {
            if line.is_empty() {
                return Vec::new();
            }
            return vec![(TokenKind::Text, line.to_string())];
        };
        let chars: Vec<char> = line.chars().collect();
        let mut runs = Builder::default();
        let mut at = 0;
        while at < chars.len() {
            if self.in_block_comment {
                at = self.block_comment_end(language, &chars, at, &mut runs);
                continue;
            }
            let rest = &chars[at..];
            if language.line_comments.iter().any(|p| starts_with(rest, p)) {
                runs.push(TokenKind::Comment, rest);
                break;
            }
            if let Some((open, _)) = language.block_comment
                && starts_with(rest, open)
            {
                self.in_block_comment = true;
                let body = at + open.chars().count();
                runs.push(TokenKind::Comment, &chars[at..body]);
                at = self.block_comment_end(language, &chars, body, &mut runs);
                continue;
            }
            let c = chars[at];
            let (kind, end) = if language.quotes.contains(&c) {
                (TokenKind::String, string_end(&chars, at))
            } else if c.is_ascii_digit() {
                let end = end_of(&chars, at, |c| {
                    c.is_ascii_alphanumeric() || matches!(c, '_' | '.')
                });
                (TokenKind::Number, end)
            } else if c.is_alphabetic() || c == '_' {
                let end = end_of(&chars, at, |c| c.is_alphanumeric() || c == '_');
                let word: String = chars[at..end].iter().collect();
                (word_kind(language, &word), end)
            } else {
                (TokenKind::Text, at + 1)
            };
            runs.push(kind, &chars[at..end]);
            at = end;
        }
        runs.finish()
    }

    /// Takes the rest of a block comment from `at`, up to and through its
    /// end if it's on this line; returns where the line goes on.
    fn block_comment_end(
        &mut self,
        language: &Language,
        chars: &[char],
        at: usize,
        runs: &mut Builder,
    ) -> usize {
        let close = language.block_comment.map_or("", |(_, close)| close);
        let end = match find(chars, at, close) {
            Some(found) => {
                self.in_block_comment = false;
                found + close.chars().count()
            }
            None => chars.len(),
        };
        runs.push(TokenKind::Comment, &chars[at..end]);
        end
    }
}

/// The language files with `extension` are in, if it's one known here.
fn by_extension(extension: &str) -> Option<&'static Language> {
    match extension {
        "rs" => Some(&RUST),
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" => Some(&JAVASCRIPT),
        "py" => Some(&PYTHON),
        "go" => Some(&GO),
        "c" | "h" | "cc" | "cpp" | "hpp" | "hh" | "m" | "mm" => Some(&C),
        "java" | "kt" | "kts" | "swift" | "scala" | "gradle" => Some(&JAVA),
        "rb" => Some(&RUBY),
        "sh" | "bash" | "zsh" | "fish" => Some(&SHELL),
        "sql" => Some(&SQL),
        "toml" | "ini" | "cfg" | "conf" => Some(&TOML),
        "yaml" | "yml" => Some(&YAML),
        "json" | "jsonc" => Some(&JSON),
        "css" | "scss" | "less" => Some(&CSS),
        _ => None,
    }
}

/// Whether `word` is one of the language's keywords.
fn word_kind(language: &Language, word: &str) -> TokenKind {
    let keyword = if language.any_case {
        language
            .keywords
            .contains(&word.to_ascii_lowercase().as_str())
    } else {
        language.keywords.contains(&word)
    };
    if keyword {
        TokenKind::Keyword
    } else {
        TokenKind::Text
    }
}

/// Where the string starting at `at` ends: after its closing quote, or at
/// the end of the line when it isn't closed there.
fn string_end(chars: &[char], at: usize) -> usize {
    let quote = chars[at];
    let mut end = at + 1;
    while end < chars.len() {
        match chars[end] {
            '\\' => end += 2,
            c if c == quote => return end + 1,
            _ => end += 1,
        }
    }
    chars.len()
}

/// Where the run of characters from `at` that are `in_run` ends.
fn end_of(chars: &[char], at: usize, in_run: impl Fn(char) -> bool) -> usize {
    chars[at..]
        .iter()
        .position(|&c| !in_run(c))
        .map_or(chars.len(), |length| at + length)
}

fn starts_with(chars: &[char], pattern: &str) -> bool {
    let mut chars = chars.iter();
    pattern.chars().all(|p| chars.next() == Some(&p))
}

/// Where `pattern` is first found in `chars`, from `from` on.
fn find(chars: &[char], from: usize, pattern: &str) -> Option<usize> {
    (from..chars.len()).find(|&at| starts_with(&chars[at..], pattern))
}

/// Puts runs together, joining one to the last when they're the same kind.
#[derive(Default)]
struct Builder {
    runs: Runs,
}

impl Builder {
    fn push(&mut self, kind: TokenKind, chars: &[char]) {
        if chars.is_empty() {
            return;
        }
        match self.runs.last_mut() {
            Some((last, text)) if *last == kind => text.extend(chars),
            _ => self.runs.push((kind, chars.iter().collect())),
        }
    }

    fn finish(self) -> Runs {
        self.runs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::{Comment, Keyword, Number, Text};

    /// Whether `line` is cut into the runs `expected` says.
    #[track_caller]
    fn check(highlighter: &mut Highlighter, line: &str, expected: &[(TokenKind, &str)]) {
        let expected: Runs = expected
            .iter()
            .map(|(kind, text)| (*kind, text.to_string()))
            .collect();
        assert_eq!(highlighter.line(line), expected);
    }

    #[test]
    fn rust_keywords_strings_comments_and_numbers() {
        let mut rust = Highlighter::for_path("src/main.rs");
        check(
            &mut rust,
            r#"let x = "hi"; // done"#,
            &[
                (Keyword, "let"),
                (Text, " x = "),
                (TokenKind::String, "\"hi\""),
                (Text, "; "),
                (Comment, "// done"),
            ],
        );
        check(
            &mut rust,
            "foo(42, 0xff)",
            &[
                (Text, "foo("),
                (Number, "42"),
                (Text, ", "),
                (Number, "0xff"),
                (Text, ")"),
            ],
        );
    }

    #[test]
    fn a_block_comment_goes_on_across_lines() {
        let mut rust = Highlighter::for_path("a.rs");
        check(
            &mut rust,
            "fn a() /* start",
            &[(Keyword, "fn"), (Text, " a() "), (Comment, "/* start")],
        );
        check(&mut rust, "still comment", &[(Comment, "still comment")]);
        check(
            &mut rust,
            "end */ let y",
            &[
                (Comment, "end */"),
                (Text, " "),
                (Keyword, "let"),
                (Text, " y"),
            ],
        );
        check(
            &mut rust,
            "a /* b */ c",
            &[(Text, "a "), (Comment, "/* b */"), (Text, " c")],
        );
    }

    #[test]
    fn an_escaped_quote_stays_inside_the_string() {
        let mut python = Highlighter::for_path("a.py");
        check(
            &mut python,
            r#"x = "a\"b" # c"#,
            &[
                (Text, "x = "),
                (TokenKind::String, r#""a\"b""#),
                (Text, " "),
                (Comment, "# c"),
            ],
        );
    }

    #[test]
    fn a_string_left_open_runs_to_the_end_of_its_line() {
        let mut python = Highlighter::for_path("a.py");
        check(
            &mut python,
            "x = 'open",
            &[(Text, "x = "), (TokenKind::String, "'open")],
        );
        check(&mut python, "y", &[(Text, "y")]);
    }

    #[test]
    fn rust_lifetimes_arent_strings() {
        let mut rust = Highlighter::for_path("a.rs");
        let runs = rust.line("fn f<'a>(x: &'a str)");
        assert!(
            runs.iter().all(|(kind, _)| *kind != TokenKind::String),
            "{runs:?}"
        );
    }

    #[test]
    fn sql_keywords_are_keywords_in_any_case() {
        let mut sql = Highlighter::for_path("q.sql");
        let runs = sql.line("SELECT id FROM users");
        assert_eq!(runs[0], (Keyword, "SELECT".to_string()));
        assert!(runs.contains(&(Keyword, "FROM".to_string())), "{runs:?}");
    }

    #[test]
    fn a_file_of_a_language_not_known_is_one_run() {
        let mut text = Highlighter::for_path("notes.txt");
        check(&mut text, "let x = \"hi\"", &[(Text, "let x = \"hi\"")]);
        check(&mut text, "", &[]);
    }

    #[test]
    fn some_files_are_known_by_their_names() {
        let mut docker = Highlighter::for_path("app/Dockerfile");
        check(
            &mut docker,
            "FROM rust",
            &[(Keyword, "FROM"), (Text, " rust")],
        );
        let mut make = Highlighter::for_path("Makefile");
        check(&mut make, "# build", &[(Comment, "# build")]);
    }

    #[test]
    fn a_fence_names_its_language() {
        let mut rust = Highlighter::for_fence("rust,ignore");
        check(&mut rust, "fn", &[(Keyword, "fn")]);
        let mut console = Highlighter::for_fence("console");
        check(&mut console, "# hi", &[(Comment, "# hi")]);
        let mut unknown = Highlighter::for_fence("klingon");
        check(&mut unknown, "fn x", &[(Text, "fn x")]);
    }
}
