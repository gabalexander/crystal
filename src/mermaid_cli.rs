//! `crystal mermaid`: a mermaid diagram drawn as text, the way crystal's
//! previews draw it, so an agent can see the diagram it's about to write
//! into a page, and so can you. It needs no daemon: it reads, draws and
//! prints.
//!
//! What it reads is a diagram, or a markdown file with ```` ```mermaid ````
//! fences, each drawn in turn under a `── diagram 2 ──` rule. A diagram
//! that can't be drawn is printed as its source, and the command fails
//! saying why, which is what an agent checking its diagram wants to know.

use crate::markdown;
use crate::mermaid::{self, Glyphs, Rendered};
use anyhow::{Context, Result, bail};
use std::io::{IsTerminal, Read, Write};

/// Draws the diagrams in the file at `input`, or on standard input with
/// `-` or none, in `width` columns or the terminal's.
pub fn run(input: Option<&str>, width: Option<usize>, ascii: bool) -> Result<()> {
    let text = match input {
        None | Some("-") => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .context("couldn't read standard input")?;
            text
        }
        Some(path) => {
            std::fs::read_to_string(path).with_context(|| format!("couldn't read {path}"))?
        }
    };
    if text.trim().is_empty() {
        bail!("no diagram: give a file, or pipe one in");
    }
    let width = width.unwrap_or_else(terminal_width).max(1);
    let glyphs = if ascii {
        Glyphs::ASCII
    } else {
        Glyphs::BOX_DRAWING
    };
    let not_drawn = draw(&text, width, glyphs, &mut std::io::stdout().lock())?;
    if !not_drawn.is_empty() {
        bail!("{}", not_drawn.join("; "));
    }
    Ok(())
}

/// The terminal's width, or 80 when the output isn't one.
fn terminal_width() -> usize {
    if !std::io::stdout().is_terminal() {
        return 80;
    }
    crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns))
}

/// The diagrams in `text`: the text itself when it's a diagram, or else
/// its mermaid fences. Text that's neither is handed on whole, so the
/// reason it isn't drawn says what it is.
fn diagrams(text: &str) -> Vec<String> {
    if mermaid::detect(text).is_some_and(|kind| kind.is_ok()) {
        return vec![text.to_string()];
    }
    let blocks = markdown::mermaid_blocks(text);
    if blocks.is_empty() {
        return vec![text.to_string()];
    }
    blocks
}

/// Draws every diagram in `text` to `out`, and the source of each that
/// can't be drawn. Returns why each of those wasn't.
fn draw(text: &str, width: usize, glyphs: Glyphs, out: &mut impl Write) -> Result<Vec<String>> {
    let diagrams = diagrams(text);
    let several = diagrams.len() > 1;
    let mut not_drawn = Vec::new();
    for (index, source) in diagrams.iter().enumerate() {
        let number = index + 1;
        if several {
            if index > 0 {
                writeln!(out)?;
            }
            writeln!(out, "── diagram {number} ──")?;
        }
        match mermaid::render(source, width, glyphs) {
            Rendered::Diagram { lines, .. } => {
                for line in lines {
                    writeln!(out, "{line}")?;
                }
            }
            Rendered::Unsupported { reason } => {
                writeln!(out, "{}", source.trim_end())?;
                let which = if several {
                    format!("diagram {number} ")
                } else {
                    String::new()
                };
                not_drawn.push(format!("{which}not drawn: {reason}"));
            }
        }
    }
    Ok(not_drawn)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `text` prints at `width`, and why what wasn't drawn wasn't.
    fn drawn(text: &str, width: usize) -> (String, Vec<String>) {
        let mut out = Vec::new();
        let not_drawn = draw(text, width, Glyphs::BOX_DRAWING, &mut out).unwrap();
        (String::from_utf8(out).unwrap(), not_drawn)
    }

    #[test]
    fn a_diagram_is_drawn() {
        let (out, not_drawn) = drawn("flowchart LR\n a --> b\n", 80);
        assert!(not_drawn.is_empty());
        assert_eq!(out, "┌───┐   ┌───┐\n│ a ├──▶│ b │\n└───┘   └───┘\n");
    }

    #[test]
    fn a_markdown_page_draws_each_of_its_diagrams_under_a_rule() {
        let page = "# Explained\n\nText.\n\n```mermaid\nflowchart LR\n a --> b\n```\n\n~~~mermaid\nsequenceDiagram\n A->>B: hi\n~~~\n\n```rust\nfn main() {}\n```\n";
        let (out, not_drawn) = drawn(page, 80);
        assert!(not_drawn.is_empty());
        assert!(out.starts_with("── diagram 1 ──\n┌───┐"), "{out}");
        assert!(out.contains("\n\n── diagram 2 ──\n"), "{out}");
        assert!(!out.contains("fn main"), "{out}");
    }

    #[test]
    fn what_cant_be_drawn_prints_its_source_and_says_why() {
        let (out, not_drawn) = drawn("pie\n \"a\": 1\n", 80);
        assert_eq!(out, "pie\n \"a\": 1\n");
        assert_eq!(
            not_drawn,
            ["not drawn: pie diagrams are not drawn in a terminal"]
        );
        let (_, not_drawn) = drawn("sequenceDiagram\n A->>B: x\n B->>C: y\n C->>D: z", 12);
        assert!(not_drawn[0].contains("columns"), "{not_drawn:?}");
    }
}
