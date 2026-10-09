//! Writing a wiki: `crystal wiki build` and `crystal wiki update`.
//!
//! A build makes a clean checkout of the default branch's tip, lists its
//! files and indexes what they define, then has Claude plan the outline,
//! write each subsection (`concurrency` at once), each section's summary
//! from its subsections, and the overview from the sections. Each text is
//! checked before it's kept (see [`super::check`]), sent back once to its
//! writer with what's wrong, then linked; the wiki is written whole at the
//! end, and what the build wrote is kept as it goes in `build.json`, so a
//! build stopped halfway carries on where it was.
//!
//! An update writes again only the subsections whose files changed since
//! the wiki's commit, gives each its text before and the diff, and the
//! summaries and the overview only where a writer says the meaning moved.
//! It plans again when much changed, or when many source files came that
//! no subsection covers, keeping every subsection whose files are as they
//! were. The links of what isn't written again are moved with the lines
//! they point at.

use super::About;
use super::book::{self, Book, Built, Done, Kind, Page, Phase, Run, Summary};
use super::check::{self, Tally};
use super::claude::{self, Ask};
use super::crystal;
use super::crystal::WikiSettings;
use super::files::Files;
use super::index::{DefKind, Index, IndexSettings};
use super::model::{self, CodeLink, Generated, Overview, Section, Subsection, Wiki};
use super::plan::{self, Plan};
use super::prose;
use super::repo::{self, Remap};
use super::write::{self, Written};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// What one run of the planner may spend, take and last.
const PLAN_BUDGET: f64 = 4.0;
const PLAN_TURNS: u32 = 80;
const PLAN_TIMEOUT: Duration = Duration::from_secs(25 * 60);

/// What one subsection's writer may spend, take and last.
const WRITE_BUDGET: f64 = 1.5;
const WRITE_TURNS: u32 = 30;
const WRITE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// What a writer asked to put its text right may.
const FIX_BUDGET: f64 = 0.6;
const FIX_TURNS: u32 = 16;
const FIX_TIMEOUT: Duration = Duration::from_secs(8 * 60);

/// What a section's summary writer may.
const SECTION_BUDGET: f64 = 0.8;
const SECTION_TURNS: u32 = 14;
const SECTION_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// What the overview's writer may.
const OVERVIEW_BUDGET: f64 = 1.2;
const OVERVIEW_TURNS: u32 = 18;

/// The least a run of Claude is started with: below it, the build stops
/// for its budget.
const LEAST_CALL: f64 = 0.15;

/// How much of its files a writer is given, numbered, in bytes: it reads
/// the rest itself.
const INLINE: usize = 120 * 1024;

/// The longest line of a file a writer is given.
const LONGEST_LINE: usize = 400;

/// How much of the diff of its files an update's writer is given.
const DIFF_SHOWN: usize = 40 * 1024;

/// How many of the commits that touched its files a writer is shown.
const COMMITS_SHOWN: usize = 15;

/// The most definitions a writer is shown the lines of.
const DEFS_SHOWN: usize = 600;

/// An update plans again when more than this share of the subsections
/// changed.
const REPLAN_SHARE: f64 = 0.5;

/// Or when more source files than this came that no subsection covers.
const REPLAN_UNCOVERED: usize = 12;

/// What a build is asked to do.
#[derive(Debug, Clone)]
pub struct Options {
    pub kind: Kind,
    /// Start over, forgetting a build that stopped before it ended.
    pub fresh: bool,
    /// The most it may spend, in place of the settings'.
    pub budget_usd: Option<f64>,
    /// Fetch the default branch from `origin` first.
    pub fetch: bool,
}

/// How a build went.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The wiki written, or `None` when it was up to date.
    pub wiki: Option<Wiki>,
    pub commit: String,
    pub written: usize,
    pub missing: usize,
    pub cost_usd: f64,
    pub seconds: u64,
    pub tally: Tally,
}

/// Where a wiki's files are.
struct Paths {
    dir: PathBuf,
    wiki: PathBuf,
    book: PathBuf,
    checkout: PathBuf,
}

/// What the threads of a build share: the book, written down as it
/// changes, the money, the log.
struct Shared<'a> {
    state: Mutex<State>,
    book_path: &'a Path,
    log: Mutex<Option<File>>,
    say: &'a (dyn Fn(&str) + Sync),
    settings: &'a WikiSettings,
    checkout: &'a Path,
}

struct State {
    book: Book,
    /// The most this invocation may spend.
    limit: f64,
    /// What it has spent.
    spent: f64,
    /// What the runs of Claude under way may still spend.
    reserved: f64,
    /// Why it stopped starting runs, once it has.
    stopped: Option<String>,
}

/// Builds or updates the wiki of the project whose main worktree is
/// `project`, for the daemon at `socket`, saying how it goes with `say`.
pub fn run(
    socket: &Path,
    project: &Path,
    settings: &WikiSettings,
    options: &Options,
    say: &(dyn Fn(&str) + Sync),
) -> Result<Outcome> {
    let dir = super::dir(socket, project);
    let paths = Paths {
        wiki: dir.join(super::WIKI_FILE),
        book: dir.join(super::BUILD_FILE),
        checkout: super::checkout_dir(&dir),
        dir,
    };
    let _lock = book::lock(&paths.dir)?;
    let started = Instant::now();
    let mut book = Book::read(&paths.book)?;
    let old = Wiki::read(&paths.wiki).ok();
    let tip = repo::default_tip(project, options.fetch)?;
    let run = match book.run.take() {
        Some(run) if !options.fresh => {
            say(&format!(
                "carrying on with the {} of {} stopped at {}",
                kind_word(run.kind),
                short(&run.commit),
                run.progress
            ));
            run
        }
        _ => new_run(options.kind, &tip),
    };
    let mut run = run;
    if run.kind == Kind::Update && (book.built.is_none() || old.is_none()) {
        run.kind = Kind::Build;
    }
    if run.kind == Kind::Update
        && run.plan.is_none()
        && let Some(built) = &book.built
        && built.commit == run.commit
        && built.missing.is_empty()
    {
        say(&format!(
            "the wiki is up to date with {} at {}",
            tip.branch,
            short(&run.commit)
        ));
        return Ok(Outcome {
            wiki: None,
            commit: run.commit,
            written: 0,
            missing: 0,
            cost_usd: 0.0,
            seconds: 0,
            tally: Tally::default(),
        });
    }
    run.pid = std::process::id();
    repo::prepare(project, &paths.checkout, &run.commit)?;
    let listed = Files::list(&paths.checkout, &settings.exclude)?;
    let index = Index::build(
        &paths.checkout,
        &run.commit,
        &paths.dir.join("index"),
        &IndexSettings {
            exclude: settings.exclude.clone(),
        },
    )?;
    let budget = options.budget_usd.unwrap_or(settings.budget_usd);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.dir.join(super::LOG_FILE))
        .ok();
    let name = repo_name(project);
    tell(socket, project, &run, |about| about, false);
    book.run = Some(run);
    book.building = true;
    let shared = Shared {
        state: Mutex::new(State {
            book,
            limit: budget,
            spent: 0.0,
            reserved: 0.0,
            stopped: None,
        }),
        book_path: &paths.book,
        log: Mutex::new(log),
        say,
        settings,
        checkout: &paths.checkout,
    };
    shared.log(&format!(
        "started: {} of {} at {}, budget ${budget:.2}",
        kind_word(shared.run(|run| run.kind)),
        project.display(),
        shared.run(|run| run.commit.clone())
    ));
    shared.save();
    let result = build(
        &shared,
        project,
        &name,
        &listed,
        &index,
        old.as_ref(),
        &paths,
    );
    let seconds = started.elapsed().as_secs();
    let mut state = shared.state.into_inner().expect("no thread panicked");
    state.book.building = false;
    match result {
        Ok((wiki, written, missing)) => {
            let run = state.book.run.take().expect("the run is there");
            state.book.remember(Done {
                kind: run.kind,
                commit: run.commit.clone(),
                started: run.started.clone(),
                ended: book::utc_now(),
                seconds,
                cost_usd: state.spent,
                written,
                subsections: wiki.subsections(),
                tally: run.tally,
                failed: None,
            });
            state.book.write(&paths.book)?;
            tell(
                socket,
                project,
                &run,
                |about| About {
                    sections: wiki.sections.len(),
                    subsections: wiki.subsections(),
                    written,
                    cost_usd: state.spent,
                    seconds,
                    ..about
                },
                true,
            );
            Ok(Outcome {
                commit: run.commit,
                wiki: Some(wiki),
                written,
                missing,
                cost_usd: state.spent,
                seconds,
                tally: run.tally,
            })
        }
        Err(err) => {
            let why = format!("{err:#}");
            if let Some(run) = &mut state.book.run {
                run.pid = 0;
                let failed = Done {
                    kind: run.kind,
                    commit: run.commit.clone(),
                    started: run.started.clone(),
                    ended: book::utc_now(),
                    seconds,
                    cost_usd: state.spent,
                    written: run.pages.len(),
                    subsections: run
                        .plan
                        .as_ref()
                        .map_or(0, |plan| plan.subsections().count()),
                    tally: run.tally,
                    failed: Some(why.clone()),
                };
                let run = run.clone();
                state.book.remember(failed);
                tell(
                    socket,
                    project,
                    &run,
                    |about| About {
                        cost_usd: state.spent,
                        seconds,
                        failed: Some(why.clone()),
                        ..about
                    },
                    true,
                );
            }
            let _ = state.book.write(&paths.book);
            Err(err)
        }
    }
}

/// A new run of `kind` at `tip`.
fn new_run(kind: Kind, tip: &repo::Tip) -> Run {
    Run {
        kind,
        commit: tip.commit.clone(),
        branch: tip.branch.clone(),
        started: book::utc_now(),
        pid: std::process::id(),
        plan: None,
        todo: BTreeSet::new(),
        replanned: false,
        pages: BTreeMap::new(),
        failed: BTreeMap::new(),
        sections: BTreeMap::new(),
        overview: None,
        phase: Phase::Planning,
        progress: "starting".to_string(),
        cost_usd: 0.0,
        tally: Tally::default(),
        models: BTreeMap::new(),
    }
}

/// The build itself, from the plan to the wiki written: the wiki, how
/// many subsections it wrote, and how many couldn't be.
fn build(
    shared: &Shared,
    project: &Path,
    name: &str,
    listed: &Files,
    index: &Index,
    old: Option<&Wiki>,
    paths: &Paths,
) -> Result<(Wiki, usize, usize)> {
    let kind = shared.run(|run| run.kind);
    let commit = shared.run(|run| run.commit.clone());
    let built = shared.state.lock().unwrap().book.built.clone();
    // The outline, and what to write.
    if shared.run(|run| run.plan.is_none()) {
        match (kind, &built) {
            (Kind::Update, Some(built)) => decide_update(shared, name, listed, built, old)?,
            _ => {
                let plan = make_plan(shared, name, listed, &commit)?;
                shared.change(|run| {
                    run.todo = plan.subsections().map(|(_, sub)| sub.id.clone()).collect();
                    run.plan = Some(plan);
                    run.replanned = true;
                });
            }
        }
    }
    let plan = shared.run(|run| run.plan.clone()).expect("planned");
    let anchors = plan.anchors();
    // The subsections.
    let todo: Vec<(plan::PlannedSection, plan::PlannedSubsection)> = plan
        .subsections()
        .filter(|(_, sub)| {
            shared.run(|run| run.todo.contains(&sub.id) && !run.pages.contains_key(&sub.id))
        })
        .map(|(section, sub)| (section.clone(), sub.clone()))
        .collect();
    let total = shared.run(|run| run.todo.len());
    shared.change(|run| {
        run.phase = Phase::Writing;
        run.progress = format!("writing {}/{total} subsections", run.pages.len());
    });
    if !todo.is_empty() {
        shared.say(&format!(
            "writing {} subsections, {} at once",
            todo.len(),
            shared.settings.concurrency
        ));
    }
    let queue = Mutex::new(todo.into_iter().collect::<VecDeque<_>>());
    let old_built = built.as_ref();
    thread::scope(|scope| {
        for _ in 0..shared.settings.concurrency.max(1) {
            scope.spawn(|| {
                loop {
                    let Some((section, sub)) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    if shared.stopped() {
                        break;
                    }
                    let near = sub.files.clone();
                    let check = check::Context {
                        root: paths.checkout.as_path(),
                        project,
                        files: listed,
                        index,
                        anchors: &anchors,
                        near: &near,
                    };
                    let earlier = old.and_then(|wiki| find_subsection(wiki, &sub.id));
                    let started = Instant::now();
                    match write_page(
                        shared, name, &plan, &section, &sub, &check, listed, index, earlier,
                        old_built,
                    ) {
                        Ok((page, tally)) => {
                            let cost = page.cost_usd;
                            let done = shared.change(|run| {
                                run.tally += tally;
                                run.failed.remove(&sub.id);
                                run.pages.insert(sub.id.clone(), page);
                                run.progress =
                                    format!("writing {}/{total} subsections", run.pages.len());
                                run.pages.len()
                            });
                            shared.say(&format!(
                                "[{done}/{total}] {} (${cost:.2}, {}s)",
                                sub.title,
                                started.elapsed().as_secs()
                            ));
                        }
                        Err(err) => {
                            let why = format!("{err:#}");
                            shared.say(&format!("couldn't write {}: {why}", sub.title));
                            shared.change(|run| {
                                run.failed.insert(sub.id.clone(), why);
                            });
                        }
                    }
                }
            });
        }
    });
    if let Some(why) = shared.stopped_why() {
        bail!(
            "{why}: {} of {total} subsections are written, and kept; `crystal wiki build` carries \
             on from there",
            shared.run(|run| run.pages.len())
        );
    }
    // The sections' summaries.
    let sections = sections_to_write(shared, &plan, old);
    let section_total = sections.len();
    shared.change(|run| {
        run.phase = Phase::Summarizing;
        run.progress = format!(
            "summarizing {}/{section_total} sections",
            run.sections.len()
        );
    });
    let queue = Mutex::new(sections.into_iter().collect::<VecDeque<_>>());
    thread::scope(|scope| {
        for _ in 0..shared.settings.concurrency.max(1) {
            scope.spawn(|| {
                loop {
                    let Some(section) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    if shared.stopped() {
                        break;
                    }
                    let near: Vec<String> = section
                        .subsections
                        .iter()
                        .flat_map(|sub| sub.files.iter().cloned())
                        .collect();
                    let check = check::Context {
                        root: paths.checkout.as_path(),
                        project,
                        files: listed,
                        index,
                        anchors: &anchors,
                        near: &near,
                    };
                    let earlier =
                        old.and_then(|wiki| wiki.sections.iter().find(|s| s.id == section.id));
                    match write_section(shared, name, &plan, &section, &check, index, earlier, old)
                    {
                        Ok((summary, tally)) => {
                            let cost = summary.cost_usd;
                            let done = shared.change(|run| {
                                run.tally += tally;
                                run.sections.insert(section.id.clone(), summary);
                                run.progress = format!(
                                    "summarizing {}/{section_total} sections",
                                    run.sections.len()
                                );
                                run.sections.len()
                            });
                            shared.say(&format!(
                                "[{done}/{section_total}] the section {} (${cost:.2})",
                                section.title
                            ));
                        }
                        Err(err) => {
                            shared.say(&format!("couldn't summarize {}: {err:#}", section.title));
                        }
                    }
                }
            });
        }
    });
    if let Some(why) = shared.stopped_why() {
        bail!(
            "{why}: the subsections are written, and kept; `crystal wiki build` carries on from there"
        );
    }
    // The overview.
    if overview_to_write(shared, &plan, old) {
        shared.change(|run| {
            run.phase = Phase::Overview;
            run.progress = "writing the overview".to_string();
        });
        let near = vec![String::new()];
        let check = check::Context {
            root: paths.checkout.as_path(),
            project,
            files: listed,
            index,
            anchors: &anchors,
            near: &near,
        };
        match write_overview(shared, name, &plan, &check, index, old) {
            Ok((summary, tally)) => {
                let cost = summary.cost_usd;
                shared.change(|run| {
                    run.tally += tally;
                    run.overview = Some(summary);
                });
                shared.say(&format!("the overview (${cost:.2})"));
            }
            Err(err) => shared.say(&format!("couldn't write the overview: {err:#}")),
        }
    }
    // The page, put together, and every text checked once more at this
    // commit.
    shared.change(|run| {
        run.phase = Phase::Finishing;
        run.progress = "finishing".to_string();
    });
    let run = shared.run(Clone::clone);
    let built_commit = built.as_ref().map(|built| built.commit.clone());
    let remap = match (&built_commit, run.kind) {
        (Some(before), Kind::Update) if *before != run.commit => {
            Remap::between(&paths.checkout, before, &run.commit).unwrap_or_default()
        }
        _ => Remap::default(),
    };
    let (wiki, missing, tally) = assemble(
        project, name, &run, &plan, old, &remap, listed, index, paths, shared,
    )?;
    shared.change(|run| run.tally += tally);
    let written = run.pages.len();
    // What it was written from, for the next update.
    let blobs = plan
        .subsections()
        .filter(|(_, sub)| !missing.contains(&sub.id))
        .map(|(_, sub)| (sub.id.clone(), blobs_of(listed, &sub.files)))
        .collect();
    let mut state = shared.state.lock().unwrap();
    let cost = built.as_ref().map_or(0.0, |built| built.cost_usd) + state.spent;
    let model = top_model(&run).or_else(|| built.as_ref().map(|built| built.model.clone()));
    let mut wiki = wiki;
    wiki.generated.cost_usd = round_cents(cost);
    if let Some(model) = &model {
        wiki.generated.model = model.clone();
        wiki.generated.by = claude::display_name(model);
    }
    wiki.write(&paths.wiki)?;
    state.book.built = Some(Built {
        commit: run.commit.clone(),
        branch: run.branch.clone(),
        at: wiki.generated.at.clone(),
        plan: plan.clone(),
        blobs,
        missing: missing.clone(),
        cost_usd: cost,
        model: model.unwrap_or_else(|| shared.settings.model.clone()),
    });
    drop(state);
    shared.log(&format!(
        "built: {} sections, {} subsections ({written} written, {} missing), {} diagrams, ${cost:.2} in all",
        wiki.sections.len(),
        wiki.subsections(),
        missing.len(),
        wiki.diagrams()
    ));
    Ok((wiki, written, missing.len()))
}

/// For an update, what to write again, from what changed since `built`:
/// the outline kept with the source files nobody covered given to the
/// subsections nearest them, or, when much changed, a new one.
fn decide_update(
    shared: &Shared,
    name: &str,
    listed: &Files,
    built: &Built,
    old: Option<&Wiki>,
) -> Result<()> {
    let mut plan = built.plan.clone();
    let changed: BTreeSet<String> = plan
        .subsections()
        .filter(|(_, sub)| {
            built.missing.contains(&sub.id)
                || old
                    .and_then(|wiki| find_subsection(wiki, &sub.id))
                    .is_none()
                || built.blobs.get(&sub.id) != Some(&blobs_of(listed, &sub.files))
        })
        .map(|(_, sub)| sub.id.clone())
        .collect();
    let all = plan.subsections().count();
    let uncovered = plan.uncovered(listed).len();
    let much = all > 0 && changed.len() as f64 / all as f64 > REPLAN_SHARE;
    if much || uncovered > REPLAN_UNCOVERED {
        shared.say(&format!(
            "{} of {all} subsections changed and {uncovered} new source files have no subsection: planning again",
            changed.len()
        ));
        let commit = shared.run(|run| run.commit.clone());
        let fresh = make_plan(shared, name, listed, &commit)?;
        // A subsection whose files are as they were is kept as it was.
        let kept: BTreeMap<String, String> = fresh
            .subsections()
            .filter_map(|(_, sub)| {
                let blobs = blobs_of(listed, &sub.files);
                let was = built.plan.subsections().find(|(_, before)| {
                    before.files == sub.files && built.blobs.get(&before.id) == Some(&blobs)
                })?;
                Some((sub.id.clone(), was.1.id.clone()))
            })
            .collect();
        let todo = fresh
            .subsections()
            .filter(|(_, sub)| !kept.contains_key(&sub.id))
            .map(|(_, sub)| sub.id.clone())
            .collect();
        shared.change(|run| {
            for (id, was) in &kept {
                if let Some(page) = old.and_then(|wiki| find_subsection(wiki, was)) {
                    run.pages.insert(
                        id.clone(),
                        Page {
                            body_md: page.body_md.clone(),
                            diagram: page.diagram.clone(),
                            blobs: built.blobs.get(was).cloned().unwrap_or_default(),
                            meaning_changed: false,
                            cost_usd: 0.0,
                        },
                    );
                }
            }
            run.todo = todo;
            run.plan = Some(fresh);
            run.replanned = true;
        });
        return Ok(());
    }
    let mut todo = changed;
    if uncovered > 0 {
        let before: BTreeMap<String, Vec<String>> = plan
            .subsections()
            .map(|(_, sub)| (sub.id.clone(), sub.files.clone()))
            .collect();
        plan.cover(listed);
        for (_, sub) in plan.subsections() {
            if before.get(&sub.id) != Some(&sub.files) {
                todo.insert(sub.id.clone());
            }
        }
    }
    // A subsection whose files are all gone goes, and a section left with
    // none.
    let mut gone = false;
    for section in &mut plan.sections {
        section.subsections.retain(|sub| {
            let keep = sub.files.iter().any(|entry| {
                listed.entry(entry.trim_end_matches('/')).is_some() || entry.is_empty()
            });
            gone |= !keep;
            keep
        });
        for sub in &mut section.subsections {
            sub.files
                .retain(|entry| entry.is_empty() || listed.entry(entry).is_some());
        }
    }
    plan.sections
        .retain(|section| !section.subsections.is_empty());
    todo.retain(|id| plan.subsections().any(|(_, sub)| &sub.id == id));
    shared.say(&format!(
        "{} of {} subsections changed since {}",
        todo.len(),
        plan.subsections().count(),
        short(&built.commit)
    ));
    shared.change(|run| {
        run.todo = todo;
        run.plan = Some(plan);
        run.replanned = gone;
    });
    Ok(())
}

/// Has the planner plan the wiki, sends its outline back once with what's
/// wrong, and gives the source files still left out to the subsections
/// nearest them.
fn make_plan(shared: &Shared, name: &str, listed: &Files, commit: &str) -> Result<Plan> {
    shared.change(|run| {
        run.phase = Phase::Planning;
        run.progress = "planning".to_string();
    });
    shared.say("planning the outline");
    let started = Instant::now();
    let ask = Ask {
        model: shared.settings.model.clone(),
        system: plan::SYSTEM.to_string(),
        schema: plan::schema(),
        message: plan::message(name, commit, listed),
        max_turns: PLAN_TURNS,
        budget_usd: PLAN_BUDGET,
        timeout: PLAN_TIMEOUT,
        tools: true,
    };
    let answer = shared.ask(ask.clone(), "the planner")?;
    let (mut plan, mut problems) = plan::read(&answer.value, listed)?;
    if !problems.is_empty() {
        shared.log(&format!("the outline's problems: {}", problems.join("; ")));
        let fix = Ask {
            message: plan::fix_message(&answer.value, &problems, listed),
            max_turns: FIX_TURNS * 2,
            budget_usd: FIX_BUDGET * 2.0,
            timeout: FIX_TIMEOUT,
            ..ask
        };
        match shared.ask(fix, "the planner, putting its outline right") {
            Ok(fixed) => match plan::read(&fixed.value, listed) {
                Ok((fixed, left)) => {
                    plan = fixed;
                    problems = left;
                }
                Err(err) => shared.log(&format!("its outline put right didn't read: {err:#}")),
            },
            Err(err) => shared.log(&format!("couldn't have the outline put right: {err:#}")),
        }
    }
    let given = plan.cover(listed);
    if given > 0 {
        shared.log(&format!(
            "{given} source files left out went to the subsections nearest them"
        ));
    }
    if !problems.is_empty() {
        shared.log(&format!(
            "the outline kept these problems: {}",
            problems.join("; ")
        ));
    }
    shared.say(&format!(
        "planned {} sections, {} subsections ({}s)",
        plan.sections.len(),
        plan.subsections().count(),
        started.elapsed().as_secs()
    ));
    Ok(plan)
}

/// Writes one subsection: the writer, the checks, the writer again with
/// what's wrong, the checks once more, then the linker.
#[allow(clippy::too_many_arguments)]
fn write_page(
    shared: &Shared,
    name: &str,
    plan: &Plan,
    section: &plan::PlannedSection,
    sub: &plan::PlannedSubsection,
    check: &check::Context,
    listed: &Files,
    index: &Index,
    earlier: Option<&Subsection>,
    built: Option<&Built>,
) -> Result<(Page, Tally)> {
    let update = earlier
        .zip(built)
        .map(|(earlier, built)| (earlier, built.commit.as_str()));
    let task = format!(
        "Write the subsection \"{}\" (#{}) of the section \"{}\" (#{}) in the wiki of {name}.",
        sub.title, sub.id, section.title, section.id
    );
    let mut message = format!(
        "{task}\n\nWhat it's to explain: {}\n\nThe outline of the whole page:\n\n{}",
        sub.about,
        plan.outline()
    );
    if let Some((earlier, before)) = update {
        let commit = shared.run(|run| run.commit.clone());
        message.push_str(&format!(
            "\n{}\n\nThe text before:\n\n{}\n\nThe diagram before:\n\n{}\n\nWhat changed in its files from {} to {}:\n\n```diff\n{}```\n",
            write::UPDATE,
            earlier.body_md,
            earlier.diagram.as_ref().map_or("(none)", |d| d.mermaid.as_str()),
            short(before),
            short(&commit),
            repo::diff(shared.checkout, before, &commit, &sub.files, DIFF_SHOWN),
        ));
    }
    message.push_str(&files_shown(shared.checkout, listed, index, &sub.files));
    let mut system = write::subsection_system();
    if update.is_some() {
        system.push_str("\n\n");
        system.push_str(write::UPDATE);
    }
    let ask = Ask {
        model: shared.settings.model.clone(),
        system,
        schema: write::schema(false, update.is_some()),
        message,
        max_turns: WRITE_TURNS,
        budget_usd: WRITE_BUDGET,
        timeout: WRITE_TIMEOUT,
        tools: true,
    };
    let what = format!("the writer of {}", sub.id);
    let (written, cost, tally) = write_checked(shared, ask, &task, check, false, &what)?;
    let (body, link_tally) = check::link(&prose::demote_headings(&written.text), index, &sub.files);
    let mut tally = tally;
    tally += link_tally;
    Ok((
        Page {
            body_md: body,
            diagram: written.diagram,
            blobs: blobs_of(listed, &sub.files),
            meaning_changed: written.meaning_changed,
            cost_usd: cost,
        },
        tally,
    ))
}

/// Runs a writer, checks what it wrote, and sends it back once with
/// what's wrong; what's still wrong then is dropped. Gives back the text
/// and diagram checked, what it all cost, and what the checks did.
fn write_checked(
    shared: &Shared,
    ask: Ask,
    task: &str,
    check: &check::Context,
    summary: bool,
    what: &str,
) -> Result<(Written, f64, Tally)> {
    let answer = match shared.ask(ask.clone(), what) {
        Ok(answer) => answer,
        // Once more, given more room, before it's given up on.
        Err(err) if !shared.stopped() => {
            shared.log(&format!("{what} failed, and is tried once more: {err:#}"));
            shared.ask(
                Ask {
                    max_turns: ask.max_turns + ask.max_turns / 2,
                    budget_usd: ask.budget_usd * 1.5,
                    ..ask.clone()
                },
                what,
            )?
        }
        Err(err) => return Err(err),
    };
    let mut cost = answer.cost_usd;
    let written =
        write::read(&answer.value).ok_or_else(|| anyhow!("{what} answered with no text"))?;
    let (first, problems, mut tally) = checked(&written, check, false);
    if problems.is_empty() {
        let (last, _, more) = checked(&first, check, true);
        tally += more;
        return Ok((last, cost, tally));
    }
    shared.log(&format!(
        "{what}: {} problems: {}",
        problems.len(),
        problems.join(" | ")
    ));
    let diagram_wrong = problems.iter().any(|p| p.starts_with("the diagram"));
    let fix = Ask {
        system: write::fix_system(&ask.system),
        message: write::fix_message(task, &written, &problems, summary),
        max_turns: FIX_TURNS,
        budget_usd: FIX_BUDGET,
        timeout: FIX_TIMEOUT,
        ..ask
    };
    let fixed = match shared.ask(fix, &format!("{what}, putting it right")) {
        Ok(answer) => {
            cost += answer.cost_usd;
            write::read(&answer.value).map(|fixed| Written {
                // An update's writer said whether its meaning changed the
                // first time.
                meaning_changed: written.meaning_changed,
                ..fixed
            })
        }
        Err(err) => {
            shared.log(&format!("{what} couldn't put it right: {err:#}"));
            None
        }
    };
    let (last, _, more) = checked(fixed.as_ref().unwrap_or(&first), check, true);
    tally += more;
    if diagram_wrong && last.diagram.is_some() && fixed.is_some() {
        tally.diagrams_fixed += 1;
    }
    Ok((last, cost, tally))
}

/// `written` checked: with `last`, what's still wrong dropped; without,
/// listed.
fn checked(written: &Written, check: &check::Context, last: bool) -> (Written, Vec<String>, Tally) {
    let text = check::text(&written.text, check, last);
    let mut problems = text.problems;
    let mut tally = text.tally;
    let diagram = match &written.diagram {
        None => None,
        Some(diagram) => match check::diagram(diagram) {
            Ok(diagram) => Some(diagram),
            Err(why) if last => {
                tally.diagrams_dropped += 1;
                let _ = why;
                None
            }
            Err(why) => {
                problems.push(format!("the diagram doesn't read: {why}"));
                Some(diagram.clone())
            }
        },
    };
    (
        Written {
            text: text.text,
            diagram,
            meaning_changed: written.meaning_changed,
        },
        problems,
        tally,
    )
}

/// Writes a section's summary from its subsections as written.
#[allow(clippy::too_many_arguments)]
fn write_section(
    shared: &Shared,
    name: &str,
    plan: &Plan,
    section: &plan::PlannedSection,
    check: &check::Context,
    index: &Index,
    earlier: Option<&Section>,
    old: Option<&Wiki>,
) -> Result<(Summary, Tally)> {
    let task = format!(
        "Write the summary of the section \"{}\" (#{}) in the wiki of {name}.",
        section.title, section.id
    );
    let mut message = format!(
        "{task}\n\nWhat it covers: {}\n\nThe outline of the whole page:\n\n{}\n\nIts subsections, as their writers wrote them:\n",
        section.about,
        plan.outline()
    );
    let pages = shared.run(|run| run.pages.clone());
    for sub in &section.subsections {
        let body = pages
            .get(&sub.id)
            .map(|page| page.body_md.clone())
            .or_else(|| {
                old.and_then(|wiki| find_subsection(wiki, &sub.id))
                    .map(|s| s.body_md.clone())
            })
            .unwrap_or_else(|| "(not written)".to_string());
        message.push_str(&format!("\n## {} (#{})\n\n{body}\n", sub.title, sub.id));
    }
    let update = earlier.is_some() && shared.run(|run| run.kind) == Kind::Update;
    let mut system = write::section_system();
    if let Some(earlier) = earlier.filter(|_| update) {
        message.push_str(&format!(
            "\n{}\n\nThe summary before:\n\n{}\n\nThe diagram before:\n\n{}\n",
            write::UPDATE,
            earlier.summary_md,
            earlier
                .diagram
                .as_ref()
                .map_or("(none)", |d| d.mermaid.as_str())
        ));
        system.push_str("\n\n");
        system.push_str(write::UPDATE);
    }
    let ask = Ask {
        model: shared.settings.model.clone(),
        system,
        schema: write::schema(true, update),
        message,
        max_turns: SECTION_TURNS,
        budget_usd: SECTION_BUDGET,
        timeout: SECTION_TIMEOUT,
        tools: true,
    };
    let what = format!("the summary of {}", section.id);
    let (written, cost, mut tally) = write_checked(shared, ask, &task, check, true, &what)?;
    let (summary, more) = check::link(&prose::demote_headings(&written.text), index, check.near);
    tally += more;
    Ok((
        Summary {
            summary_md: summary,
            diagram: written.diagram,
            meaning_changed: written.meaning_changed,
            cost_usd: cost,
        },
        tally,
    ))
}

/// Writes the overview from the sections' summaries.
fn write_overview(
    shared: &Shared,
    name: &str,
    plan: &Plan,
    check: &check::Context,
    index: &Index,
    old: Option<&Wiki>,
) -> Result<(Summary, Tally)> {
    let task = format!("Write the overview of the wiki of {name}.");
    let mut message = format!(
        "{task}\n\nWhat the overview is to say, as the outline's planner put it: {}\n\nThe outline of the whole page:\n\n{}\n\nEach section's summary:\n",
        plan.overview,
        plan.outline()
    );
    let summaries = shared.run(|run| run.sections.clone());
    for section in &plan.sections {
        let summary = summaries
            .get(&section.id)
            .map(|s| s.summary_md.clone())
            .or_else(|| {
                old.and_then(|wiki| wiki.sections.iter().find(|s| s.id == section.id))
                    .map(|s| s.summary_md.clone())
            })
            .unwrap_or_default();
        message.push_str(&format!(
            "\n## {} (#{})\n\n{summary}\n",
            section.title, section.id
        ));
    }
    let update = old.is_some() && shared.run(|run| run.kind) == Kind::Update;
    let mut system = write::overview_system();
    if let Some(old) = old.filter(|_| update) {
        message.push_str(&format!(
            "\n{}\n\nThe overview before:\n\n{}\n",
            write::UPDATE,
            old.overview.summary_md
        ));
        system.push_str("\n\n");
        system.push_str(write::UPDATE);
    }
    let ask = Ask {
        model: shared.settings.model.clone(),
        system,
        schema: write::schema(true, update),
        message,
        max_turns: OVERVIEW_TURNS,
        budget_usd: OVERVIEW_BUDGET,
        timeout: SECTION_TIMEOUT,
        tools: true,
    };
    let (written, cost, mut tally) =
        write_checked(shared, ask, &task, check, true, "the overview")?;
    let (summary, more) = check::link(&written.text, index, &[]);
    tally += more;
    Ok((
        Summary {
            summary_md: summary,
            diagram: written.diagram,
            meaning_changed: written.meaning_changed,
            cost_usd: cost,
        },
        tally,
    ))
}

/// The sections whose summaries this run writes: every one for a build;
/// for an update, those whose subsections changed in meaning, came or
/// went, and those the wiki didn't have.
fn sections_to_write(
    shared: &Shared,
    plan: &Plan,
    old: Option<&Wiki>,
) -> Vec<plan::PlannedSection> {
    let run = shared.run(Clone::clone);
    let has_text = |id: &str| {
        run.pages.contains_key(id) || old.and_then(|wiki| find_subsection(wiki, id)).is_some()
    };
    plan.sections
        .iter()
        .filter(|section| !run.sections.contains_key(&section.id))
        .filter(|section| section.subsections.iter().any(|sub| has_text(&sub.id)))
        .filter(|section| {
            if run.kind == Kind::Build {
                return true;
            }
            let Some(before) =
                old.and_then(|wiki| wiki.sections.iter().find(|s| s.id == section.id))
            else {
                return true;
            };
            let then: Vec<&str> = before.subsections.iter().map(|s| s.id.as_str()).collect();
            let now: Vec<&str> = section.subsections.iter().map(|s| s.id.as_str()).collect();
            then != now
                || section.subsections.iter().any(|sub| {
                    run.todo.contains(&sub.id)
                        && run
                            .pages
                            .get(&sub.id)
                            .is_some_and(|page| page.meaning_changed)
                })
        })
        .cloned()
        .collect()
}

/// Whether this run writes the overview: a build does; an update when its
/// outline changed, or a section's summary changed in meaning.
fn overview_to_write(shared: &Shared, plan: &Plan, old: Option<&Wiki>) -> bool {
    let run = shared.run(Clone::clone);
    if run.overview.is_some() {
        return false;
    }
    let Some(old) = old.filter(|_| run.kind == Kind::Update) else {
        return true;
    };
    let ids_then: Vec<&str> = old.sections.iter().map(|s| s.id.as_str()).collect();
    let ids_now: Vec<&str> = plan.sections.iter().map(|s| s.id.as_str()).collect();
    run.replanned
        || ids_then != ids_now
        || old.overview.summary_md.is_empty()
        || run.sections.values().any(|summary| summary.meaning_changed)
}

/// The page put together: what this run wrote, and for an update what it
/// didn't from the wiki before, its links moved with their lines; every
/// text then checked at this commit, what's wrong dropped, and linked.
/// Gives back the wiki, the subsections it couldn't have, and what the
/// checks did.
#[allow(clippy::too_many_arguments)]
fn assemble(
    project: &Path,
    name: &str,
    run: &Run,
    plan: &Plan,
    old: Option<&Wiki>,
    remap: &Remap,
    listed: &Files,
    index: &Index,
    paths: &Paths,
    shared: &Shared,
) -> Result<(Wiki, BTreeSet<String>, Tally)> {
    let mut missing = BTreeSet::new();
    let mut sections = Vec::new();
    for section in &plan.sections {
        let mut subsections = Vec::new();
        for sub in &section.subsections {
            // What this run wrote; or for an update, what the wiki had,
            // which for one that couldn't be written again is kept until it
            // can be, and counted missing meanwhile.
            let before = old
                .filter(|_| run.kind == Kind::Update)
                .and_then(|wiki| find_subsection(wiki, &sub.id));
            let (body_md, diagram) = match (run.pages.get(&sub.id), before) {
                (Some(page), _) => (page.body_md.clone(), page.diagram.clone()),
                (None, Some(before)) => {
                    if run.todo.contains(&sub.id) {
                        missing.insert(sub.id.clone());
                    }
                    (moved(&before.body_md, remap), before.diagram.clone())
                }
                (None, None) => {
                    missing.insert(sub.id.clone());
                    continue;
                }
            };
            subsections.push(Subsection {
                id: sub.id.clone(),
                title: sub.title.clone(),
                body_md,
                diagram,
                files: sub.files.clone(),
            });
        }
        if subsections.is_empty() {
            continue;
        }
        let (summary_md, diagram) = match run.sections.get(&section.id) {
            Some(summary) => (summary.summary_md.clone(), summary.diagram.clone()),
            None => match old.and_then(|wiki| wiki.sections.iter().find(|s| s.id == section.id)) {
                Some(before) => (moved(&before.summary_md, remap), before.diagram.clone()),
                None => (String::new(), None),
            },
        };
        sections.push(Section {
            id: section.id.clone(),
            title: section.title.clone(),
            summary_md,
            diagram,
            subsections,
        });
    }
    if sections.is_empty() {
        bail!("no subsection could be written");
    }
    let overview = match &run.overview {
        Some(summary) => Overview {
            summary_md: summary.summary_md.clone(),
            diagram: summary.diagram.clone(),
        },
        None => old.map_or_else(Overview::default, |wiki| Overview {
            summary_md: moved(&wiki.overview.summary_md, remap),
            diagram: wiki.overview.diagram.clone(),
        }),
    };
    // Every anchor the page has now.
    let mut anchors: BTreeSet<String> = [plan::OVERVIEW_ID.to_string()].into();
    for section in &sections {
        anchors.insert(section.id.clone());
        anchors.extend(section.subsections.iter().map(|sub| sub.id.clone()));
    }
    let mut tally = Tally::default();
    let mut last = |md: &str, near: &[String]| -> String {
        let ctx = check::Context {
            root: &paths.checkout,
            project,
            files: listed,
            index,
            anchors: &anchors,
            near,
        };
        let checked = check::text(md, &ctx, true);
        // Counted once, where each text was written.
        tally.moved += checked.tally.moved;
        tally.dropped += checked.tally.dropped;
        let (linked, more) = check::link(&checked.text, index, near);
        tally.linked += more.linked;
        linked
    };
    let overview = Overview {
        summary_md: last(&overview.summary_md, &[]),
        ..overview
    };
    for section in &mut sections {
        let near: Vec<String> = section
            .subsections
            .iter()
            .flat_map(|sub| sub.files.iter().cloned())
            .collect();
        section.summary_md = last(&section.summary_md, &near);
        for sub in &mut section.subsections {
            sub.body_md = last(&sub.body_md, &sub.files);
        }
    }
    let web = repo::web_of(project);
    let wiki = Wiki {
        version: model::VERSION,
        repo: model::Repo {
            name: name.to_string(),
            root: project.to_path_buf(),
            commit: run.commit.clone(),
            branch: run.branch.clone(),
            web_url: web.as_ref().map(|web| web.url.clone()),
            code_url: web.as_ref().map(|web| web.code_url.clone()),
        },
        generated: Generated {
            at: book::utc_now(),
            by: claude::display_name(&shared.settings.model),
            model: shared.settings.model.clone(),
            cost_usd: 0.0,
            crystal: crystal::version(),
        },
        overview,
        sections,
    };
    Ok((wiki, missing, tally))
}

/// `md`, written at an earlier commit, with its links into the code moved
/// to where their lines are now.
fn moved(md: &str, remap: &Remap) -> String {
    if remap.is_empty() {
        return md.to_string();
    }
    let edits = prose::links(md)
        .into_iter()
        .filter_map(|link| {
            let code = CodeLink::parse(&link.dest)?;
            let now = remap.link(&code);
            (now != code).then(|| (link.range, format!("[{}]({})", link.label, now.target())))
        })
        .collect();
    prose::splice(md, edits)
}

/// What a writer is shown of its files: which they are, the commits that
/// touched them, what they define by line, and the files themselves,
/// numbered, as many as fit in [`INLINE`].
fn files_shown(checkout: &Path, listed: &Files, index: &Index, entries: &[String]) -> String {
    let covered = listed.covered(entries);
    let lines: u64 = covered.iter().map(|file| u64::from(file.lines)).sum();
    let mut out = format!(
        "\nIts files ({} files, {lines} lines):\n{}\n",
        covered.len(),
        covered
            .iter()
            .map(|file| format!("- {} ({} lines)", file.path, file.lines))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let log = repo::log(checkout, entries, COMMITS_SHOWN);
    if !log.trim().is_empty() {
        out.push_str(&format!(
            "\nThe commits that touched them lately, newest first:\n{log}"
        ));
    }
    let defs: Vec<_> = index
        .outline(entries)
        .into_iter()
        .filter(|def| {
            !matches!(
                def.kind,
                DefKind::Field | DefKind::Variant | DefKind::ConfigKey
            )
        })
        .collect();
    if !defs.is_empty() {
        out.push_str("\nWhat its files define, with their lines:\n");
        let mut path = "";
        for def in defs.iter().take(DEFS_SHOWN) {
            if def.path != path {
                path = &def.path;
                out.push_str(&format!("\n{path}:"));
            }
            let lines = if def.end > def.start {
                format!("{}-{}", def.start, def.end)
            } else {
                def.start.to_string()
            };
            out.push_str(&format!(" {} {} L{lines};", def.kind.label(), def.name));
        }
        out.push('\n');
        if defs.len() > DEFS_SHOWN {
            out.push_str(&format!("(and {} more)\n", defs.len() - DEFS_SHOWN));
        }
    }
    let mut shown = String::new();
    let mut left = Vec::new();
    for file in covered.iter().filter(|file| file.text) {
        let Ok(text) = std::fs::read_to_string(checkout.join(&file.path)) else {
            continue;
        };
        if shown.len() + text.len() > INLINE {
            left.push(file.path.as_str());
            continue;
        }
        shown.push_str(&format!("\n=== {} ({} lines) ===\n", file.path, file.lines));
        for (n, line) in text.lines().enumerate() {
            let line = if line.len() > LONGEST_LINE {
                let mut cut = LONGEST_LINE;
                while !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                &line[..cut]
            } else {
                line
            };
            shown.push_str(&format!("{:>6}\t{line}\n", n + 1));
        }
    }
    out.push_str("\nIts files, numbered as Read numbers them");
    if !left.is_empty() {
        out.push_str(&format!(
            " (these didn't fit, so Read them as you need them: {})",
            left.join(", ")
        ));
    }
    out.push_str(":\n");
    out.push_str(&shown);
    out
}

/// The hash of each file `entries` cover.
fn blobs_of(listed: &Files, entries: &[String]) -> BTreeMap<String, String> {
    listed
        .covered(entries)
        .into_iter()
        .map(|file| (file.path.clone(), file.blob.clone()))
        .collect()
}

fn find_subsection<'a>(wiki: &'a Wiki, id: &str) -> Option<&'a Subsection> {
    wiki.sections
        .iter()
        .flat_map(|section| &section.subsections)
        .find(|sub| sub.id == id)
}

/// The model that cost most in `run`.
fn top_model(run: &Run) -> Option<String> {
    run.models
        .iter()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(model, _)| model.clone())
}

/// The repository's name: `owner/name` on its forge, or its directory's.
fn repo_name(project: &Path) -> String {
    repo::web_of(project).map_or_else(
        || {
            project.file_name().map_or_else(
                || project.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        },
        |web| web.path,
    )
}

fn round_cents(usd: f64) -> f64 {
    (usd * 100.0).round() / 100.0
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(7)]
}

fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Build => "build",
        Kind::Update => "update",
    }
}

/// Tells the daemon at `socket` that `run` started, or with `ended`, ended
/// as `about` says.
fn tell(socket: &Path, project: &Path, run: &Run, about: impl FnOnce(About) -> About, ended: bool) {
    let base = About {
        commit: run.commit.clone(),
        update: run.kind == Kind::Update,
        ..About::default()
    };
    crystal::tell(socket, project, &about(base), ended);
}

impl Shared<'_> {
    /// Something about the run, read.
    fn run<T>(&self, read: impl FnOnce(&Run) -> T) -> T {
        let state = self.state.lock().unwrap();
        read(state.book.run.as_ref().expect("a run is under way"))
    }

    /// The run changed, and written down.
    fn change<T>(&self, change: impl FnOnce(&mut Run) -> T) -> T {
        let mut state = self.state.lock().unwrap();
        let run = state.book.run.as_mut().expect("a run is under way");
        let out = change(run);
        if let Err(err) = state.book.write(self.book_path) {
            drop(state);
            self.log(&format!("couldn't write the book down: {err:#}"));
        }
        out
    }

    fn save(&self) {
        self.change(|_| ());
    }

    fn stopped(&self) -> bool {
        self.state.lock().unwrap().stopped.is_some()
    }

    fn stopped_why(&self) -> Option<String> {
        self.state.lock().unwrap().stopped.clone()
    }

    /// Runs Claude on `ask` within what's left of the budget, keeps what it
    /// cost, and logs it as `what`.
    fn ask(&self, ask: Ask, what: &str) -> Result<claude::Answer> {
        let budget = {
            let mut state = self.state.lock().unwrap();
            if let Some(why) = &state.stopped {
                bail!("{why}");
            }
            let left = state.limit - state.spent - state.reserved;
            let budget = ask.budget_usd.min(left);
            if budget < LEAST_CALL {
                let why = format!("the build reached its budget of ${:.2}", state.limit);
                state.stopped = Some(why.clone());
                bail!("{why}");
            }
            state.reserved += budget;
            budget
        };
        let started = Instant::now();
        let result = claude::run(
            &Ask {
                budget_usd: budget,
                ..ask
            },
            self.checkout,
        );
        let cost = match &result {
            Ok(answer) => answer.cost_usd,
            Err(err) => claude::cost_of(err),
        };
        {
            let mut state = self.state.lock().unwrap();
            state.reserved -= budget;
            state.spent += cost;
            let run = state.book.run.as_mut().expect("a run is under way");
            run.cost_usd += cost;
            if let Ok(answer) = &result
                && let Some(model) = &answer.model
            {
                *run.models.entry(model.clone()).or_default() += cost;
            }
        }
        match &result {
            Ok(_) => self.log(&format!(
                "{what}: ${cost:.3} in {}s",
                started.elapsed().as_secs()
            )),
            Err(err) => self.log(&format!(
                "{what} failed after {}s (${cost:.3}): {err:#}",
                started.elapsed().as_secs()
            )),
        }
        result.with_context(|| format!("{what} failed"))
    }

    fn say(&self, line: &str) {
        (self.say)(line);
        self.log(line);
    }

    fn log(&self, line: &str) {
        if let Some(file) = self.log.lock().unwrap().as_mut() {
            let _ = writeln!(file, "{} {line}", book::utc_now());
        }
    }
}
