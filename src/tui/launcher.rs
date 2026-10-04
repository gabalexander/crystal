//! The panel `n` opens to start a session: what to ask for, what runs it,
//! how that's set up, and where it starts. It builds the command line the
//! session runs, and shows it, so nothing it does is hidden; Ctrl+E turns
//! it into that command line, to change anything the panel can't.
//!
//! The panel is state and logic only, apart from [`draw`] at the end and
//! the two functions that keep its memory on disk; the event loop does the
//! rest.

use super::app::Place;
use super::text_area::TextArea;
use super::text_input::TextInput;
use super::theme::Theme;
use crate::catalog::{self, Agent, Choices, FirstPrompt, Setting};
use crate::flows::Flow;
use crate::forge::Checkout;
use crate::profile::{Profile, StartIn};
use crate::protocol::TaskSpec;
use crate::{git, shell};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How many earlier tasks are remembered, for Up to bring back.
const HISTORY_LENGTH: usize = 100;

/// The most rows the task box grows to before it scrolls.
const TASK_ROWS: usize = 4;

/// How wide the labels of the panel's rows are, so the choices line up.
const LABEL_WIDTH: usize = 13;

/// What can be started: a profile from the config file, an agent crystal
/// knows, the user's shell, or a flow from the config file, whose goal is
/// the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    Profile(Profile),
    Agent(&'static Agent),
    Shell,
    Flow(Flow),
}

impl Run {
    /// What the panel calls it.
    pub fn label(&self) -> String {
        match self {
            Run::Profile(profile) => profile.name.clone(),
            Run::Agent(agent) => agent.name.to_string(),
            Run::Shell => "shell".to_string(),
            Run::Flow(flow) => format!("flow: {}", flow.name),
        }
    }

    /// What it's remembered by, to pick it again next time.
    pub fn key(&self) -> String {
        match self {
            Run::Profile(profile) => format!("profile:{}", profile.name),
            Run::Agent(agent) => agent.program.to_string(),
            Run::Shell => "shell".to_string(),
            Run::Flow(flow) => format!("flow:{}", flow.name),
        }
    }

    fn agent(&self) -> Option<&'static Agent> {
        match self {
            Run::Profile(profile) => catalog::find(&profile.agent),
            Run::Agent(agent) => Some(agent),
            Run::Shell | Run::Flow(_) => None,
        }
    }

    /// Whether it can be given a task on its command line, or for a flow,
    /// a goal.
    pub fn takes_task(&self) -> bool {
        let agent_takes = self
            .agent()
            .is_some_and(|agent| agent.first_prompt != FirstPrompt::None);
        agent_takes || matches!(self, Run::Flow(_))
    }

    /// The rows of choices the panel shows for it: its agent's. A profile
    /// starts them at what it sets, and they can still be changed.
    fn settings(&self) -> &'static [Setting] {
        self.agent().map_or(&[], |agent| agent.settings)
    }

    /// What it starts from: a profile as it's written, or an agent with
    /// nothing set.
    fn profile(&self) -> Option<Profile> {
        match self {
            Run::Profile(profile) => Some(profile.clone()),
            Run::Agent(agent) => Some(Profile::for_agent(agent.program)),
            Run::Shell | Run::Flow(_) => None,
        }
    }
}

/// Where the session can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Where the selected session runs, or the TUI's own directory when
    /// `dir` is `None`. `label` says which, like `payments ⌂ main`.
    Here { dir: Option<PathBuf>, label: String },
    /// A new worktree of the project at `base`, or of the TUI's own
    /// directory's when that's `None`.
    NewWorktree {
        base: Option<PathBuf>,
        project: Option<String>,
    },
    /// Another project's main worktree.
    Project { path: PathBuf, label: String },
    /// A pull request's worktree, which `checkout` finds or makes. `label`
    /// says where that is, like `app ⎇ fix-login`, and `choice` what it's
    /// for, like `pull request #57`.
    PullRequest {
        checkout: Checkout,
        label: String,
        choice: String,
    },
}

impl Target {
    /// What the row of places calls it.
    fn choice(&self) -> String {
        match self {
            Target::Here { .. } => "here".to_string(),
            Target::NewWorktree { .. } => "new worktree".to_string(),
            Target::Project { label, .. } => label.clone(),
            Target::PullRequest { choice, .. } => choice.clone(),
        }
    }
}

/// A field of the panel, which Tab moves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Task,
    Run,
    /// How an agent that can work without a terminal runs: in one, or in
    /// the background.
    How,
    /// The agent's row of choices with this index in its settings.
    Setting(usize),
    Where,
    Branch,
}

/// The choices of the "how" row, in order: the first is the default.
const HOW: [&str; 2] = ["in a terminal", "in the background"];

/// What the panel opens with.
pub struct Setup {
    pub runs: Vec<Run>,
    pub run: usize,
    pub targets: Vec<Target>,
    pub target: usize,
    /// Earlier tasks, the most recent last.
    pub history: Vec<String>,
    pub codex_models: Vec<String>,
    /// Whether an agent that can work without a terminal may be started in
    /// the background: tasks are on.
    pub background: bool,
    /// The made-up name a new worktree's branch starts with.
    pub branch: String,
}

/// What a key in the panel leads to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Cancel,
    /// Start `command` at `place`: in a terminal, or with `background`,
    /// without one, as a background task. `task` is what the agent was
    /// asked, and with `run`, what the panel remembers. `backlog` is the
    /// backlog item the session is for.
    Start {
        place: Place,
        command: Vec<String>,
        task: String,
        run: String,
        background: bool,
        backlog: Option<u64>,
    },
    /// Start a run of the flow called `flow` at `place`, on `goal`. `run`
    /// is what the panel remembers.
    StartFlow {
        place: Place,
        flow: String,
        goal: String,
        run: String,
    },
    /// Close the panel for the one-line command line, holding `line`.
    CommandLine {
        place: Place,
        line: String,
    },
}

/// What the panel held when it was put away with a task written in it,
/// for the next `n` or `w` to open on: the task, what runs it and the
/// choices in its rows, and where it was to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    task: String,
    /// What runs it, by [`Run::key`].
    run: String,
    /// The choice in each of its rows, as the row shows it.
    rows: Vec<String>,
    touched: bool,
    how: usize,
    /// Where the panel was opened to start, which says whether the panel
    /// it comes back in is the same one.
    opened_at: Target,
    /// Where it was to start.
    target: Target,
    /// The new worktree's branch, when one was typed in place of the
    /// made-up name.
    branch: Option<String>,
    focus: Field,
}

pub struct Launcher {
    task: TextArea,
    focus: Field,
    runs: Vec<Run>,
    run: usize,
    /// The choice made in each of the agent's rows, by row.
    chosen: Vec<usize>,
    /// Whether a row has been changed by hand since what runs was chosen,
    /// so the profile's own choices aren't put back over it.
    touched: bool,
    targets: Vec<Target>,
    target: usize,
    branch: TextInput,
    /// The name crystal made up for the branch, which it keeps unless
    /// another is typed or given.
    made_up: String,
    codex_models: Vec<String>,
    history: Vec<String>,
    /// While Up is bringing back earlier tasks: how many back, and the
    /// task as it was before, for Down to return to.
    recalling: Option<(usize, String)>,
    /// Why Enter didn't start the session, for the panel to say.
    problem: Option<String>,
    /// Whether the "how" row is offered at all.
    offers_background: bool,
    /// The choice in the "how" row: 0 in a terminal, 1 in the background.
    how: usize,
    /// The backlog item the session is for, when it was started from one.
    backlog: Option<u64>,
    /// Where the panel was opened to start, by its place in `targets`.
    opened_at: usize,
    /// Whether what the panel holds is kept as a draft when it's put away:
    /// opened by `n` or `w`, it is; for an issue, a pull request or a
    /// backlog item, which bring their own task, it isn't.
    keeps_draft: bool,
    /// Whether it opened on the draft left last time, which it says.
    from_draft: bool,
}

impl Launcher {
    pub fn new(setup: Setup) -> Launcher {
        let mut launcher = Launcher {
            task: TextArea::default(),
            focus: Field::Task,
            runs: setup.runs,
            run: 0,
            chosen: Vec::new(),
            touched: false,
            targets: setup.targets,
            target: setup.target,
            branch: TextInput::with_text(&setup.branch),
            made_up: setup.branch,
            codex_models: setup.codex_models,
            history: setup.history,
            recalling: None,
            problem: None,
            offers_background: setup.background,
            how: 0,
            backlog: None,
            opened_at: setup.target,
            keeps_draft: false,
            from_draft: false,
        };
        launcher.choose_run(setup.run);
        // Opened for a new worktree, as by `w`, it stays one whatever the
        // profile picked first says.
        if setup.target != 0 {
            launcher.target = setup.target;
        }
        launcher
    }

    /// Starts out with `task` already written, as for an issue.
    pub fn with_task(mut self, task: &str) -> Launcher {
        self.task.set_text(task);
        self
    }

    /// Starts out with a new worktree's branch named `branch`, rather than
    /// a made-up name.
    pub fn with_branch(mut self, branch: &str) -> Launcher {
        self.branch = TextInput::with_text(branch);
        self
    }

    /// Starts a task for backlog item `number`, which closing the task done
    /// ticks.
    pub fn for_backlog_item(mut self, number: u64) -> Launcher {
        self.backlog = Some(number);
        self
    }

    /// Has the panel keep what it holds as a draft when it's put away, and
    /// opens it on `draft`, what it held when it was last put away. The
    /// task always comes back, and what runs it and the choices in its rows
    /// while they're still offered; where it starts, and the branch, only
    /// when the panel opens where it did before, since `n` on another
    /// session, or `w`, aims it elsewhere.
    pub fn with_draft(mut self, draft: Option<Draft>) -> Launcher {
        self.keeps_draft = true;
        let Some(draft) = draft else {
            return self;
        };
        self.from_draft = true;
        self.task.set_text(&draft.task);
        if let Some(run) = self.runs.iter().position(|run| run.key() == draft.run) {
            self.choose_run(run);
            // As in `new`: opened for a new worktree, it stays one.
            if self.opened_at != 0 {
                self.target = self.opened_at;
            }
            for (row, setting) in self.run().settings().iter().enumerate() {
                let choices = self.choices(setting);
                let wanted = draft.rows.get(row);
                if let Some(at) = choices.iter().position(|choice| Some(choice) == wanted) {
                    self.chosen[row] = at;
                }
            }
            self.touched = draft.touched;
            self.how = draft.how.min(HOW.len() - 1);
        }
        if self.targets.get(self.opened_at) == Some(&draft.opened_at) {
            if let Some(at) = self.targets.iter().position(|t| *t == draft.target) {
                self.target = at;
            }
            if let Some(branch) = &draft.branch {
                self.branch = TextInput::with_text(branch);
            }
        }
        if self.fields().contains(&draft.focus) {
            self.focus = draft.focus;
        }
        self
    }

    /// Whether what the panel holds is kept as a draft when it's put away.
    pub fn keeps_draft(&self) -> bool {
        self.keeps_draft
    }

    /// What the panel holds, to keep as it's put away, or `None` when it
    /// has no task written in it: an empty panel put away is a change of
    /// mind.
    pub fn draft(&self) -> Option<Draft> {
        if self.task.text().trim().is_empty() {
            return None;
        }
        let rows = self.run().settings().iter().zip(&self.chosen);
        let rows = rows.map(|(setting, &chosen)| {
            let choices = self.choices(setting);
            choices.get(chosen).cloned().unwrap_or_default()
        });
        Some(Draft {
            task: self.task.text().to_string(),
            run: self.run().key(),
            rows: rows.collect(),
            touched: self.touched,
            how: self.how,
            opened_at: self.targets[self.opened_at].clone(),
            target: self.target().clone(),
            branch: (self.branch.text() != self.made_up).then(|| self.branch.text().to_string()),
            focus: self.focus,
        })
    }

    /// Whether the panel opened on the draft left last time, and still has
    /// its task, for it to say so.
    pub fn shows_draft(&self) -> bool {
        self.from_draft && !self.task.text().trim().is_empty()
    }

    /// Whether the "how" row is shown: what's chosen can work without a
    /// terminal. Only Claude Code can, through `claude -p`; Codex's `codex
    /// exec` speaks another language crystal doesn't read yet.
    fn can_run_in_background(&self) -> bool {
        self.offers_background
            && self
                .run()
                .agent()
                .is_some_and(|agent| agent.program == "claude")
    }

    /// Whether the session will start in the background.
    pub fn in_background(&self) -> bool {
        self.can_run_in_background() && self.how == 1
    }

    pub fn focus(&self) -> Field {
        self.focus
    }

    pub fn task(&self) -> &TextArea {
        &self.task
    }

    pub fn branch_input(&self) -> &TextInput {
        &self.branch
    }

    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    pub fn run(&self) -> &Run {
        &self.runs[self.run]
    }

    fn target(&self) -> &Target {
        &self.targets[self.target]
    }

    /// Takes the models Codex lists, once they've been read.
    pub fn set_codex_models(&mut self, models: Vec<String>) {
        self.codex_models = models;
        if !self.touched {
            // The profile's model can be found among them now.
            self.choose_rows();
            return;
        }
        let rows = self.run().settings();
        for (row, setting) in rows.iter().enumerate() {
            let count = self.choices(setting).len();
            if self.chosen[row] >= count {
                self.chosen[row] = 0;
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        self.problem = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Char('e') if ctrl && matches!(self.run(), Run::Flow(_)) => {
                self.problem = Some("a flow has no command line to edit".to_string());
            }
            KeyCode::Char('e') if ctrl => {
                return Outcome::CommandLine {
                    place: self.place(),
                    line: self.command_line(),
                };
            }
            KeyCode::Enter if alt && self.focus == Field::Task => self.task.newline(),
            KeyCode::Enter => return self.start(),
            KeyCode::Tab => self.move_focus(1),
            KeyCode::BackTab => self.move_focus(-1),
            _ => match self.focus {
                Field::Task => self.on_task_key(&key),
                Field::Branch => self.on_branch_key(&key),
                Field::Run | Field::How | Field::Setting(_) | Field::Where => {
                    self.on_choice_key(&key)
                }
            },
        }
        Outcome::Stay
    }

    /// A paste goes into the text field that has the keyboard: the task
    /// keeps its lines, a branch name can't have any.
    pub fn on_paste(&mut self, text: &str) {
        match self.focus {
            Field::Task => {
                self.task.insert_str(text);
                self.recalling = None;
            }
            Field::Branch => self.branch.insert_str(text),
            _ => {}
        }
    }

    /// The fields Tab goes through, in order: the task when what's chosen
    /// takes one, what to run and its rows, where, and the branch when it's
    /// a new worktree.
    pub fn fields(&self) -> Vec<Field> {
        let mut fields = Vec::new();
        if self.run().takes_task() {
            fields.push(Field::Task);
        }
        fields.push(Field::Run);
        if self.can_run_in_background() {
            fields.push(Field::How);
        }
        fields.extend((0..self.run().settings().len()).map(Field::Setting));
        fields.push(Field::Where);
        if self.is_new_worktree() {
            fields.push(Field::Branch);
        }
        fields
    }

    fn move_focus(&mut self, by: isize) {
        let fields = self.fields();
        let at = fields
            .iter()
            .position(|&field| field == self.focus)
            .unwrap_or(0);
        let count = fields.len() as isize;
        let to = (at as isize + by).rem_euclid(count) as usize;
        self.focus = fields[to];
    }

    /// Up on the task's first line brings back the task before; Down on
    /// its last line goes the other way, back to what was being typed.
    fn on_task_key(&mut self, key: &KeyEvent) {
        match key.code {
            KeyCode::Up if self.task.on_first_line() => self.recall_older(),
            KeyCode::Down if self.task.on_last_line() => self.recall_newer(),
            KeyCode::Up => self.task.line_up(),
            KeyCode::Down => self.task.line_down(),
            _ => {
                if self.task.on_key(key) {
                    self.recalling = None;
                }
            }
        }
    }

    fn recall_older(&mut self) {
        let back = self.recalling.as_ref().map_or(0, |(back, _)| *back) + 1;
        if back > self.history.len() {
            return;
        }
        let draft = match self.recalling.take() {
            Some((_, draft)) => draft,
            None => self.task.text().to_string(),
        };
        let earlier = self.history[self.history.len() - back].clone();
        self.task.set_text(&earlier);
        self.recalling = Some((back, draft));
    }

    fn recall_newer(&mut self) {
        let Some((back, draft)) = self.recalling.take() else {
            return;
        };
        if back == 1 {
            self.task.set_text(&draft);
            return;
        }
        let later = self.history[self.history.len() - (back - 1)].clone();
        self.task.set_text(&later);
        self.recalling = Some((back - 1, draft));
    }

    fn on_branch_key(&mut self, key: &KeyEvent) {
        self.branch.on_key(key);
    }

    /// ←/→ change the choice in a row; ↑/↓ go to the row above or below.
    fn on_choice_key(&mut self, key: &KeyEvent) {
        match key.code {
            KeyCode::Left => self.change_choice(-1),
            KeyCode::Right => self.change_choice(1),
            KeyCode::Up => self.move_focus(-1),
            KeyCode::Down => self.move_focus(1),
            _ => {}
        }
    }

    fn change_choice(&mut self, by: isize) {
        match self.focus {
            Field::Run => {
                let to = step(self.run, by, self.runs.len());
                self.choose_run(to);
            }
            Field::Setting(row) => {
                let setting = &self.run().settings()[row];
                let count = self.choices(setting).len();
                self.chosen[row] = step(self.chosen[row], by, count);
                self.touched = true;
            }
            Field::How => self.how = step(self.how, by, HOW.len()),
            Field::Where => self.target = step(self.target, by, self.targets.len()),
            Field::Task | Field::Branch => {}
        }
    }

    /// Picks what runs, with its rows, and for a profile where it starts,
    /// set from it.
    fn choose_run(&mut self, run: usize) {
        self.run = run.min(self.runs.len().saturating_sub(1));
        self.touched = false;
        self.choose_rows();
        if let Run::Profile(Profile {
            start_in: Some(start_in),
            ..
        }) = self.run()
        {
            self.start_in(*start_in);
        }
        if self.focus == Field::Task && !self.run().takes_task() {
            self.focus = Field::Run;
        }
    }

    /// Puts each row at what the profile chosen sets, or else its default.
    fn choose_rows(&mut self) {
        let profile = self.run().profile();
        let settings = self.run().settings();
        // A model the profile names stays choosable even when Codex hasn't
        // listed its models yet, or doesn't list that one.
        if let Some(model) = profile.as_ref().and_then(|p| p.model.clone())
            && settings.iter().any(|s| s.choices == Choices::CodexModels)
            && !self.codex_models.contains(&model)
        {
            self.codex_models.push(model);
        }
        self.chosen = settings
            .iter()
            .map(|setting| {
                let wanted = profile
                    .as_ref()
                    .and_then(|p| p.choice(setting.kind).clone());
                wanted
                    .and_then(|value| self.index_of(setting, &value))
                    .unwrap_or(0)
            })
            .collect();
    }

    /// Where a row's choice gives its option `value`, if one does.
    fn index_of(&self, setting: &Setting, value: &str) -> Option<usize> {
        match setting.choices {
            Choices::Fixed(choices) => choices.iter().position(|(_, given)| *given == value),
            Choices::CodexModels => self
                .codex_models
                .iter()
                .position(|model| model == value)
                .map(|at| at + 1),
        }
    }

    /// Sets where the session starts to `start_in`, when the panel offers
    /// it.
    fn start_in(&mut self, start_in: StartIn) {
        let wanted = |target: &Target| match start_in {
            StartIn::Here => matches!(target, Target::Here { .. }),
            StartIn::Worktree => matches!(target, Target::NewWorktree { .. }),
        };
        if let Some(index) = self.targets.iter().position(wanted) {
            self.target = index;
        }
    }

    /// The choices a row offers, as the panel shows them.
    fn choices(&self, setting: &Setting) -> Vec<String> {
        match setting.choices {
            Choices::Fixed(choices) => choices.iter().map(|(label, _)| label.to_string()).collect(),
            Choices::CodexModels => {
                let models = self.codex_models.iter().cloned();
                std::iter::once("default".to_string())
                    .chain(models)
                    .collect()
            }
        }
    }

    /// What a row's choice gives its option, or `None` for the default,
    /// which leaves the option off.
    fn value(&self, setting: &Setting, choice: usize) -> Option<String> {
        let value = match setting.choices {
            Choices::Fixed(choices) => choices.get(choice)?.1.to_string(),
            Choices::CodexModels => self.codex_models.get(choice.checked_sub(1)?)?.clone(),
        };
        (!value.is_empty()).then_some(value)
    }

    /// Every row of choices, for drawing: its field, label, choices, and
    /// which one is chosen.
    pub fn choice_rows(&self) -> Vec<(Field, &'static str, Vec<String>, usize)> {
        let mut rows = vec![(
            Field::Run,
            "run",
            self.runs.iter().map(Run::label).collect(),
            self.run,
        )];
        if self.can_run_in_background() {
            let how = HOW.iter().map(|how| how.to_string()).collect();
            rows.push((Field::How, "how", how, self.how));
        }
        for (index, setting) in self.run().settings().iter().enumerate() {
            let choices = self.choices(setting);
            rows.push((
                Field::Setting(index),
                setting.label,
                choices,
                self.chosen[index],
            ));
        }
        let places = self.targets.iter().map(Target::choice).collect();
        rows.push((Field::Where, "start in", places, self.target));
        rows
    }

    pub fn is_new_worktree(&self) -> bool {
        matches!(self.target(), Target::NewWorktree { .. })
    }

    /// The new worktree's branch: the name crystal made up, or what was
    /// typed or given instead.
    pub fn branch_name(&self) -> String {
        self.branch.text().trim().to_string()
    }

    /// The task, if what's chosen takes one.
    fn task_text(&self) -> String {
        if self.run().takes_task() {
            self.task.text().trim().to_string()
        } else {
            String::new()
        }
    }

    /// The command line the session runs: what's chosen, as a profile with
    /// the rows' choices, on the task. An empty one is the user's shell.
    pub fn command(&self) -> Vec<String> {
        let Some(mut profile) = self.run().profile() else {
            return Vec::new();
        };
        for (setting, &choice) in self.run().settings().iter().zip(&self.chosen) {
            *profile.choice_mut(setting.kind) = self.value(setting, choice);
        }
        profile.command(&self.task_text())
    }

    /// The chosen profile's description, if it has one.
    pub fn description(&self) -> Option<&str> {
        match self.run() {
            Run::Profile(profile) => profile.description.as_deref(),
            Run::Flow(flow) => flow.description.as_deref(),
            _ => None,
        }
    }

    /// The command as the user would type it: what the panel shows, and
    /// what Ctrl+E starts the command line with.
    pub fn command_line(&self) -> String {
        let command = self.command();
        let quoted: Vec<String> = command.iter().map(|arg| shell::quote(arg)).collect();
        quoted.join(" ")
    }

    pub fn place(&self) -> Place {
        match self.target() {
            Target::Here { dir, .. } => Place::Directory(dir.clone()),
            Target::NewWorktree { base, .. } => Place::NewWorktree {
                branch: self.branch_name(),
                base: base.clone(),
                made_up: self.branch_name() == self.made_up,
            },
            Target::Project { path, .. } => Place::Directory(Some(path.clone())),
            Target::PullRequest { checkout, .. } => Place::PullRequest(checkout.clone()),
        }
    }

    /// Where the session will start, the way the panel's title says it.
    pub fn title(&self) -> String {
        let place = match self.target() {
            Target::Here { label, .. }
            | Target::Project { label, .. }
            | Target::PullRequest { label, .. } => label.clone(),
            Target::NewWorktree { project, .. } => {
                let project = project.as_deref().unwrap_or("this repository");
                let branch = self.branch_name();
                let branch = if branch.is_empty() {
                    "a new branch".to_string()
                } else {
                    branch
                };
                format!("{project} ⎇ {branch}")
            }
        };
        if matches!(self.run(), Run::Flow(_)) {
            format!("New flow run · {place}")
        } else if self.in_background() {
            format!("New background task · {place}")
        } else {
            format!("New session · {place}")
        }
    }

    /// The directory a new worktree goes in, when it's known.
    pub fn worktree_dir(&self) -> Option<PathBuf> {
        let Target::NewWorktree {
            base: Some(base), ..
        } = self.target()
        else {
            return None;
        };
        let branch = self.branch_name();
        (!branch.is_empty()).then(|| git::worktree_dir(base, &branch))
    }

    /// Enter: starts the session, unless a new worktree has no branch yet.
    fn start(&mut self) -> Outcome {
        if self.is_new_worktree() && self.branch_name().is_empty() {
            self.problem = Some("name the new worktree's branch".to_string());
            self.focus = Field::Branch;
            return Outcome::Stay;
        }
        // In the background there's no terminal to type a task into later.
        if self.in_background() && self.task_text().is_empty() {
            self.problem = Some("say what the background task should do".to_string());
            self.focus = Field::Task;
            return Outcome::Stay;
        }
        if let Run::Flow(flow) = self.run() {
            if self.task_text().is_empty() {
                self.problem = Some("say what the flow should do".to_string());
                self.focus = Field::Task;
                return Outcome::Stay;
            }
            return Outcome::StartFlow {
                place: self.place(),
                flow: flow.name.clone(),
                goal: self.task_text(),
                run: self.run().key(),
            };
        }
        Outcome::Start {
            place: self.place(),
            command: self.command(),
            task: self.task_text(),
            run: self.run().key(),
            background: self.in_background(),
            backlog: self.backlog,
        }
    }

    /// What the panel says runs: the command line, or, in the background,
    /// the `claude -p` run it turns into.
    fn runs_line(&self) -> String {
        if let Run::Flow(flow) = self.run() {
            return format!("flow {}: {}", flow.name, flow.chain());
        }
        let line = self.command_line();
        match background_spec(&self.command()) {
            Some(spec) if self.in_background() => {
                let mut words = vec!["claude".to_string(), "-p".to_string()];
                words.extend(spec.args.iter().map(|arg| shell::quote(arg)));
                words.push(shell::quote(&spec.prompt));
                words.join(" ")
            }
            _ => line,
        }
    }
}

/// A background task's run, from the command that would start the agent
/// in a terminal: its options for each run, and its last argument, the
/// task, as the prompt. The `--` before the task is left out: the task
/// puts its own before the prompt.
pub fn background_spec(command: &[String]) -> Option<TaskSpec> {
    let (prompt, mut args) = command.get(1..)?.split_last()?;
    if let Some((last, before)) = args.split_last()
        && last == "--"
    {
        args = before;
    }
    Some(TaskSpec {
        prompt: prompt.clone(),
        args: args.to_vec(),
    })
}

/// `at` moved `by` within `0..count`, going round at the ends.
fn step(at: usize, by: isize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    (at as isize + by).rem_euclid(count as isize) as usize
}

/// What the panel remembers between sessions: earlier tasks, and what was
/// run last, which it picks first next time.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default)]
    pub last_run: Option<String>,
}

impl Memory {
    /// Remembers a session started with `task` by `run`. The same task
    /// typed again moves to the end rather than being kept twice.
    pub fn remember(&mut self, task: &str, run: &str) {
        let task = task.trim();
        if !task.is_empty() {
            self.tasks.retain(|earlier| earlier != task);
            self.tasks.push(task.to_string());
            let extra = self.tasks.len().saturating_sub(HISTORY_LENGTH);
            self.tasks.drain(..extra);
        }
        self.last_run = Some(run.to_string());
    }
}

/// The memory kept as `json`. None, or one that can't be read, is an empty
/// one: forgetting earlier tasks is no reason to stop.
pub fn read_memory(json: Option<&str>) -> Memory {
    json.and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

/// The panel's place over `area`, the panes: as wide as reads well, near
/// the top.
pub fn panel_area(launcher: &Launcher, area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).clamp(40.min(area.width), 100);
    // The panel's room inside: two columns a side, and a row above and
    // below, plus a frame's rows where the theme can't paint a panel.
    let lines = panel_lines(launcher, width.saturating_sub(4)).len() as u16;
    let height = (lines + 4).min(area.height);
    let top = if area.height > height { 1 } else { 0 };
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + top,
        width,
        height,
    )
}

/// Draws the panel over `area`, the panes.
pub fn draw(frame: &mut Frame, launcher: &Launcher, theme: &Theme, area: Rect) {
    // What's behind the panel stays in view, dimmed, so the panel stands
    // out from it without hiding where it came from.
    frame
        .buffer_mut()
        .set_style(area, Style::new().add_modifier(Modifier::DIM));
    let panel = panel_area(launcher, area);
    frame.render_widget(Clear, panel);
    let framed = theme.panel == Color::Reset;
    let block = if framed {
        Block::bordered().border_style(Style::new().fg(theme.rule))
    } else {
        Block::new()
    };
    let block = block.style(Style::new().bg(theme.panel).fg(theme.text));
    // Two columns of room a side, and a row above and below, frame or not.
    let inside = block
        .inner(panel)
        .inner(Margin::new(if framed { 1 } else { 2 }, 1));
    frame.render_widget(block, panel);
    let lines: Vec<Line> = panel_lines(launcher, inside.width)
        .into_iter()
        .map(|line| line.styled(theme))
        .collect();
    frame.render_widget(Paragraph::new(lines), inside);
    if let Some(at) = cursor_position(launcher, inside) {
        frame.set_cursor_position(at);
    }
}

/// A line of the panel, before it's given the theme's colors.
pub struct PanelLine {
    spans: Vec<(String, Ink)>,
}

/// What a piece of text in the panel is, which says its color.
#[derive(Clone, Copy)]
enum Ink {
    Title,
    Text,
    Muted,
    Label,
    FocusedLabel,
    Choice,
    Chosen,
    ChosenHere,
    Problem,
}

impl PanelLine {
    fn new(spans: Vec<(String, Ink)>) -> PanelLine {
        PanelLine { spans }
    }

    fn blank() -> PanelLine {
        PanelLine { spans: Vec::new() }
    }

    /// The text of the line, without its colors: what tests read.
    pub fn text(&self) -> String {
        self.spans.iter().map(|(text, _)| text.as_str()).collect()
    }

    fn styled(self, theme: &Theme) -> Line<'static> {
        let spans = self.spans.into_iter().map(|(text, ink)| {
            let style = match ink {
                Ink::Title => Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
                Ink::Text => Style::new().fg(theme.text),
                Ink::Muted | Ink::Label | Ink::Choice => Style::new().fg(theme.muted),
                Ink::FocusedLabel => Style::new().fg(theme.accent),
                Ink::Chosen => Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
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

/// The panel's lines in a panel `width` wide, top to bottom: its title,
/// the task, the rows of choices, the branch, then what will run.
pub fn panel_lines(launcher: &Launcher, width: u16) -> Vec<PanelLine> {
    let width = usize::from(width).max(10);
    let mut lines = vec![
        PanelLine::new(vec![(launcher.title(), Ink::Title)]),
        PanelLine::blank(),
    ];
    lines.extend(task_lines(launcher, width));
    lines.push(PanelLine::blank());
    for (field, label, choices, chosen) in launcher.choice_rows() {
        lines.push(choice_line(launcher, field, label, &choices, chosen, width));
        // What a profile is for, under the row that chose it.
        if field == Field::Run
            && let Some(description) = launcher.description()
        {
            let room = width.saturating_sub(LABEL_WIDTH);
            lines.push(PanelLine::new(vec![
                (" ".repeat(LABEL_WIDTH), Ink::Muted),
                (cut(description, room), Ink::Muted),
            ]));
        }
    }
    if launcher.is_new_worktree() {
        let focused = launcher.focus() == Field::Branch;
        let branch = launcher.branch_name();
        let shown = if branch.is_empty() && !focused {
            ("what to call it".to_string(), Ink::Muted)
        } else {
            (branch, Ink::Text)
        };
        lines.push(PanelLine::new(vec![label("branch", focused), shown]));
    }
    lines.push(PanelLine::blank());
    let runs = match launcher.runs_line() {
        line if line.is_empty() => "your shell".to_string(),
        line => line,
    };
    lines.push(PanelLine::new(vec![
        ("runs  ".to_string(), Ink::Muted),
        (cut(&runs, width - 6), Ink::Muted),
    ]));
    if let Some(dir) = launcher.worktree_dir() {
        let dir = shell::home_relative(&dir);
        lines.push(PanelLine::new(vec![
            ("in    ".to_string(), Ink::Muted),
            (cut_front(&dir, width - 6), Ink::Muted),
        ]));
    }
    // Text the user didn't type just now: say where it came from, and that
    // Esc won't lose it.
    if launcher.shows_draft() {
        lines.push(PanelLine::new(vec![
            ("draft ".to_string(), Ink::Muted),
            (cut(DRAFT_NOTE, width - 6), Ink::Muted),
        ]));
    }
    if let Some(problem) = launcher.problem() {
        lines.push(PanelLine::new(vec![(problem.to_string(), Ink::Problem)]));
    }
    lines
}

/// What the panel says when it opens on the draft left last time.
const DRAFT_NOTE: &str = "left last time; esc keeps it";

/// The task box: its rows, scrolled to keep the cursor in sight, or a
/// note saying why there's no task to type.
fn task_lines(launcher: &Launcher, width: usize) -> Vec<PanelLine> {
    let run = launcher.run();
    if !run.takes_task() {
        let note = match run {
            Run::Shell => "a shell takes no task; ctrl+e runs any command".to_string(),
            other => format!(
                "{} can't be given a task when it starts; type it once it's open",
                other.label()
            ),
        };
        return vec![PanelLine::new(vec![(cut(&note, width), Ink::Muted)])];
    }
    let task = launcher.task();
    if task.is_empty() {
        let placeholder = match run {
            Run::Flow(_) => "What should the flow do?",
            _ => "What should it do?  (empty: just start it)",
        };
        return vec![PanelLine::new(vec![(cut(placeholder, width), Ink::Muted)])];
    }
    let rows = task.rows(width);
    let (cursor_row, _) = task.cursor_at(width);
    let first = cursor_row.saturating_sub(TASK_ROWS - 1);
    rows.iter()
        .skip(first)
        .take(TASK_ROWS)
        .map(|&row| PanelLine::new(vec![(task.row_text(row), Ink::Text)]))
        .collect()
}

/// A row of choices: its label, then each choice, the chosen one standing
/// out. When they don't all fit, those far from the chosen one go.
fn choice_line(
    launcher: &Launcher,
    field: Field,
    name: &str,
    choices: &[String],
    chosen: usize,
    width: usize,
) -> PanelLine {
    let focused = launcher.focus() == field;
    let mut spans = vec![label(name, focused)];
    let room = width.saturating_sub(LABEL_WIDTH);
    let (from, to) = shown_choices(choices, chosen, room);
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
    PanelLine::new(spans)
}

/// Which choices fit in `room`, as a range that holds the chosen one:
/// from the first while they all fit, or else starting further along.
pub(super) fn shown_choices(choices: &[String], chosen: usize, room: usize) -> (usize, usize) {
    let width = |range: std::ops::Range<usize>| -> usize {
        choices[range]
            .iter()
            .map(|choice| choice.chars().count() + 3)
            .sum::<usize>()
            + 4
    };
    let mut from = 0;
    while from < chosen && width(from..chosen + 1) > room {
        from += 1;
    }
    let mut to = chosen + 1;
    while to < choices.len() && width(from..to + 1) <= room {
        to += 1;
    }
    (from, to)
}

fn label(name: &str, focused: bool) -> (String, Ink) {
    let ink = if focused {
        Ink::FocusedLabel
    } else {
        Ink::Label
    };
    (format!("{name:<LABEL_WIDTH$}"), ink)
}

/// `text`, cut to `width` characters with `…` when it's longer.
pub(super) fn cut(text: &str, width: usize) -> String {
    let text = text.replace('\n', " ");
    if text.chars().count() <= width {
        return text;
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// `path`, cut to `width` from the front when it's longer: the end of a
/// path is what tells it apart.
fn cut_front(path: &str, width: usize) -> String {
    let count = path.chars().count();
    if count <= width {
        return path.to_string();
    }
    let kept: String = path.chars().skip(count + 1 - width.max(1)).collect();
    format!("…{kept}")
}

/// Where the cursor goes in `inside`, the panel's room: in the task box
/// or the branch, when one has the keyboard.
fn cursor_position(launcher: &Launcher, inside: Rect) -> Option<(u16, u16)> {
    let width = usize::from(inside.width).max(10);
    match launcher.focus() {
        Field::Task => {
            let task = launcher.task();
            if !launcher.run().takes_task() {
                return None;
            }
            let (row, column) = task.cursor_at(width);
            let first = row.saturating_sub(TASK_ROWS - 1);
            // Below the title and the blank line under it.
            let y = inside.y + 2 + (row - first) as u16;
            let x = inside.x + column.min(width - 1) as u16;
            Some((x, y))
        }
        Field::Branch => {
            let lines = panel_lines(launcher, inside.width);
            let at = lines
                .iter()
                .position(|line| line.text().starts_with("branch "))?;
            let column = LABEL_WIDTH + launcher.branch_input().cursor();
            Some((inside.x + column as u16, inside.y + at as u16))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(program: &str) -> Run {
        Run::Agent(catalog::find(program).unwrap())
    }

    fn here() -> Target {
        Target::Here {
            dir: Some(PathBuf::from("/code/payments")),
            label: "payments ⌂ main".into(),
        }
    }

    fn worktree() -> Target {
        Target::NewWorktree {
            base: Some(PathBuf::from("/code/payments")),
            project: Some("payments".into()),
        }
    }

    fn launcher(runs: Vec<Run>) -> Launcher {
        Launcher::new(Setup {
            runs,
            run: 0,
            targets: vec![here(), worktree()],
            target: 0,
            history: vec!["first task".into(), "second task".into()],
            codex_models: vec!["gpt-6-luna".into(), "gpt-5.5".into()],
            background: false,
            branch: "brave-otter".into(),
        })
    }

    /// A panel that offers to start Claude Code in the background.
    fn launcher_with_background(runs: Vec<Run>) -> Launcher {
        let mut panel = launcher(runs);
        panel.offers_background = true;
        panel
    }

    #[test]
    fn claude_can_start_in_the_background_with_its_options_for_each_run() {
        let mut panel = launcher_with_background(vec![agent("claude"), agent("codex")]);
        type_text(&mut panel, "fix the tests");
        press(&mut panel, KeyCode::Tab); // run
        press(&mut panel, KeyCode::Tab); // how
        assert_eq!(panel.focus(), Field::How);
        press(&mut panel, KeyCode::Right);
        press(&mut panel, KeyCode::Down); // model
        press(&mut panel, KeyCode::Right);
        assert!(panel.title().starts_with("New background task"));
        assert_eq!(panel.runs_line(), "claude -p --model fable 'fix the tests'");

        let Outcome::Start {
            command,
            background,
            ..
        } = press(&mut panel, KeyCode::Enter)
        else {
            panic!("didn't start");
        };
        assert!(background);
        let spec = background_spec(&command).unwrap();
        assert_eq!(spec.prompt, "fix the tests");
        assert_eq!(spec.args, ["--model", "fable"]);
    }

    #[test]
    fn only_claude_offers_the_background_and_only_with_something_to_do() {
        let mut panel = launcher_with_background(vec![agent("claude"), agent("codex")]);
        assert!(panel.fields().contains(&Field::How));
        press(&mut panel, KeyCode::Tab);
        press(&mut panel, KeyCode::Right); // codex
        assert!(!panel.fields().contains(&Field::How));
        assert!(
            !launcher(vec![agent("claude")])
                .fields()
                .contains(&Field::How)
        );

        let mut panel = launcher_with_background(vec![agent("claude")]);
        press(&mut panel, KeyCode::Tab);
        press(&mut panel, KeyCode::Tab);
        press(&mut panel, KeyCode::Right);
        assert_eq!(press(&mut panel, KeyCode::Enter), Outcome::Stay);
        assert_eq!(panel.focus(), Field::Task);
        assert!(panel.problem().unwrap().contains("background task"));
    }

    /// A panel at `targets`, opened to start at the one at `target`, the
    /// way `n` (0) and `w` (1) open it.
    fn opened_at(runs: Vec<Run>, targets: Vec<Target>, target: usize) -> Launcher {
        Launcher::new(Setup {
            runs,
            run: 0,
            targets,
            target,
            history: Vec::new(),
            codex_models: Vec::new(),
            background: false,
            branch: "calm-heron".into(),
        })
    }

    /// The choice made in the row called `label`.
    fn chosen(panel: &Launcher, label: &str) -> String {
        let rows = panel.choice_rows();
        let (_, _, choices, at) = rows.iter().find(|row| row.1 == label).unwrap();
        choices[*at].clone()
    }

    /// A panel by `n`, with a task, Claude Code on fable, starting in a new
    /// worktree on a branch typed in, put away with the keys on the branch.
    fn drafted() -> Draft {
        let mut panel = launcher(vec![agent("claude"), agent("codex")]).with_draft(None);
        type_text(&mut panel, "fix the refunds");
        press(&mut panel, KeyCode::Tab); // run
        press(&mut panel, KeyCode::Tab); // model
        press(&mut panel, KeyCode::Right);
        press(&mut panel, KeyCode::BackTab); // run
        press(&mut panel, KeyCode::BackTab); // task
        press(&mut panel, KeyCode::BackTab); // start in, going round
        assert_eq!(panel.focus(), Field::Where);
        press(&mut panel, KeyCode::Right);
        press(&mut panel, KeyCode::Tab);
        type_text(&mut panel, "-2");
        assert_eq!(press(&mut panel, KeyCode::Esc), Outcome::Cancel);
        panel.draft().expect("a draft")
    }

    #[test]
    fn a_draft_brings_the_panel_back_whole_where_it_was_opened_before() {
        let panel = launcher(vec![agent("claude"), agent("codex")]).with_draft(Some(drafted()));
        assert_eq!(panel.task().text(), "fix the refunds");
        assert_eq!(panel.run().key(), "claude");
        assert_eq!(chosen(&panel, "model"), "fable");
        assert!(panel.is_new_worktree());
        assert_eq!(panel.branch_name(), "brave-otter-2");
        assert_eq!(panel.focus(), Field::Branch);
        let lines: Vec<String> = panel_lines(&panel, 80)
            .iter()
            .map(PanelLine::text)
            .collect();
        assert!(
            lines.contains(&"draft left last time; esc keeps it".to_string()),
            "{lines:?}"
        );
    }

    #[test]
    fn a_draft_opened_elsewhere_keeps_what_was_chosen_but_starts_where_it_s_opened() {
        let billing = vec![
            Target::Here {
                dir: Some(PathBuf::from("/code/billing")),
                label: "billing ⌂ main".into(),
            },
            Target::NewWorktree {
                base: Some(PathBuf::from("/code/billing")),
                project: Some("billing".into()),
            },
        ];
        let runs = vec![agent("claude"), agent("codex")];
        let panel = opened_at(runs.clone(), billing, 0).with_draft(Some(drafted()));
        assert_eq!(panel.task().text(), "fix the refunds");
        assert_eq!(chosen(&panel, "model"), "fable");
        assert_eq!(chosen(&panel, "start in"), "here");
        assert_eq!(panel.focus(), Field::Task, "there's no branch row here");

        // `w` where `n` was: a new worktree, on a branch of its own.
        let panel = opened_at(runs, vec![here(), worktree()], 1).with_draft(Some(drafted()));
        assert!(panel.is_new_worktree());
        assert_eq!(panel.branch_name(), "calm-heron");
        assert_eq!(chosen(&panel, "model"), "fable");
    }

    #[test]
    fn a_draft_whose_agent_has_gone_brings_back_its_task() {
        let mut draft = drafted();
        draft.run = "gone".into();
        let panel = launcher(vec![agent("codex"), agent("claude")]).with_draft(Some(draft));
        assert_eq!(panel.task().text(), "fix the refunds");
        assert_eq!(panel.run().key(), "codex");
    }

    #[test]
    fn only_a_panel_with_a_task_written_leaves_a_draft() {
        let mut panel = launcher(vec![agent("claude")]).with_draft(None);
        assert!(panel.keeps_draft());
        type_text(&mut panel, "  ");
        assert_eq!(panel.draft(), None);
        type_text(&mut panel, "go");
        assert_eq!(panel.draft().unwrap().task, "  go");
        // Opened for an issue or a backlog item, it brings its own task.
        assert!(
            !launcher(vec![agent("claude")])
                .with_task("Fix issue #4")
                .keeps_draft()
        );
        // A panel with its draft emptied says nothing about one.
        let mut panel = launcher(vec![agent("claude")]).with_draft(Some(drafted()));
        assert!(panel.shows_draft());
        press(&mut panel, KeyCode::Tab); // from the branch round to the task
        for _ in 0..3 {
            panel.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        }
        assert!(!panel.shows_draft());
    }

    #[test]
    fn a_panel_for_a_backlog_item_says_which_item_it_is_for() {
        let mut panel = launcher(vec![agent("claude")])
            .with_task("write the docs")
            .with_branch("3-write-the-docs")
            .for_backlog_item(3);
        let Outcome::Start { backlog, task, .. } = press(&mut panel, KeyCode::Enter) else {
            panic!("didn't start");
        };
        assert_eq!((backlog, task.as_str()), (Some(3), "write the docs"));
    }

    fn press(launcher: &mut Launcher, code: KeyCode) -> Outcome {
        launcher.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_text(launcher: &mut Launcher, text: &str) {
        for c in text.chars() {
            press(launcher, KeyCode::Char(c));
        }
    }

    fn started(outcome: Outcome) -> (Place, Vec<String>) {
        match outcome {
            Outcome::Start { place, command, .. } => (place, command),
            other => panic!("didn't start: {other:?}"),
        }
    }

    #[test]
    fn claude_gets_the_task_as_one_argument() {
        let mut panel = launcher(vec![agent("claude"), Run::Shell]);
        type_text(&mut panel, "fix the flaky test");
        let (place, command) = started(press(&mut panel, KeyCode::Enter));
        assert_eq!(command, ["claude", "--", "fix the flaky test"]);
        assert_eq!(
            place,
            Place::Directory(Some(PathBuf::from("/code/payments")))
        );
    }

    #[test]
    fn each_agent_takes_its_first_prompt_its_own_way() {
        let cases = [
            ("gemini", vec!["gemini", "-i", "go"]),
            ("opencode", vec!["opencode", "--prompt", "go"]),
            ("cursor-agent", vec!["cursor-agent", "--", "go"]),
            ("aider", vec!["aider"]),
        ];
        for (program, expected) in cases {
            let mut panel = launcher(vec![agent(program)]);
            panel.task.set_text("go");
            assert_eq!(panel.command(), expected, "{program}");
        }
    }

    #[test]
    fn a_choice_other_than_the_default_adds_its_option() {
        let mut panel = launcher(vec![agent("claude")]);
        type_text(&mut panel, "plan it");
        press(&mut panel, KeyCode::Tab); // run
        press(&mut panel, KeyCode::Tab); // model
        assert_eq!(panel.focus(), Field::Setting(0));
        press(&mut panel, KeyCode::Right);
        press(&mut panel, KeyCode::Down); // effort
        press(&mut panel, KeyCode::Left); // round to the last: max
        press(&mut panel, KeyCode::Down); // permissions
        press(&mut panel, KeyCode::Right);
        press(&mut panel, KeyCode::Right);
        assert_eq!(
            panel.command(),
            [
                "claude",
                "--model",
                "fable",
                "--effort",
                "max",
                "--permission-mode",
                "plan",
                "--",
                "plan it"
            ]
        );
        assert_eq!(
            panel.command_line(),
            "claude --model fable --effort max --permission-mode plan -- 'plan it'"
        );
    }

    #[test]
    fn codex_offers_the_models_it_lists() {
        let mut panel = launcher(vec![agent("codex")]);
        press(&mut panel, KeyCode::Tab);
        press(&mut panel, KeyCode::Tab);
        press(&mut panel, KeyCode::Right);
        assert_eq!(panel.command(), ["codex", "-m", "gpt-6-luna"]);
        // Going round from the first comes back to the default.
        press(&mut panel, KeyCode::Left);
        press(&mut panel, KeyCode::Left);
        assert_eq!(panel.command(), ["codex", "-m", "gpt-5.5"]);
    }

    #[test]
    fn changing_what_runs_resets_its_rows_and_the_shell_takes_no_task() {
        let mut panel = launcher(vec![agent("claude"), Run::Shell]);
        type_text(&mut panel, "x");
        press(&mut panel, KeyCode::Tab);
        press(&mut panel, KeyCode::Right);
        assert_eq!(panel.run(), &Run::Shell);
        assert_eq!(panel.command(), Vec::<String>::new());
        assert_eq!(panel.fields(), [Field::Run, Field::Where]);
        assert_eq!(panel.command_line(), "");
    }

    #[test]
    fn tab_goes_round_the_fields_and_back() {
        let mut panel = launcher(vec![agent("claude")]);
        let mut seen = vec![panel.focus()];
        for _ in 0..5 {
            press(&mut panel, KeyCode::Tab);
            seen.push(panel.focus());
        }
        assert_eq!(
            seen,
            [
                Field::Task,
                Field::Run,
                Field::Setting(0),
                Field::Setting(1),
                Field::Setting(2),
                Field::Where
            ]
        );
        press(&mut panel, KeyCode::Tab);
        assert_eq!(panel.focus(), Field::Task);
        press(&mut panel, KeyCode::BackTab);
        assert_eq!(panel.focus(), Field::Where);
    }

    #[test]
    fn a_new_worktree_has_a_made_up_name_whatever_the_task() {
        let mut panel = launcher(vec![agent("claude")]);
        type_text(&mut panel, "Fix the flaky refund test!");
        panel.target = 1;
        assert_eq!(panel.branch_name(), "brave-otter");
        assert_eq!(panel.title(), "New session · payments ⎇ brave-otter");
        assert_eq!(
            panel.worktree_dir(),
            Some(PathBuf::from("/code/payments.worktrees/brave-otter"))
        );
        let (place, _) = started(press(&mut panel, KeyCode::Enter));
        assert_eq!(
            place,
            Place::NewWorktree {
                branch: "brave-otter".into(),
                base: Some(PathBuf::from("/code/payments")),
                made_up: true,
            }
        );
    }

    #[test]
    fn a_new_worktrees_branch_can_be_typed_instead() {
        let mut panel = launcher(vec![agent("claude")]);
        panel.target = 1;
        panel.focus = Field::Branch;
        press(&mut panel, KeyCode::Backspace);
        assert_eq!(panel.branch_name(), "brave-otte");
        let (place, _) = started(press(&mut panel, KeyCode::Enter));
        assert_eq!(
            place,
            Place::NewWorktree {
                branch: "brave-otte".into(),
                base: Some(PathBuf::from("/code/payments")),
                made_up: false,
            }
        );
    }

    #[test]
    fn a_new_worktree_whose_name_is_rubbed_out_asks_for_one() {
        let mut panel = launcher(vec![agent("claude")]);
        panel.target = 1;
        panel.focus = Field::Branch;
        panel.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        panel.focus = Field::Task;
        assert_eq!(press(&mut panel, KeyCode::Enter), Outcome::Stay);
        assert_eq!(panel.focus(), Field::Branch);
        assert_eq!(panel.problem(), Some("name the new worktree's branch"));
        type_text(&mut panel, "spike");
        let (place, _) = started(press(&mut panel, KeyCode::Enter));
        assert!(matches!(
            place,
            Place::NewWorktree { branch, made_up: false, .. } if branch == "spike"
        ));
    }

    #[test]
    fn up_brings_back_earlier_tasks_and_down_the_draft() {
        let mut panel = launcher(vec![agent("claude")]);
        type_text(&mut panel, "draft");
        press(&mut panel, KeyCode::Up);
        assert_eq!(panel.task().text(), "second task");
        press(&mut panel, KeyCode::Up);
        press(&mut panel, KeyCode::Up);
        assert_eq!(panel.task().text(), "first task");
        press(&mut panel, KeyCode::Down);
        assert_eq!(panel.task().text(), "second task");
        press(&mut panel, KeyCode::Down);
        assert_eq!(panel.task().text(), "draft");
    }

    #[test]
    fn alt_enter_breaks_the_line_and_a_paste_keeps_its_lines() {
        let mut panel = launcher(vec![agent("claude")]);
        type_text(&mut panel, "one");
        panel.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        panel.on_paste("two\nthree");
        assert_eq!(panel.task().text(), "one\ntwo\nthree");
    }

    #[test]
    fn ctrl_e_hands_over_the_command_line() {
        let mut panel = launcher(vec![agent("claude")]);
        type_text(&mut panel, "fix it");
        let outcome = panel.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(
            outcome,
            Outcome::CommandLine {
                place: Place::Directory(Some(PathBuf::from("/code/payments"))),
                line: "claude -- 'fix it'".into(),
            }
        );
    }

    #[test]
    fn a_profile_fills_the_panel_and_its_choices_can_still_change() {
        let review = Profile {
            name: "review".into(),
            description: Some("A second pair of eyes".into()),
            model: Some("opus".into()),
            effort: Some("high".into()),
            mode: Some("plan".into()),
            prompt: Some("Review the diff.".into()),
            start_in: Some(StartIn::Worktree),
            ..Profile::for_agent("claude")
        };
        let mut panel = launcher(vec![Run::Profile(review), agent("claude")]);
        assert_eq!(panel.run().key(), "profile:review");
        assert_eq!(panel.description(), Some("A second pair of eyes"));
        assert!(panel.is_new_worktree());
        assert_eq!(
            panel.fields(),
            [
                Field::Task,
                Field::Run,
                Field::Setting(0),
                Field::Setting(1),
                Field::Setting(2),
                Field::Where,
                Field::Branch
            ]
        );
        type_text(&mut panel, "Mind the tests.");
        assert_eq!(
            panel.command(),
            [
                "claude",
                "--model",
                "opus",
                "--effort",
                "high",
                "--permission-mode",
                "plan",
                "--",
                "Review the diff.\n\nMind the tests."
            ]
        );

        // Down to the effort row, and one to the right: xhigh.
        press(&mut panel, KeyCode::Tab);
        press(&mut panel, KeyCode::Tab);
        press(&mut panel, KeyCode::Tab);
        press(&mut panel, KeyCode::Right);
        assert_eq!(panel.command()[3..5], ["--effort", "xhigh"]);
        // Down to the permissions row, and one to the left: accept edits.
        press(&mut panel, KeyCode::Tab);
        press(&mut panel, KeyCode::Left);
        assert_eq!(panel.command()[5..7], ["--permission-mode", "acceptEdits"]);
    }

    #[test]
    fn a_codex_profile_s_model_is_there_before_codex_lists_its_models() {
        let fast = Profile {
            name: "fast".into(),
            model: Some("gpt-7".into()),
            ..Profile::for_agent("codex")
        };
        let mut panel = launcher(vec![Run::Profile(fast)]);
        assert_eq!(panel.command(), ["codex", "-m", "gpt-7"]);
        // The list arrives, without that model: it stays chosen.
        panel.set_codex_models(vec!["gpt-6-luna".into()]);
        assert_eq!(panel.command(), ["codex", "-m", "gpt-7"]);
    }

    #[test]
    fn opened_for_a_new_worktree_a_profile_that_starts_here_doesnt_undo_it() {
        let here = Profile {
            name: "here".into(),
            start_in: Some(StartIn::Here),
            ..Profile::for_agent("claude")
        };
        let panel = Launcher::new(Setup {
            runs: vec![Run::Profile(here)],
            run: 0,
            targets: vec![super::tests::here(), worktree()],
            target: 1,
            history: Vec::new(),
            codex_models: Vec::new(),
            background: false,
            branch: "brave-otter".into(),
        });
        assert!(panel.is_new_worktree());
    }

    #[test]
    fn the_memory_keeps_the_last_tasks_once_each() {
        let mut memory = Memory::default();
        memory.remember("a", "claude");
        memory.remember("b", "codex");
        memory.remember("a", "claude");
        memory.remember("  ", "shell");
        assert_eq!(memory.tasks, ["b", "a"]);
        assert_eq!(memory.last_run.as_deref(), Some("shell"));
        for n in 0..150 {
            memory.remember(&n.to_string(), "claude");
        }
        assert_eq!(memory.tasks.len(), HISTORY_LENGTH);
        assert_eq!(memory.tasks.last().unwrap(), "149");
    }

    #[test]
    fn the_panel_shows_what_will_run_and_where() {
        let mut panel = launcher(vec![agent("claude")]);
        type_text(&mut panel, "fix it");
        panel.target = 1;
        let lines: Vec<String> = panel_lines(&panel, 70)
            .iter()
            .map(PanelLine::text)
            .collect();
        assert!(
            lines[0].starts_with("New session · payments ⎇ brave-otter"),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("run          Claude Code")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("branch       brave-otter")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "runs  claude -- 'fix it'"),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l == "in    /code/payments.worktrees/brave-otter"),
            "{lines:?}"
        );
        // Too long for the panel, the path keeps its end.
        let narrow: Vec<String> = panel_lines(&panel, 30)
            .iter()
            .map(PanelLine::text)
            .collect();
        assert!(
            narrow.iter().any(|l| l == "in    …s.worktrees/brave-otter"),
            "{narrow:?}"
        );
    }

    #[test]
    fn a_long_row_keeps_the_chosen_choice_in_sight() {
        let choices: Vec<String> = (0..10).map(|n| format!("choice-{n}")).collect();
        let (from, to) = shown_choices(&choices, 8, 40);
        assert!(from <= 8 && 8 < to, "{from}..{to}");
        assert!(from > 0);
    }

    #[test]
    fn a_flow_takes_a_goal_and_shows_its_steps_where_a_command_would_be() {
        let config = crate::config::from_text(crate::flows::EXAMPLE).unwrap();
        let mut panel = launcher(vec![Run::Flow(config.flows[0].clone()), agent("claude")]);
        assert_eq!(panel.fields(), [Field::Task, Field::Run, Field::Where]);
        assert_eq!(panel.run().label(), "flow: ship");
        assert_eq!(
            panel.runs_line(),
            "flow ship: plan → implement → review → pr"
        );
        let ctrl_e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert_eq!(panel.on_key(ctrl_e), Outcome::Stay);
        assert_eq!(panel.problem(), Some("a flow has no command line to edit"));

        type_text(&mut panel, "add retries");
        assert_eq!(
            press(&mut panel, KeyCode::Enter),
            Outcome::StartFlow {
                place: Place::Directory(Some(PathBuf::from("/code/payments"))),
                flow: "ship".into(),
                goal: "add retries".into(),
                run: "flow:ship".into(),
            }
        );
    }
}
