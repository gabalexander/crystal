//! The settings view, `,`: the settings crystal reads as it goes, each
//! changed with a key and written to the config file at once, and how the
//! model that searches memory by meaning stands, as the daemon says it does.
//! While the view is open, the event loop reads both again every half a
//! second, so what it shows follows the file, a download or the daemon,
//! whoever changed them. The event loop does the writing (see
//! [`crate::config::set`]).
//!
//! The view is state and logic only, apart from [`draw`] at the end.

use super::appearance::Appearance;
use super::theme::{self, Theme};
use crate::config::{BarPosition, Config, SessionSettings, ThemeName};
use crate::embed::Status;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use std::path::PathBuf;

/// The settings as they are, read off the loop.
#[derive(Debug, Clone)]
pub struct Current {
    /// The config file, and what it says, or why it can't be read.
    pub path: PathBuf,
    pub config: Result<Config, String>,
    /// How the model stands, as the daemon said; `None` when it couldn't.
    pub model: Option<Status>,
}

/// A setting the view changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Notify,
    NotifyAfter,
    UnfocusedOnly,
    Sound,
    Theme,
    AutoSwitch,
    TabBar,
    HideSingleTab,
    StopIdle,
    Distill,
    Embeddings,
}

/// A change to one setting, to write to the config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Notify(bool),
    NotifyAfter(u64),
    UnfocusedOnly(bool),
    Sound(bool),
    /// The theme, which, picked by hand, stops it following the system's
    /// appearance.
    Theme(ThemeName),
    AutoSwitch(bool),
    TabBar(BarPosition),
    HideSingleTab(bool),
    /// How long an agent may sit idle, one of [`SessionSettings::CHOICES`].
    StopIdle(&'static str),
    Distill(bool),
    Embeddings(bool),
}

impl Change {
    /// Where the setting is in the config file.
    pub fn keys(self) -> &'static [&'static str] {
        match self {
            Change::Notify(_) => &["notify"],
            Change::NotifyAfter(_) => &["notifications", "after_secs"],
            Change::UnfocusedOnly(_) => &["notifications", "unfocused_only"],
            Change::Sound(_) => &["sound", "enabled"],
            Change::Theme(_) => &["theme"],
            Change::AutoSwitch(_) => &["appearance", "auto_switch"],
            Change::TabBar(_) => &["tab_bar", "position"],
            Change::HideSingleTab(_) => &["tab_bar", "hide_when_single"],
            Change::StopIdle(_) => &["sessions", "stop_idle_after"],
            Change::Distill(_) => &["memory", "distill"],
            Change::Embeddings(_) => &["memory", "embeddings"],
        }
    }

    pub fn value(self) -> toml_edit::Value {
        match self {
            Change::Notify(on)
            | Change::UnfocusedOnly(on)
            | Change::Sound(on)
            | Change::Distill(on)
            | Change::Embeddings(on)
            | Change::AutoSwitch(on)
            | Change::HideSingleTab(on) => on.into(),
            Change::TabBar(BarPosition::Top) => "top".into(),
            Change::TabBar(BarPosition::Bottom) => "bottom".into(),
            Change::NotifyAfter(secs) => i64::try_from(secs).unwrap_or(i64::MAX).into(),
            Change::Theme(theme) => theme.name().into(),
            Change::StopIdle(after) => after.into(),
        }
    }
}

/// The waits `←/→` go through for how long a session needs the user
/// before they're told, in seconds.
const NOTIFY_AFTER: [u64; 6] = [0, 10, 30, 60, 120, 300];

/// The wait after `secs` among [`NOTIFY_AFTER`], or before it: one set by
/// hand between two goes to the next, or the one before.
fn next_wait(secs: u64, forward: bool) -> u64 {
    if forward {
        let next = NOTIFY_AFTER.iter().find(|&&wait| wait > secs);
        *next.unwrap_or(&NOTIFY_AFTER[0])
    } else {
        let before = NOTIFY_AFTER.iter().rev().find(|&&wait| wait < secs);
        *before.unwrap_or(&NOTIFY_AFTER[NOTIFY_AFTER.len() - 1])
    }
}

/// A wait as the view shows it.
fn wait_text(secs: u64) -> String {
    match secs {
        0 => "at once".to_string(),
        secs if secs % 60 == 0 => format!("{}m", secs / 60),
        secs => format!("{secs}s"),
    }
}

/// What a key in the view leads to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    Change(Change),
    /// Have the daemon download the model if it isn't here, and give every
    /// entry its vector.
    Prepare,
}

/// The settings the bar can be on, in the order they're listed.
const SETTINGS: [Setting; 11] = [
    Setting::Notify,
    Setting::NotifyAfter,
    Setting::UnfocusedOnly,
    Setting::Sound,
    Setting::Theme,
    Setting::AutoSwitch,
    Setting::TabBar,
    Setting::HideSingleTab,
    Setting::StopIdle,
    Setting::Distill,
    Setting::Embeddings,
];

pub struct SettingsView {
    /// `None` until the settings have been read.
    current: Option<Current>,
    /// The setting the bar is on.
    selected: usize,
    /// Why the last thing asked for couldn't be done.
    problem: Option<String>,
}

impl SettingsView {
    pub fn new() -> SettingsView {
        SettingsView {
            current: None,
            selected: 0,
            problem: None,
        }
    }

    /// Takes the settings as they are now.
    pub fn set_current(&mut self, current: Current) {
        self.current = Some(current);
    }

    pub fn set_problem(&mut self, problem: String) {
        self.problem = Some(problem);
    }

    #[cfg(test)]
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    fn config(&self) -> Option<&Config> {
        self.current.as_ref()?.config.as_ref().ok()
    }

    fn model(&self) -> Option<&Status> {
        self.current.as_ref()?.model.as_ref()
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        self.problem = None;
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return Outcome::Stay;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | ',') => Outcome::Close,
            KeyCode::Char('j') | KeyCode::Down => {
                self.selected = (self.selected + 1).min(SETTINGS.len() - 1);
                Outcome::Stay
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                Outcome::Stay
            }
            KeyCode::Char(' ') | KeyCode::Right | KeyCode::Char('l') => self.change(true),
            KeyCode::Left | KeyCode::Char('h') => self.change(false),
            KeyCode::Enter => match SETTINGS[self.selected] {
                Setting::Embeddings => self.prepare(),
                _ => self.change(true),
            },
            _ => Outcome::Stay,
        }
    }

    /// The change the key asks for on the setting the bar is on: a switch
    /// turns over, the theme goes to the next, or `forward` false, the one
    /// before.
    fn change(&mut self, forward: bool) -> Outcome {
        let config = match &self.current {
            None => return Outcome::Stay,
            Some(Current {
                config: Err(why), ..
            }) => {
                self.problem = Some(format!("the config file can't be read: {why}"));
                return Outcome::Stay;
            }
            Some(Current { config: Ok(c), .. }) => c,
        };
        let change = match SETTINGS[self.selected] {
            Setting::Notify => Change::Notify(!config.notify),
            Setting::NotifyAfter => {
                Change::NotifyAfter(next_wait(config.notifications.after_secs, forward))
            }
            Setting::UnfocusedOnly => Change::UnfocusedOnly(!config.notifications.unfocused_only),
            Setting::Sound => Change::Sound(!config.sound.enabled),
            Setting::Theme if forward => Change::Theme(config.theme.next()),
            Setting::Theme => Change::Theme(config.theme.previous()),
            Setting::AutoSwitch => Change::AutoSwitch(!config.appearance.auto_switch),
            Setting::TabBar => Change::TabBar(match config.tab_bar.position {
                BarPosition::Top => BarPosition::Bottom,
                BarPosition::Bottom => BarPosition::Top,
            }),
            Setting::HideSingleTab => Change::HideSingleTab(!config.tab_bar.hide_when_single),
            Setting::StopIdle => {
                let choices = SessionSettings::CHOICES;
                let now = &config.sessions.stop_idle_after;
                // One written by hand goes on to the first choice.
                let at = choices.iter().position(|choice| choice == now);
                let next = match (at, forward) {
                    (Some(at), true) => (at + 1) % choices.len(),
                    (Some(at), false) => (at + choices.len() - 1) % choices.len(),
                    (None, _) => 0,
                };
                Change::StopIdle(choices[next])
            }
            Setting::Distill => Change::Distill(!config.memory.distill),
            Setting::Embeddings => Change::Embeddings(!config.memory.embeddings),
        };
        Outcome::Change(change)
    }

    /// Enter on search by meaning: get the model ready, once it's on.
    fn prepare(&mut self) -> Outcome {
        match self.config() {
            Some(config) if config.memory.embeddings => Outcome::Prepare,
            Some(_) => {
                self.problem = Some("search by meaning is off: space turns it on".to_string());
                Outcome::Stay
            }
            None => Outcome::Stay,
        }
    }
}

impl Default for SettingsView {
    fn default() -> SettingsView {
        SettingsView::new()
    }
}

/// The keys while the view is open.
pub const HINTS: &[(&str, &str)] = &[
    ("space", "change"),
    ("enter", "get the model"),
    ("j/k", "move"),
    ("esc", "close"),
];

/// What the models line says of how the models stand.
fn model_says(status: Option<&Status>, on: bool) -> (String, Option<bool>) {
    let Some(status) = status else {
        return ("the daemon didn't say".to_string(), None);
    };
    let mb = |bytes: u64| bytes / 1_000_000;
    if let Some(doing) = &status.preparing {
        if !status.is_downloaded() {
            let got = format!("{} of {} MB", mb(status.on_disk), mb(status.size));
            return (format!("{doing} · {got}"), None);
        }
        return (format!("{doing}…"), None);
    }
    if let Some(failed) = &status.failed {
        return (format!("couldn't get them ready: {failed}"), Some(false));
    }
    if !status.is_downloaded() {
        let size = mb(status.size);
        return match on {
            true => (
                format!("not downloaded: enter gets them ({size} MB)"),
                Some(false),
            ),
            false => (format!("not downloaded ({size} MB)"), None),
        };
    }
    match (on, status.loaded) {
        (_, true) => ("downloaded, loaded in the daemon".to_string(), Some(true)),
        (true, false) => (
            "downloaded; loads at the next search".to_string(),
            Some(true),
        ),
        (false, false) => ("downloaded, not loaded".to_string(), None),
    }
}

/// Draws the view over the middle of `area`, the rest dimmed behind it.
pub fn draw(frame: &mut Frame, view: &SettingsView, theme: &Theme, area: Rect) {
    frame
        .buffer_mut()
        .set_style(area, Style::new().add_modifier(Modifier::DIM));
    let lines = lines(view, theme);
    let width = area.width.saturating_sub(4).clamp(40.min(area.width), 84);
    let height = (lines.len() as u16 + 2).min(area.height);
    let top = if area.height > height { 1 } else { 0 };
    let panel = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + top,
        width,
        height,
    );
    frame.render_widget(Clear, panel);
    let framed = theme.panel == Color::Reset;
    let block = if framed {
        Block::bordered().border_style(Style::new().fg(theme.rule))
    } else {
        Block::new()
    };
    let block = block.style(Style::new().bg(theme.panel).fg(theme.text));
    let inside = block
        .inner(panel)
        .inner(Margin::new(if framed { 1 } else { 2 }, 1));
    frame.render_widget(block, panel);
    frame.render_widget(Paragraph::new(lines), inside);
}

/// The view's lines: where the file is, then each part's settings, each
/// with what it does or how it stands.
fn lines(view: &SettingsView, theme: &Theme) -> Vec<Line<'static>> {
    let muted = Style::new().fg(theme.muted);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let Some(current) = &view.current else {
        return vec![Line::styled("reading the settings…", muted)];
    };
    let path = crate::shell::home_relative(&current.path);
    let mut lines = vec![Line::from(vec![
        Span::styled("settings", bold),
        Span::styled(format!("  {path}"), muted),
    ])];
    let config = match &current.config {
        Ok(config) => config,
        Err(why) => {
            lines.push(Line::from(""));
            let said = format!("the file can't be read: {why}");
            lines.push(Line::styled(said, Style::new().fg(theme.failed)));
            return lines;
        }
    };
    let memory_on = crate::memory::enabled(config);
    let row = |setting: Setting, on: Option<bool>, value: String, about: String| {
        let selected = SETTINGS[view.selected] == setting;
        let name = match setting {
            Setting::Notify => "notifications",
            Setting::NotifyAfter => "  after",
            Setting::UnfocusedOnly => "  only when away",
            Setting::Sound => "sounds",
            Setting::Theme => "theme",
            Setting::AutoSwitch => "  follow the system",
            Setting::TabBar => "tab bar",
            Setting::HideSingleTab => "  hide with one tab",
            Setting::StopIdle => "stop idle agents",
            Setting::Distill => "distill closed tasks",
            Setting::Embeddings => "search by meaning",
        };
        let dim = (matches!(setting, Setting::Distill | Setting::Embeddings) && !memory_on)
            || (matches!(setting, Setting::NotifyAfter | Setting::UnfocusedOnly) && !config.notify);
        let (mark, color) = match on {
            Some(true) => ("● ", theme.done),
            Some(false) => ("○ ", theme.muted),
            None => ("  ", theme.muted),
        };
        let text = if dim { theme.muted } else { theme.text };
        let line = Line::from(vec![
            Span::styled(mark, Style::new().fg(color)),
            Span::styled(format!("{name:<22}"), Style::new().fg(text)),
            // As wide as the longest theme's name, and a space.
            Span::styled(format!("{value:<17}"), Style::new().fg(theme.accent)),
            Span::styled(about, muted),
        ]);
        if selected {
            line.style(theme.selection)
        } else {
            line
        }
    };
    let on_off = |on: bool| if on { "on" } else { "off" }.to_string();
    let detail = |label: &str, said: String, good: Option<bool>| {
        let color = match good {
            Some(false) => theme.failed,
            _ => theme.muted,
        };
        Line::from(vec![
            Span::raw(format!("    {label:<20}")),
            Span::styled(said, Style::new().fg(color)),
        ])
    };

    lines.push(Line::from(""));
    lines.push(Line::styled("General", bold));
    lines.push(row(
        Setting::Notify,
        Some(config.notify),
        on_off(config.notify),
        "tell you when a session needs you".to_string(),
    ));
    let notifications = &config.notifications;
    lines.push(row(
        Setting::NotifyAfter,
        None,
        wait_text(notifications.after_secs),
        "once it has needed you this long: ←/→ to change".to_string(),
    ));
    lines.push(row(
        Setting::UnfocusedOnly,
        Some(notifications.unfocused_only),
        on_off(notifications.unfocused_only),
        "only while crystal's terminal hasn't the focus".to_string(),
    ));
    let whose = if config.colors.is_empty() {
        "the TUI's colors"
    } else {
        "under your [colors]"
    };
    let themes = ThemeName::all().count();
    let at = config.theme.position() + 1;
    lines.push(row(
        Setting::Sound,
        Some(config.sound.enabled),
        on_off(config.sound.enabled),
        "a chime at the same moments".to_string(),
    ));
    lines.push(row(
        Setting::Theme,
        None,
        config.theme.name().to_string(),
        format!("{whose}: ←/→ ({at} of {themes})"),
    ));
    let appearance = &config.appearance;
    let sides = format!(
        "{} when it's light, {} when dark",
        theme::for_appearance(config, Appearance::Light).name(),
        theme::for_appearance(config, Appearance::Dark).name()
    );
    lines.push(row(
        Setting::AutoSwitch,
        Some(appearance.auto_switch),
        on_off(appearance.auto_switch),
        sides,
    ));
    let bar = &config.tab_bar;
    lines.push(row(
        Setting::TabBar,
        None,
        match bar.position {
            BarPosition::Top => "top",
            BarPosition::Bottom => "bottom",
        }
        .to_string(),
        "above the panes, or over the footer: ←/→".to_string(),
    ));
    lines.push(row(
        Setting::HideSingleTab,
        Some(bar.hide_when_single),
        on_off(bar.hide_when_single),
        "while there's only the one tab".to_string(),
    ));
    let idle = &config.sessions.stop_idle_after;
    lines.push(row(
        Setting::StopIdle,
        Some(config.sessions.idle_limit().is_some()),
        if config.sessions.idle_limit().is_some() {
            format!("after {idle}")
        } else {
            "off".to_string()
        },
        "at their prompt, unwatched: they start again where they were".to_string(),
    ));

    lines.push(Line::from(""));
    lines.push(Line::styled("Memory", bold));
    let memory = &config.memory;
    lines.push(row(
        Setting::Distill,
        Some(memory.distill),
        on_off(memory.distill),
        format!(
            "{}, at most ${} a task",
            memory.distill_model, memory.distill_budget_usd
        ),
    ));
    lines.push(row(
        Setting::Embeddings,
        Some(memory.embeddings),
        on_off(memory.embeddings),
        match memory.rerank {
            true => "jina v5 and its reranker, on this machine",
            false => "jina v5, on this machine",
        }
        .to_string(),
    ));
    let (said, good) = model_says(view.model(), memory.embeddings);
    lines.push(detail("models", said, good));
    if let Some(status) = view.model()
        && (memory.embeddings || status.is_downloaded())
    {
        let said = format!(
            "{} of {} have their vector",
            status.embedded, status.entries
        );
        let good = (status.entries > 0).then_some(status.embedded == status.entries);
        lines.push(detail("entries", said, good.filter(|all| *all)));
    }
    if !memory_on {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            "the memory plugin is off, so these do nothing: X switches it on",
            muted,
        ));
    }
    if let Some(problem) = &view.problem {
        lines.push(Line::from(""));
        lines.push(Line::styled(problem.clone(), Style::new().fg(theme.failed)));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(view: &mut SettingsView, code: KeyCode) -> Outcome {
        view.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn view_of(config: Config, model: Option<Status>) -> SettingsView {
        let mut view = SettingsView::new();
        view.set_current(Current {
            path: PathBuf::from("/home/ann/.config/crystal/config.toml"),
            config: Ok(config),
            model,
        });
        view
    }

    fn status() -> Status {
        Status {
            on_disk: 0,
            size: 134_000_000,
            ..Status::default()
        }
    }

    fn text(view: &SettingsView) -> String {
        let theme = Theme::new(ThemeName::DARK, true);
        let lines = lines(view, &theme);
        let lines: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        lines.join("\n")
    }

    #[test]
    fn space_turns_the_setting_the_bar_is_on_over() {
        let mut view = view_of(Config::default(), Some(status()));
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Change(Change::Notify(false))
        );
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Right),
            Outcome::Change(Change::NotifyAfter(10))
        );
        assert_eq!(
            press(&mut view, KeyCode::Left),
            Outcome::Change(Change::NotifyAfter(300))
        );
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Change(Change::UnfocusedOnly(true))
        );
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Change(Change::Sound(false))
        );
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Right),
            Outcome::Change(Change::Theme(ThemeName::LIGHT))
        );
        // Back from the first is the last.
        assert_eq!(
            press(&mut view, KeyCode::Left),
            Outcome::Change(Change::Theme(ThemeName::all().last().unwrap()))
        );
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Change(Change::AutoSwitch(true))
        );
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Right),
            Outcome::Change(Change::TabBar(BarPosition::Bottom))
        );
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Change(Change::HideSingleTab(true))
        );
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Right),
            Outcome::Change(Change::StopIdle("15m"))
        );
        assert_eq!(
            press(&mut view, KeyCode::Left),
            Outcome::Change(Change::StopIdle("8h"))
        );
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Change(Change::Distill(false))
        );
        // The bar stops at the last.
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Change(Change::Embeddings(false))
        );
        assert_eq!(press(&mut view, KeyCode::Char(',')), Outcome::Close);
    }

    #[test]
    fn the_wait_goes_through_its_steps_and_round() {
        assert_eq!(next_wait(0, true), 10);
        assert_eq!(next_wait(300, true), 0);
        assert_eq!(next_wait(45, true), 60);
        assert_eq!(next_wait(45, false), 30);
        assert_eq!(next_wait(0, false), 300);
        assert_eq!(wait_text(0), "at once");
        assert_eq!(wait_text(120), "2m");
        assert_eq!(wait_text(45), "45s");
    }

    #[test]
    fn the_theme_row_says_which_of_the_themes_it_is() {
        let mut config = Config {
            theme: ThemeName::find("catppuccin-latte").unwrap(),
            ..Config::default()
        };
        let shown = text(&view_of(config.clone(), None));
        let at = config.theme.position() + 1;
        let row = format!("catppuccin-latte the TUI's colors: ←/→ ({at} of 20)");
        assert!(shown.contains(&row), "{shown}");

        config.colors.insert(
            crate::config::ColorToken::Accent,
            crate::config::ColorValue(Color::Red),
        );
        let shown = text(&view_of(config, None));
        assert!(shown.contains("under your [colors]: ←/→"), "{shown}");
    }

    #[test]
    fn following_the_system_says_which_theme_each_side_is() {
        let config = Config {
            theme: ThemeName::find("kanagawa").unwrap(),
            ..Config::default()
        };
        let shown = text(&view_of(config.clone(), None));
        let row = "○   follow the system   off              \
                   kanagawa-lotus when it's light, kanagawa when dark";
        assert!(shown.contains(row), "{shown}");
        assert!(shown.contains("  tab bar               top"), "{shown}");
        let mut config = config;
        config.appearance.auto_switch = true;
        config.appearance.dark_theme = ThemeName::find("dracula");
        let shown = text(&view_of(config, None));
        assert!(shown.contains("●   follow the system   on "), "{shown}");
        assert!(
            shown.contains("when it's light, dracula when dark"),
            "{shown}"
        );
    }

    #[test]
    fn a_change_says_where_it_goes_in_the_file() {
        assert_eq!(
            Change::AutoSwitch(true).keys(),
            ["appearance", "auto_switch"]
        );
        assert_eq!(
            Change::TabBar(BarPosition::Bottom).value().as_str(),
            Some("bottom")
        );
        assert_eq!(
            Change::HideSingleTab(true).keys(),
            ["tab_bar", "hide_when_single"]
        );
        assert_eq!(Change::Embeddings(true).keys(), ["memory", "embeddings"]);
        assert_eq!(
            Change::NotifyAfter(30).keys(),
            ["notifications", "after_secs"]
        );
        assert_eq!(Change::NotifyAfter(30).value().as_integer(), Some(30));
        assert_eq!(
            Change::Theme(ThemeName::LIGHT).value().as_str(),
            Some("light")
        );
        assert_eq!(Change::Notify(false).value().as_bool(), Some(false));
        assert_eq!(
            Change::StopIdle("30m").keys(),
            ["sessions", "stop_idle_after"]
        );
        assert_eq!(Change::Sound(true).keys(), ["sound", "enabled"]);
    }

    #[test]
    fn enter_gets_the_model_only_once_search_by_meaning_is_on() {
        let mut off = Config::default();
        off.memory.embeddings = false;
        let mut view = view_of(off, Some(status()));
        for _ in 0..SETTINGS.len() - 1 {
            press(&mut view, KeyCode::Down);
        }
        assert_eq!(press(&mut view, KeyCode::Enter), Outcome::Stay);
        assert_eq!(
            view.problem(),
            Some("search by meaning is off: space turns it on")
        );
        let mut config = Config::default();
        config.memory.embeddings = true;
        view.set_current(Current {
            path: PathBuf::from("/c"),
            config: Ok(config),
            model: Some(status()),
        });
        assert_eq!(press(&mut view, KeyCode::Enter), Outcome::Prepare);
    }

    #[test]
    fn a_file_that_can_t_be_read_is_said_and_never_written() {
        let mut view = SettingsView::new();
        assert_eq!(press(&mut view, KeyCode::Char(' ')), Outcome::Stay);
        assert!(text(&view).contains("reading the settings"));
        view.set_current(Current {
            path: PathBuf::from("/c"),
            config: Err("`notfy` is not a setting".into()),
            model: None,
        });
        assert_eq!(press(&mut view, KeyCode::Char(' ')), Outcome::Stay);
        assert!(view.problem().unwrap().contains("can't be read"));
        assert!(text(&view).contains("`notfy` is not a setting"));
    }

    #[test]
    fn the_model_line_follows_the_download_and_the_daemon() {
        let config = Config::default();
        let downloading = Status {
            on_disk: 42_000_000,
            preparing: Some("downloading the models".into()),
            ..status()
        };
        let view = view_of(config.clone(), Some(downloading));
        assert!(
            text(&view).contains("downloading the models · 42 of 134 MB"),
            "{}",
            text(&view)
        );
        let ready = Status {
            on_disk: 134_000_000,
            loaded: true,
            entries: 40,
            embedded: 37,
            ..status()
        };
        let view = view_of(config.clone(), Some(ready.clone()));
        assert!(text(&view).contains("downloaded, loaded in the daemon"));
        assert!(text(&view).contains("37 of 40 have their vector"));

        let missing = view_of(config, Some(status()));
        assert!(text(&missing).contains("not downloaded: enter gets them (134 MB)"));
        let mut config = Config::default();
        config.memory.embeddings = false;
        let off = view_of(config, Some(status()));
        assert!(text(&off).contains("not downloaded (134 MB)"));
        assert!(!text(&off).contains("have their vector"));
        let failed = Status {
            failed: Some("couldn't download it".into()),
            ..ready
        };
        assert!(
            text(&view_of(Config::default(), Some(failed))).contains("couldn't get them ready")
        );
    }

    #[test]
    fn with_memory_off_its_settings_say_they_do_nothing() {
        let mut config = Config::default();
        config.plugins.insert("memory".into(), false);
        let view = view_of(config, None);
        assert!(text(&view).contains("the memory plugin is off"));
        assert!(text(&view).contains("the daemon didn't say"));
    }
}
