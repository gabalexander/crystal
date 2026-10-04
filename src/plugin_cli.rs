//! `crystal plugin`: listing plugins and the events they hear, switching
//! them on and off, the user's own or a project's for that project,
//! running their actions, trying their hooks, opening their panes, and
//! installing, building, making and removing them.

use crate::client;
use crate::config::{self, Config};
use crate::env;
use crate::events::{self, Event, Kind};
use crate::layout;
use crate::memory_cli::confirm;
use crate::plugin_hooks;
use crate::plugin_manifest::{self, Manifest, PaneSpec, Placement};
use crate::plugins::{self, Context, Id, Installed};
use crate::printable;
use crate::project;
use crate::project_cli;
use crate::protocol::{NewSession, Request, Response, SessionInfo};
use crate::shell;
use crate::tui::keymap::{Extent, SplitWay};
use crate::tui::split_tree::Way;
use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::Value;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The plugin a command names: the user's own called `name`, or with
/// `project`, the one called that which the project of `dir`, or else of
/// the current directory, ships.
pub fn id(name: &str, project: bool, dir: Option<PathBuf>) -> Result<Id> {
    if !project {
        return Ok(Id::own(name));
    }
    let dir = match dir {
        Some(dir) => dir,
        None => std::env::current_dir()?,
    };
    Ok(Id::of_project(name, &project::of(&dir).path))
}

/// Prints every plugin, crystal's own first, with whether it's on; then
/// those the project of `dir`, or else of the current directory, ships.
pub fn list(socket: &Path, dir: Option<PathBuf>) -> Result<()> {
    let config = Config::load()?;
    let mut rows: Vec<[String; 4]> = plugins::BUILT_IN
        .iter()
        .map(|plugin| {
            [
                plugin.name.to_string(),
                on_off(plugins::enabled(&config, plugin.name)),
                "built-in".to_string(),
                plugin.description.to_string(),
            ]
        })
        .collect();
    for plugin in plugins::installed() {
        rows.push(row(&config, socket, plugin));
    }
    crate::print_table(["NAME", "STATE", "VERSION", "DESCRIPTION"], &rows);
    let dir = match dir {
        Some(dir) => dir,
        None => std::env::current_dir()?,
    };
    let project = project::of(&dir);
    let shipped = plugins::of_project(&project.path);
    if !shipped.is_empty() {
        let place = shell::home_relative(&project.path.join(plugins::PROJECT_DIR));
        println!();
        println!("{}'s own, in {place}, each on for it alone:", project.name);
        let rows: Vec<[String; 4]> = shipped
            .into_iter()
            .map(|plugin| row(&config, socket, plugin))
            .collect();
        crate::print_table(["NAME", "STATE", "VERSION", "DESCRIPTION"], &rows);
    }
    if plugins::enabled(&config, "notifications") && !config.notify {
        println!();
        println!("{}", NOTIFY_OFF);
    }
    Ok(())
}

fn on_off(on: bool) -> String {
    if on { "on" } else { "off" }.to_string()
}

/// A plugin's row in the list: its name, how it stands, its version and
/// what it does, or why it can't run.
fn row(config: &Config, socket: &Path, plugin: Installed) -> [String; 4] {
    let id = plugin.id();
    let mut state = on_off(plugins::is_on(config, &id));
    if state == "on" && plugins::paused(socket, &id).is_some() {
        state = "paused".to_string();
    }
    let blocked = plugin.blocked();
    match plugin.manifest {
        Ok(manifest) => {
            let (state, about) = match blocked {
                Some(why) if manifest.unfit().is_some() => ("unsupported".to_string(), why),
                Some(why) => ("unbuilt".to_string(), why),
                None => (state, manifest.description),
            };
            [plugin.name, state, manifest.version, about]
        }
        Err(why) => [plugin.name, "broken".to_string(), "-".to_string(), why],
    }
}

/// `crystal plugin events`: every event a plugin's hooks can hear, and
/// when it happens.
pub fn events() {
    let rows: Vec<[String; 2]> = Kind::ALL
        .iter()
        .map(|kind| [kind.name().to_string(), kind.about().to_string()])
        .collect();
    crate::print_table(["EVENT", "WHEN"], &rows);
}

/// What's said when the notifications plugin is on but the older `notify`
/// setting keeps them off.
const NOTIFY_OFF: &str = "`notify = false` in the config keeps notifications off too: \
                          take it out to hear from crystal";

/// `crystal plugin enable` and `disable`: the user's own, or a project's
/// for that project, which first shows what it runs and asks, unless `yes`
/// says to go ahead, and builds it.
pub fn switch(socket: &Path, id: &Id, on: bool, yes: bool) -> Result<()> {
    match &id.project {
        Some(project) => switch_projects(socket, id, project, on, yes),
        None => switch_own(socket, &id.name, on),
    }
}

/// Switches a project's plugin, `id`, on or off for its project.
fn switch_projects(socket: &Path, id: &Id, project: &Path, on: bool, yes: bool) -> Result<()> {
    let plugin = find(id)?;
    let label = id.label();
    let path = config::path();
    if !on {
        plugins::switch(&path, socket, id, false)?;
        println!("the {label} plugin is off");
        return Ok(());
    }
    // On already: switched on again, it runs again after a pause.
    if plugins::is_on(&Config::load()?, id) {
        plugins::switch(&path, socket, id, true)?;
        println!("the {label} plugin is on");
        return Ok(());
    }
    let manifest = manifest_of(&plugin)?;
    if let Some(why) = manifest.unfit() {
        bail!("{label} can't be turned on: it {why}");
    }
    describe(manifest, &shell::home_relative(&plugin.dir));
    let keyed = manifest.actions.iter().any(|action| action.key.is_some());
    if keyed || !manifest.link_handlers.is_empty() {
        println!(
            "Its actions' keys and its link handlers aren't used: a project's plugin has none."
        );
        println!();
    }
    let name = crate::project::name_of(project);
    if !yes && !confirm(&format!("Turn {} on for {name}?", id.name))? {
        println!("{label} is still off");
        return Ok(());
    }
    let config_dir = plugins::config_dir(id);
    fs::create_dir_all(&config_dir)
        .with_context(|| format!("couldn't make {}", config_dir.display()))?;
    if let Err(err) = build_and_note(socket, id, &plugin.dir, manifest) {
        bail!(
            "{err:#}\nso {label} is still off: once that's fixed, \
             `crystal plugin enable {} --project` tries again",
            id.name
        );
    }
    plugins::switch(&path, socket, id, true)?;
    println!("the {} plugin is on, for {name} alone", id.name);
    Ok(())
}

/// Switches one of crystal's own plugins, or one the user installed.
fn switch_own(socket: &Path, name: &str, on: bool) -> Result<()> {
    let installed = plugins::installed();
    let found = installed.iter().find(|plugin| plugin.name == name);
    if !plugins::is_built_in(name) && found.is_none() {
        bail!(
            "there's no plugin called {name} in {}: `crystal plugin` lists them",
            plugins::plugins_dir().display()
        );
    }
    if let (true, Some(plugin)) = (on, found) {
        plugins::check_can_enable(plugin, &installed)?;
    }
    plugins::set_enabled(&config::path(), socket, name, on)?;
    println!("the {name} plugin is {}", if on { "on" } else { "off" });
    if on && name == "notifications" && !Config::load()?.notify {
        println!("{NOTIFY_OFF}");
    }
    Ok(())
}

/// Refuses, saying how to turn it on, when the plugin `id` is off.
fn ensure_on(config: &Config, id: &Id) -> Result<()> {
    match &id.project {
        None => plugins::ensure_enabled(config, &id.name),
        Some(_) if plugins::is_on(config, id) => Ok(()),
        Some(project) => bail!(
            "the {} plugin is off for {}: turn it on with `crystal plugin enable {} --project`",
            id.name,
            crate::project::name_of(project),
            id.name
        ),
    }
}

/// The plugin `id`, which has to be on and able to run here, with its
/// manifest.
fn runnable(id: &Id) -> Result<(Installed, Manifest)> {
    let found = find(id)?;
    ensure_on(&Config::load()?, id)?;
    if let Some(why) = found.blocked() {
        bail!("{} can't run: {why}", id.label());
    }
    let manifest = manifest_of(&found)?.clone();
    Ok((found, manifest))
}

/// `crystal plugin run`: runs one of a plugin's actions here, in the
/// foreground, on `link` if it's given one, and returns its exit code.
pub fn run(
    socket: &Path,
    plugin: &Id,
    action: &str,
    session: Option<String>,
    link: Option<&str>,
) -> Result<i32> {
    let (found, manifest) = runnable(plugin)?;
    let Some(action) = manifest.action(action) else {
        let ids: Vec<&str> = manifest.actions.iter().map(|a| a.id.as_str()).collect();
        bail!(
            "{} has no action {action}; its actions: {}",
            plugin.label(),
            ids.join(", ")
        );
    };
    let context = Context {
        link: link.map(str::to_string),
        ..context_for(socket, session)?
    };
    let status = plugins::command(plugin, &found.dir, &action.command, socket, &context)
        .status()
        .with_context(|| format!("couldn't run {}", action.command.join(" ")))?;
    Ok(status.code().unwrap_or(1))
}

/// `crystal plugin run --link`: runs the action the plugin's first link
/// handler that takes `link` names, on it, as a Ctrl+click on it would, and
/// returns its exit code.
pub fn run_link(socket: &Path, plugin: &Id, link: &str, session: Option<String>) -> Result<i32> {
    let found = find(plugin)?;
    let manifest = manifest_of(&found)?;
    let handler = (manifest.link_handlers.iter())
        .find(|handler| handler.takes(link))
        .with_context(|| format!("{} has no link handler that takes {link}", plugin.label()))?;
    run(socket, plugin, &handler.action, session, Some(link))
}

/// `crystal plugin run --event` and `--json`: runs the plugin's hooks on an
/// event here, in the foreground, whether the plugin is on or not, and
/// returns the exit code of the first that fails, or 0. The event is the
/// kind `kind` names, or else the `event` of `json`: a made-up one with
/// everything its kind carries, about the session `session` names, or else
/// the one it's run in, or else a made-up one in the current directory,
/// with whatever `json` gives in place of what it made up. `json` is an
/// object of JSON, or `-` to read it from standard input, like a line of
/// `crystal events --json`.
pub fn run_event(
    socket: &Path,
    plugin: &Id,
    kind: Option<&str>,
    json: Option<&str>,
    session: Option<String>,
) -> Result<i32> {
    let found = find(plugin)?;
    let manifest = manifest_of(&found)?;
    let given = json.map(read_json).transpose()?;
    let named = given.as_ref().and_then(|given| given["event"].as_str());
    let kind = kind
        .or(named)
        .context("say which event: --event, or the `event` of the JSON")?;
    events::check_pattern(kind)?;
    let kind =
        Kind::named(kind).with_context(|| format!("say one event, not a pattern like `{kind}`"))?;
    let session = session_for(socket, session)?;
    let mut event = events::example(kind, session.as_ref(), &std::env::current_dir()?);
    if let Some(given) = given {
        event = given_over(&event, given)?;
    }
    let hooks: Vec<_> = plugin_hooks::hooks_on(manifest, &event).collect();
    if hooks.is_empty() {
        bail!("{} has no hook on {}", plugin.label(), kind.name());
    }
    for hook in hooks {
        let status = plugin_hooks::run_here(plugin, &found.dir, &hook.command, socket, &event)?;
        if !status.success() {
            return Ok(status.code().unwrap_or(1));
        }
    }
    Ok(0)
}

/// The JSON object `text` is, or with `-`, standard input is.
fn read_json(text: &str) -> Result<Value> {
    let text = if text == "-" {
        let mut read = String::new();
        std::io::stdin().read_to_string(&mut read)?;
        read
    } else {
        text.to_string()
    };
    let value: Value = serde_json::from_str(text.trim()).context("--json isn't JSON")?;
    if !value.is_object() {
        bail!("--json is an object, like {{\"session\":{{\"name\":\"docs\"}}}}");
    }
    Ok(value)
}

/// `event` with what `given` says in place of what it says: an object
/// given goes into the object there, and anything else takes its place.
/// Its kind stays.
fn given_over(event: &Event, given: Value) -> Result<Event> {
    fn merge(into: &mut Value, given: Value) {
        match (into, given) {
            (Value::Object(into), Value::Object(given)) => {
                for (key, value) in given {
                    merge(into.entry(key).or_insert(Value::Null), value);
                }
            }
            (into, given) => *into = given,
        }
    }
    let mut json = serde_json::to_value(event)?;
    merge(&mut json, given);
    json["event"] = Value::from(event.kind.name());
    serde_json::from_value(json).context("--json doesn't read as an event")
}

/// How `crystal plugin pane open` was asked to place a pane, in place of
/// what its manifest says.
#[derive(Debug, Default)]
pub struct Placing {
    pub placement: Option<Placement>,
    pub width: Option<String>,
    pub height: Option<String>,
    pub split: Option<SplitWay>,
}

/// `crystal plugin pane open`: starts one of a plugin's panes in a session
/// of its own and has the TUI used last show it: over its panes or in a
/// popup, which needs a TUI, or split off a session's pane, zoomed or in a
/// tab of its own, which the daemon does itself with none open. It's about
/// the session `session` names, or else the one it's run in; a split goes
/// beside that session's pane. Prints the session's name.
pub fn open_pane(
    socket: &Path,
    plugin: &Id,
    pane: &str,
    placing: Placing,
    session: Option<String>,
) -> Result<()> {
    let (found, manifest) = runnable(plugin)?;
    let Some(spec) = manifest.pane(pane) else {
        let ids: Vec<&str> = manifest.panes.iter().map(|p| p.id.as_str()).collect();
        bail!(
            "{} has no pane {pane}; its panes: {}",
            plugin.label(),
            ids.join(", ")
        );
    };
    let placement = placing.placement.unwrap_or(spec.placement);
    // What the manifest says of a size or a split goes with its placement.
    let from_spec = |given: Option<&String>, said: &Option<Extent>| match given {
        Some(given) => extent(given).map(Some),
        None => Ok(said.clone().filter(|_| placement == spec.placement)),
    };
    let spot = Spot {
        placement,
        width: from_spec(placing.width.as_ref(), &spec.width)?,
        height: from_spec(placing.height.as_ref(), &spec.height)?,
        split: (placing.split).or(spec.split.filter(|_| placement == spec.placement)),
    };
    let (width, height) = (spot.width.as_ref(), spot.height.as_ref());
    PaneSpec::check_placing(placement, width, height, spot.split)
        .map_err(|why| anyhow!("{why}"))?;

    let context = context_for(socket, session.clone())?;
    let mut environment = env::current();
    for (key, said) in plugins::env(socket, plugin, &found.dir, &context) {
        match said {
            Some(value) => environment.insert(key.to_string(), value),
            None => environment.remove(key),
        };
    }
    plugins::make_state_dir(socket, plugin);
    let base = format!("{}-{}", plugin.name, spec.id);
    let request = Request::New(NewSession {
        name: Some(project_cli::free_name(&base, &sessions(socket)?)),
        cwd: found.dir.clone(),
        command: plugins::argv(&found.dir, &spec.command),
        env: environment,
        task: None,
        backlog: None,
        brief: Default::default(),
    });
    let Some(Response::Created { name, .. }) = client::ask(socket, &request, true)? else {
        bail!("the daemon didn't start {}'s pane", plugin.label());
    };
    if let Err(err) = place(socket, &name, plugin, spec, spot, session) {
        // It was only ever the pane's.
        let _ = client::ask(socket, &Request::Kill { name }, false);
        return Err(err);
    }
    println!("{name}");
    Ok(())
}

/// Where a pane goes, what the command line says put over what its
/// manifest does.
struct Spot {
    placement: Placement,
    width: Option<Extent>,
    height: Option<Extent>,
    split: Option<SplitWay>,
}

/// Places the session called `name`, a plugin's pane, at `spot`, a split
/// beside the session `beside` names, or else the one this runs in, or else
/// the one selected.
fn place(
    socket: &Path,
    name: &str,
    plugin: &Id,
    spec: &PaneSpec,
    spot: Spot,
    beside: Option<String>,
) -> Result<()> {
    let session = name.to_string();
    let Spot {
        placement,
        width,
        height,
        split,
    } = spot;
    match placement {
        Placement::Overlay | Placement::Popup => {
            let popup = (placement == Placement::Popup).then_some(layout::Popup { width, height });
            let shown = client::lay_out(
                socket,
                layout::Command::Overlay {
                    session,
                    plugin: plugin.name.clone(),
                    title: spec.title.clone(),
                    popup,
                },
            );
            shown.map_err(|err| {
                anyhow!(
                    "{err:#}: a pane placed as {} shows only in a TUI, \
                     but split, zoomed or tab do without one",
                    placement.name()
                )
            })?;
        }
        Placement::Split | Placement::Zoomed => {
            let way = match split {
                Some(SplitWay::Down) => Way::Down,
                Some(SplitWay::Right) | None => Way::Right,
            };
            let split = layout::Command::Split {
                session: session.clone(),
                beside,
                way,
                ratio: 0.5,
            };
            client::lay_out(socket, split)?;
            if placement == Placement::Zoomed {
                let zoom = layout::Command::Zoom {
                    session: Some(session),
                    on: true,
                };
                client::lay_out(socket, zoom)?;
            }
        }
        Placement::Tab => {
            let tab = layout::Command::NewTab {
                name: Some(spec.title.clone()),
            };
            let laid = client::lay_out(socket, tab)?;
            let tab = laid
                .current()
                .map(|tab| tab.number.to_string())
                .context("the new tab isn't in front")?;
            let to = layout::Command::MoveToTab {
                session: session.clone(),
                tab,
            };
            client::lay_out(socket, to)?;
            let focus = layout::Command::Focus {
                session,
                raise: false,
            };
            client::lay_out(socket, focus)?;
        }
    }
    Ok(())
}

/// A popup's size as the command line says it: so many cells, or a share
/// of the screen, `80%`.
fn extent(text: &str) -> Result<Extent> {
    let extent = match text.trim().parse::<u16>() {
        Ok(cells) => Extent::Cells(cells),
        Err(_) => Extent::Share(text.trim().to_string()),
    };
    extent.check().map_err(|why| anyhow!("{why}"))?;
    Ok(extent)
}

/// The sessions there are.
fn sessions(socket: &Path) -> Result<Vec<SessionInfo>> {
    match client::ask(socket, &Request::List, false)? {
        Some(Response::Sessions { sessions }) => Ok(sessions),
        _ => Ok(Vec::new()),
    }
}

/// What an action run from the command line is about: the session it
/// names, or else the one it's run in, or else the current directory.
fn context_for(socket: &Path, session: Option<String>) -> Result<Context> {
    match session_for(socket, session)? {
        Some(found) => Ok(Context::of_session(&found)),
        None => Ok(Context::of_dir(&std::env::current_dir()?)),
    }
}

/// The session `session` names, or else the one this runs in, if any.
fn session_for(socket: &Path, session: Option<String>) -> Result<Option<SessionInfo>> {
    let sessions = || sessions(socket);
    if let Some(name) = session {
        let found = sessions()?.into_iter().find(|info| info.name == name);
        return found
            .map(Some)
            .with_context(|| format!("no session named {name}"));
    }
    match env::own_session_id(socket) {
        Some(id) => Ok(sessions()?.into_iter().find(|info| info.id == id)),
        None => Ok(None),
    }
}

/// `crystal plugin install`: fetches a plugin from a git repository, or
/// copies it from a directory, shows what it would run, and installs it,
/// off unless `enable` says otherwise, once the user says yes.
pub fn install(socket: &Path, source: &str, yes: bool, enable: bool) -> Result<()> {
    let dir = plugins::plugins_dir();
    fs::create_dir_all(&dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    // Fetched beside where it goes, so moving it there is one rename.
    let staging = dir.join(format!(".installing-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    let installed =
        fetch(source, &staging).and_then(|()| install_from(socket, source, &staging, yes, enable));
    let _ = fs::remove_dir_all(&staging);
    installed
}

fn fetch(source: &str, into: &Path) -> Result<()> {
    let path = Path::new(source);
    if path.is_dir() {
        return copy_dir(path, into).with_context(|| format!("couldn't copy {}", path.display()));
    }
    let status = Command::new("git")
        .args(["clone", "--quiet", "--depth", "1", "--", source])
        .arg(into)
        .status()
        .context("couldn't run git")?;
    if !status.success() {
        bail!("git couldn't clone {source}");
    }
    Ok(())
}

/// Copies the directory `from` to `to`, all but a git repository's own
/// files.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if entry.file_name() == ".git" {
            continue;
        } else if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Installs the plugin fetched into `staging`, then builds it. One whose
/// build fails stays, but off until a build works.
fn install_from(
    socket: &Path,
    source: &str,
    staging: &Path,
    yes: bool,
    enable: bool,
) -> Result<()> {
    let text = fs::read_to_string(staging.join("plugin.toml"))
        .with_context(|| format!("{source} has no plugin.toml"))?;
    let named: Manifest = toml::from_str(&text).context("in its plugin.toml")?;
    let manifest = Manifest::parse(&text, &named.name).context("in its plugin.toml")?;
    let name = manifest.name.clone();
    let target = plugins::plugins_dir().join(&name);
    if target.exists() {
        bail!("{name} is installed already: `crystal plugin remove {name}` first");
    }
    if let Some(why) = manifest.unfit() {
        bail!("{name} can't be installed: it {why}");
    }
    if let Some((key, other)) = plugins::key_taken(&manifest, &plugins::installed()) {
        bail!("{name} wants the key {key}, which the {other} plugin has");
    }

    describe(&manifest, source);
    if !yes && !confirm(&format!("Install {name}?"))? {
        println!("{name} isn't installed");
        return Ok(());
    }
    fs::rename(staging, &target)?;
    let id = Id::own(&name);
    let config_dir = plugins::config_dir(&id);
    fs::create_dir_all(&config_dir)
        .with_context(|| format!("couldn't make {}", config_dir.display()))?;
    let place = shell::home_relative(&target);
    if let Err(err) = build_and_note(socket, &id, &target, &manifest) {
        bail!(
            "installed {name} in {place}, but it's off: {err:#}\n\
             once that's fixed, `crystal plugin build {name}` builds it again"
        );
    }
    if enable {
        plugins::set_enabled(&config::path(), socket, &name, true)?;
        println!("installed {name} in {place}, and it's on");
    } else {
        println!(
            "installed {name} in {place}; it's off until you run `crystal plugin enable {name}`"
        );
    }
    Ok(())
}

/// `crystal plugin build`: runs a plugin's build commands again. One that
/// fails turns the plugin off until a build works.
pub fn build(socket: &Path, id: &Id) -> Result<()> {
    let plugin = find(id)?;
    let manifest = manifest_of(&plugin)?;
    let label = id.label();
    if let Some(why) = manifest.unfit() {
        bail!("{label} can't be built: it {why}");
    }
    if let Err(err) = build_and_note(socket, id, &plugin.dir, manifest) {
        let config = Config::load()?;
        if plugins::is_on(&config, id) {
            plugins::switch(&config::path(), socket, id, false)?;
            bail!("{err:#}\nso {label} is off now");
        }
        return Err(err);
    }
    println!("built {label}");
    Ok(())
}

/// Builds the plugin `id`, in `dir`, and notes whether that worked, for it
/// to be kept off until a build does.
fn build_and_note(socket: &Path, id: &Id, dir: &Path, manifest: &Manifest) -> Result<()> {
    let built = run_build(socket, id, dir, manifest);
    let failed = built.as_ref().err().map(|err| format!("{err:#}"));
    plugins::set_build_failed(id, failed.as_deref())?;
    built
}

/// Runs the plugin's build commands for this system in turn, from its
/// directory `dir`, each as its user would, without the variables its
/// other commands get. What they print goes to the plugin's log, and the
/// last of it into the error when one fails.
fn run_build(socket: &Path, id: &Id, dir: &Path, manifest: &Manifest) -> Result<()> {
    for once in manifest.build.iter().filter(|once| once.runs_here()) {
        let words = once.command.join(" ");
        println!("building {}: {words}", id.label());
        plugins::log(socket, id, &format!("build: {words}"));
        let log = plugins::open_log(socket, id)?;
        let start = log.metadata()?.len();
        let argv = plugins::argv(dir, &once.command);
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .status()
            .with_context(|| format!("its build couldn't run {words}"))?;
        if !status.success() {
            let printed = last_printed(&plugins::log_path(socket, id), start);
            bail!("its build failed: {words} ended with {status}{printed}");
        }
    }
    Ok(())
}

/// The last lines a build command printed, from `start` in the log at
/// `path`, set off under the line saying it failed.
fn last_printed(path: &Path, start: u64) -> String {
    const LINES: usize = 10;
    let mut text = String::new();
    let read = fs::File::open(path).and_then(|mut log| {
        log.seek(SeekFrom::Start(start))?;
        log.read_to_string(&mut text)
    });
    if read.is_err() {
        return String::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    let last = &lines[lines.len().saturating_sub(LINES)..];
    last.iter().map(|line| format!("\n  {line}")).collect()
}

/// Shows what a plugin is and every command it would run, for the user to
/// read before saying yes: each line with nothing a terminal would take as
/// an order, so that nothing in the manifest can hide a command or draw
/// another over it.
fn describe(manifest: &Manifest, source: &str) {
    let say = |line: String| println!("{}", printable::line(&line));
    say(format!(
        "{} {}, from {source}",
        manifest.name, manifest.version
    ));
    if !manifest.description.is_empty() {
        say(manifest.description.clone());
    }
    println!();
    println!("It runs these commands as you, from its own directory:");
    let commands = manifest.commands();
    let width = commands
        .iter()
        .map(|(what, _)| what.len())
        .max()
        .unwrap_or(0);
    for (what, words) in commands {
        let words: Vec<String> = words.iter().map(|word| shell::quote(word)).collect();
        say(format!("  {what:width$}  {}", words.join(" ")));
    }
    println!();
}

/// `crystal plugin remove`. The plugin's settings and what it kept stay,
/// for when it's installed again.
pub fn remove(name: &str) -> Result<()> {
    let id = Id::own(name);
    let plugin = find(&id)?;
    fs::remove_dir_all(&plugin.dir)
        .with_context(|| format!("couldn't remove {}", plugin.dir.display()))?;
    plugins::set_build_failed(&id, None)?;
    plugins::forget(&config::path(), name)?;
    println!("removed {name}");
    Ok(())
}

/// `crystal plugin new`: a plugin to start from, with one of each thing a
/// plugin can have.
pub fn new(name: &str) -> Result<()> {
    plugin_manifest::check_name(name)?;
    let dir = plugins::plugins_dir().join(name);
    if dir.exists() {
        bail!("{} is there already", dir.display());
    }
    fs::create_dir_all(&dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    fs::create_dir_all(plugins::config_dir(&Id::own(name)))?;
    let files = [
        ("plugin.toml", MANIFEST),
        ("hook.sh", HOOK),
        ("hello.sh", HELLO),
        ("pane.sh", PANE),
    ];
    for (file, text) in files {
        fs::write(dir.join(file), text.replace("NAME", name))?;
    }
    println!("made {name} in {}", shell::home_relative(&dir));
    println!("turn it on with `crystal plugin enable {name}`, then press X in the TUI");
    Ok(())
}

const MANIFEST: &str = r#"name = "NAME"
version = "0.1.0"
description = "What NAME does, in a line"
# min_crystal_version = "0.3.0"    # the oldest crystal it works with
# platforms = ["macos", "linux"]   # where it runs; anywhere, left out
# timeout_secs = 30                # how long a hook or startup command may run

# Run as it's installed, and again with `crystal plugin build NAME`.
# [[build]]
# command = ["npm", "ci"]

# Run once by crystal's daemon as it starts, with CRYSTAL_EVENT=startup.
# [[startup]]
# command = ["sh", "hook.sh"]

# Run from the plugins view (X in the TUI), or with
# `crystal plugin run NAME hello`. Give it a key crystal doesn't use, and
# that key runs it from the sidebar too: a character, a chord like
# "ctrl+alt+h", or two keys pressed one after the other, like "H i".
[[actions]]
id = "hello"
title = "Say hello"
command = ["sh", "hello.sh"]
# key = "H"

# Run by crystal's daemon when something happens, with the event as JSON
# on standard input, and what happened in a line in $CRYSTAL_EVENT_TEXT.
# `session.*` is every session event; `*`, every event: `crystal plugin
# events` lists them.
[[events]]
on = "*"
command = ["sh", "hook.sh"]

# A program shown over the TUI's panes, with the keyboard, until it ends
# or you press Ctrl+\. Or `placement = "popup"`, with a `width` and a
# `height` like 40 or "80%"; or as a session of its own: "split" off the
# selected session's pane (`split = "down"` below it), "zoomed", or "tab".
# `crystal plugin pane open NAME events` opens it from the command line.
[[panes]]
id = "events"
title = "What NAME heard"
command = ["sh", "pane.sh"]

# Links a Ctrl+click in a pane hands to one of its actions rather than to
# your browser, the link in $CRYSTAL_LINK.
# [[link_handlers]]
# pattern = "^https://example\\.com/"
# action = "hello"
"#;

const HOOK: &str = r#"#!/bin/sh
# Adds each event crystal tells NAME about to events.jsonl, beside this
# script: a line of JSON each.
cat >> events.jsonl
"#;

const HELLO: &str = r#"#!/bin/sh
# Says where it was run from. Run from the TUI, what it prints goes to
# `crystal plugin log NAME`. "$CRYSTAL_BIN" is crystal, for the rest:
# "$CRYSTAL_BIN" send "$CRYSTAL_SESSION" "..." types into the session.
echo "hello from NAME: session ${CRYSTAL_SESSION:-none}, in ${CRYSTAL_WORKTREE:-?}"
"#;

const PANE: &str = r#"#!/bin/sh
# Shows the last events NAME heard, until Enter.
echo "The last events NAME heard:"
echo
if [ -f events.jsonl ]; then tail -n 20 events.jsonl; else echo "(none yet)"; fi
echo
printf 'Press Enter to close. '
read -r _
"#;

/// `crystal plugin log`.
pub fn log(socket: &Path, id: &Id) -> Result<()> {
    find(id)?;
    match fs::read_to_string(plugins::log_path(socket, id)) {
        Ok(text) => print!("{text}"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            println!("{} hasn't logged anything yet", id.label());
        }
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

/// The plugin `id`, installed or shipped by its project, or an error that
/// says why there's none.
fn find(id: &Id) -> Result<Installed> {
    let name = &id.name;
    if plugins::is_built_in(name) {
        bail!("{name} is one of crystal's own plugins, not one you installed");
    }
    let dir = match &id.project {
        Some(project) => project.join(plugins::PROJECT_DIR),
        None => plugins::plugins_dir(),
    };
    plugins::find_id(id).with_context(|| {
        format!(
            "there's no plugin called {name} in {}: `crystal plugin` lists them",
            shell::home_relative(&dir)
        )
    })
}

fn manifest_of(plugin: &Installed) -> Result<&Manifest> {
    plugin
        .manifest
        .as_ref()
        .map_err(|why| anyhow!("{}'s plugin.toml: {why}", plugin.name))
}
