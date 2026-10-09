//! What's checked in a writer's text before it's kept, and the linker.
//!
//! Every `code:` link must point at a file the repository has at the
//! commit, at lines it has; when its label names a symbol, the symbol must
//! be on or near those lines, or the link is moved to where it is. Every
//! `#id` must be an anchor on the page. A path into the checkout, the
//! user's home or anywhere outside the repository is taken out, and so is
//! anything that looks like a credential. A diagram must read: crystal's
//! own mermaid reader reads it, after its labels are quoted the way
//! mermaid wants and its colours taken out.
//!
//! What can't be put right here is a problem, which the writer is asked to
//! fix once; what's still wrong after that is dropped: a link left as its
//! label, a diagram left out. A wrong link is worse than none.
//!
//! The linker then makes every code span in the prose that names something
//! the repository has, one thing and no other, a link to it.

use super::crystal;
use super::files::Files;
use super::index::{Def, DefKind, Index, Lookup};
use super::model::{CodeLink, Diagram};
use super::prose;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::ops::AddAssign;
use std::path::Path;

/// How far from a link's lines a symbol its label names may be.
const NEAR_LINES: u32 = 3;

/// The longest a range a link is moved to is: a definition longer than
/// this is linked at its first line.
const LONGEST_RANGE: u32 = 40;

/// What a text is checked against.
pub struct Context<'a> {
    /// The checkout of the commit.
    pub root: &'a Path,
    /// The project's main worktree, which a writer may name.
    pub project: &'a Path,
    pub files: &'a Files,
    pub index: &'a Index,
    /// Every anchor on the page.
    pub anchors: &'a BTreeSet<String>,
    /// The files and directories the text is about.
    pub near: &'a [String],
}

/// What checking and linking found and did, counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    /// Links into the code checked.
    #[serde(default)]
    pub links: usize,
    /// Of those, moved to where what their label names is, or to the
    /// file it's in.
    #[serde(default)]
    pub moved: usize,
    /// Links that couldn't be put right, left as their label.
    #[serde(default)]
    pub dropped: usize,
    /// Code spans the linker made links.
    #[serde(default)]
    pub linked: usize,
    /// Code spans it left, naming nothing it could tell.
    #[serde(default)]
    pub unlinked: usize,
    /// Diagrams that didn't read until the writer wrote them again.
    #[serde(default)]
    pub diagrams_fixed: usize,
    /// Diagrams that never read, left out.
    #[serde(default)]
    pub diagrams_dropped: usize,
    /// Credentials taken out.
    #[serde(default)]
    pub redacted: usize,
}

impl AddAssign for Tally {
    fn add_assign(&mut self, other: Tally) {
        self.links += other.links;
        self.moved += other.moved;
        self.dropped += other.dropped;
        self.linked += other.linked;
        self.unlinked += other.unlinked;
        self.diagrams_fixed += other.diagrams_fixed;
        self.diagrams_dropped += other.diagrams_dropped;
        self.redacted += other.redacted;
    }
}

/// A text, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub text: String,
    /// What's wrong that the writer can put right, a line each.
    pub problems: Vec<String>,
    pub tally: Tally,
}

/// Checks `md` against `ctx`: what can be put right is, and the rest is a
/// problem, left as it is, or with `last`, dropped.
pub fn text(md: &str, ctx: &Context, last: bool) -> Checked {
    let mut tally = Tally::default();
    let md = outside_paths(md, ctx);
    let redacted = crystal::redact(&md);
    if redacted != md {
        tally.redacted += 1;
    }
    let md = redacted;
    let mut problems = Vec::new();
    let mut edits = Vec::new();
    let mut lines = Lines::new(ctx.root);
    for link in prose::links(&md) {
        let verdict = if let Some(code) = CodeLink::parse(&link.dest) {
            tally.links += 1;
            code_link(&link.label, &code, ctx, &mut lines)
        } else if let Some(id) = link.dest.strip_prefix('#') {
            if ctx.anchors.contains(id) {
                Verdict::Good
            } else {
                Verdict::Wrong(format!(
                    "the link [{}](#{id}) goes to no part of the page: use one of the ids in the outline",
                    link.label
                ))
            }
        } else if link.dest.starts_with("http://")
            || link.dest.starts_with("https://")
            || link.dest.starts_with("mailto:")
        {
            Verdict::Good
        } else {
            // `src/a.rs`, `./src/a.rs#L3`: a path into the repository
            // written as a plain link.
            let as_code = format!("code:{}", link.dest.trim_start_matches("./"));
            match CodeLink::parse(&as_code) {
                Some(code) if ctx.files.entry(&code.path).is_some() => {
                    tally.links += 1;
                    match code_link(&link.label, &code, ctx, &mut lines) {
                        Verdict::Good => Verdict::Moved(code),
                        other => other,
                    }
                }
                _ => Verdict::Wrong(format!(
                    "the link [{}]({}) isn't a link into the code, a part of the page or the web",
                    link.label, link.dest
                )),
            }
        };
        match verdict {
            Verdict::Good => {}
            Verdict::Moved(code) => {
                tally.moved += 1;
                edits.push((link.range, format!("[{}]({})", link.label, code.target())));
            }
            Verdict::Wrong(problem) if last => {
                tally.dropped += 1;
                let _ = problem;
                edits.push((link.range, link.label.clone()));
            }
            Verdict::Wrong(problem) => problems.push(problem),
        }
    }
    let text = prose::splice(&md, edits);
    Checked {
        text,
        problems,
        tally,
    }
}

/// What's to be done with a link.
enum Verdict {
    Good,
    /// It's put right: it goes here.
    Moved(CodeLink),
    /// It's wrong, as this says.
    Wrong(String),
}

/// Checks a link into the code labelled `label`.
fn code_link(label: &str, code: &CodeLink, ctx: &Context, lines: &mut Lines) -> Verdict {
    let shown = format!("[{label}]({})", code.target());
    let path = code.path.as_str();
    if path.starts_with('/') || path.split('/').any(|part| part == "..") {
        return Verdict::Wrong(format!(
            "{shown} points outside the repository: give paths from its top"
        ));
    }
    let symbol = label_symbol(label);
    let Some(file) = ctx.files.get(path) else {
        if ctx.files.is_dir(path) {
            let dir = format!("{}/", path.trim_end_matches('/'));
            return if code.start.is_some() || dir != path {
                Verdict::Moved(CodeLink {
                    path: dir,
                    start: None,
                    end: None,
                })
            } else {
                Verdict::Good
            };
        }
        // The right symbol in the wrong file, or a file's name without
        // its directory.
        let found = symbol
            .as_deref()
            .map(|symbol| ctx.index.lookup(symbol, ctx.near))
            .or_else(|| Some(ctx.index.lookup(path, ctx.near)));
        if let Some(Lookup::Unique(def)) = found {
            return Verdict::Moved(at_def(&def, def.kind == DefKind::File));
        }
        return Verdict::Wrong(format!(
            "{shown}: the repository has no file {path} at this commit"
        ));
    };
    let Some(start) = code.start else {
        return Verdict::Good;
    };
    let last = file.lines.max(1);
    let end = code.end.unwrap_or(start).max(start);
    let in_range = start >= 1 && start <= last;
    let Some(symbol) = symbol else {
        return match (in_range, end > last) {
            (true, false) => Verdict::Good,
            (true, true) => Verdict::Moved(CodeLink {
                end: Some(last),
                ..code.clone()
            }),
            (false, _) => Verdict::Wrong(format!(
                "{shown}: {path} has {last} lines, so line {start} isn't in it"
            )),
        };
    };
    let word = symbol_word(&symbol);
    if in_range {
        let defined_here = ctx
            .index
            .defined_at(path, start)
            .iter()
            .any(|def| def.name == word || def.name == symbol);
        if defined_here || lines.has_word(path, &word, start, end.min(last)) {
            return if end > last {
                Verdict::Moved(CodeLink {
                    end: Some(last),
                    ..code.clone()
                })
            } else {
                Verdict::Good
            };
        }
    }
    // Where what the label names is: its definition, in this file first.
    let mut near = vec![path.to_string()];
    near.extend(ctx.near.iter().cloned());
    if let Lookup::Unique(def) = ctx.index.lookup(&symbol, &near) {
        return Verdict::Moved(at_def(&def, false));
    }
    if let Some(line) = lines.only_line_with(path, &word) {
        return Verdict::Moved(CodeLink {
            path: path.to_string(),
            start: Some(line),
            end: None,
        });
    }
    Verdict::Wrong(format!(
        "{shown}: `{symbol}` isn't on or near those lines of {path}, and couldn't be found \
         there for sure: link it where it's defined"
    ))
}

/// A link to `def`: its lines, or with `whole_file`, its file.
fn at_def(def: &Def, whole_file: bool) -> CodeLink {
    if whole_file {
        return CodeLink {
            path: def.path.clone(),
            start: None,
            end: None,
        };
    }
    let (start, end) = (def.start, def.end);
    let end = (end > start && end - start <= LONGEST_RANGE).then_some(end);
    CodeLink {
        path: def.path.clone(),
        start: Some(start),
        end,
    }
}

/// The symbol a link's label names, when it names one: `` `spawn` ``,
/// `Session::spawn()`, `--model`; not prose, not a path.
pub fn label_symbol(label: &str) -> Option<String> {
    let label = label.trim();
    let inner = label
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix('`'))
        .unwrap_or(label)
        .trim();
    if inner.is_empty() || inner.contains(char::is_whitespace) || inner.contains('/') {
        return None;
    }
    let bare = inner
        .trim_start_matches(['&', '*'])
        .trim_end_matches("()")
        .trim_end_matches('!');
    // A word of prose, `here`, isn't one; a code span, or what only code
    // writes, `snake_case`, `Type::method`, `call()` or `--flag`, is.
    let code_like = label.starts_with('`')
        || inner.starts_with("--")
        || inner.contains(['_', ':', '(', '<'])
        || inner.chars().skip(1).any(char::is_uppercase);
    let ok = code_like
        && (bare.starts_with("--")
            || bare.chars().all(|c| {
                c.is_alphanumeric() || matches!(c, '_' | ':' | '.' | '-' | '<' | '>' | '$')
            }));
    let has_letter = bare.chars().any(char::is_alphabetic);
    // A file's name, `config.toml`, is no symbol.
    let file_like = bare.rsplit_once('.').is_some_and(|(_, ext)| {
        matches!(
            ext,
            "rs" | "go"
                | "py"
                | "ts"
                | "js"
                | "md"
                | "toml"
                | "json"
                | "yml"
                | "yaml"
                | "sh"
                | "c"
                | "h"
        )
    });
    (ok && has_letter && !file_like).then(|| inner.to_string())
}

/// The word to look for in the code for a symbol: its last part, or a
/// flag's name with its dashes as a field's underscores.
fn symbol_word(symbol: &str) -> String {
    if let Some(flag) = symbol.strip_prefix("--") {
        return flag.replace('-', "_");
    }
    let bare = symbol
        .trim_start_matches(['&', '*'])
        .trim_end_matches("()")
        .trim_end_matches('!');
    let bare = bare.split('<').next().unwrap_or(bare);
    bare.rsplit(['.', ':'])
        .find(|part| !part.is_empty())
        .unwrap_or(bare)
        .to_string()
}

/// The lines of the files links point into, read once each.
struct Lines<'a> {
    root: &'a Path,
    read: HashMap<String, Vec<String>>,
}

impl<'a> Lines<'a> {
    fn new(root: &'a Path) -> Lines<'a> {
        Lines {
            root,
            read: HashMap::new(),
        }
    }

    fn of(&mut self, path: &str) -> &[String] {
        self.read.entry(path.to_string()).or_insert_with(|| {
            fs::read_to_string(self.root.join(path))
                .map(|text| text.lines().map(str::to_string).collect())
                .unwrap_or_default()
        })
    }

    /// Whether `word` is on one of the lines `start` to `end` of `path`,
    /// or [`NEAR_LINES`] from them, as a word of its own.
    fn has_word(&mut self, path: &str, word: &str, start: u32, end: u32) -> bool {
        let lines = self.of(path);
        let from = start.saturating_sub(NEAR_LINES).max(1) as usize - 1;
        let to = ((end + NEAR_LINES) as usize).min(lines.len());
        lines
            .get(from..to)
            .is_some_and(|lines| lines.iter().any(|line| has_word(line, word)))
    }

    /// The one line of `path` that has `word`, or `None` for none or more
    /// than one.
    fn only_line_with(&mut self, path: &str, word: &str) -> Option<u32> {
        let mut found = self
            .of(path)
            .iter()
            .enumerate()
            .filter(|(_, line)| has_word(line, word));
        let (first, _) = found.next()?;
        found.next().is_none().then_some(first as u32 + 1)
    }
}

/// Whether `word` is in `line` as a word of its own.
fn has_word(line: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(word).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + word.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

/// `md` with the checkout's path and the project's taken out of paths
/// into them, and the home directory written `~`: a wiki says nothing of
/// where it was made.
fn outside_paths(md: &str, ctx: &Context) -> String {
    let mut out = md.to_string();
    let spelled = |path: &Path| {
        let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        [path.to_path_buf(), real]
    };
    for root in spelled(ctx.root).iter().chain(spelled(ctx.project).iter()) {
        let root = root.to_string_lossy();
        if root.len() > 1 {
            out = out
                .replace(&format!("{root}/"), "")
                .replace(root.as_ref(), ".");
        }
    }
    if let Some(home) = std::env::var_os("HOME").filter(|home| home.len() > 1) {
        let home = home.to_string_lossy().trim_end_matches('/').to_string();
        out = out.replace(&format!("{home}/"), "~/");
    }
    out
}

/// A diagram as the page will have it: out of a fence if it was put in
/// one, its colours taken out, a flowchart's labels quoted the way mermaid
/// wants them, its credentials taken out; or why it doesn't read.
pub fn diagram(diagram: &Diagram) -> Result<Diagram, String> {
    let mut source = diagram.mermaid.trim().to_string();
    if source.starts_with("```") {
        source = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n");
    }
    let source = source
        .lines()
        .filter(|line| {
            let first = line.split_whitespace().next().unwrap_or("");
            !matches!(first, "classDef" | "class" | "style" | "linkStyle")
        })
        .collect::<Vec<_>>()
        .join("\n");
    crystal::mermaid_reads(&source)?;
    let first = source.split_whitespace().next().unwrap_or_default();
    let source = if matches!(first, "flowchart" | "graph") {
        quote_labels(&source)
    } else {
        source
    };
    crystal::mermaid_reads(&source)
        .map_err(|why| format!("{why} (once its labels were quoted)"))?;
    let caption = crystal::redact(diagram.caption.trim());
    Ok(Diagram {
        mermaid: crystal::redact(&source),
        caption,
    })
}

/// A flowchart with every node's label that isn't quoted quoted, and every
/// edge's label that has more than words: mermaid reads `a[Loader
/// (src/x.rs)]` as a shape it doesn't know, `a["Loader (src/x.rs)"]` as
/// meant.
pub fn quote_labels(source: &str) -> String {
    let mut out = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let first = line.split_whitespace().next().unwrap_or("");
        if index == 0
            || matches!(first, "subgraph" | "end" | "direction" | "click")
            || first.starts_with("%%")
        {
            out.push(line.to_string());
            continue;
        }
        out.push(quote_line(line));
    }
    out.join("\n")
}

/// The shapes a node's label is in, longest first, each with its close.
const SHAPES: &[(&str, &str)] = &[
    ("(((", ")))"),
    ("([", "])"),
    ("[(", ")]"),
    ("[[", "]]"),
    ("((", "))"),
    ("{{", "}}"),
    ("[/", "/]"),
    ("[\\", "\\]"),
    ("[", "]"),
    ("(", ")"),
    ("{", "}"),
];

fn quote_line(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    // Not `-`, which ends an arrow: `-->` is no node.
    let is_id = |c: char| c.is_alphanumeric() || c == '_';
    while i < chars.len() {
        let c = chars[i];
        // An edge's label: `-->|label|`.
        if c == '|'
            && let Some(close) = chars[i + 1..].iter().position(|&c| c == '|')
        {
            let label: String = chars[i + 1..i + 1 + close].iter().collect();
            let plain = label
                .chars()
                .all(|c| c.is_alphanumeric() || " _-,.'".contains(c));
            if plain || label.trim().starts_with('"') {
                out.push('|');
                out.push_str(&label);
            } else {
                out.push_str(&format!("|\"{}\"", label.replace('"', "'")));
            }
            out.push('|');
            i += close + 2;
            continue;
        }
        // A node's label, right after its id.
        let after_id = i > 0 && is_id(chars[i - 1]);
        let rest: String = chars[i..].iter().collect();
        if after_id
            && let Some((open, close)) = SHAPES.iter().find(|(open, _)| rest.starts_with(open))
        {
            let body = &rest[open.len()..];
            if let Some(end) = closing(body, open, close) {
                let label = &body[..end];
                out.push_str(open);
                if label.trim_start().starts_with('"') {
                    out.push_str(label);
                } else {
                    out.push('"');
                    out.push_str(&label.replace('"', "'"));
                    out.push('"');
                }
                out.push_str(close);
                i += open.chars().count() + label.chars().count() + close.chars().count();
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Where in `body`, a label after its shape's `open`, the shape closes:
/// at `close`, past any brackets the label opens and closes itself, or a
/// quoted part.
fn closing(body: &str, open: &str, close: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut quoted = false;
    let opener = open.chars().last()?;
    let closer = close.chars().next()?;
    for (at, c) in body.char_indices() {
        if c == '"' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        if depth == 0 && body[at..].starts_with(close) {
            return Some(at);
        }
        if c == opener {
            depth += 1;
        } else if c == closer {
            depth -= 1;
        }
    }
    None
}

/// The linker: each code span in `md`'s prose that names one thing the
/// repository has, by [`Index::lookup`], made a link to it.
pub fn link(md: &str, index: &Index, near: &[String]) -> (String, Tally) {
    let mut tally = Tally::default();
    let mut edits = Vec::new();
    for code in prose::code_spans(md) {
        if !worth_linking(&code.text) {
            continue;
        }
        match index.lookup(&code.text, near) {
            Lookup::Unique(def) => {
                tally.linked += 1;
                // A file is linked whole, unless the span gave a line.
                let line_given = code.text.contains("#L")
                    || code
                        .text
                        .rsplit(':')
                        .next()
                        .is_some_and(|line| line.parse::<u32>().is_ok());
                let target = match def.kind {
                    DefKind::Directory => CodeLink {
                        path: def.path.clone(),
                        start: None,
                        end: None,
                    },
                    DefKind::File if !line_given => CodeLink {
                        path: def.path.clone(),
                        start: None,
                        end: None,
                    },
                    _ => CodeLink {
                        path: def.path.clone(),
                        start: Some(def.start),
                        end: None,
                    },
                };
                let source = &md[code.range.clone()];
                edits.push((code.range, format!("[{source}]({})", target.target())));
            }
            Lookup::Ambiguous(_) | Lookup::Missing => tally.unlinked += 1,
        }
    }
    (prose::splice(md, edits), tally)
}

/// Whether a code span could name something: not a literal, a keyword or
/// a value.
fn worth_linking(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && !matches!(
            text,
            "true" | "false" | "null" | "None" | "Some" | "Ok" | "Err" | "self" | "Self" | "nil"
        )
        && !text.starts_with(['"', '\''])
        && !text.chars().all(|c| c.is_ascii_digit() || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiki::files::File;

    struct Repo {
        dir: tempfile::TempDir,
        files: Files,
        index: Index,
        anchors: BTreeSet<String>,
    }

    const SESSION: &str = "pub struct Session {\n    pub name: String,\n}\n\nimpl Session {\n    pub fn spawn(&self) {\n        start();\n    }\n}\n\nfn start() {}\n";

    fn repo() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Files::default();
        for (path, text) in [
            ("src/session.rs", SESSION),
            ("src/main.rs", "fn main() {}\n"),
        ] {
            let file = dir.path().join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, text).unwrap();
            files.add(File {
                path: path.into(),
                blob: "b".into(),
                lines: text.lines().count() as u32,
                bytes: text.len() as u64,
                text: true,
            });
        }
        let index = Index::of(dir.path(), &files);
        let anchors = ["sessions".to_string()].into();
        Repo {
            dir,
            files,
            index,
            anchors,
        }
    }

    fn check(repo: &Repo, md: &str, last: bool) -> Checked {
        let near = vec!["src/session.rs".to_string()];
        let ctx = Context {
            root: repo.dir.path(),
            project: Path::new("/code/app"),
            files: &repo.files,
            index: &repo.index,
            anchors: &repo.anchors,
            near: &near,
        };
        text(md, &ctx, last)
    }

    #[test]
    fn a_link_to_the_right_lines_is_kept() {
        let repo = repo();
        let md = "[`Session::spawn`](code:src/session.rs#L6-L8) and [the file](code:src/main.rs) and [the struct](code:src/session.rs#L1-L3) and [sessions](#sessions).";
        let checked = check(&repo, md, false);
        assert_eq!(checked.text, md);
        assert!(checked.problems.is_empty(), "{:?}", checked.problems);
        assert_eq!((checked.tally.links, checked.tally.moved), (3, 0));
    }

    #[test]
    fn a_link_whose_symbol_is_elsewhere_is_moved_to_it() {
        let repo = repo();
        let checked = check(&repo, "[`spawn`](code:src/session.rs#L1)", false);
        assert_eq!(checked.text, "[`spawn`](code:src/session.rs#L6-L8)");
        // In the wrong file altogether.
        let checked = check(&repo, "[`start()`](code:src/main.rs#L1)", false);
        assert_eq!(checked.text, "[`start()`](code:src/session.rs#L11)");
        // Or a file that isn't there.
        let checked = check(&repo, "[`Session`](code:src/sessions.rs#L1)", false);
        assert_eq!(checked.text, "[`Session`](code:src/session.rs#L1-L3)");
        assert_eq!(checked.tally.moved, 1);
        // Lines past the end are cut to it.
        let checked = check(&repo, "[the end](code:src/main.rs#L1-L9)", false);
        assert_eq!(checked.text, "[the end](code:src/main.rs#L1)");
    }

    #[test]
    fn what_cannot_be_put_right_is_a_problem_then_dropped() {
        let repo = repo();
        let md = "[gone](code:src/gone.rs#L3), [`nothing`](code:src/main.rs#L1), [far](code:src/main.rs#L40), [out](code:../etc/passwd), [lost](#lost) and [web](https://example.com).";
        let checked = check(&repo, md, false);
        assert_eq!(checked.text, md);
        assert_eq!(checked.problems.len(), 5, "{:?}", checked.problems);
        assert!(checked.problems[0].contains("no file src/gone.rs"));
        assert!(checked.problems[1].contains("`nothing` isn't on or near"));
        assert!(checked.problems[2].contains("has 1 lines"));
        assert!(checked.problems[3].contains("outside the repository"));
        assert!(checked.problems[4].contains("goes to no part of the page"));
        let dropped = check(&repo, md, true);
        assert_eq!(
            dropped.text,
            "gone, `nothing`, far, out, lost and [web](https://example.com)."
        );
        assert_eq!(dropped.tally.dropped, 5);
    }

    #[test]
    fn a_plain_link_to_a_file_becomes_a_link_into_the_code() {
        let repo = repo();
        let checked = check(&repo, "[main](src/main.rs)", false);
        assert_eq!(checked.text, "[main](code:src/main.rs)");
    }

    #[test]
    fn where_it_was_made_and_credentials_are_taken_out() {
        let repo = repo();
        let root = repo.dir.path().display().to_string();
        let md = format!(
            "It reads {root}/src/main.rs and /code/app/src/session.rs. API_KEY=sk-abcdefghijklmnopqrstuvwxyz123456"
        );
        let checked = check(&repo, &md, false);
        assert_eq!(
            checked.text,
            "It reads src/main.rs and src/session.rs. API_KEY=[redacted]"
        );
        assert_eq!(checked.tally.redacted, 1);
    }

    #[test]
    fn links_survive_the_credentials_check() {
        let md = "[`secrets::redact`](code:src/secrets.rs#L15-L40) and [`api_key`](code:src/config.rs#L10) and [`gemini_key_file`](code:src/config.rs#L677)";
        assert_eq!(crystal::redact(md), md);
    }

    #[test]
    fn the_linker_links_what_names_one_thing() {
        let repo = repo();
        let md = "`Session` holds `name`; `spawn()` calls `start`, in `src/session.rs`; `main.rs` and `src/` too. `true`, `cargo test` and `Nothing` stay. [`spawn`](code:src/session.rs#L6) is linked already.";
        let (out, tally) = link(md, &repo.index, &["src/session.rs".into()]);
        assert_eq!(
            out,
            "[`Session`](code:src/session.rs#L1) holds [`name`](code:src/session.rs#L2); [`spawn()`](code:src/session.rs#L6) calls [`start`](code:src/session.rs#L11), in [`src/session.rs`](code:src/session.rs); [`main.rs`](code:src/main.rs) and [`src/`](code:src/) too. `true`, `cargo test` and `Nothing` stay. [`spawn`](code:src/session.rs#L6) is linked already."
        );
        assert_eq!((tally.linked, tally.unlinked), (7, 2));
    }

    #[test]
    fn a_diagram_is_read_its_labels_quoted_and_its_colours_dropped() {
        let given = Diagram {
            mermaid: "```mermaid\nflowchart TD\n  loader[Loader (src/loader.rs)] -->|loads (lazily)| core[\"Core\"]\n  core -.->|calls| out((Output))\n  classDef hot fill:#f00\n  style core fill:#fff\n```".into(),
            caption: " How it loads. ".into(),
        };
        let checked = diagram(&given).unwrap();
        assert_eq!(
            checked.mermaid,
            "flowchart TD\n  loader[\"Loader (src/loader.rs)\"] -->|\"loads (lazily)\"| core[\"Core\"]\n  core -.->|calls| out((\"Output\"))"
        );
        assert_eq!(checked.caption, "How it loads.");
        let sequence = Diagram {
            mermaid: "sequenceDiagram\n  participant C as CLI (src/main.rs)\n  C->>D: build".into(),
            caption: String::new(),
        };
        assert!(diagram(&sequence).is_ok());
        let broken = Diagram {
            mermaid: "flowchart TD\n  a --> b\n  end".into(),
            caption: String::new(),
        };
        assert!(diagram(&broken).is_err());
        assert!(
            diagram(&Diagram {
                mermaid: "pie\n \"a\": 1".into(),
                caption: String::new()
            })
            .is_err()
        );
    }

    #[test]
    fn a_label_names_a_symbol_or_not() {
        assert_eq!(
            label_symbol("`Session::spawn()`").as_deref(),
            Some("Session::spawn()")
        );
        assert_eq!(
            label_symbol("--max-budget-usd").as_deref(),
            Some("--max-budget-usd")
        );
        assert_eq!(label_symbol("the spawn function"), None);
        assert_eq!(label_symbol("here"), None);
        assert_eq!(label_symbol("spawn_all").as_deref(), Some("spawn_all"));
        assert_eq!(label_symbol("`src/a.rs`"), None);
        assert_eq!(label_symbol("`config.toml`"), None);
        assert_eq!(label_symbol("`42`"), None);
        assert_eq!(symbol_word("Session::spawn()"), "spawn");
        assert_eq!(symbol_word("--max-budget-usd"), "max_budget_usd");
        assert_eq!(symbol_word("Vec<Def>"), "Vec");
    }
}
