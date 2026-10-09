//! `crystal wiki`: writing a project's wiki, writing again what changed,
//! and how it stands.

use super::book::{self, Book, Kind};
use super::build::{self, Options};
use super::crystal::WikiSettings;
use super::crystal::{self, errln, outln};
use super::model::Wiki;
use super::{BUILD_FILE, LOG_FILE, Status, WIKI_FILE};
use anyhow::{Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// What a build may do, over the settings.
#[derive(clap::Args)]
pub struct How {
    /// The most it may spend, in US dollars [default: [wiki] budget_usd]
    #[arg(long, value_name = "USD")]
    budget: Option<f64>,

    /// The model that writes it, as `claude --model` takes it [default:
    /// [wiki] model]
    #[arg(long)]
    model: Option<String>,

    /// How many writers write at once [default: [wiki] concurrency]
    #[arg(long, value_name = "N")]
    concurrency: Option<usize>,

    /// Don't fetch the default branch from `origin` first: write from it
    /// as it was last fetched.
    #[arg(long)]
    no_fetch: bool,
}

/// The main worktree of the project `dir` is in (the current directory
/// without one), and the wiki's settings, once the plugin is on.
fn project(dir: Option<PathBuf>) -> Result<(PathBuf, WikiSettings)> {
    if !crystal::enabled()? {
        bail!(crystal::off());
    }
    let dir = match dir {
        Some(dir) => std::path::absolute(dir)?,
        None => std::env::current_dir()?,
    };
    let Some(project) = crystal::project_of(&dir) else {
        bail!(
            "{} isn't in a git repository: a wiki is written from a commit",
            dir.display()
        );
    };
    Ok((project, crystal::settings()?))
}

/// `crystal wiki build`: writes the wiki of the project `dir` is in, for
/// the daemon at `socket`, or with `fresh`, starts over.
pub fn build(socket: &Path, dir: Option<PathBuf>, fresh: bool, how: How) -> Result<()> {
    let (project, settings) = project(dir)?;
    write(socket, &project, settings, Kind::Build, fresh, how)
}

/// `crystal wiki update`: writes again what changed; with `auto`, only as
/// the settings have the daemon do it on its own.
pub fn update(socket: &Path, dir: Option<PathBuf>, how: How, auto: bool) -> Result<()> {
    let (project, settings) = project(dir)?;
    if auto {
        let has_wiki = super::dir(socket, &project).join(WIKI_FILE).exists();
        if !settings.auto_update || !has_wiki {
            return Ok(());
        }
    }
    write(socket, &project, settings, Kind::Update, false, how)
}

/// `crystal wiki status`.
pub fn status(socket: &Path, dir: Option<PathBuf>, json: bool) -> Result<()> {
    let (project, _) = project(dir)?;
    show_status(socket, &project, json)
}

fn write(
    socket: &Path,
    project: &Path,
    settings: WikiSettings,
    kind: Kind,
    fresh: bool,
    how: How,
) -> Result<()> {
    let settings = WikiSettings {
        model: how.model.unwrap_or(settings.model),
        concurrency: how.concurrency.unwrap_or(settings.concurrency),
        ..settings
    };
    if let Some(budget) = how.budget
        && (budget.is_nan() || budget <= 0.0)
    {
        bail!("--budget is {budget}: it's more than 0");
    }
    settings.check()?;
    let options = Options {
        kind,
        fresh,
        budget_usd: how.budget,
        fetch: !how.no_fetch,
    };
    let say = |line: &str| {
        let _ = outln!("{line}");
    };
    let outcome = build::run(socket, project, &settings, &options, &say)?;
    let Some(wiki) = outcome.wiki else {
        return Ok(());
    };
    let path = super::dir(socket, project).join(WIKI_FILE);
    outln!(
        "wrote the wiki of {} at {}: {} sections, {} subsections ({} written now), {} diagrams, \
         {} links into the code; ${:.2} in {}",
        wiki.repo.name,
        short(&outcome.commit),
        wiki.sections.len(),
        wiki.subsections(),
        outcome.written,
        wiki.diagrams(),
        links(&wiki),
        outcome.cost_usd,
        duration(outcome.seconds),
    )?;
    let tally = outcome.tally;
    outln!(
        "the checks moved {} links and dropped {}, rewrote {} diagrams and dropped {}; the linker \
         linked {} code spans",
        tally.moved,
        tally.dropped,
        tally.diagrams_fixed,
        tally.diagrams_dropped,
        tally.linked
    )?;
    if outcome.missing > 0 {
        errln!(
            "{} subsections couldn't be written (see {}): `crystal wiki update` tries them again",
            outcome.missing,
            super::dir(socket, project).join(LOG_FILE).display()
        );
    }
    outln!("{}", path.display())?;
    Ok(())
}

/// What `status --json` prints.
#[derive(Serialize)]
struct StatusJson<'a> {
    project: &'a Path,
    dir: PathBuf,
    #[serde(flatten)]
    status: &'a Status,
    sections: usize,
    subsections: usize,
    diagrams: usize,
    links: usize,
    last: Option<&'a book::Done>,
}

fn show_status(socket: &Path, project: &Path, json: bool) -> Result<()> {
    let dir = super::dir(socket, project);
    let status = super::status(socket, project);
    let wiki = Wiki::read(&dir.join(WIKI_FILE)).ok();
    let book = Book::read(&dir.join(BUILD_FILE)).unwrap_or_default();
    let last = book.history.last();
    if json {
        let shown = StatusJson {
            project,
            dir: dir.clone(),
            status: &status,
            sections: wiki.as_ref().map_or(0, |wiki| wiki.sections.len()),
            subsections: wiki.as_ref().map_or(0, Wiki::subsections),
            diagrams: wiki.as_ref().map_or(0, Wiki::diagrams),
            links: wiki.as_ref().map_or(0, links),
            last,
        };
        outln!("{}", serde_json::to_string_pretty(&shown)?)?;
        return Ok(());
    }
    let name = wiki.as_ref().map_or_else(
        || {
            project
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        },
        |wiki| wiki.repo.name.clone(),
    );
    outln!("the wiki of {name} ({})", project.display())?;
    match &wiki {
        None => outln!("  none yet: `crystal wiki build` writes one")?,
        Some(wiki) => {
            let at = book::seconds_of(&wiki.generated.at)
                .map(|then| crystal::ago(then, book::now_seconds()))
                .map_or_else(|| wiki.generated.at.clone(), |ago| format!("{ago} ago"));
            outln!(
                "  written {at}, at {} on {}, by {}",
                short(&wiki.repo.commit),
                wiki.repo.branch,
                wiki.generated.by
            )?;
            outln!(
                "  {} sections, {} subsections, {} diagrams, {} links into the code",
                wiki.sections.len(),
                wiki.subsections(),
                wiki.diagrams(),
                links(wiki)
            )?;
            match (status.stale, status.behind) {
                (false, _) => outln!("  up to date with {}", wiki.repo.branch)?,
                (true, Some(behind)) => outln!(
                    "  {behind} commits behind {}: `crystal wiki update` writes what changed",
                    status.branch.as_deref().unwrap_or("the default branch")
                )?,
                (true, None) => outln!(
                    "  behind {}: `crystal wiki update` writes what changed",
                    status.branch.as_deref().unwrap_or("the default branch")
                )?,
            }
            if status.missing > 0 {
                outln!(
                    "  {} subsections couldn't be written: `crystal wiki update` tries them again",
                    status.missing
                )?;
            }
            outln!("  ${:.2} in all", status.cost_usd)?;
        }
    }
    if let Some(progress) = &status.progress {
        if status.building {
            outln!("  building: {progress}")?;
        } else {
            outln!("  a build stopped at {progress}: `crystal wiki build` carries on from there")?;
        }
    }
    if let Some(last) = last {
        let kind = match last.kind {
            Kind::Build => "build",
            Kind::Update => "update",
        };
        let how = match &last.failed {
            Some(why) => format!("stopped: {why}"),
            None => format!(
                "{} of {} subsections written",
                last.written, last.subsections
            ),
        };
        outln!(
            "  the last {kind}: {how}, ${:.2} in {}",
            last.cost_usd,
            duration(last.seconds)
        )?;
    }
    outln!("  {}", dir.display())?;
    Ok(())
}

/// How many links into the code the wiki has.
fn links(wiki: &Wiki) -> usize {
    wiki.texts()
        .iter()
        .map(|text| text.matches("](code:").count())
        .sum()
}

/// `seconds` as a person says it: `42s`, `12m`, `1h 5m`.
fn duration(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
    }
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(7)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duration_reads_as_a_person_says_it() {
        assert_eq!(duration(42), "42s");
        assert_eq!(duration(754), "12m 34s");
        assert_eq!(duration(3900), "1h 5m");
    }
}
