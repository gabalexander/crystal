//! Resolving a code span to what it names. A span is read as each thing it
//! could be, in turn, and the first that resolves wins:
//!
//! - a path: `src/daemon.rs`, `src/tui/`, `daemon.rs`, `src/x.rs:12`;
//! - a command line: `crystal wiki build`, `crystal send --wait`, its first
//!   word one of the repository's programs, each subcommand followed from
//!   the one before (a clap variant's fields' types);
//! - a flag: `--wait`, `-p`;
//! - a config key: `[sessions] stop_idle_after`, `sessions.stop_idle_after`,
//!   `[[profile]]`, each table followed from the field before it by the
//!   field's type;
//! - a name: `Session`, `Session::stop()`, `fn stop`, `engine.Server`,
//!   `(*Server).Serve`, `out!`, matching a definition whose name and what
//!   it's in end the same way.
//!
//! Several definitions it could be are narrowed down, a step at a time: to
//! those of the kind the span says (`()`, `!`, `struct`), to those in the
//! files the prose is about, to those that aren't tests', to definitions
//! over declarations, and to the one those files refer to, as a SCIP
//! indexer says. What's still more than one is left unlinked.

use super::{Def, DefKind, Index, Lookup};
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

/// What a span might be.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Query {
    Path {
        path: String,
        dir: bool,
        lines: Option<(u32, u32)>,
    },
    Command {
        words: Vec<String>,
        flags: Vec<String>,
    },
    Flag(String),
    Key(Vec<String>),
    Name {
        segments: Vec<String>,
        hint: Option<Hint>,
    },
}

/// What a span says of the kind of what it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hint {
    /// `stop()`, `fn stop`.
    Callable,
    /// `out!`.
    Macro,
    /// `struct Session`, `mod tui`.
    Kind(DefKind),
}

impl Hint {
    fn fits(self, kind: DefKind) -> bool {
        match self {
            Hint::Callable => kind.is_callable(),
            Hint::Macro => kind == DefKind::Macro,
            Hint::Kind(DefKind::Class) => kind.is_type(),
            Hint::Kind(DefKind::Type) => kind.is_type(),
            Hint::Kind(DefKind::Function) => matches!(kind, DefKind::Function | DefKind::Method),
            Hint::Kind(DefKind::Const) => {
                matches!(kind, DefKind::Const | DefKind::Static | DefKind::Variable)
            }
            Hint::Kind(want) => kind == want,
        }
    }
}

pub fn lookup(index: &Index, span: &str, near: &[String]) -> Lookup {
    for query in queries(span, &index.programs) {
        let found = match &query {
            Query::Path { path, dir, lines } => self::path(index, path, *dir, *lines, near),
            Query::Command { words, flags } => command(index, words, flags, near),
            Query::Flag(flag) => self::flag(index, flag, None, near),
            Query::Key(keys) => key(index, keys, near),
            Query::Name { segments, hint } => name(index, segments, *hint, near),
        };
        if found != Lookup::Missing {
            return found;
        }
    }
    Lookup::Missing
}

/// What `span` might be, in the order it's tried.
fn queries(span: &str, programs: &HashSet<String>) -> Vec<Query> {
    let mut span = span.trim();
    if let Some(inner) = (span
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"')))
    .or_else(|| {
        span.strip_prefix('\'')
            .and_then(|rest| rest.strip_suffix('\''))
    }) {
        span = inner.trim();
    }
    let span = span
        .strip_prefix("$ ")
        .unwrap_or(span)
        .trim_end_matches([',', ';']);
    if span.is_empty() || span.len() > 200 {
        return Vec::new();
    }
    let mut queries = Vec::new();
    if let Some(key) = table_key(span) {
        queries.push(Query::Key(key));
        return queries;
    }
    if span.contains(char::is_whitespace) {
        let words: Vec<&str> = span.split_whitespace().collect();
        if programs.contains(words[0]) {
            queries.push(command_query(&words[1..]));
        }
        if let Some((key, _)) = span.split_once('=')
            && let Some(keys) = dotted_key(key.trim())
        {
            queries.push(Query::Key(keys));
        }
        queries.extend(name_query(span));
        return queries;
    }
    if span.starts_with('-') {
        let flag = span.split('=').next().unwrap_or(span);
        if FLAG.is_match(flag) {
            queries.push(Query::Flag(flag.to_string()));
        }
        return queries;
    }
    if let Some(path) = path_query(span) {
        queries.push(path);
    }
    queries.extend(name_query(span));
    if let Some(keys) = dotted_key(span) {
        queries.push(Query::Key(keys));
    }
    queries
}

static FLAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^--?[A-Za-z0-9][\w-]*$").expect("a valid regex"));

/// `[section] key`, `[section.table] key = value`, `[[section]]`.
fn table_key(span: &str) -> Option<Vec<String>> {
    static TABLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\[\[?\s*([\w.-]+)\s*\]\]?(?:\s+([\w.-]+)(?:\s*=.*)?)?$")
            .expect("a valid regex")
    });
    let found = TABLE.captures(span)?;
    let mut keys: Vec<String> = found[1].split('.').map(String::from).collect();
    if let Some(key) = found.get(2) {
        keys.extend(key.as_str().split('.').map(String::from));
    }
    Some(keys).filter(|keys| keys.iter().all(|key| !key.is_empty()))
}

/// `sessions.stop_idle_after`: lowercase words joined by dots, as a config
/// file's keys are written.
fn dotted_key(span: &str) -> Option<Vec<String>> {
    static DOTTED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[a-z_][a-z0-9_-]*(?:\.[a-z_][a-z0-9_-]*)*$").expect("a valid regex")
    });
    DOTTED
        .is_match(span)
        .then(|| span.split('.').map(String::from).collect())
}

/// A command line's subcommands and flags, the words after its program.
fn command_query(words: &[&str]) -> Query {
    static WORD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9-]*$").expect("a valid regex"));
    let mut subcommands = Vec::new();
    let mut flags = Vec::new();
    let mut words = words.iter();
    for word in words.by_ref() {
        if WORD.is_match(word) {
            subcommands.push(word.to_string());
        } else {
            if word.starts_with('-') {
                flags.push(word.split('=').next().unwrap_or(word).to_string());
            }
            break;
        }
    }
    flags.extend(
        words
            .filter(|word| word.starts_with('-'))
            .map(|word| word.split('=').next().unwrap_or(word).to_string())
            .filter(|flag| FLAG.is_match(flag)),
    );
    Query::Command {
        words: subcommands,
        flags,
    }
}

/// A path, with a line or two after it: `src/x.rs`, `src/tui/`,
/// `src/x.rs:12`, `src/x.rs:12:5`, `src/x.rs#L12-L20`, `daemon.rs`.
fn path_query(span: &str) -> Option<Query> {
    static LINES: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(.+?)(?::(\d+)(?::\d+|-(\d+))?|#L(\d+)(?:-L(\d+))?|\((\d+),\s*\d+\))$")
            .expect("a valid regex")
    });
    static EXTENSION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\.[A-Za-z][\w+-]{0,9}$").expect("a valid regex"));
    let (path, lines) = match LINES.captures(span) {
        Some(found) => {
            let number = |at: usize| {
                found
                    .get(at)
                    .and_then(|number| number.as_str().parse::<u32>().ok())
            };
            let start = number(2).or(number(4)).or(number(6));
            let end = number(3).or(number(5)).or(start);
            (
                found.get(1).map_or(span, |path| path.as_str()),
                start.zip(end),
            )
        }
        None => (span, None),
    };
    let path = path.strip_prefix("./").unwrap_or(path);
    let looks_like_one = path.contains('/') || EXTENSION.is_match(path) || lines.is_some();
    if !looks_like_one || path.contains("::") || path.contains(['*', '?', '<', '>', '{', '$', '"'])
    {
        return None;
    }
    let dir = path.ends_with('/');
    let path = path.trim_end_matches('/').to_string();
    (!path.is_empty()).then_some(Query::Path { path, dir, lines })
}

/// A name, from a span that names one in the ways code is written about:
/// `Session::stop()`, `pub fn stop`, `&mut Session`, `self.sessions`,
/// `(*Server).Serve`, `out!`, `Vec<Session>` (which names `Vec`).
fn name_query(span: &str) -> Option<Query> {
    static KEYWORD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:pub(?:\([\w\s]+\))?\s+|export\s+|async\s+|unsafe\s+|const\s+(?:fn\s)|default\s+)*(fn|func|def|function|struct|enum|trait|impl|class|interface|type|mod|module|namespace|package|const|static|let|var|macro_rules!)\s+(.+)$")
            .expect("a valid regex")
    });
    static GO_METHOD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\(\s*(?:\w+\s+)?\*?\s*(\w+)(?:\[[^\]]*\])?\s*\)\s*\.?\s*(\w+)")
            .expect("a valid regex")
    });
    static IDENTIFIER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*$").expect("a valid regex"));
    let mut text = span.trim();
    let mut hint = None;
    if let Some(found) = KEYWORD.captures(text) {
        hint = Some(match &found[1] {
            "fn" | "func" | "def" | "function" => Hint::Callable,
            "struct" => Hint::Kind(DefKind::Struct),
            "enum" => Hint::Kind(DefKind::Enum),
            "trait" => Hint::Kind(DefKind::Trait),
            "interface" => Hint::Kind(DefKind::Interface),
            "class" | "impl" | "type" => Hint::Kind(DefKind::Type),
            "mod" | "module" | "namespace" | "package" => Hint::Kind(DefKind::Module),
            "macro_rules!" => Hint::Macro,
            _ => Hint::Kind(DefKind::Const),
        });
        text = found.get(2).map_or("", |rest| rest.as_str()).trim();
        if &found[1] == "impl" && text.contains(" for ") {
            return None;
        }
    }
    // Go: `func (s *Server) Serve(...)` and `(*Server).Serve`.
    let receiver;
    if let Some(found) = GO_METHOD.captures(text) {
        receiver = format!("{}.{}", &found[1], &found[2]);
        text = &receiver;
        hint = hint.or(Some(Hint::Callable));
    }
    // A call's arguments and a type's parameters: `stop(&mut self)`,
    // `Vec<Session>`; a variable's type: `port: u16`.
    if let Some(at) = text.find('(') {
        if text[at..].trim_end().ends_with(')') || !text[at..].contains(')') {
            hint = hint.or(Some(Hint::Callable));
        }
        text = &text[..at];
    }
    if let Some(at) = text
        .find(['<', '[', ':'])
        .filter(|&at| !text[at..].starts_with("::"))
    {
        text = &text[..at];
    }
    let mut text = text.trim().trim_start_matches(['&', '*', '@']).trim();
    text = text.strip_prefix("mut ").unwrap_or(text).trim();
    if let Some(rest) = text.strip_suffix('!') {
        text = rest;
        hint = Some(Hint::Macro);
    }
    let text = text.trim_end_matches('?');
    for prefix in [
        "crate::", "self::", "super::", "Self::", "self.", "this.", "$this->", "::",
    ] {
        if let Some(rest) = text.strip_prefix(prefix)
            && !rest.is_empty()
        {
            return name_query_of(rest, hint, &IDENTIFIER);
        }
    }
    name_query_of(text, hint, &IDENTIFIER)
}

fn name_query_of(text: &str, hint: Option<Hint>, identifier: &Regex) -> Option<Query> {
    let segments: Vec<String> = text
        .split("::")
        .flat_map(|part| part.split("->"))
        .flat_map(|part| part.split(['.', '#', '\\']))
        .map(String::from)
        .collect();
    let named = !segments.is_empty() && segments.iter().all(|segment| identifier.is_match(segment));
    named.then_some(Query::Name { segments, hint })
}

/// The definitions of `ids` by their places.
fn defs(index: &Index, ids: &[u32]) -> Vec<Def> {
    ids.iter()
        .map(|&id| index.defs[id as usize].clone())
        .collect()
}

/// The one of `ids` the span names, narrowed down as the module says, or
/// those it could be.
fn choose(index: &Index, ids: &[u32], near: &[String]) -> Lookup {
    let mut ids = distinct(index, ids);
    if ids.is_empty() {
        return Lookup::Missing;
    }
    let near: HashSet<&str> = near.iter().map(String::as_str).collect();
    narrow(&mut ids, |id| {
        near.contains(index.defs[id as usize].path.as_str())
    });
    narrow(&mut ids, |id| !index.meta[id as usize].test);
    narrow(&mut ids, |id| !index.meta[id as usize].decl);
    if ids.len() > 1 {
        let referred: Vec<u32> = (ids.iter().copied())
            .filter(|id| {
                near.iter()
                    .any(|path| index.refs.get(*path).is_some_and(|refs| refs.contains(id)))
            })
            .collect();
        if referred.len() == 1 {
            ids = referred;
        }
    }
    match ids.as_slice() {
        [id] => Lookup::Unique(index.defs[*id as usize].clone()),
        _ => Lookup::Ambiguous(defs(index, &ids)),
    }
}

/// Keeps those of `ids` that `keep` says, unless that's none of them.
fn narrow(ids: &mut Vec<u32>, keep: impl Fn(u32) -> bool) {
    if ids.len() < 2 {
        return;
    }
    let kept: Vec<u32> = ids.iter().copied().filter(|&id| keep(id)).collect();
    if !kept.is_empty() {
        *ids = kept;
    }
}

/// `ids` with each definition once: two of the same kind and name in the
/// same lines of the same file, as two tiers can find, are one, the precise
/// one kept.
fn distinct(index: &Index, ids: &[u32]) -> Vec<u32> {
    let mut sorted: Vec<u32> = ids.to_vec();
    sorted.sort_by_key(|&id| (!index.defs[id as usize].precise, id));
    sorted.dedup();
    let mut kept: Vec<u32> = Vec::new();
    for id in sorted {
        let def = &index.defs[id as usize];
        let same = kept.iter().any(|&other| {
            let other = &index.defs[other as usize];
            other.path == def.path
                && other.name == def.name
                && other.kind == def.kind
                && other.start <= def.end.max(def.start)
                && def.start <= other.end.max(other.start)
        });
        if !same {
            kept.push(id);
        }
    }
    kept.sort_unstable();
    kept
}

fn path(
    index: &Index,
    path: &str,
    dir: bool,
    lines: Option<(u32, u32)>,
    near: &[String],
) -> Lookup {
    let file = |path: &str| {
        let (start, end) = match (lines, index.lines.get(path)) {
            (Some((start, end)), Some(&count)) if start >= 1 && end >= start && end <= count => {
                (start, end)
            }
            (Some((start, end)), None) if start >= 1 && end >= start => (start, end),
            _ => (0, 0),
        };
        Def {
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            qualified: path.to_string(),
            kind: DefKind::File,
            path: path.to_string(),
            start,
            end,
            precise: true,
        }
    };
    let directory = |path: &str| Def {
        name: path.rsplit('/').next().unwrap_or(path).to_string(),
        qualified: format!("{path}/"),
        kind: DefKind::Directory,
        path: path.to_string(),
        start: 0,
        end: 0,
        precise: true,
    };
    if !dir && index.files.contains(path) {
        return Lookup::Unique(file(path));
    }
    if index.dirs.contains(path) {
        return Lookup::Unique(directory(path));
    }
    // A path's end: `daemon.rs`, `tui/app.rs`, `tui/`.
    let ending = format!("/{path}");
    let mut found: Vec<Def> = Vec::new();
    if !dir {
        found.extend(
            index
                .files
                .iter()
                .filter(|file| file.ends_with(&ending))
                .map(|path| file(path)),
        );
    }
    if found.is_empty() {
        found.extend(
            index
                .dirs
                .iter()
                .filter(|dir| dir.ends_with(&ending))
                .map(|path| directory(path)),
        );
    }
    if found.len() > 1 {
        let in_near: Vec<Def> = found
            .iter()
            .filter(|def| {
                near.iter()
                    .any(|near| *near == def.path || near.starts_with(&format!("{}/", def.path)))
            })
            .cloned()
            .collect();
        if !in_near.is_empty() {
            found = in_near;
        }
    }
    match found.len() {
        0 => Lookup::Missing,
        1 => Lookup::Unique(found.remove(0)),
        _ => Lookup::Ambiguous(found),
    }
}

/// The definitions named `name` of one of `kinds`, by their own names or the
/// others they go by.
fn named(index: &Index, name: &str, kinds: &[DefKind]) -> Vec<u32> {
    let mut ids: Vec<u32> = (index.by_alias.get(name).into_iter().flatten())
        .chain(index.by_name.get(name).into_iter().flatten())
        .copied()
        .filter(|&id| kinds.contains(&index.defs[id as usize].kind))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn name(index: &Index, segments: &[String], hint: Option<Hint>, near: &[String]) -> Lookup {
    let Some(last) = segments.last() else {
        return Lookup::Missing;
    };
    let mut ids: Vec<u32> = (index.by_name.get(last).into_iter().flatten())
        .copied()
        .filter(|&id| index.meta[id as usize].segments.ends_with(segments))
        .collect();
    if let Some(hint) = hint {
        narrow(&mut ids, |id| hint.fits(index.defs[id as usize].kind));
        if ids.len() == 1 && !hint.fits(index.defs[ids[0] as usize].kind) {
            return Lookup::Missing;
        }
    }
    choose(index, &ids, near)
}

/// What `name` names in `ty`: whether `ty`, a type as written, has `name`
/// as a word of it.
fn mentions(ty: &str, name: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    ty.match_indices(name).any(|(at, _)| {
        let before = ty[..at].chars().next_back();
        let after = ty[at + name.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

/// What the definition `id` is in: the struct a field is a field of, the
/// enum a variant is of.
fn container(index: &Index, id: u32) -> Option<&str> {
    let segments = &index.meta[id as usize].segments;
    segments
        .len()
        .checked_sub(2)
        .map(|at| segments[at].as_str())
}

/// The definitions among `parents` that lead to `inner`: whose type
/// mentions it, or mentions a struct with a field whose type does, as a
/// clap variant holding its own `Args` struct does.
fn leading_to(index: &Index, parents: &[u32], inner: &str) -> Vec<u32> {
    parents
        .iter()
        .copied()
        .filter(|&parent| {
            let Some(ty) = index.meta[parent as usize].ty.as_deref() else {
                return false;
            };
            mentions(ty, inner)
                || ty
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .any(|word| {
                        !word.is_empty()
                            && word != inner
                            && (index.by_path.values().next().is_some())
                            && index.by_name.get(word).into_iter().flatten().any(|&held| {
                                index.defs[held as usize].kind.is_type()
                                    && index.meta.iter().enumerate().any(|(field, meta)| {
                                        index.defs[field].kind == DefKind::Field
                                            && meta.segments.len() >= 2
                                            && meta.segments[meta.segments.len() - 2] == word
                                            && meta
                                                .ty
                                                .as_deref()
                                                .is_some_and(|ty| mentions(ty, inner))
                                    })
                            })
                    })
        })
        .collect()
}

/// Narrows `ids` to those each of `parents`, the words before theirs from
/// the last, leads to: a key's table, a subcommand's command.
fn follow(index: &Index, ids: &mut Vec<u32>, parents: &[String], kinds: &[DefKind]) {
    if parents.is_empty() {
        return;
    }
    let held: Vec<u32> = (ids.iter().copied())
        .filter(|&id| {
            let mut inner = id;
            for parent in parents.iter().rev() {
                let Some(name) = container(index, inner) else {
                    return false;
                };
                let candidates = named(index, parent, kinds);
                match leading_to(index, &candidates, name).first() {
                    Some(&next) => inner = next,
                    None => return false,
                }
            }
            true
        })
        .collect();
    if !held.is_empty() {
        *ids = held;
    }
}

fn key(index: &Index, keys: &[String], near: &[String]) -> Lookup {
    let Some((last, parents)) = keys.split_last() else {
        return Lookup::Missing;
    };
    let mut ids = named(index, last, &[DefKind::Field]);
    follow(index, &mut ids, parents, &[DefKind::Field]);
    choose(index, &ids, near)
}

const COMMANDS: &[DefKind] = &[DefKind::Variant, DefKind::Command];

fn command(index: &Index, words: &[String], flags: &[String], near: &[String]) -> Lookup {
    let Some((last, parents)) = words.split_last() else {
        return match flags.first() {
            Some(flag) => self::flag(index, flag, None, near),
            None => Lookup::Missing,
        };
    };
    // A subcommand is a variant only where it goes by its word.
    let mut ids: Vec<u32> = (index.by_alias.get(last.as_str()).into_iter().flatten())
        .copied()
        .filter(|&id| COMMANDS.contains(&index.defs[id as usize].kind))
        .collect();
    follow(index, &mut ids, parents, COMMANDS);
    let found = choose(index, &ids, near);
    if let (Lookup::Unique(def), Some(flag)) = (&found, flags.first()) {
        let at = ids
            .iter()
            .copied()
            .find(|&id| index.defs[id as usize] == *def);
        if let Lookup::Unique(flag) = self::flag(index, flag, at, near) {
            return Lookup::Unique(flag);
        }
    }
    found
}

/// The flag `flag`, of the subcommand `of` when it's known: a field of the
/// variant, or of the struct the variant holds.
fn flag(index: &Index, flag: &str, of: Option<u32>, near: &[String]) -> Lookup {
    let mut ids: Vec<u32> = (index.by_alias.get(flag).into_iter().flatten())
        .copied()
        .filter(|&id| matches!(index.defs[id as usize].kind, DefKind::Field | DefKind::Flag))
        .collect();
    if let Some(of) = of {
        let command = &index.meta[of as usize];
        narrow(&mut ids, |id| {
            let segments = &index.meta[id as usize].segments;
            segments.len() > 1 && segments[..segments.len() - 1] == command.segments[..]
                || container(index, id).is_some_and(|inner| {
                    command.ty.as_deref().is_some_and(|ty| mentions(ty, inner))
                })
        });
    }
    choose(index, &ids, near)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn programs() -> HashSet<String> {
        HashSet::from(["crystal".to_string()])
    }

    fn name(segments: &[&str], hint: Option<Hint>) -> Query {
        Query::Name {
            segments: segments.iter().map(|segment| segment.to_string()).collect(),
            hint,
        }
    }

    #[test]
    fn a_span_is_read_as_each_thing_it_could_name() {
        let read = |span: &str| queries(span, &programs());
        assert_eq!(read("Session::stop"), [name(&["Session", "stop"], None)]);
        assert_eq!(
            read("Session::stop()"),
            [name(&["Session", "stop"], Some(Hint::Callable))]
        );
        assert_eq!(
            read("pub fn stop(&mut self) -> Result<()>"),
            [name(&["stop"], Some(Hint::Callable))]
        );
        assert_eq!(
            read("struct Session"),
            [name(&["Session"], Some(Hint::Kind(DefKind::Struct)))]
        );
        assert_eq!(read("out!"), [name(&["out"], Some(Hint::Macro))]);
        assert_eq!(
            read("crate::session::Session"),
            [name(&["session", "Session"], None)]
        );
        assert_eq!(read("&mut Session"), [name(&["Session"], None)]);
        assert_eq!(read("Vec<Session>"), [name(&["Vec"], None)]);
        assert!(read("self.sessions").contains(&name(&["sessions"], None)));
        assert_eq!(
            read("(*Server).Serve"),
            [name(&["Server", "Serve"], Some(Hint::Callable))]
        );
        assert_eq!(
            read("func (s *Server) Serve(w Writer)"),
            [name(&["Server", "Serve"], Some(Hint::Callable))]
        );
        assert_eq!(read("port: u16"), [name(&["port"], None)]);
        assert_eq!(
            read("src/daemon.rs"),
            [Query::Path {
                path: "src/daemon.rs".into(),
                dir: false,
                lines: None
            }]
        );
        assert_eq!(
            read("src/daemon.rs:12"),
            [Query::Path {
                path: "src/daemon.rs".into(),
                dir: false,
                lines: Some((12, 12))
            }]
        );
        assert_eq!(
            read("src/x.rs#L3-L9")[0],
            Query::Path {
                path: "src/x.rs".into(),
                dir: false,
                lines: Some((3, 9))
            }
        );
        assert_eq!(
            read("src/tui/"),
            [Query::Path {
                path: "src/tui".into(),
                dir: true,
                lines: None
            }]
        );
        assert_eq!(
            read("config.toml"),
            [
                Query::Path {
                    path: "config.toml".into(),
                    dir: false,
                    lines: None
                },
                name(&["config", "toml"], None),
                Query::Key(vec!["config".into(), "toml".into()]),
            ]
        );
        assert_eq!(
            read("[sessions] stop_idle_after"),
            [Query::Key(vec![
                "sessions".into(),
                "stop_idle_after".into()
            ])]
        );
        assert_eq!(read("[[profile]]"), [Query::Key(vec!["profile".into()])]);
        assert_eq!(
            read("[memory] embedder = \"gemini\""),
            [Query::Key(vec!["memory".into(), "embedder".into()])]
        );
        assert_eq!(read("--wait"), [Query::Flag("--wait".into())]);
        assert_eq!(
            read("--test-threads=4"),
            [Query::Flag("--test-threads".into())]
        );
        assert_eq!(
            read("crystal send --wait <name>"),
            [Query::Command {
                words: vec!["send".into()],
                flags: vec!["--wait".into()]
            }]
        );
        assert_eq!(
            read("$ crystal wiki build"),
            [Query::Command {
                words: vec!["wiki".into(), "build".into()],
                flags: vec![]
            }]
        );
        assert!(read("cargo test -- --test-threads=4").is_empty());
        assert!(read("a + b").is_empty());
        assert!(read("").is_empty());
        assert!(read("impl Display for Kind").is_empty());
    }

    #[test]
    fn a_word_is_mentioned_only_whole() {
        assert!(mentions("Option<SessionSettings>", "SessionSettings"));
        assert!(mentions("SessionSettings", "SessionSettings"));
        assert!(!mentions("Option<SessionSettingsX>", "SessionSettings"));
        assert!(!mentions("MySessionSettings", "SessionSettings"));
    }
}
