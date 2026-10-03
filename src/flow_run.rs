//! A flow run: one goal going through a flow's steps, and how far it's got.
//!
//! This is the state alone, and how it changes: a step ends, the user goes
//! on past a gate, sends the flow back, or runs a stopped step again. Each
//! change says what to do [`Next`], and the daemon does it: it starts the
//! step's task, or tells the user the flow waits on them. Nothing here
//! starts a process or reads a file, apart from [`load`] and [`save`], so
//! every change is unit-tested.
//!
//! The steps go in order. Those before the current step are done; the
//! current one is running, waiting at its gate, or stopped; those after it
//! are still to come. Sending the flow back makes the step it goes back to
//! the current one again, and the steps from there on still to come.

use crate::flows::{self, Flow};
use crate::profile::Profile;
use crate::protocol::TaskSpec;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// How many runs that have finished, done or failed, are kept for `crystal
/// flow` to list. Older ones are let go as new ones start.
const FINISHED_KEPT: usize = 50;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowRun {
    /// The flow's name and a number: `ship-1`, `ship-2`…
    pub name: String,
    /// The flow as it was when the run started, so that changing the
    /// config file doesn't change a run halfway.
    pub flow: Flow,
    /// The profiles its steps run with, as they were then too.
    pub profiles: Vec<Profile>,
    /// What the user asked for: `{goal}` in each step's prompt.
    pub goal: String,
    /// The directory it was started in.
    pub cwd: PathBuf,
    /// The worktree it made for its steps, once a step has wanted one.
    #[serde(default)]
    pub worktree: Option<PathBuf>,
    /// 1 to start with, and one more each time the flow is sent back.
    pub round: u32,
    /// What the last send back said: `{feedback}` in each step's prompt
    /// from then on.
    #[serde(default)]
    pub feedback: Option<String>,
    /// How each of the flow's steps stands, in the same order.
    pub steps: Vec<StepRun>,
    /// When it started, in seconds since the Unix epoch.
    pub started: u64,
    /// The environment its steps' tasks start with: the client's that
    /// started the run, or after a restart, the daemon's. Never written
    /// down or sent anywhere, since it can hold secrets.
    #[serde(skip)]
    pub env: BTreeMap<String, String>,
}

/// How one step of a run stands.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StepRun {
    pub state: StepState,
    /// The session it runs in, a task, by name, once it has started.
    #[serde(default)]
    pub session: Option<String>,
    /// What it was last asked, which running it again asks again.
    #[serde(default)]
    pub prompt: Option<String>,
    /// What Claude answered at the end of its last run, or why the step
    /// failed: `{previous}` for the step after it.
    #[serde(default)]
    pub answer: Option<String>,
    /// How many times its task has run for it.
    #[serde(default)]
    pub runs: u32,
    /// What its runs cost, in US dollars, as Claude counts it.
    #[serde(default)]
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    /// Still to come.
    #[default]
    Pending,
    Running,
    /// Done, and stopped at its gate until the user goes on or sends the
    /// flow back.
    AtGate,
    Done,
    Failed,
    /// It was running when the daemon stopped, so its run was cut short.
    Interrupted,
}

impl StepState {
    /// A word for it, the way `crystal flow show` shows it.
    pub fn word(self) -> &'static str {
        match self {
            StepState::Pending => "to come",
            StepState::Running => "running",
            StepState::AtGate => "waiting",
            StepState::Done => "done",
            StepState::Failed => "failed",
            StepState::Interrupted => "interrupted",
        }
    }
}

/// How a whole run stands, from how its steps do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Running,
    AtGate,
    Done,
    Failed,
    Interrupted,
}

impl RunState {
    /// A word for it, the way `crystal flow` shows it.
    pub fn word(self) -> &'static str {
        match self {
            RunState::Running => "running",
            RunState::AtGate => "waiting",
            RunState::Done => "done",
            RunState::Failed => "failed",
            RunState::Interrupted => "interrupted",
        }
    }
}

/// What a run needs done once it has changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Run step `step`, asking it `prompt`.
    Run { step: usize, prompt: String },
    /// Step `step` is done and waits at its gate: tell the user.
    Gate(usize),
    /// Every step is done.
    Finished,
    /// A step failed, which stops the run.
    Stopped,
}

/// How a step's run ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Ended {
    pub failed: bool,
    /// Claude's answer, or what went wrong.
    pub answer: String,
    pub cost_usd: f64,
}

impl FlowRun {
    /// A run of `flow` on `goal`, from `cwd`, before its first step starts.
    /// `profiles` are those in the config file; the run keeps the ones its
    /// steps name.
    pub fn new(
        name: String,
        flow: Flow,
        profiles: &[Profile],
        goal: String,
        cwd: PathBuf,
        env: BTreeMap<String, String>,
        started: u64,
    ) -> FlowRun {
        let profiles = profiles
            .iter()
            .filter(|profile| {
                let wanted = |step: &flows::Step| step.profile.as_ref() == Some(&profile.name);
                flow.steps.iter().any(wanted)
            })
            .cloned()
            .collect();
        FlowRun {
            name,
            steps: vec![StepRun::default(); flow.steps.len()],
            flow,
            profiles,
            goal,
            cwd,
            worktree: None,
            round: 1,
            feedback: None,
            started,
            env,
        }
    }

    pub fn state(&self) -> RunState {
        let any = |state: StepState| self.steps.iter().any(|step| step.state == state);
        if any(StepState::Failed) {
            RunState::Failed
        } else if any(StepState::Interrupted) {
            RunState::Interrupted
        } else if any(StepState::AtGate) {
            RunState::AtGate
        } else if self.steps.iter().all(|step| step.state == StepState::Done) {
            RunState::Done
        } else {
            RunState::Running
        }
    }

    /// The step the run is at: the first that isn't done. `None` once they
    /// all are.
    pub fn current(&self) -> Option<usize> {
        self.steps
            .iter()
            .position(|step| step.state != StepState::Done)
    }

    /// The step running now, if one is.
    pub fn running(&self) -> Option<usize> {
        self.current()
            .filter(|&step| self.steps[step].state == StepState::Running)
    }

    pub fn step_name(&self, step: usize) -> &str {
        &self.flow.steps[step].name
    }

    /// The step called `name`, by its place in the flow.
    pub fn step_called(&self, name: &str) -> Option<usize> {
        self.flow.steps.iter().position(|step| step.name == name)
    }

    /// What the session of `step` is called, unless that's taken: the run's
    /// name and the step's, `ship-1-plan`.
    pub fn session_name(&self, step: usize) -> String {
        format!("{}-{}", self.name, self.step_name(step))
    }

    /// What every step's runs have cost so far.
    pub fn cost_usd(&self) -> f64 {
        self.steps.iter().map(|step| step.cost_usd).sum()
    }

    /// Starts the run at its first step.
    pub fn start(&mut self) -> Next {
        self.run_step(0, false)
    }

    /// Takes how the run of `step` ended: a failure stops the run; a step
    /// with a gate waits there; any other goes on to the next step.
    pub fn step_ended(&mut self, step: usize, ended: Ended) -> Next {
        let gate = self.flow.steps[step].gate;
        let run = &mut self.steps[step];
        run.runs += 1;
        run.cost_usd += ended.cost_usd;
        run.answer = Some(ended.answer.trim().to_string());
        if ended.failed {
            run.state = StepState::Failed;
            return Next::Stopped;
        }
        if gate {
            run.state = StepState::AtGate;
            return Next::Gate(step);
        }
        run.state = StepState::Done;
        self.go_on()
    }

    /// `step` couldn't be started, for the reason `why`: it fails, and that
    /// stops the run.
    pub fn could_not_start(&mut self, step: usize, why: String) {
        let run = &mut self.steps[step];
        run.state = StepState::Failed;
        run.answer = Some(why);
    }

    /// The user goes on past the gate the run waits at.
    pub fn approve(&mut self) -> Result<Next> {
        let step = self.at_gate()?;
        self.steps[step].state = StepState::Done;
        Ok(self.go_on())
    }

    /// The user sends the flow back from the gate it waits at, with
    /// `notes`: the step its `back_to` names runs again, or this one when
    /// it names none, with `{feedback}` saying so. The steps from there on
    /// are to come again, in a new round.
    pub fn send_back(&mut self, notes: &str) -> Result<Next> {
        let from = self.at_gate()?;
        let back_to = match &self.flow.steps[from].back_to {
            Some(name) => self.step_called(name).unwrap_or(from),
            None => from,
        };
        // Going back to another step, it needs to hear what this one said;
        // a step run again carries its own answer in its conversation.
        let answer = (back_to != from)
            .then(|| self.steps[from].answer.clone())
            .flatten();
        self.feedback = Some(feedback(notes, self.step_name(from), answer.as_deref()));
        for step in &mut self.steps[back_to..=from] {
            step.state = StepState::Pending;
        }
        self.round += 1;
        Ok(self.run_step(back_to, true))
    }

    /// Runs the step that stopped again, failed or cut short, asking it
    /// what it was asked last time.
    pub fn retry(&mut self) -> Result<Next> {
        let Some(step) = self.current() else {
            bail!("{} is done", self.name);
        };
        let state = self.steps[step].state;
        if !matches!(state, StepState::Failed | StepState::Interrupted) {
            bail!(
                "{} hasn't stopped: only a failed or interrupted step runs again",
                self.name
            );
        }
        let prompt = match self.steps[step].prompt.clone() {
            Some(prompt) => prompt,
            None => self.prompt_for(step, false),
        };
        self.steps[step].state = StepState::Running;
        Ok(Next::Run { step, prompt })
    }

    /// The daemon has just started again: a step that was running then had
    /// its run cut short.
    pub fn interrupt(&mut self) {
        for step in &mut self.steps {
            if step.state == StepState::Running {
                step.state = StepState::Interrupted;
            }
        }
    }

    /// Whether `step` runs in the run's worktree: it, or a step before it,
    /// asks for one.
    pub fn wants_worktree(&self, step: usize) -> bool {
        self.flow.steps[..=step].iter().any(|step| step.worktree)
    }

    /// The branch the run's worktree is on: the goal's words, or the run's
    /// name when the goal has none that fit a branch.
    pub fn branch(&self) -> String {
        let branch = crate::tui::launcher::branch_from_task(&self.goal);
        if branch.is_empty() {
            self.name.clone()
        } else {
            branch
        }
    }

    /// The `claude -p` arguments and first prompt of the task for `step`,
    /// which is asked `prompt`: its profile's options, and the profile's
    /// own prompt ahead of the step's.
    pub fn task_spec(&self, step: usize, prompt: &str) -> TaskSpec {
        let wanted = self.flow.steps[step].profile.as_ref();
        let profile = self
            .profiles
            .iter()
            .find(|profile| Some(&profile.name) == wanted)
            .cloned()
            .unwrap_or_else(|| Profile::for_agent("claude"));
        let prompt = match profile.prompt.as_deref().map(str::trim) {
            Some(asks) if !asks.is_empty() => format!("{asks}\n\n{prompt}"),
            _ => prompt.to_string(),
        };
        // The profile's command with no prompt is the agent and its options.
        let options = Profile {
            prompt: None,
            ..profile
        }
        .command("");
        TaskSpec {
            prompt,
            args: options[1..].to_vec(),
        }
    }

    /// What `step` is asked: its prompt, with the goal, what the step
    /// before it answered, and the feedback from the last send back filled
    /// in. A step the flow was just sent back to hears the feedback even
    /// when its prompt doesn't ask for it.
    pub fn prompt_for(&self, step: usize, sent_back: bool) -> String {
        let template = &self.flow.steps[step].prompt;
        let previous = match step.checked_sub(1) {
            Some(before) => self.steps[before].answer.as_deref().unwrap_or(""),
            None => "",
        };
        let feedback = self.feedback.as_deref().unwrap_or("");
        let values = [
            ("goal", self.goal.as_str()),
            ("previous", previous),
            ("feedback", feedback),
        ];
        let mut prompt = flows::fill(template, &values);
        if sent_back && !template.contains("{feedback}") {
            prompt.push_str("\n\n");
            prompt.push_str(feedback);
        }
        prompt.trim().to_string()
    }

    /// Goes on to the step after the current one, or finishes.
    fn go_on(&mut self) -> Next {
        match self.current() {
            Some(step) => self.run_step(step, false),
            None => Next::Finished,
        }
    }

    fn run_step(&mut self, step: usize, sent_back: bool) -> Next {
        let prompt = self.prompt_for(step, sent_back);
        let run = &mut self.steps[step];
        run.state = StepState::Running;
        run.prompt = Some(prompt.clone());
        Next::Run { step, prompt }
    }

    /// The step waiting at its gate, or an error that says the run isn't.
    fn at_gate(&self) -> Result<usize> {
        match self.current() {
            Some(step) if self.steps[step].state == StepState::AtGate => Ok(step),
            _ => bail!(
                "{} isn't waiting at a gate: it's {}",
                self.name,
                self.state().word()
            ),
        }
    }
}

/// What `{feedback}` says once the flow has been sent back from the step
/// called `from`: the notes, and, going back to an earlier step, what
/// `from` answered.
fn feedback(notes: &str, from: &str, answer: Option<&str>) -> String {
    let notes = notes.trim();
    let mut text = format!("This was sent back at the {from} step");
    if notes.is_empty() {
        text.push('.');
    } else {
        text.push_str(&format!(", with these notes: {notes}"));
    }
    if let Some(answer) = answer {
        text.push_str(&format!("\n\nWhat {from} said:\n{answer}"));
    }
    text
}

/// A name for a new run of `flow`: its name and the next number its runs
/// haven't had.
pub fn new_name(flow: &str, runs: &[FlowRun]) -> String {
    let taken = runs
        .iter()
        .filter_map(|run| run.name.strip_prefix(flow)?.strip_prefix('-')?.parse().ok())
        .max()
        .unwrap_or(0u32);
    format!("{flow}-{}", taken + 1)
}

/// Lets go of the oldest finished runs, past the last [`FINISHED_KEPT`].
/// Runs still going, waiting or stopped part way stay, whatever their age.
pub fn forget_old(runs: &mut Vec<FlowRun>) {
    let finished = |run: &FlowRun| matches!(run.state(), RunState::Done | RunState::Failed);
    let mut extra = runs
        .iter()
        .filter(|run| finished(run))
        .count()
        .saturating_sub(FINISHED_KEPT);
    runs.retain(|run| {
        if extra > 0 && finished(run) {
            extra -= 1;
            false
        } else {
            true
        }
    });
}

/// The runs written down at `path`. A file that's missing or can't be
/// read means there are none.
pub fn load(path: &Path) -> Vec<FlowRun> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save(path: &Path, runs: &[FlowRun]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // Written beside it first, then moved into place in one step, so a
    // crash halfway through never leaves half a file.
    let unfinished = path.with_extension("json.unfinished");
    fs::write(&unfinished, serde_json::to_string_pretty(runs)?)?;
    fs::rename(&unfinished, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flows::Step;

    fn step(name: &str, prompt: &str) -> Step {
        Step {
            name: name.into(),
            profile: None,
            prompt: prompt.into(),
            worktree: false,
            gate: false,
            back_to: None,
        }
    }

    /// plan → implement (in a worktree) → review (a gate that goes back to
    /// implement) → pr.
    fn ship() -> FlowRun {
        let mut implement = step("implement", "Do {goal} like so:\n{previous}\n\n{feedback}");
        implement.worktree = true;
        let mut review = step("review", "Review it: {previous}");
        review.gate = true;
        review.back_to = Some("implement".into());
        let flow = Flow {
            name: "ship".into(),
            description: None,
            steps: vec![
                step("plan", "Plan {goal}"),
                implement,
                review,
                step("pr", "Open a pull request"),
            ],
        };
        FlowRun::new(
            "ship-1".into(),
            flow,
            &[],
            "add retries".into(),
            PathBuf::from("/code/app"),
            BTreeMap::new(),
            0,
        )
    }

    fn done(answer: &str) -> Ended {
        Ended {
            failed: false,
            answer: answer.into(),
            cost_usd: 0.25,
        }
    }

    fn states(run: &FlowRun) -> Vec<StepState> {
        run.steps.iter().map(|step| step.state).collect()
    }

    use StepState::*;

    #[test]
    fn a_run_starts_at_its_first_step_with_the_goal_filled_in() {
        let mut run = ship();
        assert_eq!(
            run.start(),
            Next::Run {
                step: 0,
                prompt: "Plan add retries".into()
            }
        );
        assert_eq!(states(&run), [Running, Pending, Pending, Pending]);
        assert_eq!(run.state(), RunState::Running);
    }

    #[test]
    fn a_step_that_ends_hands_its_answer_to_the_next() {
        let mut run = ship();
        run.start();
        let next = run.step_ended(0, done("change client.rs"));
        assert_eq!(
            next,
            Next::Run {
                step: 1,
                prompt: "Do add retries like so:\nchange client.rs".into()
            }
        );
        assert_eq!(states(&run), [Done, Running, Pending, Pending]);
        assert_eq!(run.steps[0].runs, 1);
    }

    #[test]
    fn a_gated_step_waits_until_the_user_goes_on() {
        let mut run = ship();
        run.start();
        run.step_ended(0, done("the plan"));
        assert_eq!(
            run.step_ended(1, done("built it")),
            Next::Run {
                step: 2,
                prompt: "Review it: built it".into()
            }
        );
        assert_eq!(run.step_ended(2, done("looks good")), Next::Gate(2));
        assert_eq!(run.state(), RunState::AtGate);
        assert_eq!(states(&run), [Done, Done, AtGate, Pending]);

        let next = run.approve().unwrap();
        assert_eq!(
            next,
            Next::Run {
                step: 3,
                prompt: "Open a pull request".into()
            }
        );
        assert_eq!(run.step_ended(3, done("#57")), Next::Finished);
        assert_eq!(run.state(), RunState::Done);
        assert_eq!(run.cost_usd(), 1.0);
    }

    #[test]
    fn going_on_takes_a_run_waiting_at_a_gate() {
        let mut run = ship();
        run.start();
        let err = run.approve().unwrap_err();
        assert_eq!(
            err.to_string(),
            "ship-1 isn't waiting at a gate: it's running"
        );
        assert!(run.send_back("again").is_err());
    }

    #[test]
    fn sending_back_runs_the_back_to_step_again_with_the_feedback() {
        let mut run = ship();
        run.start();
        run.step_ended(0, done("the plan"));
        run.step_ended(1, done("built it"));
        run.step_ended(2, done("the timeout is wrong"));

        let Next::Run { step, prompt } = run.send_back("keep the old default").unwrap() else {
            panic!("the flow didn't go back");
        };
        assert_eq!(step, 1);
        assert_eq!(
            prompt,
            "Do add retries like so:\nthe plan\n\n\
             This was sent back at the review step, with these notes: keep the old default\n\n\
             What review said:\nthe timeout is wrong"
        );
        assert_eq!(states(&run), [Done, Running, Pending, Pending]);
        assert_eq!(run.round, 2);

        // Then on through the review again, with the new answer.
        let next = run.step_ended(1, done("fixed it"));
        assert_eq!(
            next,
            Next::Run {
                step: 2,
                prompt: "Review it: fixed it".into()
            }
        );
        assert_eq!(run.steps[1].runs, 2);
    }

    #[test]
    fn a_gate_with_no_back_to_runs_itself_again_and_hears_the_notes_anyway() {
        let mut run = ship();
        run.flow.steps[2].back_to = None;
        run.start();
        run.step_ended(0, done("the plan"));
        run.step_ended(1, done("built it"));
        run.step_ended(2, done("looks good"));
        let next = run.send_back("look at the tests too").unwrap();
        assert_eq!(
            next,
            Next::Run {
                step: 2,
                prompt: "Review it: built it\n\n\
                         This was sent back at the review step, with these notes: \
                         look at the tests too"
                    .into()
            }
        );
        assert_eq!(states(&run), [Done, Done, Running, Pending]);
    }

    #[test]
    fn a_failed_step_stops_the_run_until_it_runs_again() {
        let mut run = ship();
        run.start();
        let failed = Ended {
            failed: true,
            answer: "Invalid API key".into(),
            cost_usd: 0.0,
        };
        assert_eq!(run.step_ended(0, failed), Next::Stopped);
        assert_eq!(run.state(), RunState::Failed);
        assert_eq!(run.steps[0].answer.as_deref(), Some("Invalid API key"));
        assert!(run.approve().is_err());

        // It's asked what it was asked the first time.
        assert_eq!(
            run.retry().unwrap(),
            Next::Run {
                step: 0,
                prompt: "Plan add retries".into()
            }
        );
        assert_eq!(run.state(), RunState::Running);
        assert!(run.retry().is_err());
    }

    #[test]
    fn a_step_cut_short_by_a_restart_is_interrupted_and_runs_again() {
        let mut run = ship();
        run.start();
        run.step_ended(0, done("the plan"));
        run.interrupt();
        assert_eq!(states(&run), [Done, Interrupted, Pending, Pending]);
        assert_eq!(run.state(), RunState::Interrupted);
        let Next::Run { step, prompt } = run.retry().unwrap() else {
            panic!("it didn't run again");
        };
        assert_eq!(step, 1);
        assert!(prompt.contains("the plan"), "{prompt}");
    }

    #[test]
    fn a_step_that_cant_start_fails_and_says_why() {
        let mut run = ship();
        run.start();
        run.could_not_start(0, "command not found: claude".into());
        assert_eq!(run.state(), RunState::Failed);
        assert_eq!(
            run.steps[0].answer.as_deref(),
            Some("command not found: claude")
        );
    }

    #[test]
    fn the_worktree_is_for_the_step_that_wants_it_and_every_one_after() {
        let run = ship();
        let wants: Vec<bool> = (0..4).map(|step| run.wants_worktree(step)).collect();
        assert_eq!(wants, [false, true, true, true]);
        assert_eq!(run.branch(), "add-retries");
    }

    #[test]
    fn a_step_runs_with_its_profiles_options_and_prompt() {
        let mut run = ship();
        run.flow.steps[0].profile = Some("planner".into());
        run.profiles = vec![Profile {
            name: "planner".into(),
            model: Some("opus".into()),
            mode: Some("plan".into()),
            prompt: Some("Think hard.".into()),
            instructions: Some("Change nothing.".into()),
            ..Profile::for_agent("claude")
        }];
        let spec = run.task_spec(0, "Plan add retries");
        assert_eq!(spec.prompt, "Think hard.\n\nPlan add retries");
        assert_eq!(
            spec.args,
            [
                "--model",
                "opus",
                "--permission-mode",
                "plan",
                "--append-system-prompt",
                "Change nothing."
            ]
        );
        // A step with no profile runs Claude Code as it's set up.
        assert_eq!(run.task_spec(1, "x").args, Vec::<String>::new());
    }

    #[test]
    fn a_run_keeps_only_the_profiles_its_steps_name() {
        let mut flow = ship().flow;
        flow.steps[0].profile = Some("planner".into());
        let profiles = [
            Profile {
                name: "planner".into(),
                ..Profile::for_agent("claude")
            },
            Profile {
                name: "other".into(),
                ..Profile::for_agent("codex")
            },
        ];
        let run = FlowRun::new(
            "ship-1".into(),
            flow,
            &profiles,
            String::new(),
            PathBuf::new(),
            BTreeMap::new(),
            0,
        );
        let kept: Vec<&str> = run.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(kept, ["planner"]);
        // A goal with no words for a branch has the run's name for one.
        assert_eq!(run.branch(), "ship-1");
    }

    #[test]
    fn a_new_run_takes_the_next_number_of_its_flow() {
        let mut first = ship();
        assert_eq!(new_name("ship", &[]), "ship-1");
        first.name = "ship-2".into();
        let mut other = ship();
        other.name = "shipping-7".into();
        assert_eq!(new_name("ship", &[first, other]), "ship-3");
    }

    #[test]
    fn only_the_newest_finished_runs_are_kept() {
        let finished = |number: usize| {
            let mut run = ship();
            run.name = format!("ship-{number}");
            for step in &mut run.steps {
                step.state = Done;
            }
            run
        };
        let mut waiting = ship();
        waiting.start();
        let mut runs = vec![waiting];
        runs.extend((1..=FINISHED_KEPT + 2).map(finished));
        forget_old(&mut runs);
        assert_eq!(runs.len(), FINISHED_KEPT + 1);
        assert_eq!(runs[0].state(), RunState::Running);
        assert_eq!(runs[1].name, "ship-3");
    }

    #[test]
    fn runs_are_written_down_without_their_environment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flows.json");
        let mut run = ship();
        run.env.insert("TOKEN".into(), "secret".into());
        run.start();
        save(&path, std::slice::from_ref(&run)).unwrap();
        assert!(!fs::read_to_string(&path).unwrap().contains("secret"));
        let loaded = load(&path);
        run.env.clear();
        assert_eq!(loaded, [run]);
        assert!(load(&dir.path().join("none.json")).is_empty());
    }
}
