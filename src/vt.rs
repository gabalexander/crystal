//! A terminal's screen, as alacritty_terminal keeps it: what a program has
//! drawn, the rows that scrolled up off it, and what the program asked of
//! its terminal. The daemon keeps one for each session, and answers the
//! program's questions from it; each viewer keeps one of its own, fed the
//! same output, to draw.

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Grid, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell as GridCell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Timeout};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How many rows of history each screen keeps: tmux's default. It's kept
/// once in the daemon and again in each pane that shows the session.
pub const HISTORY_LINES: usize = 2_000;

/// The modes a program sets that crystal reads, by their DEC numbers.
pub mod mode {
    /// The arrows and Home/End send `ESC O` rather than `ESC [`.
    pub const APPLICATION_CURSOR: u16 = 1;
    /// The keypad sends escapes rather than digits.
    pub const APPLICATION_KEYPAD: u16 = 66;
    /// Mouse presses and releases.
    pub const MOUSE_NORMAL: u16 = 1000;
    /// Presses, releases, and moves while a button is held.
    pub const MOUSE_BUTTON: u16 = 1002;
    /// Presses, releases, and every move.
    pub const MOUSE_ANY: u16 = 1003;
    /// Focus coming and going is reported.
    pub const FOCUS_EVENTS: u16 = 1004;
    /// Mouse positions written as UTF-8 characters.
    pub const MOUSE_UTF8: u16 = 1005;
    /// Mouse events written the SGR way.
    pub const MOUSE_SGR: u16 = 1006;
    /// Pastes marked as pastes.
    pub const BRACKETED_PASTE: u16 = 2004;
}

/// alacritty_terminal's flag for each of [`mode`]'s modes.
fn term_mode(mode: u16) -> TermMode {
    match mode {
        mode::APPLICATION_CURSOR => TermMode::APP_CURSOR,
        mode::APPLICATION_KEYPAD => TermMode::APP_KEYPAD,
        mode::MOUSE_NORMAL => TermMode::MOUSE_REPORT_CLICK,
        mode::MOUSE_BUTTON => TermMode::MOUSE_DRAG,
        mode::MOUSE_ANY => TermMode::MOUSE_MOTION,
        mode::FOCUS_EVENTS => TermMode::FOCUS_IN_OUT,
        mode::MOUSE_UTF8 => TermMode::UTF8_MOUSE,
        mode::MOUSE_SGR => TermMode::SGR_MOUSE,
        mode::BRACKETED_PASTE => TermMode::BRACKETED_PASTE,
        _ => TermMode::empty(),
    }
}

/// The Kitty keyboard protocol's flags, as alacritty_terminal keeps them,
/// with the bit each one is in the protocol.
const KITTY_FLAGS: [(TermMode, u8); 5] = [
    (TermMode::DISAMBIGUATE_ESC_CODES, 1),
    (TermMode::REPORT_EVENT_TYPES, 2),
    (TermMode::REPORT_ALTERNATE_KEYS, 4),
    (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
    (TermMode::REPORT_ASSOCIATED_TEXT, 16),
];

pub struct Screen {
    term: Term<Listener>,
    parser: Processor<Unsynced>,
    /// What the terminal has heard from the program besides what it drew.
    heard: Arc<Mutex<Heard>>,
}

/// What alacritty_terminal hands back as it reads a program's output: the
/// title the program gives its terminal, and the answers to its questions.
#[derive(Default)]
struct Heard {
    /// Agents put a spinner here while they work.
    title: String,
    replies: Vec<u8>,
    /// Only the daemon's screen answers: viewers only draw.
    answering: bool,
}

#[derive(Clone)]
struct Listener(Arc<Mutex<Heard>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let mut heard = self.0.lock().unwrap();
        match event {
            Event::Title(title) => heard.title = title,
            Event::ResetTitle => heard.title.clear(),
            Event::PtyWrite(text) if heard.answering => heard.replies.extend(text.as_bytes()),
            _ => {}
        }
    }
}

/// A synchronized update that never waits: output is drawn as it comes,
/// so the daemon's screen, what it answers and what a new viewer is sent
/// never lag behind what the program wrote, and every viewer's screen
/// keeps step with it.
#[derive(Default)]
struct Unsynced;

impl Timeout for Unsynced {
    fn set_timeout(&mut self, _: Duration) {}

    fn clear_timeout(&mut self) {}

    fn pending_timeout(&self) -> bool {
        false
    }
}

/// A screen's size, the way alacritty_terminal takes it.
struct Size {
    rows: usize,
    cols: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

fn size(rows: u16, cols: u16) -> Size {
    Size {
        rows: usize::from(rows.max(1)),
        cols: usize::from(cols.max(2)),
    }
}

/// A cell as it's to be drawn.
pub struct Cell<'a> {
    /// What's in it: a character, with any that combine with it, or a
    /// space for an empty cell.
    pub text: &'a str,
    pub style: CellStyle,
    /// The left half of a wide character, whose right half is left out.
    pub wide: bool,
}

/// How a cell looks, beyond its text.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CellStyle {
    /// `None` for the terminal's default color.
    pub fg_color: Option<CellColor>,
    pub bg_color: Option<CellColor>,
    pub bold: bool,
    pub faint: bool,
    pub italic: bool,
    pub underlined: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellColor {
    /// One of the 256 colors of the terminal's palette.
    Palette(u8),
    Rgb(RgbColor),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Screen {
    /// A screen that only draws: a viewer's.
    pub fn new(rows: u16, cols: u16) -> Screen {
        let heard = Arc::new(Mutex::new(Heard::default()));
        let config = Config {
            scrolling_history: HISTORY_LINES,
            kitty_keyboard: true,
            ..Config::default()
        };
        let term = Term::new(config, &size(rows, cols), Listener(heard.clone()));
        Screen {
            term,
            parser: Processor::new(),
            heard,
        }
    }

    /// A screen that answers the program's questions to its terminal, like
    /// where the cursor is: the daemon's, which answers whether anyone's
    /// watching or not. [`Screen::take_replies`] hands the answers over.
    pub fn answering(rows: u16, cols: u16) -> Screen {
        let screen = Screen::new(rows, cols);
        screen.heard.lock().unwrap().answering = true;
        screen
    }

    /// Reads what the program wrote.
    pub fn process(&mut self, output: &[u8]) {
        self.parser.advance(&mut self.term, output);
    }

    /// The answers to the program's questions since the last call, to send
    /// back to it.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.heard.lock().unwrap().replies)
    }

    /// The size, as `(rows, cols)`.
    pub fn size(&self) -> (u16, u16) {
        let grid = self.term.grid();
        let rows = u16::try_from(grid.screen_lines()).unwrap_or(u16::MAX);
        let cols = u16::try_from(grid.columns()).unwrap_or(u16::MAX);
        (rows, cols)
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.term.resize(size(rows, cols));
    }

    /// The title the program gave its terminal: empty when it gave none.
    pub fn title(&self) -> String {
        self.heard.lock().unwrap().title.clone()
    }

    /// Whether the program has set `mode`, one of [`mode`]'s.
    pub fn mode(&self, mode: u16) -> bool {
        let flag = term_mode(mode);
        !flag.is_empty() && self.term.mode().contains(flag)
    }

    /// Whether the program has asked the arrow keys to send `ESC O`.
    pub fn application_cursor(&self) -> bool {
        self.mode(mode::APPLICATION_CURSOR)
    }

    /// Whether the program has asked for pastes to be marked as pastes.
    pub fn bracketed_paste(&self) -> bool {
        self.mode(mode::BRACKETED_PASTE)
    }

    /// Whether the program is on the alternate screen, which keeps no
    /// history.
    pub fn alternate_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// The Kitty keyboard protocol's flags the program has asked for: 0
    /// for keys the old way.
    pub fn kitty_keyboard(&self) -> u8 {
        let mode = self.term.mode();
        KITTY_FLAGS
            .iter()
            .filter(|(flag, _)| mode.contains(*flag))
            .fold(0, |flags, (_, bit)| flags | bit)
    }

    /// What the program has asked of the keyboard and the mouse.
    pub fn input_modes(&self) -> InputModes {
        InputModes {
            modes: INPUT_MODES.map(|mode| self.mode(mode)),
            kitty_keyboard: self.kitty_keyboard(),
        }
    }

    /// Output that gives a new screen of the same size this one's state:
    /// what's on the screen, with `with_history` the history before it,
    /// the modes the program set, and the cursor with its colors. On the
    /// alternate screen, that screen alone: the main one and its history
    /// aren't to be had.
    pub fn state_formatted(&self, with_history: bool) -> Vec<u8> {
        let grid = self.term.grid();
        let mode = *self.term.mode();
        let mut out = String::new();
        if self.alternate_screen() {
            out.push_str("\x1b[?1049h\x1b[H\x1b[2J");
        }
        // Each row is written out from the top, and a line feed after it
        // takes the next one down, scrolling the rows before up into the
        // history. A row that wrapped is written to its end instead, so the
        // new screen wraps it the same way and knows it for one line.
        let first = if with_history {
            -(grid.history_size() as i32)
        } else {
            0
        };
        let last = grid.screen_lines() as i32 - 1;
        for line in first..=last {
            let row = &grid[Line(line)];
            let wrapped = row[grid.last_column()].flags.contains(Flags::WRAPLINE);
            write_row(&mut out, row, grid.columns(), !wrapped);
            if line != last && !wrapped {
                out.push_str("\r\n");
            }
        }
        out.push_str("\x1b[m");
        write_modes(&mut out, mode);
        let cursor = &grid.cursor;
        let _ = write!(
            out,
            "\x1b[{};{}H",
            cursor.point.line.0 + 1,
            cursor.point.column.0 + 1
        );
        let pen = Style::of(&cursor.template);
        if pen != Style::default() {
            out.push_str(&pen.sequence());
        }
        out.into_bytes()
    }

    /// What's on the screen, one string per row without the blanks at its
    /// end, after the rows of the history with `with_history`.
    pub fn rows(&self, with_history: bool) -> Vec<String> {
        let grid = self.term.grid();
        let first = if with_history {
            -(grid.history_size() as i32)
        } else {
            0
        };
        (first..grid.screen_lines() as i32)
            .map(|line| row_text(grid, Line(line)))
            .collect()
    }

    /// The screen's rows down to its last one with something on it, as
    /// text with the escapes that color it, to print.
    pub fn styled(&self) -> String {
        let grid = self.term.grid();
        let rows = self.rows(false);
        let used = rows
            .iter()
            .rposition(|row| !row.trim().is_empty())
            .map_or(0, |last| last + 1);
        let mut out = String::new();
        for line in 0..used {
            if line > 0 {
                out.push_str("\x1b[m\n");
            }
            write_row(&mut out, &grid[Line(line as i32)], grid.columns(), true);
        }
        out
    }

    /// How many rows back into the history the screen is showing, or 0
    /// when it's live.
    pub fn scrolled_back(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Shows `rows` further back into the history, or toward live when
    /// it's negative. It stops at either end. While it's back, new output
    /// doesn't move it.
    pub fn scroll_back(&mut self, rows: isize) {
        let rows = i32::try_from(rows).unwrap_or(if rows < 0 { i32::MIN } else { i32::MAX });
        self.term.scroll_display(Scroll::Delta(rows));
    }

    /// Shows the live screen again.
    pub fn scroll_to_live(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// Calls `visit` with each cell showing, a row at a time, with its
    /// `(row, col)`: the live screen, or the history's rows when it's
    /// scrolled back.
    pub fn each_cell(&self, mut visit: impl FnMut(u16, u16, &Cell)) {
        let grid = self.term.grid();
        let offset = grid.display_offset() as i32;
        let mut text = String::new();
        for row in 0..grid.screen_lines() {
            let line = &grid[Line(row as i32 - offset)];
            for col in 0..grid.columns() {
                let cell = &line[Column(col)];
                // A wide character's right half is drawn by its left half.
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                text.clear();
                cell_text(cell, &mut text);
                visit(
                    row as u16,
                    col as u16,
                    &Cell {
                        text: &text,
                        style: Style::of(cell).cell_style(),
                        wide: cell.flags.contains(Flags::WIDE_CHAR),
                    },
                );
            }
        }
    }

    /// Where the cursor is on what's showing, as `(row, col)`, when it's
    /// to be seen there: not hidden, and not with the screen back in the
    /// history.
    pub fn cursor(&self) -> Option<(u16, u16)> {
        if !self.term.mode().contains(TermMode::SHOW_CURSOR) {
            return None;
        }
        let grid = self.term.grid();
        let mut point = grid.cursor.point;
        if grid[point].flags.contains(Flags::WIDE_CHAR_SPACER) {
            point.column.0 = point.column.0.saturating_sub(1);
        }
        let row = point.line.0 + grid.display_offset() as i32;
        let row = u16::try_from(row).ok()?;
        (usize::from(row) < grid.screen_lines()).then_some((row, point.column.0 as u16))
    }
}

/// What a cell has in it: its character and any that combine with it, or a
/// space when it's empty.
fn cell_text(cell: &GridCell, text: &mut String) {
    if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) || cell.c == '\0' {
        text.push(' ');
        return;
    }
    text.push(cell.c);
    if let Some(combining) = cell.zerowidth() {
        text.extend(combining);
    }
}

/// A row's text, without the blanks at its end.
fn row_text(grid: &Grid<GridCell>, line: Line) -> String {
    let row = &grid[line];
    let mut text = String::new();
    for col in 0..grid.columns() {
        let cell = &row[Column(col)];
        if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            cell_text(cell, &mut text);
        }
    }
    text.truncate(text.trim_end().len());
    text
}

/// Writes `row` as output that draws it again: its text, with a style
/// sequence wherever the style changes. With `trim`, the blank cells at
/// its end with no style of their own are left out.
fn write_row(
    out: &mut String,
    row: &alacritty_terminal::grid::Row<GridCell>,
    cols: usize,
    trim: bool,
) {
    let cells: Vec<&GridCell> = (0..cols).map(|col| &row[Column(col)]).collect();
    let used = if trim {
        cells
            .iter()
            .rposition(|cell| {
                let blank = cell.c == ' ' || cell.c == '\0';
                !blank || Style::of(cell) != Style::default()
            })
            .map_or(0, |last| last + 1)
    } else {
        cols
    };
    let mut style = Style::default();
    out.push_str("\x1b[m");
    for cell in &cells[..used] {
        // A wide character's right half is drawn by its left half, and the
        // space a wide one left at the end of a row comes back by itself
        // when the character wraps.
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let cell_style = Style::of(cell);
        if cell_style != style {
            out.push_str(&cell_style.sequence());
            style = cell_style;
        }
        cell_text(cell, out);
    }
}

/// The modes a fresh terminal doesn't have that the program set, and the
/// ones it has that the program reset.
fn write_modes(out: &mut String, mode: TermMode) {
    let private = [
        (TermMode::APP_CURSOR, 1),
        (TermMode::ORIGIN, 6),
        (TermMode::LINE_WRAP, 7),
        (TermMode::SHOW_CURSOR, 25),
        (TermMode::MOUSE_REPORT_CLICK, 1000),
        (TermMode::MOUSE_DRAG, 1002),
        (TermMode::MOUSE_MOTION, 1003),
        (TermMode::FOCUS_IN_OUT, 1004),
        (TermMode::UTF8_MOUSE, 1005),
        (TermMode::SGR_MOUSE, 1006),
        (TermMode::ALTERNATE_SCROLL, 1007),
        (TermMode::BRACKETED_PASTE, 2004),
    ];
    let fresh = TermMode::default();
    for (flag, number) in private {
        if mode.contains(flag) != fresh.contains(flag) {
            let set = if mode.contains(flag) { 'h' } else { 'l' };
            let _ = write!(out, "\x1b[?{number}{set}");
        }
    }
    let ansi = [(TermMode::INSERT, 4), (TermMode::LINE_FEED_NEW_LINE, 20)];
    for (flag, number) in ansi {
        if mode.contains(flag) {
            let _ = write!(out, "\x1b[{number}h");
        }
    }
    if mode.contains(TermMode::APP_KEYPAD) {
        out.push_str("\x1b=");
    }
    let kitty = KITTY_FLAGS
        .iter()
        .filter(|(flag, _)| mode.contains(*flag))
        .fold(0, |flags, (_, bit)| flags | bit);
    if kitty != 0 {
        let _ = write!(out, "\x1b[>{kitty}u");
    }
}

/// How a cell looks, beyond its text, as alacritty_terminal keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Style {
    fg: Color,
    bg: Color,
    flags: Flags,
}

impl Default for Style {
    fn default() -> Style {
        Style::of(&GridCell::default())
    }
}

/// The flags that are a cell's look, rather than its place in a wide
/// character or a wrapped line.
fn look(flags: Flags) -> Flags {
    flags
        & (Flags::INVERSE
            | Flags::BOLD
            | Flags::ITALIC
            | Flags::ALL_UNDERLINES
            | Flags::DIM
            | Flags::HIDDEN
            | Flags::STRIKEOUT)
}

impl Style {
    fn of(cell: &GridCell) -> Style {
        Style {
            fg: cell.fg,
            bg: cell.bg,
            flags: look(cell.flags),
        }
    }

    fn cell_style(&self) -> CellStyle {
        CellStyle {
            fg_color: cell_color(self.fg),
            bg_color: cell_color(self.bg),
            bold: self.flags.contains(Flags::BOLD),
            faint: self.flags.contains(Flags::DIM),
            italic: self.flags.contains(Flags::ITALIC),
            underlined: self.flags.intersects(Flags::ALL_UNDERLINES),
            inverse: self.flags.contains(Flags::INVERSE),
            invisible: self.flags.contains(Flags::HIDDEN),
            strikethrough: self.flags.contains(Flags::STRIKEOUT),
        }
    }

    /// The SGR sequence that sets this style from scratch.
    fn sequence(&self) -> String {
        let mut codes = vec!["0".to_string()];
        let flags = [
            (Flags::BOLD, "1"),
            (Flags::DIM, "2"),
            (Flags::ITALIC, "3"),
            (Flags::UNDERLINE, "4"),
            (Flags::DOUBLE_UNDERLINE, "4:2"),
            (Flags::UNDERCURL, "4:3"),
            (Flags::DOTTED_UNDERLINE, "4:4"),
            (Flags::DASHED_UNDERLINE, "4:5"),
            (Flags::INVERSE, "7"),
            (Flags::HIDDEN, "8"),
            (Flags::STRIKEOUT, "9"),
        ];
        for (flag, code) in flags {
            if self.flags.contains(flag) {
                codes.push(code.to_string());
            }
        }
        codes.extend(sgr_color(self.fg, 38));
        codes.extend(sgr_color(self.bg, 48));
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// The color a program asked for, or `None` for the terminal's default.
fn cell_color(color: Color) -> Option<CellColor> {
    match color {
        Color::Spec(rgb) => Some(CellColor::Rgb(RgbColor {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        })),
        Color::Indexed(index) => Some(CellColor::Palette(index)),
        Color::Named(named) => palette_index(named).map(CellColor::Palette),
    }
}

/// The palette's number for one of its 16 named colors.
fn palette_index(named: NamedColor) -> Option<u8> {
    let index = named as usize;
    (index < 16).then_some(index as u8)
}

/// The SGR codes for a color, given the code that introduces it: 38 for
/// the foreground, 48 for the background. The default color needs none,
/// since every style sequence starts from a reset.
fn sgr_color(color: Color, introducer: u8) -> Option<String> {
    match cell_color(color)? {
        CellColor::Palette(index) => Some(format!("{introducer};5;{index}")),
        CellColor::Rgb(RgbColor { r, g, b }) => Some(format!("{introducer};2;{r};{g};{b}")),
    }
}

/// The modes that change what a terminal sends the program in it, rather
/// than what it shows.
const INPUT_MODES: [u16; 9] = [
    mode::APPLICATION_CURSOR,
    mode::APPLICATION_KEYPAD,
    mode::MOUSE_NORMAL,
    mode::MOUSE_BUTTON,
    mode::MOUSE_ANY,
    mode::FOCUS_EVENTS,
    mode::MOUSE_UTF8,
    mode::MOUSE_SGR,
    mode::BRACKETED_PASTE,
];

/// What a program has asked of the keyboard and the mouse: what a terminal
/// it's drawn on, rather than one it runs in, must ask for too, so that its
/// keys reach the program the way it wants them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct InputModes {
    modes: [bool; INPUT_MODES.len()],
    kitty_keyboard: u8,
}

impl InputModes {
    /// Output that asks a terminal that was set as `before` to be set as
    /// these are. The Kitty keyboard flags are set in the terminal's
    /// current entry of its stack, which is the caller's to push and pop.
    pub fn changes_from(&self, before: &InputModes) -> Vec<u8> {
        let mut out = String::new();
        for ((&mode, &on), &was) in INPUT_MODES.iter().zip(&self.modes).zip(&before.modes) {
            if on == was {
                continue;
            }
            if mode == mode::APPLICATION_KEYPAD {
                // DECKPAM and DECKPNM, which every terminal knows.
                out.push_str(if on { "\x1b=" } else { "\x1b>" });
            } else {
                let set = if on { 'h' } else { 'l' };
                let _ = write!(out, "\x1b[?{mode}{set}");
            }
        }
        if self.kitty_keyboard != before.kitty_keyboard {
            let _ = write!(out, "\x1b[={};1u", self.kitty_keyboard);
        }
        out.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(rows: u16, cols: u16, output: &[u8]) -> Screen {
        let mut screen = Screen::new(rows, cols);
        screen.process(output);
        screen
    }

    /// Every cell showing, as `(text, style)`, row by row.
    fn cells(screen: &Screen) -> Vec<Vec<(String, CellStyle)>> {
        let mut rows: Vec<Vec<(String, CellStyle)>> = Vec::new();
        screen.each_cell(|row, _, cell| {
            if rows.len() <= usize::from(row) {
                rows.resize_with(usize::from(row) + 1, Vec::new);
            }
            rows[usize::from(row)].push((cell.text.to_string(), cell.style));
        });
        rows
    }

    #[test]
    fn rows_are_the_text_without_the_blanks_at_their_end() {
        let screen = screen(3, 10, b"hi\r\n  there   ");
        assert_eq!(screen.rows(false), ["hi", "  there", ""]);
    }

    #[test]
    fn the_history_is_the_rows_that_scrolled_off() {
        let screen = screen(2, 10, b"one\r\ntwo\r\nthree\r\nfour");
        assert_eq!(screen.rows(false), ["three", "four"]);
        assert_eq!(screen.rows(true), ["one", "two", "three", "four"]);
    }

    #[test]
    fn rows_scrolled_out_of_a_region_at_the_top_reach_the_history() {
        // Codex prints inline: a region down to just above its prompt,
        // scrolled up by line feeds and by `CSI S`.
        let mut output = b"\x1b[1;3r\x1b[3;1H".to_vec();
        for line in 0..5 {
            output.extend(format!("out {line}\n\r").as_bytes());
        }
        output.extend(b"\x1b[2S\x1b[4;1Hprompt");
        let screen = screen(4, 20, &output);
        let rows = screen.rows(true);
        let out: Vec<&str> = rows
            .iter()
            .map(String::as_str)
            .filter(|row| row.starts_with("out"))
            .collect();
        assert_eq!(out, ["out 0", "out 1", "out 2", "out 3", "out 4"]);
        assert_eq!(screen.rows(false), ["", "", "", "prompt"]);
        assert_eq!(rows.last().unwrap(), "prompt");
    }

    #[test]
    fn a_region_that_does_not_start_at_the_top_keeps_no_history() {
        let screen = screen(4, 20, b"\x1b[2;3r\x1b[3;1Hone\n\rtwo\n\rthree");
        assert_eq!(screen.rows(true), screen.rows(false));
    }

    #[test]
    fn the_alternate_screen_keeps_no_history() {
        let screen = screen(2, 20, b"\x1b[?1049hone\r\ntwo\r\nthree");
        assert_eq!(screen.rows(true), ["two", "three"]);
    }

    #[test]
    fn output_split_anywhere_reads_the_same() {
        let output = "\x1b[1;31mred\x1b[0m\r\n中文\x1b]0;title\x07\r\nend".as_bytes();
        let whole = screen(3, 10, output);
        let mut bytewise = Screen::new(3, 10);
        for byte in output {
            bytewise.process(std::slice::from_ref(byte));
        }
        assert_eq!(cells(&bytewise), cells(&whole));
        assert_eq!(bytewise.title(), "title");
    }

    #[test]
    fn a_title_and_modes_are_kept() {
        let screen = screen(2, 10, b"\x1b]0;\xe2\x9c\xb3 Claude\x07\x1b[?1h\x1b[?2004h");
        assert_eq!(screen.title(), "✳ Claude");
        assert!(screen.application_cursor());
        assert!(screen.bracketed_paste());
        assert!(!screen.mode(mode::MOUSE_SGR));
    }

    #[test]
    fn questions_are_answered_only_by_an_answering_screen() {
        let mut viewer = screen(2, 10, b"\x1b[6n");
        assert!(viewer.take_replies().is_empty());

        let mut daemon = Screen::answering(2, 10);
        daemon.process(b"ab\x1b[6n");
        assert_eq!(daemon.take_replies(), b"\x1b[1;3R");
        assert!(daemon.take_replies().is_empty());
    }

    #[test]
    fn a_program_is_told_images_arent_drawn() {
        let mut screen = Screen::answering(2, 10);
        screen.process(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
        let reply = String::from_utf8(screen.take_replies()).unwrap();
        assert!(!reply.contains("OK"), "{reply:?}");
    }

    #[test]
    fn the_state_formatted_rebuilds_the_screen() {
        let output = b"\x1b[?2004h\x1b[1;31mred\x1b[0m plain\r\n\x1b[48;5;21m  \x1b[0m\
\x1b[7mrev\x1b[0m\r\n\xe4\xb8\xad\xe6\x96\x87 wide\x1b[2;4H";
        let original = screen(4, 20, output);
        let copy = screen(4, 20, &original.state_formatted(false));
        assert_eq!(copy.rows(false), original.rows(false));
        assert_eq!(cells(&copy), cells(&original));
        assert_eq!(copy.cursor(), original.cursor());
        assert_eq!(copy.cursor(), Some((1, 3)));
        assert!(copy.bracketed_paste());
    }

    #[test]
    fn the_state_formatted_carries_the_modes_and_the_colors_to_write_in() {
        let output = b"\x1b[?1h\x1b=\x1b[?1002h\x1b[?1006h\x1b[?25l\x1b[>5uhi\x1b[1;4;38;5;208m";
        let original = screen(3, 10, output);
        let mut copy = screen(3, 10, &original.state_formatted(false));
        assert_eq!(copy.input_modes(), original.input_modes());
        assert_eq!(copy.kitty_keyboard(), 5);
        assert_eq!(copy.cursor(), None);
        // What the program writes next comes out in the colors it chose.
        copy.process(b"!");
        let written = cells(&copy)[0][2].1;
        assert_eq!(written.fg_color, Some(CellColor::Palette(208)));
        assert!(written.bold && written.underlined);
    }

    #[test]
    fn the_state_formatted_with_history_gives_the_history_too() {
        let mut output = Vec::new();
        for line in 0..30 {
            output.extend(format!("line {line}\r\n").as_bytes());
        }
        output.extend(b"last");
        let original = screen(5, 20, &output);
        let copy = screen(5, 20, &original.state_formatted(true));
        assert_eq!(copy.rows(true), original.rows(true));
        assert_eq!(copy.cursor(), original.cursor());

        let without = screen(5, 20, &original.state_formatted(false));
        assert_eq!(without.rows(false), original.rows(false));
        assert_eq!(without.rows(true), original.rows(false));
    }

    #[test]
    fn soft_wrapped_rows_stay_one_line() {
        let original = screen(4, 5, b"abcdefgh");
        let mut copy = screen(4, 5, &original.state_formatted(true));
        assert_eq!(copy.rows(false), ["abcde", "fgh", "", ""]);
        // Wider, the line comes back together.
        copy.resize(4, 10);
        assert_eq!(copy.rows(false)[0], "abcdefgh");
    }

    #[test]
    fn the_alternate_screen_is_formatted_as_itself() {
        let original = screen(3, 10, b"shell\r\n\x1b[?1049h\x1b[Hfull");
        let copy = screen(3, 10, &original.state_formatted(true));
        assert!(copy.alternate_screen());
        assert_eq!(copy.rows(false)[0], "full");
    }

    #[test]
    fn scrolling_back_stops_at_the_ends_and_stays_put_as_output_comes() {
        let mut output = Vec::new();
        for line in 0..24 {
            output.extend(format!("line {line}\r\n").as_bytes());
        }
        let mut screen = screen(5, 20, &output);
        screen.scroll_back(4);
        assert_eq!(screen.scrolled_back(), 4);
        let showing = cells(&screen);
        screen.process(b"line 24\r\nline 25\r\n");
        assert_eq!(cells(&screen), showing);
        screen.scroll_back(1000);
        assert_eq!(screen.scrolled_back(), 22);
        screen.scroll_to_live();
        assert_eq!(screen.scrolled_back(), 0);
    }

    #[test]
    fn the_kitty_keyboard_flags_follow_the_program() {
        let mut screen = Screen::answering(2, 10);
        screen.process(b"\x1b[>1u");
        assert_eq!(screen.kitty_keyboard(), 1);
        screen.process(b"\x1b[?u");
        assert_eq!(screen.take_replies(), b"\x1b[?1u");
        screen.process(b"\x1b[<u");
        assert_eq!(screen.kitty_keyboard(), 0);
    }

    #[test]
    fn input_modes_say_what_changed() {
        let mut screen = Screen::new(2, 10);
        let none = screen.input_modes();
        assert_eq!(none, InputModes::default());
        assert!(none.changes_from(&none).is_empty());

        screen.process(b"\x1b[?1h\x1b=\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[>5u");
        let set = screen.input_modes();
        assert_eq!(
            String::from_utf8(set.changes_from(&none)).unwrap(),
            "\x1b[?1h\x1b=\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[=5;1u"
        );
        assert_eq!(
            String::from_utf8(none.changes_from(&set)).unwrap(),
            "\x1b[?1l\x1b>\x1b[?1000l\x1b[?1006l\x1b[?2004l\x1b[=0;1u"
        );
    }

    #[test]
    fn colors_and_attributes_come_through() {
        let screen = screen(1, 4, b"\x1b[1;31;48;5;21mR\x1b[0;7;38;2;1;2;3mT");
        let row = &cells(&screen)[0];
        let red = row[0].1;
        assert_eq!(red.fg_color, Some(CellColor::Palette(1)));
        assert_eq!(red.bg_color, Some(CellColor::Palette(21)));
        assert!(red.bold);
        let truecolor = row[1].1;
        assert_eq!(
            truecolor.fg_color,
            Some(CellColor::Rgb(RgbColor { r: 1, g: 2, b: 3 }))
        );
        assert!(truecolor.inverse);
        assert!(!truecolor.bold);
    }

    #[test]
    fn a_wide_character_is_one_cell() {
        let screen = screen(1, 6, "中x".as_bytes());
        let mut seen = Vec::new();
        screen.each_cell(|_, col, cell| seen.push((col, cell.text.to_string(), cell.wide)));
        assert_eq!(seen[0], (0, "中".to_string(), true));
        assert_eq!(seen[1], (2, "x".to_string(), false));
    }

    #[test]
    fn the_cursor_hides_when_the_program_hides_it() {
        let mut screen = screen(2, 10, b"ab");
        assert_eq!(screen.cursor(), Some((0, 2)));
        screen.process(b"\x1b[?25l");
        assert_eq!(screen.cursor(), None);
    }
}
