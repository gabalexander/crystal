//! Mermaid diagrams drawn as text: boxes and arrows in terminal cells,
//! where a page in a preview has a ```` ```mermaid ```` fence, and for
//! `crystal mermaid`.
//!
//! Agents explain code with mermaid because a browser draws it. What a grid
//! of characters can draw well is a sequence of messages between lifelines,
//! and boxes joined by lines at right angles, so that's what is drawn, as
//! far as it reads well; anything past it is refused with a reason rather
//! than drawn wrong, and the caller shows the source instead:
//!
//! * `sequenceDiagram`: participants, the six message arrows, notes,
//!   activations, `loop`/`alt`/`opt`/`par`/`critical`/`break` frames and
//!   `autonumber` ([`sequence`]).
//! * `flowchart` / `graph`: every direction, the node shapes as boxes, the
//!   four edge strokes, labels, `&`, chains and subgraphs ([`flowchart`]).
//! * `stateDiagram-v2`: start and end, described and composite states,
//!   choice, fork and join ([`state`]).
//! * `classDiagram` and `erDiagram`: entities with their members, and
//!   relations with their cardinalities as text ([`class`], [`er`]).
//!
//! All but the sequence go through one layered layout ([`graph`]). It's
//! all pure functions that never panic, whatever they're given (the tests
//! throw garbage at the parsers): the diagrams come from what a model
//! wrote.
//!
//! Adapted from docket's `docket-mermaid` crate.

mod canvas;
mod class;
mod er;
mod flowchart;
mod graph;
mod sequence;
mod state;
mod width;

pub use canvas::Glyphs;
pub use width::display_width;

/// Which kind of diagram was drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Sequence,
    Flowchart,
    State,
    Class,
    Er,
}

impl Kind {
    /// The word the caption under a drawn diagram names it by.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Sequence => "sequence",
            Kind::Flowchart => "flowchart",
            Kind::State => "state",
            Kind::Class => "class",
            Kind::Er => "er",
        }
    }
}

/// What [`render`] made of a diagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rendered {
    /// The diagram, a string a row, none wider than the width asked for.
    Diagram { lines: Vec<String>, kind: Kind },
    /// Why it wasn't drawn: a kind not drawn here, a line that couldn't be
    /// read, or a layout too wide for the width.
    Unsupported { reason: String },
}

/// Draws `source` in at most `width` columns, with `glyphs`.
pub fn render(source: &str, width: usize, glyphs: Glyphs) -> Rendered {
    let unsupported = |reason: String| Rendered::Unsupported { reason };
    let kind = match detect(source) {
        None => return unsupported("the diagram is empty".into()),
        Some(Err(name)) => {
            return unsupported(format!("{name} diagrams are not drawn in a terminal"));
        }
        Some(Ok(kind)) => kind,
    };
    let drawn = match kind {
        Kind::Sequence => sequence::parse(source).and_then(|d| sequence::draw(&d, &glyphs, width)),
        Kind::Flowchart => flowchart::parse(source).and_then(|g| graph::draw(&g, &glyphs, width)),
        Kind::State => state::parse(source).and_then(|g| graph::draw(&g, &glyphs, width)),
        Kind::Class => class::parse(source).and_then(|g| graph::draw(&g, &glyphs, width)),
        Kind::Er => er::parse(source).and_then(|g| graph::draw(&g, &glyphs, width)),
    };
    match drawn {
        Ok(lines) => {
            debug_assert!(lines.iter().all(|line| display_width(line) <= width));
            Rendered::Diagram { lines, kind }
        }
        Err(reason) => unsupported(reason),
    }
}

/// The diagram's kind, from its first statement: `Some(Ok(kind))` for one
/// drawn here, `Some(Err(name))` for one that isn't, `None` for an empty
/// source.
pub fn detect(source: &str) -> Option<Result<Kind, String>> {
    let (_, first) = lines(source).next()?;
    let word = first
        .split(|c: char| c.is_whitespace() || c == ';')
        .next()
        .unwrap_or("");
    Some(match word {
        "sequenceDiagram" => Ok(Kind::Sequence),
        "flowchart" | "graph" | "flowchart-elk" => Ok(Kind::Flowchart),
        "stateDiagram" | "stateDiagram-v2" => Ok(Kind::State),
        "classDiagram" | "classDiagram-v2" => Ok(Kind::Class),
        "erDiagram" => Ok(Kind::Er),
        "" => Err("unnamed".to_string()),
        other => Err(other.to_string()),
    })
}

/// The source's statements, each with its line number and trimmed: blank
/// lines, `%%` comments, `%%{init}%%` directives and a `---` front matter
/// block (a diagram's title and config) left out.
fn lines(source: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut in_front_matter = false;
    let mut seen = false;
    source.lines().enumerate().filter_map(move |(index, raw)| {
        let line = raw.trim();
        if line == "---" && (!seen || in_front_matter) {
            in_front_matter = !in_front_matter;
            seen = true;
            return None;
        }
        if in_front_matter || line.is_empty() || line.starts_with("%%") {
            return None;
        }
        seen = true;
        Some((index + 1, line))
    })
}

#[cfg(test)]
mod tests;
