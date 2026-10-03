//! The grid of terminal cells every diagram is drawn into, and the glyphs
//! it's printed with.
//!
//! A line isn't a character while it's being drawn: a cell remembers which
//! of its four sides lines leave it by, and the glyph is chosen only when
//! the grid is printed. That's what makes junctions come out right without
//! any drawing code knowing about its neighbours: an edge leaving the
//! bottom of a box adds "down" to a border cell that had "left" and
//! "right", and the cell prints as `┬`; two edges that cross print `┼`; a
//! lifeline that meets a message prints `├`. Text sits above the lines and
//! always wins, so a label written across a line hides it.
//!
//! A wide character takes two cells: the second holds a marker that prints
//! as nothing, so every row is exactly as wide on screen as its cells.

use super::width::char_width;

/// A line leaves the cell upward.
pub const UP: u8 = 1;
/// …downward.
pub const DOWN: u8 = 2;
/// …to the left.
pub const LEFT: u8 = 4;
/// …to the right.
pub const RIGHT: u8 = 8;

/// How a line is drawn: the three edge styles mermaid has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stroke {
    #[default]
    Solid,
    Dotted,
    Thick,
}

/// The characters a diagram is printed with: box drawing, or ASCII for
/// fonts without it and for pasting where box drawing gets mangled.
#[derive(Debug, Clone, Copy)]
pub struct Glyphs {
    pub ascii: bool,
}

impl Glyphs {
    pub const BOX_DRAWING: Glyphs = Glyphs { ascii: false };
    pub const ASCII: Glyphs = Glyphs { ascii: true };

    /// The line character for a set of sides and a stroke.
    pub fn line(&self, mask: u8, stroke: Stroke, rounded: bool) -> char {
        let horizontal = mask & (UP | DOWN) == 0;
        let vertical = mask & (LEFT | RIGHT) == 0;
        if self.ascii {
            return match (horizontal, vertical, stroke) {
                (true, _, Stroke::Dotted) => '.',
                (true, _, Stroke::Thick) => '=',
                (true, _, Stroke::Solid) => '-',
                (_, true, Stroke::Dotted) => ':',
                (_, true, _) => '|',
                _ => '+',
            };
        }
        match (horizontal, vertical, stroke) {
            (true, _, Stroke::Dotted) => return '┄',
            (true, _, Stroke::Thick) => return '━',
            (_, true, Stroke::Dotted) => return '┆',
            (_, true, Stroke::Thick) => return '┃',
            _ => {}
        }
        match mask {
            m if m == LEFT | RIGHT || m == LEFT || m == RIGHT => '─',
            m if m == UP | DOWN || m == UP || m == DOWN => '│',
            m if m == RIGHT | DOWN => {
                if rounded {
                    '╭'
                } else {
                    '┌'
                }
            }
            m if m == LEFT | DOWN => {
                if rounded {
                    '╮'
                } else {
                    '┐'
                }
            }
            m if m == RIGHT | UP => {
                if rounded {
                    '╰'
                } else {
                    '└'
                }
            }
            m if m == LEFT | UP => {
                if rounded {
                    '╯'
                } else {
                    '┘'
                }
            }
            m if m == UP | DOWN | RIGHT => '├',
            m if m == UP | DOWN | LEFT => '┤',
            m if m == LEFT | RIGHT | DOWN => '┬',
            m if m == LEFT | RIGHT | UP => '┴',
            _ => '┼',
        }
    }

    /// An arrowhead pointing in `dir` (one of the side constants).
    pub fn arrow(&self, dir: u8) -> char {
        match (self.ascii, dir) {
            (false, DOWN) => '▼',
            (false, UP) => '▲',
            (false, LEFT) => '◀',
            (false, _) => '▶',
            (true, DOWN) => 'v',
            (true, UP) => '^',
            (true, LEFT) => '<',
            (true, _) => '>',
        }
    }

    /// An open (async) arrowhead: mermaid's `-)`.
    pub fn open_arrow(&self, dir: u8) -> char {
        match (self.ascii, dir) {
            (false, DOWN) => '▽',
            (false, UP) => '△',
            (false, LEFT) => '◁',
            (false, _) => '▷',
            (true, LEFT) => '(',
            (true, _) => ')',
        }
    }

    /// The end of a message that was stopped: mermaid's `-x`.
    pub fn cross(&self) -> char {
        if self.ascii { 'x' } else { '×' }
    }

    /// A class diagram's composition end: a filled diamond.
    pub fn diamond(&self) -> char {
        if self.ascii { '*' } else { '◆' }
    }

    /// A class diagram's aggregation end: a hollow diamond.
    pub fn hollow_diamond(&self) -> char {
        if self.ascii { 'o' } else { '◇' }
    }

    /// A state diagram's start: a filled circle.
    pub fn start(&self) -> char {
        if self.ascii { '*' } else { '●' }
    }

    /// A state diagram's end: a ringed circle.
    pub fn end(&self) -> char {
        if self.ascii { '@' } else { '◉' }
    }

    /// Label text in this set: in ASCII, the typography the drawing adds
    /// itself (the ellipsis of a cut label, a class's `«annotation»`, the
    /// dash between cardinalities) is spelled in ASCII too. What the author
    /// wrote is left as written.
    pub fn text(&self, s: String) -> String {
        if !self.ascii || s.is_ascii() {
            return s;
        }
        s.replace('…', "~")
            .replace('«', "<<")
            .replace('»', ">>")
            .replace('—', "-")
    }

    /// A self-transition's marker inside a state box.
    pub fn self_loop(&self) -> &'static str {
        if self.ascii { "@" } else { "↻" }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Text {
    None,
    Char(char),
    /// The right half of a wide character to its left.
    Cont,
}

#[derive(Debug, Clone, Copy)]
struct Cell {
    mask: u8,
    stroke: Stroke,
    rounded: bool,
    text: Text,
}

const EMPTY: Cell = Cell {
    mask: 0,
    stroke: Stroke::Solid,
    rounded: false,
    text: Text::None,
};

/// The most cells a canvas side may have.
const MAX_SIDE: usize = 4096;

/// A grid that grows to whatever is drawn into it.
#[derive(Debug, Clone)]
pub struct Canvas {
    rows: Vec<Vec<Cell>>,
    scratch: Cell,
}

impl Default for Canvas {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            scratch: EMPTY,
        }
    }
}

impl Canvas {
    pub fn new() -> Self {
        Self::default()
    }

    fn cell(&mut self, x: usize, y: usize) -> &mut Cell {
        // A coordinate past the bound is a layout bug, never a diagram: it
        // lands in a scratch cell rather than growing the grid to a size
        // that takes the process down.
        if x >= MAX_SIDE || y >= MAX_SIDE {
            self.scratch = EMPTY;
            return &mut self.scratch;
        }
        if self.rows.len() <= y {
            self.rows.resize(y + 1, Vec::new());
        }
        let row = &mut self.rows[y];
        if row.len() <= x {
            row.resize(x + 1, EMPTY);
        }
        &mut row[x]
    }

    fn peek(&self, x: usize, y: usize) -> Cell {
        self.rows
            .get(y)
            .and_then(|r| r.get(x))
            .copied()
            .unwrap_or(EMPTY)
    }

    /// Add sides to a cell's line.
    pub fn join(&mut self, x: usize, y: usize, mask: u8, stroke: Stroke) {
        let cell = self.cell(x, y);
        cell.mask |= mask;
        if stroke != Stroke::Solid || cell.mask == mask {
            cell.stroke = stroke;
        }
    }

    /// Mark a corner cell to print rounded.
    pub fn round(&mut self, x: usize, y: usize) {
        self.cell(x, y).rounded = true;
    }

    /// A horizontal line from `x0` to `x1` (either order) on row `y`.
    pub fn hline(&mut self, y: usize, x0: usize, x1: usize, stroke: Stroke) {
        let (a, b) = (x0.min(x1), x0.max(x1));
        if a == b {
            return;
        }
        for x in a..=b {
            let mut m = 0;
            if x > a {
                m |= LEFT;
            }
            if x < b {
                m |= RIGHT;
            }
            self.join(x, y, m, stroke);
        }
    }

    /// A vertical line from `y0` to `y1` (either order) in column `x`.
    pub fn vline(&mut self, x: usize, y0: usize, y1: usize, stroke: Stroke) {
        let (a, b) = (y0.min(y1), y0.max(y1));
        if a == b {
            return;
        }
        for y in a..=b {
            let mut m = 0;
            if y > a {
                m |= UP;
            }
            if y < b {
                m |= DOWN;
            }
            self.join(x, y, m, stroke);
        }
    }

    /// A rectangle's border, `w` by `h` cells from (`x`, `y`).
    pub fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, stroke: Stroke, rounded: bool) {
        if w < 2 || h < 2 {
            return;
        }
        let (x1, y1) = (x + w - 1, y + h - 1);
        self.hline(y, x, x1, stroke);
        self.hline(y1, x, x1, stroke);
        self.vline(x, y, y1, stroke);
        self.vline(x1, y, y1, stroke);
        if rounded {
            for (cx, cy) in [(x, y), (x1, y), (x, y1), (x1, y1)] {
                self.round(cx, cy);
            }
        }
    }

    /// Blank the text and lines of a rectangle's cells: a box's inside,
    /// so nothing drawn earlier shows through it.
    pub fn clear(&mut self, x: usize, y: usize, w: usize, h: usize) {
        for yy in y..y + h {
            for xx in x..x + w {
                let cell = self.cell(xx, yy);
                *cell = Cell {
                    text: Text::Char(' '),
                    ..EMPTY
                };
            }
        }
    }

    /// Take the text off a run of cells, so a line drawn there shows: a
    /// rule across a box whose inside was cleared.
    pub fn untext(&mut self, x: usize, y: usize, w: usize) {
        for xx in x..x + w {
            self.cell(xx, y).text = Text::None;
        }
    }

    /// Write one character at (`x`, `y`), over any line there. Returns the
    /// columns it took.
    pub fn put(&mut self, x: usize, y: usize, c: char) -> usize {
        let w = char_width(c);
        if w == 0 {
            return 0;
        }
        // Overwriting half of a wide character blanks the other half, so a
        // row never prints a stray half.
        if self.peek(x, y).text == Text::Cont && x > 0 {
            self.cell(x - 1, y).text = Text::Char(' ');
        }
        if w == 2 {
            if let Text::Char(prev) = self.peek(x + 1, y).text
                && char_width(prev) == 2
            {
                self.cell(x + 2, y).text = Text::Char(' ');
            }
            self.cell(x + 1, y).text = Text::Cont;
        } else if let Text::Char(prev) = self.peek(x, y).text
            && char_width(prev) == 2
        {
            self.cell(x + 1, y).text = Text::Char(' ');
        }
        self.cell(x, y).text = Text::Char(c);
        w
    }

    /// Write `s` from (`x`, `y`) rightward. Returns the columns it took.
    pub fn text(&mut self, x: usize, y: usize, s: &str) -> usize {
        let mut at = x;
        for c in s.chars() {
            at += self.put(at, y, c);
        }
        at - x
    }

    /// Whether a cell holds anything: a line, or text other than a space.
    pub fn occupied(&self, x: usize, y: usize) -> bool {
        let c = self.peek(x, y);
        c.mask != 0 || matches!(c.text, Text::Char(ch) if ch != ' ') || c.text == Text::Cont
    }

    /// The grid as text, one string per row, trailing spaces trimmed and
    /// trailing empty rows dropped.
    pub fn lines(&self, glyphs: &Glyphs) -> Vec<String> {
        let mut out: Vec<String> = self
            .rows
            .iter()
            .map(|row| {
                let mut s = String::with_capacity(row.len());
                for cell in row {
                    match cell.text {
                        Text::Char(c) => s.push(c),
                        Text::Cont => {}
                        Text::None if cell.mask != 0 => {
                            s.push(glyphs.line(cell.mask, cell.stroke, cell.rounded))
                        }
                        Text::None => s.push(' '),
                    }
                }
                s.trim_end().to_string()
            })
            .collect();
        while out.last().is_some_and(|l| l.is_empty()) {
            out.pop();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mermaid::width::display_width;

    const U: Glyphs = Glyphs { ascii: false };
    const A: Glyphs = Glyphs { ascii: true };

    #[test]
    fn a_box_with_an_edge_leaving_its_bottom_joins_as_a_tee() {
        let mut c = Canvas::new();
        c.rect(0, 0, 5, 3, Stroke::Solid, false);
        c.vline(2, 2, 4, Stroke::Solid);
        assert_eq!(c.lines(&U), vec!["┌───┐", "│   │", "└─┬─┘", "  │", "  │"]);
        assert_eq!(c.lines(&A), vec!["+---+", "|   |", "+-+-+", "  |", "  |"]);
    }

    #[test]
    fn crossing_lines_print_a_cross_and_text_wins_over_lines() {
        let mut c = Canvas::new();
        c.hline(1, 0, 4, Stroke::Solid);
        c.vline(2, 0, 2, Stroke::Solid);
        assert_eq!(c.lines(&U), vec!["  │", "──┼──", "  │"]);
        c.text(1, 1, "ab");
        assert_eq!(c.lines(&U), vec!["  │", "─ab──", "  │"]);
    }

    #[test]
    fn rounded_corners_dotted_and_thick_strokes() {
        let mut c = Canvas::new();
        c.rect(0, 0, 3, 3, Stroke::Solid, true);
        c.hline(3, 0, 2, Stroke::Dotted);
        c.hline(4, 0, 2, Stroke::Thick);
        assert_eq!(c.lines(&U), vec!["╭─╮", "│ │", "╰─╯", "┄┄┄", "━━━"]);
        assert_eq!(c.lines(&A), vec!["+-+", "| |", "+-+", "...", "==="]);
    }

    #[test]
    fn a_wide_character_takes_two_cells_and_overwriting_half_blanks_the_rest() {
        let mut c = Canvas::new();
        assert_eq!(c.text(0, 0, "日x"), 3);
        assert_eq!(c.lines(&U), vec!["日x"]);
        c.put(1, 0, 'a');
        assert_eq!(c.lines(&U), vec![" ax"]);
        let mut c = Canvas::new();
        c.hline(0, 0, 5, Stroke::Solid);
        c.text(1, 0, "本");
        let row = &c.lines(&U)[0];
        assert_eq!(display_width(row), 6, "{row}");
    }
}
