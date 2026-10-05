//! The sidebar's rows as `[sidebar]` in the config lays them out: `rows`, a
//! session's lines, each a list of what goes on it; `rows_by_agent`, the
//! same for one agent's sessions; and `worktree_row` and `project_row`, a
//! worktree's line and a project's heading. What goes on a line is a token:
//! one of crystal's, like `name` or `when`, or `$name`, a value an agent or
//! a script reported with `crystal report --token` (a session's) or `crystal
//! project report --token` (a project's). A token is written alone, or in a
//! table that styles it and gives it rules: the first rule its value
//! matches styles it, or hides it. Adapted from herdr's
//! `ui.sidebar.agents.rows`.
//!
//! Read and checked here, and a line laid out from the values its tokens
//! have, in a width: a token with no value is left out with the separator
//! before it, and a line with nothing on it goes. What each token's value
//! is, the sidebar says. Nothing laid out, the sidebar draws its own rows.

use super::theme::Theme;
use crate::config::{ColorValue, SidebarSettings};
use crate::report;
use anyhow::{Result, bail, ensure};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use unicode_width::UnicodeWidthStr;

/// What a line's tokens are about: each kind has tokens of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Session,
    Worktree,
    Project,
}

/// The token that sends what follows it on a line to the right edge.
pub const GAP: &str = "gap";

/// Every token a session's lines take, and what each says.
pub const SESSION_TOKENS: &[(&str, &str)] = &[
    ("mark", "its status's mark, in its color"),
    ("name", "its name"),
    ("title", "the title reported for it, or else its name"),
    (
        "agent",
        "what's in front in it, in a word, or the agent reported",
    ),
    ("model", "the model its agent runs on"),
    ("subagents", "how many subagents its agent has running, +2"),
    (
        "state",
        "its status in a word, or the label reported for it",
    ),
    (
        "when",
        "how long ago it changed, or the tab it's in when shown from another",
    ),
    ("bell", "♪ while its bell rang out of sight"),
    (
        "task",
        "its task: what it was asked to do, or how that went",
    ),
    ("line", "the line reported for it"),
    ("branch", "its worktree's branch"),
    ("project", "its project's name"),
    ("tab", "the tab it's in"),
    ("context", "how full a background task's conversation is"),
    (GAP, "what follows goes at the right edge"),
];

/// Every token a worktree's line takes.
pub const WORKTREE_TOKENS: &[(&str, &str)] = &[
    ("mark", "⌂ for the main worktree, ⎇ for a linked one"),
    (
        "name",
        "its label, its branch, or claude for one Claude Code made",
    ),
    (
        "about",
        "what's said after its name: its branch, what git is doing",
    ),
    ("branch", "its branch"),
    ("label", "the label it was given"),
    ("doing", "what git is in the middle of there, like rebasing"),
    ("changes", "its changes not committed, +3 ±42"),
    ("upstream", "how far it is from its upstream, ↑2 ↓1"),
    ("pull_request", "its pull request and how it stands, #57 ✓"),
    ("path", "its directory"),
    ("project", "its project's name"),
    (GAP, "what follows goes at the right edge"),
];

/// Every token a project's heading takes.
pub const PROJECT_TOKENS: &[(&str, &str)] = &[
    ("name", "its name"),
    ("to_do", "how many backlog items are still to do, 3 to do"),
    ("path", "its main worktree's directory"),
    (GAP, "the rule goes between what's before and what follows"),
];

/// The tokens whose look is fixed: a table can style them, but they take
/// no rules.
const FIXED: &[&str] = &["mark", "bell", "changes", "upstream", "pull_request"];

/// The most lines a layout has, tokens on a line and rules for a token.
const MOST: usize = 16;

/// What goes in one place on a line: a token, alone or styled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    untagged,
    expecting = "a token, like \"name\", or a table with one, like { token = \"$load\", fg = \"red\" }"
)]
pub enum Piece {
    Token(String),
    Styled(Styled),
}

/// A token with a style of its own, over the one it has, and rules: what
/// the style says changes, what it doesn't stays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Styled {
    pub token: String,
    /// Its color: one of the theme's, by what it's for (`text`, `muted`,
    /// `accent`, `waiting`, …), or a color as `[colors]` takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<Paint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dim: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    /// The first that matches the token's value styles it, or hides it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,
}

/// A style over a token's own: what's said changes, what isn't stays.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Look {
    fg: Option<Paint>,
    bold: Option<bool>,
    dim: Option<bool>,
    italic: Option<bool>,
}

/// A rule on a token's value: one condition, and the style it gives the
/// token when it holds, or `hide`, to leave it out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_with: Option<String>,
    /// The value, read whole as a number, is greater.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gt: Option<f64>,
    /// The value, read whole as a number, is less.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lt: Option<f64>,
    /// `equals`, `contains` and `starts_with` match whatever the case of
    /// the letters A to Z.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ignore_case: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<Paint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dim: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hide: bool,
}

/// A color a token is painted: the theme's for what it's for, which
/// follows the theme, or one of the user's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(into = "String")]
pub enum Paint {
    Theme(&'static str),
    Own(Color),
}

/// The theme's colors a token can be painted, by what they're for.
const THEME_COLORS: [&str; 13] = [
    "text", "muted", "accent", "rule", "branch", "waiting", "working", "done", "running", "ended",
    "failed", "added", "removed",
];

impl Paint {
    pub fn color(self, theme: &Theme) -> Color {
        match self {
            Paint::Own(color) => color,
            Paint::Theme(name) => match name {
                "text" => theme.text,
                "muted" => theme.muted,
                "accent" => theme.accent,
                "rule" => theme.rule,
                "branch" => theme.branch,
                "waiting" => theme.waiting,
                "working" => theme.working,
                "done" => theme.done,
                "running" => theme.running,
                "ended" => theme.ended,
                "failed" => theme.failed,
                "added" => theme.added,
                _ => theme.removed,
            },
        }
    }
}

impl TryFrom<String> for Paint {
    type Error = String;

    fn try_from(text: String) -> Result<Paint, String> {
        let name = text.trim().to_lowercase();
        if let Some(name) = THEME_COLORS.iter().find(|known| **known == name) {
            return Ok(Paint::Theme(name));
        }
        ColorValue::try_from(text)
            .map(|ColorValue(color)| Paint::Own(color))
            .map_err(|err| format!("{err}, or one of the theme's: {}", THEME_COLORS.join(", ")))
    }
}

impl<'de> Deserialize<'de> for Paint {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Paint, D::Error> {
        let text = String::deserialize(deserializer)?;
        Paint::try_from(text).map_err(serde::de::Error::custom)
    }
}

impl From<Paint> for String {
    fn from(paint: Paint) -> String {
        match paint {
            Paint::Theme(name) => name.to_string(),
            Paint::Own(color) => ColorValue(color).into(),
        }
    }
}

impl Look {
    /// `style` with this over it.
    fn over(&self, style: Style, theme: &Theme) -> Style {
        let mut style = style;
        if let Some(paint) = self.fg {
            style = style.fg(paint.color(theme));
        }
        for (on, modifier) in [
            (self.bold, Modifier::BOLD),
            (self.dim, Modifier::DIM),
            (self.italic, Modifier::ITALIC),
        ] {
            style = match on {
                Some(true) => style.add_modifier(modifier),
                Some(false) => style.remove_modifier(modifier),
                None => style,
            };
        }
        style
    }

    fn is_empty(&self) -> bool {
        *self == Look::default()
    }
}

impl Styled {
    fn look(&self) -> Look {
        Look {
            fg: self.fg,
            bold: self.bold,
            dim: self.dim,
            italic: self.italic,
        }
    }
}

impl Rule {
    fn look(&self) -> Look {
        Look {
            fg: self.fg,
            bold: self.bold,
            dim: self.dim,
            italic: self.italic,
        }
    }

    /// Whether the rule holds for `value`.
    fn matches(&self, value: &str) -> bool {
        let text = |wanted: &str, test: fn(&str, &str) -> bool| {
            if self.ignore_case {
                test(&value.to_ascii_lowercase(), &wanted.to_ascii_lowercase())
            } else {
                test(value, wanted)
            }
        };
        if let Some(wanted) = &self.equals {
            return text(wanted, |value, wanted| value == wanted);
        }
        if let Some(wanted) = &self.contains {
            return text(wanted, |value, wanted| value.contains(wanted));
        }
        if let Some(wanted) = &self.starts_with {
            return text(wanted, |value, wanted| value.starts_with(wanted));
        }
        let number = value.parse::<f64>().ok().filter(|n| n.is_finite());
        match (self.gt, self.lt, number) {
            (Some(gt), _, Some(number)) => number > gt,
            (_, Some(lt), Some(number)) => number < lt,
            _ => false,
        }
    }

    fn check(&self) -> Result<()> {
        let texts = [&self.equals, &self.contains, &self.starts_with];
        let texts = texts.iter().filter(|said| said.is_some()).count();
        let numbers = [self.gt, self.lt]
            .iter()
            .filter(|said| said.is_some())
            .count();
        ensure!(
            texts + numbers == 1,
            "a rule has one condition: equals, contains, starts_with, gt or lt"
        );
        ensure!(
            !(self.ignore_case && numbers == 1),
            "ignore_case goes with equals, contains and starts_with, not with gt or lt"
        );
        let finite = [self.gt, self.lt].into_iter().flatten().all(f64::is_finite);
        ensure!(finite, "gt and lt take a finite number");
        Ok(())
    }
}

impl Piece {
    /// The token it shows.
    pub fn token(&self) -> &str {
        match self {
            Piece::Token(token) => token,
            Piece::Styled(styled) => &styled.token,
        }
    }

    /// The token's value styled as the piece and its rules say, from the
    /// style it has: `None` when a rule hides it.
    fn style(&self, value: &str, style: Style, theme: &Theme) -> Option<Style> {
        let Piece::Styled(styled) = self else {
            return Some(style);
        };
        let style = styled.look().over(style, theme);
        match styled.rules.iter().find(|rule| rule.matches(value)) {
            Some(rule) if rule.hide => None,
            Some(rule) => Some(rule.look().over(style, theme)),
            None => Some(style),
        }
    }

    fn hidden(&self, value: &str) -> bool {
        let Piece::Styled(styled) = self else {
            return false;
        };
        styled
            .rules
            .iter()
            .find(|rule| rule.matches(value))
            .is_some_and(|rule| rule.hide)
    }

    fn check(&self, kind: Kind) -> Result<()> {
        let token = self.token();
        let known = match kind {
            Kind::Session => SESSION_TOKENS,
            Kind::Worktree => WORKTREE_TOKENS,
            Kind::Project => PROJECT_TOKENS,
        };
        match token.strip_prefix('$') {
            Some(name) => report::check_token(name)?,
            None if known.iter().any(|(name, _)| *name == token) => {}
            None => {
                let names: Vec<&str> = known.iter().map(|(name, _)| *name).collect();
                bail!(
                    "`{token}` isn't a token here: say one of {}, or `$name` for one reported",
                    names.join(", ")
                );
            }
        }
        let Piece::Styled(styled) = self else {
            return Ok(());
        };
        ensure!(
            token != GAP || (styled.look().is_empty() && styled.rules.is_empty()),
            "`gap` takes no style or rules"
        );
        ensure!(
            styled.rules.is_empty() || !FIXED.contains(&token),
            "`{token}` takes a style, but no rules"
        );
        ensure!(
            styled.rules.len() <= MOST,
            "`{token}` has more than {MOST} rules"
        );
        for rule in &styled.rules {
            rule.check()?;
        }
        Ok(())
    }
}

/// How `[sidebar]` lays the sidebar's rows out, where it does.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowLayouts {
    session: Vec<Vec<Piece>>,
    by_agent: BTreeMap<String, Vec<Vec<Piece>>>,
    worktree: Vec<Piece>,
    project: Vec<Piece>,
}

impl RowLayouts {
    pub fn of(settings: &SidebarSettings) -> RowLayouts {
        RowLayouts {
            session: settings.rows.clone(),
            by_agent: settings.rows_by_agent.clone(),
            worktree: settings.worktree_row.clone(),
            project: settings.project_row.clone(),
        }
    }

    /// The lines of a session with `agent` in front, by its program's
    /// name: its own, or else `rows`; `None` for crystal's own.
    pub fn session(&self, agent: Option<&str>) -> Option<&[Vec<Piece>]> {
        let own = agent.and_then(|agent| self.by_agent.get(agent));
        let lines = own.unwrap_or(&self.session);
        (!lines.is_empty()).then_some(lines.as_slice())
    }

    /// A worktree's line, or `None` for crystal's own.
    pub fn worktree(&self) -> Option<&[Piece]> {
        (!self.worktree.is_empty()).then_some(self.worktree.as_slice())
    }

    /// A project's heading, or `None` for crystal's own.
    pub fn project(&self) -> Option<&[Piece]> {
        (!self.project.is_empty()).then_some(self.project.as_slice())
    }
}

/// Checks a line of tokens of `kind`.
pub fn check_line(line: &[Piece], kind: Kind) -> Result<()> {
    ensure!(line.len() <= MOST, "a line has {MOST} tokens at most");
    ensure!(
        line.iter().filter(|piece| piece.token() == GAP).count() <= 1,
        "a line has one `gap` at most"
    );
    for piece in line {
        piece.check(kind)?;
    }
    Ok(())
}

/// Checks a session's lines, `rows` or one of `rows_by_agent`'s.
pub fn check_lines(lines: &[Vec<Piece>]) -> Result<()> {
    ensure!(lines.len() <= MOST, "a layout has {MOST} lines at most");
    for line in lines {
        check_line(line, Kind::Session)?;
    }
    Ok(())
}

/// Checks `rows_by_agent`'s agents, by their programs' names.
pub fn check_agents(by_agent: &BTreeMap<String, Vec<Vec<Piece>>>) -> Result<()> {
    for (agent, lines) in by_agent {
        let word = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_.".contains(c);
        ensure!(
            !agent.is_empty() && agent.chars().all(word),
            "an agent in rows_by_agent is its program's name, like claude or codex: not `{agent}`"
        );
        check_lines(lines).map_err(|err| anyhow::anyhow!("for {agent}, {err}"))?;
    }
    Ok(())
}

/// Whether `line` shows anything: one of its tokens, but `gap`, has a
/// value, as `text` says, that no rule hides.
pub fn shows(line: &[Piece], mut text: impl FnMut(&str) -> Option<String>) -> bool {
    line.iter()
        .filter(|piece| piece.token() != GAP)
        .any(|piece| text(piece.token()).is_some_and(|value| !piece.hidden(&value)))
}

/// A token's value on a line: the text its rules match, and the spans that
/// draw it, in the style it has unless the layout says otherwise.
#[derive(Debug, Clone)]
pub struct Value<'a> {
    pub text: String,
    pub spans: Vec<Span<'a>>,
}

impl<'a> Value<'a> {
    /// A value drawn as its text, in `style`.
    pub fn styled(text: impl Into<String>, style: Style) -> Value<'a> {
        let text = text.into();
        let spans = vec![Span::styled(text.clone(), style)];
        Value { text, spans }
    }
}

/// A line laid out: what goes from its left, and what goes at its right
/// edge, each already fitting the room it was laid out in, with room
/// between them.
#[derive(Debug, Clone, Default)]
pub struct Laid<'a> {
    pub left: Vec<Span<'a>>,
    pub right: Vec<Span<'a>>,
}

impl<'a> Laid<'a> {
    pub fn left_width(&self) -> usize {
        width_of(&self.left)
    }

    pub fn right_width(&self) -> usize {
        width_of(&self.right)
    }
}

/// Lays `line` out in `room` columns, each token's value as `value` says:
/// a token with no value, or that a rule hides, is left out with the
/// separator before it, ` · `, or after `mark` a space. What follows `gap`
/// goes at the right edge. Short of room, what's to the left of it is cut,
/// down to a few columns, before what's to the right of it goes. `None`
/// when nothing's left on it.
pub fn lay_out<'a>(
    line: &[Piece],
    theme: &Theme,
    room: usize,
    mut value: impl FnMut(&str) -> Option<Value<'a>>,
) -> Option<Laid<'a>> {
    let mut sides: [Vec<Span<'a>>; 2] = [Vec::new(), Vec::new()];
    let mut side = 0;
    let mut shown = false;
    // The token shown last on this side, for the separator before the
    // next.
    let mut last: Option<&str> = None;
    for piece in line {
        let token = piece.token();
        if token == GAP {
            side = 1;
            last = None;
            continue;
        }
        let Some(value) = value(token) else {
            continue;
        };
        let mut spans = Vec::new();
        for span in value.spans {
            let Some(style) = piece.style(&value.text, span.style, theme) else {
                spans.clear();
                break;
            };
            spans.push(Span::styled(span.content, style));
        }
        if spans.is_empty() || width_of(&spans) == 0 {
            continue;
        }
        shown = true;
        if let Some(before) = last {
            let separator = if before == "mark" { " " } else { " · " };
            sides[side].push(Span::styled(separator, Style::new().fg(theme.muted)));
        }
        sides[side].extend(spans);
        last = Some(token);
    }
    if !shown {
        return None;
    }
    let [left, right] = sides;
    let (left_width, right_width) = (width_of(&left), width_of(&right));
    let between = usize::from(left_width > 0 && right_width > 0);
    let laid = if left_width + between + right_width <= room {
        Laid { left, right }
    } else if right_width > 0 && right_width + between + left_width.min(6) <= room {
        let left = cut(left, room - right_width - between);
        Laid { left, right }
    } else {
        Laid {
            left: cut(left, room),
            right: Vec::new(),
        }
    };
    Some(laid)
}

fn width_of(spans: &[Span]) -> usize {
    spans.iter().map(|span| span.content.width()).sum()
}

/// `spans` cut down to `width` columns, ending in `…` where they're cut.
pub fn cut<'a>(spans: Vec<Span<'a>>, width: usize) -> Vec<Span<'a>> {
    if width_of(&spans) <= width {
        return spans;
    }
    let mut kept = Vec::new();
    let mut used = 0;
    for span in spans {
        let span_width = span.content.width();
        if used + span_width < width {
            used += span_width;
            kept.push(span);
            continue;
        }
        // This one is cut, leaving a column for the ellipsis.
        let mut text = String::new();
        for c in span.content.chars() {
            let c_width = c.to_string().width();
            if used + c_width + 1 > width {
                break;
            }
            used += c_width;
            text.push(c);
        }
        if width > 0 {
            text.push('…');
        }
        kept.push(Span::styled(text, span.style));
        break;
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeName;

    fn theme() -> Theme {
        Theme::new(ThemeName::DARK, false)
    }

    fn line(toml: &str) -> Vec<Piece> {
        #[derive(Deserialize)]
        struct Line {
            line: Vec<Piece>,
        }
        toml::from_str::<Line>(&format!("line = {toml}"))
            .unwrap()
            .line
    }

    fn text(laid: &Laid, room: usize) -> String {
        let left: String = laid.left.iter().map(|s| s.content.as_ref()).collect();
        let right: String = laid.right.iter().map(|s| s.content.as_ref()).collect();
        if right.is_empty() {
            return left;
        }
        let gap = room - left.width() - right.width();
        format!("{left}{}{right}", " ".repeat(gap))
    }

    fn values(token: &str) -> Option<Value<'static>> {
        let text = match token {
            "mark" => "▲",
            "name" => "fixer",
            "model" => "opus 5.5",
            "when" => "4m",
            "$load" => "90",
            _ => return None,
        };
        Some(Value::styled(text, Style::new()))
    }

    #[test]
    fn tokens_are_separated_and_those_with_nothing_left_out() {
        let theme = theme();
        let pieces = line(r#"["mark", "name", "agent", "model", "gap", "when"]"#);
        let laid = lay_out(&pieces, &theme, 24, values).unwrap();
        assert_eq!(text(&laid, 24), "▲ fixer · opus 5.5    4m");
        // A line with nothing on it goes.
        assert!(lay_out(&line(r#"["agent", "$summary"]"#), &theme, 24, values).is_none());
    }

    #[test]
    fn short_of_room_the_left_is_cut_before_the_right_goes() {
        let theme = theme();
        let pieces = line(r#"["mark", "name", "model", "gap", "when"]"#);
        let laid = lay_out(&pieces, &theme, 16, values).unwrap();
        assert_eq!(text(&laid, 16), "▲ fixer · op… 4m");
        let laid = lay_out(&pieces, &theme, 8, values).unwrap();
        assert_eq!(text(&laid, 8), "▲ fixer…");
    }

    #[test]
    fn the_first_rule_that_matches_styles_the_token_or_hides_it() {
        let theme = theme();
        let pieces = line(
            r##"[{ token = "$load", fg = "#ffffff", rules = [
                { gt = 80, fg = "failed", bold = true },
                { gt = 50, fg = "waiting" },
                { equals = "0", hide = true },
            ] }, "name"]"##,
        );
        let load = |value: &'static str| {
            move |token: &str| match token {
                "$load" => Some(Value::styled(value, Style::new())),
                _ => values(token),
            }
        };
        let first = |laid: Laid<'static>| laid.left[0].clone();
        let span = first(lay_out(&pieces, &theme, 30, load("90")).unwrap());
        assert_eq!(span.style.fg, Some(theme.failed));
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
        let span = first(lay_out(&pieces, &theme, 30, load("60")).unwrap());
        assert_eq!(span.style.fg, Some(theme.waiting));
        // Not a number, whole: no rule on numbers holds.
        let span = first(lay_out(&pieces, &theme, 30, load("90%")).unwrap());
        assert_eq!(span.style.fg, Some(Color::Rgb(255, 255, 255)));
        let laid = lay_out(&pieces, &theme, 30, load("0")).unwrap();
        assert_eq!(text(&laid, 30), "fixer");
        assert!(!shows(&pieces[..1], |_| Some("0".into())));
        assert!(shows(&pieces[..1], |_| Some("1".into())));
    }

    #[test]
    fn text_rules_match_the_case_they_re_told_to() {
        let rule = |toml: &str| -> Rule { toml::from_str(toml).unwrap() };
        assert!(rule("equals = \"Local\"").matches("Local"));
        assert!(!rule("equals = \"local\"").matches("Local"));
        assert!(rule("equals = \"local\"\nignore_case = true").matches("LOCAL"));
        assert!(rule("contains = \"\"").matches("anything"));
        assert!(rule("starts_with = \"ci\"").matches("ci:lint"));
        assert!(rule("lt = 10").matches("1e0"));
        assert!(!rule("lt = 10").matches(" 1"));
        assert!(!rule("gt = 10").matches("inf"));
    }

    #[test]
    fn layouts_are_checked() {
        let check = |toml: &str| check_line(&line(toml), Kind::Session);
        assert!(check(r#"["mark", "name", "gap", "when", "$load"]"#).is_ok());
        let unknown = check(r#"["nmae"]"#).unwrap_err().to_string();
        assert!(unknown.contains("`nmae` isn't a token here"), "{unknown}");
        assert!(check(r#"["$9"]"#).is_err());
        assert!(check(r#"["gap", "name", "gap"]"#).is_err());
        assert!(check(r#"[{ token = "mark", rules = [{ equals = "▲" }] }]"#).is_err());
        assert!(check(r#"[{ token = "gap", bold = true }]"#).is_err());
        assert!(check(r#"[{ token = "name", rules = [{ equals = "a", gt = 1 }] }]"#).is_err());
        assert!(
            check(r#"[{ token = "name", rules = [{ gt = 1, ignore_case = true }] }]"#).is_err()
        );
        assert!(check(r#"[{ token = "name", rules = [{}] }]"#).is_err());
        // Each kind has tokens of its own.
        assert!(check_line(&line(r#"["changes"]"#), Kind::Worktree).is_ok());
        assert!(check(r#"["changes"]"#).is_err());
        assert!(check_line(&line(r#"["name", "gap", "to_do"]"#), Kind::Project).is_ok());
        let agents = BTreeMap::from([("Claude Code".to_string(), vec![line(r#"["name"]"#)])]);
        assert!(check_agents(&agents).is_err());
    }

    #[test]
    fn a_style_paints_with_the_theme_s_colors_or_one_s_own() {
        let theme = theme();
        let paint = |text: &str| Paint::try_from(text.to_string());
        assert_eq!(paint("Muted").unwrap().color(&theme), theme.muted);
        assert_eq!(paint("#f00").unwrap(), Paint::Own(Color::Rgb(255, 0, 0)));
        assert_eq!(paint("bright-red").unwrap(), Paint::Own(Color::LightRed));
        assert!(paint("mauve").is_err());
        let look = Look {
            bold: Some(false),
            italic: Some(true),
            ..Look::default()
        };
        let style = look.over(Style::new().add_modifier(Modifier::BOLD), &theme);
        assert!(!style.add_modifier.contains(Modifier::BOLD));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn spans_are_cut_with_an_ellipsis() {
        let spans = vec![Span::raw("ab"), Span::raw("cdef")];
        let cut = cut(spans, 4);
        let text: String = cut.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abc…");
    }
}
