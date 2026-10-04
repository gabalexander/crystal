//! Drawing the TUI: a bar along the top with the tabs, the sidebar of
//! sessions, the tab's panes beside it, split as its tree has them, each
//! under a header line, and the footer. There are no boxes: thin rules and
//! the theme's colors tell the parts apart. Drawing only reads the state;
//! it never changes it.

use super::app::{App, Filter, Focus, Hit, PluginPane, Prompt, Question, Slot, View};
use super::backlog_view::{self, BacklogView};
use super::copy_mode::{self, SearchPrompt};
use super::diff_view;
use super::finder;
use super::grep;
use super::help;
use super::issues;
use super::launcher;
use super::layouts::{self, LayoutsView};
use super::memory_view;
use super::needs_you;
use super::pane::Pane;
use super::plugins_view;
use super::profiles;
use super::pull_requests;
use super::screen_widget::{Marks, ScreenWidget};
use super::settings_view;
use super::sidebar::{self, fit};
use super::split_tree::{Border, Way};
use super::status::Status;
use super::switcher;
use super::tabs::Tab;
use super::theme::Theme;
use super::timeline;
use super::tree_browser;
use crate::flow_run::RunState;
use crate::protocol::{SessionInfo, State, TaskState};
use crate::shell;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

const SIDEBAR_WIDTH: u16 = 28;

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
    pub top: Rect,
    /// Everything between the top bar and the footer: where an open view
    /// goes, in place of the sidebar and the panes.
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
    /// tree has them, or zoomed, one pane taking everything between the top
    /// bar and the footer, the sidebar and its rule with no room.
    pub fn of(app: &App, screen: Rect) -> Areas {
        let [top, main, footer] = rows(screen);
        let [sidebar, rule, tiles] = Layout::horizontal([
            Constraint::Length(SIDEBAR_WIDTH),
            Constraint::Length(1),
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

/// The top bar, everything between, and the footer.
fn rows(screen: Rect) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(screen)
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
/// line. The session is sized to fit it exactly.
pub fn screen_area(pane: Rect) -> Rect {
    let [_header, screen] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(pane);
    screen
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
    let index = app.slots().iter().position(|at| *at == slot)?;
    let screen = screen_area(*areas.panes.get(index)?);
    if screen.is_empty() {
        return None;
    }
    let column = column.clamp(screen.x, screen.right() - 1);
    let row = row.clamp(screen.y, screen.bottom() - 1);
    Some((row - screen.y, column - screen.x))
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
    // The float is over the others, its frame and all: the last pane
    // first.
    let panes = app.slots().into_iter().zip(&areas.panes).rev();
    for (slot, area) in panes {
        let frame = match (slot, areas.float) {
            (Slot::Float, Some(frame)) => frame,
            _ => *area,
        };
        if at(frame) {
            let screen = screen_area(*area);
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
pub fn draw(frame: &mut Frame, app: &App, panes: &[Pane], overlay: Option<&Pane>, look: &Look) {
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
    // Zoomed, the sidebar comes out over the pane while `/` looks through
    // it, rather than squeezing the pane, which its program would redraw
    // for.
    if app.zoomed() && app.filter().is_some() {
        let main = areas.main;
        let width = SIDEBAR_WIDTH.min(main.width.saturating_sub(1));
        let drawer = Rect::new(main.x, main.y, width, main.height);
        let rule = Rect::new(drawer.right(), main.y, 1, main.height);
        frame.render_widget(Clear, drawer.union(rule));
        frame.render_widget(Block::new().style(look.theme.base()), drawer.union(rule));
        sidebar::draw(frame, app, look, drawer);
        draw_rule(frame, look, rule);
    }
    // Over everything between the top bar and the footer.
    let below_top = areas.top.bottom();
    let middle = Rect::new(0, below_top, frame.area().width, areas.footer.y - below_top);
    if let Some(view) = app.issues_view() {
        issues::draw(frame, view, look.theme, look.now, middle);
    }
    if let Some(view) = app.pull_requests_view() {
        pull_requests::draw(frame, view, look.theme, look.now, middle);
    }
    if let Some(view) = app.backlog_view() {
        backlog_view::draw(frame, view, look.theme, middle);
    }
    if let Some(view) = app.layouts_view() {
        layouts::draw(frame, view, look.theme, look.now, middle);
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
    if let Some(view) = app.profiles_view() {
        let below_top = areas.top.bottom();
        let middle = Rect::new(0, below_top, frame.area().width, areas.footer.y - below_top);
        profiles::draw(frame, view, look.theme, middle);
    }
    if let Some(view) = app.plugins_view() {
        plugins_view::draw(frame, view, look.theme, middle);
    }
    if let Some(view) = app.settings_view() {
        settings_view::draw(frame, view, look.theme, middle);
    }
    if let (Some(open), Some(pane)) = (app.plugin_pane(), overlay) {
        draw_plugin_pane(frame, open, pane, look, &areas);
    }
    draw_footer(frame, app, panes, look, areas.footer);
    if app.showing_keys() {
        let plugin_on = |plugin: &str| app.plugin_on(plugin);
        let plugin_keys = app.plugin_key_rows();
        let shown = help::Shown {
            plugin_on: &plugin_on,
            plugin_keys: &plugin_keys,
        };
        help::draw(frame, look.theme, frame.area(), &shown, app.keys_page());
    }
}

/// Where a plugin's pane goes: over every pane, beside the sidebar.
fn plugin_pane_area(areas: &Areas) -> Rect {
    let left = areas.rule.right();
    let main = areas.main;
    Rect::new(left, main.y, main.right().saturating_sub(left), main.height)
}

/// The part of a plugin's pane its program's screen takes: inside its
/// frame.
pub fn plugin_pane_screen(areas: &Areas) -> Rect {
    Block::bordered().inner(plugin_pane_area(areas))
}

/// A plugin's pane: its program's screen in a frame, its title on top and
/// the key that closes it below.
fn draw_plugin_pane(frame: &mut Frame, open: &PluginPane, pane: &Pane, look: &Look, areas: &Areas) {
    let theme = look.theme;
    let area = plugin_pane_area(areas);
    frame.render_widget(Clear, area);
    let title = Style::new().fg(theme.accent).add_modifier(Modifier::BOLD);
    let block = Block::bordered()
        .border_style(Style::new().fg(theme.accent))
        .style(theme.base())
        .title(Line::styled(
            format!(" {} · {} ", open.plugin, open.title),
            title,
        ))
        .title_bottom(Line::styled(
            " ctrl+\\ closes ",
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
/// many wait on the user.
fn draw_top_bar(frame: &mut Frame, app: &App, look: &Look, area: Rect) {
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
    let summary = summary(app.sessions(), app.server(), theme);
    frame.render_widget(summary.right_aligned(), area);
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
/// of it, but for the server's name the summary starts with, if it does.
fn tabs_width(app: &App, area: Rect) -> u16 {
    let server = app
        .server()
        .map_or(0, |server| width_of(&server_label(server)));
    area.width.saturating_sub(server)
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
    let screen = screen_area(area);
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
        notes.push(session.state.to_string());
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
/// session runs and its command, then only where it runs.
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
    vec![format!("{place} · {command}"), place]
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
        frame.render_widget(hint_spans(&[("ctrl+\\", "close")], theme), area);
    } else if app.launcher().is_some() {
        frame.render_widget(hint_spans(LAUNCHER_HINTS, theme), area);
    } else if let Some(view) = app.profiles_view() {
        frame.render_widget(hint_spans(profiles::hints(view), theme), area);
    } else if app.plugins_view().is_some() {
        frame.render_widget(hint_spans(plugins_view::HINTS, theme), area);
    } else if app.settings_view().is_some() {
        frame.render_widget(hint_spans(settings_view::HINTS, theme), area);
    } else if let Some(prompt) = app.prompt() {
        draw_prompt(frame, theme, prompt, area);
    } else if let Some(view) = app.needs_you_view() {
        draw_notice_or(frame, app.notice(), needs_you::hints(view), theme, area);
    } else if app.timeline_view().is_some() {
        draw_notice_or(frame, app.notice(), timeline::HINTS, theme, area);
    } else if let Some(view) = app.issues_view() {
        draw_notice_or(frame, app.notice(), issues::hints(view), theme, area);
    } else if let Some(view) = app.pull_requests_view() {
        draw_notice_or(frame, app.notice(), pull_requests::hints(view), theme, area);
    } else if let Some(view) = app.backlog_view() {
        draw_backlog_footer(frame, view, theme, area);
    } else if let Some(view) = app.layouts_view() {
        draw_layouts_footer(frame, app.notice(), view, theme, area);
    } else if let Some(name) = app.closing() {
        let question = format!("close {name}'s task? d done · f failed · any other key, not yet");
        frame.render_widget(question_line(&question, theme), area);
    } else if let Some(name) = app.moving() {
        let question = format!("move {name} to tab 1-9, or t a new one · any other key, not yet");
        frame.render_widget(question_line(&question, theme), area);
    } else if let Some(filter) = app.filter() {
        draw_filter(frame, theme, filter, app.matches().len(), area);
    } else if let Some(confirm) = app.confirm() {
        frame.render_widget(question_line(&confirm.question(), theme), area);
    } else if let Some(prompt) = searching {
        draw_search_prompt(frame, theme, prompt, area);
    } else if let Some(notice) = app.notice() {
        let notice = Line::styled(format!(" {notice}"), Style::new().fg(theme.failed));
        frame.render_widget(notice, area);
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

/// The keys the footer offers in the sidebar, most needed first; the rest
/// are behind `?`.
const SIDEBAR_HINTS: &[(&str, &str)] = &[
    ("enter", "type"),
    ("n", "new"),
    ("s", "split"),
    ("x", "kill"),
    ("q", "quit"),
    ("z", "zoom"),
    ("v", "copy"),
    ("w", "worktree"),
    ("u", "next"),
    ("/", "find"),
    ("d", "diff"),
    ("p", "files"),
    ("t", "tab"),
    ("R", "resize"),
];

/// The sidebar's keys while the tab is zoomed: `j` and `k` choose the
/// session the one pane shows.
const ZOOMED_HINTS: &[(&str, &str)] = &[
    ("z", "unzoom"),
    ("enter", "type"),
    ("j/k", "switch"),
    ("v", "copy"),
    ("n", "new"),
    ("q", "quit"),
];

/// The sidebar's keys while the selected step's flow run waits at a gate.
const GATE_HINTS: &[(&str, &str)] = &[
    ("g", "go on"),
    ("f", "send back"),
    ("n", "new"),
    ("x", "kill"),
    ("q", "quit"),
    ("u", "next"),
    ("/", "find"),
    ("d", "diff"),
];

/// The sidebar's keys while the selected background task asks for a
/// permission.
const ASKING_HINTS: &[(&str, &str)] = &[
    ("y", "allow"),
    ("n", "deny"),
    ("Y", "always"),
    ("enter", "watch"),
    ("x", "kill"),
    ("q", "quit"),
    ("u", "next"),
];

/// The sidebar's keys while the selection is on a worktree with no
/// sessions.
const EMPTY_WORKTREE_HINTS: &[(&str, &str)] = &[
    ("n", "start one here"),
    ("W", "remove it"),
    ("d", "diff"),
    ("p", "files"),
    ("q", "quit"),
    ("u", "next"),
    ("/", "find"),
];

/// The sidebar's keys while the selected step's flow run has stopped, at a
/// step that failed or was cut short.
const STOPPED_HINTS: &[(&str, &str)] = &[
    ("g", "run again"),
    ("n", "new"),
    ("x", "kill"),
    ("q", "quit"),
    ("u", "next"),
    ("/", "find"),
    ("d", "diff"),
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
    ("space", "done/undone"),
    ("x", "remove"),
    ("/", "filter"),
    ("esc", "close"),
];

/// The footer while the backlog view is open: the item being added, the
/// question `x` asks, or the view's keys.
fn draw_backlog_footer(frame: &mut Frame, view: &BacklogView, theme: &Theme, area: Rect) {
    if let Some(adding) = &view.adding {
        let label = " add to the backlog: ";
        let line = Line::from(vec![
            Span::styled(
                label,
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(adding.text().to_string(), Style::new().fg(theme.text)),
        ]);
        frame.render_widget(line, area);
        // The label is plain ASCII, so its length in bytes is its width.
        let column = area.x + (label.len() + adding.cursor()) as u16;
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

/// `/`'s filter, with the cursor in it, and how many sessions match.
fn draw_filter(frame: &mut Frame, theme: &Theme, filter: &Filter, matches: usize, area: Rect) {
    let label = " find: ";
    let line = Line::from(vec![
        Span::styled(
            label,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(filter.input.text().to_string(), Style::new().fg(theme.text)),
    ]);
    frame.render_widget(line, area);
    let noun = if matches == 1 { "match" } else { "matches" };
    let count = Line::styled(format!("{matches} {noun} "), Style::new().fg(theme.muted));
    frame.render_widget(count.right_aligned(), area);
    // The label is plain ASCII, so its length in bytes is its width.
    let column = area.x + (label.len() + filter.input.cursor()) as u16;
    frame.set_cursor_position((column.min(area.right().saturating_sub(1)), area.y));
}

/// The keys that don't go to the program, while a pane has the keyboard.
const PANE_HINTS: &[(&str, &str)] = &[("ctrl+\\", "sidebar"), ("shift+pgup", "history")];

/// The keys a background task's pane takes, while it has the keyboard.
const TASK_PANE_HINTS: &[(&str, &str)] = &[
    ("ctrl+\\", "sidebar"),
    ("y/n/Y", "answer"),
    ("ctrl+c", "stop the run"),
    ("shift+pgup", "history"),
];

/// The keys in resize mode.
const RESIZE_HINTS: &[(&str, &str)] = &[
    ("h/j/k/l", "move a border"),
    ("=", "even out"),
    ("esc", "done"),
    ("shift+arrows", "another pane"),
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
    let (mut spans, hints) = match app.focus() {
        Focus::Sidebar if app.resizing() => (doing("resizing", app.selected()), RESIZE_HINTS),
        Focus::Sidebar => (whereabouts(app, theme, width), sidebar_hints(app)),
        Focus::Pane(slot) if app.pane_shows_task(slot) => {
            (doing("in", app.pane_session(slot)), TASK_PANE_HINTS)
        }
        Focus::Pane(slot) => (doing("typing into", app.pane_session(slot)), PANE_HINTS),
        Focus::Copy(slot) => {
            let hints = copying.map_or(&[][..], |pane| copy_mode::hints(&pane.screen));
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
fn sidebar_hints(app: &App) -> &'static [(&'static str, &'static str)] {
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

    fn theme() -> Theme {
        Theme::new(ThemeName::Dark, false)
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

    /// The sidebar's rows on an 80 by 12 screen, up to the rule: what's
    /// beside them, like the pane's header, left out.
    fn sidebar_text(app: &App) -> Vec<String> {
        screen_text(app)
            .iter()
            .map(|line| line.chars().take(usize::from(SIDEBAR_WIDTH) + 1).collect())
            .collect()
    }

    fn session(name: &str, state: State) -> SessionInfo {
        SessionInfo {
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
    fn the_keys_overlay_draws_over_an_80_by_24_screen_a_page_at_a_time() {
        let mut app = App::new(None);
        app.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        let text = screen_text_at(&app, 80, 24).join("\n");
        for on_screen in ["In the sidebar", "select a session", "1/2 · ← → turn"] {
            assert!(text.contains(on_screen), "{on_screen} isn't on screen");
        }
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let text = screen_text_at(&app, 80, 24).join("\n");
        for on_screen in ["In a pane", "Ctrl+\\", "With the mouse", "2/2"] {
            assert!(text.contains(on_screen), "{on_screen} isn't on screen");
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
        // The one waiting is in the first tab, out of this one's sidebar.
        let sidebar = sidebar_text(&app);
        assert!(
            sidebar.iter().any(|line| line.contains("❯ b")),
            "{sidebar:?}"
        );
        assert!(
            !sidebar.iter().any(|line| line.contains("▲ a")),
            "{sidebar:?}"
        );
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
    fn a_pane_header_notes_how_its_session_ended() {
        let theme = theme();
        let ended = session("done", State::Exited { code: 3 });
        let notes = vec!["exited 3".to_string()];
        let header = text_of(&pane_header(&ended, &notes, false, &look(&theme), 60));
        assert!(header.starts_with(" ■ done · exited 3 ─"), "{header}");
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
        assert_eq!(screen_area(areas.panes[0]), Rect::new(0, 2, 80, 21));
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
        // The screen is at column 29, row 2, 51 by 21.
        let nearest = |column, row| nearest_cell(&areas, &app, Slot::Selected, column, row);
        assert_eq!(nearest(31, 3), Some((1, 2)));
        assert_eq!(nearest(5, 0), Some((0, 0)));
        assert_eq!(nearest(200, 200), Some((20, 50)));
        assert_eq!(nearest_cell(&areas, &app, Slot::Split(0), 31, 3), None);
    }

    #[test]
    fn the_session_screen_sits_below_its_header() {
        let areas = Areas::of(&App::new(None), Rect::new(0, 0, 80, 24));
        assert_eq!(areas.top, Rect::new(0, 0, 80, 1));
        assert_eq!(areas.rule, Rect::new(28, 1, 1, 22));
        assert_eq!(areas.tiles, Rect::new(29, 1, 51, 22));
        assert_eq!(areas.panes, [areas.tiles], "one pane takes all the room");
        assert_eq!(screen_area(areas.panes[0]), Rect::new(29, 2, 51, 21));
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
        assert_eq!(hit(&areas, &app, 28, 5), Hit::Elsewhere, "the rule");
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
}
