//! A session's history: the rows that have scrolled up off its screen, so
//! that what an agent wrote earlier can be read back, not only what's on
//! the screen now.
//!
//! vt100 keeps the rows that scroll off the top of the whole screen. It
//! drops the ones that scroll out of a scroll region, and that's how inline
//! agents like Codex print: they set a region from the top of the screen
//! down to just above their prompt, and scroll their output up through it.
//! A real terminal keeps those rows, but vt100 would lose every one of
//! them. So [`HistoryKeeper`] catches each such scroll before vt100 sees
//! it, and turns it into steps that vt100 does keep.

/// How many rows of history each session keeps: tmux's default. A row
/// costs 32 bytes a column, so at 120 columns a full history is under 8 MB,
/// once in the daemon and again in each pane that shows the session.
pub const HISTORY_LINES: usize = 2_000;

/// Feeds a program's output to its screen, turning each scroll that vt100
/// would leave out of the history into one it keeps.
#[derive(Default)]
pub struct HistoryKeeper {
    /// Reads the output alongside vt100, to find the scrolls in it.
    parser: vte::Parser,
    /// The scroll region set on the main screen, as 0-based `(top, bottom)`
    /// rows, or `None` while it's the whole screen. The alternate screen
    /// keeps no history, so its region doesn't matter here.
    region: Option<(u16, u16)>,
}

impl HistoryKeeper {
    /// Feeds `output` to `screen`, and returns what was fed: `output`, with
    /// some scrolls rewritten. Viewers get these same bytes, so that their
    /// screens keep the same history.
    pub fn feed<C: vt100::Callbacks>(
        &mut self,
        screen: &mut vt100::Parser<C>,
        output: &[u8],
    ) -> Vec<u8> {
        let mut fed = Vec::with_capacity(output.len());
        // Everything from `start` on is still to be fed to the screen.
        let mut start = 0;
        // One byte at a time: the parser hands over plain text in runs, and
        // the keeper has to know which byte of the run is the line feed.
        for (index, byte) in output.iter().enumerate() {
            let mut finder = Finder::default();
            self.parser.advance(&mut finder, std::slice::from_ref(byte));
            let Some(found) = finder.found else {
                continue;
            };
            let read = index + 1;
            match found {
                Found::Region { top, bottom } => {
                    feed(screen, &output[start..read], &mut fed);
                    start = read;
                    if !screen.screen().alternate_screen() {
                        let (rows, _) = screen.screen().size();
                        self.region = region(top, bottom, rows);
                    }
                }
                Found::Reset => {
                    feed(screen, &output[start..read], &mut fed);
                    start = read;
                    self.region = None;
                }
                Found::LineFeed | Found::ScrollUp(_) => {
                    // The screen is brought up to just before this byte, so
                    // its cursor is where the scroll would happen. A scroll
                    // sequence is cut short there, which is fine: the
                    // escape that starts the rewrite cancels it.
                    feed(screen, &output[start..index], &mut fed);
                    start = index;
                    if let Some(rewrite) = self.rewrite(screen.screen(), &found) {
                        feed(screen, rewrite.as_bytes(), &mut fed);
                        start = read;
                    }
                }
            }
        }
        feed(screen, &output[start..], &mut fed);
        fed
    }

    /// Keeps the region in step with vt100's when the screen changes size.
    pub fn resize(&mut self, old_rows: u16, new_rows: u16) {
        let Some((top, bottom)) = self.region else {
            return;
        };
        // vt100 keeps a region that reached the bottom of the screen
        // reaching it, and cuts one that no longer fits.
        let mut bottom = if bottom == old_rows - 1 {
            new_rows - 1
        } else {
            bottom
        };
        bottom = bottom.min(new_rows - 1);
        let top = if bottom < top { 0 } else { top };
        self.region = partial(top, bottom, new_rows);
    }

    /// The steps that do what was found while keeping the rows it scrolls
    /// away, if it's a scroll vt100 would drop from the history: one of a
    /// region that starts at the top of the main screen.
    fn rewrite(&self, screen: &vt100::Screen, found: &Found) -> Option<String> {
        if screen.alternate_screen() {
            return None;
        }
        let Some((0, bottom)) = self.region else {
            return None;
        };
        let (row, col) = screen.cursor_position();
        match *found {
            // A line feed only scrolls from the bottom row of the region.
            // The cursor stays on that row.
            Found::LineFeed if row == bottom => Some(scroll_region_up(bottom, 1, (bottom, col))),
            Found::ScrollUp(lines) => Some(scroll_region_up(bottom, lines, (row, col))),
            _ => None,
        }
    }
}

/// Output that scrolls the region from the top of the screen down to
/// `bottom` up by `lines`, keeping the rows that leave it, and leaves the
/// cursor at `cursor`. The whole screen scrolls up, which vt100 keeps in
/// the history; then blank lines go in just above the rows below the
/// region, which puts those rows back where they were.
fn scroll_region_up(bottom: u16, lines: u16, cursor: (u16, u16)) -> String {
    let lines = lines.clamp(1, bottom + 1);
    let insert_at = bottom + 1 - lines;
    let (row, col) = cursor;
    // Without the region, scroll the screen; then insert the lines, put the
    // region back (which moves the cursor home) and the cursor too. All of
    // it 1-based.
    format!(
        "\x1b[r\x1b[{lines}S\x1b[{};1H\x1b[{lines}L\x1b[1;{}r\x1b[{};{}H",
        insert_at + 1,
        bottom + 1,
        row + 1,
        col + 1,
    )
}

/// The region that `CSI top ; bottom r` sets on a screen of `rows` rows,
/// worked out as vt100 does: 1-based, 0 meaning the edge of the screen,
/// and a region that makes no sense meaning the whole screen.
fn region(top: u16, bottom: u16, rows: u16) -> Option<(u16, u16)> {
    let top = top.max(1) - 1;
    let bottom = if bottom == 0 { rows } else { bottom };
    let bottom = bottom.min(rows) - 1;
    if top < bottom {
        partial(top, bottom, rows)
    } else {
        None
    }
}

/// `Some` region, unless it's the whole screen.
fn partial(top: u16, bottom: u16, rows: u16) -> Option<(u16, u16)> {
    if top == 0 && bottom == rows - 1 {
        None
    } else {
        Some((top, bottom))
    }
}

fn feed<C: vt100::Callbacks>(screen: &mut vt100::Parser<C>, bytes: &[u8], fed: &mut Vec<u8>) {
    screen.process(bytes);
    fed.extend_from_slice(bytes);
}

/// What the keeper has to act on.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// A line feed, which scrolls when the cursor is at the bottom of the
    /// region. vt100 takes vertical tabs and form feeds as line feeds too.
    LineFeed,
    /// `CSI n S`: scroll the region up `n` rows.
    ScrollUp(u16),
    /// `CSI top ; bottom r`: set the scroll region.
    Region { top: u16, bottom: u16 },
    /// `ESC c`: reset the terminal.
    Reset,
}

/// Listens to the parser for what the keeper acts on.
#[derive(Default)]
struct Finder {
    found: Option<Found>,
}

impl vte::Perform for Finder {
    fn execute(&mut self, byte: u8) {
        if matches!(byte, b'\n' | 0x0b | 0x0c) {
            self.found = Some(Found::LineFeed);
        }
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _: bool, action: char) {
        if !intermediates.is_empty() {
            return;
        }
        let mut params = params.iter().map(|param| param[0]);
        match action {
            'S' => {
                let lines = params.next().unwrap_or(0).max(1);
                self.found = Some(Found::ScrollUp(lines));
            }
            'r' => {
                let top = params.next().unwrap_or(0);
                let bottom = params.next().unwrap_or(0);
                self.found = Some(Found::Region { top, bottom });
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _: bool, byte: u8) {
        if intermediates.is_empty() && byte == b'c' {
            self.found = Some(Found::Reset);
        }
    }
}

/// The rows of the screen's history as plain text, oldest first.
pub fn text(screen: &mut vt100::Screen) -> Vec<String> {
    let (_, cols) = screen.size();
    each_row(screen, |screen| screen.rows(0, cols).collect())
}

/// Output that gives a new viewer's screen this screen's history: each row
/// of it written out, then scrolled up out of sight. What's on the screen
/// now comes after this, drawn over the top.
pub fn replay(screen: &mut vt100::Screen) -> Vec<u8> {
    let (rows, cols) = screen.size();
    let history = each_row(screen, |screen| {
        (0..rows).map(|row| styled_row(screen, row, cols)).collect()
    });
    if history.is_empty() {
        return Vec::new();
    }
    let mut replay = Vec::new();
    for row in history {
        replay.extend(row);
        replay.extend_from_slice(b"\r\n");
    }
    // The last rows written are still on the screen. A screenful of line
    // feeds, less the row the cursor is on, scrolls them up after the rest.
    replay.extend(std::iter::repeat_n(b'\n', usize::from(rows) - 1));
    replay
}

/// Every row of the screen's history, oldest first, as `page` reads it.
/// vt100 shows history a screenful at a time, so this goes back to the
/// start of it and pages forward, then puts the screen back to live.
/// `page` reads every row on the screen; at each step, the ones at the top
/// are history.
fn each_row<T>(screen: &mut vt100::Screen, page: impl Fn(&vt100::Screen) -> Vec<T>) -> Vec<T> {
    let (rows, _) = screen.size();
    screen.set_scrollback(usize::MAX);
    let mut back = screen.scrollback();
    let mut history = Vec::with_capacity(back);
    while back > 0 {
        screen.set_scrollback(back);
        let from_history = back.min(usize::from(rows));
        history.extend(page(screen).into_iter().take(from_history));
        back -= from_history;
    }
    screen.set_scrollback(0);
    history
}

/// A row as output that draws it again: its text, with a style sequence
/// wherever the style changes. Blank cells at the end with no style of
/// their own are left out.
fn styled_row(screen: &vt100::Screen, row: u16, cols: u16) -> Vec<u8> {
    let cells: Vec<&vt100::Cell> = (0..cols).filter_map(|col| screen.cell(row, col)).collect();
    let used = cells
        .iter()
        .rposition(|cell| cell.has_contents() || Style::of(cell) != Style::default())
        .map_or(0, |last| last + 1);

    let mut out = String::from("\x1b[m");
    let mut style = Style::default();
    for cell in &cells[..used] {
        // A wide character's right half is drawn by its left half.
        if cell.is_wide_continuation() {
            continue;
        }
        let cell_style = Style::of(cell);
        if cell_style != style {
            out.push_str(&cell_style.sequence());
            style = cell_style;
        }
        if cell.has_contents() {
            out.push_str(cell.contents());
        } else {
            out.push(' ');
        }
    }
    out.push_str("\x1b[m");
    out.into_bytes()
}

/// How a cell looks, beyond its text.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Style {
    fg: vt100::Color,
    bg: vt100::Color,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}

impl Style {
    fn of(cell: &vt100::Cell) -> Style {
        Style {
            fg: cell.fgcolor(),
            bg: cell.bgcolor(),
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
        }
    }

    /// The SGR sequence that sets this style from scratch.
    fn sequence(&self) -> String {
        let mut codes = vec!["0".to_string()];
        let flags = [
            (self.bold, "1"),
            (self.dim, "2"),
            (self.italic, "3"),
            (self.underline, "4"),
            (self.inverse, "7"),
        ];
        for (on, code) in flags {
            if on {
                codes.push(code.to_string());
            }
        }
        codes.extend(color(self.fg, 38));
        codes.extend(color(self.bg, 48));
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// The SGR codes for a color, given the code that introduces it: 38 for
/// the foreground, 48 for the background. The default color needs none,
/// since every style sequence starts from a reset.
fn color(color: vt100::Color, introducer: u8) -> Option<String> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(index) => Some(format!("{introducer};5;{index}")),
        vt100::Color::Rgb(r, g, b) => Some(format!("{introducer};2;{r};{g};{b}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(rows: u16, cols: u16) -> vt100::Parser {
        vt100::Parser::new(rows, cols, HISTORY_LINES)
    }

    /// What an inline agent like Codex writes to put `lines` above its
    /// prompt: a region from the top of the screen down to the row above
    /// the prompt, the cursor at the bottom of it, a line feed and a line of
    /// output for each one, and the region reset.
    fn inline_output(region_rows: u16, lines: std::ops::Range<usize>) -> Vec<u8> {
        let mut output = format!("\x1b[1;{region_rows}r\x1b[{region_rows};1H");
        for line in lines {
            output.push_str(&format!("\r\nline {line}"));
        }
        output.push_str("\x1b[r");
        output.into_bytes()
    }

    /// The screen as `output` leaves it, with and without the keeper, and
    /// what the keeper fed.
    fn both_ways(output: &[u8]) -> (vt100::Parser, vt100::Parser, Vec<u8>) {
        let mut plain = screen(10, 20);
        plain.process(output);
        let mut kept = screen(10, 20);
        let fed = HistoryKeeper::default().feed(&mut kept, output);
        (plain, kept, fed)
    }

    fn history(parser: &mut vt100::Parser) -> Vec<String> {
        text(parser.screen_mut())
            .into_iter()
            .map(|row| row.trim_end().to_string())
            .collect()
    }

    #[test]
    fn rows_scrolled_out_of_a_region_at_the_top_are_kept() {
        let (mut plain, mut kept, _) = both_ways(&inline_output(6, 0..10));
        assert!(history(&mut plain).is_empty(), "vt100 alone drops them");
        // The region's six rows start out empty, and scroll off first, as
        // they would in a terminal.
        let mut expected = vec![String::new(); 6];
        expected.extend((0..4).map(|line| format!("line {line}")));
        assert_eq!(history(&mut kept), expected);
    }

    #[test]
    fn the_screen_looks_the_same_with_the_rows_kept() {
        let mut output = b"prompt below\x1b[9;1H> type here".to_vec();
        output.extend(inline_output(6, 0..10));
        output.extend(b"\x1b[3S");
        let (plain, kept, _) = both_ways(&output);
        assert_eq!(kept.screen().contents(), plain.screen().contents());
        assert_eq!(
            kept.screen().cursor_position(),
            plain.screen().cursor_position()
        );
    }

    #[test]
    fn scrolling_a_region_up_by_several_rows_keeps_them_all() {
        let mut output = b"\x1b[1;5r".to_vec();
        for row in 1..=5 {
            output.extend(format!("\x1b[{row};1Hrow {row}").into_bytes());
        }
        output.extend(b"\x1b[3S");
        let (plain, mut kept, _) = both_ways(&output);
        assert_eq!(history(&mut kept), ["row 1", "row 2", "row 3"]);
        assert_eq!(kept.screen().contents(), plain.screen().contents());
    }

    #[test]
    fn a_region_that_does_not_start_at_the_top_is_left_alone() {
        let mut output = b"\x1b[3;6r\x1b[6;1H".to_vec();
        output.extend(b"\r\na\r\nb\r\nc");
        let (plain, mut kept, fed) = both_ways(&output);
        assert!(history(&mut kept).is_empty());
        assert_eq!(fed, output);
        assert_eq!(kept.screen().contents(), plain.screen().contents());
    }

    #[test]
    fn the_alternate_screen_is_left_alone() {
        let mut output = b"\x1b[?1049h".to_vec();
        output.extend(inline_output(6, 0..10));
        let (plain, mut kept, fed) = both_ways(&output);
        assert!(history(&mut kept).is_empty());
        assert_eq!(fed, output);
        assert_eq!(kept.screen().contents(), plain.screen().contents());
    }

    #[test]
    fn output_split_anywhere_keeps_the_same_rows() {
        let output = inline_output(6, 0..10);
        let mut whole = screen(10, 20);
        HistoryKeeper::default().feed(&mut whole, &output);

        let mut bytewise = screen(10, 20);
        let mut keeper = HistoryKeeper::default();
        for byte in &output {
            keeper.feed(&mut bytewise, std::slice::from_ref(byte));
        }
        assert_eq!(history(&mut bytewise), history(&mut whole));
        assert_eq!(bytewise.screen().contents(), whole.screen().contents());
    }

    #[test]
    fn a_viewer_fed_the_same_bytes_keeps_the_same_rows() {
        let (_, mut kept, fed) = both_ways(&inline_output(6, 0..10));
        let mut viewer = screen(10, 20);
        viewer.process(&fed);
        assert_eq!(history(&mut viewer), history(&mut kept));
        assert_eq!(viewer.screen().contents(), kept.screen().contents());
    }

    #[test]
    fn a_region_follows_the_screen_when_it_changes_size() {
        let mut keeper = HistoryKeeper {
            region: Some((0, 5)),
            ..HistoryKeeper::default()
        };
        keeper.resize(10, 20);
        assert_eq!(keeper.region, Some((0, 5)));
        keeper.resize(20, 4);
        assert_eq!(keeper.region, None, "cut to the screen, it's all of it");

        let mut reaching_the_bottom = HistoryKeeper {
            region: Some((2, 9)),
            ..HistoryKeeper::default()
        };
        reaching_the_bottom.resize(10, 30);
        assert_eq!(reaching_the_bottom.region, Some((2, 29)));
    }

    #[test]
    fn a_region_is_worked_out_as_vt100_does() {
        assert_eq!(region(1, 6, 10), Some((0, 5)));
        assert_eq!(region(0, 0, 10), None, "the whole screen");
        assert_eq!(region(3, 99, 10), Some((2, 9)));
        assert_eq!(region(6, 6, 10), None, "a region of one row");
    }

    #[test]
    fn history_comes_back_oldest_first() {
        let mut parser = screen(4, 20);
        for line in 0..10 {
            parser.process(format!("line {line}\r\n").as_bytes());
        }
        let history = history(&mut parser);
        assert_eq!(history.first().map(String::as_str), Some("line 0"));
        assert_eq!(history.last().map(String::as_str), Some("line 6"));
        assert_eq!(parser.screen().scrollback(), 0, "back to live");
    }

    #[test]
    fn history_stops_at_its_limit() {
        let mut parser = vt100::Parser::new(4, 20, 5);
        for line in 0..20 {
            parser.process(format!("line {line}\r\n").as_bytes());
        }
        let history = history(&mut parser);
        assert_eq!(history.len(), 5);
        assert_eq!(history[0], "line 12");
    }

    #[test]
    fn a_replay_gives_a_new_screen_the_same_history_in_its_colors() {
        let mut parser = screen(4, 20);
        parser.process(b"\x1b[1;31mred\x1b[m plain\r\n");
        for line in 0..6 {
            parser.process(format!("line {line}\r\n").as_bytes());
        }
        let mut viewer = screen(4, 20);
        viewer.process(&replay(parser.screen_mut()));
        viewer.process(&parser.screen().state_formatted());

        assert_eq!(history(&mut viewer), history(&mut parser));
        assert_eq!(viewer.screen().contents(), parser.screen().contents());
        viewer.screen_mut().set_scrollback(usize::MAX);
        let red = viewer.screen().cell(0, 0).unwrap();
        assert_eq!(red.fgcolor(), vt100::Color::Idx(1));
        assert!(red.bold());
    }

    #[test]
    fn nothing_to_replay_without_history() {
        assert!(replay(screen(4, 20).screen_mut()).is_empty());
    }
}
