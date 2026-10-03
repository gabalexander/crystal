//! The profiles view, `P`: the profiles in the config file, to look over,
//! add to, change, copy and take out. A change goes to the file when it's
//! saved; the event loop writes it, and the rest of the file stays as the
//! user wrote it (see [`crate::profile`]).
//!
//! The view is state and logic only, apart from [`draw`] at the end.

use super::command_line;
use super::launcher::{cut, shown_choices};
use super::text_area::TextArea;
use super::text_input::TextInput;
use super::theme::Theme;
use crate::catalog::{self, Agent, Choices, Instructions};
use crate::profile::{Profile, StartIn};
use crate::shell;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

/// How wide the labels of the form's rows are, so the values line up.
const LABEL_WIDTH: usize = 14;

/// The most rows a prompt or instructions box shows before it scrolls.
const TEXT_ROWS: usize = 3;

/// What the form's "where" row offers, in its order: what a profile's
/// `where` can be, the first being "not set".
const PLACES: &[(&str, Option<StartIn>)] = &[
    ("as the panel is set", None),
    ("here", Some(StartIn::Here)),
    ("new worktree", Some(StartIn::Worktree)),
];

/// What a key in the view leads to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    /// Write `profile` to the config file, in place of the profile called
    /// `replacing`, or as a new one.
    Save {
        replacing: Option<String>,
        profile: Profile,
    },
    /// Take the profile with this name out of the config file.
    Delete(String),
}

pub struct ProfilesView {
    /// As the config file has them.
    profiles: Vec<Profile>,
    selected: usize,
    /// The profile being changed or made, while the form is open.
    form: Option<Form>,
    /// The profile to take out, until the user says yes or no.
    deleting: Option<String>,
    /// Why the last change wasn't saved.
    problem: Option<String>,
    codex_models: Vec<String>,
}

impl ProfilesView {
    pub fn new(profiles: Vec<Profile>, codex_models: Vec<String>) -> ProfilesView {
        ProfilesView {
            profiles,
            selected: 0,
            form: None,
            deleting: None,
            problem: None,
            codex_models,
        }
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn form(&self) -> Option<&Form> {
        self.form.as_ref()
    }

    pub fn deleting(&self) -> Option<&str> {
        self.deleting.as_deref()
    }

    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Takes the models Codex lets the user choose, for the form's rows.
    pub fn set_codex_models(&mut self, models: Vec<String>) {
        self.codex_models = models;
        if let Some(form) = &mut self.form {
            form.set_codex_models(&self.codex_models);
        }
    }

    /// The profiles as the file has them after a save: the form closes, and
    /// the bar goes to the profile called `select`.
    pub fn saved(&mut self, profiles: Vec<Profile>, select: Option<&str>) {
        self.profiles = profiles;
        self.form = None;
        self.problem = None;
        let found = select.and_then(|name| self.profiles.iter().position(|p| p.name == name));
        let last = self.profiles.len().saturating_sub(1);
        self.selected = found.unwrap_or(self.selected).min(last);
    }

    /// Why a save didn't happen, for the view to say. The form stays open,
    /// so the user can put it right.
    pub fn failed(&mut self, problem: String) {
        self.problem = Some(problem);
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        self.problem = None;
        if let Some(name) = self.deleting.take() {
            if key.code == KeyCode::Char('y') {
                return Outcome::Delete(name);
            }
            return Outcome::Stay;
        }
        if self.form.is_some() {
            return self.on_form_key(key);
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Enter | KeyCode::Char('e') => self.edit_selected(),
            KeyCode::Char('a') => self.add(),
            KeyCode::Char('c') => self.copy_selected(),
            KeyCode::Char('x') => self.deleting = self.current().map(|p| p.name.clone()),
            _ => {}
        }
        Outcome::Stay
    }

    /// A paste goes into the form's text field that has the keyboard.
    pub fn on_paste(&mut self, text: &str) {
        if let Some(form) = &mut self.form {
            form.paste(text);
        }
    }

    fn current(&self) -> Option<&Profile> {
        self.profiles.get(self.selected)
    }

    fn move_selection(&mut self, by: isize) {
        let last = self.profiles.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
    }

    fn edit_selected(&mut self) {
        if let Some(profile) = self.current() {
            let form = Form::new(Some(profile.name.clone()), profile, &self.codex_models);
            self.form = Some(form);
        }
    }

    /// A new profile, for the first agent crystal knows.
    fn add(&mut self) {
        let profile = Profile::for_agent(catalog::AGENTS[0].program);
        self.form = Some(Form::new(None, &profile, &self.codex_models));
    }

    /// A new profile like the selected one, under another name.
    fn copy_selected(&mut self) {
        let Some(profile) = self.current() else {
            return;
        };
        let copy = Profile {
            name: format!("{} copy", profile.name),
            ..profile.clone()
        };
        self.form = Some(Form::new(None, &copy, &self.codex_models));
    }

    fn on_form_key(&mut self, key: KeyEvent) -> Outcome {
        let Some(form) = &mut self.form else {
            return Outcome::Stay;
        };
        match form.on_key(&key) {
            FormKey::Stay => Outcome::Stay,
            FormKey::Back => {
                self.form = None;
                Outcome::Stay
            }
            FormKey::Save => match form.profile() {
                Ok(profile) => Outcome::Save {
                    replacing: form.replacing.clone(),
                    profile,
                },
                Err(problem) => {
                    self.problem = Some(problem);
                    Outcome::Stay
                }
            },
        }
    }
}

/// A row of the form, which Tab moves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Description,
    Agent,
    Model,
    Mode,
    Where,
    Args,
    Prompt,
    Instructions,
}

/// What a key in the form leads to.
#[derive(Debug, PartialEq, Eq)]
enum FormKey {
    Stay,
    /// Back to the list, without saving.
    Back,
    Save,
}

/// A profile being made or changed: a row for each thing it can say.
pub struct Form {
    /// The name the profile has in the file, which a save replaces; `None`
    /// for a new one.
    replacing: Option<String>,
    name: TextInput,
    description: TextInput,
    /// Which of the agents crystal knows, as an index into
    /// [`catalog::AGENTS`].
    agent: usize,
    /// The model row's choices, as `(shown, value)`, and the one chosen.
    models: Vec<(String, String)>,
    /// The models Codex lets the user choose, for when the agent becomes
    /// Codex.
    codex_models: Vec<String>,
    model: usize,
    mode: usize,
    /// Which of [`PLACES`].
    place: usize,
    args: TextInput,
    prompt: TextArea,
    instructions: TextArea,
    focus: Field,
}

impl Form {
    fn new(replacing: Option<String>, profile: &Profile, codex_models: &[String]) -> Form {
        let agent = catalog::AGENTS
            .iter()
            .position(|agent| agent.program == profile.agent)
            .unwrap_or(0);
        let mut prompt = TextArea::default();
        prompt.set_text(profile.prompt.as_deref().unwrap_or(""));
        let mut instructions = TextArea::default();
        instructions.set_text(profile.instructions.as_deref().unwrap_or(""));
        let args: Vec<String> = profile.args.iter().map(|arg| shell::quote(arg)).collect();
        let mut form = Form {
            replacing,
            name: TextInput::with_text(&profile.name),
            description: TextInput::with_text(profile.description.as_deref().unwrap_or("")),
            agent,
            models: Vec::new(),
            codex_models: codex_models.to_vec(),
            model: 0,
            mode: 0,
            place: PLACES
                .iter()
                .position(|(_, start_in)| *start_in == profile.start_in)
                .unwrap_or(0),
            args: TextInput::with_text(&args.join(" ")),
            prompt,
            instructions,
            focus: Field::Name,
        };
        form.models = model_choices(form.agent(), codex_models, profile.model.as_deref());
        form.model = form
            .models
            .iter()
            .position(|(_, value)| Some(value.as_str()) == profile.model.as_deref())
            .unwrap_or(0);
        form.mode = mode_row(form.agent())
            .iter()
            .position(|(_, value)| Some(*value) == profile.mode.as_deref())
            .unwrap_or(0);
        form
    }

    pub fn agent(&self) -> &'static Agent {
        &catalog::AGENTS[self.agent]
    }

    pub fn focus(&self) -> Field {
        self.focus
    }

    fn set_codex_models(&mut self, codex_models: &[String]) {
        self.codex_models = codex_models.to_vec();
        let chosen = self.models.get(self.model).map(|(_, value)| value.clone());
        self.models = model_choices(self.agent(), codex_models, chosen.as_deref());
        self.model = self
            .models
            .iter()
            .position(|(_, value)| Some(value) == chosen.as_ref())
            .unwrap_or(0);
    }

    /// The rows the form has for its agent, in order: the model and mode
    /// only for an agent that takes them, instructions only for one that
    /// can be given them.
    pub fn fields(&self) -> Vec<Field> {
        let agent = self.agent();
        let mut fields = vec![Field::Name, Field::Description, Field::Agent];
        if agent.model_setting().is_some() {
            fields.push(Field::Model);
        }
        if agent.mode_setting().is_some() {
            fields.push(Field::Mode);
        }
        fields.extend([Field::Where, Field::Args, Field::Prompt]);
        if agent.instructions != Instructions::None {
            fields.push(Field::Instructions);
        }
        fields
    }

    fn on_key(&mut self, key: &KeyEvent) -> FormKey {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => return FormKey::Back,
            KeyCode::Enter if alt => {
                if let Some(area) = self.text_area() {
                    area.newline();
                }
            }
            KeyCode::Enter => return FormKey::Save,
            KeyCode::Tab => self.move_focus(1),
            KeyCode::BackTab => self.move_focus(-1),
            KeyCode::Up => self.on_up(),
            KeyCode::Down => self.on_down(),
            KeyCode::Left if self.is_choice() => self.change_choice(-1),
            KeyCode::Right if self.is_choice() => self.change_choice(1),
            _ => self.type_key(key),
        }
        FormKey::Stay
    }

    /// Up moves within a box of lines, and from its first line, or any
    /// other row, to the row above.
    fn on_up(&mut self) {
        match self.text_area() {
            Some(area) if !area.on_first_line() => area.line_up(),
            _ => self.move_focus(-1),
        }
    }

    fn on_down(&mut self) {
        match self.text_area() {
            Some(area) if !area.on_last_line() => area.line_down(),
            _ => self.move_focus(1),
        }
    }

    fn move_focus(&mut self, by: isize) {
        let fields = self.fields();
        let at = fields.iter().position(|&f| f == self.focus).unwrap_or(0);
        let to = (at as isize + by).rem_euclid(fields.len() as isize) as usize;
        self.focus = fields[to];
    }

    fn is_choice(&self) -> bool {
        matches!(
            self.focus,
            Field::Agent | Field::Model | Field::Mode | Field::Where
        )
    }

    fn change_choice(&mut self, by: isize) {
        match self.focus {
            Field::Agent => {
                self.agent = step(self.agent, by, catalog::AGENTS.len());
                // Another agent takes other models and modes.
                self.models = model_choices(self.agent(), &self.codex_models, None);
                self.model = 0;
                self.mode = 0;
            }
            Field::Model => self.model = step(self.model, by, self.models.len()),
            Field::Mode => self.mode = step(self.mode, by, mode_row(self.agent()).len()),
            Field::Where => self.place = step(self.place, by, PLACES.len()),
            _ => {}
        }
    }

    fn type_key(&mut self, key: &KeyEvent) {
        match self.focus {
            Field::Name => self.name.on_key(key),
            Field::Description => self.description.on_key(key),
            Field::Args => self.args.on_key(key),
            Field::Prompt => {
                self.prompt.on_key(key);
            }
            Field::Instructions => {
                self.instructions.on_key(key);
            }
            _ => {}
        }
    }

    fn paste(&mut self, text: &str) {
        let one_line = text.replace(['\r', '\n'], " ");
        match self.focus {
            Field::Name => self.name.insert_str(&one_line),
            Field::Description => self.description.insert_str(&one_line),
            Field::Args => self.args.insert_str(&one_line),
            Field::Prompt => self.prompt.insert_str(text),
            Field::Instructions => self.instructions.insert_str(text),
            _ => {}
        }
    }

    fn text_area(&mut self) -> Option<&mut TextArea> {
        match self.focus {
            Field::Prompt => Some(&mut self.prompt),
            Field::Instructions => Some(&mut self.instructions),
            _ => None,
        }
    }

    /// The profile the form describes, or why it doesn't describe one yet.
    /// What only the config file can check, like a name taken twice, is
    /// checked when it's saved.
    pub fn profile(&self) -> Result<Profile, String> {
        let name = self.name.text().trim();
        if name.is_empty() {
            return Err("give the profile a name".to_string());
        }
        let args = command_line::split_words(self.args.text())?;
        let agent = self.agent();
        let model = self
            .models
            .get(self.model)
            .map(|(_, value)| value.clone())
            .filter(|value| !value.is_empty());
        let mode = mode_row(agent)
            .get(self.mode)
            .map(|(_, value)| value.to_string());
        let instructions = if agent.instructions == Instructions::None {
            None
        } else {
            filled(self.instructions.text())
        };
        Ok(Profile {
            name: name.to_string(),
            description: filled(self.description.text()),
            agent: agent.program.to_string(),
            model,
            mode: mode.filter(|mode| !mode.is_empty()),
            args,
            prompt: filled(self.prompt.text()),
            instructions,
            start_in: PLACES[self.place].1,
        })
    }

    /// What the row for `field` holds, for drawing: its lines of text as
    /// they fit in `width`, or its choices.
    fn value(&self, field: Field, width: usize) -> Value {
        let agent = self.agent();
        match field {
            Field::Name => Value::Text(vec![self.name.text().to_string()]),
            Field::Description => Value::Text(vec![self.description.text().to_string()]),
            Field::Agent => {
                let names = catalog::AGENTS.iter().map(|a| a.name.to_string()).collect();
                Value::Choices(names, self.agent)
            }
            Field::Model => {
                let shown = self.models.iter().map(|(shown, _)| shown.clone()).collect();
                Value::Choices(shown, self.model)
            }
            Field::Mode => {
                let shown = mode_row(agent).iter().map(|(s, _)| s.to_string()).collect();
                Value::Choices(shown, self.mode)
            }
            Field::Where => {
                let shown = PLACES.iter().map(|(shown, _)| shown.to_string()).collect();
                Value::Choices(shown, self.place)
            }
            Field::Args => Value::Text(vec![self.args.text().to_string()]),
            Field::Prompt => Value::Text(area_lines(&self.prompt, width)),
            Field::Instructions => Value::Text(area_lines(&self.instructions, width)),
        }
    }

    /// The label of the row for `field`. The mode row takes the agent's own
    /// word for it, as the panel does: permissions, approvals.
    fn label(&self, field: Field) -> &'static str {
        match field {
            Field::Name => "name",
            Field::Description => "description",
            Field::Agent => "agent",
            Field::Model => "model",
            Field::Mode => self.agent().mode_setting().map_or("mode", |s| s.label),
            Field::Where => "where",
            Field::Args => "arguments",
            Field::Prompt => "prompt",
            Field::Instructions => "instructions",
        }
    }

    /// Where the cursor is in the row for `field`, as (line, column), when
    /// it's a row to type in.
    fn cursor_in(&self, field: Field, width: usize) -> Option<(usize, usize)> {
        match field {
            Field::Name => Some((0, self.name.cursor())),
            Field::Description => Some((0, self.description.cursor())),
            Field::Args => Some((0, self.args.cursor())),
            Field::Prompt => Some(area_cursor(&self.prompt, width)),
            Field::Instructions => Some(area_cursor(&self.instructions, width)),
            _ => None,
        }
    }
}

/// What a row of the form shows.
enum Value {
    /// Lines of text: one, or a few of a box of lines.
    Text(Vec<String>),
    /// Choices on one line, and which is chosen.
    Choices(Vec<String>, usize),
}

impl Value {
    /// How many lines the row takes.
    fn height(&self) -> usize {
        match self {
            Value::Text(lines) => lines.len(),
            Value::Choices(..) => 1,
        }
    }
}

/// The choices a model row offers for `agent`, as `(shown, value)`, the
/// first being "default", which sets nothing. For Codex, its models, with
/// `keep` among them even when Codex doesn't list it.
fn model_choices(
    agent: &Agent,
    codex_models: &[String],
    keep: Option<&str>,
) -> Vec<(String, String)> {
    let mut choices = vec![("default".to_string(), String::new())];
    match agent.model_setting().map(|setting| &setting.choices) {
        Some(Choices::Fixed(fixed)) => {
            choices.extend(
                fixed
                    .iter()
                    .skip(1)
                    .map(|(shown, value)| (shown.to_string(), value.to_string())),
            );
        }
        Some(Choices::CodexModels) => {
            choices.extend(codex_models.iter().map(|m| (m.clone(), m.clone())));
            if let Some(model) = keep.filter(|model| !model.is_empty())
                && !codex_models.iter().any(|m| m == model)
            {
                choices.push((model.to_string(), model.to_string()));
            }
        }
        None => {}
    }
    choices
}

/// The mode row's choices for `agent`, as `(shown, value)`, the first
/// being the agent's own default, which sets nothing.
fn mode_row(agent: &Agent) -> &'static [(&'static str, &'static str)] {
    match agent.mode_setting().map(|setting| &setting.choices) {
        Some(Choices::Fixed(fixed)) => fixed,
        _ => &[],
    }
}

/// The text, unless it's only blanks.
fn filled(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// `at` moved `by` within `0..count`, going round at the ends.
fn step(at: usize, by: isize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    (at as isize + by).rem_euclid(count as isize) as usize
}

/// A box of lines as rows `width` wide, at most [`TEXT_ROWS`] of them,
/// scrolled to keep the cursor in sight.
fn area_lines(area: &TextArea, width: usize) -> Vec<String> {
    let rows = area.rows(width);
    let (cursor_row, _) = area.cursor_at(width);
    let first = cursor_row.saturating_sub(TEXT_ROWS - 1);
    let lines: Vec<String> = rows
        .iter()
        .skip(first)
        .take(TEXT_ROWS)
        .map(|&row| area.row_text(row))
        .collect();
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

/// Where the cursor is among a box's lines as [`area_lines`] shows them.
fn area_cursor(area: &TextArea, width: usize) -> (usize, usize) {
    let (row, column) = area.cursor_at(width);
    (row.min(TEXT_ROWS - 1), column)
}

/// A line of the view, before it's given the theme's colors.
struct ViewLine {
    spans: Vec<(String, Ink)>,
}

#[derive(Clone, Copy)]
enum Ink {
    Title,
    Text,
    Muted,
    Label,
    FocusedLabel,
    Selected,
    Choice,
    Chosen,
    /// The chosen one on the row that has the keyboard.
    ChosenHere,
    Problem,
}

impl ViewLine {
    fn new(spans: Vec<(String, Ink)>) -> ViewLine {
        ViewLine { spans }
    }

    fn blank() -> ViewLine {
        ViewLine { spans: Vec::new() }
    }

    /// The text of the line, without its colors: what tests read.
    #[cfg(test)]
    fn text(&self) -> String {
        self.spans.iter().map(|(text, _)| text.as_str()).collect()
    }

    fn styled(self, theme: &Theme) -> Line<'static> {
        let spans = self.spans.into_iter().map(|(text, ink)| {
            let style = match ink {
                Ink::Title => Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
                Ink::Text => Style::new().fg(theme.text),
                Ink::Muted | Ink::Label | Ink::Choice => Style::new().fg(theme.muted),
                Ink::FocusedLabel => Style::new().fg(theme.accent),
                Ink::Selected | Ink::Chosen => {
                    Style::new().fg(theme.text).add_modifier(Modifier::BOLD)
                }
                Ink::ChosenHere => Style::new()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                Ink::Problem => Style::new().fg(theme.failed),
            };
            Span::styled(text, style)
        });
        Line::from(spans.collect::<Vec<_>>())
    }
}

/// The view's lines in a panel `width` wide, top to bottom.
fn view_lines(view: &ProfilesView, width: usize) -> Vec<ViewLine> {
    let mut lines = match view.form() {
        Some(form) => form_lines(form, width),
        None => list_lines(view, width),
    };
    if let Some(name) = view.deleting() {
        lines.push(ViewLine::blank());
        lines.push(ViewLine::new(vec![(
            format!("remove profile {name}? y/n"),
            Ink::Problem,
        )]));
    }
    if let Some(problem) = view.problem() {
        lines.push(ViewLine::blank());
        lines.push(ViewLine::new(vec![(cut(problem, width), Ink::Problem)]));
    }
    lines
}

fn list_lines(view: &ProfilesView, width: usize) -> Vec<ViewLine> {
    let mut lines = vec![
        ViewLine::new(vec![("Profiles".to_string(), Ink::Title)]),
        ViewLine::blank(),
    ];
    if view.profiles().is_empty() {
        lines.push(ViewLine::new(vec![(
            "No profiles yet: a adds one.".to_string(),
            Ink::Muted,
        )]));
    }
    for (index, profile) in view.profiles().iter().enumerate() {
        let selected = index == view.selected();
        let (bar, ink) = if selected {
            ("› ", Ink::Selected)
        } else {
            ("  ", Ink::Text)
        };
        let name = format!("{bar}{:<width$}", profile.name, width = LABEL_WIDTH - 2);
        let about = cut(&summary(profile), width.saturating_sub(LABEL_WIDTH));
        lines.push(ViewLine::new(vec![(name, ink), (about, Ink::Muted)]));
        if let Some(description) = &profile.description {
            lines.push(ViewLine::new(vec![
                (" ".repeat(LABEL_WIDTH), Ink::Muted),
                (
                    cut(description, width.saturating_sub(LABEL_WIDTH)),
                    Ink::Muted,
                ),
            ]));
        }
    }
    lines
}

/// The footer's keys for the view, as it is now: the list's or the form's.
pub fn hints(view: &ProfilesView) -> &'static [(&'static str, &'static str)] {
    if view.form().is_some() {
        &[
            ("enter", "save"),
            ("tab", "next"),
            ("←/→", "choose"),
            ("alt+enter", "new line"),
            ("esc", "back"),
        ]
    } else {
        &[
            ("enter", "change"),
            ("a", "add"),
            ("c", "copy"),
            ("x", "remove"),
            ("esc", "close"),
        ]
    }
}

/// What a profile runs, in a few words: `Claude Code · opus · plan · here`.
fn summary(profile: &Profile) -> String {
    let agent = catalog::find(&profile.agent).map_or(profile.agent.as_str(), |a| a.name);
    let mut parts = vec![agent.to_string()];
    parts.extend(profile.model.clone());
    parts.extend(profile.mode.clone());
    match profile.start_in {
        Some(StartIn::Here) => parts.push("here".to_string()),
        Some(StartIn::Worktree) => parts.push("new worktree".to_string()),
        None => {}
    }
    parts.join(" · ")
}

fn form_lines(form: &Form, width: usize) -> Vec<ViewLine> {
    let title = match &form.replacing {
        Some(name) => format!("Profile · {name}"),
        None => "New profile".to_string(),
    };
    let mut lines = vec![ViewLine::new(vec![(title, Ink::Title)]), ViewLine::blank()];
    let room = width.saturating_sub(LABEL_WIDTH);
    for field in form.fields() {
        let focused = form.focus() == field;
        let ink = if focused {
            Ink::FocusedLabel
        } else {
            Ink::Label
        };
        let label = (format!("{:<LABEL_WIDTH$}", form.label(field)), ink);
        match form.value(field, room) {
            Value::Text(texts) => {
                for (index, text) in texts.into_iter().enumerate() {
                    let label = if index == 0 {
                        label.clone()
                    } else {
                        (" ".repeat(LABEL_WIDTH), ink)
                    };
                    lines.push(ViewLine::new(vec![label, (cut(&text, room), Ink::Text)]));
                }
            }
            Value::Choices(choices, chosen) => {
                let mut spans = vec![label];
                spans.extend(choice_spans(&choices, chosen, focused, room));
                lines.push(ViewLine::new(spans));
            }
        }
    }
    lines.push(ViewLine::blank());
    let runs = match form.profile() {
        Ok(profile) => {
            let command = profile.command("<task>");
            let quoted: Vec<String> = command.iter().map(|arg| shell::quote(arg)).collect();
            quoted.join(" ")
        }
        Err(problem) => problem,
    };
    lines.push(ViewLine::new(vec![
        (format!("{:<LABEL_WIDTH$}", "runs"), Ink::Muted),
        (cut(&runs, room), Ink::Muted),
    ]));
    lines
}

/// A row of choices as the panel draws one: as many as fit in `room`
/// around the chosen one, with `…` where there are more.
fn choice_spans(
    choices: &[String],
    chosen: usize,
    focused: bool,
    room: usize,
) -> Vec<(String, Ink)> {
    let (from, to) = shown_choices(choices, chosen, room);
    let mut spans = Vec::new();
    if from > 0 {
        spans.push(("… ".to_string(), Ink::Muted));
    }
    for (index, choice) in choices.iter().enumerate().take(to).skip(from) {
        let ink = match (index == chosen, focused) {
            (true, true) => Ink::ChosenHere,
            (true, false) => Ink::Chosen,
            (false, _) => Ink::Choice,
        };
        spans.push((choice.clone(), ink));
        spans.push(("   ".to_string(), Ink::Muted));
    }
    if to < choices.len() {
        spans.push(("…".to_string(), Ink::Muted));
    }
    spans
}

/// Where the cursor goes among the form's lines, as (line, column), when
/// the row that has the keyboard is one to type in.
fn form_cursor(form: &Form, width: usize) -> Option<(usize, usize)> {
    let room = width.saturating_sub(LABEL_WIDTH);
    // Below the title and a blank line.
    let mut line = 2;
    for field in form.fields() {
        if field == form.focus() {
            let (row, column) = form.cursor_in(field, room)?;
            return Some((line + row, LABEL_WIDTH + column));
        }
        line += form.value(field, room).height();
    }
    None
}

/// Draws the view over `area`, the panes, which stay in sight, dimmed.
pub fn draw(frame: &mut Frame, view: &ProfilesView, theme: &Theme, area: Rect) {
    frame
        .buffer_mut()
        .set_style(area, Style::new().add_modifier(Modifier::DIM));
    let width = area.width.saturating_sub(4).clamp(40.min(area.width), 100);
    // Two columns of room a side, and a row above and below, frame or not.
    let room = usize::from(width.saturating_sub(4));
    let lines = view_lines(view, room);
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
    let styled: Vec<Line> = lines.into_iter().map(|line| line.styled(theme)).collect();
    frame.render_widget(Paragraph::new(styled), inside);
    let cursor = view
        .form()
        .filter(|_| view.deleting().is_none())
        .and_then(|form| form_cursor(form, room));
    if let Some((line, column)) = cursor {
        let (x, y) = (inside.x + column as u16, inside.y + line as u16);
        if x < inside.right() && y < inside.bottom() {
            frame.set_cursor_position((x, y));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press(view: &mut ProfilesView, code: KeyCode) -> Outcome {
        view.on_key(key(code))
    }

    fn type_text(view: &mut ProfilesView, text: &str) {
        for c in text.chars() {
            press(view, KeyCode::Char(c));
        }
    }

    fn review() -> Profile {
        Profile {
            name: "review".into(),
            description: Some("A second pair of eyes".into()),
            mode: Some("plan".into()),
            instructions: Some("Point out risks first.".into()),
            ..Profile::for_agent("claude")
        }
    }

    fn view() -> ProfilesView {
        ProfilesView::new(vec![review()], vec!["gpt-6-luna".into()])
    }

    #[test]
    fn editing_a_profile_saves_it_in_place_of_itself() {
        let mut view = view();
        press(&mut view, KeyCode::Enter);
        assert_eq!(view.form().unwrap().focus(), Field::Name);
        type_text(&mut view, "er");
        let Outcome::Save { replacing, profile } = press(&mut view, KeyCode::Enter) else {
            panic!("didn't save");
        };
        assert_eq!(replacing.as_deref(), Some("review"));
        assert_eq!(profile.name, "reviewer");
        assert_eq!(profile.mode.as_deref(), Some("plan"));
        assert_eq!(
            profile.instructions.as_deref(),
            Some("Point out risks first.")
        );
    }

    #[test]
    fn a_new_profile_needs_a_name() {
        let mut view = view();
        press(&mut view, KeyCode::Char('a'));
        assert_eq!(view.form().unwrap().replacing, None);
        assert_eq!(press(&mut view, KeyCode::Enter), Outcome::Stay);
        assert_eq!(view.problem(), Some("give the profile a name"));
        type_text(&mut view, "quick");
        let Outcome::Save { replacing, profile } = press(&mut view, KeyCode::Enter) else {
            panic!("didn't save");
        };
        assert_eq!(replacing, None);
        assert_eq!(
            profile,
            Profile {
                name: "quick".into(),
                ..Profile::for_agent("claude")
            }
        );
    }

    #[test]
    fn rows_change_with_the_arrows_and_follow_the_agent() {
        let mut view = view();
        press(&mut view, KeyCode::Char('a'));
        type_text(&mut view, "fast");
        // Down to the agent row, and right to Codex.
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Down);
        assert_eq!(view.form().unwrap().focus(), Field::Agent);
        press(&mut view, KeyCode::Right);
        assert_eq!(view.form().unwrap().agent().program, "codex");
        // Codex's model row offers its models; its mode row, approvals.
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Right);
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Right);
        // Where: a new worktree.
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Right);
        press(&mut view, KeyCode::Right);
        let profile = view.form().unwrap().profile().unwrap();
        assert_eq!(profile.agent, "codex");
        assert_eq!(profile.model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(profile.mode.as_deref(), Some("on-request"));
        assert_eq!(profile.start_in, Some(StartIn::Worktree));
    }

    #[test]
    fn an_agent_that_takes_no_instructions_has_no_row_for_them() {
        let mut view = view();
        press(&mut view, KeyCode::Char('a'));
        assert!(view.form().unwrap().fields().contains(&Field::Instructions));
        // Aider is the last agent crystal knows: left from the first.
        press(&mut view, KeyCode::Tab);
        press(&mut view, KeyCode::Tab);
        press(&mut view, KeyCode::Left);
        let form = view.form().unwrap();
        assert_eq!(form.agent().program, "aider");
        assert!(!form.fields().contains(&Field::Instructions));
        assert!(!form.fields().contains(&Field::Model));
    }

    #[test]
    fn arguments_are_read_the_way_a_shell_reads_them() {
        let mut view = view();
        press(&mut view, KeyCode::Char('a'));
        type_text(&mut view, "x");
        // Name, description, agent, model, mode, where, then arguments.
        for _ in 0..6 {
            press(&mut view, KeyCode::Tab);
        }
        assert_eq!(view.form().unwrap().focus(), Field::Args);
        type_text(&mut view, "--add-dir '../shared code'");
        let profile = view.form().unwrap().profile().unwrap();
        assert_eq!(profile.args, ["--add-dir", "../shared code"]);
    }

    #[test]
    fn copying_makes_a_new_profile_under_another_name() {
        let mut view = view();
        press(&mut view, KeyCode::Char('c'));
        let Outcome::Save { replacing, profile } = press(&mut view, KeyCode::Enter) else {
            panic!("didn't save");
        };
        assert_eq!(replacing, None);
        assert_eq!(profile.name, "review copy");
        assert_eq!(profile.description, review().description);
    }

    #[test]
    fn removing_asks_first() {
        let mut view = view();
        press(&mut view, KeyCode::Char('x'));
        assert_eq!(view.deleting(), Some("review"));
        assert_eq!(press(&mut view, KeyCode::Char('n')), Outcome::Stay);
        assert_eq!(view.deleting(), None);
        press(&mut view, KeyCode::Char('x'));
        assert_eq!(
            press(&mut view, KeyCode::Char('y')),
            Outcome::Delete("review".into())
        );
    }

    #[test]
    fn esc_leaves_the_form_unsaved_then_closes_the_view() {
        let mut view = view();
        press(&mut view, KeyCode::Enter);
        type_text(&mut view, "zzz");
        assert_eq!(press(&mut view, KeyCode::Esc), Outcome::Stay);
        assert!(view.form().is_none());
        assert_eq!(view.profiles()[0].name, "review");
        assert_eq!(press(&mut view, KeyCode::Esc), Outcome::Close);
    }

    #[test]
    fn the_cursor_is_where_typing_goes() {
        let mut view = view();
        press(&mut view, KeyCode::Char('a'));
        type_text(&mut view, "ab");
        // Below the title and a blank line, after the label.
        let form = view.form().unwrap();
        assert_eq!(form_cursor(form, 60), Some((2, LABEL_WIDTH + 2)));
        // A row of choices has none.
        press(&mut view, KeyCode::Tab);
        press(&mut view, KeyCode::Tab);
        assert_eq!(form_cursor(view.form().unwrap(), 60), None);
    }

    #[test]
    fn the_form_shows_what_the_profile_runs() {
        let mut view = view();
        press(&mut view, KeyCode::Enter);
        let lines: Vec<String> = view_lines(&view, 96).iter().map(ViewLine::text).collect();
        let runs = lines.iter().find(|line| line.starts_with("runs")).unwrap();
        assert!(
            runs.contains("claude --permission-mode plan --append-system-prompt"),
            "{runs}"
        );
    }

    #[test]
    fn the_list_says_what_each_profile_runs() {
        let lines: Vec<String> = view_lines(&view(), 96).iter().map(ViewLine::text).collect();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("review") && line.contains("Claude Code · plan"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("A second pair of eyes"))
        );
    }
}
