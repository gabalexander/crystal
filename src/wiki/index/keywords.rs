//! The syntactic tier's fallback, for a language with no grammar compiled
//! in: its definitions read off its lines by the keywords that start them
//! (`class`, `fun`, `def`, `func`, a shell function's `name()`), each line
//! after the keywords that may come before (`public`, `static`, `override`).
//! A definition's first line is exact; its last is the line before the
//! next one indented no deeper, or the `}` or `end` at its own depth, as
//! code laid out the usual way has it. What one is in is what's indented
//! above it, and a definition inside a function's lines is that function's
//! own, left out.

use super::DefKind;
use super::files::Lang;
use super::grammar::{Found, Outline, lines};
use regex::Regex;
use std::sync::LazyLock;

/// How a language's definitions start: each pattern's `name` group names
/// one of the kind beside it, unless its `kind` group says another.
struct Keywords {
    patterns: Vec<(Regex, DefKind)>,
}

/// The keywords that may come before a definition's own, in the languages
/// read this way.
const MODIFIERS: &str = r"(?:(?:public|private|protected|internal|open|abstract|sealed|data|enum|inner|annotation|value|inline|override|suspend|operator|infix|tailrec|external|const|lateinit|companion|expect|actual|static|final|readonly|virtual|partial|async|unsafe|extern|new|implicit|lazy|case|fileprivate|mutating|nonmutating|convenience|required|dynamic|indirect|nonisolated|export|pub|local|declare|synchronized|native|transient|volatile|default)\s+)*";

fn keywords(lang: Lang) -> Option<&'static Keywords> {
    static TABLES: LazyLock<Vec<(Lang, Keywords)>> = LazyLock::new(|| {
        let table = |patterns: &[(&str, DefKind)]| Keywords {
            patterns: patterns
                .iter()
                .map(|(pattern, kind)| {
                    let pattern = pattern.replace("{m}", MODIFIERS);
                    (
                        Regex::new(&format!(r"^\s*{pattern}")).expect("a valid regex"),
                        *kind,
                    )
                })
                .collect(),
        };
        const NAME: &str = r"(?P<name>[A-Za-z_$][\w$]*)";
        let p = |pattern: &str| pattern.replace("{n}", NAME);
        vec![
            (
                Lang::Kotlin,
                table(&[
                    (&p(r"{m}(?:class|object)\s+{n}"), DefKind::Class),
                    (&p(r"{m}interface\s+{n}"), DefKind::Interface),
                    (
                        &p(r"{m}fun\s+(?:<[^>]*>\s*)?(?:[\w.<>?]+\.)?{n}"),
                        DefKind::Function,
                    ),
                    (&p(r"{m}(?:val|var)\s+{n}"), DefKind::Field),
                    (&p(r"{m}typealias\s+{n}"), DefKind::Type),
                ]),
            ),
            (
                Lang::CSharp,
                table(&[
                    (&p(r"{m}(?:class|record)\s+{n}"), DefKind::Class),
                    (&p(r"{m}struct\s+{n}"), DefKind::Struct),
                    (&p(r"{m}interface\s+{n}"), DefKind::Interface),
                    (&p(r"{m}enum\s+{n}"), DefKind::Enum),
                    (&p(r"namespace\s+(?P<name>[\w.]+)"), DefKind::Module),
                    (
                        &p(
                            r"(?:(?:public|private|protected|internal|static|virtual|override|abstract|async|sealed|extern|unsafe|new|partial)\s+)+[\w<>\[\],.?]+\s+{n}\s*(?:<[^>]*>)?\s*\(",
                        ),
                        DefKind::Function,
                    ),
                    (
                        &p(
                            r"(?:(?:public|private|protected|internal|static|virtual|override|abstract|required|readonly|new)\s+)+[\w<>\[\],.?]+\s+{n}\s*(?:\{|=>|=|;)",
                        ),
                        DefKind::Field,
                    ),
                ]),
            ),
            (
                Lang::Swift,
                table(&[
                    (&p(r"{m}(?:class|actor)\s+{n}"), DefKind::Class),
                    (&p(r"{m}struct\s+{n}"), DefKind::Struct),
                    (&p(r"{m}enum\s+{n}"), DefKind::Enum),
                    (&p(r"{m}protocol\s+{n}"), DefKind::Interface),
                    (&p(r"{m}func\s+{n}"), DefKind::Function),
                    (&p(r"{m}(?:var|let)\s+{n}"), DefKind::Field),
                    (&p(r"{m}typealias\s+{n}"), DefKind::Type),
                ]),
            ),
            (
                Lang::Ruby,
                table(&[
                    (r"class\s+(?:[\w:]+::)?(?P<name>[A-Z]\w*)", DefKind::Class),
                    (r"module\s+(?:[\w:]+::)?(?P<name>[A-Z]\w*)", DefKind::Module),
                    (
                        r"def\s+(?:self\.)?(?P<name>[A-Za-z_]\w*[?!=]?)",
                        DefKind::Function,
                    ),
                    (r"(?P<name>[A-Z][A-Z0-9_]*)\s*=[^=]", DefKind::Const),
                ]),
            ),
            (
                Lang::Php,
                table(&[
                    (&p(r"{m}class\s+{n}"), DefKind::Class),
                    (&p(r"{m}interface\s+{n}"), DefKind::Interface),
                    (&p(r"{m}trait\s+{n}"), DefKind::Trait),
                    (&p(r"{m}enum\s+{n}"), DefKind::Enum),
                    (&p(r"{m}function\s+&?{n}"), DefKind::Function),
                    (&p(r"{m}const\s+{n}"), DefKind::Const),
                    (r"namespace\s+(?P<name>[\w\\]+)", DefKind::Module),
                ]),
            ),
            (
                Lang::Shell,
                table(&[
                    (
                        r"(?:function\s+)?(?P<name>[\w:.-]+)\s*\(\s*\)",
                        DefKind::Function,
                    ),
                    (r"function\s+(?P<name>[\w:.-]+)", DefKind::Function),
                    (
                        r"(?:export\s+|readonly\s+|declare\s+-\w+\s+)?(?P<name>[A-Z][A-Z0-9_]*)=",
                        DefKind::Variable,
                    ),
                ]),
            ),
            (
                Lang::Scala,
                table(&[
                    (&p(r"{m}(?:class|object)\s+{n}"), DefKind::Class),
                    (&p(r"{m}trait\s+{n}"), DefKind::Trait),
                    (&p(r"{m}enum\s+{n}"), DefKind::Enum),
                    (&p(r"{m}def\s+{n}"), DefKind::Function),
                    (&p(r"{m}(?:val|var)\s+{n}"), DefKind::Field),
                    (&p(r"{m}type\s+{n}"), DefKind::Type),
                ]),
            ),
            (
                Lang::Lua,
                table(&[
                    (
                        r"(?:local\s+)?function\s+(?:[\w.]+[.:])?(?P<name>\w+)\s*\(",
                        DefKind::Function,
                    ),
                    (
                        r"(?:local\s+)?(?:[\w.]+\.)?(?P<name>\w+)\s*=\s*function\b",
                        DefKind::Function,
                    ),
                ]),
            ),
            (
                Lang::Elixir,
                table(&[
                    (r"defmodule\s+(?P<name>[\w.]+)", DefKind::Module),
                    (r"defprotocol\s+(?P<name>[\w.]+)", DefKind::Interface),
                    (
                        r"(?:def|defp|defmacro|defmacrop|defguard|defdelegate)\s+(?P<name>[a-z_]\w*[?!]?)",
                        DefKind::Function,
                    ),
                ]),
            ),
            (
                Lang::Dart,
                table(&[
                    (&p(r"{m}(?:class|mixin)\s+{n}"), DefKind::Class),
                    (&p(r"{m}enum\s+{n}"), DefKind::Enum),
                    (&p(r"{m}(?:extension|typedef)\s+{n}"), DefKind::Type),
                    (
                        &p(r"(?:[\w<>?,]+\s+)?{n}\s*\([^;]*\)\s*(?:async\s*)?\{"),
                        DefKind::Function,
                    ),
                ]),
            ),
            (
                Lang::Zig,
                table(&[
                    (
                        r"(?:pub\s+)?(?:export\s+|inline\s+)?fn\s+(?P<name>\w+)",
                        DefKind::Function,
                    ),
                    (
                        r"(?:pub\s+)?const\s+(?P<name>\w+)\s*=\s*(?:packed\s+|extern\s+)?struct\b",
                        DefKind::Struct,
                    ),
                    (
                        r"(?:pub\s+)?const\s+(?P<name>\w+)\s*=\s*(?:enum|union\(enum\))\b",
                        DefKind::Enum,
                    ),
                    (r"(?:pub\s+)?(?:const|var)\s+(?P<name>\w+)", DefKind::Const),
                ]),
            ),
            (
                Lang::Perl,
                table(&[
                    (r"sub\s+(?P<name>\w+)", DefKind::Function),
                    (r"package\s+(?P<name>[\w:]+)", DefKind::Module),
                ]),
            ),
            (
                Lang::Groovy,
                table(&[
                    (&p(r"{m}(?:class|trait)\s+{n}"), DefKind::Class),
                    (&p(r"{m}interface\s+{n}"), DefKind::Interface),
                    (&p(r"{m}enum\s+{n}"), DefKind::Enum),
                    (&p(r"{m}def\s+{n}\s*\("), DefKind::Function),
                ]),
            ),
        ]
    });
    TABLES
        .iter()
        .find(|(of, _)| *of == lang)
        .map(|(_, keywords)| keywords)
}

/// Whether `lang`'s definitions are read by their keywords.
pub fn reads(lang: Lang) -> bool {
    keywords(lang).is_some()
}

/// What the file `source`, in `lang`, defines, read by its keywords;
/// `None` for a language with none.
pub fn outline(lang: Lang, source: &[u8]) -> Option<Outline> {
    let keywords = keywords(lang)?;
    let text = String::from_utf8_lossy(source);
    let rows: Vec<&str> = text.lines().collect();
    // Each definition: its row, its indentation, its name and kind.
    let mut starts: Vec<(usize, usize, String, DefKind)> = Vec::new();
    let mut in_comment = false;
    for (row, line) in rows.iter().enumerate() {
        let trimmed = line.trim_start();
        if in_comment {
            in_comment = !trimmed.contains("*/");
            continue;
        }
        if trimmed.starts_with("/*") && !trimmed.contains("*/") {
            in_comment = true;
            continue;
        }
        if trimmed.starts_with("//")
            || trimmed.starts_with('#') && lang != Lang::Shell
            || trimmed.starts_with('*')
        {
            continue;
        }
        if lang == Lang::Shell && trimmed.starts_with('#') {
            continue;
        }
        for (pattern, kind) in &keywords.patterns {
            if let Some(found) = pattern.captures(line) {
                let name = found.name("name").map_or("", |name| name.as_str());
                if !name.is_empty() && !RESERVED.contains(&name) {
                    starts.push((row, indentation(line), name.to_string(), *kind));
                }
                break;
            }
        }
    }
    let mut defs: Vec<Found> = Vec::new();
    // The rows each definition spans, to tell what one is in.
    let mut spans: Vec<(usize, usize, usize, DefKind, Vec<String>)> = Vec::new();
    for (row, indent, name, kind) in starts {
        let end = end_row(&rows, row, indent);
        // What it's in: the definitions above it whose rows hold its own,
        // indented less.
        let inside: Vec<&(usize, usize, usize, DefKind, Vec<String>)> = spans
            .iter()
            .filter(|(from, to, outer, _, _)| *from < row && row <= *to && *outer < indent)
            .collect();
        if inside
            .iter()
            .any(|(_, _, _, kind, _)| matches!(kind, DefKind::Function | DefKind::Method))
        {
            continue;
        }
        let mut segments = inside
            .last()
            .map(|(_, _, _, _, segments)| segments.clone())
            .unwrap_or_default();
        // A namespace written whole, `App.Web` or `Foo::Bar`, is in the
        // namespaces before its last part.
        segments.extend(
            name.split(['.', '\\'])
                .flat_map(|part| part.split("::"))
                .filter(|part| !part.is_empty())
                .map(String::from),
        );
        let in_type = inside
            .last()
            .is_some_and(|(_, _, _, kind, _)| !matches!(kind, DefKind::Module));
        let kind = match kind {
            DefKind::Function if in_type => DefKind::Method,
            DefKind::Field if !in_type => DefKind::Variable,
            kind => kind,
        };
        spans.push((row, end, indent, kind, segments.clone()));
        let line = u32::try_from(row + 1).unwrap_or(u32::MAX);
        defs.push(Found {
            segments,
            kind,
            start: line,
            end: u32::try_from(end + 1).unwrap_or(u32::MAX),
            line,
            names: Vec::new(),
            ty: None,
            test: false,
            decl: false,
        });
    }
    Some(Outline {
        package: Vec::new(),
        defs,
        lines: lines(source),
    })
}

/// Words the patterns would take for a name that aren't one.
const RESERVED: &[&str] = &[
    "if", "for", "while", "switch", "return", "else", "catch", "new", "case", "do", "when", "try",
    "in",
];

fn indentation(line: &str) -> usize {
    line.chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// The last row of the definition starting at `row`, indented `indent`:
/// its closing `}` or `end` at that indentation, or else the last row
/// before the next one indented no deeper.
fn end_row(rows: &[&str], row: usize, indent: usize) -> usize {
    let mut last = row;
    for (at, line) in rows.iter().enumerate().skip(row + 1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if indentation(line) <= indent {
            // A brace on a line of its own opens the body, as C# lays it.
            if trimmed.starts_with('{') {
                last = at;
                continue;
            }
            let closes = trimmed.starts_with('}')
                || trimmed.starts_with(')')
                || trimmed == "end"
                || trimmed.starts_with("end ")
                || trimmed.starts_with("end;")
                || trimmed == "fi"
                || trimmed == "esac";
            return if closes { at } else { last };
        }
        last = at;
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs(lang: Lang, source: &str) -> Vec<(String, DefKind, u32, u32)> {
        let outline = outline(lang, source.as_bytes()).unwrap();
        outline
            .defs
            .into_iter()
            .map(|found| (found.segments.join("."), found.kind, found.start, found.end))
            .collect()
    }

    #[test]
    fn kotlin_is_read_by_its_keywords_with_what_each_is_in() {
        let source = "package app\n\
            \n\
            data class Server(val port: Int) {\n\
            \x20   private val name = \"x\"\n\
            \x20   override fun serve(x: Int): Int {\n\
            \x20       val local = 3\n\
            \x20       return local\n\
            \x20   }\n\
            }\n\
            \n\
            fun <T> List<T>.second(): T = this[1]\n\
            interface Handler\n";
        assert_eq!(
            defs(Lang::Kotlin, source),
            [
                ("Server".to_string(), DefKind::Class, 3, 9),
                ("Server.name".to_string(), DefKind::Field, 4, 4),
                ("Server.serve".to_string(), DefKind::Method, 5, 8),
                ("second".to_string(), DefKind::Function, 11, 11),
                ("Handler".to_string(), DefKind::Interface, 12, 12),
            ]
        );
    }

    #[test]
    fn ruby_s_definitions_end_at_their_end() {
        let source = "module Crystal\n  class Session\n    MAX = 3\n    def stop!\n      nil\n    end\n\n    def self.start\n    end\n  end\nend\n";
        assert_eq!(
            defs(Lang::Ruby, source),
            [
                ("Crystal".to_string(), DefKind::Module, 1, 11),
                ("Crystal.Session".to_string(), DefKind::Class, 2, 10),
                ("Crystal.Session.MAX".to_string(), DefKind::Const, 3, 3),
                ("Crystal.Session.stop!".to_string(), DefKind::Method, 4, 6),
                ("Crystal.Session.start".to_string(), DefKind::Method, 8, 9),
            ]
        );
    }

    #[test]
    fn a_shell_script_s_functions_and_settings_are_read() {
        let source = "#!/bin/sh\n# a comment()\nPREFIX=/usr\nusage() {\n  echo hi\n}\nfunction main {\n  usage\n}\n";
        assert_eq!(
            defs(Lang::Shell, source),
            [
                ("PREFIX".to_string(), DefKind::Variable, 3, 3),
                ("usage".to_string(), DefKind::Function, 4, 6),
                ("main".to_string(), DefKind::Function, 7, 9),
            ]
        );
    }

    #[test]
    fn csharp_methods_need_a_modifier_and_comments_are_passed_over() {
        let source = "namespace App.Web\n{\n    // class Fake\n    public sealed class Server : IServer\n    {\n        public int Port { get; set; }\n        public async Task<int> ServeAsync(int x)\n        {\n            if (x) { }\n        }\n    }\n}\n";
        assert_eq!(
            defs(Lang::CSharp, source),
            [
                ("App.Web".to_string(), DefKind::Module, 1, 12),
                ("App.Web.Server".to_string(), DefKind::Class, 4, 11),
                ("App.Web.Server.Port".to_string(), DefKind::Field, 6, 6),
                (
                    "App.Web.Server.ServeAsync".to_string(),
                    DefKind::Method,
                    7,
                    10
                ),
            ]
        );
    }

    #[test]
    fn languages_with_no_keywords_are_not_read() {
        assert!(outline(Lang::Rust, b"fn main() {}").is_none());
        assert!(reads(Lang::Swift));
        assert!(!reads(Lang::Go));
    }
}
