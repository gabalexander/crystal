//! The TUI's colors, each named for what it's for. The drawing code asks for
//! "muted" or "waiting", never for a color, so a theme can change every one
//! of them in one place.
//!
//! crystal's own `dark` and `light` paint their own background, so they look
//! the same in any terminal. `terminal` paints nothing and uses the
//! terminal's own sixteen colors. The rest are well-known schemes, each a
//! palette of a few colors that [`Theme::from_palette`] gives their roles,
//! blending the tints it needs toward the background. The user's `[colors]`
//! go over whichever it is. With `NO_COLOR` set there's no color at all, as
//! that convention asks: only bold and reversed text tell things apart.

use super::status::Status;
use crate::config::{ColorToken, ColorValue, Config, ThemeName};
use crate::markdown::{Ink, Mark};
use crate::syntax::TokenKind;
use ratatui::style::{Color, Modifier, Style};
use std::collections::BTreeMap;

/// A theme crystal has: the name the config file gives it, the other names
/// it answers to, and its colors.
pub struct Named {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    colors: Colors,
}

enum Colors {
    /// One of crystal's own, every color picked by hand.
    Own(fn() -> Theme),
    /// A scheme's palette, given its roles by [`Theme::from_palette`].
    Palette(Palette),
}

/// A color as red, green and blue.
type Rgb = (u8, u8, u8);

/// A scheme's few colors: its background and text, a dimmer text, its
/// accent, and the hues the statuses, the diff and code are drawn in.
struct Palette {
    background: Rgb,
    text: Rgb,
    muted: Rgb,
    accent: Rgb,
    red: Rgb,
    green: Rgb,
    yellow: Rgb,
    blue: Rgb,
    cyan: Rgb,
    orange: Rgb,
}

const fn own(name: &'static str, theme: fn() -> Theme) -> Named {
    Named {
        name,
        aliases: &[],
        colors: Colors::Own(theme),
    }
}

const fn scheme(name: &'static str, aliases: &'static [&'static str], palette: Palette) -> Named {
    Named {
        name,
        aliases,
        colors: Colors::Palette(palette),
    }
}

/// Every theme, in the order the settings view goes through them: crystal's
/// own, then each scheme's dark side before its light one. The schemes'
/// colors are herdr's (Apache-2.0), with a few changes: the dimmer text is
/// light (or dark) enough to read on the background, where herdr's often
/// isn't (nord's was 1.7:1); solarized-light's text is a shade darker for
/// the same reason; kanagawa's red and green are its wave red and spring
/// green; rose pine's blue is its foam, so a session at work doesn't look
/// done; and vesper's background is its own `#101010`.
pub const THEMES: &[Named] = &[
    own("dark", Theme::dark),
    own("light", Theme::light),
    own("terminal", Theme::terminal),
    scheme(
        "catppuccin",
        &["catppuccin-mocha", "mocha"],
        Palette {
            background: (30, 30, 46),
            text: (205, 214, 244),
            muted: (127, 132, 156),
            accent: (137, 180, 250),
            red: (243, 139, 168),
            green: (166, 227, 161),
            yellow: (249, 226, 175),
            blue: (137, 180, 250),
            cyan: (148, 226, 213),
            orange: (250, 179, 135),
        },
    ),
    scheme(
        "catppuccin-latte",
        &["latte"],
        Palette {
            background: (239, 241, 245),
            text: (76, 79, 105),
            muted: (124, 127, 147),
            accent: (30, 102, 245),
            red: (210, 15, 57),
            green: (64, 160, 43),
            yellow: (223, 142, 29),
            blue: (30, 102, 245),
            cyan: (23, 146, 153),
            orange: (254, 100, 11),
        },
    ),
    scheme(
        "tokyo-night",
        &["tokyonight"],
        Palette {
            background: (26, 27, 38),
            text: (192, 202, 245),
            muted: (105, 113, 150),
            accent: (122, 162, 247),
            red: (247, 118, 142),
            green: (158, 206, 106),
            yellow: (224, 175, 104),
            blue: (122, 162, 247),
            cyan: (125, 207, 255),
            orange: (255, 158, 100),
        },
    ),
    scheme(
        "tokyo-night-day",
        &["tokyo-day", "tokyonight-day"],
        Palette {
            background: (225, 226, 231),
            text: (55, 96, 191),
            muted: (104, 112, 154),
            accent: (46, 125, 233),
            red: (245, 42, 101),
            green: (88, 117, 57),
            yellow: (140, 108, 62),
            blue: (46, 125, 233),
            cyan: (17, 140, 116),
            orange: (177, 92, 0),
        },
    ),
    scheme(
        "dracula",
        &[],
        Palette {
            background: (40, 42, 54),
            text: (248, 248, 242),
            muted: (130, 140, 180),
            accent: (189, 147, 249),
            red: (255, 85, 85),
            green: (80, 250, 123),
            yellow: (241, 250, 140),
            blue: (139, 233, 253),
            cyan: (139, 233, 253),
            orange: (255, 184, 108),
        },
    ),
    scheme(
        "nord",
        &[],
        Palette {
            background: (46, 52, 64),
            text: (236, 239, 244),
            muted: (126, 136, 156),
            accent: (136, 192, 208),
            red: (191, 97, 106),
            green: (163, 190, 140),
            yellow: (235, 203, 139),
            blue: (129, 161, 193),
            cyan: (143, 188, 187),
            orange: (208, 135, 112),
        },
    ),
    scheme(
        "gruvbox",
        &["gruvbox-dark"],
        Palette {
            background: (40, 40, 40),
            text: (235, 219, 178),
            muted: (146, 131, 116),
            accent: (215, 153, 33),
            red: (251, 73, 52),
            green: (184, 187, 38),
            yellow: (250, 189, 47),
            blue: (131, 165, 152),
            cyan: (142, 192, 124),
            orange: (254, 128, 25),
        },
    ),
    scheme(
        "gruvbox-light",
        &[],
        Palette {
            background: (251, 241, 199),
            text: (60, 56, 54),
            muted: (124, 111, 100),
            accent: (7, 102, 120),
            red: (157, 0, 6),
            green: (121, 116, 14),
            yellow: (181, 118, 20),
            blue: (7, 102, 120),
            cyan: (66, 123, 88),
            orange: (175, 58, 3),
        },
    ),
    scheme(
        "one-dark",
        &["onedark"],
        Palette {
            background: (40, 44, 52),
            text: (171, 178, 191),
            muted: (115, 122, 135),
            accent: (97, 175, 239),
            red: (224, 108, 117),
            green: (152, 195, 121),
            yellow: (229, 192, 123),
            blue: (97, 175, 239),
            cyan: (86, 182, 194),
            orange: (209, 154, 102),
        },
    ),
    scheme(
        "one-light",
        &["onelight"],
        Palette {
            background: (250, 250, 250),
            text: (56, 58, 66),
            muted: (128, 130, 140),
            accent: (64, 120, 242),
            red: (228, 86, 73),
            green: (80, 161, 79),
            yellow: (193, 132, 1),
            blue: (64, 120, 242),
            cyan: (1, 132, 188),
            orange: (152, 104, 1),
        },
    ),
    scheme(
        "solarized",
        &["solarized-dark"],
        Palette {
            background: (0, 43, 54),
            text: (147, 161, 161),
            muted: (101, 123, 131),
            accent: (38, 139, 210),
            red: (220, 50, 47),
            green: (133, 153, 0),
            yellow: (181, 137, 0),
            blue: (38, 139, 210),
            cyan: (42, 161, 152),
            orange: (203, 75, 22),
        },
    ),
    scheme(
        "solarized-light",
        &[],
        Palette {
            background: (253, 246, 227),
            text: (88, 110, 117),
            muted: (131, 148, 150),
            accent: (38, 139, 210),
            red: (220, 50, 47),
            green: (133, 153, 0),
            yellow: (181, 137, 0),
            blue: (38, 139, 210),
            cyan: (42, 161, 152),
            orange: (203, 75, 22),
        },
    ),
    scheme(
        "kanagawa",
        &[],
        Palette {
            background: (31, 31, 40),
            text: (220, 215, 186),
            muted: (135, 134, 125),
            accent: (126, 156, 216),
            red: (228, 104, 118),
            green: (152, 187, 108),
            yellow: (192, 163, 110),
            blue: (126, 156, 216),
            cyan: (127, 180, 202),
            orange: (255, 160, 102),
        },
    ),
    scheme(
        "kanagawa-lotus",
        &["lotus"],
        Palette {
            background: (242, 236, 188),
            text: (84, 84, 100),
            muted: (128, 127, 118),
            accent: (77, 105, 155),
            red: (200, 64, 83),
            green: (111, 137, 78),
            yellow: (119, 113, 63),
            blue: (77, 105, 155),
            cyan: (78, 140, 162),
            orange: (204, 109, 0),
        },
    ),
    scheme(
        "rose-pine",
        &["rosepine"],
        Palette {
            background: (25, 23, 36),
            text: (224, 222, 244),
            muted: (144, 140, 170),
            accent: (196, 167, 231),
            red: (235, 111, 146),
            green: (49, 116, 143),
            yellow: (246, 193, 119),
            blue: (156, 207, 216),
            cyan: (156, 207, 216),
            orange: (234, 154, 151),
        },
    ),
    scheme(
        "rose-pine-dawn",
        &["rosepine-dawn", "dawn"],
        Palette {
            background: (250, 244, 237),
            text: (70, 66, 97),
            muted: (121, 117, 147),
            accent: (144, 122, 169),
            red: (180, 99, 122),
            green: (40, 105, 131),
            yellow: (234, 157, 52),
            blue: (86, 148, 159),
            cyan: (86, 148, 159),
            orange: (215, 130, 126),
        },
    ),
    scheme(
        "vesper",
        &[],
        Palette {
            background: (16, 16, 16),
            text: (255, 255, 255),
            muted: (139, 139, 139),
            accent: (255, 199, 153),
            red: (255, 128, 128),
            green: (153, 255, 228),
            yellow: (255, 199, 153),
            blue: (176, 176, 176),
            cyan: (102, 221, 204),
            orange: (255, 199, 153),
        },
    ),
];

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
    /// Text selected in a pane, to copy.
    pub copy_selection: Style,
    /// What a search through a pane's history found.
    pub found: Style,
    /// The match copy mode's cursor is on.
    pub found_current: Style,
    /// A keyword in highlighted code, and code in a line of prose.
    pub keyword: Style,
    /// A string in highlighted code.
    pub string: Style,
    /// A number in highlighted code.
    pub number: Style,
    /// Behind a block of code on a markdown page: a surface of its own,
    /// where a theme paints.
    pub code_block: Style,
}

impl Theme {
    /// The theme called `name`, or none at all when `no_color` is set.
    pub fn new(name: ThemeName, no_color: bool) -> Theme {
        if no_color {
            return Theme::plain();
        }
        // A `ThemeName` is only ever made from a name in `THEMES`.
        let named = THEMES.iter().find(|theme| theme.name == name.name());
        match named.map(|theme| &theme.colors) {
            Some(Colors::Own(theme)) => theme(),
            Some(Colors::Palette(palette)) => Theme::from_palette(palette),
            None => Theme::dark(),
        }
    }

    /// The theme the user asked for in their config, with their colors
    /// over it, unless the environment asks for no color.
    pub fn from_config(config: &Config) -> Theme {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        Theme::configured(config, no_color)
    }

    /// The config's theme and colors, or none at all when `no_color` is
    /// set: no color the user picked outweighs the environment's asking for
    /// none.
    fn configured(config: &Config, no_color: bool) -> Theme {
        if no_color {
            return Theme::plain();
        }
        Theme::new(config.theme, false).with_colors(&config.colors)
    }

    /// The theme with `colors`, the user's `[colors]`, over its own. A
    /// color for what's drawn with a background is that background, and
    /// takes the place of text reversed where a theme can't paint one.
    pub fn with_colors(mut self, colors: &BTreeMap<ColorToken, ColorValue>) -> Theme {
        let behind =
            |style: Style, color: Color| style.remove_modifier(Modifier::REVERSED).bg(color);
        for (&token, &ColorValue(color)) in colors {
            match token {
                ColorToken::Background => self.background = color,
                ColorToken::Text => self.text = color,
                ColorToken::Muted => self.muted = color,
                ColorToken::Accent => self.accent = color,
                ColorToken::Rule => self.rule = color,
                ColorToken::Branch => self.branch = color,
                ColorToken::Selection => self.selection = behind(self.selection, color),
                ColorToken::Panel => self.panel = color,
                ColorToken::Waiting => self.waiting = color,
                ColorToken::Working => self.working = color,
                ColorToken::Done => self.done = color,
                ColorToken::Running => self.running = color,
                ColorToken::Ended => self.ended = color,
                ColorToken::Failed => self.failed = color,
                ColorToken::Added => self.added = color,
                ColorToken::Removed => self.removed = color,
                ColorToken::AddedLine => self.added_line = behind(self.added_line, color),
                ColorToken::RemovedLine => self.removed_line = behind(self.removed_line, color),
                ColorToken::AddedWords => self.added_words = behind(self.added_words, color),
                ColorToken::RemovedWords => {
                    self.removed_words = behind(self.removed_words, color);
                }
                ColorToken::CopySelection => {
                    self.copy_selection = behind(self.copy_selection, color);
                }
                ColorToken::Found => self.found = behind(self.found, color),
                ColorToken::FoundCurrent => self.found_current = behind(self.found_current, color),
                ColorToken::Keyword => self.keyword = self.keyword.fg(color),
                ColorToken::String => self.string = self.string.fg(color),
                ColorToken::Number => self.number = self.number.fg(color),
                ColorToken::CodeBlock => self.code_block = behind(self.code_block, color),
            }
        }
        self
    }

    /// A scheme's palette with its colors given their roles. The tints
    /// behind a diff's lines, a search's finds, the selection and code are
    /// each a hue blended toward the background, so they suit a light
    /// background as well as a dark one.
    fn from_palette(palette: &Palette) -> Theme {
        let color = |(r, g, b): Rgb| Color::Rgb(r, g, b);
        let tint = |toward: Rgb, amount: f32| blend(palette.background, toward, amount);
        Theme {
            background: color(palette.background),
            text: color(palette.text),
            muted: color(palette.muted),
            accent: color(palette.accent),
            rule: tint(palette.muted, 0.35),
            branch: color(palette.cyan),
            selection: Style::new().bg(tint(palette.accent, 0.15)),
            panel: tint(palette.text, 0.06),
            waiting: color(palette.orange),
            working: color(palette.blue),
            done: color(palette.green),
            // A program at work is calmer than an agent done: the green,
            // toward the dimmer text.
            running: blend(palette.green, palette.muted, 0.4),
            ended: color(palette.muted),
            failed: color(palette.red),
            added: color(palette.green),
            removed: color(palette.red),
            added_line: Style::new().bg(tint(palette.green, 0.15)),
            removed_line: Style::new().bg(tint(palette.red, 0.15)),
            added_words: Style::new().bg(tint(palette.green, 0.35)),
            removed_words: Style::new().bg(tint(palette.red, 0.35)),
            copy_selection: Style::new().bg(tint(palette.accent, 0.35)),
            found: Style::new().bg(tint(palette.yellow, 0.3)),
            found_current: Style::new()
                .fg(color(palette.background))
                .bg(color(palette.orange)),
            keyword: Style::new().fg(color(palette.blue)),
            string: Style::new().fg(color(palette.green)),
            number: Style::new().fg(color(palette.orange)),
            code_block: Style::new().bg(tint(palette.text, 0.05)),
        }
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
            copy_selection: Style::new().bg(Color::Rgb(70, 62, 130)),
            found: Style::new().bg(Color::Rgb(92, 72, 30)),
            found_current: Style::new()
                .fg(Color::Rgb(16, 18, 24))
                .bg(Color::Rgb(255, 164, 84)),
            keyword: Style::new().fg(Color::Rgb(122, 162, 247)),
            string: Style::new().fg(Color::Rgb(158, 206, 106)),
            number: Style::new().fg(Color::Rgb(255, 158, 100)),
            code_block: Style::new().bg(Color::Rgb(23, 26, 35)),
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
            copy_selection: Style::new().bg(Color::Rgb(206, 198, 248)),
            found: Style::new().bg(Color::Rgb(250, 226, 160)),
            found_current: Style::new()
                .fg(Color::Rgb(250, 249, 246))
                .bg(Color::Rgb(196, 98, 16)),
            keyword: Style::new().fg(Color::Rgb(46, 92, 196)),
            string: Style::new().fg(Color::Rgb(40, 120, 52)),
            number: Style::new().fg(Color::Rgb(176, 90, 20)),
            code_block: Style::new().bg(Color::Rgb(241, 239, 234)),
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
            copy_selection: Style::new().add_modifier(Modifier::REVERSED),
            found: Style::new().fg(Color::Black).bg(Color::Yellow),
            found_current: Style::new().fg(Color::Black).bg(Color::LightRed),
            keyword: Style::new().fg(Color::Blue),
            string: Style::new().fg(Color::Green),
            number: Style::new().fg(Color::Yellow),
            code_block: Style::new(),
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
            // Reversed is the selection; what a search found is underlined,
            // and the match the cursor is on bold as well.
            copy_selection: Style::new().add_modifier(Modifier::REVERSED),
            found: Style::new().add_modifier(Modifier::UNDERLINED),
            found_current: Style::new().add_modifier(Modifier::UNDERLINED.union(Modifier::BOLD)),
            // Keywords are bold; the rest of code is as it's written.
            keyword: Style::new().add_modifier(Modifier::BOLD),
            string: Style::new(),
            number: Style::new(),
            code_block: Style::new(),
        }
    }

    /// Text on the theme's background: where everything starts.
    pub fn base(&self) -> Style {
        Style::new().fg(self.text).bg(self.background)
    }

    /// How a run of highlighted code that's `kind` is drawn.
    pub fn token(&self, kind: TokenKind) -> Style {
        match kind {
            TokenKind::Keyword => self.keyword,
            TokenKind::String => self.string,
            TokenKind::Number => self.number,
            TokenKind::Comment => Style::new().fg(self.muted),
            TokenKind::Text => Style::new().fg(self.text),
        }
    }

    /// How a piece of a markdown page marked `mark` is drawn.
    pub fn mark(&self, mark: Mark) -> Style {
        let style = match mark.ink {
            Ink::Text => Style::new().fg(self.text),
            Ink::Muted => Style::new().fg(self.muted),
            Ink::Accent => Style::new().fg(self.accent),
            Ink::Rule => Style::new().fg(self.rule),
            Ink::Code => self.keyword,
            Ink::Done => Style::new().fg(self.done),
            Ink::Warning => Style::new().fg(self.waiting),
            Ink::Failed => Style::new().fg(self.failed),
            Ink::Token(kind) => self.token(kind),
        };
        let style = style.add_modifier(mark.modifier);
        if mark.on_code {
            style.patch(self.code_block)
        } else {
            style
        }
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
            Status::Starting => self.muted,
        }
    }
}

/// `from` moved `amount` of the way to `to`, 0 to 1.
fn blend(from: Rgb, to: Rgb, amount: f32) -> Color {
    let channel = |from: u8, to: u8| {
        let (from, to) = (f32::from(from), f32::from(to));
        // Between two channels, so within 0 to 255.
        (from + (to - from) * amount).round() as u8
    };
    Color::Rgb(
        channel(from.0, to.0),
        channel(from.1, to.1),
        channel(from.2, to.2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every theme, by its name.
    fn every_theme() -> Vec<(&'static str, Theme)> {
        ThemeName::all()
            .map(|name| (name.name(), Theme::new(name, false)))
            .collect()
    }

    /// How far apart two colors are in lightness, as WCAG measures
    /// contrast: 1 for none, 21 for black on white.
    fn contrast(a: Color, b: Color) -> f64 {
        let luminance = |color: Color| {
            let Color::Rgb(r, g, b) = color else {
                panic!("{color:?} isn't painted");
            };
            let linear = |channel: u8| {
                let channel = f64::from(channel) / 255.0;
                if channel <= 0.04045 {
                    channel / 12.92
                } else {
                    ((channel + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
        };
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn every_theme_has_a_name_of_its_own_and_none_answers_to_anothers() {
        let mut names: Vec<&str> = THEMES
            .iter()
            .flat_map(|theme| std::iter::once(theme.name).chain(theme.aliases.iter().copied()))
            .collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "a name is taken twice");
        for name in names {
            // Each is written as `ThemeName::find` looks for it.
            assert_eq!(name, name.to_lowercase().replace([' ', '_'], "-"));
        }
    }

    #[test]
    fn every_painted_theme_can_be_read_on_its_background() {
        for (name, theme) in every_theme() {
            if theme.background == Color::Reset {
                continue;
            }
            let text = contrast(theme.text, theme.background);
            assert!(text >= 4.5, "{name}: text is {text:.2} to its background");
            let muted = contrast(theme.muted, theme.background);
            assert!(
                muted >= 2.8,
                "{name}: muted is {muted:.2} to its background"
            );
            for status in [
                Status::Waiting,
                Status::Working,
                Status::Done,
                Status::Failed,
            ] {
                let color = contrast(theme.status(status), theme.background);
                assert!(color >= 1.8, "{name}: {status:?} is {color:.2}");
            }
        }
    }

    #[test]
    fn a_palette_is_given_its_roles() {
        let theme = Theme::new(ThemeName::find("catppuccin").unwrap(), false);
        assert_eq!(theme.background, Color::Rgb(30, 30, 46));
        assert_eq!(theme.working, Color::Rgb(137, 180, 250));
        assert_eq!(theme.waiting, Color::Rgb(250, 179, 135));
        // A tint is between the background and its hue.
        let Some(Color::Rgb(r, g, b)) = theme.added_line.bg else {
            panic!("an added line is painted");
        };
        assert!(r > 30 && r < 166 && g > 30 && g < 227 && b > 46 && b < 161);
        // And on a light background, darker than it.
        let latte = Theme::new(ThemeName::find("latte").unwrap(), false);
        let Some(Color::Rgb(r, g, b)) = latte.selection.bg else {
            panic!("the selection is painted");
        };
        assert!(r < 239 && g < 241 && b <= 245);
    }

    #[test]
    fn blending_goes_from_one_color_to_the_other() {
        assert_eq!(blend((0, 0, 0), (200, 100, 50), 0.0), Color::Rgb(0, 0, 0));
        assert_eq!(
            blend((0, 0, 0), (200, 100, 50), 0.5),
            Color::Rgb(100, 50, 25)
        );
        assert_eq!(blend((250, 250, 250), (0, 0, 0), 1.0), Color::Rgb(0, 0, 0));
    }

    #[test]
    fn colors_of_ones_own_go_over_the_theme() {
        let config = crate::config::from_text(
            "theme = \"nord\"\n[colors]\naccent = \"#ff0000\"\nselection = \"blue\"\n\
             keyword = \"bright-green\"\nfound_current = \"#00ff00\"\n",
        )
        .unwrap();
        let theme = Theme::configured(&config, false);
        let nord = Theme::new(config.theme, false);
        assert_eq!(theme.accent, Color::Rgb(255, 0, 0));
        assert_eq!(theme.selection.bg, Some(Color::Blue));
        assert_eq!(theme.keyword.fg, Some(Color::LightGreen));
        assert_eq!(theme.found_current.bg, Some(Color::Rgb(0, 255, 0)));
        // What isn't given keeps the theme's: the text over the match too.
        assert_eq!(theme.found_current.fg, nord.found_current.fg);
        assert_eq!(theme.text, nord.text);
        assert_eq!(theme.background, nord.background);

        // Where the terminal theme reverses the selection, a color of
        // one's own is painted behind it instead.
        let mut config = config;
        config.theme = ThemeName::TERMINAL;
        let theme = Theme::configured(&config, false);
        assert_eq!(theme.selection.bg, Some(Color::Blue));
        assert!(theme.selection.sub_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn no_color_outweighs_colors_of_ones_own() {
        let config = crate::config::from_text(
            "theme = \"dracula\"\n[colors]\naccent = \"#ff0000\"\nbackground = \"#000000\"\n",
        )
        .unwrap();
        let theme = Theme::configured(&config, true);
        assert_eq!(theme.accent, Color::Reset);
        assert_eq!(theme.background, Color::Reset);
    }

    #[test]
    fn no_color_means_no_color_whatever_the_theme() {
        let theme = Theme::new(ThemeName::DARK, true);
        let colors = [
            theme.background,
            theme.text,
            theme.accent,
            theme.waiting,
            theme.failed,
        ];
        assert!(colors.iter().all(|color| *color == Color::Reset));
        assert!(theme.selection.add_modifier.contains(Modifier::REVERSED));
        let code = [theme.keyword, theme.string, theme.number, theme.code_block];
        assert!(
            code.iter()
                .all(|style| style.fg.is_none() && style.bg.is_none())
        );
        assert!(theme.keyword.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn every_theme_but_the_terminal_one_paints() {
        for (name, theme) in every_theme() {
            let painted = theme.background != Color::Reset;
            assert_eq!(painted, name != "terminal", "{name}");
        }
    }

    #[test]
    fn in_every_theme_each_status_has_a_color_of_its_own() {
        let statuses = [
            Status::Waiting,
            Status::Working,
            Status::Done,
            Status::Running,
            Status::Failed,
        ];
        for (name, theme) in every_theme() {
            let colors: std::collections::HashSet<String> = statuses
                .iter()
                .map(|status| format!("{:?}", theme.status(*status)))
                .collect();
            assert_eq!(colors.len(), statuses.len(), "{name}");
        }
    }
}
