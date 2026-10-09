//! The index of what a repository defines, by name, which the wiki's
//! links are made from: every file and directory git has, and each
//! function, type, method, field, constant, flag, config key and command
//! the code defines, with the lines it's on.
//!
//! This one is a light scan of each file ([`scan`]): no parser, no
//! language server, a guess at where each definition starts and ends,
//! every [`Def`] it makes `precise: false`. The rule everything that reads
//! it keeps is that a wrong link is worse than none: [`Index::lookup`]
//! answers [`Lookup::Unique`] only when one definition is meant, and a
//! name it can't tell apart stays unlinked.

pub mod scan;

use super::files::{self, Files};
use anyhow::Result;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

/// What changes how the index is made.
#[derive(Debug, Clone, Default)]
pub struct IndexSettings {
    /// Globs of the files left out, as `[wiki] exclude` gives them.
    pub exclude: Vec<String>,
}

/// What a definition is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefKind {
    File,
    Directory,
    Module,
    Function,
    Method,
    Struct,
    Enum,
    Variant,
    Trait,
    Interface,
    Class,
    Type,
    Const,
    Static,
    Variable,
    Field,
    Macro,
    Flag,
    ConfigKey,
    Command,
}

impl DefKind {
    /// Whether it's a type: what a bare name means first.
    pub fn is_type(self) -> bool {
        matches!(
            self,
            DefKind::Struct
                | DefKind::Enum
                | DefKind::Trait
                | DefKind::Interface
                | DefKind::Class
                | DefKind::Type
        )
    }

    /// Whether what's defined inside it is its: a type's, or a module's.
    pub fn is_container(self) -> bool {
        self.is_type() || self == DefKind::Module
    }

    /// The word a writer's outline names it by.
    pub fn label(self) -> &'static str {
        match self {
            DefKind::File => "file",
            DefKind::Directory => "dir",
            DefKind::Module => "mod",
            DefKind::Function => "fn",
            DefKind::Method => "method",
            DefKind::Struct => "struct",
            DefKind::Enum => "enum",
            DefKind::Variant => "variant",
            DefKind::Trait => "trait",
            DefKind::Interface => "interface",
            DefKind::Class => "class",
            DefKind::Type => "type",
            DefKind::Const => "const",
            DefKind::Static => "static",
            DefKind::Variable => "var",
            DefKind::Field => "field",
            DefKind::Macro => "macro",
            DefKind::Flag => "flag",
            DefKind::ConfigKey => "key",
            DefKind::Command => "command",
        }
    }
}

/// Something the repository defines, where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Def {
    /// Its own name: `spawn`, `--model`, `budget_usd`.
    pub name: String,
    /// Its name with what it's in: `session::Session::spawn`.
    pub qualified: String,
    pub kind: DefKind,
    /// The file it's in, from the top of the repository.
    pub path: String,
    /// The lines it's on, from 1.
    pub start: u32,
    pub end: u32,
    /// Whether a parser or an indexer placed it, rather than a guess.
    pub precise: bool,
}

/// What a name in the prose is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// The one thing it means.
    Unique(Def),
    /// Several things it could mean, none of them more than another.
    Ambiguous(Vec<Def>),
    /// Nothing the repository defines.
    Missing,
}

/// What a repository defines at a commit.
#[derive(Debug, Clone, Default)]
pub struct Index {
    defs: Vec<Def>,
    by_name: HashMap<String, Vec<usize>>,
    by_path: BTreeMap<String, Vec<usize>>,
    /// Every file with its lines, and every directory.
    files: BTreeMap<String, u32>,
    dirs: Vec<String>,
    by_basename: HashMap<String, Vec<String>>,
}

impl Index {
    /// The index of the checkout at `root`, which is at `commit`. This
    /// one keeps nothing in `cache`: a light scan is quick enough to make
    /// again.
    pub fn build(
        root: &Path,
        commit: &str,
        cache: &Path,
        settings: &IndexSettings,
    ) -> Result<Index> {
        let _ = (commit, cache);
        let listed = Files::list(root, &settings.exclude)?;
        Ok(Index::of(root, &listed))
    }

    /// The index of `listed`, each text file read from `root`.
    pub fn of(root: &Path, listed: &Files) -> Index {
        let mut index = Index::default();
        for file in listed.files.values() {
            index.files.insert(file.path.clone(), file.lines.max(1));
            let base = file.path.rsplit('/').next().unwrap_or(&file.path);
            index
                .by_basename
                .entry(base.to_string())
                .or_default()
                .push(file.path.clone());
            if !file.text {
                continue;
            }
            let Some(lang) = scan::lang_of(&file.path) else {
                continue;
            };
            let Ok(text) = fs::read_to_string(root.join(&file.path)) else {
                continue;
            };
            for def in scan::scan(&file.path, lang, &text) {
                index.add(def);
            }
        }
        index.dirs = listed.dirs.iter().cloned().collect();
        index
    }

    fn add(&mut self, def: Def) {
        let at = self.defs.len();
        self.by_name.entry(def.name.clone()).or_default().push(at);
        self.by_path.entry(def.path.clone()).or_default().push(at);
        self.defs.push(def);
    }

    /// What the code span `span` names: a file or a directory, a flag, a
    /// config key, a command, or a symbol, plain or qualified. Among
    /// several, the one in `near`, the files and directories the text it's
    /// in is about, is meant; then a type over what isn't one. Anything
    /// else stays [`Lookup::Ambiguous`].
    pub fn lookup(&self, span: &str, near: &[String]) -> Lookup {
        let mut span = span.trim().trim_matches('`').trim();
        // A reference or a trait object is the type it's of.
        for prefix in ["&mut ", "&", "*", "mut ", "dyn ", "impl "] {
            span = span.strip_prefix(prefix).unwrap_or(span).trim_start();
        }
        if span.is_empty() || span.len() > 200 {
            return Lookup::Missing;
        }
        if let Some(found) = self.path(span) {
            return found;
        }
        if span.split_whitespace().count() > 1 {
            return self.phrase(span, near);
        }
        if span.starts_with('-') {
            let flag = span.split('=').next().unwrap_or(span);
            return self.choose(self.named(flag, |kind| kind == DefKind::Flag), near);
        }
        let Some(segments) = symbol(span) else {
            return Lookup::Missing;
        };
        let (name, qualifiers) = segments.split_last().expect("a symbol has a name");
        let candidates = self.named(name, |kind| {
            !matches!(
                kind,
                DefKind::File | DefKind::Directory | DefKind::Flag | DefKind::Command
            )
        });
        let candidates: Vec<&Def> = candidates
            .into_iter()
            .filter(|def| qualified_by(def, qualifiers))
            .collect();
        self.choose(candidates, near)
    }

    /// The definitions in `path` whose lines take in `line`, the innermost
    /// first.
    pub fn defined_at(&self, path: &str, line: u32) -> Vec<&Def> {
        let mut found: Vec<&Def> = self
            .by_path
            .get(path)
            .into_iter()
            .flatten()
            .map(|&at| &self.defs[at])
            .filter(|def| def.start <= line && line <= def.end)
            .collect();
        found.sort_by_key(|def| def.end - def.start);
        found
    }

    /// What `files`, files and directories ending with `/`, define, by file
    /// and line.
    pub fn outline(&self, files: &[String]) -> Vec<&Def> {
        let mut found: Vec<&Def> = self
            .by_path
            .iter()
            .filter(|(path, _)| files::covers(files, path))
            .flat_map(|(_, defs)| defs.iter().map(|&at| &self.defs[at]))
            .collect();
        found.sort_by(|a, b| (&a.path, a.start).cmp(&(&b.path, b.start)));
        found
    }

    /// The definitions called `name` of a kind `kind` takes.
    fn named(&self, name: &str, kind: impl Fn(DefKind) -> bool) -> Vec<&Def> {
        self.by_name
            .get(name)
            .into_iter()
            .flatten()
            .map(|&at| &self.defs[at])
            .filter(|def| kind(def.kind))
            .collect()
    }

    /// The one of `candidates` that's meant, as [`Index::lookup`] says.
    fn choose(&self, candidates: Vec<&Def>, near: &[String]) -> Lookup {
        let mut candidates = candidates;
        // A definition the scan found twice on one line is one.
        candidates.dedup_by(|a, b| a.path == b.path && a.start == b.start);
        match candidates.len() {
            0 => return Lookup::Missing,
            1 => return Lookup::Unique(candidates[0].clone()),
            _ => {}
        }
        let close: Vec<&Def> = candidates
            .iter()
            .copied()
            .filter(|def| !near.is_empty() && files::covers(near, &def.path))
            .collect();
        if close.len() == 1 {
            return Lookup::Unique(close[0].clone());
        }
        let pool = if close.is_empty() {
            &candidates
        } else {
            &close
        };
        let types: Vec<&&Def> = pool.iter().filter(|def| def.kind.is_type()).collect();
        if types.len() == 1 {
            return Lookup::Unique((*types[0]).clone());
        }
        Lookup::Ambiguous(candidates.into_iter().cloned().collect())
    }

    /// `span` as a path: a file, `path:12`, `path:12:5` or `path#L12`, a
    /// directory, or a file's name only one file has; `None` when it
    /// doesn't look like one.
    fn path(&self, span: &str) -> Option<Lookup> {
        let span = span.trim_start_matches("./");
        let (path, line) = split_line(span);
        if let Some(&lines) = self.files.get(path) {
            let (start, end) = match line {
                Some(line) if line <= lines => (line, line),
                Some(_) => return Some(Lookup::Missing),
                None => (1, lines),
            };
            return Some(Lookup::Unique(self.file_def(
                path,
                DefKind::File,
                start,
                end,
            )));
        }
        let dir = path.trim_end_matches('/');
        if line.is_none()
            && !dir.is_empty()
            && self.dirs.binary_search_by(|d| d.as_str().cmp(dir)).is_ok()
        {
            return Some(Lookup::Unique(self.file_def(
                &format!("{dir}/"),
                DefKind::Directory,
                1,
                1,
            )));
        }
        if path.contains('/') {
            return Some(Lookup::Missing);
        }
        let named = self.by_basename.get(path)?;
        Some(match named.as_slice() {
            [only] => {
                let lines = self.files[only];
                let (start, end) = match line {
                    Some(line) if line <= lines => (line, line),
                    Some(_) => return Some(Lookup::Missing),
                    None => (1, lines),
                };
                Lookup::Unique(self.file_def(only, DefKind::File, start, end))
            }
            many => Lookup::Ambiguous(
                many.iter()
                    .map(|path| self.file_def(path, DefKind::File, 1, self.files[path]))
                    .collect(),
            ),
        })
    }

    fn file_def(&self, path: &str, kind: DefKind, start: u32, end: u32) -> Def {
        let name = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(path);
        Def {
            name: name.to_string(),
            qualified: path.to_string(),
            kind,
            path: path.to_string(),
            start,
            end,
            precise: true,
        }
    }

    /// A span of several words: a config key as `[table] key`, or a
    /// command line, `crystal wiki build`, its subcommand's definition.
    fn phrase(&self, span: &str, near: &[String]) -> Lookup {
        if let Some((table, key)) = span
            .strip_prefix('[')
            .and_then(|rest| rest.split_once(']'))
            .map(|(table, key)| (table.trim(), key.trim()))
            .filter(|(_, key)| is_identifier(key))
        {
            let table_word = squash(table.rsplit('.').next().unwrap_or(table));
            let keys = self.named(key, |kind| {
                matches!(kind, DefKind::ConfigKey | DefKind::Field)
            });
            let keys = keys
                .into_iter()
                .filter(|def| {
                    let container = container_of(def);
                    let container =
                        squash(container.rsplit(['.', ':']).next().unwrap_or(&container));
                    !table_word.is_empty() && container.contains(&table_word)
                })
                .collect();
            return self.choose(keys, near);
        }
        let words: Vec<&str> = span
            .split_whitespace()
            .take_while(|word| !word.starts_with('-') && is_command_word(word))
            .collect();
        if words.len() < 2
            || words.len()
                != span
                    .split_whitespace()
                    .take_while(|w| !w.starts_with('-'))
                    .count()
        {
            return Lookup::Missing;
        }
        let (name, before) = words.split_last().expect("two words at least");
        let commands = self.named(name, |kind| kind == DefKind::Command);
        let parent = squash(before.last().expect("one before it"));
        let under: Vec<&Def> = commands
            .iter()
            .copied()
            .filter(|def| squash(&container_of(def)).contains(&parent))
            .collect();
        if !under.is_empty() {
            return self.choose(under, near);
        }
        // `crystal done`: the program, then a command of its own.
        if words.len() == 2 {
            return self.choose(commands, near);
        }
        Lookup::Missing
    }
}

/// What a definition is in: its qualified name less its own.
fn container_of(def: &Def) -> String {
    let qualified = def.qualified.as_str();
    let without = qualified
        .strip_suffix(&def.name)
        .unwrap_or(qualified)
        .trim_end_matches([':', '.', ' ']);
    without.to_string()
}

/// Whether `def`'s qualified name ends with `qualifiers` and then its
/// name: `Session::spawn` names `session::Session::spawn`; a Rust module,
/// a Go package or a Python module before it does too.
fn qualified_by(def: &Def, qualifiers: &[String]) -> bool {
    if qualifiers.is_empty() {
        return true;
    }
    let parts: Vec<&str> = def
        .qualified
        .split(['.', ':'])
        .filter(|part| !part.is_empty())
        .collect();
    let Some((_, before)) = parts.split_last() else {
        return false;
    };
    before.len() >= qualifiers.len()
        && before[before.len() - qualifiers.len()..]
            .iter()
            .zip(qualifiers)
            .all(|(part, qualifier)| part == qualifier)
}

/// `span`'s path and the line after it: `src/a.rs:12`, `src/a.rs:12:5`
/// and `src/a.rs#L12` are at line 12.
fn split_line(span: &str) -> (&str, Option<u32>) {
    if let Some((path, line)) = span.split_once("#L") {
        let line = line.split('-').next().unwrap_or(line);
        return (path, line.parse().ok());
    }
    let mut parts = span.splitn(3, ':');
    let path = parts.next().unwrap_or(span);
    match parts.next().map(str::parse::<u32>) {
        Some(Ok(line)) => (path, Some(line)),
        _ => (span, None),
    }
}

/// A symbol's parts, its name last, from how code writes one: `Type::new`,
/// `obj.method()`, `&mut Session`, `Vec<Def>`, `crate::wiki::build`, `out!`.
/// `None` for what isn't one, like `a + b` or `"text"`.
fn symbol(span: &str) -> Option<Vec<String>> {
    let mut text = span.trim();
    for prefix in ["&mut ", "&", "*", "mut ", "dyn ", "impl "] {
        text = text.strip_prefix(prefix).unwrap_or(text).trim_start();
    }
    let text = strip_generics(text);
    let mut text = text.as_str().trim_end_matches(';').trim_end_matches('!');
    if let Some(at) = text.find('(') {
        if !text.ends_with(')') {
            return None;
        }
        text = &text[..at];
    }
    let text = text.strip_suffix("[]").unwrap_or(text);
    let sep = if text.contains("::") { "::" } else { "." };
    let mut parts: Vec<String> = text.split(sep).map(str::to_string).collect();
    while parts
        .first()
        .is_some_and(|first| matches!(first.as_str(), "crate" | "self" | "super" | "this"))
        && parts.len() > 1
    {
        parts.remove(0);
    }
    parts
        .iter()
        .all(|part| is_identifier(part))
        .then_some(parts)
}

/// `Vec<Def>` as `Vec`: what's between angle brackets taken out.
fn strip_generics(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn is_identifier(word: &str) -> bool {
    let mut chars = word.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$' || c == '#')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

fn is_command_word(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `word` lower-cased without `_` and `-`, to tell `WikiCommand` holds
/// `wiki`.
fn squash(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiki::files::File;

    /// An index of `sources`, written in a directory of their own.
    fn index(sources: &[(&str, &str)]) -> (tempfile::TempDir, Index) {
        let dir = tempfile::tempdir().unwrap();
        let mut listed = Files::default();
        for (path, text) in sources {
            let file = dir.path().join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, text).unwrap();
            listed.add(File {
                path: path.to_string(),
                blob: "b".into(),
                lines: text.lines().count() as u32,
                bytes: text.len() as u64,
                text: true,
            });
        }
        let index = Index::of(dir.path(), &listed);
        (dir, index)
    }

    fn unique(lookup: Lookup) -> (String, u32) {
        match lookup {
            Lookup::Unique(def) => (def.path, def.start),
            other => panic!("not unique: {other:?}"),
        }
    }

    const SESSION: &str = "pub struct Session {\n    pub name: String,\n}\n\nimpl Session {\n    pub fn spawn(&self) {}\n    pub fn run(&self) {}\n}\n";
    const TASK: &str = "pub fn run() {}\n\npub enum Kind {\n    Session,\n}\n";
    const CONFIG: &str = "pub struct WikiSettings {\n    pub budget_usd: f64,\n}\npub struct TaskSettings {\n    pub budget_usd: f64,\n}\n";

    #[test]
    fn a_name_one_thing_defines_is_linked_and_one_several_do_is_not() {
        let (_dir, index) = index(&[("src/session.rs", SESSION), ("src/task.rs", TASK)]);
        assert_eq!(
            unique(index.lookup("spawn", &[])),
            ("src/session.rs".into(), 6)
        );
        assert_eq!(
            unique(index.lookup("`Session::spawn()`", &[])),
            ("src/session.rs".into(), 6)
        );
        assert_eq!(
            unique(index.lookup("session::Session::run", &[])),
            ("src/session.rs".into(), 7)
        );
        assert_eq!(
            unique(index.lookup("task::run", &[])),
            ("src/task.rs".into(), 1)
        );
        assert!(matches!(index.lookup("run", &[]), Lookup::Ambiguous(found) if found.len() == 2));
        // Unless the text is about one of the files.
        assert_eq!(
            unique(index.lookup("run()", &["src/task.rs".into()])),
            ("src/task.rs".into(), 1)
        );
        // A type is meant over a variant of the same name.
        assert_eq!(
            unique(index.lookup("Session", &[])),
            ("src/session.rs".into(), 1)
        );
        assert_eq!(
            unique(index.lookup("&mut Session", &[])),
            ("src/session.rs".into(), 1)
        );
        assert_eq!(index.lookup("Other::spawn", &[]), Lookup::Missing);
        assert_eq!(index.lookup("nothing_here", &[]), Lookup::Missing);
        assert_eq!(index.lookup("a + b", &[]), Lookup::Missing);
        assert_eq!(index.lookup("cargo test --all", &[]), Lookup::Missing);
    }

    #[test]
    fn paths_directories_and_lines_are_linked() {
        let (_dir, index) = index(&[
            ("src/session.rs", SESSION),
            ("src/tui/app.rs", "fn a() {}\n"),
        ]);
        assert_eq!(
            unique(index.lookup("src/session.rs", &[])),
            ("src/session.rs".into(), 1)
        );
        assert_eq!(
            unique(index.lookup("src/session.rs:6", &[])),
            ("src/session.rs".into(), 6)
        );
        assert_eq!(
            unique(index.lookup("./src/session.rs#L7", &[])),
            ("src/session.rs".into(), 7)
        );
        assert_eq!(index.lookup("src/session.rs:99", &[]), Lookup::Missing);
        assert_eq!(
            unique(index.lookup("src/tui/", &[])),
            ("src/tui/".into(), 1)
        );
        assert_eq!(unique(index.lookup("src/tui", &[])), ("src/tui/".into(), 1));
        assert_eq!(
            unique(index.lookup("app.rs", &[])),
            ("src/tui/app.rs".into(), 1)
        );
        assert_eq!(index.lookup("src/gone.rs", &[]), Lookup::Missing);
    }

    #[test]
    fn config_keys_flags_and_commands_are_linked_by_what_they_are_in() {
        let main = "#[derive(Subcommand)]\nenum Command {\n    Done,\n    Wiki {\n        #[arg(long)]\n        fresh: bool,\n    },\n}\n#[derive(Subcommand)]\nenum WikiCommand {\n    Build,\n    Status,\n}\n#[derive(Subcommand)]\nenum MemoryCommand {\n    Status,\n}\n";
        let (_dir, index) = index(&[("src/config.rs", CONFIG), ("src/main.rs", main)]);
        assert_eq!(
            unique(index.lookup("[wiki] budget_usd", &[])),
            ("src/config.rs".into(), 2)
        );
        assert_eq!(
            unique(index.lookup("[tasks] budget_usd", &[])),
            ("src/config.rs".into(), 5)
        );
        assert!(matches!(
            index.lookup("budget_usd", &[]),
            Lookup::Ambiguous(_)
        ));
        assert_eq!(
            unique(index.lookup("--fresh", &[])),
            ("src/main.rs".into(), 6)
        );
        assert_eq!(
            unique(index.lookup("crystal wiki status", &[])),
            ("src/main.rs".into(), 12)
        );
        assert_eq!(
            unique(index.lookup("crystal memory status", &[])),
            ("src/main.rs".into(), 16)
        );
        assert_eq!(
            unique(index.lookup("crystal done", &[])),
            ("src/main.rs".into(), 3)
        );
        assert_eq!(
            unique(index.lookup("crystal wiki build --fresh", &[])),
            ("src/main.rs".into(), 11)
        );
        assert_eq!(
            index.lookup("crystal status", &[]),
            Lookup::Ambiguous(
                index
                    .named("status", |k| k == DefKind::Command)
                    .into_iter()
                    .cloned()
                    .collect()
            )
        );
    }

    #[test]
    fn what_is_at_a_line_and_in_some_files() {
        let (_dir, index) = index(&[("src/session.rs", SESSION), ("src/task.rs", TASK)]);
        let at: Vec<&str> = index
            .defined_at("src/session.rs", 6)
            .iter()
            .map(|def| def.name.as_str())
            .collect();
        assert_eq!(at, ["spawn"]);
        let outline: Vec<&str> = index
            .outline(&["src/session.rs".into()])
            .iter()
            .map(|def| def.name.as_str())
            .collect();
        assert_eq!(outline, ["Session", "name", "spawn", "run"]);
    }
}
