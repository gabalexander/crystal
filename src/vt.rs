//! A terminal's screen, as alacritty_terminal keeps it: what a program has
//! drawn, the rows that scrolled up off it, and what the program asked of
//! its terminal. The daemon keeps one for each session, and answers the
//! program's questions from it; each viewer keeps one of its own, fed the
//! same output, to draw, and to copy from: copy mode's cursor, the
//! selection and searches are Alacritty's own vi mode, kept in step with
//! the output as it scrolls. A viewer's screen also finds the links on it,
//! the hyperlinks a program wrote (OSC 8) and the URLs in its text. Every
//! screen counts the times the program rang the terminal's bell, for the
//! daemon to mark the session and a viewer to ring the user's terminal.

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Grid, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use alacritty_terminal::term::cell::{Cell as GridCell, Flags, Hyperlink};
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vi_mode::ViMotion;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Timeout};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How many rows of history a screen keeps when the config doesn't say:
/// Alacritty's default. It's kept once in the daemon and again in each pane
/// that shows the session.
pub const DEFAULT_HISTORY_LINES: usize = 10_000;

/// The most rows of history the config may ask a screen to keep.
pub const MAX_HISTORY_LINES: usize = 1_000_000;

/// How many rows of history the screens made from now on keep:
/// `scrollback_lines` in the config, which each process that makes screens
/// sets as it reads it.
static HISTORY_LINES: AtomicUsize = AtomicUsize::new(DEFAULT_HISTORY_LINES);

/// Has the screens made from now on keep `lines` rows of history, at most
/// [`MAX_HISTORY_LINES`]. A screen made before keeps what it kept.
pub fn set_history_lines(lines: usize) {
    HISTORY_LINES.store(lines.min(MAX_HISTORY_LINES), Ordering::Relaxed);
}

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
    /// The last search, while copy mode keeps it.
    search: Option<Search>,
    /// What finds URLs in the text, made the first time a link is looked
    /// for, and borrowed mutably for the cache it keeps as it goes.
    urls: RefCell<Option<RegexSearch>>,
    /// The progress the program reports, which alacritty_terminal passes
    /// over.
    progress: Progress,
}

/// Picks the progress a program reports out of its output: OSC 9;4, which
/// ConEmu began and Windows Terminal and Ghostty draw. Agents report with
/// it that they're working. It's kept as written after `9;`, like `4;1;-1`
/// (a state, then a percentage), for the rules that read agents' screens.
#[derive(Default)]
struct Progress {
    /// What the last report said.
    last: String,
    /// Where in an escape sequence the output has got to.
    at: Sniff,
    /// The OSC sequence so far, up to [`Progress::LONGEST`] bytes.
    osc: Vec<u8>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Sniff {
    #[default]
    Text,
    /// After an ESC.
    Escape,
    /// Inside an OSC sequence.
    Osc,
    /// After an ESC inside one, which a `\` ends it with.
    OscEscape,
}

impl Progress {
    /// The longest OSC sequence kept: a progress report is short.
    const LONGEST: usize = 32;

    fn read(&mut self, output: &[u8]) {
        let mut rest = output;
        while !rest.is_empty() {
            if self.at == Sniff::Text {
                // Most output is text: skip to the next escape.
                match rest.iter().position(|&byte| byte == 0x1b) {
                    Some(at) => rest = &rest[at..],
                    None => return,
                }
            }
            let byte = rest[0];
            rest = &rest[1..];
            self.at = match (self.at, byte) {
                (_, 0x1b) if self.at != Sniff::Osc => Sniff::Escape,
                (Sniff::Escape, b']') => {
                    self.osc.clear();
                    Sniff::Osc
                }
                (Sniff::Osc, 0x07) | (Sniff::OscEscape, b'\\') => {
                    self.finish();
                    Sniff::Text
                }
                (Sniff::Osc, 0x1b) => Sniff::OscEscape,
                (Sniff::Osc, byte) => {
                    if self.osc.len() <= Self::LONGEST {
                        self.osc.push(byte);
                    }
                    Sniff::Osc
                }
                _ => Sniff::Text,
            };
        }
    }

    fn finish(&mut self) {
        // `9;` and text is a notification, which could start with a 4.
        if self.osc.len() <= Self::LONGEST
            && let Some(report) = self.osc.strip_prefix(b"9;4")
            && (report.is_empty() || report.starts_with(b";"))
        {
            self.last = format!("4{}", String::from_utf8_lossy(report));
        }
    }
}

/// What a URL written out in a screen's text looks like: a scheme a
/// browser opens, then everything up to a blank, a quote or a bracket that
/// can't be in one. Punctuation that ends a sentence is taken off after.
const URL: &str = r#"(https?|file)://[^\x00-\x1f\x7f-\x9f\s<>"{}|\\^`⟨⟩]+"#;

/// A link on a screen: a hyperlink the program wrote (OSC 8), or a URL in
/// its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    /// Its first and last cells on what's showing, as `(row, col)`; every
    /// cell between them, in reading order, is the link's. One that goes
    /// on out of sight is cut at the edge.
    pub start: (u16, u16),
    pub end: (u16, u16),
}

impl Link {
    /// Whether the cell at `(row, col)` of what's showing is the link's.
    pub fn covers(&self, cell: (u16, u16)) -> bool {
        self.start <= cell && cell <= self.end
    }
}

/// A search through the screen and its history.
struct Search {
    /// Alacritty's search, which keeps a cache as it goes, and so is
    /// borrowed mutably even to draw the matches.
    regex: RefCell<RegexSearch>,
}

/// A move of copy mode's cursor, as vi has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Down,
    Up,
    Right,
    /// To the start of the line: `0`.
    LineStart,
    /// To its first character that isn't a blank: `^`.
    LineText,
    /// To its end: `$`.
    LineEnd,
    /// To the top, middle or bottom row showing: `H`, `M`, `L`.
    ViewTop,
    ViewMiddle,
    ViewBottom,
    /// To the start of the next word, the start of this one or the one
    /// before, or its end: `w`, `b`, `e`. A word ends at punctuation.
    WordNext,
    WordBack,
    WordEnd,
    /// The same, with words that only blanks end: `W`, `B`, `E`.
    BigWordNext,
    BigWordBack,
    BigWordEnd,
    /// To the blank line before or after this paragraph: `{`, `}`.
    ParagraphBack,
    ParagraphNext,
    /// To the bracket that pairs with the one under the cursor: `%`.
    Bracket,
    /// To the first row of the history: `gg`.
    HistoryTop,
    /// To the last row of the screen: `G`.
    HistoryBottom,
}

/// What a selection takes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    /// Characters, from one to another, wrapping onto the rows between.
    Chars,
    /// Whole lines.
    Lines,
    /// A rectangle.
    Block,
}

impl SelectionKind {
    fn alacritty(self) -> SelectionType {
        match self {
            SelectionKind::Chars => SelectionType::Simple,
            SelectionKind::Lines => SelectionType::Lines,
            SelectionKind::Block => SelectionType::Block,
        }
    }
}

/// Where a search landed: the match the cursor is on now, counted from
/// the top of the history, out of how many there are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub number: usize,
    pub of: usize,
}

/// What copy mode marks on a cell, beyond what the program drew there.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    #[default]
    None,
    /// Something the search found.
    Found,
    /// The match copy mode's cursor is on.
    Current,
    /// Inside the selection.
    Selected,
    /// Under copy mode's cursor.
    Cursor,
}

/// A screen as one daemon hands it to the next (see [`crate::handover`]):
/// its size, the title the program gave it, and output that draws it again
/// on a fresh screen of that size, history and all, on both its screens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Saved {
    pub rows: u16,
    pub cols: u16,
    pub title: String,
    pub output: String,
}

/// What alacritty_terminal hands back as it reads a program's output: the
/// title the program gives its terminal, the answers to its questions, and
/// its bell.
#[derive(Default)]
struct Heard {
    /// Agents put a spinner here while they work.
    title: String,
    replies: Vec<u8>,
    /// The times the program rang the bell since they were last taken.
    bells: u32,
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
            Event::Bell => heard.bells = heard.bells.saturating_add(1),
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
    pub mark: Mark,
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
        Screen::keeping(rows, cols, HISTORY_LINES.load(Ordering::Relaxed))
    }

    /// A screen that keeps `history` rows of history.
    fn keeping(rows: u16, cols: u16, history: usize) -> Screen {
        let heard = Arc::new(Mutex::new(Heard::default()));
        let config = Config {
            scrolling_history: history,
            kitty_keyboard: true,
            ..Config::default()
        };
        let term = Term::new(config, &size(rows, cols), Listener(heard.clone()));
        Screen {
            term,
            parser: Processor::new(),
            heard,
            search: None,
            urls: RefCell::default(),
            progress: Progress::default(),
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
        self.progress.read(output);
    }

    /// The progress the program last reported (OSC 9;4), as it wrote it
    /// after `9;`, like `4;1;-1`: empty when it reported none.
    pub fn progress(&self) -> &str {
        &self.progress.last
    }

    /// The answers to the program's questions since the last call, to send
    /// back to it.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.heard.lock().unwrap().replies)
    }

    /// How many times the program rang the terminal's bell since the last
    /// call: a `BEL` on its own, not the one that ends a title.
    pub fn take_bells(&mut self) -> u32 {
        std::mem::take(&mut self.heard.lock().unwrap().bells)
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
        self.formatted(with_history).into_bytes()
    }

    /// [`Screen::state_formatted`], as the text it is.
    fn formatted(&self, with_history: bool) -> String {
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
        // A hyperlink the program has opened and not yet closed takes in
        // what it writes next.
        if let Some(link) = cursor.template.hyperlink() {
            write_hyperlink(&mut out, Some(&link));
        }
        out
    }

    /// The screen as it's handed to the next daemon. On the alternate
    /// screen, the main one and its history go first, then the alternate
    /// one, which the program goes back from to find the main one as it
    /// left it. Leaves the screen as it was.
    pub fn save(&mut self) -> Saved {
        let output = if self.alternate_screen() {
            let alternate = self.formatted(false);
            // The main screen, history and all, isn't to be had without
            // going back to it.
            self.term.swap_alt();
            let main = self.formatted(true);
            // Going to the alternate screen clears it: drawn again.
            self.term.swap_alt();
            self.process(alternate.as_bytes());
            // A link left open on the main screen stays there.
            let mut output = main;
            write_hyperlink(&mut output, None);
            output + &alternate
        } else {
            self.formatted(true)
        };
        let (rows, cols) = self.size();
        Saved {
            rows,
            cols,
            title: self.title(),
            output,
        }
    }

    /// The daemon's screen, answering, as the last daemon handed it over.
    pub fn restored(saved: &Saved) -> Screen {
        let mut screen = Screen::answering(saved.rows, saved.cols);
        screen.process(saved.output.as_bytes());
        // Drawing it asks the program nothing; anything it did ask was
        // answered before.
        screen.take_replies();
        screen.heard.lock().unwrap().title = saved.title.clone();
        screen
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

    /// The screen's rows, after the last `history` rows of the history.
    pub fn recent_rows(&self, history: usize) -> Vec<String> {
        let grid = self.term.grid();
        let first = -(grid.history_size().min(history) as i32);
        (first..grid.screen_lines() as i32)
            .map(|line| row_text(grid, Line(line)))
            .collect()
    }

    /// The history and the screen as text to read in an editor: a line
    /// that wrapped onto several rows is one line again, the blanks at the
    /// end of each are left off, and so are the empty rows after the last
    /// with something on it. Ends in a line break, as a text file does,
    /// unless there's nothing at all.
    pub fn text(&self) -> String {
        let grid = self.term.grid();
        let start = Point::new(Line(-(grid.history_size() as i32)), Column(0));
        let end = Point::new(Line(grid.screen_lines() as i32 - 1), grid.last_column());
        let text = self.term.bounds_to_string(start, end);
        let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
        let used = lines
            .iter()
            .rposition(|line| !line.is_empty())
            .map_or(0, |last| last + 1);
        let mut text = lines[..used].join("\n");
        if used > 0 {
            text.push('\n');
        }
        text
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
        let mut marks = self.marks();
        let mut text = String::new();
        for row in 0..grid.screen_lines() {
            let line = Line(row as i32 - offset);
            for col in 0..grid.columns() {
                let cell = &grid[line][Column(col)];
                // A wide character's right half is drawn by its left half.
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let wide = cell.flags.contains(Flags::WIDE_CHAR);
                text.clear();
                cell_text(cell, &mut text);
                visit(
                    row as u16,
                    col as u16,
                    &Cell {
                        text: &text,
                        style: Style::of(cell).cell_style(),
                        wide,
                        mark: marks.at(Point::new(line, Column(col)), wide),
                    },
                );
            }
        }
    }

    /// What copy mode marks on the rows showing.
    fn marks(&self) -> Marks {
        let cursor = self.copying().then_some(self.term.vi_mode_cursor.point);
        let selection = self.term.selection.as_ref();
        Marks {
            cursor,
            selection: selection.and_then(|selection| selection.to_range(&self.term)),
            found: self.matches_showing(),
            next_found: 0,
        }
    }

    /// The matches of the search on the rows showing, in order. A match
    /// that starts on a row above, and wraps onto them, counts.
    fn matches_showing(&self) -> Vec<Match> {
        let Some(search) = &self.search else {
            return Vec::new();
        };
        let grid = self.term.grid();
        let top = Line(-(grid.display_offset() as i32));
        let bottom = top + (grid.screen_lines() as i32 - 1);
        let start = self.term.line_search_left(Point::new(top, Column(0)));
        let end = self
            .term
            .line_search_right(Point::new(bottom, grid.last_column()));
        let mut regex = search.regex.borrow_mut();
        RegexIter::new(start, end, Direction::Right, &self.term, &mut regex).collect()
    }

    /// Whether copy mode is on: a cursor of its own over the screen and
    /// the history, which the program's output doesn't move.
    pub fn copying(&self) -> bool {
        self.term.mode().contains(TermMode::VI)
    }

    /// Turns copy mode on, its cursor where the program's is, or at the
    /// top of what's showing when the program's isn't in sight.
    pub fn start_copying(&mut self) {
        self.term.selection = None;
        if !self.copying() {
            self.term.toggle_vi_mode();
        }
    }

    /// Turns copy mode off, forgetting its selection and its search. What's
    /// showing stays where it is.
    pub fn stop_copying(&mut self) {
        self.term.selection = None;
        self.search = None;
        if self.copying() {
            self.term.toggle_vi_mode();
        }
    }

    /// Where copy mode's cursor is on what's showing, as `(row, col)`,
    /// while copy mode is on.
    pub fn copy_cursor(&self) -> Option<(u16, u16)> {
        if !self.copying() {
            return None;
        }
        let point = self.term.vi_mode_cursor.point;
        let row = point.line.0 + self.term.grid().display_offset() as i32;
        let row = u16::try_from(row).ok()?;
        Some((row, point.column.0 as u16))
    }

    /// Moves copy mode's cursor, taking the view with it to keep it in
    /// sight, and the end of the selection with it.
    pub fn move_copy_cursor(&mut self, motion: Motion) {
        let vi = match motion {
            Motion::Left => ViMotion::Left,
            Motion::Down => ViMotion::Down,
            Motion::Up => ViMotion::Up,
            Motion::Right => ViMotion::Right,
            Motion::LineStart => ViMotion::First,
            Motion::LineText => ViMotion::FirstOccupied,
            Motion::LineEnd => ViMotion::Last,
            Motion::ViewTop => ViMotion::High,
            Motion::ViewMiddle => ViMotion::Middle,
            Motion::ViewBottom => ViMotion::Low,
            Motion::WordNext => ViMotion::SemanticRight,
            Motion::WordBack => ViMotion::SemanticLeft,
            Motion::WordEnd => ViMotion::SemanticRightEnd,
            Motion::BigWordNext => ViMotion::WordRight,
            Motion::BigWordBack => ViMotion::WordLeft,
            Motion::BigWordEnd => ViMotion::WordRightEnd,
            Motion::ParagraphBack => ViMotion::ParagraphUp,
            Motion::ParagraphNext => ViMotion::ParagraphDown,
            Motion::Bracket => ViMotion::Bracket,
            Motion::HistoryTop => {
                let top = Point::new(self.term.topmost_line(), Column(0));
                return self.term.vi_goto_point(top);
            }
            Motion::HistoryBottom => {
                let bottom = Point::new(self.term.bottommost_line(), Column(0));
                self.term.vi_goto_point(bottom);
                ViMotion::FirstOccupied
            }
        };
        self.term.vi_motion(vi);
    }

    /// Moves the view `rows` further back into the history, or toward live
    /// when it's negative, and copy mode's cursor as far: `Ctrl+U` and
    /// `Ctrl+D`, `PageUp` and `PageDown`.
    pub fn page_copy_cursor(&mut self, rows: i32) {
        self.term.vi_mode_cursor = self.term.vi_mode_cursor.scroll(&self.term, rows);
        // Scrolling the view keeps the cursor in it, and the selection's end
        // on the cursor.
        self.term.scroll_display(Scroll::Delta(rows));
    }

    /// Puts copy mode's cursor on the cell at `(row, col)` of what's
    /// showing, as a click does.
    pub fn put_copy_cursor(&mut self, cell: (u16, u16)) {
        let point = self.point_showing(cell);
        self.term.vi_goto_point(point);
    }

    /// Starts a selection of `kind` at copy mode's cursor, which then
    /// takes its end along as it moves. Asked again for the same kind, it
    /// takes the selection away; for another kind, it changes it to that.
    pub fn toggle_selection(&mut self, kind: SelectionKind) {
        let ty = kind.alacritty();
        // A click that never dragged leaves a selection of nothing.
        if !self.selecting() {
            self.term.selection = None;
        }
        match &mut self.term.selection {
            Some(selection) if selection.ty == ty => self.term.selection = None,
            Some(selection) => selection.ty = ty,
            None => {
                let point = self.term.vi_mode_cursor.point;
                let mut selection = Selection::new(ty, point, Side::Left);
                // Both ends take in the cells they're on.
                selection.include_all();
                self.term.selection = Some(selection);
            }
        }
    }

    /// Starts a selection with the mouse, at the cell at `(row, col)` of
    /// what's showing. It holds nothing until [`Screen::select_to`] takes
    /// its end somewhere.
    pub fn select_from(&mut self, cell: (u16, u16)) {
        let point = self.point_showing(cell);
        self.term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
    }

    /// Takes the end of the selection the mouse started to the cell at
    /// `(row, col)` of what's showing: both cells, and every one between
    /// them, are in it.
    pub fn select_to(&mut self, cell: (u16, u16)) {
        let point = self.point_showing(cell);
        if let Some(selection) = &mut self.term.selection {
            selection.update(point, Side::Left);
            selection.include_all();
        }
    }

    /// Whether anything is selected.
    pub fn selecting(&self) -> bool {
        self.term
            .selection
            .as_ref()
            .is_some_and(|selection| selection.to_range(&self.term).is_some())
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// The text selected, with a line break where a line ended rather
    /// than wrapped, and none after the last; `None` when nothing is.
    pub fn selected_text(&self) -> Option<String> {
        let text = self.term.selection_to_string()?;
        let text = text.strip_suffix('\n').unwrap_or(&text).to_string();
        (!text.is_empty()).then_some(text)
    }

    /// The line copy mode's cursor is on, whole, however many rows it
    /// wrapped onto.
    pub fn copy_cursor_line(&self) -> String {
        let point = self.term.vi_mode_cursor.point;
        let start = self.term.line_search_left(point);
        let end = self.term.line_search_right(point);
        let text = self.term.bounds_to_string(start, end);
        text.trim_end_matches('\n').to_string()
    }

    /// Searches for `text`, a word or a phrase as it's written, from copy
    /// mode's cursor down to the end and on round from the top, or up when
    /// not `forward`, and puts the cursor on what it finds. Upper and
    /// lower case are the same unless `text` has a capital letter.
    pub fn search(&mut self, text: &str, forward: bool) -> Option<Found> {
        self.search = RegexSearch::new(&literal(text)).ok().map(|regex| Search {
            regex: RefCell::new(regex),
        });
        self.search_again(forward)
    }

    /// Finds the next match of the last search past copy mode's cursor,
    /// that way, and puts the cursor on it.
    pub fn search_again(&mut self, forward: bool) -> Option<Found> {
        let search = self.search.as_ref()?;
        let cursor = self.term.vi_mode_cursor.point;
        // From the cell beside the cursor, so the match it's on is passed.
        let (origin, direction) = if forward {
            let origin = cursor.add(&self.term, Boundary::None, 1);
            (origin, Direction::Right)
        } else {
            let origin = cursor.sub(&self.term, Boundary::None, 1);
            (origin, Direction::Left)
        };
        let mut regex = search.regex.borrow_mut();
        let found = self
            .term
            .search_next(&mut regex, origin, direction, Side::Left, None)?;
        let place = found_at(&self.term, &mut regex, &found);
        drop(regex);
        self.term.vi_goto_point(*found.start());
        Some(place)
    }

    /// Whether there's a search whose matches are marked.
    pub fn searched(&self) -> bool {
        self.search.is_some()
    }

    pub fn clear_search(&mut self) {
        self.search = None;
    }

    /// The point in the grid of the cell at `(row, col)` of what's showing.
    fn point_showing(&self, (row, col): (u16, u16)) -> Point {
        let grid = self.term.grid();
        let line = Line(i32::from(row) - grid.display_offset() as i32);
        Point::new(line, Column(usize::from(col))).grid_clamp(&self.term, Boundary::Grid)
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

    /// The link on the cell at `(row, col)` of what's showing, if there's
    /// one: a hyperlink the program wrote, or else a URL in the text, whole
    /// across the rows it wrapped onto.
    pub fn link_at(&self, cell: (u16, u16)) -> Option<Link> {
        let point = self.point_showing(cell);
        self.hyperlink_at(point).or_else(|| self.url_at(point))
    }

    /// The hyperlink on the cell at `point`: the cells around it with the
    /// same one, as far as what's showing goes.
    fn hyperlink_at(&self, point: Point) -> Option<Link> {
        let grid = self.term.grid();
        let link = grid[point].hyperlink()?;
        let same = |at: &Point| grid[*at].hyperlink().as_ref() == Some(&link);
        let (top, bottom) = self.lines_showing();
        let before = |at: &Point| self.cell_before(*at).filter(|at| at.line >= top);
        let after = |at: &Point| self.cell_after(*at).filter(|at| at.line <= bottom);
        let start = std::iter::successors(Some(point), before)
            .take_while(same)
            .last()?;
        let end = std::iter::successors(Some(point), after)
            .take_while(same)
            .last()?;
        Some(self.link_showing(link.uri().to_string(), start, end))
    }

    /// The URL in the text on the cell at `point`, if it's in one.
    fn url_at(&self, point: Point) -> Option<Link> {
        let mut urls = self.urls.borrow_mut();
        let regex = match &mut *urls {
            Some(regex) => regex,
            empty => empty.insert(RegexSearch::new(URL).ok()?),
        };
        let start = self.term.line_search_left(point);
        let end = self.term.line_search_right(point);
        let found = RegexIter::new(start, end, Direction::Right, &self.term, regex)
            .find(|found| found.contains(&point))?;
        let text = self.term.bounds_to_string(*found.start(), *found.end());
        let url = trim_url(&text);
        // What's taken off the end is punctuation, a cell a character.
        let mut last = *found.end();
        for _ in url.chars().count()..text.chars().count() {
            last = self.cell_before(last)?;
        }
        (point <= last).then(|| self.link_showing(url.to_string(), *found.start(), last))
    }

    /// The first and last lines showing.
    fn lines_showing(&self) -> (Line, Line) {
        let grid = self.term.grid();
        let top = Line(-(grid.display_offset() as i32));
        (top, top + (grid.screen_lines() as i32 - 1))
    }

    /// The cell before `point` in reading order, back up the rows to the
    /// top of the history.
    fn cell_before(&self, point: Point) -> Option<Point> {
        if point.column.0 > 0 {
            Some(Point::new(point.line, point.column - 1))
        } else if point.line > self.term.topmost_line() {
            Some(Point::new(point.line - 1, self.term.last_column()))
        } else {
            None
        }
    }

    /// The cell after `point` in reading order, down the rows to the
    /// bottom of the screen.
    fn cell_after(&self, point: Point) -> Option<Point> {
        if point.column < self.term.last_column() {
            Some(Point::new(point.line, point.column + 1))
        } else if point.line < self.term.bottommost_line() {
            Some(Point::new(point.line + 1, Column(0)))
        } else {
            None
        }
    }

    /// A link to `url` from `start` to `end` in the grid, placed on what's
    /// showing, and cut at its edges.
    fn link_showing(&self, url: String, start: Point, end: Point) -> Link {
        let (top, bottom) = self.lines_showing();
        let last_column = self.term.last_column();
        let place = |point: Point| {
            let point = if point.line < top {
                Point::new(top, Column(0))
            } else if point.line > bottom {
                Point::new(bottom, last_column)
            } else {
                point
            };
            ((point.line - top).0 as u16, point.column.0 as u16)
        };
        Link {
            url,
            start: place(start),
            end: place(end),
        }
    }
}

/// A URL found in the text, without what ends the sentence around it: a
/// full stop, a comma, a quote, or a closing bracket that opens nowhere in
/// it, the way `(see https://example.com/a)` reads.
fn trim_url(found: &str) -> &str {
    let mut url = found;
    loop {
        let unpaired = |open: char, close: char| {
            url.ends_with(close) && url.matches(close).count() > url.matches(open).count()
        };
        if url.ends_with(['.', ',', ':', ';', '!', '?', '\''])
            || unpaired('(', ')')
            || unpaired('[', ']')
        {
            url = &url[..url.len() - 1];
        } else {
            return url;
        }
    }
}

/// What copy mode marks on the rows showing, asked about each cell in turn
/// from the top left.
struct Marks {
    cursor: Option<Point>,
    selection: Option<SelectionRange>,
    /// The search's matches, in order.
    found: Vec<Match>,
    /// The first match that doesn't end before the cell last asked about:
    /// the cells come in order, and so do the matches.
    next_found: usize,
}

impl Marks {
    /// The mark on the cell at `point`; a `wide` one takes in the cell
    /// after it too.
    fn at(&mut self, point: Point, wide: bool) -> Mark {
        let right = Point::new(point.line, point.column + usize::from(wide));
        if self
            .cursor
            .is_some_and(|cursor| cursor == point || cursor == right)
        {
            return Mark::Cursor;
        }
        let selection = self.selection.as_ref();
        if selection.is_some_and(|range| range.contains(point) || range.contains(right)) {
            return Mark::Selected;
        }
        while self
            .found
            .get(self.next_found)
            .is_some_and(|found| *found.end() < point)
        {
            self.next_found += 1;
        }
        match self.found.get(self.next_found) {
            Some(found) if *found.start() <= right => {
                // The cursor sits at the start of the match a search put it
                // on.
                if self.cursor == Some(*found.start()) {
                    Mark::Current
                } else {
                    Mark::Found
                }
            }
            _ => Mark::None,
        }
    }
}

/// Where `found` is among every match of `regex` in the history and on the
/// screen.
fn found_at(term: &Term<Listener>, regex: &mut RegexSearch, found: &Match) -> Found {
    let start = Point::new(term.topmost_line(), Column(0));
    let end = Point::new(term.bottommost_line(), term.last_column());
    let mut place = Found { number: 0, of: 0 };
    for each in RegexIter::new(start, end, Direction::Right, term, regex) {
        place.of += 1;
        if each.start() == found.start() {
            place.number = place.of;
        }
    }
    place
}

/// A pattern that matches `text` as it's written: every character regular
/// expressions give a meaning to is escaped.
fn literal(text: &str) -> String {
    let mut pattern = String::new();
    for c in text.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern
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
/// sequence wherever the style changes, and its hyperlinks. With `trim`,
/// the blank cells at its end with no style or link of their own are left
/// out.
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
                !blank || Style::of(cell) != Style::default() || cell.hyperlink().is_some()
            })
            .map_or(0, |last| last + 1)
    } else {
        cols
    };
    let mut style = Style::default();
    let mut link = None;
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
        let cell_link = cell.hyperlink();
        if cell_link != link {
            write_hyperlink(out, cell_link.as_ref());
            link = cell_link;
        }
        cell_text(cell, out);
    }
    // A link that wraps onto the next row opens again there, with the same
    // id, which makes it the same link.
    if link.is_some() {
        write_hyperlink(out, None);
    }
}

/// Writes the OSC 8 sequence that starts `link`, with its id, or with
/// `None` the one that ends the link before.
fn write_hyperlink(out: &mut String, link: Option<&Hyperlink>) {
    match link {
        Some(link) => {
            let _ = write!(out, "\x1b]8;id={};{}\x1b\\", link.id(), link.uri());
        }
        None => out.push_str("\x1b]8;;\x1b\\"),
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
    fn the_history_keeps_as_many_rows_as_the_screen_was_made_to() {
        let mut screen = Screen::keeping(2, 10, 1);
        screen.process(b"one\r\ntwo\r\nthree\r\nfour");
        assert_eq!(screen.rows(true), ["two", "three", "four"]);
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
    fn the_text_is_the_history_then_the_screen_with_wrapped_lines_whole() {
        let screen = screen(3, 8, b"one\r\nabcdefghij\r\nthree  \r\nfour\r\n");
        // "abcdefghij" took two rows; the last row is empty.
        assert_eq!(
            screen.rows(true),
            ["one", "abcdefgh", "ij", "three", "four", ""]
        );
        assert_eq!(screen.text(), "one\nabcdefghij\nthree\nfour\n");
    }

    #[test]
    fn the_text_keeps_blank_lines_between_but_not_after() {
        let screen = screen(5, 10, b"one\r\n\r\ntwo");
        assert_eq!(screen.text(), "one\n\ntwo\n");
        assert_eq!(Screen::new(3, 10).text(), "");
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
    fn the_progress_a_program_reports_is_kept() {
        let mut screen = screen(2, 10, b"\x1b]9;4;1;-1\x07work");
        assert_eq!(screen.progress(), "4;1;-1");
        // Split anywhere, and ended with ST rather than BEL.
        for byte in b"\x1b]9;4;3\x1b\\" {
            screen.process(std::slice::from_ref(byte));
        }
        assert_eq!(screen.progress(), "4;3");
        // A notification isn't progress, and neither is a title.
        screen.process(b"\x1b]9;4 tests failed\x07\x1b]0;9;4;0\x07");
        assert_eq!(screen.progress(), "4;3");
        screen.process(b"\x1b]9;4;0\x07");
        assert_eq!(screen.progress(), "4;0");
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
    fn the_bell_is_counted_and_a_title_s_end_isnt_a_bell() {
        let mut screen = screen(2, 10, b"a\x07b\x1b]0;title\x07\x07");
        assert_eq!(screen.take_bells(), 2);
        assert_eq!(screen.take_bells(), 0);
        // What catches a new viewer up rings nothing.
        let mut viewer = Screen::new(2, 10);
        viewer.process(&screen.state_formatted(true));
        assert_eq!(viewer.take_bells(), 0);
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

    /// History scrolled off a screen of 5 rows, colors, a wrapped line, a
    /// title and modes: the main screen of the program a shell ran.
    const SHELL: &str = "\x1b]2;my title\x07\x1b[?2004h\x1b[1;32mgreen\x1b[0m\r\n";

    fn shell_output() -> Vec<u8> {
        let mut output = SHELL.as_bytes().to_vec();
        for line in 0..20 {
            output.extend(format!("line {line}\r\n").as_bytes());
        }
        output.extend(b"a line long enough to wrap\r\n$ ");
        output
    }

    #[test]
    fn a_saved_screen_comes_back_with_its_history_title_and_modes() {
        let mut original = screen(5, 20, &shell_output());
        let saved = original.save();
        let restored = Screen::restored(&saved);
        assert_eq!(restored.size(), (5, 20));
        assert_eq!(restored.rows(true), original.rows(true));
        assert_eq!(cells(&restored), cells(&original));
        assert_eq!(restored.cursor(), original.cursor());
        assert_eq!(restored.title(), "my title");
        assert!(restored.bracketed_paste());
        assert_eq!(restored.text(), original.text());
    }

    #[test]
    fn a_screen_saved_on_the_alternate_screen_keeps_the_main_one_under_it() {
        let mut output = shell_output();
        output.extend(b"\x1b[?1049h\x1b[H\x1b[2Jfull screen\x1b[3;5H");
        let mut original = screen(5, 20, &output);
        let before = (original.rows(true), cells(&original), original.cursor());

        let saved = original.save();
        // Saving left the screen as it was.
        assert_eq!(
            (original.rows(true), cells(&original), original.cursor()),
            before
        );
        let mut restored = Screen::restored(&saved);
        assert!(restored.alternate_screen());
        assert_eq!(restored.rows(true), original.rows(true));
        assert_eq!(restored.cursor(), Some((2, 4)));

        // The program leaves the alternate screen: the shell is under it,
        // with its history.
        original.process(b"\x1b[?1049l");
        restored.process(b"\x1b[?1049l");
        assert!(!restored.alternate_screen());
        assert_eq!(restored.rows(true), original.rows(true));
        assert_eq!(restored.cursor(), original.cursor());
        assert!(restored.rows(true).contains(&"line 0".to_string()));
    }

    #[test]
    fn a_saved_screen_keeps_its_hyperlinks_on_both_screens() {
        let mut output = b"\x1b]8;id=7;file:///tmp/main\x1b\\main link\x1b]8;;\x1b\\\r\n".to_vec();
        // Left open as the program goes to the alternate screen.
        output.extend(b"\x1b]8;id=8;https://example.com/open\x1b\\");
        output
            .extend(b"\x1b[?1049h\x1b]8;;\x1b\\plain \x1b]8;id=9;https://example.com/alt\x1b\\alt");
        let mut original = screen(4, 30, &output);
        let mut restored = Screen::restored(&original.save());
        // The alternate screen starts where the cursor was: the second row.
        let link = restored.link_at((1, 7)).unwrap();
        assert_eq!(link.url, "https://example.com/alt");
        assert_eq!(restored.link_at((1, 7)), original.link_at((1, 7)));
        assert_eq!(restored.link_at((1, 2)), None);
        assert_eq!(
            restored.link_at((0, 2)),
            None,
            "the main screen's stays there"
        );

        original.process(b"\x1b[?1049l");
        restored.process(b"\x1b[?1049l");
        let link = restored.link_at((0, 2)).unwrap();
        assert_eq!(link.url, "file:///tmp/main");
        assert_eq!((link.start, link.end), ((0, 0), (0, 8)));
        assert_eq!(restored.link_at((0, 2)), original.link_at((0, 2)));
    }

    #[test]
    fn a_restored_screen_answers_the_program_and_asked_nothing_itself() {
        let mut original = Screen::answering(3, 10);
        original.process(b"hi\x1b[6n");
        let mut restored = Screen::restored(&original.save());
        assert!(restored.take_replies().is_empty());
        restored.process(b"\x1b[6n");
        assert_eq!(restored.take_replies(), b"\x1b[1;3R");
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

    /// A 4-row screen, 30 wide, with numbered lines behind it, the last of
    /// them `line 9`, and the cursor on the empty row below.
    fn copying() -> Screen {
        let mut output = Vec::new();
        for line in 0..10 {
            output.extend(format!("line {line} of ten\r\n").as_bytes());
        }
        let mut screen = screen(4, 30, &output);
        screen.start_copying();
        screen
    }

    /// The mark on each cell of row `row` showing, as a letter: `.` for
    /// none, `f` found, `F` the current match, `s` selected, `c` the cursor.
    fn marks_on(screen: &Screen, row: u16) -> String {
        let mut marks = String::new();
        screen.each_cell(|at, _, cell| {
            if at == row {
                marks.push(match cell.mark {
                    Mark::None => '.',
                    Mark::Found => 'f',
                    Mark::Current => 'F',
                    Mark::Selected => 's',
                    Mark::Cursor => 'c',
                });
            }
        });
        marks.trim_end_matches('.').to_string()
    }

    #[test]
    fn copy_mode_starts_at_the_programs_cursor_and_stops_where_it_was() {
        let mut screen = copying();
        assert!(screen.copying());
        assert_eq!(screen.copy_cursor(), Some((3, 0)));
        screen.move_copy_cursor(Motion::Up);
        screen.move_copy_cursor(Motion::Right);
        assert_eq!(screen.copy_cursor(), Some((2, 1)));

        screen.stop_copying();
        assert!(!screen.copying());
        assert_eq!(screen.copy_cursor(), None);
    }

    #[test]
    fn copy_modes_cursor_takes_the_view_back_into_the_history() {
        let mut screen = copying();
        for _ in 0..5 {
            screen.move_copy_cursor(Motion::Up);
        }
        assert_eq!(screen.scrolled_back(), 2);
        assert_eq!(screen.copy_cursor(), Some((0, 0)));
        assert_eq!(screen.copy_cursor_line(), "line 5 of ten");

        screen.move_copy_cursor(Motion::HistoryTop);
        assert_eq!(screen.copy_cursor_line(), "line 0 of ten");
        assert_eq!(screen.scrolled_back(), 7);
        screen.move_copy_cursor(Motion::HistoryBottom);
        assert_eq!(screen.scrolled_back(), 0);
    }

    #[test]
    fn a_page_moves_the_view_and_the_cursor_together() {
        let mut screen = copying();
        screen.page_copy_cursor(2);
        assert_eq!(screen.scrolled_back(), 2);
        assert_eq!(screen.copy_cursor(), Some((3, 0)));
        screen.page_copy_cursor(-2);
        assert_eq!(screen.scrolled_back(), 0);
    }

    #[test]
    fn words_end_at_punctuation_and_big_words_only_at_blanks() {
        let mut screen = screen(2, 30, b"git log --oneline\r\n");
        screen.start_copying();
        screen.move_copy_cursor(Motion::Up);
        screen.move_copy_cursor(Motion::LineStart);
        screen.move_copy_cursor(Motion::WordNext);
        screen.move_copy_cursor(Motion::WordNext);
        assert_eq!(screen.copy_cursor(), Some((0, 8)));
        screen.move_copy_cursor(Motion::LineStart);
        screen.move_copy_cursor(Motion::BigWordNext);
        screen.move_copy_cursor(Motion::BigWordNext);
        assert_eq!(screen.copy_cursor(), Some((0, 8)));
        screen.move_copy_cursor(Motion::BigWordEnd);
        assert_eq!(screen.copy_cursor(), Some((0, 16)));
        screen.move_copy_cursor(Motion::LineStart);
        screen.move_copy_cursor(Motion::LineEnd);
        assert_eq!(screen.copy_cursor(), Some((0, 16)));
    }

    #[test]
    fn a_selection_follows_the_cursor_and_copies_what_it_covers() {
        let mut screen = copying();
        screen.move_copy_cursor(Motion::Up);
        screen.move_copy_cursor(Motion::Up);
        screen.move_copy_cursor(Motion::WordNext);
        screen.toggle_selection(SelectionKind::Chars);
        screen.move_copy_cursor(Motion::Down);
        assert_eq!(screen.selected_text().unwrap(), "8 of ten\nline 9");
        // The first row is selected to its end, as the line goes on.
        assert_eq!(marks_on(&screen, 1), format!(".....{}", "s".repeat(25)));
        assert_eq!(marks_on(&screen, 2), "sssssc");

        // Asked again, it goes.
        screen.toggle_selection(SelectionKind::Chars);
        assert!(!screen.selecting());
        assert_eq!(screen.selected_text(), None);
    }

    #[test]
    fn a_selection_of_lines_takes_them_whole() {
        let mut screen = copying();
        screen.move_copy_cursor(Motion::Up);
        screen.move_copy_cursor(Motion::WordEnd);
        screen.toggle_selection(SelectionKind::Lines);
        screen.move_copy_cursor(Motion::Up);
        assert_eq!(
            screen.selected_text().unwrap(),
            "line 8 of ten\nline 9 of ten"
        );
    }

    #[test]
    fn a_block_selection_takes_the_same_columns_of_each_row() {
        let mut screen = copying();
        screen.move_copy_cursor(Motion::Up);
        screen.move_copy_cursor(Motion::Up);
        screen.toggle_selection(SelectionKind::Block);
        screen.move_copy_cursor(Motion::Down);
        screen.move_copy_cursor(Motion::WordEnd);
        screen.move_copy_cursor(Motion::Right);
        screen.move_copy_cursor(Motion::Right);
        assert_eq!(screen.selected_text().unwrap(), "line 8\nline 9");
    }

    #[test]
    fn a_selection_stays_on_its_text_as_output_scrolls_it_up() {
        let mut screen = copying();
        screen.move_copy_cursor(Motion::Up);
        screen.toggle_selection(SelectionKind::Lines);
        screen.process(b"more\r\nand more\r\n");
        assert_eq!(screen.selected_text().unwrap(), "line 9 of ten");
    }

    #[test]
    fn the_mouse_selects_from_where_it_went_down_to_where_it_is() {
        let mut screen = screen(3, 20, b"one two\r\nthree four");
        screen.select_from((0, 4));
        assert!(!screen.selecting(), "a click alone selects nothing");
        screen.select_to((1, 4));
        assert_eq!(screen.selected_text().unwrap(), "two\nthree");
        // Back past where it started, it selects the other way.
        screen.select_to((0, 0));
        assert_eq!(screen.selected_text().unwrap(), "one t");
        screen.clear_selection();
        assert_eq!(screen.selected_text(), None);
    }

    #[test]
    fn in_copy_mode_a_drag_takes_the_cursor_and_the_keys_go_on_from_there() {
        let mut screen = screen(3, 20, b"one two\r\nthree four");
        screen.start_copying();
        // As the pane does it: the selection, then the cursor, each time.
        screen.select_from((0, 4));
        screen.put_copy_cursor((0, 4));
        screen.select_to((1, 4));
        screen.put_copy_cursor((1, 4));
        assert_eq!(screen.selected_text().unwrap(), "two\nthree");
        assert_eq!(screen.copy_cursor(), Some((1, 4)));
        screen.move_copy_cursor(Motion::LineEnd);
        assert_eq!(screen.selected_text().unwrap(), "two\nthree four");
    }

    #[test]
    fn the_line_under_the_cursor_is_copied_whole_across_its_wraps() {
        let mut screen = screen(4, 5, b"abcdefgh\r\n");
        screen.start_copying();
        screen.move_copy_cursor(Motion::Up);
        assert_eq!(screen.copy_cursor_line(), "abcdefgh");
    }

    #[test]
    fn a_search_finds_text_up_or_down_and_goes_round() {
        let mut screen = copying();
        let found = screen.search("line 4", false).unwrap();
        assert_eq!(found, Found { number: 1, of: 1 });
        assert_eq!(screen.copy_cursor_line(), "line 4 of ten");

        // Every line has "of": 1 of 10 at the top, down to 10 of 10.
        let found = screen.search("of", false).unwrap();
        assert_eq!(found, Found { number: 4, of: 10 });
        assert_eq!(screen.copy_cursor_line(), "line 3 of ten");
        let found = screen.search_again(true).unwrap();
        assert_eq!(found.number, 5);
        screen.move_copy_cursor(Motion::HistoryTop);
        let found = screen.search_again(false).unwrap();
        assert_eq!(found.number, 10, "up from the top goes round to the end");
    }

    #[test]
    fn a_search_is_literal_and_any_case_unless_it_has_a_capital() {
        let mut screen = screen(4, 30, b"cost: $5 (more)\r\nCost\r\n");
        screen.start_copying();
        assert_eq!(screen.search("$5 (m", false).map(|f| f.of), Some(1));
        assert_eq!(screen.search("cost", false).map(|f| f.of), Some(2));
        assert_eq!(screen.search("Cost", false).map(|f| f.of), Some(1));
        assert_eq!(screen.search("nowhere", false), None);
        assert_eq!(screen.search_again(true), None);
    }

    #[test]
    fn a_hyperlink_is_found_on_any_of_its_cells() {
        let screen = screen(
            2,
            30,
            b"see \x1b]8;;https://example.com/a\x1b\\the docs\x1b]8;;\x1b\\ now",
        );
        let link = screen.link_at((0, 6)).unwrap();
        assert_eq!(link.url, "https://example.com/a");
        assert_eq!((link.start, link.end), ((0, 4), (0, 11)));
        assert!(link.covers((0, 11)) && !link.covers((0, 12)));
        assert_eq!(screen.link_at((0, 1)), None);
        assert_eq!(screen.link_at((0, 13)), None);
    }

    #[test]
    fn a_url_in_the_text_is_a_link_without_the_punctuation_after_it() {
        let screen = screen(
            3,
            40,
            b"read https://example.com/x?q=1. then\r\n(at http://h/a_(b)) ok",
        );
        let link = screen.link_at((0, 10)).unwrap();
        assert_eq!(link.url, "https://example.com/x?q=1");
        assert_eq!((link.start, link.end), ((0, 5), (0, 29)));
        // The full stop after it isn't the link.
        assert_eq!(screen.link_at((0, 30)), None);
        let link = screen.link_at((1, 5)).unwrap();
        assert_eq!(link.url, "http://h/a_(b)");
        assert_eq!(screen.link_at((0, 2)), None);
    }

    #[test]
    fn a_url_that_wraps_is_one_link_from_either_row() {
        let screen = screen(3, 12, b"go https://example.com/abc end");
        let from_top = screen.link_at((0, 5)).unwrap();
        let from_below = screen.link_at((1, 3)).unwrap();
        assert_eq!(from_top, from_below);
        assert_eq!(from_top.url, "https://example.com/abc");
        assert_eq!((from_top.start, from_top.end), ((0, 3), (2, 1)));
    }

    #[test]
    fn a_hyperlink_reaches_a_new_viewer() {
        let original = screen(
            3,
            20,
            b"\x1b]8;id=7;file:///tmp/x\x1b\\linked\x1b]8;;\x1b\\ plain",
        );
        let copy = screen(3, 20, &original.state_formatted(true));
        let link = copy.link_at((0, 2)).unwrap();
        assert_eq!(link.url, "file:///tmp/x");
        assert_eq!((link.start, link.end), ((0, 0), (0, 5)));
        assert_eq!(copy.link_at((0, 8)), None);
    }

    #[test]
    fn the_matches_showing_are_marked_and_the_one_the_cursor_is_on_most() {
        let mut screen = copying();
        screen.search("ten", false).unwrap();
        // The cursor is on line 9's "ten", so its first cell is the cursor.
        assert_eq!(marks_on(&screen, 2), "..........cFF");
        assert_eq!(marks_on(&screen, 1), "..........fff");
        screen.clear_search();
        assert!(!screen.searched());
        assert_eq!(marks_on(&screen, 1), "");
    }
}
