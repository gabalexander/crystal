//! Drawing the TUI: a bar along the top with the tabs, the sidebar of
//! sessions, the tab's panes beside it, split as its tree has them, each
//! under a header line, and the footer. There are no boxes: thin rules and
//! the theme's colors tell the parts apart. Drawing only reads the state;
//! it never changes it.

use super::app::{
    App, Counted, Filter, Focus, Hit, OpenOnForge, PluginPane, Popup, Prompt, Question, Slot, View,
};
use super::archived_view;
use super::backlog_view::{self, BacklogView};
use super::command_list;
use super::copy_mode::{self, SearchPrompt};
use super::diff_view;
use super::finder;
use super::grep;
use super::help;
use super::issues;
use super::keymap::{Command, Extent, ModeKey};
use super::launcher;
use super::layouts::{self, LayoutsView};
use super::memory_view;
use super::menu;
use super::needs_you;
use super::pane::Pane;
use super::plugins_view;
use super::profiles;
use super::pull_requests;
use super::reply;
use super::restarted::Restarted;
use super::screen_widget::{Marks, ScreenWidget};
use super::scrollbar;
use super::settings_view;
use super::sidebar::{self, fit};
use super::split_tree::{Border, Way};
use super::status::Status;
use super::switcher;
use super::tabs::Tab;
use super::theme::Theme;
use super::timeline;
use super::tree_browser;
use crate::config::BarPosition;
use crate::flow_run::RunState;
use crate::model;
use crate::printable;
use crate::protocol::{SessionInfo, State, TaskState};
use crate::shell;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

/// Where the tabs start in the top bar: after crystal's name and a gap.
const TABS_START: u16 = 10;

/// The columns the top bar keeps on its right for the summary, the most
/// it's likely to need.
const SUMMARY_ROOM: u16 = 26;

/// The longest a tab's name gets in the top bar.
const TAB_NAME_LENGTH: usize = 16;

/// What drawing needs besides the state.
pub struct Look<'a> {
    pub theme: &'a Theme,
    /// Seconds since the Unix epoch, to say how long ago sessions changed.
    pub now: u64,
    /// How far the working mark has turned: a number that goes up with
    /// time.
    pub spin: usize,
}

/// Where each part of the TUI goes on a screen of a given size.
pub struct Areas {
    /// The tab bar: the top row, or the one over the footer, or no row at
    /// all while it's left out.
    pub top: Rect,
    /// Everything but the tab bar and the footer: where an open view goes,
    /// in place of the sidebar and the panes.
    pub main: Rect,
    pub sidebar: Rect,
    /// The column with the rule between the sidebar and the panes.
    pub rule: Rect,
    /// The room beside the sidebar the tab's panes share when it isn't
    /// zoomed.
    pub tiles: Rect,
    /// One per pane, in the app's [`App::slots`] order: the tab's panes,
    /// then the float's. Each is its header line, then its screen.
    pub panes: Vec<Rect>,
    /// The frame around the float, over the other panes, while one floats.
    pub float: Option<Rect>,
    pub footer: Rect,
}

impl Areas {
    /// Lays out a screen the way `app` has it: its panes split as the tab's
    /// tree has them, or zoomed, one pane taking everything but the tab bar
    /// and the footer, the sidebar and its rule with no room.
    pub fn of(app: &App, screen: Rect) -> Areas {
        let [top, main, footer] = rows(app, screen);
        // A sidebar folded away has no rule either.
        let sidebar_width = app.sidebar_columns(screen.width);
        let [sidebar, rule, tiles] = Layout::horizontal([
            Constraint::Length(sidebar_width),
            Constraint::Length(u16::from(sidebar_width > 0)),
            Constraint::Min(0),
        ])
        .areas(main);
        let mut areas = Areas {
            top,
            main,
            sidebar,
            rule,
            tiles,
            panes: (app.panes().layout(tiles).into_iter())
                .map(|(_, area)| area)
                .collect(),
            float: None,
            footer,
        };
        if app.zoomed() {
            let nowhere = Rect::new(main.x, main.y, 0, main.height);
            areas.sidebar = nowhere;
            areas.rule = nowhere;
            areas.panes = vec![main];
        }
        if app.floating().is_some() {
            areas.add_float();
        }
        areas
    }

    /// Puts a pane floating over the others, in a frame, in the middle of
    /// the room they have.
    fn add_float(&mut self) {
        let frame = float_frame(self);
        self.panes.push(Block::bordered().inner(frame));
        self.float = Some(frame);
    }

    /// The panes laid side by side or stacked: all of them but the float.
    pub fn tiled(&self) -> &[Rect] {
        let floats = usize::from(self.float.is_some());
        &self.panes[..self.panes.len() - floats]
    }
}

/// How much of the room beside the sidebar a float takes, each way, in
/// tenths.
const FLOAT_TENTHS: u16 = 8;

/// Where the frame of a float goes: over the panes, in the middle of the
/// room they have, most of it each way, but no smaller than a small
/// terminal while there's room for that.
fn float_frame(areas: &Areas) -> Rect {
    let left = areas.rule.right();
    let main = areas.main;
    let room = Rect::new(left, main.y, main.right().saturating_sub(left), main.height);
    let width = (room.width * FLOAT_TENTHS / 10).max(room.width.min(60));
    let height = (room.height * FLOAT_TENTHS / 10).max(room.height.min(12));
    Rect::new(
        room.x + (room.width - width) / 2,
        room.y + (room.height - height) / 2,
        width,
        height,
    )
}

/// The tab bar, everything else, and the footer: the bar on top or over
/// the footer, as the settings say, and with no row while it's left out.
fn rows(app: &App, screen: Rect) -> [Rect; 3] {
    let bar = Constraint::Length(u16::from(app.tab_bar_shown()));
    let (main, footer) = (Constraint::Min(0), Constraint::Length(1));
    match app.tab_bar().position {
        BarPosition::Top => Layout::vertical([bar, main, footer]).areas(screen),
        BarPosition::Bottom => {
            let [main, bar, footer] = Layout::vertical([main, bar, footer]).areas(screen);
            [bar, main, footer]
        }
    }
}

/// Where an open view's parts go: a header line across the top, then its
/// list on the left, a rule, and the rest, its content, on the right.
pub struct ViewAreas {
    pub header: Rect,
    pub list: Rect,
    pub rule: Rect,
    pub content: Rect,
}

/// Lays out `view` in `area`.
pub fn view_areas(view: &View, area: Rect) -> ViewAreas {
    let list_width = match view {
        View::Diff(_) => diff_view::list_width(area.width),
        View::Files(_) => finder::list_width(area.width),
        View::Tree(tree) => tree.list_width(area.width),
        View::Grep(_) => grep::list_width(area.width),
        View::Branches(_) => switcher::list_width(area.width),
        View::Memory(_) => memory_view::list_width(area.width),
    };
    let [header, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let [list, rule, content] = Layout::horizontal([
        Constraint::Length(list_width),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(body);
    ViewAreas {
        header,
        list,
        rule,
        content,
    }
}

/// A view's header line, `width` columns wide: its mark and `title` in the
/// accent color, its `notes`, then a rule, and `right` muted on the right
/// when it fits.
pub fn view_header<'a>(
    mark: &str,
    title: &str,
    notes: &[String],
    right: &str,
    look: &Look,
    width: u16,
) -> Line<'a> {
    let theme = look.theme;
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(mark.to_string(), Style::new().fg(theme.accent)),
        Span::raw(" "),
        Span::styled(
            title.to_string(),
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
    ];
    for note in notes {
        spans.push(Span::styled(" · ", Style::new().fg(theme.muted)));
        spans.push(Span::styled(note.clone(), Style::new().fg(theme.text)));
    }
    let left: usize = spans.iter().map(Span::width).sum();
    let width = usize::from(width);
    // A space, a few columns of rule, a space, the right side, a space.
    let needed = left + 1 + 3 + 1 + right.chars().count() + 1;
    let right_fits = needed <= width;
    let right_width = if right_fits {
        right.chars().count() + 2
    } else {
        0
    };
    let rule = width.saturating_sub(left + 1 + right_width);
    spans.push(Span::raw(" "));
    spans.push(Span::styled("─".repeat(rule), Style::new().fg(theme.rule)));
    if right_fits {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            right.to_string(),
            Style::new().fg(theme.muted),
        ));
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

/// Where a pane's session's screen goes: all of the pane below its header
/// line, but for the column on its right its scrollbar takes, with
/// `scrollbars` on. The session is sized to fit it exactly.
pub fn screen_area(pane: Rect, scrollbars: bool) -> Rect {
    let [_header, screen] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(pane);
    match scrollbar_area(pane, scrollbars) {
        Some(_) => Rect {
            width: screen.width - 1,
            ..screen
        },
        None => screen,
    }
}

/// The narrowest a pane is to have a scrollbar beside its screen.
const SCROLLBAR_FROM: u16 = 8;

/// Where a pane's scrollbar goes, with `scrollbars` on: the column on its
/// right, beside its screen. A pane too narrow to spare one has none.
pub fn scrollbar_area(pane: Rect, scrollbars: bool) -> Option<Rect> {
    if !scrollbars || pane.width < SCROLLBAR_FROM || pane.height < 2 {
        return None;
    }
    Some(Rect::new(pane.right() - 1, pane.y + 1, 1, pane.height - 1))
}

/// The area of the pane at `slot`, as `areas` lays it out.
fn pane_area(areas: &Areas, app: &App, slot: Slot) -> Option<Rect> {
    let index = app.slots().iter().position(|at| *at == slot)?;
    areas.panes.get(index).copied()
}

/// The cell of the screen of the pane at `slot` nearest `(column, row)`:
/// where a drag that started in that pane has got to, once it's left it
/// too.
pub fn nearest_cell(
    areas: &Areas,
    app: &App,
    slot: Slot,
    column: u16,
    row: u16,
) -> Option<(u16, u16)> {
    let screen = screen_area(pane_area(areas, app, slot)?, app.scrollbars());
    if screen.is_empty() {
        return None;
    }
    let column = column.clamp(screen.x, screen.right() - 1);
    let row = row.clamp(screen.y, screen.bottom() - 1);
    Some((row - screen.y, column - screen.x))
}

/// How many rows above the screen of the pane at `slot` the mouse is, at
/// `row`, as less than 0, or below it, or 0 when it's level with it: how
/// far past the edge a drag selecting there has gone.
pub fn rows_past(areas: &Areas, app: &App, slot: Slot, row: u16) -> i32 {
    let Some(pane) = pane_area(areas, app, slot) else {
        return 0;
    };
    let screen = screen_area(pane, app.scrollbars());
    let row = i32::from(row);
    let (top, bottom) = (i32::from(screen.y), i32::from(screen.bottom()));
    if row < top {
        row - top
    } else if row >= bottom {
        row - bottom + 1
    } else {
        0
    }
}

/// The row of the scrollbar of the pane at `slot` nearest the screen's
/// `row`: where a drag of its thumb has got to, wherever the mouse is.
pub fn scrollbar_row(areas: &Areas, app: &App, slot: Slot, row: u16) -> Option<u16> {
    let track = scrollbar_area(pane_area(areas, app, slot)?, app.scrollbars())?;
    Some(row.clamp(track.y, track.bottom() - 1) - track.y)
}

/// What's at `(column, row)` on a screen laid out as `areas`, for `app`.
pub fn hit(areas: &Areas, app: &App, column: u16, row: u16) -> Hit {
    let at = |area: Rect| area.contains((column, row).into());
    if let Some(view) = app.view() {
        let parts = view_areas(view, areas.main);
        // The tree browser's border is the mouse's while it's dragged.
        if let View::Tree(tree) = view
            && (tree.dragging || at(parts.rule))
        {
            return Hit::ViewBorder(column.saturating_sub(areas.main.x));
        }
        if at(parts.list) {
            return match view {
                View::Diff(diff) => diff_view::list_hit(diff, parts.list, row),
                View::Files(finder) => finder::list_hit(finder, parts.list, row),
                View::Tree(tree) => tree_browser::list_hit(tree, parts.list, row),
                View::Grep(grep) => grep::list_hit(grep, parts.list, row),
                View::Branches(switcher) => switcher::list_hit(switcher, parts.list, row),
                View::Memory(memory) => memory_view::list_hit(memory, parts.list, row),
            };
        }
        if at(parts.content) {
            return Hit::ViewContent;
        }
        return Hit::Elsewhere;
    }
    if at(areas.top) {
        return tab_hit(app, areas.top, column);
    }
    if at(areas.sidebar) {
        return sidebar::hit(areas.sidebar, app, row);
    }
    // The rule beside the sidebar is its edge, which the mouse drags.
    if at(areas.rule) && !app.zoomed() {
        return Hit::SidebarEdge(column);
    }
    // The float is over the others, its frame and all: the last pane
    // first.
    let panes = app.slots().into_iter().zip(&areas.panes).rev();
    for (slot, area) in panes {
        let frame = match (slot, areas.float) {
            (Slot::Float, Some(frame)) => frame,
            _ => *area,
        };
        if at(frame) {
            if let Some(track) = scrollbar_area(*area, app.scrollbars())
                && at(track)
            {
                let row = row - track.y;
                return Hit::Scrollbar { slot, row };
            }
            let screen = screen_area(*area, app.scrollbars());
            let cell = at(screen).then(|| (row - screen.y, column - screen.x));
            // A header line below another pane is the border between
            // them, but for the name on it, which takes the pane to move.
            let on_name = column < area.x + name_width(app, slot);
            if cell.is_none() && slot != Slot::Float && !on_name {
                let border = borders(areas, app)
                    .into_iter()
                    .find(|border| at(border.line));
                if let Some(border) = border {
                    return Hit::Border {
                        split: border.split,
                        at: row,
                    };
                }
            }
            return Hit::Pane { slot, cell };
        }
    }
    // Between panes side by side, the rule is their border.
    let border = borders(areas, app)
        .into_iter()
        .find(|border| at(border.line));
    if let Some(border) = border {
        return border_hit(areas, app, border.split, column, row);
    }
    Hit::Elsewhere
}

/// The borders between the tab's panes on screen: none while it's zoomed.
fn borders(areas: &Areas, app: &App) -> Vec<Border> {
    if app.zoomed() {
        return Vec::new();
    }
    app.panes().borders(areas.tiles)
}

/// How much of the header line of the pane at `slot` its session's mark and
/// name take, from its left edge.
fn name_width(app: &App, slot: Slot) -> u16 {
    let session = app.pane_session(slot);
    session.map_or(0, |session| 3 + width_of(&session.name))
}

/// The border [`SplitTree::borders`] counts as `split` with the mouse at
/// `(column, row)`: where along the screen a drag that took it has got to.
///
/// [`SplitTree::borders`]: super::split_tree::SplitTree::borders
pub fn border_hit(areas: &Areas, app: &App, split: usize, column: u16, row: u16) -> Hit {
    let border = borders(areas, app).into_iter().nth(split);
    match border.map(|border| border.way) {
        Some(Way::Right) => Hit::Border { split, at: column },
        Some(Way::Down) => Hit::Border { split, at: row },
        None => Hit::Elsewhere,
    }
}

/// Draws the whole TUI. `panes` are the viewers of the sessions on screen.
/// The frame is scrubbed last, so that whatever a session's name, a pull
/// request, a file or anything else crystal didn't write holds, the
/// terminal only draws it: see [`printable`].
pub fn draw(frame: &mut Frame, app: &App, panes: &[Pane], overlay: Option<&Pane>, look: &Look) {
    draw_everything(frame, app, panes, overlay, look);
    printable::scrub(frame.buffer_mut());
}

/// What [`draw`] draws, before it's scrubbed.
fn draw_everything(
    frame: &mut Frame,
    app: &App,
    panes: &[Pane],
    overlay: Option<&Pane>,
    look: &Look,
) {
    frame.render_widget(Block::new().style(look.theme.base()), frame.area());
    let areas = Areas::of(app, frame.area());
    draw_top_bar(frame, app, look, areas.top);
    if let Some(view) = app.view() {
        let parts = view_areas(view, areas.main);
        match view {
            View::Diff(diff) => diff_view::draw(frame, diff, look, &parts),
            View::Files(files) => finder::draw(frame, files, look, &parts),
            View::Tree(tree) => tree_browser::draw(frame, tree, look, &parts),
            View::Grep(grep) => grep::draw(frame, grep, look, &parts),
            View::Branches(switcher) => switcher::draw(frame, switcher, look, &parts),
            View::Memory(memory) => memory_view::draw(frame, memory, look, &parts),
        }
        draw_view_footer(frame, app, view, look, areas.footer);
        return;
    }
    sidebar::draw(frame, app, look, areas.sidebar);
    draw_rule(frame, look, areas.rule);
    let slots = app.slots();
    for (slot, area) in slots.iter().zip(areas.tiled()) {
        draw_pane(frame, app, look, *slot, *area, panes);
    }
    draw_borders(frame, app, look, &areas);
    if let (Some(around), Some(area)) = (areas.float, areas.panes.last()) {
        draw_float_frame(frame, app, look, around);
        draw_pane(frame, app, look, Slot::Float, *area, panes);
    }
    // Zoomed or folded, the sidebar comes out over the panes while `/`
    // looks through it, rather than squeezing them, which their programs
    // would redraw for.
    if (app.zoomed() || app.sidebar_folded()) && app.filter().is_some() {
        let main = areas.main;
        let width = app.sidebar_shape().width.min(main.width.saturating_sub(1));
        let drawer = Rect::new(main.x, main.y, width, main.height);
        let rule = Rect::new(drawer.right(), main.y, 1, main.height);
        frame.render_widget(Clear, drawer.union(rule));
        frame.render_widget(Block::new().style(look.theme.base()), drawer.union(rule));
        sidebar::draw(frame, app, look, drawer);
        draw_rule(frame, look, rule);
    }
    // Over everything but the tab bar and the footer.
    let middle = Rect::new(0, areas.main.y, frame.area().width, areas.main.height);
    if let Some(view) = app.issues_view() {
        issues::draw(frame, view, look.theme, look.now, middle);
    }
    if let Some(view) = app.pull_requests_view() {
        pull_requests::draw(frame, view, look.theme, look.now, middle);
    }
    if let Some(view) = app.backlog_view() {
        backlog_view::draw(frame, view, look.theme, look.now, middle);
    }
    if let Some(view) = app.layouts_view() {
        layouts::draw(frame, view, look.theme, look.now, middle);
    }
    if let Some(view) = app.archived_view() {
        archived_view::draw(frame, view, look.theme, look.now, middle);
    }
    if let Some(view) = app.timeline_view() {
        timeline::draw(frame, view, look.theme, look.now * 1000, middle);
    }
    if let Some(view) = app.needs_you_view() {
        needs_you::draw(frame, view, look.theme, look.now, middle);
    }
    if let Some(panel) = app.launcher() {
        // Over the panes, beside the sidebar.
        let left = areas.rule.right();
        let over = Rect::new(
            left,
            areas.main.y,
            frame.area().width - left,
            areas.main.height,
        );
        launcher::draw(frame, panel, look.theme, over);
    }
    if let Some(reply) = app.reply() {
        reply::draw(frame, reply, look.theme, middle);
    }
    if let Some(view) = app.profiles_view() {
        profiles::draw(frame, view, look.theme, middle);
    }
    if let Some(view) = app.plugins_view() {
        plugins_view::draw(frame, view, look.theme, middle);
    }
    if let Some(view) = app.settings_view() {
        settings_view::draw(frame, view, look.theme, middle);
    }
    if let (Some(open), Some(pane)) = (app.plugin_pane(), overlay) {
        let hand_back = app.keymap().hand_back().hint();
        draw_plugin_pane(frame, open, pane, look, &areas, &hand_back);
    }
    if let Some(list) = app.command_list() {
        command_list::draw(frame, list, look.theme, middle);
    }
    draw_footer(frame, app, panes, look, areas.footer);
    if let Some(open) = app.menu() {
        menu::draw(frame, open, look.theme, frame.area());
    }
    if app.showing_keys() {
        let plugin_on = |plugin: &str| app.plugin_on(plugin);
        let plugin_keys = app.plugin_key_rows();
        let shown = help::Shown {
            plugin_on: &plugin_on,
            plugin_keys: &plugin_keys,
            keymap: app.keymap(),
        };
        help::draw(frame, look.theme, frame.area(), &shown, app.keys_page());
    }
}

/// Where a plugin's pane goes: over every pane, beside the sidebar; or a
/// popup, as big as it says, in the middle of everything but the tab bar
/// and the footer.
fn plugin_pane_area(areas: &Areas, popup: Option<&Popup>) -> Rect {
    let main = areas.main;
    let Some(popup) = popup else {
        let left = areas.rule.right();
        return Rect::new(left, main.y, main.right().saturating_sub(left), main.height);
    };
    // A frame and a cell inside it, however small the terminal.
    let width = Extent::of(popup.width.as_ref(), main.width).max(3);
    let height = Extent::of(popup.height.as_ref(), main.height).max(3);
    let (width, height) = (width.min(main.width), height.min(main.height));
    let x = main.x + (main.width - width) / 2;
    let y = main.y + (main.height - height) / 2;
    Rect::new(x, y, width, height)
}

/// The part of a plugin's pane or a popup its program's screen takes:
/// inside its frame.
pub fn plugin_pane_screen(areas: &Areas, popup: Option<&Popup>) -> Rect {
    Block::bordered().inner(plugin_pane_area(areas, popup))
}

/// A plugin's pane: its program's screen in a frame, its title on top and
/// the key that closes it below.
fn draw_plugin_pane(
    frame: &mut Frame,
    open: &PluginPane,
    pane: &Pane,
    look: &Look,
    areas: &Areas,
    look_hand_back: &str,
) {
    let theme = look.theme;
    let area = plugin_pane_area(areas, open.popup.as_ref());
    frame.render_widget(Clear, area);
    let title = Style::new().fg(theme.accent).add_modifier(Modifier::BOLD);
    let block = Block::bordered()
        .border_style(Style::new().fg(theme.accent))
        .style(theme.base())
        .title(Line::styled(open.heading(), title))
        .title_bottom(Line::styled(
            format!(" {} closes ", look_hand_back),
            Style::new().fg(theme.muted),
        ));
    let screen = block.inner(area);
    frame.render_widget(block, area);
    let widget =
        ScreenWidget::new(&pane.screen).with_defaults(look.theme.text, look.theme.background);
    frame.render_widget(widget, screen);
    if let Some((row, col)) = pane.screen.cursor()
        && row < screen.height
        && col < screen.width
    {
        frame.set_cursor_position((screen.x + col, screen.y + row));
    }
}

/// crystal's name and the tabs on the left, and on the right the server,
/// when it isn't the default, then how many sessions there are and how
/// many wait on the user, then, when there's room, how many pull requests
/// and issues are open on the selected session's project's forge.
fn draw_top_bar(frame: &mut Frame, app: &App, look: &Look, area: Rect) {
    // Left out, it has no row to draw on.
    if area.height == 0 {
        return;
    }
    let theme = look.theme;
    let name = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "crystal",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(name, area);
    draw_tabs(frame, app, look, area);
    let mut right = summary(app.sessions(), app.server(), theme);
    let counts = counts_width(app, area.width) > 0;
    let status = status_shown(app, area.width);
    if counts || status.is_some() {
        // After the count, before the space the summary ends with.
        let end = right.spans.pop();
        let separator = Span::styled(
            app.tab_bar().separator.clone(),
            Style::new().fg(theme.muted),
        );
        let mut shown = Vec::new();
        if counts {
            shown.extend(forge_counts(app.open_on_forge(), theme));
        }
        let text = Style::new().fg(theme.text);
        shown.extend(
            status
                .into_iter()
                .flatten()
                .map(|said| Span::styled(said, text)),
        );
        for said in shown {
            right.spans.push(separator.clone());
            right.spans.push(said);
        }
        right.spans.extend(end);
    }
    frame.render_widget(right.right_aligned(), area);
}

/// The columns the tabs keep at least, before what the tab bar shows at
/// its right is left out to give them room.
const TABS_KEEP: u16 = 12;

/// What the tab bar shows at its right, after the count, on a bar `width`
/// columns wide: each thing with something to show, unless all of them
/// together would leave the tabs less than [`TABS_KEEP`].
fn status_shown(app: &App, width: u16) -> Option<Vec<String>> {
    let shown: Vec<String> = (app.tab_bar().status.iter())
        .filter(|text| !text.is_empty())
        .cloned()
        .collect();
    if shown.is_empty() {
        return None;
    }
    let separator = width_of(&app.tab_bar().separator);
    let needs: u16 = shown.iter().map(|text| separator + width_of(text)).sum();
    (TABS_START + SUMMARY_ROOM + TABS_KEEP + needs <= width).then_some(shown)
}

/// How many columns what the tab bar shows at its right takes.
fn status_width(app: &App, width: u16) -> u16 {
    let separator = width_of(&app.tab_bar().separator);
    status_shown(app, width).map_or(0, |shown| {
        shown.iter().map(|text| separator + width_of(text)).sum()
    })
}

/// What the tab bar counts on the forge of the selected session's
/// project, after the sessions: `3 prs` in the accent and `5 issues` in
/// green.
fn forge_counts<'a>(open: Option<OpenOnForge>, theme: &Theme) -> Vec<Span<'a>> {
    count_words(open)
        .into_iter()
        .map(|(said, pull_requests)| {
            let color = if pull_requests {
                theme.accent
            } else {
                theme.done
            };
            Span::styled(said, Style::new().fg(color))
        })
        .collect()
}

/// The counts [`forge_counts`] draws, each with whether it counts pull
/// requests: `3 prs`, `mrs` on GitLab, and `5 issues`. A count of none says
/// nothing, and a list the forge cut short counts `100+`.
fn count_words(open: Option<OpenOnForge>) -> Vec<(String, bool)> {
    let Some(open) = open else {
        return Vec::new();
    };
    let (pr, prs) = match open.forge {
        crate::forge::Forge::GitHub => ("pr", "prs"),
        crate::forge::Forge::GitLab => ("mr", "mrs"),
    };
    let said = |counted: Option<Counted>, one: &str, many: &str| {
        let counted = counted.filter(|counted| counted.count > 0)?;
        let plus = if counted.more { "+" } else { "" };
        let noun = if counted.count == 1 && !counted.more {
            one
        } else {
            many
        };
        Some(format!("{}{plus} {noun}", counted.count))
    };
    let pull_requests = said(open.pull_requests, pr, prs).map(|said| (said, true));
    let issues = said(open.issues, "issue", "issues").map(|said| (said, false));
    pull_requests.into_iter().chain(issues).collect()
}

/// How many columns the forge's counts take in a tab bar `width` columns
/// wide: none when there are none, or they'd leave the tabs fewer than
/// [`TABS_KEEP`] columns beside what the bar shows at its right, which
/// they give way to.
fn counts_width(app: &App, width: u16) -> u16 {
    let separator = width_of(&app.tab_bar().separator);
    let words = count_words(app.open_on_forge());
    let counts: u16 = words
        .iter()
        .map(|(said, _)| separator + width_of(said))
        .sum();
    let beside = server_width(app) + status_width(app, width);
    let room = TABS_START + SUMMARY_ROOM + beside + counts + TABS_KEEP;
    if counts > 0 && width >= room {
        counts
    } else {
        0
    }
}

/// The tabs, after crystal's name in the top bar in `area`: the one in
/// front stands out the way the sidebar's selection does, the others are
/// muted. A tab with something going on in it ends in that thing's mark,
/// in its color: `▲` for an agent waiting on the user.
fn draw_tabs(frame: &mut Frame, app: &App, look: &Look, area: Rect) {
    let theme = look.theme;
    let in_front = app.tabs().current_index();
    let statuses = tab_statuses(app);
    let labels = tab_labels(app.tabs().all(), &statuses, tabs_width(app, area), in_front);
    for (index, column, label) in labels {
        let style = if index == in_front {
            theme
                .selection
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.muted)
        };
        let width = width_of(&label);
        let place = Rect::new(area.x + column, area.y, width, 1);
        frame.render_widget(Span::styled(label, style), place);
        // The mark is the label's last but one column; drawn again over
        // it, it turns as the sidebar's does, and takes its own color.
        if let Some(status) = statuses[index] {
            let mark_at = Rect::new(place.right() - 2, area.y, 1, 1);
            let color = Style::new().fg(theme.status(status));
            let mark = Span::styled(status.mark(look.spin), color);
            frame.render_widget(mark, mark_at);
        }
    }
}

/// What each tab's label ends with: see [`App::tab_status`].
fn tab_statuses(app: &App) -> Vec<Option<Status>> {
    let count = app.tabs().all().len();
    (0..count).map(|index| app.tab_status(index)).collect()
}

/// The tabs' labels in a top bar `width` columns wide, each with the tab's
/// index and the column it starts at: ` 1 `, or ` 2 review ` once the tab
/// has a name, and ` 2 review ▲ ` with its status's mark when `statuses`
/// gives it one. While they all fit they show their names; when they
/// don't, only their numbers, and those that still don't fit are left off,
/// from the end, or from the start as far as it takes to show the tab at
/// `in_front`.
pub fn tab_labels(
    tabs: &[Tab],
    statuses: &[Option<Status>],
    width: u16,
    in_front: usize,
) -> Vec<(usize, u16, String)> {
    let room = width.saturating_sub(TABS_START + SUMMARY_ROOM);
    let labels = |named: bool| -> Vec<String> {
        let numbered = tabs.iter().zip(statuses).enumerate();
        numbered
            .map(|(index, (tab, status))| tab_label(index + 1, tab, *status, named))
            .collect()
    };
    let mut shown = labels(true);
    let all_named: u16 = shown.iter().map(|label| width_of(label)).sum();
    if all_named > room {
        shown = labels(false);
    }
    let widths: Vec<u16> = shown.iter().map(|label| width_of(label)).collect();
    // The first tab shown: the first of all, unless the one in front then
    // wouldn't fit.
    let mut first = 0;
    let through_front = |first: usize| -> u16 { widths[first..=in_front].iter().sum() };
    while in_front < widths.len() && first < in_front && through_front(first) > room {
        first += 1;
    }
    let mut placed = Vec::new();
    let mut used = 0;
    for (index, label) in shown.into_iter().enumerate().skip(first) {
        let width = widths[index];
        if used + width > room {
            break;
        }
        placed.push((index, TABS_START + used, label));
        used += width;
    }
    placed
}

/// Tab `number`'s label: its number, its name if it has one and `named`
/// says to show it, and its status's mark, if it has one.
fn tab_label(number: usize, tab: &Tab, status: Option<Status>, named: bool) -> String {
    let mark = status.map_or(String::new(), |status| format!(" {}", status.mark(0)));
    if named && !tab.name.is_empty() {
        format!(" {number} {}{mark} ", fit(&tab.name, TAB_NAME_LENGTH))
    } else {
        format!(" {number}{mark} ")
    }
}

/// How much of the top bar in `area` the tabs share with the summary: all
/// of it, but for the server's name the summary starts with, if it does,
/// what the bar shows at its right, and the forge's counts before that,
/// if they're shown.
fn tabs_width(app: &App, area: Rect) -> u16 {
    let status = status_width(app, area.width);
    let counts = counts_width(app, area.width);
    area.width
        .saturating_sub(server_width(app) + status + counts)
}

/// How many columns the server's name takes at the start of the summary.
fn server_width(app: &App) -> u16 {
    app.server()
        .map_or(0, |server| width_of(&server_label(server)))
}

/// What the summary says of the server, before the sessions.
fn server_label(server: &str) -> String {
    format!("{server} · ")
}

/// The tab drawn at `column` of the top bar in `area`, if there's one
/// there.
fn tab_hit(app: &App, area: Rect, column: u16) -> Hit {
    let column = column - area.x;
    let labels = tab_labels(
        app.tabs().all(),
        &tab_statuses(app),
        tabs_width(app, area),
        app.tabs().current_index(),
    );
    let under = labels.iter().find(|(_, start, label)| {
        let end = start + width_of(label);
        (*start..end).contains(&column)
    });
    under.map_or(Hit::Elsewhere, |(index, _, _)| Hit::Tab(*index))
}

/// How many columns `text` takes on screen.
fn width_of(text: &str) -> u16 {
    Span::raw(text).width() as u16
}

/// "6 sessions · 2 waiting": the waiting count only when some are, in the
/// color that says so; "work · 6 sessions" on a server that isn't the
/// default.
pub fn summary<'a>(sessions: &[SessionInfo], server: Option<&str>, theme: &Theme) -> Line<'a> {
    let count = sessions.len();
    let noun = if count == 1 { "session" } else { "sessions" };
    let mut spans: Vec<Span> = server
        .map(|server| Span::styled(server_label(server), Style::new().fg(theme.muted)))
        .into_iter()
        .collect();
    spans.push(Span::styled(
        format!("{count} {noun}"),
        Style::new().fg(theme.muted),
    ));
    let waiting = sessions
        .iter()
        .filter(|session| Status::of(session) == Status::Waiting)
        .count();
    if waiting > 0 {
        spans.push(Span::styled(" · ", Style::new().fg(theme.muted)));
        spans.push(Span::styled(
            format!("{waiting} waiting"),
            Style::new().fg(theme.waiting),
        ));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// A thin vertical rule down `area`.
pub fn draw_rule(frame: &mut Frame, look: &Look, area: Rect) {
    draw_rule_in(frame, look.theme.rule, area);
}

/// A thin vertical rule down `area`, in `color`.
fn draw_rule_in(frame: &mut Frame, color: Color, area: Rect) {
    let lines: Vec<Line> = (0..area.height).map(|_| Line::from("│")).collect();
    let rule = Paragraph::new(lines).style(Style::new().fg(color));
    frame.render_widget(rule, area);
}

/// The frame around the float, over the panes under it: in the accent
/// color while it has the keyboard, with how to put it back below.
fn draw_float_frame(frame: &mut Frame, app: &App, look: &Look, around: Rect) {
    let theme = look.theme;
    let focused = matches!(
        app.focus(),
        Focus::Pane(Slot::Float) | Focus::Copy(Slot::Float)
    );
    let color = if focused { theme.accent } else { theme.rule };
    let hint = if app.focus() == Focus::Sidebar {
        " F puts it back "
    } else {
        " ctrl+\\ sidebar · then F puts it back "
    };
    let block = Block::bordered()
        .border_style(Style::new().fg(color))
        .style(theme.base())
        .title_bottom(Line::styled(hint, Style::new().fg(theme.muted)));
    frame.render_widget(Clear, around);
    frame.render_widget(block, around);
}

/// The rules between panes side by side, in the column each split keeps
/// between them: in the accent color while the mouse is moving one.
fn draw_borders(frame: &mut Frame, app: &App, look: &Look, areas: &Areas) {
    let rules = borders(areas, app).into_iter();
    for border in rules.filter(|border| border.way == Way::Right) {
        let color = if app.moving_border() == Some(border.split) {
            look.theme.accent
        } else {
            look.theme.rule
        };
        draw_rule_in(frame, color, border.line);
    }
}

/// Draws the pane at `slot` in `area`: a header line naming its session,
/// then the session's screen, or a word on why there's none to show. Only
/// the pane with the keyboard shows the cursor.
fn draw_pane(frame: &mut Frame, app: &App, look: &Look, slot: Slot, area: Rect, panes: &[Pane]) {
    let copying = app.focus() == Focus::Copy(slot);
    let focused = app.focus() == Focus::Pane(slot) || copying;
    let session = app.pane_session(slot);
    // Until a viewer has attached to the session, there's nothing to show
    // yet.
    let pane = pane_in(app, panes, slot);
    let back = pane.map_or(0, Pane::scrolled_back);
    let screen = screen_area(area, app.scrollbars());
    let header = Rect::new(area.x, area.y, area.width, 1);

    let Some(session) = session else {
        if slot != Slot::Selected {
            return;
        }
        if let Some(worktree) = app.selected_empty_worktree() {
            let branch = worktree.branch.as_deref().unwrap_or("(detached)");
            let message = format!("No sessions in ⎇ {branch}");
            draw_message(frame, look, &message, screen);
        } else if app.sessions().is_empty() {
            draw_message(frame, look, "No sessions yet: n starts one", screen);
        } else {
            draw_message(frame, look, "Nothing in this tab yet", screen);
        }
        return;
    };
    let notes = header_notes(app, slot, session, back);
    let line = pane_header(session, &notes, focused, look, header.width);
    frame.render_widget(line, header);

    if slot == Slot::Selected && app.is_own(session) {
        let message = "This is the session crystal is running in.";
        draw_message(frame, look, message, screen);
        return;
    }
    if !app.shows_screen(slot) {
        // The selected session is split off, or floats: point at its pane
        // rather than draw it twice at two sizes.
        let floats = app
            .floating()
            .is_some_and(|float| float.name == session.name);
        let message = if floats {
            format!("{} floats over the panes", session.name)
        } else {
            format!("{} has a pane of its own", session.name)
        };
        draw_message(frame, look, &message, screen);
        return;
    }
    let Some(pane) = pane else {
        return;
    };
    let theme = look.theme;
    let marks = Marks {
        selected: theme.copy_selection,
        found: theme.found,
        current: theme.found_current,
        ..Marks::default()
    };
    // The link under the mouse, with Ctrl held, is underlined.
    let link = app
        .link_hover()
        .filter(|(over, _)| *over == slot)
        .and_then(|(_, cell)| pane.screen.link_at(cell));
    let widget = ScreenWidget::new(&pane.screen)
        .with_defaults(theme.text, theme.background)
        .with_marks(marks)
        .with_link(link);
    frame.render_widget(widget, screen);
    if let (Some(track), Some(thumb)) = (scrollbar_area(area, app.scrollbars()), pane.thumb()) {
        // The thumb stands out while the mouse holds it.
        let held = app.holding_thumb() == Some(slot);
        let thumb_color = if held { theme.accent } else { theme.muted };
        let line = Style::new().fg(theme.rule);
        let held = Style::new().fg(thumb_color);
        scrollbar::draw(frame.buffer_mut(), track, thumb, line, held);
    }
    // In copy mode, the cursor is copy mode's. Back in the history, the
    // program's cursor's place on the live screen means nothing.
    let cursor = if copying {
        pane.screen.copy_cursor()
    } else if focused && back == 0 {
        pane.screen.cursor()
    } else {
        None
    };
    if let Some((row, col)) = cursor
        && row < screen.height
        && col < screen.width
    {
        frame.set_cursor_position((screen.x + col, screen.y + row));
    }
}

/// The short notes after a pane's session name: how it ended, that it's the
/// selected session when a split shows it, and how far back in its history
/// the pane is.
fn header_notes(app: &App, slot: Slot, session: &SessionInfo, back: usize) -> Vec<String> {
    let mut notes = Vec::new();
    if session.state != State::Running {
        notes.push(session.status());
    }
    // Zoomed, the one pane is the selected session's, and says why the
    // sidebar has gone.
    let selected = app.selected().is_some_and(|s| s.name == session.name);
    if slot == Slot::Float {
        notes.push("floating".to_string());
    } else if app.zoomed() {
        notes.push("zoomed".to_string());
    } else if slot != Slot::Selected && selected {
        notes.push("selected".to_string());
    }
    if app.focus() == Focus::Copy(slot) {
        notes.push("copy mode".to_string());
    }
    if app.resizing() && selected && slot != Slot::Float {
        notes.push("resizing".to_string());
    }
    if let Some(grab) = app.grabbed() {
        if grab.from == slot {
            notes.push("moving".to_string());
        } else if grab.over == Some(slot) {
            notes.push("let go to swap".to_string());
        }
    }
    if let Some(asking) = &session.asking {
        let gist = fit(&asking.gist, TASK_NOTE_LENGTH);
        notes.push(format!("⚠ {} {gist} · y/n/Y", asking.tool));
    }
    // How full a background task's conversation is.
    if let Some(context) = session.context {
        notes.push(format!("ctx {}%", context.percent()));
    }
    let index = app.sessions().iter().position(|s| s.name == session.name);
    let flow_step = index.and_then(|index| app.flow_step_of(index));
    if let Some((run, step)) = flow_step {
        // A step's task is the step: the run and how far it's got say more.
        let state = run.steps[step].state.word();
        notes.push(format!("{} {} · {state}", run.name, run.step_name(step)));
    } else if app.shows_tasks()
        && let Some(note) = task_note(session)
    {
        notes.push(note);
    }
    if back > 0 {
        notes.push(format!("↑ {back} lines"));
    }
    notes
}

/// The longest a task's goal or summary gets in a pane's header.
const TASK_NOTE_LENGTH: usize = 40;

/// A session's task, for its pane's header: what it was asked to do while
/// it's open, that it waits on the user, and how it went, ✓, ✗ or –, once
/// it's closed.
fn task_note(session: &SessionInfo) -> Option<String> {
    let task = session.task.as_ref()?;
    let goal = task.goal.lines().next().unwrap_or("");
    let mark = match task.state() {
        TaskState::Running | TaskState::Pending => {
            return Some(format!("task: {}", fit(goal, TASK_NOTE_LENGTH)));
        }
        TaskState::Waiting => {
            return Some(format!(
                "task waits on you: {}",
                fit(goal, TASK_NOTE_LENGTH)
            ));
        }
        TaskState::Done => "✓",
        TaskState::Failed => "✗",
        TaskState::Cancelled => "–",
    };
    let said = match &task.outcome {
        Some(outcome) if !outcome.summary.is_empty() => &outcome.summary,
        _ => goal,
    };
    Some(format!("{mark} {}", fit(said, TASK_NOTE_LENGTH)))
}

/// A pane's header line, `width` columns wide: the session's mark and name,
/// in the accent color when the pane has the keyboard, and its `notes`;
/// then a rule; then, muted on the right, where the session runs and its
/// command. When that doesn't all fit, the command goes first, then where
/// it runs.
pub fn pane_header<'a>(
    session: &SessionInfo,
    notes: &[String],
    focused: bool,
    look: &Look,
    width: u16,
) -> Line<'a> {
    let theme = look.theme;
    let (mark, mark_color) = sidebar::session_mark(session, look);
    let name_color = if focused { theme.accent } else { theme.text };
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(mark, Style::new().fg(mark_color)),
        Span::raw(" "),
        Span::styled(
            session.name.clone(),
            Style::new().fg(name_color).add_modifier(Modifier::BOLD),
        ),
    ];
    for note in notes {
        spans.push(Span::styled(" · ", Style::new().fg(theme.muted)));
        spans.push(Span::styled(note.clone(), Style::new().fg(theme.muted)));
    }
    let left: usize = spans.iter().map(Span::width).sum();
    let width = usize::from(width);

    // The longest right side that still leaves a few columns of rule: a
    // space, three of rule, a space, the right side, and a space to end.
    let fits = |right: &String| {
        let needed = left + 1 + 3 + 1 + right.chars().count() + 1;
        needed <= width
    };
    let right = right_sides(session)
        .into_iter()
        .find(fits)
        .unwrap_or_default();
    let right_width = if right.is_empty() {
        0
    } else {
        right.chars().count() + 2
    };
    let rule = width.saturating_sub(left + 1 + right_width);
    spans.push(Span::raw(" "));
    spans.push(Span::styled("─".repeat(rule), Style::new().fg(theme.rule)));
    if !right.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(right, Style::new().fg(theme.muted)));
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

/// What can go on the right of a pane's header, longest first: where the
/// session runs, the model its agent runs on and its command; then without
/// the command; then without the model; then only where it runs.
fn right_sides(session: &SessionInfo) -> Vec<String> {
    let command: Vec<String> = session
        .command
        .iter()
        .map(|arg| shell::quote(arg))
        .collect();
    let command = fit(&command.join(" "), 32);
    let place = match &session.worktree {
        Some(worktree) => {
            // The same marks as the sidebar's worktree lines.
            let mark = if worktree.main { "⌂" } else { "⎇" };
            let branch = worktree.branch.as_deref().unwrap_or("(detached)");
            format!("{} {mark} {branch}", worktree.project)
        }
        None => shell::home_relative(&session.cwd),
    };
    match session.model.as_deref().map(model::short) {
        Some(model) => vec![
            format!("{place} · {model} · {command}"),
            format!("{place} · {model}"),
            format!("{place} · {command}"),
            place,
        ],
        None => vec![format!("{place} · {command}"), place],
    }
}

/// One line of text across the middle of `area`.
pub fn draw_message(frame: &mut Frame, look: &Look, message: &str, area: Rect) {
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);
    let message = Paragraph::new(message)
        .centered()
        .style(Style::new().fg(look.theme.muted));
    frame.render_widget(message, middle);
}

/// The footer: a question being asked, a notice, or else where the keyboard
/// is and the keys that matter there, with "? keys" on the right.
fn draw_footer(frame: &mut Frame, app: &App, panes: &[Pane], look: &Look, area: Rect) {
    let theme = look.theme;
    let copying = match app.focus() {
        Focus::Copy(slot) => pane_in(app, panes, slot),
        _ => None,
    };
    let searching = copying.and_then(|pane| pane.copy.as_ref()?.prompt.as_ref());
    if app.plugin_pane().is_some() {
        let hand_back = app.keymap().hand_back().hint();
        frame.render_widget(hint_spans(&[(&hand_back, "close")], theme), area);
    } else if app.reply().is_some() {
        frame.render_widget(hint_spans(REPLY_HINTS, theme), area);
    } else if app.command_list().is_some() {
        let hints = [("enter", "run it"), ("↑/↓", "choose"), ("esc", "close")];
        frame.render_widget(hint_spans(&hints, theme), area);
    } else if app.launcher().is_some() {
        frame.render_widget(hint_spans(LAUNCHER_HINTS, theme), area);
    } else if let Some(view) = app.profiles_view() {
        frame.render_widget(hint_spans(profiles::hints(view), theme), area);
    } else if app.plugins_view().is_some() {
        let hints = as_keys_are(app, plugins_view::HINTS, false);
        frame.render_widget(hint_spans(&borrowed(&hints), theme), area);
    } else if app.settings_view().is_some() {
        let hints = as_keys_are(app, settings_view::HINTS, false);
        frame.render_widget(hint_spans(&borrowed(&hints), theme), area);
    } else if let Some(prompt) = app.prompt() {
        draw_prompt(frame, theme, prompt, area);
    } else if let Some(view) = app.needs_you_view() {
        let hints = as_keys_are(app, needs_you::hints(view), true);
        draw_notice_or(frame, app.notice(), &borrowed(&hints), theme, area);
    } else if app.timeline_view().is_some() {
        draw_notice_or(frame, app.notice(), timeline::HINTS, theme, area);
    } else if let Some(view) = app.issues_view() {
        draw_notice_or(frame, app.notice(), issues::hints(view), theme, area);
    } else if let Some(view) = app.pull_requests_view() {
        draw_notice_or(frame, app.notice(), pull_requests::hints(view), theme, area);
    } else if let Some(view) = app.backlog_view() {
        draw_backlog_footer(frame, view, theme, area);
    } else if app.menu().is_some() {
        frame.render_widget(hint_spans(menu::HINTS, theme), area);
    } else if let Some(view) = app.layouts_view() {
        draw_layouts_footer(frame, app.notice(), view, theme, area);
    } else if let Some(view) = app.archived_view() {
        match (view.deleting(), app.notice()) {
            (Some(question), _) => frame.render_widget(question_line(&question, theme), area),
            (None, notice) => draw_notice_or(frame, notice, archived_view::HINTS, theme, area),
        }
    } else if let Some(name) = app.closing() {
        let question = format!("close {name}'s task? d done · f failed · any other key, not yet");
        frame.render_widget(question_line(&question, theme), area);
    } else if let Some(name) = app.moving() {
        let question = format!("move {name} to tab 1-9, or t a new one · any other key, not yet");
        frame.render_widget(question_line(&question, theme), area);
    } else if let Some(filter) = app.filter() {
        draw_filter(frame, theme, filter, app.found().len(), area);
    } else if let Some(confirm) = app.confirm() {
        frame.render_widget(question_line(&confirm.question(), theme), area);
    } else if let Some(prompt) = searching {
        draw_search_prompt(frame, theme, prompt, area);
    } else if let Some(notice) = app.notice() {
        let notice = Line::styled(format!(" {notice}"), Style::new().fg(theme.failed));
        frame.render_widget(notice, area);
    } else if let Some(restarted) = app.restarted() {
        frame.render_widget(restarted_line(restarted, theme), area);
    } else if let Some(away) = app.away_line() {
        frame.render_widget(away_line(away, theme), area);
    } else {
        let right = footer_right(app, theme);
        let room = usize::from(area.width).saturating_sub(right.width() + 1);
        frame.render_widget(hints_line(app, copying, theme, area.width, room), area);
        frame.render_widget(right.right_aligned(), area);
    }
}

/// What happened while the user was away, `while you were away:` standing
/// out, and the keys that show more.
fn away_line<'a>(away: &str, theme: &Theme) -> Line<'a> {
    let (lead, counts) = away.split_once(": ").unwrap_or((away, ""));
    let mut spans = vec![
        Span::styled(
            format!(" {lead}: "),
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(counts.to_string(), Style::new().fg(theme.text)),
        Span::raw("  "),
    ];
    spans.extend(hint_spans(&[("a", "timeline"), ("U", "needs you")], theme).spans);
    Line::from(spans)
}

/// What a restart brought back, `after the restart:` standing out, and
/// what it couldn't in the failed color.
fn restarted_line<'a>(restarted: &Restarted, theme: &Theme) -> Line<'a> {
    let (lead, said) = restarted
        .line
        .split_once(": ")
        .unwrap_or((&restarted.line, ""));
    let color = if restarted.failed {
        theme.failed
    } else {
        theme.text
    };
    Line::from(vec![
        Span::styled(
            format!(" {lead}: "),
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(said.to_string(), Style::new().fg(color)),
    ])
}

/// The viewer of the session the pane at `slot` shows, once it has one.
fn pane_in<'a>(app: &App, panes: &'a [Pane], slot: Slot) -> Option<&'a Pane> {
    let session = app.pane_session(slot).filter(|_| app.shows_screen(slot))?;
    panes.iter().find(|pane| pane.session_id == session.id)
}

/// The search being typed in copy mode, with the cursor in it.
fn draw_search_prompt(frame: &mut Frame, theme: &Theme, prompt: &SearchPrompt, area: Rect) {
    let label = if prompt.forward {
        " search down: "
    } else {
        " search up: "
    };
    let line = Line::from(vec![
        Span::styled(
            label,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(prompt.input.text().to_string(), Style::new().fg(theme.text)),
    ]);
    frame.render_widget(line, area);
    // The label is plain ASCII, so its length in bytes is its width.
    let column = area.x + (label.len() + prompt.input.cursor()) as u16;
    frame.set_cursor_position((column.min(area.right().saturating_sub(1)), area.y));
}

/// The footer while a view is open: a notice, if there is one, or else the
/// view's keys.
fn draw_view_footer(frame: &mut Frame, app: &App, view: &View, look: &Look, area: Rect) {
    let theme = look.theme;
    if let Some(notice) = app.notice() {
        let notice = Line::styled(format!(" {notice}"), Style::new().fg(theme.failed));
        frame.render_widget(notice, area);
        return;
    }
    let owned = |hints: Vec<(&str, &str)>| -> Vec<(String, String)> {
        hints
            .into_iter()
            .map(|(key, does)| (key.to_string(), does.to_string()))
            .collect()
    };
    let hints = match view {
        View::Diff(diff) => owned(diff_view::hints(diff)),
        View::Files(finder) => owned(finder::hints(finder)),
        View::Tree(tree) => owned(tree_browser::hints(tree)),
        View::Grep(_) => owned(grep::hints()),
        View::Branches(switcher) => owned(switcher::hints(switcher)),
        View::Memory(memory) => memory_view::hints(memory),
    };
    let mut spans = vec![Span::raw(" ")];
    for (key, does) in hints {
        let Some(key) = as_key_is(app, &key, false) else {
            continue;
        };
        spans.push(Span::styled(key, Style::new().fg(theme.text)));
        spans.push(Span::styled(
            format!(" {does}  "),
            Style::new().fg(theme.muted),
        ));
    }
    frame.render_widget(Line::from(spans), area);
}

/// A yes-or-no question: the question in the accent color, the answers
/// muted.
fn question_line<'a>(question: &str, theme: &Theme) -> Line<'a> {
    let (asked, answers) = question.split_once("? ").unwrap_or((question, ""));
    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            format!("{asked}?"),
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {answers}"), Style::new().fg(theme.muted)),
    ])
}

/// A key the footer names: a command's, written as the user's `[keys]`
/// has it, and left out when it has none; one of the sidebar's own; the
/// key that hands the keyboard back; or the prefix.
#[derive(Debug, Clone, Copy)]
enum Hint {
    Run(Command, &'static str),
    /// A mode's key, like answering's.
    Mode(ModeKey, &'static str),
    /// A few commands' or modes' keys, written as the label says while
    /// they're the defaults, or else each one's first, `/` between them.
    Keys(
        &'static str,
        &'static [Command],
        &'static [ModeKey],
        &'static str,
    ),
    Key(&'static str, &'static str),
    HandBack(&'static str),
    Prefix(&'static str),
}

/// The keys `hints` name, as the keymap writes them.
fn written(app: &App, hints: &[Hint]) -> Vec<(String, String)> {
    let keymap = app.keymap();
    hints
        .iter()
        .filter_map(|hint| {
            let (key, does) = match *hint {
                Hint::Run(command, does) => (keymap.hint(command)?, does),
                Hint::Mode(key, does) => (keymap.mode_keys(key).first()?.hint(), does),
                Hint::Keys(label, commands, modes, does) => {
                    (keymap.row_hint(label, commands, modes)?, does)
                }
                Hint::Key(key, does) => (key.to_string(), does),
                Hint::HandBack(does) => (keymap.hand_back().hint(), does),
                Hint::Prefix(does) => (keymap.prefix()?.hint(), does),
            };
            Some((key, does.to_string()))
        })
        .collect()
}

/// The sidebar's keys, the most used first: as many as fit are shown.
const SIDEBAR_HINTS: &[Hint] = &[
    Hint::Run(Command::Open, "type"),
    Hint::Run(Command::Reply, "reply"),
    Hint::Run(Command::NewSession, "new"),
    Hint::Run(Command::ToggleSplit, "split"),
    Hint::Run(Command::Kill, "kill"),
    Hint::Run(Command::Quit, "quit"),
    Hint::Run(Command::Zoom, "zoom"),
    Hint::Run(Command::Copy, "copy"),
    Hint::Run(Command::NewWorktree, "worktree"),
    Hint::Run(Command::NextNeedingYou, "next"),
    Hint::Run(Command::Search, "find"),
    Hint::Run(Command::Commands, "commands"),
    Hint::Run(Command::Diff, "diff"),
    Hint::Run(Command::FindFile, "files"),
    Hint::Run(Command::NewTab, "tab"),
    Hint::Run(Command::Resize, "resize"),
];

/// The sidebar's keys while the tab is zoomed: `j` and `k` choose the
/// session the one pane shows.
const ZOOMED_HINTS: &[Hint] = &[
    Hint::Run(Command::Zoom, "unzoom"),
    Hint::Run(Command::Open, "type"),
    Hint::Run(Command::Down, "switch"),
    Hint::Run(Command::Copy, "copy"),
    Hint::Run(Command::NewSession, "new"),
    Hint::Run(Command::Quit, "quit"),
];

/// The sidebar's keys while the selected step's flow run waits at a gate.
const GATE_HINTS: &[Hint] = &[
    Hint::Run(Command::FlowGoOn, "go on"),
    Hint::Run(Command::FlowSendBack, "send back"),
    Hint::Run(Command::NewSession, "new"),
    Hint::Run(Command::Kill, "kill"),
    Hint::Run(Command::Quit, "quit"),
    Hint::Run(Command::NextNeedingYou, "next"),
    Hint::Run(Command::Search, "find"),
    Hint::Run(Command::Diff, "diff"),
];

/// The sidebar's keys while the selected background task asks for a
/// permission.
const ASKING_HINTS: &[Hint] = &[
    Hint::Mode(ModeKey::AnswerYes, "allow"),
    Hint::Mode(ModeKey::AnswerNo, "deny"),
    Hint::Mode(ModeKey::AnswerAlways, "always"),
    Hint::Run(Command::Open, "watch"),
    Hint::Run(Command::Kill, "kill"),
    Hint::Run(Command::Quit, "quit"),
    Hint::Run(Command::NextNeedingYou, "next"),
];

/// The sidebar's keys while the selection is on a worktree with no
/// sessions.
const EMPTY_WORKTREE_HINTS: &[Hint] = &[
    Hint::Run(Command::NewSession, "start one here"),
    Hint::Run(Command::RemoveWorktree, "remove it"),
    Hint::Run(Command::Diff, "diff"),
    Hint::Run(Command::FindFile, "files"),
    Hint::Run(Command::Quit, "quit"),
    Hint::Run(Command::NextNeedingYou, "next"),
    Hint::Run(Command::Search, "find"),
];

/// The sidebar's keys while the selected step's flow run has stopped, at a
/// step that failed or was cut short.
const STOPPED_HINTS: &[Hint] = &[
    Hint::Run(Command::FlowGoOn, "run again"),
    Hint::Run(Command::NewSession, "new"),
    Hint::Run(Command::Kill, "kill"),
    Hint::Run(Command::Quit, "quit"),
    Hint::Run(Command::NextNeedingYou, "next"),
    Hint::Run(Command::Search, "find"),
    Hint::Run(Command::Diff, "diff"),
];

/// The keys while the new-session panel is open.
const LAUNCHER_HINTS: &[(&str, &str)] = &[
    ("enter", "start"),
    ("tab", "next"),
    ("←/→", "choose"),
    ("alt+enter", "new line"),
    ("ctrl+e", "command line"),
    ("esc", "cancel"),
];

/// The keys while the reply box is open.
const REPLY_HINTS: &[(&str, &str)] = &[
    ("enter", "send"),
    ("alt+enter", "new line"),
    ("esc", "cancel"),
];

/// The footer of a view that leaves it to the app to say things: what's
/// to be said, or else the view's keys.
fn draw_notice_or(
    frame: &mut Frame,
    notice: Option<&str>,
    hints: &[(&str, &str)],
    theme: &Theme,
    area: Rect,
) {
    match notice {
        Some(notice) => {
            let notice = Line::styled(format!(" {notice}"), Style::new().fg(theme.failed));
            frame.render_widget(notice, area);
        }
        None => frame.render_widget(hint_spans(hints, theme), area),
    }
}

/// The keys while the backlog view is open.
const BACKLOG_HINTS: &[(&str, &str)] = &[
    ("enter", "start a task"),
    ("a", "add"),
    ("e", "edit"),
    ("space", "done/undone"),
    ("x", "remove"),
    ("/", "filter"),
    ("t", "tag"),
    ("esc", "close"),
];

/// The footer while the backlog view is open: the line being written, the
/// question `x` asks, or the view's keys.
fn draw_backlog_footer(frame: &mut Frame, view: &BacklogView, theme: &Theme, area: Rect) {
    if let Some(writing) = &view.writing {
        let label = writing.label();
        let line = Line::from(vec![
            Span::styled(
                label.clone(),
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                writing.input.text().to_string(),
                Style::new().fg(theme.text),
            ),
        ]);
        frame.render_widget(line, area);
        // The label is plain ASCII, so its length in bytes is its width.
        let column = area.x + (label.len() + writing.input.cursor()) as u16;
        frame.set_cursor_position((column.min(area.right().saturating_sub(1)), area.y));
    } else if let Some(question) = view.removing() {
        frame.render_widget(question_line(&question, theme), area);
    } else {
        frame.render_widget(hint_spans(BACKLOG_HINTS, theme), area);
    }
}

/// The footer while the layouts view is open: the name the tabs are being
/// saved as, the question `x` asks, what the last key did, or the view's
/// keys.
fn draw_layouts_footer(
    frame: &mut Frame,
    notice: Option<&str>,
    view: &LayoutsView,
    theme: &Theme,
    area: Rect,
) {
    if let Some(naming) = &view.naming {
        let label = " save the tabs as: ";
        let line = Line::from(vec![
            Span::styled(
                label,
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(naming.text().to_string(), Style::new().fg(theme.text)),
        ]);
        frame.render_widget(line, area);
        // The label is plain ASCII, so its length in bytes is its width.
        let column = area.x + (label.len() + naming.cursor()) as u16;
        frame.set_cursor_position((column.min(area.right().saturating_sub(1)), area.y));
    } else if let Some(question) = view.removing() {
        frame.render_widget(question_line(&question, theme), area);
    } else if let Some(notice) = notice {
        let notice = Line::styled(format!(" {notice}"), Style::new().fg(theme.failed));
        frame.render_widget(notice, area);
    } else {
        frame.render_widget(hint_spans(layouts::HINTS, theme), area);
    }
}

/// A key a view's hints name, as the user's `[keys]` has it: the views'
/// `j/k`, and with `answers`, the answer keys; `None` when the user left
/// it with no key.
fn as_key_is(app: &App, key: &str, answers: bool) -> Option<String> {
    let keymap = app.keymap();
    let moved = |modes: &[ModeKey]| keymap.row_hint(key, &[], modes);
    match key {
        "j/k" => Some(moved(&[ModeKey::ViewDown, ModeKey::ViewUp]).unwrap_or("↓/↑".into())),
        "y" if answers => moved(&[ModeKey::AnswerYes]),
        "n" if answers => moved(&[ModeKey::AnswerNo]),
        "Y" if answers => moved(&[ModeKey::AnswerAlways]),
        key => Some(key.to_string()),
    }
}

/// [`as_key_is`] for each of `hints`.
fn as_keys_are<'a>(app: &App, hints: &[(&str, &'a str)], answers: bool) -> Vec<(String, &'a str)> {
    hints
        .iter()
        .filter_map(|&(key, does)| Some((as_key_is(app, key, answers)?, does)))
        .collect()
}

fn borrowed<'a>(hints: &'a [(String, &'a str)]) -> Vec<(&'a str, &'a str)> {
    hints
        .iter()
        .map(|(key, does)| (key.as_str(), *does))
        .collect()
}

/// A line of key hints, keys a touch brighter than what they do.
fn hint_spans<'a>(hints: &[(&str, &str)], theme: &Theme) -> Line<'a> {
    let mut spans = vec![Span::raw(" ")];
    for (key, does) in hints {
        spans.push(Span::styled(key.to_string(), Style::new().fg(theme.text)));
        spans.push(Span::styled(
            format!(" {does}  "),
            Style::new().fg(theme.muted),
        ));
    }
    Line::from(spans)
}

/// `/`'s filter, with the cursor in it and the status it keeps to before
/// it, in that status's color; and on the right how many things it found,
/// and the key that changes the status.
fn draw_filter(frame: &mut Frame, theme: &Theme, filter: &Filter, matches: usize, area: Rect) {
    let bold = Modifier::BOLD;
    let mut label = vec![Span::styled(
        " find",
        Style::new().fg(theme.accent).add_modifier(bold),
    )];
    if let Some(status) = filter.status {
        let color = theme.status(status.status());
        label.push(Span::raw(" "));
        label.push(Span::styled(
            status.name(),
            Style::new().fg(color).add_modifier(bold),
        ));
    }
    label.push(Span::styled(
        ": ",
        Style::new().fg(theme.accent).add_modifier(bold),
    ));
    let label_width: usize = label.iter().map(Span::width).sum();
    let mut line = Line::from(label);
    line.push_span(Span::styled(
        filter.input.text().to_string(),
        Style::new().fg(theme.text),
    ));
    frame.render_widget(line, area);
    let noun = if matches == 1 { "match" } else { "matches" };
    let right = Line::from(vec![
        Span::styled(format!("{matches} {noun}  "), Style::new().fg(theme.muted)),
        Span::styled("tab", Style::new().fg(theme.text)),
        Span::styled(" status ", Style::new().fg(theme.muted)),
    ]);
    frame.render_widget(right.right_aligned(), area);
    let column = area.x + (label_width + filter.input.cursor()) as u16;
    frame.set_cursor_position((column.min(area.right().saturating_sub(1)), area.y));
}

/// The keys that don't go to the program, while a pane has the keyboard.
const PANE_HINTS: &[Hint] = &[
    Hint::HandBack("sidebar"),
    Hint::Prefix("then a command's key"),
    Hint::Key("shift+pgup", "history"),
];

/// The keys a background task's pane takes, while it has the keyboard.
const TASK_PANE_HINTS: &[Hint] = &[
    Hint::HandBack("sidebar"),
    Hint::Keys("y/n/Y", &[], ANSWERS, "answer"),
    Hint::Key("ctrl+c", "stop the run"),
    Hint::Key("space", "follow-up"),
    Hint::Prefix("then a command's key"),
    Hint::Key("shift+pgup", "history"),
];

/// The keys after the prefix, in a pane.
const PREFIXED_HINTS: &[Hint] = &[
    Hint::Key("a command's key", "runs it"),
    Hint::Run(Command::Commands, "every command"),
    Hint::Prefix("again: to the program"),
    Hint::Key("esc", "never mind"),
];

/// The keys after the first of a plugin's two.
const PENDING_HINTS: &[Hint] = &[
    Hint::Key("its second key", "runs it"),
    Hint::Key("esc", "never mind"),
];

/// The answer keys.
const ANSWERS: &[ModeKey] = &[ModeKey::AnswerYes, ModeKey::AnswerNo, ModeKey::AnswerAlways];

/// The keys in resize mode.
const RESIZE_HINTS: &[Hint] = &[
    Hint::Keys(
        "h/j/k/l",
        &[],
        &[
            ModeKey::ResizeLeft,
            ModeKey::ResizeDown,
            ModeKey::ResizeUp,
            ModeKey::ResizeRight,
        ],
        "move a border",
    ),
    Hint::Mode(ModeKey::ResizeEven, "even out"),
    Hint::Mode(ModeKey::ResizeDone, "done"),
    Hint::Keys(
        "shift+arrows",
        &[
            Command::PaneLeft,
            Command::PaneDown,
            Command::PaneUp,
            Command::PaneRight,
        ],
        &[],
        "another pane",
    ),
];

/// Where the keyboard is, then the keys that matter most there, as many as
/// fit in `room`, what the right of the footer leaves.
fn hints_line<'a>(
    app: &App,
    copying: Option<&Pane>,
    theme: &Theme,
    width: u16,
    room: usize,
) -> Line<'a> {
    let doing = |what: &str, session: Option<&SessionInfo>| {
        let name = session.map_or("", |s| s.name.as_str());
        vec![
            Span::styled(format!(" {what} "), Style::new().fg(theme.muted)),
            Span::styled(name.to_string(), Style::new().fg(theme.accent)),
        ]
    };
    let lead = |text: String| {
        vec![Span::styled(
            text,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        )]
    };
    let (mut spans, hints) = match app.focus() {
        _ if app.pending().is_some() => {
            let first = app.pending().map(|first| first.hint()).unwrap_or_default();
            (lead(format!(" {first} …")), written(app, PENDING_HINTS))
        }
        Focus::Sidebar if app.resizing() => (
            doing("resizing", app.selected()),
            written(app, RESIZE_HINTS),
        ),
        Focus::Sidebar => (
            whereabouts(app, theme, width),
            written(app, sidebar_hints(app)),
        ),
        Focus::Pane(_) if app.prefixed() => {
            let prefix = app.keymap().prefix().map(|p| p.hint()).unwrap_or_default();
            (lead(format!(" {prefix} …")), written(app, PREFIXED_HINTS))
        }
        Focus::Pane(slot) if app.pane_shows_task(slot) => (
            doing("in", app.pane_session(slot)),
            written(app, TASK_PANE_HINTS),
        ),
        Focus::Pane(slot) => (
            doing("typing into", app.pane_session(slot)),
            written(app, PANE_HINTS),
        ),
        Focus::Copy(slot) => {
            let hints = copying.map_or(&[][..], |pane| copy_mode::hints(&pane.screen));
            let hints = hints
                .iter()
                .map(|(key, does)| (key.to_string(), does.to_string()))
                .collect();
            (doing("copying from", app.pane_session(slot)), hints)
        }
    };
    for (key, does) in hints {
        let used: usize = spans.iter().map(Span::width).sum();
        let hint = 2 + key.chars().count() + 1 + does.chars().count();
        if used + hint > room {
            break;
        }
        spans.push(Span::raw("  "));
        spans.push(Span::styled(key.to_string(), Style::new().fg(theme.text)));
        spans.push(Span::styled(
            format!(" {does}"),
            Style::new().fg(theme.muted),
        ));
    }
    Line::from(spans)
}

/// The sidebar's keys, led by what the selected step's flow run takes
/// while it waits on the user, or by what can be done with a worktree
/// with no sessions.
fn sidebar_hints(app: &App) -> &'static [Hint] {
    if app.selected_empty_worktree().is_some() {
        return EMPTY_WORKTREE_HINTS;
    }
    if app
        .selected()
        .is_some_and(|session| session.asking.is_some())
    {
        return ASKING_HINTS;
    }
    let index = app.selected_index();
    let run = index.and_then(|index| app.flow_step_of(index));
    match run.map(|(run, _)| run.state()) {
        Some(RunState::AtGate) => GATE_HINTS,
        Some(RunState::Failed | RunState::Interrupted) => STOPPED_HINTS,
        _ if app.zoomed() => ZOOMED_HINTS,
        _ => SIDEBAR_HINTS,
    }
}

/// Where the selection is: the selected session's project, branch and
/// name, or the project and branch of the worktree with no sessions it's
/// on. Cut from the left to a third of the footer, so the keys keep their
/// room.
fn whereabouts<'a>(app: &App, theme: &Theme, width: u16) -> Vec<Span<'a>> {
    let Some(full) = selection_place(app) else {
        return vec![Span::raw(" ")];
    };
    let room = usize::from(width / 3);
    let shown = if full.chars().count() <= room {
        full
    } else {
        let skip = full.chars().count() + 1 - room;
        format!("…{}", full.chars().skip(skip).collect::<String>())
    };
    vec![
        Span::raw(" "),
        Span::styled(shown, Style::new().fg(theme.text)),
    ]
}

/// Where the selection is, written out whole: `payments ▸ fix/login ▸
/// claude`, or `payments ▸ old-spike` on a worktree with no sessions.
fn selection_place(app: &App) -> Option<String> {
    if let Some(worktree) = app.selected_empty_worktree() {
        let branch = worktree.branch.as_deref().unwrap_or("(detached)");
        return Some(format!("{} ▸ {branch}", worktree.project));
    }
    let session = app.selected()?;
    let place = match &session.worktree {
        Some(worktree) => {
            let branch = worktree.branch.as_deref().unwrap_or("(detached)");
            format!("{} ▸ {branch} ▸ ", worktree.project)
        }
        None => String::new(),
    };
    Some(format!("{place}{}", session.name))
}

/// The right of the footer: what background tasks have spent today, in
/// red past the daily budget, and "? keys", where `?` opens the list of
/// every key: from the sidebar only, since in a pane `?` goes to the
/// program.
fn footer_right<'a>(app: &App, theme: &Theme) -> Line<'a> {
    let mut spans = Vec::new();
    if let Some(spending) = app.spending() {
        let today = format!("${:.2} today", spending.today_usd);
        let said = if spending.over_budget() {
            let over = format!("{today} · over ${:.2}", spending.daily_budget_usd);
            Span::styled(over, Style::new().fg(theme.failed))
        } else {
            Span::styled(today, Style::new().fg(theme.muted))
        };
        spans.push(said);
        spans.push(Span::raw("  "));
    }
    if app.focus() == Focus::Sidebar && !app.resizing() {
        spans.push(Span::styled("?", Style::new().fg(theme.text)));
        spans.push(Span::styled(" keys ", Style::new().fg(theme.muted)));
    }
    Line::from(spans)
}

/// Asks the prompt's question, with the cursor in the answer.
fn draw_prompt(frame: &mut Frame, theme: &Theme, prompt: &Prompt, area: Rect) {
    let question = match prompt.question {
        Question::Command(_) => " new session: ",
        Question::Rename(_) => " new name: ",
        Question::TabName => " tab name: ",
        Question::CloseTask { failed: false, .. } => " done; what was done: ",
        Question::CloseTask { failed: true, .. } => " failed; why: ",
        Question::SendFlowBack(_) => " send back; what to do differently: ",
    };
    let line = Line::from(vec![
        Span::styled(
            question,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(prompt.input.text().to_string(), Style::new().fg(theme.text)),
    ]);
    frame.render_widget(line, area);
    // The question is plain ASCII, so its length in bytes is its width.
    let column = area.x + (question.len() + prompt.input.cursor()) as u16;
    frame.set_cursor_position((column.min(area.right().saturating_sub(1)), area.y));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeName;
    use crate::protocol::{Activity, Front, Worktree};
    use crate::tui::groups::Row;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;
    use std::time::Instant;

    fn theme() -> Theme {
        Theme::new(ThemeName::DARK, false)
    }

    fn look(theme: &Theme) -> Look<'_> {
        Look {
            theme,
            now: 1_000,
            spin: 0,
        }
    }

    /// Draws `app` on a `width` by `height` screen and returns it as lines
    /// of text.
    fn screen_text_at(app: &App, width: u16, height: u16) -> Vec<String> {
        let theme = theme();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, app, &[], None, &look(&theme)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    /// Draws `app` on an 80 by 12 screen.
    fn screen_text(app: &App) -> Vec<String> {
        screen_text_at(app, 80, 12)
    }

    /// How wide the sidebar is unless the config says.
    const SIDEBAR: u16 = 28;

    /// The sidebar's rows on an 80 by 12 screen, up to the rule: what's
    /// beside them, like the pane's header, left out.
    fn sidebar_text(app: &App) -> Vec<String> {
        screen_text(app)
            .iter()
            .map(|line| line.chars().take(usize::from(SIDEBAR) + 1).collect())
            .collect()
    }

    fn session(name: &str, state: State) -> SessionInfo {
        SessionInfo {
            stopped_idle: false,
            front: None,
            name: name.into(),
            id: name.into(),
            command: vec!["sh".into()],
            cwd: PathBuf::from("/"),
            pid: Some(1),
            state,
            activity: None,
            worktree: None,
            changed: 0,
            task: None,
            asking: None,
            reporter: None,
            subagents: 0,
            model: None,
            line: None,
            bell: false,
            unseen_copies: 0,
            context: None,
            output_waits: 0,
        }
    }

    fn in_worktree(name: &str, branch: &str, main: bool) -> SessionInfo {
        SessionInfo {
            front: None,
            worktree: Some(Worktree {
                project: "app".into(),
                project_path: PathBuf::from("/code/app"),
                path: PathBuf::from(format!("/code/app/{branch}")),
                main,
                branch: Some(branch.into()),
                in_progress: None,
            }),
            ..session(name, State::Running)
        }
    }

    /// `session` with Claude Code in front.
    fn agent(mut session: SessionInfo) -> SessionInfo {
        session.front = Some(Front::Agent {
            program: "claude".into(),
            name: "Claude Code".into(),
        });
        session
    }

    /// `session` with zsh in front, at its prompt.
    fn shell(mut session: SessionInfo) -> SessionInfo {
        session.front = Some(Front::Shell { name: "zsh".into() });
        session
    }

    /// The number of the first line that holds `text`.
    fn line_with(lines: &[String], text: &str) -> usize {
        let found = lines.iter().position(|line| line.contains(text));
        found.unwrap_or_else(|| panic!("{text:?} isn't on screen:\n{}", lines.join("\n")))
    }

    fn text_of(line: &Line) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_worktree_with_no_sessions_is_drawn_with_a_row_saying_so() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_worktree("planner", "main", true)]);
        let old = Worktree {
            project: "app".into(),
            project_path: PathBuf::from("/code/app"),
            path: PathBuf::from("/code/app/old"),
            main: false,
            branch: Some("old".into()),
            in_progress: None,
        };
        app.set_worktrees(PathBuf::from("/code/app"), vec![old]);
        let lines = sidebar_text(&app);
        let heading = line_with(&lines, "⎇ old");
        assert!(lines[heading + 1].contains("· no sessions"), "{lines:?}");

        // On it, the pane says there's nothing there, and the footer how to
        // start something or remove it.
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        let screen = screen_text(&app).join("\n");
        assert!(screen.contains("No sessions in ⎇ old"), "{screen}");
        assert!(screen.contains("n start one here"), "{screen}");
        assert!(screen.contains("W remove it"), "{screen}");
    }

    #[test]
    fn slash_shows_a_pull_request_it_found_under_its_project_and_the_status_it_keeps_to() {
        use crate::forge::{Checks, Forge, PullRequest, Review};
        let mut app = App::new(None);
        app.set_sessions(vec![in_worktree("planner", "main", true)]);
        let pull_request = PullRequest {
            forge: Forge::GitHub,
            number: 57,
            title: "Fix the login redirect".into(),
            author: "ana".into(),
            branch: "fix-login".into(),
            from_fork: false,
            local_branch: "fix-login".into(),
            draft: false,
            conflicts: false,
            merged: false,
            checks: Checks::Failed,
            review: Review::None,
            updated_at: "2026-10-02T09:30:00Z".into(),
            url: "https://github.com/acme/app/pull/57".into(),
        };
        app.set_pull_requests(
            PathBuf::from("/code/app"),
            Ok((Forge::GitHub, vec![pull_request])),
            Instant::now(),
        );
        let press = |app: &mut App, code| app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        press(&mut app, KeyCode::Char('/'));
        for letter in "login".chars() {
            press(&mut app, KeyCode::Char(letter));
        }
        let lines = sidebar_text(&app);
        let heading = line_with(&lines, "app ─");
        let row = line_with(&lines, "#57 Fix the login");
        assert!(row > heading, "{lines:?}");
        assert!(lines[row].contains('✗'), "its checks failed: {lines:?}");
        let screen = screen_text(&app).join("\n");
        assert!(screen.contains("find: login"), "{screen}");
        assert!(screen.contains("1 match  tab status"), "{screen}");

        press(&mut app, KeyCode::Tab);
        let screen = screen_text(&app).join("\n");
        assert!(screen.contains("find waiting: login"), "{screen}");
        assert!(screen.contains("0 matches"), "{screen}");
    }

    #[test]
    fn the_keys_overlay_draws_over_an_80_by_24_screen_a_page_at_a_time() {
        let mut app = App::new(None);
        app.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        let text = screen_text_at(&app, 80, 24).join("\n");
        for on_screen in ["In the sidebar", "select a session", "1/3 · ← → turn"] {
            assert!(text.contains(on_screen), "{on_screen} isn't on screen");
        }
        let mut rest = String::new();
        for _ in 0..2 {
            app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
            rest += &screen_text_at(&app, 80, 24).join("\n");
        }
        let pages = [
            "In a pane",
            "Ctrl+\\",
            "In resize mode",
            "In a view",
            "With the mouse",
            "3/3",
        ];
        for on_screen in pages {
            assert!(rest.contains(on_screen), "{on_screen} isn't on screen");
        }
    }

    #[test]
    fn with_no_sessions_the_pane_says_how_to_start_one() {
        let app = App::new(None);
        let text = screen_text(&app).join("\n");
        assert!(text.contains("No sessions yet: n starts one"));
        assert!(text.contains("q quit"));
        assert!(text.contains("? keys"));
    }

    #[test]
    fn the_top_bar_names_crystal_and_counts_the_sessions() {
        let mut app = App::new(None);
        app.set_sessions(vec![session("a", State::Running)]);
        let text = screen_text(&app);
        assert!(text[0].starts_with(" crystal"), "{}", text[0]);
        assert!(text[0].trim_end().ends_with("1 session"), "{}", text[0]);
    }

    #[test]
    fn the_top_bar_names_a_server_that_isn_t_the_default_before_the_count() {
        let mut app = app_with_three_tabs();
        app.set_server(Some("work".into()));
        let text = screen_text(&app);
        assert!(
            text[0].trim_end().ends_with("work · 1 session"),
            "{}",
            text[0]
        );
        assert!(text[0].contains(" 2 review "), "{}", text[0]);
    }

    /// `app` with the settings in `text`, a config file's.
    fn configured(mut app: App, text: &str) -> App {
        app.set_interface(&crate::config::from_text(text).unwrap());
        app
    }

    #[test]
    fn the_tab_bar_goes_over_the_footer_when_told() {
        let app = configured(app_with_three_tabs(), "[tab_bar]\nposition = \"bottom\"\n");
        let text = screen_text(&app);
        assert!(
            text[10].starts_with(" crystal   1  2 review  3 "),
            "{}",
            text[10]
        );
        assert!(!text[0].contains("crystal"), "{}", text[0]);
        let areas = Areas::of(&app, Rect::new(0, 0, 80, 12));
        assert_eq!((areas.main.y, areas.main.height, areas.top.y), (0, 10, 10));
        // A click on a tab finds it there.
        assert_eq!(hit(&areas, &app, 15, 10), Hit::Tab(1));
    }

    #[test]
    fn the_tab_bar_can_be_left_out_while_there_is_one_tab() {
        let mut app = App::new(None);
        app.set_sessions(vec![session("a", State::Running)]);
        let app = configured(app, "[tab_bar]\nhide_when_single = true\n");
        let text = screen_text(&app);
        assert!(
            !text.iter().any(|line| line.contains("crystal")),
            "{text:?}"
        );
        assert_eq!(Areas::of(&app, Rect::new(0, 0, 80, 12)).main.height, 11);
        // With a second tab it's back.
        let app = configured(
            app_with_three_tabs(),
            "[tab_bar]\nhide_when_single = true\n",
        );
        assert!(screen_text(&app)[0].starts_with(" crystal"));
    }

    #[test]
    fn the_tab_bar_shows_its_status_after_the_count_while_the_tabs_have_room() {
        let config = "[tab_bar]\nright = [{ type = \"text\", text = \"prod\" }, \
                      { type = \"hostname\" }, { type = \"clock\" }]\n";
        let mut app = configured(app_with_three_tabs(), config);
        app.set_status(vec!["prod".into(), String::new(), "14:03".into()]);
        let text = screen_text(&app);
        assert!(
            text[0].trim_end().ends_with("1 session · prod · 14:03"),
            "{}",
            text[0]
        );
        assert!(
            text[0].starts_with(" crystal   1  2 review  3 "),
            "{}",
            text[0]
        );
        // On a narrow bar, the tabs keep their room.
        app.set_status(vec![
            "a much longer status line".into(),
            String::new(),
            "14:03".into(),
        ]);
        let narrow = screen_text_at(&app, 60, 12);
        assert!(narrow[0].trim_end().ends_with("1 session"), "{}", narrow[0]);
        assert!(narrow[0].contains(" 1 "), "{}", narrow[0]);
    }

    /// An app with one session and three tabs, the second named `review`
    /// and in front.
    fn app_with_three_tabs() -> App {
        let mut app = App::new(None);
        app.set_sessions(vec![session("a", State::Running)]);
        let mut press = |code| app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        press(KeyCode::Char('t'));
        press(KeyCode::Char('T'));
        for c in "review".chars() {
            press(KeyCode::Char(c));
        }
        press(KeyCode::Enter);
        press(KeyCode::Char('t'));
        press(KeyCode::Char('2'));
        app
    }

    #[test]
    fn the_top_bar_shows_the_tabs_by_number_and_name_after_crystal() {
        let text = screen_text(&app_with_three_tabs());
        assert!(
            text[0].starts_with(" crystal   1  2 review  3 "),
            "{}",
            text[0]
        );
    }

    #[test]
    fn the_tab_in_front_stands_out_like_the_selection() {
        let theme = theme();
        let app = app_with_three_tabs();
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal
            .draw(|frame| draw(frame, &app, &[], None, &look(&theme)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let (front, behind) = (&buffer[(16, 0)], &buffer[(11, 0)]);
        assert_eq!(front.symbol(), "r");
        assert_eq!(front.bg, theme.selection.bg.unwrap());
        assert_eq!(front.fg, theme.accent);
        assert_eq!(behind.symbol(), "1");
        assert_eq!(behind.fg, theme.muted);
    }

    #[test]
    fn tabs_go_by_their_numbers_alone_when_their_names_dont_fit() {
        let named = |name: &str| Tab {
            name: name.into(),
            ..Tab::default()
        };
        let tabs = [named("agents"), named("a-long-name-for-a-tab"), named("")];
        let labels = |width| -> Vec<String> {
            let labels = tab_labels(&tabs, &[None; 3], width, 0);
            labels.into_iter().map(|(_, _, label)| label).collect()
        };
        assert_eq!(labels(120), [" 1 agents ", " 2 a-long-name-for… ", " 3 "]);
        assert_eq!(labels(60), [" 1 ", " 2 ", " 3 "]);
        assert_eq!(labels(42), [" 1 ", " 2 "], "the last doesn't fit");
    }

    #[test]
    fn tabs_that_dont_fit_are_left_off_from_the_start_to_show_the_one_in_front() {
        let tabs = vec![Tab::default(); 20];
        let statuses = [None; 20];
        let labels = |in_front| -> Vec<(usize, String)> {
            let labels = tab_labels(&tabs, &statuses, 60, in_front);
            labels
                .into_iter()
                .map(|(index, _, label)| (index, label))
                .collect()
        };
        let first = labels(0);
        assert_eq!(first.first().unwrap().0, 0);
        assert!(first.len() < 20, "{first:?}");
        let last = labels(19);
        assert_eq!(last.last().unwrap(), &(19, " 20 ".to_string()));
        assert!(last.first().unwrap().0 > 0);
        // The labels start where the first always does.
        let start = tab_labels(&tabs, &statuses, 60, 19)[0].1;
        assert_eq!(start, tab_labels(&tabs, &statuses, 60, 0)[0].1);
    }

    #[test]
    fn a_tab_with_an_agent_waiting_shows_it_from_another_tab() {
        let theme = theme();
        let mut waiting = session("a", State::Running);
        waiting.activity = Some(Activity::Waiting);
        let mut app = App::new(None);
        app.set_sessions(vec![waiting.clone()]);
        app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        app.set_sessions(vec![waiting, session("b", State::Running)]);

        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal
            .draw(|frame| draw(frame, &app, &[], None, &look(&theme)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let top: String = (0..30).map(|x| buffer[(x, 0)].symbol()).collect();
        assert!(top.starts_with(" crystal   1 ▲  2 "), "{top}");
        assert_eq!(buffer[(13, 0)].fg, theme.waiting);
        // The one waiting is in the first tab: pinned at the top of this
        // one's sidebar, saying which tab it's in, and nowhere else.
        let sidebar = sidebar_text(&app);
        assert!(sidebar[1].contains("needs you · 1"), "{sidebar:?}");
        assert!(
            sidebar[2].contains("▲ a") && sidebar[2].contains("⇥ 1"),
            "{sidebar:?}"
        );
        assert!(
            sidebar.iter().any(|line| line.contains("❯ b")),
            "{sidebar:?}"
        );
        let waiting = sidebar.iter().filter(|line| line.contains("▲ a")).count();
        assert_eq!(waiting, 1, "{sidebar:?}");
    }

    #[test]
    fn a_click_finds_the_tab_under_it() {
        let app = app_with_three_tabs();
        let areas = Areas::of(&app, Rect::new(0, 0, 80, 24));
        // " 1 " is drawn at columns 10 to 12, " 2 review " at 13 to 22.
        assert_eq!(hit(&areas, &app, 10, 0), Hit::Tab(0));
        assert_eq!(hit(&areas, &app, 18, 0), Hit::Tab(1));
        assert_eq!(hit(&areas, &app, 24, 0), Hit::Tab(2));
        assert_eq!(hit(&areas, &app, 40, 0), Hit::Elsewhere);
    }

    #[test]
    fn the_summary_counts_the_waiting_only_when_some_wait() {
        let theme = theme();
        let quiet = vec![session("a", State::Running), session("b", State::Running)];
        assert_eq!(text_of(&summary(&quiet, None, &theme)), "2 sessions ");

        let mut waiting = quiet.clone();
        waiting[1].activity = Some(Activity::Waiting);
        let line = summary(&waiting, None, &theme);
        assert_eq!(text_of(&line), "2 sessions · 1 waiting ");
        let count = line.spans.iter().find(|span| span.content == "1 waiting");
        assert_eq!(count.unwrap().style.fg, Some(theme.waiting));
    }

    #[test]
    fn the_forge_counts_say_nothing_of_none_and_a_plus_for_a_list_cut_short() {
        let theme = theme();
        let counted = |count: usize, more: bool| Some(Counted { count, more });
        let open = OpenOnForge {
            forge: crate::forge::Forge::GitHub,
            pull_requests: counted(3, false),
            issues: counted(1, false),
        };
        let said = Line::from(forge_counts(Some(open), &theme));
        assert_eq!(text_of(&said), "3 prs1 issue");
        let prs = said.spans.iter().find(|span| span.content == "3 prs");
        assert_eq!(prs.unwrap().style.fg, Some(theme.accent));

        let gitlab = OpenOnForge {
            forge: crate::forge::Forge::GitLab,
            pull_requests: counted(100, true),
            issues: counted(0, false),
        };
        let said = Line::from(forge_counts(Some(gitlab), &theme));
        assert_eq!(text_of(&said), "100+ mrs");
        let unlisted = OpenOnForge {
            issues: None,
            pull_requests: None,
            ..gitlab
        };
        assert!(forge_counts(Some(unlisted), &theme).is_empty());
        assert!(forge_counts(None, &theme).is_empty());
    }

    #[test]
    fn the_top_bar_counts_what_is_open_on_the_selected_projects_forge() {
        use crate::forge::{Checks, Forge, PullRequest, Review};
        let mut app = App::new(None);
        let mut planner = session("planner", State::Running);
        planner.worktree = Some(crate::protocol::Worktree {
            project: "app".into(),
            project_path: PathBuf::from("/code/app"),
            path: PathBuf::from("/code/app"),
            main: true,
            branch: Some("main".into()),
            in_progress: None,
        });
        app.set_sessions(vec![planner]);
        let pull_request = |number: u64, draft: bool, merged: bool| PullRequest {
            forge: Forge::GitHub,
            number,
            title: "a change".into(),
            author: "ana".into(),
            branch: format!("b{number}"),
            from_fork: false,
            local_branch: format!("b{number}"),
            draft,
            conflicts: false,
            merged,
            checks: Checks::None,
            review: Review::None,
            updated_at: String::new(),
            url: String::new(),
        };
        let listed = vec![
            pull_request(1, false, false),
            pull_request(2, true, false),
            pull_request(3, false, true),
        ];
        app.set_pull_requests(
            PathBuf::from("/code/app"),
            Ok((Forge::GitHub, listed)),
            Instant::now(),
        );
        let top = |app: &App, width: u16| {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            let theme = theme();
            let look = Look {
                theme: &theme,
                now: 0,
                spin: 0,
            };
            terminal
                .draw(|frame| draw_top_bar(frame, app, &look, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            (0..width)
                .map(|x| buffer[(x, 0)].symbol())
                .collect::<String>()
        };
        // The merged one isn't open; the draft is, until drafts are hidden.
        assert!(
            top(&app, 100).ends_with("1 session · 2 prs "),
            "{}",
            top(&app, 100)
        );
        let config = crate::config::Config {
            forge: crate::config::ForgeSettings {
                hide_draft_prs: true,
            },
            ..crate::config::Config::default()
        };
        app.set_features(&config);
        assert!(
            top(&app, 100).ends_with("1 session · 1 pr "),
            "{}",
            top(&app, 100)
        );
        // What the user has the bar show at its right comes after them,
        // and the counts give way to it first when room runs short.
        app.set_status(vec!["devbox".into()]);
        assert!(
            top(&app, 100).ends_with("1 session · 1 pr · devbox "),
            "{}",
            top(&app, 100)
        );
        assert!(
            top(&app, 60).ends_with("1 session · devbox "),
            "{}",
            top(&app, 60)
        );
        assert!(top(&app, 50).ends_with("1 session "), "{}", top(&app, 50));
    }

    #[test]
    fn the_sidebar_lists_sessions_under_a_heading_with_marks() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            agent(session("claude", State::Running)),
            agent(session("codex", State::Exited { code: 1 })),
        ]);
        let sidebar = sidebar_text(&app);
        assert!(line_with(&sidebar, "outside git ─") < line_with(&sidebar, "▸ claude"));
        assert!(line_with(&sidebar, "▸ claude") < line_with(&sidebar, "■ codex"));
        let screen = screen_text(&app);
        assert!(
            screen[1].contains("▸ claude ─"),
            "the pane's header names it"
        );
    }

    #[test]
    fn a_flow_run_heads_a_row_for_each_of_its_steps() {
        use crate::flow_run::{FlowRun, StepState};
        let config = crate::config::from_text(crate::flows::EXAMPLE).unwrap();
        let mut run = FlowRun::new(
            "ship-1".into(),
            config.flows[0].clone(),
            &config.profiles,
            "add retries".into(),
            PathBuf::from("/"),
            Default::default(),
            0,
        );
        run.round = 2;
        run.steps[0].state = StepState::Done;
        run.steps[0].session = Some("ship-1-plan".into());
        run.steps[1].state = StepState::Done;
        run.steps[1].session = Some("ship-1-implement".into());
        run.steps[2].state = StepState::AtGate;
        run.steps[2].session = Some("ship-1-review".into());
        let mut app = App::new(None);
        app.set_flows(vec![run]);
        app.set_sessions(vec![
            session("ship-1-plan", State::Running),
            session("ship-1-implement", State::Running),
            session("ship-1-review", State::Running),
        ]);
        app.select("ship-1-review");
        let sidebar = sidebar_text(&app);
        let heading = line_with(&sidebar, "◇ ship add retr");
        assert!(sidebar[heading].contains("round 2"), "{}", sidebar[heading]);
        assert_eq!(line_with(&sidebar, "✓ plan"), heading + 1);
        assert_eq!(line_with(&sidebar, "✓ implement"), heading + 2);
        assert_eq!(line_with(&sidebar, "▲ review"), heading + 3);
        assert_eq!(line_with(&sidebar, "· pr"), heading + 4);
        // The footer offers what the gate takes, and the pane's header says
        // where the run is.
        let screen = screen_text(&app);
        let footer = screen.last().unwrap();
        assert!(
            footer.contains("g go on") && footer.contains("f send back"),
            "{footer}"
        );
        assert!(
            screen[1].contains("ship-1 review · waiting"),
            "{}",
            screen[1]
        );
    }

    #[test]
    fn the_sidebar_marks_what_each_agent_is_doing() {
        let mut app = App::new(None);
        let mut sessions = Vec::new();
        for (name, activity) in [
            ("asks", Activity::Waiting),
            ("busy", Activity::Working),
            ("finished", Activity::Done),
            ("resting", Activity::Idle),
        ] {
            let mut session = agent(session(name, State::Running));
            session.activity = Some(activity);
            sessions.push(session);
        }
        app.set_sessions(sessions);
        let text = screen_text(&app);
        line_with(&text, "▲ asks");
        line_with(&text, "◐ busy");
        line_with(&text, "✓ finished");
        line_with(&text, "▸ resting");
    }

    #[test]
    fn agents_and_terminals_tell_apart_by_shape_alone() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            agent(in_worktree("claude", "main", true)),
            shell(in_worktree("zsh-2", "main", true)),
            agent(in_worktree("claude-2", "main", true)),
            in_worktree("server", "main", true),
        ]);
        // The text has no color: the marks and the line between say it all.
        let text = sidebar_text(&app);
        let order = [
            line_with(&text, "⌂ main"),
            line_with(&text, "▸ claude "),
            line_with(&text, "▸ claude-2"),
            line_with(&text, "terminals ┄"),
            line_with(&text, "❯ zsh-2"),
            line_with(&text, "❯ server"),
        ];
        let one_after_another: Vec<usize> = (order[0]..order[0] + 6).collect();
        assert_eq!(order.to_vec(), one_after_another, "\n{}", text.join("\n"));
    }

    #[test]
    fn a_session_row_says_how_long_ago_it_changed() {
        let mut app = App::new(None);
        let mut old = session("old", State::Running);
        old.changed = 1_000 - 12 * 60;
        app.set_sessions(vec![old]);
        let text = sidebar_text(&app);
        let row = &text[line_with(&text, "❯ old")];
        // Right-aligned against the rule.
        assert!(row.contains("12m │"), "{row}");
    }

    #[test]
    fn a_name_too_long_for_the_time_beside_it_keeps_its_room() {
        let mut app = App::new(None);
        let mut long = session("a-very-long-session-name", State::Running);
        long.changed = 1_000 - 45;
        app.set_sessions(vec![long]);
        let text = sidebar_text(&app);
        let row = &text[line_with(&text, "❯ a-very-long")];
        assert!(!row.contains("45s"), "{row}");
    }

    #[test]
    fn sessions_sit_under_their_project_and_worktree() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            agent(in_worktree("fixer", "fix", false)),
            agent(in_worktree("planner", "main", true)),
            shell(session("shell", State::Running)),
        ]);
        let text = sidebar_text(&app);
        let order = [
            line_with(&text, "app ─"),
            line_with(&text, "⌂ main"),
            line_with(&text, "▸ planner"),
            line_with(&text, "⎇ fix"),
            line_with(&text, "▸ fixer"),
            line_with(&text, "outside git"),
            line_with(&text, "❯ shell"),
        ];
        assert!(
            order.is_sorted(),
            "out of order: {order:?}\n{}",
            text.join("\n")
        );
    }

    #[test]
    fn a_pane_header_drops_the_command_then_the_place_as_it_narrows() {
        let theme = theme();
        let look = look(&theme);
        let session = agent(in_worktree("planner", "main", true));
        let header = |width| text_of(&pane_header(&session, &[], false, &look, width));

        let wide = header(60);
        assert!(wide.contains("app ⌂ main · sh"), "{wide}");
        assert_eq!(wide.chars().count(), 60);

        let middling = header(30);
        assert!(middling.contains("app ⌂ main"), "{middling}");
        assert!(!middling.contains("· sh"), "{middling}");

        let narrow = header(18);
        assert!(narrow.starts_with(" ▸ planner ─"), "{narrow}");
        assert!(!narrow.contains("app"), "{narrow}");
    }

    #[test]
    fn a_pane_header_names_its_agent_s_model_before_its_command() {
        let theme = theme();
        let look = look(&theme);
        let mut session = agent(in_worktree("planner", "main", true));
        session.model = Some("claude-opus-5-5".into());
        let header = |width| text_of(&pane_header(&session, &[], false, &look, width));
        let wide = header(60);
        assert!(wide.contains("app ⌂ main · opus 5.5 · sh"), "{wide}");
        let middling = header(40);
        assert!(middling.contains("app ⌂ main · opus 5.5 "), "{middling}");
    }

    #[test]
    fn a_pane_header_notes_how_its_session_ended() {
        let theme = theme();
        let ended = session("done", State::Exited { code: 3 });
        let notes = vec!["exited 3".to_string()];
        let header = text_of(&pane_header(&ended, &notes, false, &look(&theme), 60));
        assert!(header.starts_with(" ■ done · exited 3 ─"), "{header}");
    }

    #[test]
    fn a_background_task_s_pane_says_how_full_its_conversation_is() {
        let mut app = App::new(None);
        let task = SessionInfo {
            front: Some(Front::Task),
            context: Some(crate::protocol::ContextUse {
                tokens: 24_000,
                window: 200_000,
            }),
            ..session("fixer", State::Running)
        };
        app.set_sessions(vec![task]);
        let text = screen_text_at(&app, 100, 24).join("\n");
        assert!(text.contains("fixer · ctx 12%"), "{text}");
    }

    #[test]
    fn the_new_session_panel_shows_over_the_panes_with_what_will_run() {
        let mut app = App::new(None);
        app.set_agents(vec![crate::catalog::find("claude").unwrap()]);
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        for c in "fix it".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let text = screen_text_at(&app, 100, 24).join("\n");
        assert!(text.contains("New session · this directory"), "{text}");
        assert!(text.contains("fix it"), "{text}");
        assert!(text.contains("run          Claude Code   shell"), "{text}");
        assert!(text.contains("runs  claude -- 'fix it'"), "{text}");
        assert!(text.contains("enter start"), "{text}");
    }

    #[test]
    fn ctrl_e_puts_the_command_line_on_the_footer() {
        let mut app = App::new(None);
        app.set_agents(vec![crate::catalog::find("claude").unwrap()]);
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        let text = screen_text(&app);
        assert!(text[11].contains("new session: claude"), "{text:?}");
    }

    #[test]
    fn killing_asks_on_the_footer() {
        let mut app = App::new(None);
        app.set_sessions(vec![session("doomed", State::Running)]);
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let text = screen_text(&app);
        assert!(text[11].contains("kill doomed? y/n"));
    }

    #[test]
    fn the_footer_says_where_the_selection_is() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_worktree("planner", "main", true)]);
        let text = screen_text(&app);
        assert!(
            text[11].starts_with(" app ▸ main ▸ planner"),
            "{}",
            text[11]
        );
    }

    #[test]
    fn zoomed_the_pane_takes_the_sidebars_room_and_says_so() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            session("planner", State::Running),
            session("other", State::Running),
        ]);
        app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        let areas = Areas::of(&app, Rect::new(0, 0, 80, 24));
        assert_eq!(areas.panes, [Rect::new(0, 1, 80, 22)]);
        assert_eq!(screen_area(areas.panes[0], false), Rect::new(0, 2, 80, 21));
        assert_eq!(
            hit(&areas, &app, 0, 5),
            Hit::Pane {
                slot: Slot::Selected,
                cell: Some((3, 0)),
            }
        );

        let text = screen_text(&app);
        assert!(text[1].starts_with(" ❯ planner · zoomed "), "{}", text[1]);
        assert!(!text.iter().any(|line| line.contains("other")));
        assert!(text[11].contains("z unzoom"), "{}", text[11]);

        // `/` brings the sidebar out over the pane.
        app.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        let text = screen_text(&app);
        assert!(text.iter().any(|line| line.contains("other")));
    }

    #[test]
    fn in_copy_mode_the_footer_and_header_say_so() {
        let mut app = App::new(None);
        app.set_sessions(vec![session("planner", State::Running)]);
        app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        let text = screen_text(&app);
        assert!(text[1].contains("planner · copy mode"), "{}", text[1]);
        assert!(
            text[11].starts_with(" copying from planner"),
            "{}",
            text[11]
        );
    }

    #[test]
    fn a_drag_keeps_to_the_edge_of_the_pane_it_started_in() {
        let app = app_with_sessions(1);
        let areas = Areas::of(&app, Rect::new(0, 0, 80, 24));
        // The screen is at column 29, row 2, 50 by 21, the scrollbar on
        // its right.
        let nearest = |column, row| nearest_cell(&areas, &app, Slot::Selected, column, row);
        assert_eq!(nearest(31, 3), Some((1, 2)));
        assert_eq!(nearest(5, 0), Some((0, 0)));
        assert_eq!(nearest(200, 200), Some((20, 49)));
        assert_eq!(nearest_cell(&areas, &app, Slot::Split(0), 31, 3), None);
        // Past its top, on the header line and the top bar, or below its
        // bottom, on the footer, it says how far.
        let past = |row| rows_past(&areas, &app, Slot::Selected, row);
        assert_eq!(
            [past(0), past(1), past(2), past(22), past(23)],
            [-2, -1, 0, 0, 1]
        );
    }

    #[test]
    fn a_click_beside_a_panes_screen_is_on_its_scrollbar() {
        let app = app_with_sessions(1);
        let areas = Areas::of(&app, Rect::new(0, 0, 80, 24));
        let selected = Slot::Selected;
        assert_eq!(
            hit(&areas, &app, 79, 2),
            Hit::Scrollbar {
                slot: selected,
                row: 0
            }
        );
        assert_eq!(
            hit(&areas, &app, 78, 22),
            Hit::Pane {
                slot: selected,
                cell: Some((20, 49))
            }
        );
        // Above it is the header line; a drag of the thumb keeps to it.
        assert!(matches!(
            hit(&areas, &app, 79, 1),
            Hit::Pane { cell: None, .. }
        ));
        let row = |at| scrollbar_row(&areas, &app, selected, at);
        assert_eq!([row(0), row(10), row(23)], [Some(0), Some(8), Some(20)]);
    }

    #[test]
    fn the_session_screen_sits_below_its_header() {
        let areas = Areas::of(&App::new(None), Rect::new(0, 0, 80, 24));
        assert_eq!(areas.top, Rect::new(0, 0, 80, 1));
        assert_eq!(areas.rule, Rect::new(28, 1, 1, 22));
        assert_eq!(areas.tiles, Rect::new(29, 1, 51, 22));
        assert_eq!(areas.panes, [areas.tiles], "one pane takes all the room");
        assert_eq!(screen_area(areas.panes[0], false), Rect::new(29, 2, 51, 21));
        // A scrollbar takes the column on its right.
        assert_eq!(screen_area(areas.panes[0], true), Rect::new(29, 2, 50, 21));
        let track = scrollbar_area(areas.panes[0], true);
        assert_eq!(track, Some(Rect::new(79, 2, 1, 21)));
        assert_eq!(scrollbar_area(areas.panes[0], false), None);
        assert_eq!(
            scrollbar_area(Rect::new(0, 0, 7, 10), true),
            None,
            "too narrow"
        );
    }

    #[test]
    fn panes_go_where_the_tabs_tree_puts_them_a_rule_between_side_by_side() {
        let mut app = app_with_sessions(3);
        let screen = Rect::new(0, 0, 200, 24);
        app.set_tiles(Areas::of(&app, screen).tiles);
        // 171 columns beside the sidebar: s puts the two side by side, and
        // `-` splits the selection's pane, on the right, in two.
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('-'), KeyModifiers::NONE));
        let areas = Areas::of(&app, screen);
        assert_eq!(
            areas.panes,
            [
                Rect::new(29, 1, 85, 22),
                Rect::new(115, 1, 85, 11),
                Rect::new(115, 12, 85, 11),
            ]
        );
        let text = screen_text_at(&app, 200, 24);
        assert_eq!(text[5].chars().nth(114), Some('│'), "{}", text[5]);
        // A header line under another pane is the border between them,
        // but for the name on it.
        assert_eq!(hit(&areas, &app, 150, 12), Hit::Border { split: 1, at: 12 });
        assert!(matches!(
            hit(&areas, &app, 117, 12),
            Hit::Pane { cell: None, .. }
        ));
        assert_eq!(hit(&areas, &app, 114, 5), Hit::Border { split: 0, at: 114 });
        assert!(matches!(
            hit(&areas, &app, 150, 1),
            Hit::Pane { cell: None, .. }
        ));
        // A drag goes on following the border it took, wherever it goes.
        assert_eq!(
            border_hit(&areas, &app, 0, 60, 20),
            Hit::Border { split: 0, at: 60 }
        );
        assert_eq!(
            border_hit(&areas, &app, 1, 60, 20),
            Hit::Border { split: 1, at: 20 }
        );
    }

    #[test]
    fn in_resize_mode_the_footer_and_header_say_so() {
        let mut app = app_with_sessions(2);
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE));
        let text = screen_text_at(&app, 80, 24);
        assert!(text[1].contains("s0 · selected · resizing"), "{}", text[1]);
        assert!(text[23].starts_with(" resizing s0"), "{}", text[23]);
        assert!(text[23].contains("= even out"), "{}", text[23]);
        assert!(!text[23].contains("? keys"), "{}", text[23]);
    }

    #[test]
    fn a_selected_session_with_a_split_is_pointed_to_not_drawn_twice() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            session("left", State::Running),
            session("right", State::Running),
        ]);
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        let text = screen_text(&app).join("\n");
        assert!(text.contains("left has a pane of its own"));
        assert!(text.contains("left · selected"));
    }

    /// An app with `count` sessions, outside git, so under two headings.
    fn app_with_sessions(count: usize) -> App {
        let mut app = App::new(None);
        let sessions = (0..count)
            .map(|n| session(&format!("s{n}"), State::Running))
            .collect();
        app.set_sessions(sessions);
        app
    }

    #[test]
    fn a_click_finds_the_sidebar_row_under_it() {
        let app = app_with_sessions(3);
        let areas = Areas::of(&app, Rect::new(0, 0, 80, 24));
        // Row 0 is the top bar; the sidebar's rows start below it.
        assert_eq!(hit(&areas, &app, 5, 0), Hit::Elsewhere);
        assert_eq!(hit(&areas, &app, 5, 1), Hit::SidebarRow(0));
        assert_eq!(hit(&areas, &app, 5, 3), Hit::SidebarRow(2));
        // Below the last of the 5 rows: still the sidebar, but no row.
        assert_eq!(hit(&areas, &app, 5, 10), Hit::Sidebar);
    }

    #[test]
    fn a_folded_sidebar_is_a_rail_of_marks_with_the_pinned_over_a_rule() {
        let mut waiting = session("a", State::Running);
        waiting.activity = Some(Activity::Waiting);
        let mut app = App::new(None);
        app.set_sessions(vec![waiting, session("b", State::Running)]);
        app.on_key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::NONE));
        let rail: Vec<String> = screen_text(&app)
            .iter()
            .skip(1)
            .take(4)
            .map(|line| line.chars().take(4).collect())
            .collect();
        assert!(rail[0].starts_with(" ▲ │"), "{rail:?}");
        assert!(rail[1].starts_with("── │"), "{rail:?}");
        assert!(rail[2].starts_with(" ▲ │"), "{rail:?}");
        assert!(!rail[3].starts_with("   "), "{rail:?}");
        // A click on the rail selects the session drawn there.
        let areas = Areas::of(&app, Rect::new(0, 0, 80, 12));
        assert_eq!(areas.sidebar.width, 3);
        assert_eq!(hit(&areas, &app, 1, 2), Hit::Sidebar, "the rule");
        assert!(matches!(hit(&areas, &app, 1, 4), Hit::SidebarRow(_)));
    }

    #[test]
    fn a_click_finds_the_cell_on_the_panes_screen() {
        let app = app_with_sessions(1);
        let areas = Areas::of(&app, Rect::new(0, 0, 80, 24));
        // The pane starts after the 28-column sidebar and its rule; its
        // screen below the header line, at column 29, row 2.
        assert_eq!(
            hit(&areas, &app, 31, 3),
            Hit::Pane {
                slot: Slot::Selected,
                cell: Some((1, 2))
            }
        );
        let on_the_header = hit(&areas, &app, 40, 1);
        assert_eq!(
            on_the_header,
            Hit::Pane {
                slot: Slot::Selected,
                cell: None
            }
        );
        assert_eq!(hit(&areas, &app, 28, 5), Hit::SidebarEdge(28), "the rule");
        assert_eq!(hit(&areas, &app, 40, 23), Hit::Elsewhere, "the footer");
    }

    #[test]
    fn a_click_finds_which_pane_when_there_are_splits() {
        let mut app = app_with_sessions(2);
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        // At 80 columns the two panes are stacked: the session split off
        // stays on top, where it was, and the selection's pane goes below.
        let areas = Areas::of(&app, Rect::new(0, 0, 80, 24));
        let below = areas.panes[1];
        let Hit::Pane { slot, .. } = hit(&areas, &app, 40, below.y + 2) else {
            panic!("not a pane");
        };
        assert_eq!(slot, Slot::Selected);
    }

    #[test]
    fn a_click_finds_a_pane_where_it_was_moved_to() {
        let mut app = app_with_sessions(2);
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::NONE));
        // The selection's pane is on top now, the split below it.
        let areas = Areas::of(&app, Rect::new(0, 0, 80, 24));
        let on_top = areas.panes[0];
        let Hit::Pane { slot, .. } = hit(&areas, &app, 40, on_top.y + 2) else {
            panic!("not a pane");
        };
        assert_eq!(slot, Slot::Selected);
        // Its name on the header line below takes the split, to move it.
        let header = hit(&areas, &app, 31, areas.panes[1].y);
        let split = Hit::Pane {
            slot: Slot::Split(0),
            cell: None,
        };
        assert_eq!(header, split);
    }

    #[test]
    fn a_float_goes_over_the_middle_of_the_panes_and_takes_the_clicks_there() {
        let mut app = app_with_sessions(2);
        app.on_key(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::NONE));
        let areas = Areas::of(&app, Rect::new(0, 0, 120, 40));
        // Beside the sidebar and its rule there are 91 columns, and 38 rows
        // between the top bar and the footer: the frame takes eight tenths.
        let frame = areas.float.unwrap();
        assert_eq!(frame, Rect::new(38, 5, 72, 30));
        assert_eq!(areas.tiled().len(), 1);
        assert_eq!(*areas.panes.last().unwrap(), Rect::new(39, 6, 70, 28));

        // Inside it is the float, even over the pane under it; its frame
        // counts as its own; outside it, the pane under it.
        let on = |column, row| hit(&areas, &app, column, row);
        let inside = Hit::Pane {
            slot: Slot::Float,
            cell: Some((1, 1)),
        };
        assert_eq!(on(40, 8), inside);
        let edge = Hit::Pane {
            slot: Slot::Float,
            cell: None,
        };
        assert_eq!(on(38, 10), edge);
        assert!(matches!(
            on(32, 10),
            Hit::Pane {
                slot: Slot::Selected,
                ..
            }
        ));
    }

    #[test]
    fn a_float_keeps_a_small_terminals_size_while_there_is_room() {
        let mut app = app_with_sessions(1);
        app.on_key(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::NONE));
        // 51 by 22 beside the sidebar: eight tenths would be 40 by 17.
        let frame = Areas::of(&app, Rect::new(0, 0, 80, 24)).float.unwrap();
        assert_eq!((frame.width, frame.height), (51, 17));
    }

    #[test]
    fn a_long_sidebar_scrolls_to_the_selection_and_clicks_follow_it() {
        let mut app = app_with_sessions(30);
        app.select("s29");
        let areas = Areas::of(&app, Rect::new(0, 0, 80, 12));
        // Between the top bar and the footer, 10 rows fit: the selection is
        // drawn on the last of them, screen row 10.
        let last_visible = hit(&areas, &app, 5, 10);
        let Hit::SidebarRow(row) = last_visible else {
            panic!("not a row");
        };
        assert_eq!(app.rows()[row], Row::Session(29));
        assert!(screen_text(&app)[10].contains("s29"));
    }

    /// What crossterm writes to the terminal for `app`, drawn on a `width`
    /// by `height` screen: the bytes, not the cells.
    fn written(app: &App, width: u16, height: u16) -> String {
        use ratatui::backend::CrosstermBackend;
        use ratatui::{TerminalOptions, Viewport};
        let theme = theme();
        let options = TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, width, height)),
        };
        let mut out = Vec::new();
        let mut terminal = Terminal::with_options(CrosstermBackend::new(&mut out), options)
            .expect("a terminal over a Vec");
        terminal
            .draw(|frame| draw(frame, app, &[], None, &look(&theme)))
            .unwrap();
        drop(terminal);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn what_crystal_didnt_write_reaches_the_terminal_as_text_alone() {
        let sessions = printable::HOSTILE
            .iter()
            .enumerate()
            .map(|(n, hostile)| {
                let mut session = agent(in_worktree(&format!("{n}{hostile}"), hostile, false));
                if let Some(worktree) = &mut session.worktree {
                    worktree.project = hostile.to_string();
                }
                session.activity = Some(Activity::Waiting);
                session.line = Some(hostile.to_string());
                session.model = Some(hostile.to_string());
                session.reporter = Some(crate::protocol::Reporter {
                    agent: hostile.to_string(),
                    message: Some(hostile.to_string()),
                    resume: None,
                });
                session
            })
            .collect();
        let mut app = App::new(None);
        app.set_sessions(sessions);
        let out = written(&app, 160, 48);
        for order in printable::orders(&out) {
            // Where to draw, in which colors, and the cursor hidden while
            // it draws and shown again once the terminal's let go of.
            let cursor = ["\x1b[?25l", "\x1b[?25h"].contains(&order.as_str());
            let crosstermss =
                order.starts_with("\x1b[") && (order.ends_with('H') || order.ends_with('m'));
            let crosstermss = crosstermss || cursor;
            assert!(crosstermss, "{order:?} reached the terminal");
        }
        // The text around each order is drawn, and what's left of the
        // order with it, doing nothing.
        assert!(out.contains("]0;pwned"), "{out:?}");
        assert!(out.contains("[?1049h"), "{out:?}");
    }
}
