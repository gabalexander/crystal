//! Markdown laid out as a page for one width: CommonMark with GitHub's
//! tables, strikethrough, task lists and alerts, as lines a terminal can
//! draw. It's what the file finder and the tree browser show for a `.md`
//! file, and what a background task's transcript makes of Claude's answers.
//!
//! The whole text is laid out up front for the width, so scrolling is a
//! slice and the number of lines is exact, and no line is wider than the
//! width. A terminal can do bold, italic, underlining, colors and box
//! drawing, and that's the whole vocabulary: headings bold in the accent
//! color with a rule under the first two levels, lists bulleted and
//! numbered again with hanging indents, quotes behind a bar, fenced code on
//! a surface of its own and highlighted, tables in aligned columns, links
//! underlined with their address muted beside them. An image is its alt
//! text in brackets, since there's no picture to show, and raw HTML stays
//! as written, muted.
//!
//! A ```` ```mermaid ```` fence is the one picture: the diagram is drawn
//! as boxes and arrows by [`crate::mermaid`], in the width left beside the
//! fence's indent, with a caption under it (`mermaid · sequence`). One that
//! isn't drawn (another kind, a line that can't be read, a layout too wide)
//! stays its source, highlighted as code, with why in the caption.
//!
//! Nothing here picks a color: each piece of a line says what it is, as a
//! [`Mark`], and whoever draws it, the TUI with its theme or a transcript
//! with a terminal's own colors, gives that a color.
//!
//! Adapted from docket's `markdown.rs`.

use crate::mermaid::{self, Glyphs, Rendered};
use crate::syntax::{Highlighter, TokenKind};
use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};
use ratatui::style::Modifier;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// What a piece of a page is, which is what color it's drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// Prose.
    Text,
    /// What's there to be found but not read first: a link's address, a
    /// caption, raw HTML, a quote.
    Muted,
    /// Headings, links, list markers and diagrams.
    Accent,
    /// Rules, a table's lines, and a quote's bar.
    Rule,
    /// Code in a line of prose.
    Code,
    /// A ticked box, and a tip.
    Done,
    /// A warning.
    Warning,
    /// A caution.
    Failed,
    /// Part of a line of highlighted code.
    Token(TokenKind),
}

/// How a piece of a page is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub ink: Ink,
    /// Bold, italic, underlined or crossed out.
    pub modifier: Modifier,
    /// Whether it's on a code block's surface.
    pub on_code: bool,
}

impl Mark {
    const fn new(ink: Ink) -> Mark {
        Mark {
            ink,
            modifier: Modifier::empty(),
            on_code: false,
        }
    }

    const fn on_code(ink: Ink) -> Mark {
        Mark {
            on_code: true,
            ..Mark::new(ink)
        }
    }

    fn with(self, modifier: Modifier) -> Mark {
        Mark {
            modifier: self.modifier | modifier,
            ..self
        }
    }

    fn ink(self, ink: Ink) -> Mark {
        Mark { ink, ..self }
    }
}

/// A run of a line's text, all drawn the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub text: String,
    pub mark: Mark,
}

impl Piece {
    fn new(text: impl Into<String>, mark: Mark) -> Piece {
        Piece {
            text: text.into(),
            mark,
        }
    }

    /// How many columns it takes.
    pub fn width(&self) -> usize {
        self.text.width()
    }
}

/// A line of a page.
pub type PageLine = Vec<Piece>;

/// Bullets by how deep a list is, going round again past the third.
const BULLETS: [&str; 3] = ["•", "◦", "▪"];
/// The bar a quote sits behind.
const QUOTE_BAR: &str = "▎ ";
/// The space on the left of a code block's surface.
const CODE_INSET: &str = " ";
/// The narrowest a table's column is squeezed to before the table is let
/// past the edge.
const NARROWEST_COLUMN: usize = 3;
/// What's between two of a table's columns.
const COLUMN_RULE: &str = " │ ";

/// Whether the file at `path` is shown as a page.
pub fn is_markdown_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    matches!(
        name.rsplit_once('.').map(|(_, extension)| extension),
        Some("md" | "markdown" | "mdown" | "mkd" | "mkdn")
    )
}

/// Whether a fence's info string says mermaid: its first word does.
fn is_mermaid(info: &str) -> bool {
    info.split_whitespace()
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("mermaid"))
}

/// The diagrams in `text`'s ```` ```mermaid ```` fences, in order.
pub fn mermaid_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut open: Option<String> = None;
    for event in Parser::new_ext(text, options()) {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) if is_mermaid(&info) => {
                open = Some(String::new());
            }
            Event::Text(text) => {
                if let Some(block) = &mut open {
                    block.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => blocks.extend(open.take()),
            _ => {}
        }
    }
    blocks
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
}

/// Lays `text` out as a page `width` columns wide.
pub fn render(text: &str, width: usize) -> Vec<PageLine> {
    let mut page = Page::new(width);
    for event in Parser::new_ext(text, options()) {
        page.event(event);
    }
    page.finish()
}

/// A piece of a paragraph before it's laid out in lines: a run of text in
/// one mark, and how it joins what came before it.
#[derive(Debug, Clone)]
struct Atom {
    text: String,
    mark: Mark,
    join: Join,
    /// An image's stand-in, `[alt]`, so a link that's only a badge can
    /// keep its address to itself.
    image: bool,
}

/// How an atom joins the one before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Join {
    /// A space came before it: a line can break here.
    Word,
    /// It's stuck to the one before: `**bo**ld` is one word.
    Glue,
    /// A line break.
    Break,
}

/// A line laid out, and how many columns it takes.
type Laid = (PageLine, usize);

/// One part of the margin every line of a page starts with.
#[derive(Debug, Clone)]
enum Margin {
    /// A quote's bar, in the quote's mark.
    Quote(Mark),
    /// A list item's hanging indent: as wide as its marker.
    Indent(usize),
}

#[derive(Debug, Clone)]
struct List {
    /// The number of an ordered list's next item; `None` for bullets.
    next: Option<u64>,
    /// Whether its items hold paragraphs, with blank lines between them,
    /// so the page has a blank line between them too.
    loose: bool,
}

/// A table being read: every cell keeps its atoms, to be laid out once the
/// columns' widths are known.
#[derive(Debug, Clone)]
struct Table {
    aligns: Vec<Alignment>,
    rows: Vec<Vec<Vec<Atom>>>,
    row: Vec<Vec<Atom>>,
    /// How many of the first rows are the header: one, or none.
    head_rows: usize,
}

/// A page being laid out, from pulldown-cmark's events.
struct Page {
    width: usize,
    out: Vec<PageLine>,
    /// The margin, innermost last.
    margin: Vec<Margin>,
    /// A list item's marker, drawn in place of its indent on the item's
    /// first line only.
    marker: Option<PageLine>,
    /// The paragraph, heading or cell being read.
    inline: Vec<Atom>,
    /// A space came since the last atom: the next one is a word.
    space: bool,
    bold: usize,
    italic: usize,
    strike: usize,
    link: usize,
    heading: Option<HeadingLevel>,
    table_head: bool,
    lists: Vec<List>,
    /// The block before has ended: the next line gets a blank one above.
    need_blank: bool,
    /// A code block being read: its info string and text.
    code: Option<(String, String)>,
    /// Inside an HTML or front matter block, shown as written.
    raw_block: bool,
    table: Option<Table>,
    /// Open links: their address, and where in `inline` their text starts.
    links: Vec<(String, usize)>,
    /// Open images: where in `inline` their alt text starts.
    images: Vec<usize>,
}

impl Page {
    fn new(width: usize) -> Page {
        Page {
            width: width.max(1),
            out: Vec::new(),
            margin: Vec::new(),
            marker: None,
            inline: Vec::new(),
            space: false,
            bold: 0,
            italic: 0,
            strike: 0,
            link: 0,
            heading: None,
            table_head: false,
            lists: Vec::new(),
            need_blank: false,
            code: None,
            raw_block: false,
            table: None,
            links: Vec::new(),
            images: Vec::new(),
        }
    }

    fn finish(mut self) -> Vec<PageLine> {
        self.flush_inline();
        while self
            .out
            .last()
            .is_some_and(|line| line.iter().all(|piece| piece.text.trim().is_empty()))
        {
            self.out.pop();
        }
        self.out
    }

    /// The mark prose takes here, from the tags it's in.
    fn mark(&self) -> Mark {
        let mut mark = Mark::new(Ink::Text);
        if self.margin.iter().any(|m| matches!(m, Margin::Quote(_))) {
            mark = mark.ink(Ink::Muted);
        }
        if let Some(level) = self.heading {
            mark = mark.with(Modifier::BOLD);
            if level <= HeadingLevel::H3 {
                mark = mark.ink(Ink::Accent);
            }
        }
        if self.table_head || self.bold > 0 {
            mark = mark.with(Modifier::BOLD);
        }
        if self.italic > 0 {
            mark = mark.with(Modifier::ITALIC);
        }
        if self.strike > 0 {
            mark = mark.with(Modifier::CROSSED_OUT);
        }
        if self.link > 0 {
            mark = mark.ink(Ink::Accent).with(Modifier::UNDERLINED);
        }
        mark
    }

    fn margin_width(&self) -> usize {
        self.margin
            .iter()
            .map(|margin| match margin {
                Margin::Quote(_) => QUOTE_BAR.width(),
                Margin::Indent(width) => *width,
            })
            .sum()
    }

    /// The columns left for what's in the margin.
    fn room(&self) -> usize {
        self.width.saturating_sub(self.margin_width()).max(1)
    }

    /// The margin as pieces; with `with_marker`, a list item's marker
    /// waiting to be drawn takes its indent's place, once.
    fn margin_pieces(&mut self, with_marker: bool) -> PageLine {
        let marker_at = if with_marker && self.marker.is_some() {
            self.margin
                .iter()
                .rposition(|m| matches!(m, Margin::Indent(_)))
        } else {
            None
        };
        let mut pieces = Vec::with_capacity(self.margin.len());
        for (index, margin) in self.margin.iter().enumerate() {
            match margin {
                Margin::Quote(mark) => pieces.push(Piece::new(QUOTE_BAR, *mark)),
                Margin::Indent(_) if Some(index) == marker_at => {
                    pieces.extend(self.marker.take().unwrap_or_default());
                }
                Margin::Indent(width) => {
                    pieces.push(Piece::new(" ".repeat(*width), Mark::new(Ink::Text)))
                }
            }
        }
        pieces
    }

    /// The blank line the block before asked for, if any, but never as the
    /// page's first line.
    fn settle_blank(&mut self) {
        if self.need_blank && !self.out.is_empty() {
            self.blank();
        }
        self.need_blank = false;
    }

    /// A line, behind the margin, after the blank line the block before
    /// asked for.
    fn emit(&mut self, content: PageLine) {
        self.settle_blank();
        let mut pieces = self.margin_pieces(true);
        pieces.extend(content);
        self.out.push(fit(pieces, self.width));
    }

    /// A blank line, still behind a quote's bar, so a quote reads as one
    /// block across its paragraphs.
    fn blank(&mut self) {
        let pieces = self.margin_pieces(false);
        self.out.push(fit(pieces, self.width));
    }

    fn push_text(&mut self, text: &str, mark: Mark) {
        let text = text.replace('\t', "    ");
        for (index, word) in text.split(' ').enumerate() {
            if index > 0 {
                self.space = true;
            }
            if word.is_empty() {
                continue;
            }
            let line_starts = matches!(
                self.inline.last(),
                None | Some(Atom {
                    join: Join::Break,
                    ..
                })
            );
            let join = if self.space || line_starts {
                Join::Word
            } else {
                Join::Glue
            };
            self.inline.push(Atom {
                text: word.to_string(),
                mark,
                join,
                image: false,
            });
            self.space = false;
        }
    }

    fn push_break(&mut self) {
        self.inline.push(Atom {
            text: String::new(),
            mark: Mark::new(Ink::Text),
            join: Join::Break,
            image: false,
        });
        self.space = false;
    }

    /// The plain text of the atoms from `start` on.
    fn inline_text(&self, start: usize) -> String {
        let mut text = String::new();
        for (index, atom) in self.inline.iter().skip(start).enumerate() {
            match atom.join {
                Join::Word if index > 0 => text.push(' '),
                Join::Break => text.push(' '),
                _ => {}
            }
            text.push_str(&atom.text);
        }
        text
    }

    /// Lays the paragraph read so far out in lines, in the room beside the
    /// margin. Returns how wide its widest line is.
    fn flush_inline(&mut self) -> usize {
        let atoms = std::mem::take(&mut self.inline);
        self.space = false;
        if atoms.is_empty() {
            return 0;
        }
        let lines = lay_out(&atoms, self.room());
        let widest = lines.iter().map(|(_, width)| *width).max().unwrap_or(0);
        for (line, _) in lines {
            self.emit(line);
        }
        widest
    }

    /// A code block: every line on the surface, highlighted by the fence's
    /// language, broken at the edge rather than wrapped (code has no words
    /// to wrap on), and not numbered.
    fn code_block(&mut self, info: &str, text: &str) {
        let room = self.room();
        let inner = room.saturating_sub(CODE_INSET.width()).max(1);
        let surface = Mark::on_code(Ink::Text);
        let mut highlighter = if info.trim().is_empty() {
            Highlighter::plain()
        } else {
            Highlighter::for_fence(info)
        };
        let text = text.replace('\t', "    ");
        let mut lines: Vec<&str> = text.lines().collect();
        while lines.last().is_some_and(|line| line.trim().is_empty()) {
            lines.pop();
        }
        for line in lines {
            let runs: Vec<(String, Mark)> = highlighter
                .line(line)
                .into_iter()
                .map(|(kind, text)| (text, Mark::on_code(Ink::Token(kind))))
                .collect();
            for chunk in break_at_edge(&runs, inner) {
                let used: usize = chunk.iter().map(Piece::width).sum();
                let mut pieces = vec![Piece::new(CODE_INSET, surface)];
                pieces.extend(chunk);
                let rest = room.saturating_sub(CODE_INSET.width() + used);
                if rest > 0 {
                    pieces.push(Piece::new(" ".repeat(rest), surface));
                }
                self.emit(pieces);
            }
        }
    }

    /// A mermaid fence: the diagram drawn in the room beside the margin,
    /// with its caption under it; or, when it isn't drawn, its source as
    /// any other code block, with why for the caption.
    fn mermaid_block(&mut self, text: &str) {
        let muted = Mark::new(Ink::Muted);
        match mermaid::render(text, self.room(), Glyphs::BOX_DRAWING) {
            Rendered::Diagram { lines, kind } => {
                for line in lines {
                    self.emit(vec![Piece::new(line, Mark::new(Ink::Accent))]);
                }
                self.caption(&format!("mermaid · {}", kind.label()), muted);
            }
            Rendered::Unsupported { reason } => {
                self.code_block("mermaid", text);
                self.caption(&format!("mermaid · not drawn: {reason}"), muted);
            }
        }
    }

    /// A line under a block, wrapped on its words like prose.
    fn caption(&mut self, text: &str, mark: Mark) {
        self.flush_inline();
        self.push_text(text, mark);
        self.flush_inline();
    }

    /// Raw HTML or front matter: as written, muted, each line of it a line
    /// of its own, wrapped on its words when it's too wide.
    fn raw_lines(&mut self, text: &str) {
        let muted = Mark::new(Ink::Muted);
        for line in text.lines() {
            self.push_text(line, muted);
            self.push_break();
        }
        self.flush_inline();
    }

    /// A table: its columns sized the way a browser sizes them (see
    /// [`column_widths`]), every cell wrapped to its column so nothing is
    /// cut, a rule under the header, and no header at all when its cells
    /// are all blank: the `| | |` that only wanted the rule.
    fn table_lines(&mut self, table: Table) {
        let columns = table
            .aligns
            .len()
            .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
        if columns == 0 || table.rows.is_empty() {
            return;
        }
        let mut natural = vec![1; columns];
        let mut least = vec![1; columns];
        for row in &table.rows {
            for (column, cell) in row.iter().enumerate() {
                natural[column] = natural[column].max(natural_width(cell));
                least[column] = least[column].max(longest_word(cell));
            }
        }
        let rules = COLUMN_RULE.width() * (columns - 1);
        let widths = column_widths(&natural, &least, self.room().saturating_sub(rules));

        let mut rows = table.rows;
        let mut head_rows = table.head_rows;
        let blank_head = head_rows == 1
            && rows[0]
                .iter()
                .all(|cell| cell.iter().all(|atom| atom.text.trim().is_empty()));
        if blank_head {
            rows.remove(0);
            head_rows = 0;
        }
        let rule = Mark::new(Ink::Rule);
        for (index, row) in rows.iter().enumerate() {
            let cells: Vec<Vec<Laid>> = widths
                .iter()
                .enumerate()
                .map(|(column, width)| {
                    let atoms = row.get(column).map(Vec::as_slice).unwrap_or(&[]);
                    lay_out(atoms, *width)
                })
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(0).max(1);
            for line in 0..height {
                let mut pieces = Vec::new();
                for (column, cell) in cells.iter().enumerate() {
                    if column > 0 {
                        pieces.push(Piece::new(COLUMN_RULE, rule));
                    }
                    let (content, used) = cell.get(line).cloned().unwrap_or_default();
                    let align = table.aligns.get(column).copied().unwrap_or(Alignment::None);
                    pieces.extend(aligned(content, used, widths[column], align));
                }
                self.emit(pieces);
            }
            if index + 1 == head_rows {
                let under: Vec<String> = widths.iter().map(|width| "─".repeat(*width)).collect();
                self.emit(vec![Piece::new(under.join("─┼─"), rule)]);
            }
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if let Some((_, code)) = &mut self.code {
                    code.push_str(&text);
                } else if self.raw_block {
                    self.raw_lines(&text);
                } else {
                    self.push_text(&text, self.mark());
                }
            }
            Event::Code(code) => self.push_text(&code, self.mark().ink(Ink::Code)),
            Event::Html(html) => self.raw_lines(&html),
            Event::InlineHtml(html) => {
                if html.trim_start().to_ascii_lowercase().starts_with("<br") {
                    self.push_break();
                } else {
                    self.push_text(&html, Mark::new(Ink::Muted));
                }
            }
            Event::SoftBreak => self.space = true,
            Event::HardBreak => self.push_break(),
            Event::Rule => {
                self.flush_inline();
                self.need_blank = true;
                let rule = "─".repeat(self.room());
                self.emit(vec![Piece::new(rule, Mark::new(Ink::Rule))]);
                self.need_blank = true;
            }
            Event::TaskListMarker(done) => {
                let (glyph, ink) = if done {
                    ("☑", Ink::Done)
                } else {
                    ("☐", Ink::Muted)
                };
                self.push_text(glyph, Mark::new(ink));
                self.space = true;
            }
            Event::FootnoteReference(label) => {
                self.push_text(&format!("[^{label}]"), Mark::new(Ink::Muted));
            }
            Event::InlineMath(_) | Event::DisplayMath(_) => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if let Some(list) = self.lists.last_mut() {
                    list.loose = true;
                }
            }
            Tag::Heading { level, .. } => {
                self.flush_inline();
                self.need_blank = true;
                self.heading = Some(level);
            }
            Tag::BlockQuote(kind) => self.start_quote(kind),
            Tag::CodeBlock(kind) => {
                self.flush_inline();
                self.need_blank = true;
                let info = match kind {
                    CodeBlockKind::Fenced(info) => info.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((info, String::new()));
            }
            Tag::HtmlBlock | Tag::MetadataBlock(_) => {
                self.flush_inline();
                self.need_blank = true;
                self.raw_block = true;
            }
            Tag::List(start) => {
                self.flush_inline();
                if self.lists.is_empty() {
                    self.need_blank = true;
                }
                self.lists.push(List {
                    next: start,
                    loose: false,
                });
            }
            Tag::Item => self.start_item(),
            Tag::Table(aligns) => {
                self.flush_inline();
                self.need_blank = true;
                self.table = Some(Table {
                    aligns,
                    rows: Vec::new(),
                    row: Vec::new(),
                    head_rows: 0,
                });
            }
            Tag::TableHead => self.table_head = true,
            Tag::TableCell => {
                self.inline.clear();
                self.space = false;
            }
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } => {
                self.link += 1;
                self.links.push((dest_url.to_string(), self.inline.len()));
            }
            Tag::Image { .. } => self.images.push(self.inline.len()),
            Tag::TableRow
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript => {}
        }
    }

    /// A quote starts: its bar from its first line, and an alert's label,
    /// in the alert's color.
    fn start_quote(&mut self, kind: Option<BlockQuoteKind>) {
        self.flush_inline();
        // The blank line above a quote is the page's, not the quote's: the
        // bar starts on the quote's first line.
        self.need_blank = true;
        self.settle_blank();
        let (label, ink) = match kind {
            None => (None, Ink::Rule),
            Some(BlockQuoteKind::Note) => (Some("Note"), Ink::Accent),
            Some(BlockQuoteKind::Tip) => (Some("Tip"), Ink::Done),
            Some(BlockQuoteKind::Important) => (Some("Important"), Ink::Accent),
            Some(BlockQuoteKind::Warning) => (Some("Warning"), Ink::Warning),
            Some(BlockQuoteKind::Caution) => (Some("Caution"), Ink::Failed),
        };
        self.margin.push(Margin::Quote(Mark::new(ink)));
        if let Some(label) = label {
            self.emit(vec![Piece::new(label, Mark::new(ink).with(Modifier::BOLD))]);
        }
    }

    /// A list item starts: its marker waits to take the place of its
    /// indent on its first line.
    fn start_item(&mut self) {
        self.flush_inline();
        // An item that starts straight away with a list inside it: its own
        // marker gets a line of its own rather than being lost.
        if self.marker.is_some() {
            self.emit(Vec::new());
        }
        let depth = self.lists.len().saturating_sub(1);
        let marker = match self.lists.last_mut() {
            Some(List {
                next: Some(number), ..
            }) => {
                let marker = format!("{number}. ");
                *number += 1;
                marker
            }
            _ => format!("{} ", BULLETS[depth % BULLETS.len()]),
        };
        let width = marker.width();
        self.marker = Some(vec![Piece::new(marker, Mark::new(Ink::Accent))]);
        self.margin.push(Margin::Indent(width));
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_inline();
                self.need_blank = true;
            }
            TagEnd::Heading(level) => {
                let width = self.flush_inline();
                self.heading = None;
                let rule = match level {
                    HeadingLevel::H1 => Some("━"),
                    HeadingLevel::H2 => Some("─"),
                    _ => None,
                };
                if let Some(rule) = rule
                    && width > 0
                {
                    let rule = rule.repeat(width.min(self.room()));
                    self.emit(vec![Piece::new(rule, Mark::new(Ink::Rule))]);
                }
                self.need_blank = true;
            }
            TagEnd::BlockQuote(_) => {
                self.flush_inline();
                self.margin.pop();
                self.need_blank = true;
            }
            TagEnd::CodeBlock => {
                let (info, text) = self.code.take().unwrap_or_default();
                if is_mermaid(&info) {
                    self.mermaid_block(&text);
                } else {
                    self.code_block(&info, &text);
                }
                self.need_blank = true;
            }
            TagEnd::HtmlBlock | TagEnd::MetadataBlock(_) => {
                self.raw_block = false;
                self.need_blank = true;
            }
            TagEnd::List(_) => {
                self.lists.pop();
                if self.lists.is_empty() {
                    self.need_blank = true;
                }
            }
            TagEnd::Item => {
                self.flush_inline();
                // An empty item still shows its marker.
                if self.marker.is_some() {
                    self.emit(Vec::new());
                }
                self.margin.pop();
                if self.lists.last().is_some_and(|list| list.loose) {
                    self.need_blank = true;
                }
            }
            TagEnd::Table => {
                self.table_head = false;
                if let Some(table) = self.table.take() {
                    self.table_lines(table);
                }
                self.need_blank = true;
            }
            TagEnd::TableHead => {
                self.table_head = false;
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push(row);
                    table.head_rows = 1;
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push(row);
                }
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.inline);
                self.space = false;
                if let Some(table) = &mut self.table {
                    table.row.push(cell);
                }
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => self.end_link(),
            TagEnd::Image => self.end_image(),
            TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript => {}
        }
    }

    /// A link ends: its address follows its text, muted, unless the text
    /// says it already or the link is only a badge, an image to be seen
    /// rather than followed.
    fn end_link(&mut self) {
        self.link = self.link.saturating_sub(1);
        let Some((address, start)) = self.links.pop() else {
            return;
        };
        let text = self.inline_text(start);
        let badge = start < self.inline.len() && self.inline[start..].iter().all(|atom| atom.image);
        if !badge && shows_address(&address, &text) {
            self.space = true;
            self.push_text(&format!("({address})"), Mark::new(Ink::Muted));
        }
    }

    /// An image ends: its alt text, in brackets, stands in for it.
    fn end_image(&mut self) {
        let Some(start) = self.images.pop() else {
            return;
        };
        let alt = self.inline_text(start);
        let join = self.inline.get(start).map_or(Join::Word, |atom| atom.join);
        self.inline.truncate(start);
        let alt = alt.trim();
        let text = if alt.is_empty() {
            "[image]".to_string()
        } else {
            format!("[{alt}]")
        };
        self.inline.push(Atom {
            text,
            mark: Mark::new(Ink::Muted),
            join,
            image: true,
        });
    }
}

/// Whether a link's address is worth showing beside its text: not when the
/// text is the address, and not for a place on the same page.
fn shows_address(address: &str, text: &str) -> bool {
    let address = address.trim();
    if address.is_empty() || address.starts_with('#') {
        return false;
    }
    let bare = address.strip_prefix("mailto:").unwrap_or(address);
    let same = |a: &str, b: &str| a.trim_end_matches('/') == b.trim_end_matches('/');
    !same(bare, text.trim()) && !same(address, text.trim())
}

/// Adds `text` to a line, to its last piece when that's drawn the same
/// way, so a link or a bold phrase is one piece rather than one a word.
fn push_run(line: &mut PageLine, text: &str, mark: Mark) {
    match line.last_mut() {
        Some(last) if last.mark == mark => last.text.push_str(text),
        _ => line.push(Piece::new(text, mark)),
    }
}

fn push_char(line: &mut PageLine, c: char, mark: Mark) {
    push_run(line, c.encode_utf8(&mut [0; 4]), mark);
}

/// Runs cut into lines of at most `width` columns at any character: for
/// code, which has no gaps between words to break at. Always at least one
/// line, so an empty line stays a line.
fn break_at_edge(runs: &[(String, Mark)], width: usize) -> Vec<PageLine> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = Vec::new();
    let mut used = 0;
    for (text, mark) in runs {
        for c in text.chars() {
            let columns = c.width().unwrap_or(0);
            if used > 0 && used + columns > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            push_char(&mut line, c, *mark);
            used += columns;
        }
    }
    lines.push(line);
    lines
}

/// Atoms laid out in lines no wider than `room`, broken between words, and
/// at the edge for a word that can never fit. A space between two words
/// marked the same keeps their mark (a link stays underlined across it);
/// any other space is plain. A line break ends a line, even an empty one.
fn lay_out(atoms: &[Atom], room: usize) -> Vec<Laid> {
    let room = room.max(1);
    let mut lines = Vec::new();
    let mut line = Vec::new();
    let mut used = 0;
    let mut start = 0;
    while start < atoms.len() {
        if atoms[start].join == Join::Break {
            lines.push((std::mem::take(&mut line), used));
            used = 0;
            start += 1;
            continue;
        }
        let end = atoms[start + 1..]
            .iter()
            .position(|atom| atom.join != Join::Glue)
            .map_or(atoms.len(), |length| start + 1 + length);
        let word = &atoms[start..end];
        let width: usize = word.iter().map(|atom| atom.text.width()).sum();
        if used > 0 && used + 1 + width > room {
            lines.push((std::mem::take(&mut line), used));
            used = 0;
        }
        if used > 0 {
            let before = line.last().map(|piece: &Piece| piece.mark);
            let gap = match word.first() {
                Some(atom) if before == Some(atom.mark) => atom.mark,
                _ => Mark::new(Ink::Text),
            };
            push_run(&mut line, " ", gap);
            used += 1;
        }
        if width > room {
            for atom in word {
                for c in atom.text.chars() {
                    let columns = c.width().unwrap_or(0);
                    if used > 0 && used + columns > room {
                        lines.push((std::mem::take(&mut line), used));
                        used = 0;
                    }
                    push_char(&mut line, c, atom.mark);
                    used += columns;
                }
            }
        } else {
            for atom in word {
                push_run(&mut line, &atom.text, atom.mark);
            }
            used += width;
        }
        start = end;
    }
    if !line.is_empty() {
        lines.push((line, used));
    }
    lines
}

/// The widest word in `atoms`: what a table's column can't be narrower
/// than without breaking words.
fn longest_word(atoms: &[Atom]) -> usize {
    let mut widest = 0;
    let mut word = 0;
    for atom in atoms {
        match atom.join {
            Join::Glue => word += atom.text.width(),
            Join::Word => {
                widest = widest.max(word);
                word = atom.text.width();
            }
            Join::Break => {
                widest = widest.max(word);
                word = 0;
            }
        }
    }
    widest.max(word)
}

/// How wide `atoms` are with no wrapping at all: the widest of the lines
/// their line breaks make.
fn natural_width(atoms: &[Atom]) -> usize {
    lay_out(atoms, usize::MAX)
        .iter()
        .map(|(_, width)| *width)
        .max()
        .unwrap_or(0)
}

/// The widths of a table's columns in `room` columns, given how wide each
/// would like to be (`natural`, its widest cell unwrapped) and the least it
/// can be without breaking a word (`least`, its longest word): the way a
/// browser lays a table out. Everything each wants when that fits; or
/// else every column its longest word at least, the rest shared by what
/// each wanted beyond that; and when even the longest words don't fit, the
/// widest columns give way first, down to a floor, and words break at the
/// edge.
fn column_widths(natural: &[usize], least: &[usize], room: usize) -> Vec<usize> {
    let wanted: usize = natural.iter().sum();
    if wanted <= room {
        return natural.to_vec();
    }
    let least: Vec<usize> = natural
        .iter()
        .zip(least)
        .map(|(natural, least)| (*least).min(*natural).max(1))
        .collect();
    let needed: usize = least.iter().sum();
    if needed > room {
        return give_way(least, room);
    }
    let spare = room - needed;
    let beyond: usize = natural.iter().zip(&least).map(|(n, l)| n - l).sum();
    let mut widths: Vec<usize> = natural
        .iter()
        .zip(&least)
        .map(|(natural, least)| match beyond {
            0 => *least,
            _ => least + spare * (natural - least) / beyond,
        })
        .collect();
    // Shared out whole, a column or two is left over: the columns that
    // wanted the most take one each.
    let mut left = room - widths.iter().sum::<usize>();
    let mut most_wanted: Vec<usize> = (0..widths.len()).collect();
    most_wanted.sort_by_key(|&column| std::cmp::Reverse(natural[column]));
    for column in most_wanted {
        if left == 0 {
            break;
        }
        if widths[column] < natural[column] {
            widths[column] += 1;
            left -= 1;
        }
    }
    widths
}

/// Columns too wide for `room` even at their longest words: the widest
/// gives way a column at a time, but none goes below the floor.
fn give_way(mut widths: Vec<usize>, room: usize) -> Vec<usize> {
    let mut total: usize = widths.iter().sum();
    while total > room {
        let widest = widths
            .iter()
            .enumerate()
            .filter(|(_, width)| **width > NARROWEST_COLUMN)
            .max_by_key(|(_, width)| **width);
        let Some((column, _)) = widest else {
            break;
        };
        widths[column] -= 1;
        total -= 1;
    }
    widths
}

/// A line of a table's cell, padded out to its column's width the way the
/// column is aligned.
fn aligned(mut pieces: PageLine, used: usize, width: usize, align: Alignment) -> PageLine {
    let pad = width.saturating_sub(used);
    let (left, right) = match align {
        Alignment::Right => (pad, 0),
        Alignment::Center => (pad / 2, pad - pad / 2),
        Alignment::Left | Alignment::None => (0, pad),
    };
    let text = Mark::new(Ink::Text);
    if left > 0 {
        pieces.insert(0, Piece::new(" ".repeat(left), text));
    }
    if right > 0 {
        pieces.push(Piece::new(" ".repeat(right), text));
    }
    pieces
}

/// The line cut to `width` columns: whatever was laid out above, nothing
/// wider than the page leaves here.
fn fit(pieces: PageLine, width: usize) -> PageLine {
    let total: usize = pieces.iter().map(Piece::width).sum();
    if total <= width {
        return pieces;
    }
    let mut kept = Vec::new();
    let mut used = 0;
    for piece in pieces {
        for c in piece.text.chars() {
            let columns = c.width().unwrap_or(0);
            if used + columns > width {
                return kept;
            }
            push_char(&mut kept, c, piece.mark);
            used += columns;
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str, width: usize) -> Vec<PageLine> {
        render(text, width)
    }

    fn plain(lines: &[PageLine]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                let text: String = line.iter().map(|piece| piece.text.as_str()).collect();
                text.trim_end().to_string()
            })
            .collect()
    }

    fn width(line: &PageLine) -> usize {
        line.iter().map(Piece::width).sum()
    }

    /// The first piece with `text` in it.
    #[track_caller]
    fn piece<'a>(lines: &'a [PageLine], text: &str) -> &'a Piece {
        lines
            .iter()
            .flatten()
            .find(|piece| piece.text.contains(text))
            .unwrap_or_else(|| panic!("no {text:?} in {:?}", plain(lines)))
    }

    #[track_caller]
    fn ink(lines: &[PageLine], text: &str) -> Ink {
        piece(lines, text).mark.ink
    }

    #[track_caller]
    fn has(lines: &[PageLine], text: &str, modifier: Modifier) -> bool {
        piece(lines, text).mark.modifier.contains(modifier)
    }

    #[test]
    fn markdown_files_are_known_by_their_extension() {
        assert!(is_markdown_path("docs/keys.md"));
        assert!(is_markdown_path("/abs/README.MD"));
        assert!(is_markdown_path("notes.markdown"));
        assert!(!is_markdown_path("src/main.rs"));
        assert!(!is_markdown_path("README"));
        assert!(!is_markdown_path("md"));
    }

    #[test]
    fn headings_are_bold_in_the_accent_with_a_rule_under_the_first_two_levels() {
        let out = lines(
            "# Title\n\nbody\n\n## Section\n\n### Sub\n\n#### Minor\n",
            40,
        );
        assert_eq!(
            plain(&out),
            [
                "Title",
                "━━━━━",
                "",
                "body",
                "",
                "Section",
                "───────",
                "",
                "Sub",
                "",
                "Minor"
            ]
        );
        assert_eq!(ink(&out, "Title"), Ink::Accent);
        assert!(has(&out, "Title", Modifier::BOLD));
        assert_eq!(ink(&out, "Minor"), Ink::Text, "H4 is bold, not accent");
        assert!(has(&out, "Minor", Modifier::BOLD));
        assert_eq!(ink(&out, "━"), Ink::Rule);
    }

    #[test]
    fn paragraphs_wrap_on_words() {
        let text = "one two three\nfour five six seven";
        assert_eq!(
            plain(&lines(text, 12)),
            ["one two", "three four", "five six", "seven"]
        );
        // Two spaces at the end of a line break it.
        assert_eq!(plain(&lines("a  \nb", 40)), ["a", "b"]);
        // Paragraphs stay apart; runs of spaces are one.
        assert_eq!(plain(&lines("a\n\nb   c", 40)), ["a", "", "b c"]);
    }

    #[test]
    fn a_word_wider_than_the_page_breaks_at_the_edge() {
        let out = lines("see https://example.dev/a/very/long/path/indeed now", 12);
        assert_eq!(
            plain(&out),
            [
                "see",
                "https://exam",
                "ple.dev/a/ve",
                "ry/long/path",
                "/indeed now"
            ]
        );
    }

    #[test]
    fn emphasis_is_a_modifier_and_code_has_its_own_color() {
        let out = lines("**bold** *it* ~~gone~~ `code` **bo**ld", 40);
        assert_eq!(plain(&out), ["bold it gone code bold"]);
        assert!(has(&out, "bold", Modifier::BOLD));
        assert!(has(&out, "it", Modifier::ITALIC));
        assert!(has(&out, "gone", Modifier::CROSSED_OUT));
        assert_eq!(ink(&out, "code"), Ink::Code);
        // `**bo**ld` is one word: its two pieces are together on one line.
        let texts: Vec<&str> = out[0].iter().map(|piece| piece.text.as_str()).collect();
        assert!(texts.ends_with(&["bo", "ld"]), "{texts:?}");
    }

    #[test]
    fn lists_get_bullets_numbers_and_hanging_indents() {
        let text =
            "- alpha beta gamma delta\n- two\n  - nested item\n    - deeper\n3. three\n4. four\n";
        let out = lines(text, 16);
        assert_eq!(
            plain(&out),
            [
                "• alpha beta",
                "  gamma delta",
                "• two",
                "  ◦ nested item",
                "    ▪ deeper",
                "",
                "3. three",
                "4. four",
            ]
        );
        assert_eq!(ink(&out, "•"), Ink::Accent);
        assert_eq!(ink(&out, "3. "), Ink::Accent);
    }

    #[test]
    fn loose_lists_keep_a_blank_line_between_items_and_tight_ones_dont() {
        assert_eq!(plain(&lines("- a\n- b\n", 20)), ["• a", "• b"]);
        assert_eq!(
            plain(&lines("- a\n\n- b\n\n  second para\n", 20)),
            ["• a", "", "• b", "", "  second para"]
        );
        // A paragraph before a list gets its blank line; the items don't.
        assert_eq!(
            plain(&lines("intro\n- a\n- b\n\nafter", 20)),
            ["intro", "", "• a", "• b", "", "after"]
        );
    }

    #[test]
    fn task_lists_show_their_boxes() {
        let out = lines("- [x] done\n- [ ] todo\n", 20);
        assert_eq!(plain(&out), ["• ☑ done", "• ☐ todo"]);
        assert_eq!(ink(&out, "☑"), Ink::Done);
        assert_eq!(ink(&out, "☐"), Ink::Muted);
    }

    #[test]
    fn fenced_code_is_on_its_surface_highlighted_and_broken_at_the_edge() {
        let text = "before\n\n```rust\nfn main() {\n    let x = \"a very long string literal here\";\n}\n```\n\nafter\n";
        let out = lines(text, 24);
        assert_eq!(
            plain(&out),
            [
                "before",
                "",
                " fn main() {",
                "     let x = \"a very lon",
                " g string literal here\";",
                " }",
                "",
                "after",
            ]
        );
        for line in &out[2..6] {
            assert!(
                line.iter().all(|piece| piece.mark.on_code),
                "every piece of a code line is on the surface: {line:?}"
            );
            assert_eq!(width(line), 24, "code lines fill the width: {line:?}");
        }
        assert_eq!(ink(&out, "fn"), Ink::Token(TokenKind::Keyword));
        assert_eq!(ink(&out, "\"a very"), Ink::Token(TokenKind::String));
        assert!(!piece(&out, "before").mark.on_code);
    }

    #[test]
    fn an_indented_or_unnamed_block_is_plain_on_its_surface() {
        assert_eq!(plain(&lines("```\nplain text\n```\n", 20)), [" plain text"]);
        let out = lines("    four spaces\n", 20);
        assert_eq!(plain(&out), [" four spaces"]);
        assert_eq!(ink(&out, "four"), Ink::Token(TokenKind::Text));
    }

    #[test]
    fn quotes_sit_behind_a_bar_and_alerts_get_a_label() {
        let out = lines("> quoted words\n> go on\n>\n> second\n\nplain\n", 18);
        assert_eq!(
            plain(&out),
            ["▎ quoted words go", "▎ on", "▎", "▎ second", "", "plain"]
        );
        assert_eq!(ink(&out, "▎"), Ink::Rule);
        assert_eq!(ink(&out, "quoted"), Ink::Muted);
        assert_eq!(ink(&out, "plain"), Ink::Text);

        let out = lines("> [!WARNING]\n> careful now\n", 30);
        assert_eq!(plain(&out), ["▎ Warning", "▎ careful now"]);
        assert_eq!(ink(&out, "Warning"), Ink::Warning);
        assert_eq!(ink(&out, "▎"), Ink::Warning);
    }

    #[test]
    fn a_list_in_a_quote_and_a_quote_in_a_list_keep_both_margins() {
        assert_eq!(plain(&lines("> - a\n> - b\n", 20)), ["▎ • a", "▎ • b"]);
        assert_eq!(
            plain(&lines("- item\n\n  > quoted\n", 20)),
            ["• item", "", "  ▎ quoted"]
        );
    }

    #[test]
    fn rules_are_as_wide_as_the_room() {
        assert_eq!(
            plain(&lines("a\n\n---\n\nb\n", 10)),
            ["a", "", "──────────", "", "b"]
        );
        assert_eq!(
            plain(&lines("- x\n\n  ---\n", 10)),
            ["• x", "", "  ────────"]
        );
    }

    #[test]
    fn tables_align_their_columns_with_a_rule_under_the_header() {
        let text =
            "| Key | Value | N |\n|:----|:-----:|--:|\n| a | middle | 1 |\n| longer | b | 22 |\n";
        let out = lines(text, 40);
        assert_eq!(
            plain(&out),
            [
                "Key    │ Value  │  N",
                "───────┼────────┼───",
                "a      │ middle │  1",
                "longer │   b    │ 22",
            ]
        );
        assert!(has(&out, "Key", Modifier::BOLD));
        assert_eq!(ink(&out, "│"), Ink::Rule);
        assert_eq!(piece(&out, "middle").mark.modifier, Modifier::empty());
    }

    #[test]
    fn a_wide_table_wraps_its_cells_rather_than_cut_them() {
        let text = "| a | b |\n|---|---|\n| short | a much longer cell than fits |\n";
        let out = lines(text, 20);
        assert_eq!(
            plain(&out),
            [
                "a     │ b",
                "──────┼─────────────",
                "short │ a much",
                "      │ longer cell",
                "      │ than fits",
            ]
        );
        assert!(out.iter().all(|line| width(line) <= 20));
    }

    #[test]
    fn a_blank_header_row_goes_with_its_rule() {
        assert_eq!(
            plain(&lines("| | |\n|---|---|\n| a | b |\n", 20)),
            ["a │ b"]
        );
    }

    #[test]
    fn columns_get_what_they_want_then_their_longest_word_then_the_floor() {
        assert_eq!(column_widths(&[5, 10], &[3, 4], 30), [5, 10], "it all fits");
        // 40 wanted and 20 there: each column its longest word at least
        // (6 + 4 = 10), and the other 10 shared by what each wanted beyond.
        assert_eq!(column_widths(&[36, 4], &[6, 4], 20), [16, 4]);
        // What sharing out whole leaves over goes to the one that wanted
        // most.
        assert_eq!(column_widths(&[10, 10, 10], &[2, 2, 2], 13), [5, 4, 4]);
        // Even the longest words don't fit: the widest give way, evenly.
        assert_eq!(column_widths(&[20, 20], &[20, 20], 10), [5, 5]);
        assert_eq!(column_widths(&[20, 8], &[20, 8], 12), [6, 6]);
        assert_eq!(
            column_widths(&[20, 20], &[20, 20], 2),
            [3, 3],
            "the floor holds: the line is cut rather than a column lost"
        );
    }

    #[test]
    fn links_show_their_address_muted_unless_its_the_text_or_on_the_page() {
        let out = lines(
            "see [the docs](https://example.dev/docs) and <https://x.y> and [top](#top)",
            80,
        );
        assert_eq!(
            plain(&out),
            ["see the docs (https://example.dev/docs) and https://x.y and top"]
        );
        assert_eq!(ink(&out, "the docs"), Ink::Accent);
        assert!(has(&out, "the docs", Modifier::UNDERLINED));
        assert_eq!(ink(&out, "(https://example.dev/docs)"), Ink::Muted);
        assert!(!shows_address("mailto:a@b.c", "a@b.c"));
        assert!(!shows_address("https://a.b/", "https://a.b"));
        assert!(shows_address("https://a.b/x", "a.b"));
    }

    #[test]
    fn images_are_their_alt_text_in_brackets() {
        let out = lines(
            "![CI status](https://img.example/ci.svg) and ![](x.png)",
            80,
        );
        assert_eq!(plain(&out), ["[CI status] and [image]"]);
        assert_eq!(ink(&out, "[CI status]"), Ink::Muted);
        // A badge, a link that's only an image, keeps its address to
        // itself; a link with words beside its image still shows it.
        let out = lines(
            "[![b](i.svg)](https://ci.example) [![c](i.svg) docs](https://d.example)",
            80,
        );
        assert_eq!(plain(&out), ["[b] [c] docs (https://d.example)"]);
    }

    #[test]
    fn html_stays_as_written_and_muted_and_br_breaks_the_line() {
        let out = lines(
            "<p align=\"center\">\n  <img src=\"x.png\">\n</p>\n\ntext<br>more <b>bold</b>\n",
            40,
        );
        assert_eq!(
            plain(&out),
            [
                "<p align=\"center\">",
                "<img src=\"x.png\">",
                "</p>",
                "",
                "text",
                "more <b>bold</b>",
            ]
        );
        assert_eq!(ink(&out, "<p"), Ink::Muted);
        assert_eq!(ink(&out, "<b>"), Ink::Muted);
        // A long tag wraps on its words rather than in an attribute.
        let out = lines("<img src=\"long.png\" alt=\"some words here\">\n", 24);
        assert_eq!(
            plain(&out),
            ["<img src=\"long.png\"", "alt=\"some words here\">"]
        );
    }

    #[test]
    fn front_matter_is_shown_muted_as_written() {
        let out = lines("---\nname: x\ntags: [a, b]\n---\n\n# Doc\n", 40);
        assert_eq!(plain(&out), ["name: x", "tags: [a, b]", "", "Doc", "━━━"]);
        assert_eq!(ink(&out, "name: x"), Ink::Muted);
    }

    #[test]
    fn nothing_makes_no_lines_and_blank_lines_at_the_end_go() {
        assert!(lines("", 40).is_empty());
        assert!(lines("  \n\n", 40).is_empty());
        assert_eq!(plain(&lines("a\n\n\n\n", 40)), ["a"]);
    }

    /// Every line fits the page at every width, for a text with one of
    /// everything: what the scrolling and drawing both rely on.
    #[test]
    fn no_line_is_ever_wider_than_the_page() {
        let text = "# A heading that goes on for a while\n\n\
            Some prose with **bold**, a [link](https://example.dev/quite/a/long/address/really) and `code`.\n\n\
            - a bullet with enough words to wrap a couple of times over\n  - nested\n1. one\n\n\
            > quoted text that is also long enough to need wrapping at narrow widths\n\n\
            ```rust\nfn main() { println!(\"a fairly long line of code that will not fit\"); }\n```\n\n\
            | col one | column two | three |\n|---|---|---|\n| x | a longer cell | yy |\n\n\
            ---\n\n<div>raw html that is long enough to wrap around the edge</div>\n\n\
            日本語のテキストと絵文字 🎉 mixed with ascii words here\n";
        for page in [4, 7, 12, 20, 33, 48, 80, 120] {
            for line in lines(text, page) {
                assert!(
                    width(&line) <= page,
                    "{page}: {} columns in {:?}",
                    width(&line),
                    plain(std::slice::from_ref(&line))
                );
            }
        }
    }

    const SEQUENCE: &str = "Before.\n\n```mermaid\nsequenceDiagram\n    TUI->>daemon: CreateAgent\n    daemon-->>TUI: Ack\n```\n\nAfter.\n";

    /// A mermaid fence is drawn, not shown as its source: boxes and arrows
    /// in the accent color, with a caption under them naming the kind.
    #[test]
    fn a_mermaid_fence_is_drawn_with_a_caption_under_it() {
        let out = lines(SEQUENCE, 60);
        let text = plain(&out);
        assert!(!text.iter().any(|line| line.contains("->>")), "{text:#?}");
        let top = text
            .iter()
            .position(|line| line.contains("┌─────┐"))
            .expect("boxes");
        assert!(
            text[top + 1].contains("│ TUI │") && text[top + 1].contains("│ daemon │"),
            "{text:#?}"
        );
        assert!(text.iter().any(|line| line.contains("CreateAgent")));
        assert!(text.iter().any(|line| line.contains('▶')));
        assert!(text.iter().any(|line| line.contains('◀')));
        let caption = text
            .iter()
            .position(|line| line == "mermaid · sequence")
            .expect("caption");
        assert!(text[caption - 1].contains('│'), "right under the diagram");
        assert_eq!(text[caption + 1], "", "then the page's blank line");
        assert_eq!(text.last().map(String::as_str), Some("After."));
        assert_eq!(ink(&out, "┌─────┐"), Ink::Accent);
        assert_eq!(ink(&out, "mermaid · sequence"), Ink::Muted);
    }

    /// The diagram is laid out in the room beside the fence's indent: in a
    /// list item it's narrower by the marker, and no line passes the edge.
    #[test]
    fn a_diagram_in_a_list_item_fits_the_room_beside_the_marker() {
        let text = "- item\n\n  ```mermaid\n  flowchart LR\n    parse --> layout --> draw --> print\n  ```\n";
        for page in [30, 44, 80] {
            let out = lines(text, page);
            assert!(
                out.iter().all(|line| width(line) <= page),
                "{:?}",
                plain(&out)
            );
            assert!(
                plain(&out)
                    .iter()
                    .any(|line| line.starts_with("  ") && line.contains('┌'))
            );
        }
    }

    /// One that can't be drawn (another kind, a line that can't be read, a
    /// layout too wide) is its source, as any code block, with why in the
    /// caption.
    #[test]
    fn a_diagram_not_drawn_is_its_source_with_why() {
        let pie = "```mermaid\npie title Pets\n  \"Dogs\" : 3\n```\n";
        let out = plain(&lines(pie, 80));
        assert_eq!(out[0].trim(), "pie title Pets");
        assert_eq!(
            out.last().map(String::as_str),
            Some("mermaid · not drawn: pie diagrams are not drawn in a terminal")
        );
        let narrow = plain(&lines(pie, 40));
        assert_eq!(
            &narrow[narrow.len() - 2..],
            [
                "mermaid · not drawn: pie diagrams are",
                "not drawn in a terminal"
            ],
            "a long reason wraps like prose"
        );
        let out = plain(&lines("```mermaid\nflowchart TD\n  A --> \n```\n", 60));
        assert!(
            out.last()
                .unwrap()
                .starts_with("mermaid · not drawn: line 2"),
            "{out:#?}"
        );
        let wide =
            "```mermaid\nsequenceDiagram\n  A->>B: x\n  B->>C: y\n  C->>D: z\n  D->>E: w\n```\n";
        let out = plain(&lines(wide, 14));
        assert!(out.iter().any(|line| line.contains("A->>B")), "{out:#?}");
        let caption = out
            .iter()
            .position(|line| line.starts_with("mermaid · not"))
            .expect("caption");
        assert!(out[caption..].join(" ").contains("columns"), "{out:#?}");
    }

    #[test]
    fn mermaid_blocks_are_the_fences_that_say_mermaid() {
        let page = "# Explained\n\n```mermaid\nflowchart LR\n a --> b\n```\n\n~~~mermaid\nsequenceDiagram\n A->>B: hi\n~~~\n\n```rust\nfn main() {}\n```\n";
        assert_eq!(
            mermaid_blocks(page),
            ["flowchart LR\n a --> b\n", "sequenceDiagram\n A->>B: hi\n"]
        );
        assert!(mermaid_blocks("no fences here").is_empty());
    }
}
