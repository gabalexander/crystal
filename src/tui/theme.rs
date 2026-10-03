//! The TUI's colors, each named for what it's for. The drawing code asks for
//! "muted" or "waiting", never for a color, so a theme can change every one
//! of them in one place.
//!
//! `dark` and `light` paint their own background, so they look the same in
//! any terminal. `terminal` paints nothing and uses the terminal's own
//! sixteen colors. With `NO_COLOR` set there's no color at all, as that
//! convention asks: only bold and reversed text tell things apart.

use super::status::Status;
use crate::config::ThemeName;
use ratatui::style::{Color, Modifier, Style};

pub struct Theme {
    /// Painted behind everything. `Color::Reset` leaves the terminal's own.
    pub background: Color,
    pub text: Color,
    /// For what's there to be found but not read first: hints, times.
    pub muted: Color,
    /// For what has the keyboard, and for crystal's own name.
    pub accent: Color,
    /// Thin lines between the parts.
    pub rule: Color,
    pub branch: Color,
    /// How the selected sidebar row stands out.
    pub selection: Style,
    /// Behind what's drawn over the rest, like the list of keys. `Reset`
    /// when the theme can't paint one, and a thin frame stands in for it.
    pub panel: Color,
    pub waiting: Color,
    pub working: Color,
    pub done: Color,
    pub running: Color,
    /// A program that ended well.
    pub ended: Color,
    /// A program that failed, and errors.
    pub failed: Color,
    /// How many lines a diff adds, and the letter of a file it adds.
    pub added: Color,
    /// How many lines a diff removes, and the letter of a file it deletes.
    pub removed: Color,
    /// A line a diff adds: a green tint behind it, where a theme paints.
    pub added_line: Style,
    /// A line a diff removes: a red tint behind it.
    pub removed_line: Style,
    /// The words that changed inside an added line: the stronger green.
    pub added_words: Style,
    /// The words that changed inside a removed line: the stronger red.
    pub removed_words: Style,
}

impl Theme {
    /// The theme called `name`, or none at all when `no_color` is set.
    pub fn new(name: ThemeName, no_color: bool) -> Theme {
        if no_color {
            return Theme::plain();
        }
        match name {
            ThemeName::Dark => Theme::dark(),
            ThemeName::Light => Theme::light(),
            ThemeName::Terminal => Theme::terminal(),
        }
    }

    /// The theme the user asked for in their config, unless the
    /// environment asks for no color.
    pub fn from_env(name: ThemeName) -> Theme {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        Theme::new(name, no_color)
    }

    /// Deep ink, with a violet accent.
    fn dark() -> Theme {
        Theme {
            background: Color::Rgb(16, 18, 24),
            text: Color::Rgb(208, 213, 222),
            muted: Color::Rgb(104, 112, 128),
            accent: Color::Rgb(168, 148, 255),
            rule: Color::Rgb(44, 48, 60),
            branch: Color::Rgb(102, 204, 190),
            selection: Style::new().bg(Color::Rgb(34, 38, 52)),
            panel: Color::Rgb(26, 29, 40),
            waiting: Color::Rgb(255, 164, 84),
            working: Color::Rgb(110, 170, 255),
            done: Color::Rgb(120, 220, 140),
            running: Color::Rgb(124, 178, 142),
            ended: Color::Rgb(104, 112, 128),
            failed: Color::Rgb(240, 104, 112),
            added: Color::Rgb(120, 220, 140),
            removed: Color::Rgb(240, 104, 112),
            added_line: Style::new().bg(Color::Rgb(22, 48, 36)),
            removed_line: Style::new().bg(Color::Rgb(58, 26, 32)),
            added_words: Style::new().bg(Color::Rgb(36, 92, 60)),
            removed_words: Style::new().bg(Color::Rgb(112, 40, 52)),
        }
    }

    /// Warm paper, with the same roles a shade darker.
    fn light() -> Theme {
        Theme {
            background: Color::Rgb(250, 249, 246),
            text: Color::Rgb(34, 37, 43),
            muted: Color::Rgb(122, 126, 136),
            accent: Color::Rgb(108, 82, 224),
            rule: Color::Rgb(222, 223, 228),
            branch: Color::Rgb(13, 128, 115),
            selection: Style::new().bg(Color::Rgb(234, 232, 244)),
            panel: Color::Rgb(240, 238, 248),
            waiting: Color::Rgb(196, 98, 16),
            working: Color::Rgb(30, 110, 214),
            done: Color::Rgb(28, 135, 64),
            running: Color::Rgb(70, 125, 92),
            ended: Color::Rgb(122, 126, 136),
            failed: Color::Rgb(196, 40, 52),
            added: Color::Rgb(28, 135, 64),
            removed: Color::Rgb(196, 40, 52),
            added_line: Style::new().bg(Color::Rgb(226, 246, 232)),
            removed_line: Style::new().bg(Color::Rgb(253, 232, 232)),
            added_words: Style::new().bg(Color::Rgb(170, 230, 188)),
            removed_words: Style::new().bg(Color::Rgb(248, 186, 186)),
        }
    }

    /// The terminal's own sixteen colors, nothing painted. A background
    /// bar can't be made subtle without knowing the terminal's colors, so
    /// the selection is reversed.
    fn terminal() -> Theme {
        Theme {
            background: Color::Reset,
            text: Color::Reset,
            muted: Color::DarkGray,
            accent: Color::Magenta,
            rule: Color::DarkGray,
            branch: Color::Cyan,
            selection: Style::new().add_modifier(Modifier::REVERSED),
            panel: Color::Reset,
            waiting: Color::Yellow,
            working: Color::LightBlue,
            done: Color::Green,
            running: Color::Gray,
            ended: Color::DarkGray,
            failed: Color::Red,
            added: Color::Green,
            removed: Color::Red,
            // No tint can be picked without knowing the terminal's colors:
            // the lines take the color, and changed words are reversed.
            added_line: Style::new().fg(Color::Green),
            removed_line: Style::new().fg(Color::Red),
            added_words: Style::new()
                .fg(Color::Green)
                .add_modifier(Modifier::REVERSED),
            removed_words: Style::new().fg(Color::Red).add_modifier(Modifier::REVERSED),
        }
    }

    /// No color at all: every color is the terminal's default.
    fn plain() -> Theme {
        Theme {
            background: Color::Reset,
            text: Color::Reset,
            muted: Color::Reset,
            accent: Color::Reset,
            rule: Color::Reset,
            branch: Color::Reset,
            selection: Style::new().add_modifier(Modifier::REVERSED),
            panel: Color::Reset,
            waiting: Color::Reset,
            working: Color::Reset,
            done: Color::Reset,
            running: Color::Reset,
            ended: Color::Reset,
            failed: Color::Reset,
            added: Color::Reset,
            removed: Color::Reset,
            // The `+` and `-` in front of a line say which it is; changed
            // words are underlined.
            added_line: Style::new(),
            removed_line: Style::new(),
            added_words: Style::new().add_modifier(Modifier::UNDERLINED),
            removed_words: Style::new().add_modifier(Modifier::UNDERLINED),
        }
    }

    /// Text on the theme's background: where everything starts.
    pub fn base(&self) -> Style {
        Style::new().fg(self.text).bg(self.background)
    }

    /// The color that says `status`.
    pub fn status(&self, status: Status) -> Color {
        match status {
            Status::Waiting => self.waiting,
            Status::Working => self.working,
            Status::Done => self.done,
            Status::Running => self.running,
            Status::Ended => self.ended,
            Status::Failed => self.failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_means_no_color_whatever_the_theme() {
        let theme = Theme::new(ThemeName::Dark, true);
        let colors = [
            theme.background,
            theme.text,
            theme.accent,
            theme.waiting,
            theme.failed,
        ];
        assert!(colors.iter().all(|color| *color == Color::Reset));
        assert!(theme.selection.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn the_painted_themes_paint_and_the_terminal_one_doesnt() {
        assert_ne!(Theme::new(ThemeName::Dark, false).background, Color::Reset);
        assert_ne!(Theme::new(ThemeName::Light, false).background, Color::Reset);
        assert_eq!(
            Theme::new(ThemeName::Terminal, false).background,
            Color::Reset
        );
    }

    #[test]
    fn each_status_has_a_color_of_its_own() {
        let theme = Theme::new(ThemeName::Dark, false);
        let statuses = [
            Status::Waiting,
            Status::Working,
            Status::Done,
            Status::Running,
            Status::Failed,
        ];
        let colors: std::collections::HashSet<String> = statuses
            .iter()
            .map(|status| format!("{:?}", theme.status(*status)))
            .collect();
        assert_eq!(colors.len(), statuses.len());
    }
}
