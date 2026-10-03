//! `crystal flow` and its commands: start a flow run, see how runs stand,
//! and answer one waiting at a gate.

use crate::client;
use crate::config::Config;
use crate::flow_run::{FlowRun, RunState};
use crate::flows;
use crate::protocol::{Request, Response};
use crate::shell;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

/// How often `crystal flow wait` asks how the run stands.
const POLL_EVERY: Duration = Duration::from_millis(250);

/// Starts a run of the flow called `flow` on `goal`, in `cwd`, and prints
/// the run's name; with `wait`, then waits for it as [`wait`] does.
pub fn run(socket: &Path, flow: &str, goal: &str, cwd: PathBuf, wait: bool) -> Result<()> {
    ensure_on()?;
    let run = client::start_flow(socket, flow, goal, cwd)?;
    println!("{run}");
    if wait {
        self::wait(socket, &run, None)?;
    }
    Ok(())
}

/// Lists every run: how far each has got, and what it's for. With `json`,
/// everything about each, for scripts.
pub fn list(socket: &Path, json: bool) -> Result<()> {
    let runs = runs(socket)?;
    if json {
        let listed: Vec<Listed> = runs.iter().map(Listed::from).collect();
        println!("{}", serde_json::to_string_pretty(&listed)?);
        return Ok(());
    }
    if runs.is_empty() {
        print_flows();
        return Ok(());
    }
    let rows: Vec<[String; 7]> = runs
        .iter()
        .map(|run| {
            [
                run.name.clone(),
                run.flow.name.clone(),
                run.state().word().to_string(),
                run.current()
                    .map_or("-".to_string(), |step| run.step_name(step).to_string()),
                run.round.to_string(),
                dollars(run.cost_usd()),
                first_line(&run.goal).to_string(),
            ]
        })
        .collect();
    let header = ["RUN", "FLOW", "STATE", "STEP", "ROUND", "COST", "GOAL"];
    crate::print_table(header, &rows);
    Ok(())
}

/// Prints one run: what it's for, where it runs, and each step, with the
/// first line of what it answered.
pub fn show(socket: &Path, name: &str, json: bool) -> Result<()> {
    let run = find(socket, name)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&Listed::from(&run))?);
        return Ok(());
    }
    println!(
        "{} · {} · {} · round {} · {}",
        run.name,
        run.flow.name,
        describe(&run),
        run.round,
        dollars(run.cost_usd())
    );
    println!("goal  {}", run.goal);
    let mut place = shell::home_relative(&run.cwd);
    if let Some(worktree) = &run.worktree {
        place.push_str(&format!(", then {}", shell::home_relative(worktree)));
    }
    println!("in    {place}");
    println!();
    let rows: Vec<[String; 6]> = run
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            [
                run.step_name(index).to_string(),
                step.state.word().to_string(),
                step.session.clone().unwrap_or_else(|| "-".to_string()),
                step.runs.to_string(),
                dollars(step.cost_usd),
                step.answer.as_deref().map_or("", first_line).to_string(),
            ]
        })
        .collect();
    let header = ["STEP", "STATE", "SESSION", "RUNS", "COST", "ANSWER"];
    crate::print_table(header, &rows);
    Ok(())
}

/// Goes on past the gate the run called `name` waits at.
pub fn approve(socket: &Path, name: &str) -> Result<()> {
    change(socket, Request::ApproveFlow { run: name.into() })
}

/// Sends the run called `name` back from its gate, with `notes`.
pub fn back(socket: &Path, name: &str, notes: &str) -> Result<()> {
    let request = Request::SendFlowBack {
        run: name.into(),
        notes: notes.into(),
    };
    change(socket, request)
}

/// Runs the step that stopped the run called `name` again.
pub fn retry(socket: &Path, name: &str) -> Result<()> {
    change(socket, Request::RetryFlow { run: name.into() })
}

/// Waits until the run called `name` stops running: it waits at a gate, is
/// done, or stopped at a step that failed or was cut short. Prints which;
/// a step that failed or was cut short is an error, for scripts to see.
/// Gives up after `timeout`, if there is one.
pub fn wait(socket: &Path, name: &str, timeout: Option<Duration>) -> Result<()> {
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    loop {
        let run = find(socket, name)?;
        match run.state() {
            RunState::Running => {}
            RunState::AtGate | RunState::Done => {
                println!("{}", describe(&run));
                return Ok(());
            }
            RunState::Failed | RunState::Interrupted => bail!("{}", describe(&run)),
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let seconds = timeout.unwrap_or_default().as_secs_f64();
            bail!("{name} was still running after {seconds}s");
        }
        thread::sleep(POLL_EVERY);
    }
}

/// Says there are no runs yet, and which flows there are to run: their
/// names and steps, or how to write one.
fn print_flows() {
    let config = Config::load().unwrap_or_default();
    if config.flows.is_empty() {
        println!("no flows yet: `crystal flow example` prints one to copy into your config file");
        return;
    }
    println!("no flow runs yet; `crystal flow run <flow> \"<goal>\"` starts one of these:");
    for flow in &config.flows {
        println!("  {}  {}", flow.name, flow.chain());
    }
}

/// Prints the example flow, to copy into the config file.
pub fn example() {
    print!("{}", flows::EXAMPLE);
}

/// How a run stands, in a few words, with the step that matters.
fn describe(run: &FlowRun) -> String {
    let step = run.current().map(|step| run.step_name(step)).unwrap_or("");
    match run.state() {
        RunState::Running => format!("running {step}"),
        RunState::AtGate => format!("waiting at {step}"),
        RunState::Done => "done".to_string(),
        RunState::Interrupted => format!("interrupted at {step}"),
        RunState::Failed => {
            let why = run
                .current()
                .and_then(|step| run.steps[step].answer.as_deref())
                .map_or("", first_line);
            format!("failed at {step}: {why}")
        }
    }
}

/// A run as `--json` prints it: everything the daemon sends, plus how it
/// stands and the step it's at, which a script would otherwise work out.
#[derive(Serialize)]
struct Listed<'a> {
    #[serde(flatten)]
    run: &'a FlowRun,
    state: &'static str,
    step: Option<&'a str>,
}

impl<'a> From<&'a FlowRun> for Listed<'a> {
    fn from(run: &'a FlowRun) -> Listed<'a> {
        Listed {
            run,
            state: run.state().word(),
            step: run.current().map(|step| run.step_name(step)),
        }
    }
}

fn runs(socket: &Path) -> Result<Vec<FlowRun>> {
    ensure_on()?;
    // Starting the daemon takes up the runs the last one wrote down.
    match client::ask(socket, &Request::ListFlows, true)? {
        Some(Response::Flows { runs }) => Ok(runs),
        _ => bail!("the daemon didn't list the flow runs"),
    }
}

fn find(socket: &Path, name: &str) -> Result<FlowRun> {
    runs(socket)?
        .into_iter()
        .find(|run| run.name == name)
        .with_context(|| format!("there's no flow run called {name}; `crystal flow` lists them"))
}

fn change(socket: &Path, request: Request) -> Result<()> {
    ensure_on()?;
    if client::ask(socket, &request, false)?.is_none() {
        bail!("no daemon is running on {}", socket.display());
    }
    Ok(())
}

fn ensure_on() -> Result<()> {
    flows::ensure_enabled(&Config::load().unwrap_or_default())
}

fn dollars(amount: f64) -> String {
    format!("${amount:.2}")
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}
