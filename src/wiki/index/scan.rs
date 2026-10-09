//! A light scan of a file for what it defines, line by line, with no
//! parser: the text with its strings and comments blanked first, so that a
//! brace or a keyword in one is never taken for code, then each language's
//! patterns, the type or class a method is in followed by its braces (or
//! for Python and Ruby, its indentation), and where each definition ends
//! from the same. It's a guess, `precise: false`, good enough to link a
//! name to its line: a name it can't tell apart stays unlinked.

use super::{Def, DefKind};
use regex::Regex;
use std::sync::LazyLock;

/// The languages scanned, by how their text is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Go,
    Python,
    Js,
    /// Java, Kotlin, C#, Scala, Swift, Dart and PHP: classes with their
    /// methods inside, in braces.
    Java,
    C,
    Ruby,
    Shell,
    Toml,
    Yaml,
}

/// The language of the file at `path`, by its name, or `None` for one not
/// scanned.
pub fn lang_of(path: &str) -> Option<Lang> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let extension = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase());
    Some(match extension.as_deref()? {
        "rs" => Lang::Rust,
        "go" => Lang::Go,
        "py" | "pyi" => Lang::Python,
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue" | "svelte" => Lang::Js,
        "java" | "kt" | "kts" | "cs" | "scala" | "swift" | "dart" | "php" => Lang::Java,
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "m" | "mm" => Lang::C,
        "rb" => Lang::Ruby,
        "sh" | "bash" | "zsh" => Lang::Shell,
        "toml" => Lang::Toml,
        "yml" | "yaml" => Lang::Yaml,
        _ => return None,
    })
}

/// What a definition found on a line is, before its end is known.
struct Found {
    name: String,
    kind: DefKind,
    /// The type, class or table it's in.
    container: Option<String>,
    /// Whether what follows in its braces is a container: the type a
    /// method or a field is in.
    opens: bool,
}

/// A container whose braces are open: what's defined directly inside it
/// is its.
struct Open {
    name: String,
    kind: DefKind,
    /// The depth of braces it was opened at.
    depth: i32,
    /// Its clap role: an enum of subcommands, or a struct of arguments.
    commands: bool,
}

/// A definition whose end isn't known yet.
struct Pending {
    def: usize,
    depth: i32,
    /// Whether its braces have opened.
    opened: bool,
}

/// Everything `text`, the file at `path` in `lang`, defines, in order.
pub fn scan(path: &str, lang: Lang, text: &str) -> Vec<Def> {
    let lines: Vec<&str> = text.lines().collect();
    let masked = mask(lang, &lines);
    let module = module_of(path, lang);
    let mut defs = match lang {
        Lang::Python | Lang::Ruby | Lang::Yaml => by_indentation(lang, &lines, &masked),
        Lang::Toml => toml(&lines),
        _ => by_braces(lang, &lines, &masked),
    };
    for def in &mut defs {
        def.path = path.to_string();
        def.qualified = qualify(&module, lang, &def.qualified, &def.name);
    }
    defs
}

/// The module a file is, as code elsewhere names it: `src/wiki/build.rs`
/// is `wiki::build` in Rust, a Go file's package its directory, a Python
/// file's its path with dots.
fn module_of(path: &str, lang: Lang) -> Vec<String> {
    let mut parts: Vec<String> = path.split('/').map(str::to_string).collect();
    let file = parts.pop().unwrap_or_default();
    let stem = file.split('.').next().unwrap_or_default().to_string();
    match lang {
        Lang::Rust => {
            if parts.first().is_some_and(|first| first == "src") {
                parts.remove(0);
            }
            if !matches!(stem.as_str(), "mod" | "lib" | "main") {
                parts.push(stem);
            }
            parts
        }
        Lang::Go => parts.last().cloned().into_iter().collect(),
        Lang::Toml | Lang::Yaml => Vec::new(),
        _ => {
            if stem != "__init__" && stem != "index" {
                parts.push(stem);
            }
            parts
        }
    }
}

/// The qualified name: the file's module, the container, the name, joined
/// as the language joins them.
fn qualify(module: &[String], lang: Lang, container: &str, name: &str) -> String {
    let sep = match lang {
        Lang::Rust | Lang::C => "::",
        _ => ".",
    };
    let mut parts: Vec<&str> = module.iter().map(String::as_str).collect();
    if !container.is_empty() {
        parts.extend(container.split(sep));
    }
    parts.push(name);
    parts.join(sep)
}

/// The definition found on the masked line `line`, in `lang`, inside
/// `inside` when it's directly in a container's braces, at the top of the
/// file when `at_top`.
fn found(lang: Lang, line: &str, inside: Option<&Open>, at_top: bool) -> Option<Found> {
    let kind_in = |kind: DefKind| {
        inside
            .map(|open| open.kind)
            .filter(|_| kind == DefKind::Function)
    };
    let container = inside.map(|open| open.name.clone());
    let def = |name: &str, kind: DefKind, opens: bool| Found {
        name: name.to_string(),
        kind,
        container: container.clone(),
        opens,
    };
    match lang {
        Lang::Rust => {
            if let Some(c) = RUST_FN.captures(line) {
                let kind = match kind_in(DefKind::Function) {
                    Some(DefKind::Type | DefKind::Trait) => DefKind::Method,
                    _ => DefKind::Function,
                };
                return Some(def(&c[1], kind, false));
            }
            if let Some(c) = RUST_ITEM.captures(line) {
                let (kind, opens) = match &c[1] {
                    "struct" | "union" => (DefKind::Struct, true),
                    "enum" => (DefKind::Enum, true),
                    "trait" => (DefKind::Trait, true),
                    "type" => (DefKind::Type, false),
                    "mod" => (DefKind::Module, false),
                    "static" => (DefKind::Static, false),
                    _ => (DefKind::Const, false),
                };
                return Some(def(&c[2], kind, opens));
            }
            if let Some(c) = RUST_MACRO.captures(line) {
                return Some(def(&c[1], DefKind::Macro, false));
            }
            match inside.map(|open| open.kind) {
                Some(DefKind::Struct | DefKind::Variant) => RUST_FIELD
                    .captures(line)
                    .map(|c| def(&c[1], DefKind::Field, false)),
                Some(DefKind::Enum) => RUST_VARIANT
                    .captures(line)
                    .map(|c| def(&c[1], DefKind::Variant, false)),
                _ => None,
            }
        }
        Lang::Go => {
            if let Some(c) = GO_METHOD.captures(line) {
                return Some(Found {
                    name: c[2].to_string(),
                    kind: DefKind::Method,
                    container: Some(c[1].to_string()),
                    opens: false,
                });
            }
            if let Some(c) = GO_FUNC.captures(line) {
                return Some(def(&c[1], DefKind::Function, false));
            }
            if let Some(c) = GO_TYPE.captures(line) {
                let kind = match c.get(2).map(|m| m.as_str()) {
                    Some("struct") => DefKind::Struct,
                    Some("interface") => DefKind::Interface,
                    _ => DefKind::Type,
                };
                return Some(def(&c[1], kind, kind != DefKind::Type));
            }
            if let Some(c) = GO_VALUE.captures(line) {
                let kind = if &c[1] == "const" {
                    DefKind::Const
                } else {
                    DefKind::Variable
                };
                return Some(def(&c[2], kind, false));
            }
            match inside.map(|open| open.kind) {
                Some(DefKind::Struct) => GO_FIELD
                    .captures(line)
                    .map(|c| def(&c[1], DefKind::Field, false)),
                Some(DefKind::Interface) => GO_INTERFACE_METHOD
                    .captures(line)
                    .map(|c| def(&c[1], DefKind::Method, false)),
                _ => None,
            }
        }
        Lang::Js => {
            if let Some(c) = JS_FUNCTION.captures(line) {
                return Some(def(&c[1], DefKind::Function, false));
            }
            if let Some(c) = JS_CLASS.captures(line) {
                return Some(def(&c[1], DefKind::Class, true));
            }
            if let Some(c) = JS_TYPE.captures(line) {
                let kind = match &c[1] {
                    "interface" => DefKind::Interface,
                    "enum" => DefKind::Enum,
                    "namespace" => DefKind::Module,
                    _ => DefKind::Type,
                };
                return Some(def(&c[2], kind, false));
            }
            if at_top && let Some(c) = JS_VALUE.captures(line) {
                let kind = if &c[1] == "const" {
                    DefKind::Const
                } else {
                    DefKind::Variable
                };
                return Some(def(&c[2], kind, false));
            }
            if inside.is_some_and(|open| open.kind == DefKind::Class) {
                if let Some(c) = JS_METHOD.captures(line)
                    && !KEYWORDS.contains(&&c[1])
                {
                    return Some(def(&c[1], DefKind::Method, false));
                }
                if let Some(c) = JS_FIELD.captures(line)
                    && !KEYWORDS.contains(&&c[1])
                {
                    return Some(def(&c[1], DefKind::Field, false));
                }
            }
            None
        }
        Lang::Java => {
            if let Some(c) = JAVA_TYPE.captures(line) {
                let kind = match &c[1] {
                    "interface" | "protocol" | "trait" => DefKind::Interface,
                    "enum" => DefKind::Enum,
                    "struct" => DefKind::Struct,
                    "object" => DefKind::Class,
                    "extension" => DefKind::Type,
                    _ => DefKind::Class,
                };
                return Some(def(&c[2], kind, true));
            }
            if let Some(c) = JAVA_FUN.captures(line) {
                let kind = if inside.is_some() {
                    DefKind::Method
                } else {
                    DefKind::Function
                };
                return Some(def(&c[1], kind, false));
            }
            if inside.is_some_and(|open| open.kind.is_type()) {
                if let Some(c) = JAVA_METHOD.captures(line)
                    && !KEYWORDS.contains(&&c[1])
                {
                    return Some(def(&c[1], DefKind::Method, false));
                }
                if let Some(c) = JAVA_CONST.captures(line) {
                    return Some(def(&c[1], DefKind::Const, false));
                }
            }
            None
        }
        Lang::C => {
            if let Some(c) = C_DEFINE.captures(line) {
                return Some(def(&c[1], DefKind::Macro, false));
            }
            if let Some(c) = C_TYPE.captures(line) {
                let kind = match &c[1] {
                    "class" => DefKind::Class,
                    "enum" => DefKind::Enum,
                    _ => DefKind::Struct,
                };
                return Some(def(&c[2], kind, true));
            }
            if let Some(c) = C_TYPEDEF.captures(line) {
                return Some(def(&c[1], DefKind::Type, false));
            }
            let in_type = inside.is_some_and(|open| open.kind.is_type());
            if (at_top || in_type || inside.is_some_and(|open| open.kind == DefKind::Module))
                && !line.contains(';')
                && let Some(c) = C_FUNCTION.captures(line)
                && !KEYWORDS.contains(&&c[2])
            {
                let qualifier = c.get(1).map(|m| m.as_str().trim_end_matches("::"));
                return Some(Found {
                    name: c[2].to_string(),
                    kind: if qualifier.is_some() || in_type {
                        DefKind::Method
                    } else {
                        DefKind::Function
                    },
                    container: qualifier.map(str::to_string).or(container),
                    opens: false,
                });
            }
            None
        }
        Lang::Shell => {
            if let Some(c) = SHELL_FUNCTION.captures(line) {
                let name = c.get(1).or(c.get(2)).map_or("", |m| m.as_str());
                return Some(def(name, DefKind::Function, false));
            }
            if at_top && let Some(c) = SHELL_VARIABLE.captures(line) {
                return Some(def(&c[1], DefKind::Variable, false));
            }
            None
        }
        Lang::Python | Lang::Ruby | Lang::Toml | Lang::Yaml => None,
    }
}

/// The definitions of a language of braces, each ending where its braces
/// close, or at the `;` that ends it without any.
fn by_braces(lang: Lang, raw: &[&str], masked: &[String]) -> Vec<Def> {
    let mut defs: Vec<Def> = Vec::new();
    let mut open: Vec<Open> = Vec::new();
    let mut pending: Vec<Pending> = Vec::new();
    // A Rust `impl` or a Swift `extension` waiting for its braces, and
    // whether the attributes since the last item derive clap's.
    let mut container_next: Option<(String, DefKind)> = None;
    let mut attributes = String::new();
    // Go's `const (`, `var (` and `type (` blocks.
    let mut go_block: Option<DefKind> = None;
    let mut depth: i32 = 0;
    for (index, line) in masked.iter().enumerate() {
        let number = index as u32 + 1;
        let trimmed = line.trim();
        if lang == Lang::Rust && trimmed.starts_with("#[") {
            attributes.push_str(raw[index]);
            attributes.push('\n');
        }
        let inside = open.last().filter(|o| o.depth == depth - 1);
        let mut found_here = None;
        if lang == Lang::Go
            && let Some(kind) = go_block
        {
            if trimmed.starts_with(')') {
                go_block = None;
            } else if depth == 0
                && let Some(c) = GO_BLOCK_ITEM.captures(line)
            {
                found_here = Some(Found {
                    name: c[1].to_string(),
                    kind,
                    container: None,
                    opens: false,
                });
            }
        } else if lang == Lang::Go
            && let Some(c) = GO_BLOCK.captures(line)
        {
            go_block = Some(match &c[1] {
                "const" => DefKind::Const,
                "var" => DefKind::Variable,
                _ => DefKind::Type,
            });
        } else if lang == Lang::Rust
            && let Some(c) = RUST_IMPL.captures(line)
        {
            container_next = Some((impl_type(&c[1]), DefKind::Type));
        } else if lang == Lang::Java
            && let Some(c) = SWIFT_EXTENSION.captures(line)
        {
            container_next = Some((c[1].to_string(), DefKind::Type));
        } else if lang == Lang::C
            && let Some(c) = C_NAMESPACE.captures(line)
        {
            container_next = Some((c[1].to_string(), DefKind::Module));
        } else if !trimmed.starts_with("#[") {
            found_here = found(lang, line, inside, depth == 0);
        }
        if let Some(found) = found_here {
            // What was waiting for braces that never came ends where it
            // was: a Go `type ID int` before the next `func`.
            pending.retain(|p| {
                if !p.opened && p.depth == depth {
                    defs[p.def].end = defs[p.def].start;
                    false
                } else {
                    true
                }
            });
            let commands = attributes.contains("Subcommand");
            if lang == Lang::Rust {
                flags_of(&attributes, &found, inside, number, &mut defs);
            }
            // A container's braces are on its line, or soon after; a
            // variant's only on its own line, or it has none.
            let opens = found.opens || (found.kind == DefKind::Variant && line.contains('{'));
            if opens {
                let name = if commands {
                    format!("{}\u{1}", found.name)
                } else {
                    found.name.clone()
                };
                container_next = Some((name, found.kind));
            }
            let container = found.container.clone().unwrap_or_default();
            defs.push(Def {
                name: found.name,
                qualified: container,
                kind: found.kind,
                path: String::new(),
                start: number,
                end: number,
                precise: false,
            });
            pending.push(Pending {
                def: defs.len() - 1,
                depth,
                opened: false,
            });
        }
        if !trimmed.is_empty() && !trimmed.starts_with("#[") && !trimmed.starts_with("///") {
            attributes.clear();
        }
        for c in line.chars() {
            match c {
                '{' => {
                    if let Some((name, kind)) = container_next.take() {
                        let (name, commands) = match name.strip_suffix('\u{1}') {
                            Some(name) => (name.to_string(), true),
                            None => (name, false),
                        };
                        open.push(Open {
                            name,
                            kind,
                            depth,
                            commands,
                        });
                    }
                    for p in pending.iter_mut().filter(|p| p.depth == depth) {
                        p.opened = true;
                    }
                    depth += 1;
                }
                '}' => {
                    depth -= 1;
                    while open.last().is_some_and(|o| o.depth >= depth) {
                        open.pop();
                    }
                    pending.retain(|p| {
                        if p.opened && p.depth >= depth {
                            defs[p.def].end = number;
                            false
                        } else {
                            true
                        }
                    });
                }
                ';' => {
                    // `struct Unit;` has no braces to be a container in.
                    container_next = None;
                    pending.retain(|p| {
                        if !p.opened && p.depth == depth {
                            defs[p.def].end = number;
                            false
                        } else {
                            true
                        }
                    });
                }
                _ => {}
            }
        }
        // Without braces or a `;` by now, a field, a variant or a Go type
        // ends on its own line; anything else waits a while for its
        // braces, then is taken to end where it started.
        pending.retain(|p| {
            if p.opened {
                return true;
            }
            let def = &defs[p.def];
            let alone = matches!(def.kind, DefKind::Field | DefKind::Variant)
                || (lang == Lang::Go && !line.trim_end().ends_with(['(', ',']));
            let ends_here = def.start == number && alone && !line.contains('{');
            !ends_here && number - def.start < 40
        });
    }
    defs
}

/// The flags clap makes of a field, and the command it makes of a variant
/// of an enum of subcommands, from the attributes before it.
fn flags_of(
    attributes: &str,
    found: &Found,
    inside: Option<&Open>,
    line: u32,
    defs: &mut Vec<Def>,
) {
    let container = found.container.clone().unwrap_or_default();
    let push = |defs: &mut Vec<Def>, name: String, kind: DefKind| {
        defs.push(Def {
            name,
            qualified: container.clone(),
            kind,
            path: String::new(),
            start: line,
            end: line,
            precise: false,
        });
    };
    match found.kind {
        DefKind::Field if attributes.contains("#[arg(") || attributes.contains("#[clap(") => {
            if let Some(c) = CLAP_LONG_NAMED.captures(attributes) {
                push(defs, format!("--{}", &c[1]), DefKind::Flag);
            } else if CLAP_LONG.is_match(attributes) {
                push(defs, format!("--{}", kebab(&found.name)), DefKind::Flag);
            }
            if let Some(c) = CLAP_SHORT_NAMED.captures(attributes) {
                push(defs, format!("-{}", &c[1]), DefKind::Flag);
            } else if CLAP_SHORT.is_match(attributes) {
                let first = found.name.chars().next().unwrap_or('x');
                push(defs, format!("-{first}"), DefKind::Flag);
            }
        }
        DefKind::Variant if inside.is_some_and(|open| open.commands) => {
            let name = match COMMAND_NAMED.captures(attributes) {
                Some(c) => c[1].to_string(),
                None => kebab(&found.name),
            };
            push(defs, name, DefKind::Command);
            for c in COMMAND_ALIAS.captures_iter(attributes) {
                push(defs, c[1].to_string(), DefKind::Command);
            }
        }
        _ => {}
    }
}

/// `MaxBudgetUsd` or `max_budget_usd` as clap names it: `max-budget-usd`.
fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.trim_start_matches("r#").chars().enumerate() {
        if c == '_' {
            out.push('-');
        } else if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The type an `impl` is for: `impl<T> Trait for Type<T> where …` is for
/// `Type`.
fn impl_type(rest: &str) -> String {
    let mut rest = rest.trim();
    if rest.starts_with('<') {
        let mut depth = 0;
        for (i, c) in rest.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        rest = &rest[i + 1..];
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    let rest = match rest.find(" for ") {
        Some(at) => &rest[at + 5..],
        None => rest,
    };
    let path = rest
        .trim()
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .next()
        .unwrap_or_default();
    path.rsplit("::").next().unwrap_or_default().to_string()
}

/// The definitions of Python, Ruby and YAML, each ending at the last line
/// indented deeper than it.
fn by_indentation(lang: Lang, raw: &[&str], masked: &[String]) -> Vec<Def> {
    let indent = |line: &str| line.len() - line.trim_start().len();
    let mut defs: Vec<Def> = Vec::new();
    // The open definitions, with their indentation, innermost last.
    let mut open: Vec<(usize, usize)> = Vec::new();
    for (index, line) in masked.iter().enumerate() {
        let number = index as u32 + 1;
        if line.trim().is_empty() {
            continue;
        }
        let here = indent(line);
        let ruby_end = lang == Lang::Ruby && line.trim() == "end";
        while let Some(&(def, at)) = open.last() {
            if here > at || (ruby_end && here == at && defs[def].end < number) {
                break;
            }
            open.pop();
        }
        if ruby_end {
            if let Some(&(def, at)) = open.last()
                && at == here
            {
                defs[def].end = number;
                open.pop();
            }
            continue;
        }
        for &(def, _) in &open {
            defs[def].end = number;
        }
        let container = open
            .iter()
            .rev()
            .map(|&(def, _)| &defs[def])
            .find(|def| def.kind.is_container() || lang == Lang::Yaml)
            .map(Def::qualified_as_container);
        let found = match lang {
            Lang::Python => python(line, raw[index], container.is_some(), here == 0),
            Lang::Ruby => ruby(line, container.is_some()),
            _ => YAML_KEY
                .captures(raw[index])
                .map(|c| (c[1].to_string(), DefKind::ConfigKey)),
        };
        if let Some((name, kind)) = found {
            defs.push(Def {
                name,
                qualified: container.unwrap_or_default(),
                kind,
                path: String::new(),
                start: number,
                end: number,
                precise: false,
            });
            if !matches!(kind, DefKind::Flag | DefKind::Command | DefKind::Const) {
                open.push((defs.len() - 1, here));
            }
        }
    }
    defs
}

fn python(line: &str, raw: &str, in_class: bool, top: bool) -> Option<(String, DefKind)> {
    if let Some(c) = PY_DEF.captures(line) {
        let kind = if in_class {
            DefKind::Method
        } else {
            DefKind::Function
        };
        return Some((c[1].to_string(), kind));
    }
    if let Some(c) = PY_CLASS.captures(line) {
        return Some((c[1].to_string(), DefKind::Class));
    }
    if top && let Some(c) = PY_CONST.captures(line) {
        return Some((c[1].to_string(), DefKind::Const));
    }
    if let Some(c) = PY_FLAG.captures(raw) {
        return Some((c[1].to_string(), DefKind::Flag));
    }
    PY_COMMAND
        .captures(raw)
        .map(|c| (c[1].to_string(), DefKind::Command))
}

fn ruby(line: &str, in_class: bool) -> Option<(String, DefKind)> {
    if let Some(c) = RUBY_DEF.captures(line) {
        let kind = if in_class {
            DefKind::Method
        } else {
            DefKind::Function
        };
        return Some((c[1].to_string(), kind));
    }
    RUBY_CLASS.captures(line).map(|c| {
        let kind = if &c[1] == "module" {
            DefKind::Module
        } else {
            DefKind::Class
        };
        (
            c[2].rsplit("::").next().unwrap_or_default().to_string(),
            kind,
        )
    })
}

/// A TOML file's keys, each in its table.
fn toml(raw: &[&str]) -> Vec<Def> {
    let mut defs = Vec::new();
    let mut table = String::new();
    for (index, line) in raw.iter().enumerate() {
        let number = index as u32 + 1;
        if let Some(c) = TOML_TABLE.captures(line) {
            table = c[1].trim().to_string();
            continue;
        }
        if let Some(c) = TOML_KEY.captures(line) {
            defs.push(Def {
                name: c[1].trim_matches('"').to_string(),
                qualified: table.clone(),
                kind: DefKind::ConfigKey,
                path: String::new(),
                start: number,
                end: number,
                precise: false,
            });
        }
    }
    defs
}

impl Def {
    /// What a definition inside this one has as its container, while its
    /// `qualified` is still its own container.
    fn qualified_as_container(&self) -> String {
        if self.qualified.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.qualified, self.name)
        }
    }
}

/// `lines` with their strings' contents and comments blanked, as `lang`
/// writes them, a string or a comment going on across lines where it does.
pub fn mask(lang: Lang, lines: &[&str]) -> Vec<String> {
    #[derive(Clone, PartialEq)]
    enum Mode {
        Code,
        Block(u32),
        Quote { close: String, escapes: bool },
    }
    let line_comments: &[&str] = match lang {
        Lang::Python | Lang::Ruby | Lang::Shell | Lang::Toml | Lang::Yaml => &["#"],
        _ => &["//"],
    };
    let blocks = !matches!(
        lang,
        Lang::Python | Lang::Ruby | Lang::Shell | Lang::Toml | Lang::Yaml
    );
    let mut mode = Mode::Code;
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let chars: Vec<char> = line.chars().collect();
        let mut masked = String::with_capacity(line.len());
        let mut i = 0;
        let starts = |i: usize, s: &str| {
            s.chars()
                .enumerate()
                .all(|(k, c)| chars.get(i + k) == Some(&c))
        };
        while i < chars.len() {
            match mode.clone() {
                Mode::Block(depth) => {
                    if starts(i, "*/") {
                        masked.push_str("  ");
                        i += 2;
                        mode = if depth > 1 {
                            Mode::Block(depth - 1)
                        } else {
                            Mode::Code
                        };
                    } else if lang == Lang::Rust && starts(i, "/*") {
                        masked.push_str("  ");
                        i += 2;
                        mode = Mode::Block(depth + 1);
                    } else {
                        masked.push(' ');
                        i += 1;
                    }
                }
                Mode::Quote { close, escapes } => {
                    if escapes && chars[i] == '\\' {
                        masked.push_str("  ");
                        i += 2;
                    } else if starts(i, &close) {
                        masked.push_str(&close);
                        i += close.chars().count();
                        mode = Mode::Code;
                    } else {
                        masked.push(' ');
                        i += 1;
                    }
                }
                Mode::Code => {
                    let c = chars[i];
                    let before = if i == 0 { ' ' } else { chars[i - 1] };
                    // A `#` starts a comment at the start of a word: `$#`
                    // and `${#x}` aren't one.
                    if let Some(comment) = line_comments.iter().find(|m| starts(i, m))
                        && (*comment != "#" || i == 0 || before.is_whitespace())
                    {
                        break;
                    }
                    if blocks && starts(i, "/*") {
                        masked.push_str("  ");
                        i += 2;
                        mode = Mode::Block(1);
                        continue;
                    }
                    let ident_before = before.is_alphanumeric() || before == '_';
                    // Rust's raw strings: r"…", r#"…"#, br"…".
                    if lang == Lang::Rust && c == 'r' && !ident_before {
                        let mut j = i + 1;
                        while chars.get(j) == Some(&'#') {
                            j += 1;
                        }
                        if chars.get(j) == Some(&'"') {
                            let hashes = j - i - 1;
                            masked.push_str(&chars[i..=j].iter().collect::<String>());
                            i = j + 1;
                            mode = Mode::Quote {
                                close: format!("\"{}", "#".repeat(hashes)),
                                escapes: false,
                            };
                            continue;
                        }
                    }
                    if lang == Lang::Python && (starts(i, "\"\"\"") || starts(i, "'''")) {
                        let close: String = chars[i..i + 3].iter().collect();
                        masked.push_str(&close);
                        i += 3;
                        mode = Mode::Quote {
                            close,
                            escapes: true,
                        };
                        continue;
                    }
                    let quote = match c {
                        '"' => Some(true),
                        '`' if matches!(lang, Lang::Js | Lang::Go | Lang::Shell) => {
                            Some(lang == Lang::Js)
                        }
                        '\'' => match lang {
                            Lang::Rust | Lang::C | Lang::Java | Lang::Go => {
                                // A character, not a lifetime: `'x'`, `'\n'`.
                                let close = if chars.get(i + 1) == Some(&'\\') {
                                    chars[i + 2..]
                                        .iter()
                                        .position(|&c| c == '\'')
                                        .map(|at| i + 2 + at)
                                        .filter(|&at| at <= i + 10)
                                } else {
                                    Some(i + 2).filter(|&at| chars.get(at) == Some(&'\''))
                                };
                                if let Some(close) = close {
                                    masked.push('\'');
                                    masked.push_str(&" ".repeat(close - i - 1));
                                    masked.push('\'');
                                    i = close + 1;
                                    continue;
                                }
                                None
                            }
                            Lang::Shell | Lang::Toml => Some(false),
                            _ => Some(true),
                        },
                        _ => None,
                    };
                    match quote {
                        // A word's apostrophe in a comment-less language
                        // isn't a quote: `don't` in shell is, but rarely.
                        Some(escapes) => {
                            masked.push(c);
                            i += 1;
                            mode = Mode::Quote {
                                close: c.to_string(),
                                escapes,
                            };
                        }
                        None => {
                            masked.push(c);
                            i += 1;
                        }
                    }
                }
            }
        }
        // A one-line string that never closed doesn't run on, but in the
        // languages whose strings may.
        if let Mode::Quote { close, .. } = &mode {
            let runs_on = match lang {
                Lang::Rust | Lang::Shell => true,
                Lang::Python => close.len() == 3,
                Lang::Js | Lang::Go => close == "`",
                _ => false,
            };
            if !runs_on {
                mode = Mode::Code;
            }
        }
        out.push(masked);
    }
    out
}

/// Words that open a statement, never a definition's name.
const KEYWORDS: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "catch",
    "return",
    "new",
    "else",
    "do",
    "case",
    "throw",
    "sizeof",
    "typeof",
    "function",
    "await",
    "yield",
    "delete",
    "try",
    "finally",
    "with",
    "in",
    "of",
    "when",
    "match",
    "constructor",
    "super",
    "this",
    "static",
    "defined",
];

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a pattern of crystal's own")
}

const RUST_VIS: &str = r"(?:pub(?:\s*\([^)]*\))?\s+)?";

static RUST_FN: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r#"^\s*{RUST_VIS}(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+(?:"[^"]*"\s*)?)?fn\s+(?:r#)?([A-Za-z_]\w*)"#
    ))
});
static RUST_ITEM: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^\s*{RUST_VIS}(?:unsafe\s+)?(?:auto\s+)?(struct|enum|union|trait|type|mod|const|static)\s+(?:mut\s+)?([A-Za-z_]\w*)"
    ))
});
static RUST_MACRO: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*(?:#\[macro_export\]\s*)?macro_rules!\s*([A-Za-z_]\w*)"));
static RUST_IMPL: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*(?:unsafe\s+)?impl\b\s*(.*)"));
static RUST_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^\s*{RUST_VIS}(?:r#)?([a-z_][A-Za-z0-9_]*)\s*:[^:]"
    ))
});
static RUST_VARIANT: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*([A-Z][A-Za-z0-9_]*)\s*(?:[,({=]|$)"));
static CLAP_LONG: LazyLock<Regex> = LazyLock::new(|| re(r"\blong\b"));
static CLAP_LONG_NAMED: LazyLock<Regex> = LazyLock::new(|| re(r#"\blong\s*=\s*"([\w-]+)""#));
static CLAP_SHORT: LazyLock<Regex> = LazyLock::new(|| re(r"\bshort\b"));
static CLAP_SHORT_NAMED: LazyLock<Regex> = LazyLock::new(|| re(r"\bshort\s*=\s*'(\w)'"));
static COMMAND_NAMED: LazyLock<Regex> =
    LazyLock::new(|| re(r#"#\[command\([^\]]*\bname\s*=\s*"([\w-]+)""#));
static COMMAND_ALIAS: LazyLock<Regex> =
    LazyLock::new(|| re(r#"\b(?:visible_)?alias\s*=\s*"([\w-]+)""#));

static GO_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^func\s*\(\s*\w*\s*\*?\s*([A-Za-z_]\w*)(?:\[[^\]]*\])?\s*\)\s*([A-Za-z_]\w*)")
});
static GO_FUNC: LazyLock<Regex> = LazyLock::new(|| re(r"^func\s+([A-Za-z_]\w*)"));
static GO_TYPE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^type\s+([A-Za-z_]\w*)(?:\[[^\]]*\])?\s+(struct|interface)?"));
static GO_VALUE: LazyLock<Regex> = LazyLock::new(|| re(r"^(const|var)\s+([A-Za-z_]\w*)"));
static GO_BLOCK: LazyLock<Regex> = LazyLock::new(|| re(r"^(const|var|type)\s*\(\s*$"));
static GO_BLOCK_ITEM: LazyLock<Regex> = LazyLock::new(|| re(r"^\s+([A-Za-z_]\w*)\b"));
static GO_FIELD: LazyLock<Regex> = LazyLock::new(|| re(r"^\s+([A-Za-z_]\w*)\s+[^\s=(]"));
static GO_INTERFACE_METHOD: LazyLock<Regex> = LazyLock::new(|| re(r"^\s+([A-Za-z_]\w*)\s*\("));

static JS_FUNCTION: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)")
});
static JS_CLASS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^\s*(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)")
});
static JS_TYPE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^\s*(?:export\s+)?(?:declare\s+)?(?:const\s+)?(interface|enum|namespace|type)\s+([A-Za-z_$][\w$]*)",
    )
});
static JS_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^\s*(?:export\s+)?(?:declare\s+)?(const|let|var)\s+([A-Za-z_$][\w$]*)\s*[:=]")
});
static JS_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^\s*(?:(?:public|private|protected|static|readonly|async|override|abstract|get|set)\s+)*\*?(#?[A-Za-z_$][\w$]*)\s*(?:<[^>]*>)?\s*\([^;]*$",
    )
});
static JS_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^\s*(?:(?:public|private|protected|static|readonly|declare)\s+)*(#?[A-Za-z_$][\w$]*)\s*[?!]?\s*[:=]",
    )
});

const JAVA_MODIFIERS: &str = r"(?:(?:public|private|protected|internal|static|final|abstract|sealed|open|data|inline|value|annotation|enum|partial|fileprivate|override|suspend|mutating|virtual|async|synchronized|native|default|extern|unsafe|new|readonly|lazy|weak|@\w+(?:\([^)]*\))?)\s+)*";

static JAVA_TYPE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^\s*{JAVA_MODIFIERS}(class|interface|enum|record|struct|protocol|object|trait)\s+([A-Za-z_]\w*)"
    ))
});
static SWIFT_EXTENSION: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*(?:public\s+|private\s+)?extension\s+([A-Za-z_]\w*)"));
static JAVA_FUN: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^\s*{JAVA_MODIFIERS}(?:fun|func|def|function)\s+(?:<[^>]*>\s*)?(?:[A-Za-z_]\w*\.)?([A-Za-z_]\w*)"
    ))
});
static JAVA_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^\s*{JAVA_MODIFIERS}(?:<[^>]+>\s+)?(?:[\w\[\],.?]+(?:<[^()]*>)?\s+)?([A-Za-z_]\w*)\s*\([^;]*$"
    ))
});
static JAVA_CONST: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^\s*(?:(?:public|private|protected|internal)\s+)?(?:static\s+final|const|static\s+readonly)\s+[\w<>\[\],.?]+\s+([A-Z_][A-Z0-9_]*)\s*=",
    )
});

static C_DEFINE: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*#\s*define\s+([A-Za-z_]\w*)"));
static C_TYPE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^\s*(?:typedef\s+)?(?:template\s*<[^>]*>\s*)?(struct|class|enum|union)\s+(?:class\s+)?([A-Za-z_]\w*)\s*(?:final\s*)?(?:[:{]|$)",
    )
});
static C_TYPEDEF: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*typedef\b[^;{]*?\b([A-Za-z_]\w*)\s*;"));
static C_NAMESPACE: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*namespace\s+([A-Za-z_][\w:]*)"));
static C_FUNCTION: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^\s*(?:(?:static|inline|extern|virtual|constexpr|explicit|const|unsigned|signed|struct|enum)\s+)*[A-Za-z_][\w:<>,]*[\s\*&]+\**\s*((?:[A-Za-z_]\w*::)+)?(~?[A-Za-z_]\w*)\s*\(",
    )
});

static SHELL_FUNCTION: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*(?:function\s+([A-Za-z_][\w-]*)|([A-Za-z_][\w-]*)\s*\(\s*\))"));
static SHELL_VARIABLE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:export\s+|readonly\s+|declare\s+(?:-\w+\s+)?)?([A-Z_][A-Z0-9_]*)="));

static PY_DEF: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*(?:async\s+)?def\s+([A-Za-z_]\w*)"));
static PY_CLASS: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*class\s+([A-Za-z_]\w*)"));
static PY_CONST: LazyLock<Regex> = LazyLock::new(|| re(r"^([A-Z][A-Z0-9_]*)\s*(?::[^=]*)?=[^=]"));
static PY_FLAG: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"(?:add_argument|click\.option)\(\s*(?:['"]-\w['"]\s*,\s*)?['"](--[\w-]+)['"]"#)
});
static PY_COMMAND: LazyLock<Regex> = LazyLock::new(|| re(r#"add_parser\(\s*['"]([\w-]+)['"]"#));

static RUBY_DEF: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*def\s+(?:self\.)?([A-Za-z_]\w*[?!=]?)"));
static RUBY_CLASS: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*(class|module)\s+([A-Z]\w*(?:::[A-Z]\w*)*)"));

static TOML_TABLE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*\[\[?\s*([^\]]+?)\s*\]\]?\s*(?:#.*)?$"));
static TOML_KEY: LazyLock<Regex> = LazyLock::new(|| re(r#"^\s*([A-Za-z0-9_-]+|"[^"]+")\s*="#));
static YAML_KEY: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*(?:-\s+)?([A-Za-z_][\w-]*)\s*:(?:\s|$)"));

#[cfg(test)]
mod tests {
    use super::*;

    fn defs(path: &str, text: &str) -> Vec<(String, DefKind, String, u32, u32)> {
        let lang = lang_of(path).unwrap();
        scan(path, lang, text)
            .into_iter()
            .map(|def| (def.name, def.kind, def.qualified, def.start, def.end))
            .collect()
    }

    fn span(def: &(String, DefKind, String, u32, u32)) -> (u32, u32) {
        (def.3, def.4)
    }

    fn find<'a>(
        defs: &'a [(String, DefKind, String, u32, u32)],
        name: &str,
    ) -> &'a (String, DefKind, String, u32, u32) {
        defs.iter()
            .find(|def| def.0 == name)
            .unwrap_or_else(|| panic!("no {name} in {defs:?}"))
    }

    #[test]
    fn strings_and_comments_are_blanked_across_lines() {
        let lines = [
            r##"let s = "a { b"; // c {"##,
            r##"let r = r#"x { "##,
            r##"y }"#; let c = '{';"##,
            "fn a<'a>(x: &'a str) {}",
            "/* {",
            "} */ x",
        ];
        let masked = mask(Lang::Rust, &lines);
        assert!(
            masked
                .iter()
                .all(|line| !line.contains('{') || line.contains("fn a")),
            "{masked:?}"
        );
        assert!(masked[3].contains("fn a<'a>(x: &'a str) {}"), "{masked:?}");
        assert_eq!(masked[5].trim(), "x");
        let python = mask(Lang::Python, &["x = '''{", "}''' # {", "def f(): pass"]);
        assert!(
            !python[0].contains('{') && !python[1].contains('{'),
            "{python:?}"
        );
    }

    #[test]
    fn rust_items_their_methods_fields_variants_and_ends() {
        let text = r#"//! A module.
pub struct Session {
    pub name: String,
    id: u64,
}

impl Session {
    pub fn spawn(&self) -> Result<()> {
        let s = "}";
        Ok(())
    }
}

pub enum Kind {
    Started,
    Ended { code: i32 },
}

pub const MAX: usize = 8;

fn helper(
    a: u32,
) -> u32 {
    a
}
"#;
        let defs = defs("src/session.rs", text);
        assert_eq!(find(&defs, "Session").1, DefKind::Struct);
        assert_eq!(span(find(&defs, "Session")), (2, 5));
        assert_eq!(find(&defs, "name").1, DefKind::Field);
        assert_eq!(find(&defs, "name").2, "session::Session::name");
        let spawn = find(&defs, "spawn");
        assert_eq!(
            (spawn.1, spawn.2.as_str(), spawn.3, spawn.4),
            (DefKind::Method, "session::Session::spawn", 8, 11)
        );
        assert_eq!(find(&defs, "Started").1, DefKind::Variant);
        assert_eq!(find(&defs, "Ended").2, "session::Kind::Ended");
        assert!(!defs.iter().any(|def| def.0 == "code"), "{defs:?}");
        assert_eq!(span(find(&defs, "MAX")), (19, 19));
        assert_eq!(span(find(&defs, "helper")), (21, 25));
    }

    #[test]
    fn clap_s_fields_are_flags_and_its_subcommands_commands() {
        let text = r#"#[derive(Subcommand)]
enum WikiCommand {
    /// Writes it.
    Build {
        #[arg(long)]
        fresh: bool,
        #[arg(short = 'C', long = "dir")]
        dir: Option<PathBuf>,
        #[arg(long, value_name = "USD")]
        max_budget_usd: Option<f64>,
    },
    #[command(name = "status", visible_alias = "st")]
    Show,
}
"#;
        let defs = defs("src/main.rs", text);
        let named = |name: &str, kind: DefKind| defs.iter().any(|d| d.0 == name && d.1 == kind);
        assert!(named("build", DefKind::Command), "{defs:?}");
        assert!(named("status", DefKind::Command), "{defs:?}");
        assert!(named("st", DefKind::Command), "{defs:?}");
        assert!(named("--fresh", DefKind::Flag), "{defs:?}");
        assert!(named("--dir", DefKind::Flag), "{defs:?}");
        assert!(named("-C", DefKind::Flag), "{defs:?}");
        assert!(named("--max-budget-usd", DefKind::Flag), "{defs:?}");
        assert_eq!(find(&defs, "build").2, "WikiCommand::build");
    }

    #[test]
    fn go_python_typescript_c_ruby_shell_and_config() {
        let go = defs(
            "pkg/server/server.go",
            "package server\n\ntype Server struct {\n\tAddr string\n}\n\nfunc (s *Server) Serve() error {\n\treturn nil\n}\n\nfunc New() *Server {\n\treturn nil\n}\n\nconst (\n\tPort = 80\n)\n\ntype ID int\n",
        );
        assert_eq!(find(&go, "Serve").2, "server.Server.Serve");
        assert_eq!(span(find(&go, "Serve")), (7, 9));
        assert_eq!(find(&go, "Addr").1, DefKind::Field);
        assert_eq!(find(&go, "Port").1, DefKind::Const);
        assert_eq!(span(find(&go, "ID")), (19, 19));
        let python = defs(
            "app/jobs.py",
            "MAX_TRIES = 3\n\nclass Queue:\n    def push(self, job):\n        return job\n\n    async def pop(self):\n        pass\n\ndef main():\n    p.add_argument('-v', '--verbose')\n",
        );
        assert_eq!(find(&python, "push").2, "app.jobs.Queue.push");
        assert_eq!(span(find(&python, "Queue")), (3, 8));
        assert_eq!(find(&python, "MAX_TRIES").1, DefKind::Const);
        assert_eq!(find(&python, "--verbose").1, DefKind::Flag);
        let ts = defs(
            "web/src/api.ts",
            "export interface Wiki {\n  version: number;\n}\nexport class Client {\n  private base: string;\n  async fetchWiki(key: string): Promise<Wiki> {\n    if (key) {\n      return x;\n    }\n  }\n}\nexport const VERSION = 1;\nexport function render(w: Wiki) {\n}\n",
        );
        assert_eq!(find(&ts, "fetchWiki").2, "web.src.api.Client.fetchWiki");
        assert_eq!(span(find(&ts, "fetchWiki")), (6, 10));
        assert!(!ts.iter().any(|d| d.0 == "if"), "{ts:?}");
        assert_eq!(find(&ts, "VERSION").1, DefKind::Const);
        let c = defs(
            "src/buf.c",
            "#define CAP 64\n\nstruct buf {\n  int len;\n};\n\nstatic int buf_push(struct buf *b, int x)\n{\n  if (x) {\n  }\n  return 0;\n}\n",
        );
        assert_eq!(find(&c, "CAP").1, DefKind::Macro);
        assert_eq!(span(find(&c, "buf_push")), (7, 12));
        let ruby = defs(
            "lib/a.rb",
            "module Shop\n  class Cart\n    def add(item)\n      item\n    end\n  end\nend\n",
        );
        assert_eq!(span(find(&ruby, "add")), (3, 5));
        assert_eq!(find(&ruby, "add").2, "lib.a.Shop.Cart.add");
        let shell = defs(
            "install.sh",
            "#!/bin/sh\nPREFIX=/usr\nfetch() {\n  curl x\n}\n",
        );
        assert_eq!(span(find(&shell, "fetch")), (3, 5));
        assert_eq!(find(&shell, "PREFIX").1, DefKind::Variable);
        let toml = defs(
            "Cargo.toml",
            "[package]\nname = \"x\"\n\n[profile.release]\nlto = true\n",
        );
        assert_eq!(find(&toml, "lto").2, "profile.release.lto");
    }

    #[test]
    fn kebab_is_clap_s_case() {
        assert_eq!(kebab("max_budget_usd"), "max-budget-usd");
        assert_eq!(kebab("RestartServer"), "restart-server");
        assert_eq!(impl_type("<T: Clone> Display for Wrapper<T> {"), "Wrapper");
        assert_eq!(impl_type("crate::Session {"), "Session");
    }
}
