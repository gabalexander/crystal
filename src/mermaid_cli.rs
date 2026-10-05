//! `crystal mermaid`: a mermaid diagram drawn as text, the way crystal's
//! previews draw it, so an agent can see the diagram it's about to write
//! into a page, and so can you. It needs no daemon: it reads, draws and
//! prints.
//!
//! What it reads is a diagram, or a markdown file with ```` ```mermaid ````
//! fences, each drawn in turn under a `── diagram 2 ──` rule. A diagram
//! that can't be drawn is printed as its source, and the command fails
//! saying why, which is what an agent checking its diagram wants to know.
//!
//! With `--open`, mermaid itself draws them instead, in the browser: for the
//! exact picture, curves and all, and the kinds a terminal can't draw. The
//! diagrams go on a small page in the state directory, named for what's on
//! it, so the same diagrams are always the same file, which loads mermaid
//! from the jsDelivr CDN.

use crate::mermaid::{self, Glyphs, Rendered};
use crate::output::{self, outln};
use crate::{links, markdown, printable, state};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

/// The mermaid the pages `--open` writes load: the 11s, as an ES module.
const MERMAID_MODULE: &str = "https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs";

/// Draws the diagrams in the file at `input`, or on standard input with
/// `-` or none, in `width` columns or the terminal's, with ASCII when
/// `ascii` says so; or, with `open`, opens them in the browser.
pub fn run(input: Option<&str>, width: Option<usize>, ascii: bool, open: bool) -> Result<()> {
    let text = read(input)?;
    if open {
        let page = write_page(&text, &state::diagrams_dir())?;
        outln!("{}", page.display())?;
        eprintln!("{}", links::open(&file_url(&page))?);
        return Ok(());
    }
    let width = width.unwrap_or_else(terminal_width).max(1);
    let glyphs = if ascii {
        Glyphs::ASCII
    } else {
        Glyphs::BOX_DRAWING
    };
    let not_drawn =
        draw(&text, width, glyphs, &mut std::io::stdout().lock()).map_err(output::failed)?;
    if !not_drawn.is_empty() {
        bail!("{}", not_drawn.join("; "));
    }
    Ok(())
}

/// What's in the file at `input`, or on standard input with `-` or none:
/// something, or an error.
fn read(input: Option<&str>) -> Result<String> {
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
    Ok(text)
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
fn draw(
    text: &str,
    width: usize,
    glyphs: Glyphs,
    out: &mut impl Write,
) -> std::io::Result<Vec<String>> {
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
        // A diagram comes from anywhere, and a label's entities are read
        // as the characters they stand for.
        match mermaid::render(source, width, glyphs) {
            Rendered::Diagram { lines, .. } => {
                for line in lines {
                    writeln!(out, "{}", printable::line(&line))?;
                }
            }
            Rendered::Unsupported { reason } => {
                writeln!(out, "{}", printable::text(source.trim_end()))?;
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

/// Writes the diagrams in `text` on a page in `dir` for mermaid to draw in
/// the browser, unless it's there already, and gives back its path: named
/// for what's on it, so the same diagrams are always the same file.
fn write_page(text: &str, dir: &Path) -> Result<PathBuf> {
    let page = page(&diagrams(text));
    let hash = Sha256::digest(page.as_bytes());
    let name: String = hash[..8].iter().map(|byte| format!("{byte:02x}")).collect();
    let path = dir.join(format!("{name}.html"));
    if !path.exists() {
        std::fs::create_dir_all(dir).with_context(|| format!("couldn't make {}", dir.display()))?;
        std::fs::write(&path, page)
            .with_context(|| format!("couldn't write {}", path.display()))?;
    }
    Ok(path)
}

/// A page that has mermaid draw `diagrams`, one under another, in the
/// light or dark the system has. Each is in the page as text, which is how
/// mermaid reads it, so nothing in it is taken for HTML.
fn page(diagrams: &[String]) -> String {
    let mut body = String::new();
    for diagram in diagrams {
        body.push_str("<pre class=\"mermaid\">\n");
        body.push_str(&escape(diagram.trim_end()));
        body.push_str("\n</pre>\n");
    }
    format!(
        "<!doctype html>\n\
         <html>\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>crystal · mermaid</title>\n\
         <style>\n\
         :root {{ color-scheme: light dark; }}\n\
         body {{ margin: 2rem; background: Canvas; color: CanvasText; font-family: system-ui, sans-serif; }}\n\
         pre.mermaid {{ display: flex; justify-content: center; margin: 0 0 3rem; }}\n\
         </style>\n\
         </head>\n\
         <body>\n\
         {body}\
         <script type=\"module\">\n\
         import mermaid from \"{MERMAID_MODULE}\";\n\
         const dark = window.matchMedia(\"(prefers-color-scheme: dark)\").matches;\n\
         mermaid.initialize({{ startOnLoad: true, securityLevel: \"strict\", theme: dark ? \"dark\" : \"default\" }});\n\
         </script>\n\
         </body>\n\
         </html>\n"
    )
}

/// `text` as HTML holds it as text.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The `file:` link to `path`, with what a link can't hold as it is
/// written with `%`.
fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            url.push(char::from(byte));
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
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

    #[test]
    fn a_diagram_draws_nothing_a_terminal_would_take_as_an_order() {
        let (out, _) = drawn(
            "flowchart LR\n a[\"x\x1b]0;t\x07\"] --> b[\"\u{202e}y\"]\n",
            80,
        );
        let held = out
            .lines()
            .any(|line| line.contains(printable::is_unprintable));
        assert!(!held, "{out:?}");
        assert!(out.contains("]0;t"), "{out}");
        let (out, _) = drawn("pie\n \"\x1b[?1049h\": 1\n", 80);
        assert_eq!(out, "pie\n \"[?1049h\": 1\n");
    }

    #[test]
    fn open_writes_each_diagram_on_a_page_for_mermaid_to_draw() {
        let dir = tempfile::tempdir().unwrap();
        let page_of = |text: &str| write_page(text, dir.path()).unwrap();
        let markdown =
            "# Two\n\n```mermaid\nflowchart LR\n a --> b\n```\n\n```mermaid\npie\n \"x\": 1\n```\n";
        let path = page_of(markdown);
        assert_eq!(path.extension().unwrap(), "html");
        let page = std::fs::read_to_string(&path).unwrap();
        assert_eq!(page.matches("<pre class=\"mermaid\">").count(), 2, "{page}");
        // A kind the terminal can't draw is the browser's to draw.
        assert!(page.contains("pie\n \"x\": 1\n</pre>"), "{page}");
        assert!(page.contains(MERMAID_MODULE), "{page}");
        assert!(!page.contains("# Two"), "{page}");
        // The same diagrams are the same file, and others another.
        assert_eq!(page_of(markdown), path);
        assert_ne!(page_of("flowchart LR\n a --> c\n"), path);
    }

    #[test]
    fn what_a_diagram_holds_is_never_taken_for_html() {
        let page = page(&["flowchart LR\n a[\"</pre><script>x()</script>\"] --> b".into()]);
        assert!(!page.contains("<script>x()"), "{page}");
        assert!(page.contains("&lt;/pre&gt;&lt;script&gt;x()"), "{page}");
    }

    #[test]
    fn a_page_is_opened_by_a_file_link() {
        assert_eq!(
            file_url(Path::new("/home/ann/my state/diagrams/0a1b.html")),
            "file:///home/ann/my%20state/diagrams/0a1b.html"
        );
    }
}
