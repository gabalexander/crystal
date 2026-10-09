//! Reading a SCIP index (Sourcegraph's Code Intelligence Protocol), the
//! file the precise tier's indexers write: a protobuf `Index` of
//! `Document`s, each a file's occurrences of symbols, every definition and
//! every reference with its exact range, and what the indexer says of each
//! symbol defined there. Only what the wiki uses is read, by a reader of
//! protobuf's wire format of its own, a document at a time, so an index of
//! hundreds of megabytes is never held whole; the schema is
//! <https://github.com/scip-code/scip/blob/main/scip.proto>.

use super::DefKind;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::io::{BufReader, Read};

/// A definition a document has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub symbol: String,
    /// The line its name is on, from 1.
    pub line: u32,
    /// Its first and last lines, from its enclosing range when the indexer
    /// gives one, else its name's line.
    pub start: u32,
    pub end: u32,
    /// What the indexer says it is, by SCIP's `Kind`.
    pub kind: Option<DefKind>,
    pub test: bool,
}

/// What an indexer says of a file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    /// From the directory the indexer ran in.
    pub path: String,
    pub definitions: Vec<Definition>,
    /// The symbols it refers to, each once.
    pub references: Vec<String>,
}

/// What an index says: the indexer that wrote it, and each document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Indexed {
    pub tool: String,
    pub documents: Vec<Document>,
}

/// Reads the SCIP index `input`.
pub fn read(input: impl Read) -> Result<Indexed> {
    let mut input = Wire::new(BufReader::new(input));
    let mut read = Indexed::default();
    while let Some((field, wire)) = input.key()? {
        match (field, wire) {
            (1, LEN) => {
                let metadata = input.bytes()?;
                read.tool = tool(&metadata).unwrap_or_default();
            }
            (2, LEN) => {
                let document = input.bytes()?;
                read.documents.push(
                    document_in(&document).context("a document the indexer wrote is broken")?,
                );
            }
            (_, wire) => input.skip(wire)?,
        }
    }
    Ok(read)
}

/// The tool a `Metadata` names: `rust-analyzer 0.3.2`.
fn tool(metadata: &[u8]) -> Option<String> {
    let mut fields = Fields(metadata);
    while let Some((field, value)) = fields.next_field().ok()? {
        if let (2, Value::Bytes(info)) = (field, value) {
            let (mut name, mut version) = (String::new(), String::new());
            let mut info = Fields(info);
            while let Some((field, value)) = info.next_field().ok()? {
                match (field, value) {
                    (1, Value::Bytes(text)) => name = String::from_utf8_lossy(text).into_owned(),
                    (2, Value::Bytes(text)) => version = String::from_utf8_lossy(text).into_owned(),
                    _ => {}
                }
            }
            return Some(format!("{name} {version}").trim().to_string());
        }
    }
    None
}

/// Reads a `Document`: its path (1), occurrences (2) and symbols (3).
fn document_in(bytes: &[u8]) -> Result<Document> {
    let mut document = Document::default();
    let mut kinds: HashMap<String, i64> = HashMap::new();
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut fields = Fields(bytes);
    while let Some((field, value)) = fields.next_field()? {
        match (field, value) {
            (1, Value::Bytes(path)) => document.path = String::from_utf8_lossy(path).into_owned(),
            (2, Value::Bytes(occurrence)) => {
                let occurrence = occurrence_in(occurrence)?;
                if occurrence.symbol.is_empty() || occurrence.symbol.starts_with("local ") {
                    continue;
                }
                if occurrence.roles & DEFINITION != 0 {
                    let Some(&line) = occurrence.range.first() else {
                        continue;
                    };
                    let (start, end) = match occurrence.enclosing.as_slice() {
                        [start, _, end, _] => (*start, *end),
                        [line, _, _] => (*line, *line),
                        _ => (line, line),
                    };
                    let line_of = |row: i64| u32::try_from(row + 1).unwrap_or(0);
                    document.definitions.push(Definition {
                        symbol: occurrence.symbol,
                        line: line_of(line),
                        start: line_of(start.min(line)),
                        end: line_of(end.max(line)),
                        kind: None,
                        test: occurrence.roles & TEST != 0,
                    });
                } else if seen.insert(occurrence.symbol.clone(), ()).is_none() {
                    document.references.push(occurrence.symbol);
                }
            }
            (3, Value::Bytes(information)) => {
                if let Some((symbol, kind)) = information_in(information)? {
                    kinds.insert(symbol, kind);
                }
            }
            _ => {}
        }
    }
    for definition in &mut document.definitions {
        definition.kind = kinds
            .get(&definition.symbol)
            .and_then(|&kind| kind_of(kind));
    }
    Ok(document)
}

/// What the wiki reads of an `Occurrence`.
struct Occurrence {
    range: Vec<i64>,
    symbol: String,
    roles: i64,
    enclosing: Vec<i64>,
}

/// SCIP's `SymbolRole`s the wiki reads.
const DEFINITION: i64 = 0x1;
const TEST: i64 = 0x20;

/// Reads an `Occurrence`: its range (1), symbol (2), roles (3) and
/// enclosing range (7).
fn occurrence_in(bytes: &[u8]) -> Result<Occurrence> {
    let mut occurrence = Occurrence {
        range: Vec::new(),
        symbol: String::new(),
        roles: 0,
        enclosing: Vec::new(),
    };
    let mut fields = Fields(bytes);
    while let Some((field, value)) = fields.next_field()? {
        match (field, value) {
            (1, Value::Bytes(packed)) => occurrence.range = packed_in(packed)?,
            (1, Value::Varint(one)) => occurrence.range.push(one),
            (2, Value::Bytes(symbol)) => {
                occurrence.symbol = String::from_utf8_lossy(symbol).into_owned()
            }
            (3, Value::Varint(roles)) => occurrence.roles = roles,
            (7, Value::Bytes(packed)) => occurrence.enclosing = packed_in(packed)?,
            (7, Value::Varint(one)) => occurrence.enclosing.push(one),
            _ => {}
        }
    }
    Ok(occurrence)
}

/// Reads a `SymbolInformation`'s symbol (1) and kind (5).
fn information_in(bytes: &[u8]) -> Result<Option<(String, i64)>> {
    let (mut symbol, mut kind) = (None, None);
    let mut fields = Fields(bytes);
    while let Some((field, value)) = fields.next_field()? {
        match (field, value) {
            (1, Value::Bytes(text)) => symbol = Some(String::from_utf8_lossy(text).into_owned()),
            (5, Value::Varint(value)) => kind = Some(value),
            _ => {}
        }
    }
    Ok(symbol.zip(kind))
}

/// A definition's kind from SCIP's `SymbolInformation.Kind`.
fn kind_of(kind: i64) -> Option<DefKind> {
    Some(match kind {
        7 | 75 => DefKind::Class,
        8 => DefKind::Const,
        11 => DefKind::Enum,
        12 => DefKind::Variant,
        15 | 41 | 77 | 79 | 81 => DefKind::Field,
        17 => DefKind::Function,
        9 | 26 | 66 | 67 | 68 | 69 | 70 | 71 | 76 | 80 => DefKind::Method,
        21 | 42 => DefKind::Interface,
        25 => DefKind::Macro,
        29 | 30 | 35 => DefKind::Module,
        49 => DefKind::Struct,
        53 => DefKind::Trait,
        3 | 54 | 55 => DefKind::Type,
        59 => DefKind::Union,
        61 | 82 => DefKind::Variable,
        _ => return None,
    })
}

/// A symbol's descriptors, from the end of the symbol: each its name and
/// what kind of name, by SCIP's suffixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suffix {
    /// `/`: a namespace, a module or a package.
    Namespace,
    /// `#`: a type.
    Type,
    /// `.`: a term, a field or a constant.
    Term,
    /// `().`: a method or a function.
    Method,
    /// `[name]`: a type parameter, or an impl's type in rust-analyzer's.
    TypeParameter,
    /// `(name)`: a parameter.
    Parameter,
    /// `:`.
    Meta,
    /// `!`: a macro.
    Macro,
}

/// The descriptors of a global symbol, `<scheme> <manager> <package>
/// <version> <descriptors>`, or `None` for a local one or one that can't be
/// read.
pub fn descriptors(symbol: &str) -> Option<Vec<(String, Suffix)>> {
    if symbol.starts_with("local ") {
        return None;
    }
    // Four words, a space in one written twice.
    let mut rest = symbol;
    for _ in 0..4 {
        let mut at = 0;
        let bytes = rest.as_bytes();
        loop {
            match bytes.get(at) {
                None => return None,
                Some(b' ') if bytes.get(at + 1) == Some(&b' ') => at += 2,
                Some(b' ') => break,
                Some(_) => at += 1,
            }
        }
        rest = &rest[at + 1..];
    }
    let mut descriptors = Vec::new();
    let mut chars = rest.chars().peekable();
    let name = |chars: &mut std::iter::Peekable<std::str::Chars>| -> Option<String> {
        let mut name = String::new();
        if chars.peek() == Some(&'`') {
            chars.next();
            loop {
                match chars.next()? {
                    '`' if chars.peek() == Some(&'`') => {
                        chars.next();
                        name.push('`');
                    }
                    '`' => break,
                    c => name.push(c),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_alphanumeric() || matches!(c, '_' | '+' | '-' | '$') {
                    name.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
        }
        Some(name)
    };
    while chars.peek().is_some() {
        match chars.peek()? {
            '[' => {
                chars.next();
                let inner = name(&mut chars)?;
                (chars.next()? == ']').then_some(())?;
                descriptors.push((inner, Suffix::TypeParameter));
            }
            '(' => {
                chars.next();
                let inner = name(&mut chars)?;
                (chars.next()? == ')').then_some(())?;
                descriptors.push((inner, Suffix::Parameter));
            }
            _ => {
                let text = name(&mut chars)?;
                let suffix = match chars.next()? {
                    '/' => Suffix::Namespace,
                    '#' => Suffix::Type,
                    '.' => Suffix::Term,
                    ':' => Suffix::Meta,
                    '!' => Suffix::Macro,
                    '(' => {
                        // A method's disambiguator, then `).`.
                        while chars.next()? != ')' {}
                        (chars.next()? == '.').then_some(())?;
                        Suffix::Method
                    }
                    _ => return None,
                };
                descriptors.push((text, suffix));
            }
        }
    }
    Some(descriptors)
}

/// What a symbol's definition is called, and what it's in, as crystal names
/// it: Rust's `impl#[Session]stop().` is `Session::stop`, Go's package path
/// its last part.
pub fn segments(symbol: &str) -> Option<(Vec<String>, Suffix)> {
    let descriptors = descriptors(symbol)?;
    let (_, last) = descriptors.last()?.clone();
    let mut segments = Vec::new();
    let mut at = 0;
    while at < descriptors.len() {
        let (name, suffix) = &descriptors[at];
        match suffix {
            // rust-analyzer's impl: `impl#[Type][Trait]`.
            Suffix::Type
                if name == "impl"
                    && descriptors
                        .get(at + 1)
                        .is_some_and(|(_, next)| *next == Suffix::TypeParameter) =>
            {
                segments.push(descriptors[at + 1].0.clone());
                at += 2;
                while descriptors
                    .get(at)
                    .is_some_and(|(_, next)| *next == Suffix::TypeParameter)
                {
                    at += 1;
                }
                continue;
            }
            Suffix::Namespace => {
                let part = name.rsplit('/').next().unwrap_or(name);
                if !part.is_empty() && name != "crate" {
                    segments.push(part.to_string());
                }
            }
            Suffix::TypeParameter | Suffix::Parameter | Suffix::Meta => {}
            _ => segments.push(name.clone()),
        }
        at += 1;
    }
    Some((segments, last)).filter(|(segments, _)| !segments.is_empty())
}

/// What a definition is by its symbol's last suffix, for an indexer that
/// doesn't say.
pub fn kind_of_suffix(suffix: Suffix) -> Option<DefKind> {
    match suffix {
        Suffix::Namespace => Some(DefKind::Module),
        Suffix::Type => Some(DefKind::Type),
        Suffix::Term => Some(DefKind::Variable),
        Suffix::Method => Some(DefKind::Function),
        Suffix::Macro => Some(DefKind::Macro),
        _ => None,
    }
}

// Protobuf's wire format.

const VARINT: u8 = 0;
const I64: u8 = 1;
const LEN: u8 = 2;
const I32: u8 = 5;

/// A field's value.
enum Value<'a> {
    Varint(i64),
    Bytes(&'a [u8]),
    Fixed,
}

/// The fields of a message held whole.
struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn varint(&mut self) -> Result<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = self
                .0
                .split_first()
                .context("a number runs past the message's end")?;
            self.0 = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Ok(value);
            }
        }
        bail!("a number is too long")
    }

    fn next_field(&mut self) -> Result<Option<(u64, Value<'a>)>> {
        if self.0.is_empty() {
            return Ok(None);
        }
        let key = self.varint()?;
        let (field, wire) = (key >> 3, (key & 7) as u8);
        let value = match wire {
            VARINT => Value::Varint(self.varint()? as i64),
            LEN => {
                let length = usize::try_from(self.varint()?)?;
                (length <= self.0.len())
                    .then_some(())
                    .context("a field runs past the message's end")?;
                let (bytes, rest) = self.0.split_at(length);
                self.0 = rest;
                Value::Bytes(bytes)
            }
            I64 | I32 => {
                let length = if wire == I64 { 8 } else { 4 };
                (length <= self.0.len())
                    .then_some(())
                    .context("a field runs past the message's end")?;
                self.0 = &self.0[length..];
                Value::Fixed
            }
            _ => bail!("a field of wire type {wire}"),
        };
        Ok(Some((field, value)))
    }
}

/// A packed repeated `int32`, as a range is.
fn packed_in(bytes: &[u8]) -> Result<Vec<i64>> {
    let mut fields = Fields(bytes);
    let mut values = Vec::new();
    while !fields.0.is_empty() {
        values.push(fields.varint()? as i32 as i64);
    }
    Ok(values)
}

/// The top level of a message read from a stream, a field at a time.
struct Wire<R> {
    input: R,
}

impl<R: std::io::Read> Wire<R> {
    fn new(input: R) -> Wire<R> {
        Wire { input }
    }

    /// The next byte, or `None` at the end.
    fn byte(&mut self) -> Result<Option<u8>> {
        let mut byte = [0];
        match self.input.read(&mut byte)? {
            0 => Ok(None),
            _ => Ok(Some(byte[0])),
        }
    }

    fn varint(&mut self) -> Result<Option<u64>> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let Some(byte) = self.byte()? else {
                if shift == 0 {
                    return Ok(None);
                }
                bail!("the index ends in a number");
            };
            value |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Ok(Some(value));
            }
        }
        bail!("a number is too long")
    }

    fn key(&mut self) -> Result<Option<(u64, u8)>> {
        Ok(self.varint()?.map(|key| (key >> 3, (key & 7) as u8)))
    }

    fn bytes(&mut self) -> Result<Vec<u8>> {
        let length = usize::try_from(self.varint()?.context("the index ends in a field")?)?;
        let mut bytes = vec![0; length];
        self.input
            .read_exact(&mut bytes)
            .context("the index ends in a field")?;
        Ok(bytes)
    }

    fn skip(&mut self, wire: u8) -> Result<()> {
        let length = match wire {
            VARINT => {
                self.varint()?;
                return Ok(());
            }
            I64 => 8,
            I32 => 4,
            LEN => self.varint()?.context("the index ends in a field")?,
            _ => bail!("a field of wire type {wire}"),
        };
        std::io::copy(&mut (&mut self.input).take(length), &mut std::io::sink())?;
        Ok(())
    }
}

/// Writing SCIP, for tests: the fields of a message, each written as
/// protobuf does.
#[cfg(test)]
pub mod write {
    pub fn varint(mut value: u64, out: &mut Vec<u8>) {
        while value >= 0x80 {
            out.push((value as u8) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }

    pub fn bytes(field: u64, bytes: &[u8], out: &mut Vec<u8>) {
        varint(field << 3 | 2, out);
        varint(bytes.len() as u64, out);
        out.extend_from_slice(bytes);
    }

    pub fn number(field: u64, value: u64, out: &mut Vec<u8>) {
        varint(field << 3, out);
        varint(value, out);
    }

    pub fn packed(field: u64, values: &[u64]) -> Vec<u8> {
        let mut inner = Vec::new();
        for &value in values {
            varint(value, &mut inner);
        }
        let mut out = Vec::new();
        bytes(field, &inner, &mut out);
        out
    }

    /// An occurrence of `symbol` at `range`, with `roles` and, for a
    /// definition, its `enclosing` range.
    pub fn occurrence(range: &[u64], symbol: &str, roles: u64, enclosing: &[u64]) -> Vec<u8> {
        let mut occurrence = packed(1, range);
        bytes(2, symbol.as_bytes(), &mut occurrence);
        if roles != 0 {
            number(3, roles, &mut occurrence);
        }
        if !enclosing.is_empty() {
            occurrence.extend(packed(7, enclosing));
        }
        occurrence
    }

    pub fn information(symbol: &str, kind: u64) -> Vec<u8> {
        let mut information = Vec::new();
        bytes(1, symbol.as_bytes(), &mut information);
        number(5, kind, &mut information);
        information
    }

    pub fn document(path: &str, occurrences: &[Vec<u8>], informations: &[Vec<u8>]) -> Vec<u8> {
        let mut document = Vec::new();
        bytes(1, path.as_bytes(), &mut document);
        for occurrence in occurrences {
            bytes(2, occurrence, &mut document);
        }
        for information in informations {
            bytes(3, information, &mut document);
        }
        document
    }

    pub fn index(tool: (&str, &str), documents: &[Vec<u8>]) -> Vec<u8> {
        let mut info = Vec::new();
        bytes(1, tool.0.as_bytes(), &mut info);
        bytes(2, tool.1.as_bytes(), &mut info);
        let mut metadata = Vec::new();
        number(1, 0, &mut metadata);
        bytes(2, &info, &mut metadata);
        let mut index = Vec::new();
        bytes(1, &metadata, &mut index);
        for document in documents {
            bytes(2, document, &mut index);
        }
        // An external symbol, which is passed over.
        bytes(
            3,
            &information("rust-analyzer cargo std 1.0 io/", 29),
            &mut index,
        );
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RA: &str = "rust-analyzer cargo crystal 0.3.0 ";

    /// A tiny index as rust-analyzer writes one: `src/bell.rs` defining a
    /// struct, its field, a method in its impl and a test, and `src/main.rs`
    /// referring to the struct and the method.
    fn fixture() -> Vec<u8> {
        let ringer = format!("{RA}bell/Ringer#");
        let last = format!("{RA}bell/Ringer#last.");
        let ring = format!("{RA}bell/impl#[Ringer]ring().");
        let test = format!("{RA}bell/tests/rings().");
        let bell = write::document(
            "src/bell.rs",
            &[
                write::occurrence(&[17, 11, 17], &ringer, 1, &[15, 0, 19, 1]),
                write::occurrence(&[18, 4, 8], &last, 1, &[18, 4, 25]),
                write::occurrence(&[37, 11, 15], &ring, 1, &[35, 4, 44, 5]),
                write::occurrence(&[41, 16, 19], "local 4", 1, &[]),
                write::occurrence(&[52, 7, 12], &test, 1 | 0x20, &[51, 4, 62, 5]),
                write::occurrence(&[54, 12, 18], &ringer, 8, &[]),
            ],
            &[
                write::information(&ringer, 49),
                write::information(&last, 15),
                write::information(&ring, 26),
            ],
        );
        let main = write::document(
            "src/main.rs",
            &[
                write::occurrence(&[3, 4, 10], &ringer, 8, &[]),
                write::occurrence(&[9, 4, 10], &ring, 0, &[]),
                write::occurrence(&[12, 4, 10], &ringer, 8, &[]),
                write::occurrence(&[0, 0, 3, 0], &format!("{RA}crate/"), 1, &[]),
            ],
            &[],
        );
        write::index(("rust-analyzer", "0.3.3073"), &[bell, main])
    }

    /// A definition as a test looks at it: the end of its symbol, its name's
    /// line, its lines, its kind and whether it's a test's.
    type Seen<'a> = (&'a str, u32, u32, u32, Option<DefKind>, bool);

    #[test]
    fn an_index_s_definitions_and_references_are_read_a_document_at_a_time() {
        let read = read(fixture().as_slice()).unwrap();
        assert_eq!(read.tool, "rust-analyzer 0.3.3073");
        let [bell, main] = read.documents.as_slice() else {
            panic!("two documents, not {}", read.documents.len());
        };
        assert_eq!(bell.path, "src/bell.rs");
        let defs: Vec<Seen> = bell
            .definitions
            .iter()
            .map(|def| {
                (
                    def.symbol.rsplit(' ').next().unwrap(),
                    def.line,
                    def.start,
                    def.end,
                    def.kind,
                    def.test,
                )
            })
            .collect();
        assert_eq!(
            defs,
            [
                ("bell/Ringer#", 18, 16, 20, Some(DefKind::Struct), false),
                ("bell/Ringer#last.", 19, 19, 19, Some(DefKind::Field), false),
                (
                    "bell/impl#[Ringer]ring().",
                    38,
                    36,
                    45,
                    Some(DefKind::Method),
                    false
                ),
                ("bell/tests/rings().", 53, 52, 63, None, true),
            ]
        );
        assert_eq!(bell.references, [format!("{RA}bell/Ringer#")]);
        let refs: Vec<&str> = main
            .references
            .iter()
            .map(|symbol| symbol.rsplit(' ').next().unwrap())
            .collect();
        assert_eq!(refs, ["bell/Ringer#", "bell/impl#[Ringer]ring()."]);
    }

    #[test]
    fn a_broken_index_says_so() {
        let mut broken = fixture();
        broken.truncate(broken.len() / 2);
        assert!(read(broken.as_slice()).is_err());
        assert_eq!(read(&[][..]).unwrap(), Indexed::default());
    }

    #[test]
    fn a_symbol_s_descriptors_name_it_as_crystal_does() {
        let named =
            |symbol: &str| segments(symbol).map(|(segments, suffix)| (segments.join("::"), suffix));
        assert_eq!(
            named(&format!("{RA}bell/impl#[Ringer]ring().")),
            Some(("bell::Ringer::ring".to_string(), Suffix::Method))
        );
        assert_eq!(
            named(&format!("{RA}bell/impl#[Ringer][Default]default().")),
            Some(("bell::Ringer::default".to_string(), Suffix::Method))
        );
        assert_eq!(
            named(
                "scip-go gomod github.com/containers/podman/v6 . `github.com/containers/podman/v6/pkg/domain/entities`/PodmanConfig#FlagSet."
            ),
            Some(("entities::PodmanConfig::FlagSet".to_string(), Suffix::Term))
        );
        assert_eq!(
            named("scip-typescript npm my  pkg 1.0.0 src/`server.ts`/Server#create()."),
            Some(("src::server.ts::Server::create".to_string(), Suffix::Method))
        );
        assert_eq!(
            named(&format!("{RA}out!")),
            Some(("out".to_string(), Suffix::Macro))
        );
        assert_eq!(named(&format!("{RA}crate/")), None);
        assert_eq!(named("local 4"), None);
        assert_eq!(named("rust-analyzer cargo"), None);
    }
}
