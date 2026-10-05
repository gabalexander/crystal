//! `crystal remember` and `crystal memory`: adding to a project's memory,
//! and reading, searching and tidying it, from a shell or from an agent in
//! a session.

use crate::client;
use crate::config::{Config, Embedder, MemorySettings};
use crate::distill::{self, Change, Proposal};
use crate::embed::{self, Embed, Models};
use crate::env;
use crate::events::{self, Event};
use crate::gemini;
use crate::git::Checkout;
use crate::memory::{
    self, Added, Entry, Forgotten, Kind, Listed, Memory, Merge, Near, New, Replacing, Source,
    Store, Superseded, Wanted,
};
use crate::output::{err, errln, out, outln};
use crate::printable;
use crate::protocol::{Request, Response};
use crate::tui::sidebar::ago;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Remembers `text` for the project `dir` is in (the current directory
/// without one), about `files`, under `title` if it's given, and says which
/// entry it is; with `replacing`, in place of the entry it names, which no
/// longer holds, and is retired. One near an entry said before, about the
/// same thing but saying something else, says so, and how to have it
/// replace that one. In the words of an entry that stopped holding, it
/// isn't remembered, and says so.
pub fn remember(
    socket: &Path,
    dir: Option<PathBuf>,
    kind: Kind,
    files: Vec<String>,
    title: Option<String>,
    text: &str,
    replacing: Option<Replacing>,
) -> Result<()> {
    check_on()?;
    let text = match title {
        Some(title) => memory::titled(&title, text)?,
        None => text.to_string(),
    };
    let dir = dir_or_current(dir)?;
    let project = memory::project_of(&dir);
    let top = top_of(&dir);
    let files = files
        .iter()
        .map(|file| from_top(file, &dir, &top))
        .collect();
    let new = New {
        kind,
        text,
        files,
        source: source(socket),
        checkout: Some(top),
    };
    let replacing = replacing.map(|replacing| Replacing {
        why: match replacing.why.trim() {
            "" => format!("{} remembered another in its place", source(socket)),
            why => why.to_string(),
        },
        ..replacing
    });
    let (added, retired) = added(socket, &project, new, replacing)?;
    let title = |entry: &Entry| printable::line(&memory::title(&entry.text)).into_owned();
    let place = (retired.as_ref())
        .map(|was| format!(", in place of {}", was.entry.id))
        .unwrap_or_default();
    match added {
        Added::New(entry) => outln!("remembered {}{place}", entry.id)?,
        Added::Near { entry, near } => {
            let (id, near_id) = (entry.id, near.entry.id);
            outln!("remembered {id}{place}")?;
            if retired.is_none() {
                outln!("near {near_id}: {}", title(&near.entry))?;
                outln!("if {id} replaces it: crystal memory retire {near_id} --by {id}")?;
            }
        }
        Added::Again(entry) => outln!("remembered {} already{place}", entry.id)?,
        Added::Alike(entry) => {
            let (id, said) = (entry.id, title(&entry));
            outln!("remembered {id} already, in other words{place}: {said}")?;
            if retired.is_none() {
                outln!("if it replaces {id} rather than saying it again: add --replaces {id}")?;
            }
        }
        Added::Outdated(was) => bail!("{}", outdated(socket, &project, &was)),
        Added::Refused => unreachable!("only crystal is refused what was forgotten"),
    }
    Ok(())
}

/// Adds `new` to `project`'s memory, with `replacing` in place of the entry
/// it names: by the daemon, which keeps the models that find it said already
/// in other words loaded, and tells of a new entry and the one retired; or
/// here, when there's no daemon to ask. Gives what it came to, and the entry
/// retired, as it was.
fn added(
    socket: &Path,
    project: &Path,
    new: New,
    replacing: Option<Replacing>,
) -> Result<(Added, Option<Superseded>)> {
    let request = Request::Remember {
        project: project.to_path_buf(),
        entry: new.clone(),
        replaces: replacing.clone(),
    };
    match client::ask(socket, &request, false) {
        Ok(Some(Response::Remembered(added))) => return Ok((added, None)),
        Ok(Some(Response::Replaced { added, retired })) => return Ok((added, retired.map(|r| *r))),
        _ => {}
    }
    let embedder = embed::shared_now();
    let embedder = embed::as_embed(&embedder);
    let mut store = Store::open(socket)?;
    let (added, retired) = match replacing {
        Some(replacing) => store.replace(project, new, replacing.id, &replacing.why, embedder)?,
        None => (store.add_with(project, new, embedder)?, None),
    };
    for event in events::remembered(project, &added, retired.as_ref()) {
        tell(socket, event);
    }
    Ok((added, retired))
}

/// Why what was to be remembered wasn't: `was`, an entry of `project` that
/// said it, stopped holding; what holds in its place, and how to put it
/// back.
fn outdated(socket: &Path, project: &Path, was: &Superseded) -> String {
    let id = was.entry.id;
    let mut text = format!(
        "entry {id} said that, and it stopped holding {}: {}",
        how_long_ago(was.superseded, now()),
        was.why
    );
    let holding = Store::open(socket).and_then(|mut store| store.holding(project, id));
    if let Ok(Some(holder)) = holding {
        let title = memory::title(&holder.text);
        match holder.id == id {
            true => text.push_str(&format!("\n{id} says now: {title}")),
            false => text.push_str(&format!("\n{} holds in its place: {title}", holder.id)),
        }
    }
    text.push_str(&format!(
        "\n`crystal memory restore {id}` puts it back as it was"
    ));
    printable::text(&text).into_owned()
}

/// Retires entry `id` of the project's memory, which no longer holds, for
/// `why`: entry `by` holds in its place. Says so.
pub fn retire(
    socket: &Path,
    dir: Option<PathBuf>,
    id: u64,
    by: u64,
    why: Option<String>,
) -> Result<()> {
    check_on()?;
    let project = memory::project_of(&dir_or_current(dir)?);
    let why = match why.as_deref().map(str::trim) {
        Some(why) if !why.is_empty() => why.to_string(),
        _ => format!("{} said {by} holds in its place", source(socket)),
    };
    let mut store = Store::open(socket)?;
    let retired = store.retire(&project, id, by, &why)?;
    let holder = store
        .get(&project, by)?
        .context("the entry in its place is gone")?;
    let line = format!("retired {id}: {}", memory::title(&retired.entry.text));
    outln!("{}", printable::line(&line))?;
    let line = format!("{by} holds in its place: {}", memory::title(&holder.text));
    outln!("{}", printable::line(&line))?;
    tell(socket, Event::superseded(project, holder, retired));
    Ok(())
}

/// Puts entry `id` of the project's memory back as it was before it last
/// stopped holding: a retired entry back in the list, or an updated one's
/// words before back in place of its words now. Says so.
pub fn restore(socket: &Path, dir: Option<PathBuf>, id: u64) -> Result<()> {
    check_on()?;
    let project = memory::project_of(&dir_or_current(dir)?);
    let entry = Store::open(socket)?.restore(&project, id)?;
    let line = format!("put {id} back: {}", memory::title(&entry.text));
    outln!("{}", printable::line(&line))?;
    tell(
        socket,
        Event::memory(events::Kind::MemoryRestored, project, entry),
    );
    Ok(())
}

/// Lists the entries of the project's memory that say what another does,
/// each group under the one it keeps, as `crystal memory dedupe` finds
/// them, and with `apply`, merges them; found and merged by the daemon,
/// which keeps the models loaded, or here, when there's no daemon to ask.
pub fn dedupe(socket: &Path, dir: Option<PathBuf>, apply: bool) -> Result<()> {
    check_on()?;
    let dir = dir_or_current(dir)?;
    let request = Request::DedupeMemory {
        dir: dir.clone(),
        apply,
    };
    let merges = match client::ask(socket, &request, false) {
        Ok(Some(Response::Deduped { merges })) => merges,
        Ok(_) => {
            let project = memory::project_of(&dir);
            let embedder = embed::shared_now();
            let merges = memory::dedupe(socket, &project, embed::as_embed(&embedder), apply)?;
            if apply {
                for merge in &merges {
                    let ids: Vec<u64> = merge.merged.iter().map(|twin| twin.entry.id).collect();
                    tell(
                        socket,
                        Event::merged(project.clone(), merge.kept.clone(), &ids),
                    );
                }
            }
            merges
        }
        Err(err) => return Err(err),
    };
    out!("{}", merges_text(&merges, apply))?;
    Ok(())
}

/// What `crystal memory dedupe` prints of `merges`: each one kept, by its
/// id, kind and title, and under it each that goes into it, with how alike
/// they are; then how many went, or would go with `--apply`.
fn merges_text(merges: &[Merge], applied: bool) -> String {
    let line = |entry: &Entry| {
        printable::line(&format!(
            "{:>4}  {:<8}  {}",
            entry.id,
            entry.kind.to_string(),
            memory::title(&entry.text)
        ))
        .into_owned()
    };
    let mut text = String::new();
    for merge in merges {
        text.push_str(&format!("{}\n", line(&merge.kept)));
        for twin in &merge.merged {
            text.push_str(&format!("  ← {:.2}  {}\n", twin.alike, line(&twin.entry)));
        }
    }
    let merged: usize = merges.iter().map(|merge| merge.merged.len()).sum();
    let entries = if merged == 1 { "entry" } else { "entries" };
    let into = merges.len();
    text.push_str(&match (merged, applied) {
        (0, _) => "no two entries say the same thing\n".to_string(),
        (_, true) => format!("merged {merged} {entries} into {into}\n"),
        (_, false) => format!("{merged} {entries} would go into {into}: --apply merges them\n"),
    });
    text
}

/// Has the distiller's model look through the groups of entries near one
/// another in the project's memory for any another of its group shows no
/// longer holds, and lists what it proposes: each retired, another holding
/// in its place, or updated, and why. What it lists is kept, and `apply`
/// makes it so, any changed or gone since passed over; with nothing kept,
/// `apply` asks the model now, and makes what it proposes so. Each change
/// can be undone with `restore`.
pub fn reconcile(socket: &Path, dir: Option<PathBuf>, apply: bool) -> Result<()> {
    check_on()?;
    let dir = dir_or_current(dir)?;
    let project = memory::project_of(&dir);
    let mut proposed = Proposed::read(socket);
    let kept = proposed.0.remove(&project);
    let proposals = match kept {
        Some(kept) if apply => kept,
        _ => {
            let Near { groups, said_again } = near_groups(socket, &dir)?;
            if !said_again.is_empty() {
                let count = said_again.len();
                errln!(
                    "left out {count} that say what another does: `crystal memory dedupe` merges \
                     them"
                );
            }
            if groups.is_empty() {
                outln!("no two entries are near one another")?;
                return Ok(());
            }
            let settings = Config::load()?.memory;
            let entries: usize = groups.iter().map(Vec::len).sum();
            errln!(
                "asking {} whether any of the {entries} entries in {} groups near one another no \
                 longer holds…",
                settings.distill_model,
                groups.len()
            );
            let env = std::env::vars().collect();
            let found = distill::superseded_among(&groups, &settings, &top_of(&dir), &env)?;
            for why in &found.rejected {
                errln!("  rejected {why}");
            }
            errln!("(${:.4})", found.cost_usd);
            found.proposals
        }
    };
    let mut store = Store::open(socket)?;
    if !apply {
        out!("{}", proposals_text(&proposals, &store.entries(&project)?))?;
        proposed.0.insert(project, proposals);
        return proposed.write(socket);
    }
    // Made so, they're done with.
    proposed.write(socket)?;
    for proposal in &proposals {
        let id = proposal.id;
        match apply_proposal(&mut store, &project, proposal) {
            Ok((holder, was)) => {
                let line = match &proposal.change {
                    Change::Retire { by } => {
                        format!(
                            "retired {id}, {by} in its place: {}",
                            memory::title(&was.entry.text)
                        )
                    }
                    Change::Update { .. } => {
                        format!("updated {id}: {}", memory::title(&holder.text))
                    }
                };
                outln!("{}", printable::line(&line))?;
                tell(socket, Event::superseded(project.clone(), holder, was));
            }
            Err(err) => errln!("passed over {id}: {err:#}"),
        }
    }
    if proposals.is_empty() {
        outln!("every entry holds")?;
    }
    Ok(())
}

/// Makes `proposal` so in `project`'s memory, unless its entry changed or
/// went since it was read: gives the entry that holds in its place, as it
/// is now, and what it was.
fn apply_proposal(
    store: &mut Store,
    project: &Path,
    proposal: &Proposal,
) -> Result<(Entry, Superseded)> {
    let id = proposal.id;
    let entry = store.get(project, id)?;
    let Some(entry) = entry.filter(|entry| entry.text == proposal.was) else {
        bail!("it changed or went since it was read");
    };
    match &proposal.change {
        Change::Retire { by } => {
            let was = store.retire(project, id, *by, &proposal.why)?;
            let holder = store
                .get(project, *by)?
                .context("the entry in its place is gone")?;
            Ok((holder, was))
        }
        Change::Update { text } => {
            let checkout = entry.checkout.filter(|dir| dir.is_dir());
            let checkout = checkout.as_deref().unwrap_or(project);
            let (was, now) = store.update(project, id, text, None, &[], checkout, &proposal.why)?;
            Ok((now, was))
        }
    }
}

/// What `crystal memory reconcile` prints of `proposals`, `entries` the
/// project's as they are: each entry that no longer holds, by its id, kind
/// and title, then what holds in its place, or what it's updated to, and
/// why; then how many, and how to make them so.
fn proposals_text(proposals: &[Proposal], entries: &[Entry]) -> String {
    let title = |id: u64| {
        (entries.iter().find(|entry| entry.id == id))
            .map(|entry| memory::title(&entry.text))
            .unwrap_or_default()
    };
    let mut text = String::new();
    let (mut retired, mut updated) = (0, 0);
    for proposal in proposals {
        let id = proposal.id;
        let kind = (entries.iter().find(|entry| entry.id == id))
            .map(|entry| entry.kind.to_string())
            .unwrap_or_default();
        let mut lines = vec![format!(
            "{id:>4}  {kind:<8}  {}",
            memory::title(&proposal.was)
        )];
        match &proposal.change {
            Change::Retire { by } => {
                retired += 1;
                lines.push(format!("  → {by:<6}  {}", title(*by)));
            }
            Change::Update { text } => {
                updated += 1;
                lines.push(format!("  → update  {}", memory::one_line(text)));
            }
        }
        lines.push(format!("    why: {}", proposal.why));
        for line in lines {
            text.push_str(&printable::line(&line));
            text.push('\n');
        }
    }
    text.push_str(&match (retired, updated) {
        (0, 0) => "every entry holds\n".to_string(),
        _ => format!(
            "would retire {retired} and update {updated}: --apply makes it so, and `crystal \
             memory restore <id>` puts one back\n"
        ),
    });
    text
}

/// The groups of entries near one another in the memory of the project
/// `dir` is in, each with whether it holds, and those left out as they say
/// what another does: found by the daemon, which keeps the models loaded,
/// or here, when there's no daemon to ask.
fn near_groups(socket: &Path, dir: &Path) -> Result<Near> {
    let request = Request::NearMemory {
        dir: dir.to_path_buf(),
    };
    match client::ask(socket, &request, false)? {
        Some(Response::Near(near)) => Ok(near),
        _ => {
            let embedder = embed::shared_now();
            memory::near(socket, &memory::project_of(dir), embed::as_embed(&embedder))
        }
    }
}

/// What `crystal memory reconcile` proposed for each project, by its path,
/// kept beside the memory until `--apply` makes it so.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Proposed(BTreeMap<PathBuf, Vec<Proposal>>);

impl Proposed {
    fn file(socket: &Path) -> PathBuf {
        memory::dir(socket).join("reconcile.json")
    }

    /// What's kept, or nothing when there's nothing, or it can't be read.
    fn read(socket: &Path) -> Proposed {
        std::fs::read_to_string(Proposed::file(socket))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn write(&self, socket: &Path) -> Result<()> {
        let file = Proposed::file(socket);
        std::fs::write(&file, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("couldn't write {}", file.display()))
    }
}

/// Which of a project's entries `crystal memory list` lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Listing {
    /// Of this kind alone.
    pub kind: Option<Kind>,
    /// What was forgotten instead, the latest forgotten first.
    pub forgotten: bool,
    /// Those that read as status rather than lessons: see [`is_status`].
    pub status: bool,
    /// The expired alone.
    pub expired: bool,
    /// Those that stopped holding instead, the latest first.
    pub superseded: bool,
}

/// Prints the project's memory, newest first, as `listing` says.
pub fn list(socket: &Path, dir: Option<PathBuf>, listing: Listing) -> Result<()> {
    let kind = listing.kind;
    if listing.superseded {
        check_on()?;
        let project = memory::project_of(&dir_or_current(dir)?);
        let superseded = Store::open(socket)?.superseded(&project)?;
        let superseded = superseded
            .iter()
            .filter(|was| kind.is_none_or(|kind| was.entry.kind == kind));
        for was in superseded {
            outln!("{}", printable::line(&superseded_line(was, now())))?;
        }
        return Ok(());
    }
    if listing.forgotten {
        check_on()?;
        let project = memory::project_of(&dir_or_current(dir)?);
        let forgotten = Store::open(socket)?.forgotten(&project)?;
        let forgotten = forgotten
            .iter()
            .filter(|entry| kind.is_none_or(|kind| entry.kind == kind));
        for entry in forgotten {
            outln!("{}", printable::line(&forgotten_line(entry, now())))?;
        }
        return Ok(());
    }
    check_on()?;
    let listed = listed(socket, &dir_or_current(dir)?)?;
    let now = now();
    let listed: Vec<&Listed> = listed
        .iter()
        .filter(|item| kind.is_none_or(|kind| item.entry.kind == kind))
        .filter(|item| !listing.status || is_status(&item.entry))
        .filter(|item| !listing.expired || item.entry.expired(now))
        .collect();
    print_entries(&listed)
}

/// Whether `entry` reads as progress or status rather than a lesson, by
/// [`memory::reads_as_status`]: tasks' outcomes, all status, have a kind
/// of their own to list them by, and expire, so they aren't counted.
fn is_status(entry: &Entry) -> bool {
    entry.kind != Kind::Outcome && memory::reads_as_status(&entry.text)
}

/// Prints entry `id` in full. An agent in a session reading it uses it,
/// which keeps it from expiring.
pub fn show(socket: &Path, dir: Option<PathBuf>, id: u64) -> Result<()> {
    check_on()?;
    let project = memory::project_of(&dir_or_current(dir)?);
    let used = env::own_session_id(socket).is_some();
    outln!("{}", shown(socket, &project, id, used)?)?;
    Ok(())
}

/// Entry `id` of `project`'s memory in full, as `crystal memory show` and
/// the `memory_show` tool give it, `used` when an agent reads it, which
/// keeps it from expiring; with what it said before it was last updated,
/// when it was. One that stopped holding is given as it was, with what
/// holds in its place; one merged into another says where it went.
pub fn shown(socket: &Path, project: &Path, id: u64, used: bool) -> Result<String> {
    let mut store = Store::open(socket)?;
    let entry = match used {
        true => store.used(project, id)?,
        false => store.get(project, id)?,
    };
    let was = store.last_superseded(project, id)?;
    let now = now();
    let Some(entry) = entry else {
        let Some(was) = was.filter(|was| !was.updated()) else {
            bail!("{}", no_entry(socket, project, id));
        };
        let holder = store.holding(project, id)?;
        return Ok(superseded_in_full(&was, holder.as_ref(), now));
    };
    let item = memory::checked(entry, project);
    let mut text = in_full(&item, now);
    if let Some(was) = was.filter(Superseded::updated) {
        let before = format!(
            "\nupdated {}: {}; it said before: {}",
            how_long_ago(was.superseded, now),
            was.why,
            memory::title(&was.entry.text)
        );
        text.push_str(&printable::text(&before));
    }
    Ok(text)
}

/// Why there's no entry `id` in `project`'s memory to show: none was ever
/// that, or it was merged into another, which says the same thing.
fn no_entry(socket: &Path, project: &Path, id: u64) -> String {
    let merged = Store::open(socket).and_then(|mut store| store.merged_into(project, id));
    match merged {
        Ok(Some(kept)) => format!("entry {id} was merged into {kept}, which says the same thing"),
        _ => format!("there's no entry {id}"),
    }
}

/// Prints the project's memory as markdown, newest first.
pub fn export(socket: &Path, dir: Option<PathBuf>) -> Result<()> {
    check_on()?;
    let dir = dir_or_current(dir)?;
    let project = memory::project_of(&dir);
    let name = project.file_name().unwrap_or_default();
    let listed = listed(socket, &dir)?;
    out!("{}", memory::markdown(&name.to_string_lossy(), &listed))?;
    Ok(())
}

/// Every entry of the memory of the project `dir` is in, newest first,
/// each with whether it still holds: from the daemon, which keeps each
/// worktree's words from one look to the next, or read here, when there's
/// no daemon to ask.
fn listed(socket: &Path, dir: &Path) -> Result<Vec<Listed>> {
    let request = Request::ListMemory {
        dir: dir.to_path_buf(),
    };
    if let Ok(Some(Response::Memory { entries })) = client::ask(socket, &request, false) {
        return Ok(entries);
    }
    Ok(Memory::read(socket, &memory::project_of(dir))?.listed())
}

/// What `crystal memory search` keeps to besides its words.
#[derive(Debug, Default)]
pub struct SearchArgs {
    pub kind: Option<Kind>,
    /// Files or directories, as given from the current directory.
    pub files: Vec<String>,
    /// Leave the stale out.
    pub fresh: bool,
    /// The expired too.
    pub all: bool,
    pub limit: Option<usize>,
}

/// Prints the entries that have to do with `words`, the best first, as
/// `args` says: those that hold before the stale, or with `fresh`, the
/// stale left out; the expired left out unless it says all.
pub fn search(
    socket: &Path,
    dir: Option<PathBuf>,
    words: &[String],
    args: SearchArgs,
) -> Result<()> {
    check_on()?;
    let dir = dir_or_current(dir)?;
    let settings = Config::load()?.memory;
    let downloaded = embed::models_dir().is_some_and(|dir| embed::is_downloaded(&dir));
    if settings.embeddings && settings.embedder == Embedder::Local && !downloaded {
        errln!(
            "the models that search by meaning aren't downloaded yet, so this goes by words \
             alone: the daemon gets them as it starts, or `crystal memory embed` does now"
        );
    }
    let top = top_of(&dir);
    let wanted = Wanted {
        kind: args.kind,
        files: args
            .files
            .iter()
            .map(|file| from_top(file, &dir, &top))
            .collect(),
        fresh: args.fresh,
        expired: args.all,
        limit: args.limit.unwrap_or(memory::SEARCH_LIMIT).max(1),
    };
    let found = found(socket, &dir, &words.join(" "), &wanted)?;
    print_entries(&found.iter().collect::<Vec<_>>())
}

/// The entries of the memory of the project `dir` is in that have to do
/// with `query`, as `wanted` says, the best first: searched by the daemon,
/// which keeps the model that searches by meaning loaded, or here, when
/// there's no daemon to ask.
pub fn found(socket: &Path, dir: &Path, query: &str, wanted: &Wanted) -> Result<Vec<Listed>> {
    let request = Request::SearchMemory {
        dir: dir.to_path_buf(),
        query: query.to_string(),
        wanted: wanted.clone(),
    };
    if let Ok(Some(Response::Memory { entries })) = client::ask(socket, &request, false) {
        return Ok(entries);
    }
    let project = memory::project_of(dir);
    let embedder = embed::shared_now();
    let mut store = Store::open(socket)?;
    store.find(&project, query, wanted, embed::as_embed(&embedder))
}

/// Which entries `crystal memory rm` forgets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Forgetting {
    /// These, by their ids.
    pub ids: Vec<u64>,
    /// Every one that reads as status, as `list --status` lists them, of
    /// `kind` if it's given.
    pub status: bool,
    pub kind: Option<Kind>,
    /// With `status`, forget them: without it, they're only listed, as what
    /// would be forgotten.
    pub yes: bool,
}

/// Forgets the entries `forgetting` says, and says which: each of its ids,
/// once every one is there, or with `status`, every entry that reads as
/// status, listed first and forgotten only once it says yes.
pub fn remove(socket: &Path, dir: Option<PathBuf>, forgetting: Forgetting) -> Result<()> {
    check_on()?;
    let project = memory::project_of(&dir_or_current(dir)?);
    let mut store = Store::open(socket)?;
    let ids = if forgetting.status {
        let listed = memory::marked(store.entries(&project)?, &project);
        let listed: Vec<&Listed> = listed
            .iter()
            .filter(|item| forgetting.kind.is_none_or(|kind| item.entry.kind == kind))
            .filter(|item| is_status(&item.entry))
            .collect();
        if !forgetting.yes {
            print_entries(&listed)?;
            match listed.len() {
                0 => outln!("nothing reads as status")?,
                n => outln!("would forget these {n}: add --yes to forget them")?,
            }
            return Ok(());
        }
        listed.iter().map(|item| item.entry.id).collect()
    } else {
        let mut missing = Vec::new();
        for &id in &forgetting.ids {
            if store.get(&project, id)?.is_none() {
                missing.push(id.to_string());
            }
        }
        if !missing.is_empty() {
            bail!("there's no entry {}", missing.join(", "));
        }
        forgetting.ids
    };
    for id in ids {
        let entry = store.remove(&project, id)?;
        outln!("forgot {}: {}", entry.id, entry.text)?;
        let forgotten = Event::memory(events::Kind::MemoryForgotten, project.clone(), entry);
        tell(socket, forgotten);
    }
    Ok(())
}

/// What `crystal memory kind` changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kinding {
    /// The kind to give `ids`.
    pub kind: Option<Kind>,
    pub ids: Vec<u64>,
    /// Have the distiller's model read the notes for the lessons among
    /// them instead.
    pub notes: bool,
    /// With `notes`, give each the kind it says: without it, they're only
    /// listed, as what would change.
    pub yes: bool,
}

/// Changes the kind of the entries `kinding` says, and says which: each of
/// its ids, once every one is there, given its kind; or with `notes`, the
/// notes the distiller's model reads as lessons, listed with the kind it
/// gives each, and given it only once it says yes. What an entry says, its
/// vector and its place in the index stay as they are.
pub fn kind(socket: &Path, dir: Option<PathBuf>, kinding: Kinding) -> Result<()> {
    check_on()?;
    let dir = dir_or_current(dir)?;
    let project = memory::project_of(&dir);
    let mut store = Store::open(socket)?;
    let changes: Vec<(u64, Kind)> = if kinding.notes {
        // Those that read as status are no lessons: `list --status` has them.
        let (status, notes): (Vec<Entry>, Vec<Entry>) = store
            .entries(&project)?
            .into_iter()
            .filter(|entry| entry.kind == Kind::Note)
            .partition(|entry| memory::reads_as_status(&entry.text));
        if notes.is_empty() {
            outln!("there are no notes but those that read as status")?;
            return Ok(());
        }
        let settings = Config::load()?.memory;
        let count = notes.len();
        let model = &settings.distill_model;
        errln!(
            "asking {model} which of the {count} notes are lessons ({} that read as status left \
             out: `list --status` lists them)…",
            status.len()
        );
        let env = std::env::vars().collect();
        let read = distill::lessons_among(&notes, &settings, &top_of(&dir), &env)?;
        for why in &read.rejected {
            errln!("  rejected {why}");
        }
        errln!("(${:.4})", read.cost_usd);
        if !kinding.yes {
            for &(id, kind) in &read.lessons {
                let note = notes.iter().find(|note| note.id == id);
                let title = note
                    .map(|note| memory::title(&note.text))
                    .unwrap_or_default();
                let line = format!("{id:>4}  note → {:<8}  {title}", kind.to_string());
                outln!("{}", printable::line(&line))?;
            }
            match read.lessons.len() {
                0 => outln!("none of the notes reads as a lesson")?,
                1 => outln!("would make this note a lesson: add --yes to make it one")?,
                n => outln!("would make these {n} notes lessons: add --yes to make them")?,
            }
            return Ok(());
        }
        read.lessons
    } else {
        let kind = kinding.kind.expect("clap asks for a kind with ids");
        let mut missing = Vec::new();
        for &id in &kinding.ids {
            if store.get(&project, id)?.is_none() {
                missing.push(id.to_string());
            }
        }
        if !missing.is_empty() {
            bail!("there's no entry {}", missing.join(", "));
        }
        kinding.ids.iter().map(|&id| (id, kind)).collect()
    };
    for (id, kind) in changes {
        let entry = store.set_kind(&project, id, kind)?;
        let line = format!("{id} is a {kind} now: {}", memory::title(&entry.text));
        outln!("{}", printable::line(&line))?;
        let changed = Event::memory(events::Kind::MemoryChanged, project.clone(), entry);
        tell(socket, changed);
    }
    Ok(())
}

/// Tells the daemon what was done to the memory, for its event log. The
/// memory has it either way, so a daemon that can't be told is no reason
/// to fail.
fn tell(socket: &Path, event: Event) {
    if let Err(err) = client::tell(socket, event) {
        errln!("crystal: couldn't tell the daemon: {err:#}");
    }
}

/// Gives every entry of every project its vector from what searches by
/// meaning, and says how many that was: with Gemini, from it first, and
/// what Google counted; then downloads the models here if they aren't yet,
/// which with Gemini are the reranker and what a search falls back on.
pub fn embed(socket: &Path) -> Result<()> {
    check_on()?;
    let settings = Config::load()?.memory;
    let gemini = settings.embedder == Embedder::Gemini;
    if gemini {
        let remote = embed::gemini_alone(&settings);
        let counted = |remote: &embed::Remote| remote.status().tokens;
        let before = counted(&remote);
        let count = Store::open(socket)?.embed_missing(&remote)?;
        let tokens = counted(&remote) - before;
        outln!(
            "embedded {count} entries with {}: {tokens} tokens, {}",
            remote.model(),
            gemini::cost(tokens)
        )?;
    }
    let root = match embed::models_dir().filter(|root| embed::is_downloaded(root)) {
        Some(root) => root,
        None => {
            errln!("downloading {} ({} MB)", embed::names(), embed::size_mb());
            embed::download(std::io::stderr().is_terminal())?
        }
    };
    if !gemini {
        // Embedding needs only the one model.
        let models = Models::load(&root, false)?;
        let count = Store::open(socket)?.embed_missing(&models)?;
        outln!("embedded {count} entries with {}", embed::MODEL)?;
    }
    if !settings.embeddings {
        outln!(
            "searches use it once `embeddings = true` is under `[memory]` in {}",
            crate::config::path().display()
        )?;
    }
    Ok(())
}

/// Says how search by meaning stands: as the daemon has it, which keeps
/// what Gemini last answered, or as it is here with no daemon.
pub fn status(socket: &Path) -> Result<()> {
    check_on()?;
    let settings = Config::load()?.memory;
    let status = match client::ask(socket, &Request::EmbeddingStatus, false) {
        Ok(Some(Response::EmbeddingStatus(status))) => status,
        _ => embed::status(socket, &settings)?,
    };
    for (label, said) in status_lines(&settings, &status) {
        outln!("{label:<18}{said}")?;
    }
    Ok(())
}

/// The lines `crystal memory status` prints, each a label and what it says.
fn status_lines(settings: &MemorySettings, status: &embed::Status) -> Vec<(&'static str, String)> {
    let mut lines = Vec::new();
    let downloaded = status.is_downloaded();
    let models = match (&status.preparing, downloaded, status.loaded) {
        (Some(doing), false, _) => format!(
            "{doing}: {} of {} MB",
            status.on_disk / 1_000_000,
            status.size / 1_000_000
        ),
        (Some(doing), true, _) => format!("{doing}…"),
        (None, false, _) => format!(
            "not downloaded ({} MB): `crystal memory embed` gets them",
            status.size / 1_000_000
        ),
        (None, true, true) => "downloaded, loaded".to_string(),
        (None, true, false) => "downloaded, loaded once a search needs them".to_string(),
    };
    if !settings.embeddings {
        lines.push((
            "search by meaning",
            "off: by words alone (`embeddings = false` under [memory])".to_string(),
        ));
    } else if let Some(gemini) = &status.gemini {
        lines.push((
            "search by meaning",
            format!(
                "by {gemini_model} through Google's Gemini API: entries' text and searches go \
                 to Google",
                gemini_model = gemini.model
            ),
        ));
        let key = match (&gemini.key, gemini.key_shared) {
            (Some(key), false) => key.clone(),
            (Some(key), true) => format!("{key} (others can read it: chmod 600 it)"),
            (None, _) => "none found".to_string(),
        };
        lines.push(("key", key));
        let meanwhile = match downloaded {
            true => format!("searches go by {} meanwhile", embed::MODEL),
            false => "searches go by words meanwhile".to_string(),
        };
        let said = match (&gemini.failed, gemini.failed_secs_ago) {
            (Some(failed), Some(secs)) => {
                let when = match ago(0, secs) {
                    now if now == "now" => "just now".to_string(),
                    ago => format!("{ago} ago"),
                };
                format!("failed {when}: {failed}; {meanwhile}")
            }
            (Some(failed), None) => format!("{failed}; {meanwhile}"),
            (None, _) if gemini.tokens == 0 => "nothing asked yet".to_string(),
            (None, _) => format!(
                "working: {} tokens sent since the daemon started, {}",
                gemini.tokens,
                gemini::cost(gemini.tokens)
            ),
        };
        lines.push(("gemini", said));
    } else {
        lines.push((
            "search by meaning",
            format!("by {}, on this machine", embed::MODEL),
        ));
    }
    if settings.embeddings {
        lines.push((
            "entries",
            format!(
                "{} of {} have their vector from {}",
                status.embedded, status.entries, status.embedder
            ),
        ));
    }
    lines.push(("models here", format!("{}: {models}", embed::names())));
    if let Some(failed) = &status.failed {
        lines.push(("", format!("couldn't get them ready: {failed}")));
    }
    let rerank = match settings.rerank {
        true => "the reranker reads the best of each search again",
        false => "off (`rerank = false` under [memory])",
    };
    lines.push(("rerank", rerank.to_string()));
    lines
}

/// Has the daemon run the distiller over what the session `name` did, and
/// says what came of it.
pub fn distill(socket: &Path, name: &str) -> Result<()> {
    check_on()?;
    let request = Request::Distill {
        name: name.to_string(),
    };
    let Some(Response::Distilled(report)) = client::ask(socket, &request, false)? else {
        bail!("there's no session called {name}: no daemon is running");
    };
    outln!("distilled {name}: {}", report.line())?;
    for why in &report.rejected {
        outln!("  rejected {why}")?;
    }
    Ok(())
}

/// Writes entry `id` into the project's CLAUDE.md or AGENTS.md, after the
/// user says yes, unless `yes` says it already.
pub fn promote(socket: &Path, dir: Option<PathBuf>, id: u64, yes: bool) -> Result<()> {
    let memory = read(socket, dir)?;
    let Some(entry) = memory.get(id) else {
        bail!("there's no entry {id}");
    };
    if !yes
        && !confirm(&format!(
            "Add entry {id} to the project's instructions file?"
        ))?
    {
        outln!("left as it was")?;
        return Ok(());
    }
    let file = memory::promote(&memory.project, entry)?;
    outln!("added entry {id} to {}", file.display())?;
    let promoted = Event::promoted(memory.project.clone(), entry.clone(), file);
    tell(socket, promoted);
    Ok(())
}

fn read(socket: &Path, dir: Option<PathBuf>) -> Result<Memory> {
    check_on()?;
    let project = memory::project_of(&dir_or_current(dir)?);
    Memory::read(socket, &project)
}

/// Every memory command goes through here first: with memory off, they
/// say so rather than doing anything.
fn check_on() -> Result<()> {
    crate::plugins::ensure_enabled(&Config::load()?, "memory")
}

fn dir_or_current(dir: Option<PathBuf>) -> Result<PathBuf> {
    match dir {
        Some(dir) => Ok(std::path::absolute(dir)?),
        None => Ok(std::env::current_dir()?),
    }
}

/// Who's remembering: the session this runs in, when it runs in one of
/// this daemon's, or else the user.
fn source(socket: &Path) -> Source {
    let session = std::env::var("CRYSTAL_SESSION").ok();
    match (env::own_session_id(socket), session) {
        (Some(_), Some(name)) => Source::Session(name),
        _ => Source::User,
    }
}

/// The top of the worktree `dir` is in, or `dir` itself outside git.
fn top_of(dir: &Path) -> PathBuf {
    Checkout::find(dir).map_or_else(|| dir.to_path_buf(), |c| c.worktree().path)
}

/// `file`, given from `dir`, from `top`, the top of its worktree: the same
/// in every worktree of the project. A file outside the worktree stays as
/// it was given.
fn from_top(file: &str, dir: &Path, top: &Path) -> String {
    let path = dir.join(file);
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    match path.strip_prefix(top) {
        Ok(relative) => relative.to_string_lossy().into_owned(),
        Err(_) => file.to_string(),
    }
}

/// One line an entry: its id, kind and age, then its text, marked when
/// it's drifting, stale or expired.
fn print_entries(entries: &[&Listed]) -> Result<()> {
    let now = now();
    for item in entries {
        let entry = &item.entry;
        let expired = entry.expired(now).then_some("expired");
        let marks = item.freshness.mark().into_iter().chain(expired);
        let mark: String = marks.map(|mark| format!("  [{mark}]")).collect();
        let files = if entry.files.is_empty() {
            String::new()
        } else {
            format!("  ({})", entry.files.join(", "))
        };
        let line = format!(
            "{:>4}  {:<8}  {:>4}  {}{files}{mark}",
            entry.id,
            entry.kind.to_string(),
            ago(entry.created, now),
            memory::title(&entry.text),
        );
        // Its text is kept clean, but not its files, nor what was kept
        // before that.
        outln!("{}", printable::line(&line))?;
    }
    Ok(())
}

/// A forgotten entry on one line: the id it had, its kind, how long ago it
/// was forgotten, its title and its files.
fn forgotten_line(entry: &Forgotten, now: u64) -> String {
    let files = if entry.files.is_empty() {
        String::new()
    } else {
        format!("  ({})", entry.files.join(", "))
    };
    format!(
        "{:>4}  {:<8}  {:>4}  {}{files}",
        entry.id,
        entry.kind.to_string(),
        ago(entry.forgotten, now),
        memory::title(&entry.text),
    )
}

/// An entry that stopped holding on one line: its id, its kind, how long
/// ago it stopped holding and its title, then what holds in its place, or
/// that it was updated, and why.
fn superseded_line(was: &Superseded, now: u64) -> String {
    let entry = &was.entry;
    let how = match was.updated() {
        true => "updated".to_string(),
        false => format!("→ {}", was.by),
    };
    format!(
        "{:>4}  {:<8}  {:>4}  {}  [{how}: {}]",
        entry.id,
        entry.kind.to_string(),
        ago(was.superseded, now),
        memory::title(&entry.text),
        was.why,
    )
}

/// A retired entry in full, as `crystal memory show` and the `memory_show`
/// tool give it: its id and kind, when it stopped holding and why, what
/// holds in its place, `holder` as it is now, then what it said, as it was,
/// and how to put it back.
fn superseded_in_full(was: &Superseded, holder: Option<&Entry>, now: u64) -> String {
    let entry = &was.entry;
    let mut text = format!(
        "{} · {} · retired {}: {}",
        entry.id,
        entry.kind,
        how_long_ago(was.superseded, now),
        was.why
    );
    match holder {
        Some(holder) => text.push_str(&format!(
            "\n{} holds in its place: {}",
            holder.id,
            memory::title(&holder.text)
        )),
        None => text.push_str(&format!(
            "\n{} held in its place, and has gone since",
            was.by
        )),
    }
    text.push_str(&format!("\n\n{}\n", entry.text));
    if !entry.files.is_empty() {
        text.push_str(&format!("\nfiles: {}", entry.files.join(", ")));
    }
    text.push_str(&format!("\nfrom: {}", entry.source));
    text.push_str(&format!(
        "\nadded {} ago; `crystal memory restore {}` puts it back",
        ago(entry.created, now),
        entry.id
    ));
    printable::text(&text).into_owned()
}

/// An entry in full, as `crystal memory show` and the `memory_show` tool
/// give it: its id and kind, how it holds when it's drifting or stale, and
/// what's gone, whether it's expired, its text, its files, where it came
/// from, how often and how lately it was said, and when an agent last read
/// it in full.
pub fn in_full(item: &Listed, now: u64) -> String {
    let entry = &item.entry;
    let mut text = format!("{} · {}", entry.id, entry.kind);
    if let Some(holds) = item.how_it_holds() {
        text.push_str(&format!(" · {holds}"));
    }
    if entry.expired(now) {
        text.push_str(" · expired: nobody has found it again, so searches leave it out");
    }
    text.push_str(&format!("\n\n{}\n", entry.text));
    if !entry.files.is_empty() {
        text.push_str(&format!("\nfiles: {}", entry.files.join(", ")));
    }
    text.push_str(&format!("\nfrom: {}", entry.source));
    let times = if entry.seen == 1 {
        "once".to_string()
    } else {
        format!("{} times", entry.seen)
    };
    text.push_str(&format!(
        "\nadded {} ago; said {times}, last {} ago",
        ago(entry.created, now),
        ago(entry.last_seen, now)
    ));
    if let Some(used) = entry.used {
        let when = how_long_ago(used, now);
        text.push_str(&format!("; an agent read it in full {when}"));
    }
    printable::text(&text).into_owned()
}

/// How long before `now` `time` was, in words: `3h ago`, or `just now`.
fn how_long_ago(time: u64, now: u64) -> String {
    match ago(time, now).as_str() {
        "now" => "just now".to_string(),
        age => format!("{age} ago"),
    }
}

/// Asks `question` at the terminal, and says whether the answer was yes.
/// Away from a terminal there's nobody to answer, so it's an error.
pub fn confirm(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("not at a terminal to ask: add --yes to go ahead");
    }
    err!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

fn now() -> u64 {
    memory::seconds_since_epoch(SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Twin;

    fn entry(id: u64, kind: Kind, text: &str) -> Entry {
        Entry {
            id,
            kind,
            text: text.into(),
            files: Vec::new(),
            source: Source::User,
            created: 0,
            seen: 1,
            last_seen: 0,
            anchors: Default::default(),
            checkout: None,
            used: None,
            names: Vec::new(),
            counted_from: None,
        }
    }

    #[test]
    fn dedupe_lists_each_one_kept_with_what_goes_into_it() {
        let merges = [Merge {
            kept: entry(3, Kind::Gotcha, "The ledger needs redis\n\nStart it first."),
            merged: vec![
                Twin {
                    entry: entry(7, Kind::Note, "redis has to run for the ledger"),
                    alike: 0.934,
                    reranked: Some(0.5),
                },
                Twin {
                    entry: entry(9, Kind::Command, "make redis \x1b[31mfirst"),
                    alike: 0.95,
                    reranked: None,
                },
            ],
        }];
        assert_eq!(
            merges_text(&merges, false),
            "   3  gotcha    The ledger needs redis\n\
             \x20 ← 0.93     7  note      redis has to run for the ledger\n\
             \x20 ← 0.95     9  command   make redis [31mfirst\n\
             2 entries would go into 1: --apply merges them\n"
        );
        assert!(merges_text(&merges, true).ends_with("\nmerged 2 entries into 1\n"));
        assert_eq!(
            merges_text(&[], false),
            "no two entries say the same thing\n"
        );
    }

    #[test]
    fn the_status_says_what_makes_vectors_where_gemini_s_key_is_and_how_it_went() {
        let mut settings = MemorySettings::default();
        let status = embed::Status {
            size: 2_449_000_000,
            entries: 512,
            embedded: 512,
            embedder: embed::MODEL.into(),
            ..embed::Status::default()
        };
        let said = |settings: &MemorySettings, status: &embed::Status| {
            status_lines(settings, status)
                .iter()
                .map(|(label, said)| format!("{label:<18}{said}\n"))
                .collect::<String>()
        };
        let here = said(&settings, &status);
        assert!(
            here.contains(
                "search by meaning by jinaai/jina-embeddings-v5-text-small, on this machine"
            ),
            "{here}"
        );
        assert!(
            here.contains("512 of 512 have their vector from jinaai/"),
            "{here}"
        );
        assert!(here.contains("not downloaded (2449 MB)"), "{here}");
        settings.embedder = Embedder::Gemini;
        let working = gemini::Status {
            model: "gemini-embedding-2@768".into(),
            key: Some("~/.config/crystal/gemini.key".into()),
            tokens: 31_691,
            ..gemini::Status::default()
        };
        let through = embed::Status {
            on_disk: 2_449_000_000,
            loaded: true,
            embedder: working.model.clone(),
            gemini: Some(working.clone()),
            ..status.clone()
        };
        let gemini = said(&settings, &through);
        assert!(
            gemini.contains("by gemini-embedding-2@768 through Google's Gemini API: entries' text"),
            "{gemini}"
        );
        assert!(gemini.contains("key               ~/.config/crystal/gemini.key\n"));
        assert!(gemini.contains("working: 31691 tokens sent"), "{gemini}");
        assert!(gemini.contains("downloaded, loaded"), "{gemini}");
        let failed = embed::Status {
            gemini: Some(gemini::Status {
                failed: Some("gemini-embedding-2: Quota exceeded. (RESOURCE_EXHAUSTED)".into()),
                failed_secs_ago: Some(180),
                key_shared: true,
                ..working
            }),
            ..through
        };
        let failing = said(&settings, &failed);
        assert!(
            failing.contains(
                "failed 3m ago: gemini-embedding-2: Quota exceeded. (RESOURCE_EXHAUSTED); \
                 searches go by jinaai/jina-embeddings-v5-text-small meanwhile"
            ),
            "{failing}"
        );
        assert!(
            failing.contains("(others can read it: chmod 600 it)"),
            "{failing}"
        );
        settings.embeddings = false;
        let off = said(&settings, &failed);
        assert!(off.contains("off: by words alone"), "{off}");
        assert!(!off.contains("have their vector"), "{off}");
    }

    #[test]
    fn reconcile_lists_what_no_longer_holds_with_what_holds_in_its_place() {
        let entries = [
            entry(2, Kind::Gotcha, "Idle stop is off by default"),
            entry(
                7,
                Kind::Decision,
                "Idle stop is on by default\n\nSince #103.",
            ),
            entry(9, Kind::Command, "crystal ls lists the sessions"),
        ];
        let proposals = [
            Proposal {
                id: 2,
                was: "Idle stop is off by default".into(),
                change: Change::Retire { by: 7 },
                why: "the default flipped".into(),
            },
            Proposal {
                id: 9,
                was: "crystal ls lists the sessions".into(),
                change: Change::Update {
                    text: "crystal list lists\nthe sessions".into(),
                },
                why: "ls was renamed".into(),
            },
        ];
        assert_eq!(
            proposals_text(&proposals, &entries),
            "   2  gotcha    Idle stop is off by default\n\
             \x20 → 7       Idle stop is on by default\n\
             \x20   why: the default flipped\n\
             \x20  9  command   crystal ls lists the sessions\n\
             \x20 → update  crystal list lists the sessions\n\
             \x20   why: ls was renamed\n\
             would retire 1 and update 1: --apply makes it so, and `crystal memory restore <id>` \
             puts one back\n"
        );
        assert_eq!(proposals_text(&[], &entries), "every entry holds\n");
    }

    #[test]
    fn what_stopped_holding_is_listed_and_shown_with_what_holds_in_its_place() {
        let retired = Superseded {
            entry: Entry {
                files: vec!["src/daemon.rs".into()],
                ..entry(
                    2,
                    Kind::Gotcha,
                    "Idle stop is off by default\n\nSo sessions run on.",
                )
            },
            by: 7,
            why: "the default flipped".into(),
            superseded: 7_200,
        };
        assert_eq!(
            superseded_line(&retired, 10_800),
            "   2  gotcha      1h  Idle stop is off by default  [→ 7: the default flipped]"
        );
        let updated = Superseded {
            by: 2,
            ..retired.clone()
        };
        assert!(superseded_line(&updated, 10_800).ends_with("[updated: the default flipped]"));
        let holder = entry(7, Kind::Decision, "Idle stop is on by default");
        assert_eq!(
            superseded_in_full(&retired, Some(&holder), 10_800),
            "2 · gotcha · retired 1h ago: the default flipped\n\
             7 holds in its place: Idle stop is on by default\n\n\
             Idle stop is off by default\n\nSo sessions run on.\n\n\
             files: src/daemon.rs\n\
             from: you\n\
             added 3h ago; `crystal memory restore 2` puts it back"
        );
        assert!(
            superseded_in_full(&retired, None, 10_800)
                .contains("\n7 held in its place, and has gone since\n")
        );
    }
}
