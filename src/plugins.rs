//! Plugins: what crystal does beyond running sessions, in parts that can
//! each be switched on and off.
//!
//! crystal's own plugins, listed in [`BUILT_IN`], are on unless the config
//! file switches them off. Each has one gate, its module's `enabled`, that
//! asks [`enabled`] here; with a plugin off, everything it adds is gone:
//! its keys and views, what it shows on the sidebar, what it tells agents,
//! the work it does in the background.
//!
//! Plugins the user installs live in a directory each, under
//! [`plugins_dir`], with a `plugin.toml` saying what they add: see
//! [`crate::plugin_manifest`]. They are off until switched on, and they use
//! crystal through its own command line, like any script would. One that
//! doesn't fit this crystal or this system, or whose build failed, can't be
//! switched on, and doesn't run if it was.
//!
//! Each has a directory for its user's settings, shared by every server
//! like the config file, and one for what it keeps as it runs, a server's
//! own like the server's sessions: what a plugin keeps is about what it
//! saw happen, which is a server's.

use crate::config::{self, Config};
use crate::events::Event;
use crate::git::Checkout;
use crate::plugin_manifest::Manifest;
use crate::protocol::SessionInfo;
use crate::state;
use crate::tui::keymap::Sequence;
use anyhow::{Context as _, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use toml_edit::{DocumentMut, Item, Table, Value, value};

/// One of crystal's own plugins.
pub struct BuiltIn {
    pub name: &'static str,
    pub description: &'static str,
}

pub const BUILT_IN: &[BuiltIn] = &[
    BuiltIn {
        name: "tasks",
        description: "sessions with something to do, closed with `crystal done`",
    },
    BuiltIn {
        name: "handoff",
        description: "notes a worktree keeps for the sessions after, `crystal handoff`",
    },
    BuiltIn {
        name: "backlog",
        description: "each project's list of things to do later",
    },
    BuiltIn {
        name: "memory",
        description: "what a project's sessions learned, shown to the next ones",
    },
    BuiltIn {
        name: "profiles",
        description: "saved ways of starting an agent",
    },
    BuiltIn {
        name: "github",
        description: "pull requests and issues, from GitHub or GitLab",
    },
    BuiltIn {
        name: "flows",
        description: "chains of tasks on one goal, with gates",
    },
    BuiltIn {
        name: "notifications",
        description: "a notification when a session needs you",
    },
];

/// The sidebar keys crystal uses itself, which a plugin's action can't
/// take.
pub const RESERVED_KEYS: &str =
    "jkhlsnwWrxuUadpGBEmPcbqoOiXgf?/tT&[]{}123456789>zveHJKLRFS,yY|-AZ!.:()\\";

pub fn is_built_in(name: &str) -> bool {
    BUILT_IN.iter().any(|plugin| plugin.name == name)
}

/// Whether the plugin called `name` is on, by the config: crystal's own
/// are on unless switched off, and ones the user installed are off until
/// switched on.
pub fn enabled(config: &Config, name: &str) -> bool {
    match config.plugins.get(name) {
        Some(on) => *on,
        None => is_built_in(name),
    }
}

/// What a command says when the plugin it needs is off.
pub fn off(name: &str) -> String {
    format!("the {name} plugin is off: turn it on with `crystal plugin enable {name}`")
}

/// Refuses, saying how to turn it on, when the plugin called `name` is off.
pub fn ensure_enabled(config: &Config, name: &str) -> Result<()> {
    if !enabled(config, name) {
        bail!(off(name));
    }
    Ok(())
}

/// Switches the plugin called `name` on or off in the config file at
/// `path`, under `[plugins]`, keeping the rest of the file as the user
/// wrote it, comments and all. Switching a plugin on also lets it run
/// again after it was paused for failing.
pub fn set_enabled(path: &Path, socket: &Path, name: &str, on: bool) -> Result<()> {
    edit_plugins(path, |plugins| {
        // A line that's there already keeps its comment.
        match plugins.get_mut(name).and_then(Item::as_value_mut) {
            Some(said) => {
                let decor = said.decor().clone();
                *said = Value::from(on);
                *said.decor_mut() = decor;
            }
            None => {
                plugins.insert(name, value(on));
            }
        }
    })?;
    if on {
        unpause(socket, name);
    }
    Ok(())
}

/// Takes the plugin called `name` out of the config file's `[plugins]`,
/// once it's removed: a name there that's no plugin is an error.
pub fn forget(path: &Path, name: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    edit_plugins(path, |plugins| {
        plugins.remove(name);
    })
}

/// Reads the config file at `path`, lets `change` change its `[plugins]`
/// table, and writes it back, unless the result doesn't make sense as a
/// config file. A table left empty goes. As with profiles, the new file is
/// written beside the old one and then moved over it.
fn edit_plugins(path: &Path, change: impl FnOnce(&mut Table)) -> Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("couldn't read {}", path.display())),
    };
    let mut document: DocumentMut = text
        .parse()
        .with_context(|| format!("couldn't read {}", path.display()))?;
    let plugins = document
        .entry("plugins")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .context("`plugins` in the config file isn't a [plugins] table")?;
    change(plugins);
    if plugins.is_empty() {
        document.remove("plugins");
    }

    let new_text = document.to_string();
    config::from_text(&new_text).with_context(|| format!("in {}", path.display()))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let unfinished = path.with_extension("toml.saving");
    fs::write(&unfinished, &new_text)?;
    fs::rename(&unfinished, path)?;
    Ok(())
}

/// Refuses, saying why, to switch on the plugin `plugin` of those
/// `installed`: it can't run here, or it wants a key another has.
pub fn check_can_enable(plugin: &Installed, installed: &[Installed]) -> Result<()> {
    if let Some(why) = plugin.blocked() {
        bail!("{} can't be turned on: {why}", plugin.name);
    }
    if let Ok(manifest) = &plugin.manifest
        && let Some((key, other)) = key_taken(manifest, installed)
    {
        bail!(
            "{} wants the key {key}, which the {other} plugin has",
            plugin.name
        );
    }
    Ok(())
}

/// The first of `manifest`'s keys another installed plugin's action has
/// already taken, with that plugin's name. Two plugins can't share a key,
/// nor can one's be the first of another's two.
pub fn key_taken(manifest: &Manifest, others: &[Installed]) -> Option<(String, String)> {
    let read = |key: &String| Some((key.clone(), Sequence::parse(key).ok()?));
    let keys = manifest
        .actions
        .iter()
        .filter_map(|action| action.key.as_ref().and_then(read));
    for (key, ours) in keys {
        for other in others.iter().filter(|other| other.name != manifest.name) {
            let Ok(theirs) = &other.manifest else {
                continue;
            };
            let taken = theirs
                .actions
                .iter()
                .filter_map(|action| action.key.as_ref().and_then(read))
                .any(|(_, theirs)| theirs.clashes(&ours));
            if taken {
                return Some((key, other.name.clone()));
            }
        }
    }
    None
}

/// What a plugin's command is told about where it was run from: the
/// session, and the project and worktree it's in; and for an action run on
/// a link, the link.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Context {
    pub session: Option<String>,
    pub session_id: Option<String>,
    pub project: Option<PathBuf>,
    pub worktree: Option<PathBuf>,
    pub link: Option<String>,
}

impl Context {
    /// Run for `session`.
    pub fn of_session(session: &SessionInfo) -> Context {
        let (project, worktree) = match &session.worktree {
            Some(worktree) => (worktree.project_path.clone(), worktree.path.clone()),
            None => (session.cwd.clone(), session.cwd.clone()),
        };
        Context {
            session: Some(session.name.clone()),
            session_id: Some(session.id.clone()),
            project: Some(project),
            worktree: Some(worktree),
            link: None,
        }
    }

    /// About what `event` is about.
    pub fn of_event(event: &Event) -> Context {
        let session = event.session.as_ref();
        let worktree = match (session, &event.worktree) {
            (Some(session), _) => Some(session.worktree.as_ref().unwrap_or(&session.cwd).clone()),
            (None, Some(worktree)) => Some(worktree.path.clone()),
            (None, None) => None,
        };
        Context {
            session: session.map(|session| session.name.clone()),
            session_id: session.map(|session| session.id.clone()),
            project: event.project.clone(),
            worktree,
            link: None,
        }
    }

    /// Run from `dir`, in no session.
    pub fn of_dir(dir: &Path) -> Context {
        let (project, worktree) = match Checkout::find(dir) {
            Some(checkout) => {
                let worktree = checkout.worktree();
                (worktree.project_path, worktree.path)
            }
            None => (dir.to_path_buf(), dir.to_path_buf()),
        };
        Context {
            project: Some(project),
            worktree: Some(worktree),
            ..Context::default()
        }
    }

    /// The variables a command finds this in: `CRYSTAL_SESSION`,
    /// `CRYSTAL_SESSION_ID`, `CRYSTAL_PROJECT`, `CRYSTAL_WORKTREE` and
    /// `CRYSTAL_LINK`, each `None` when there's nothing to say, to take away
    /// one the command would otherwise get from whoever ran it.
    pub fn vars(&self) -> [(&'static str, Option<String>); 5] {
        let path = |path: &Option<PathBuf>| path.as_ref().map(|path| path.display().to_string());
        [
            ("CRYSTAL_SESSION", self.session.clone()),
            ("CRYSTAL_SESSION_ID", self.session_id.clone()),
            ("CRYSTAL_PROJECT", path(&self.project)),
            ("CRYSTAL_WORKTREE", path(&self.worktree)),
            ("CRYSTAL_LINK", self.link.clone()),
        ]
    }
}

/// `words`, a command of the plugin called `plugin`, ready to run from the
/// plugin's directory `dir`, with [`env`] in its environment and the
/// plugin's state directory made.
pub fn command(
    plugin: &str,
    dir: &Path,
    words: &[String],
    socket: &Path,
    context: &Context,
) -> Command {
    make_state_dir(socket, plugin);
    let argv = argv(dir, words);
    let (program, args) = argv.split_first().expect("a checked command has a program");
    let mut command = Command::new(program);
    command.args(args).current_dir(dir);
    for (key, said) in env(socket, plugin, context) {
        match said {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
    command
}

/// `words`, a plugin's command, with a program given as a path made to go
/// from the plugin's directory `dir`, like the command does.
pub fn argv(dir: &Path, words: &[String]) -> Vec<String> {
    let mut argv = words.to_vec();
    if let Some(program) = argv.first_mut()
        && program.contains('/')
    {
        *program = dir.join(&*program).display().to_string();
    }
    argv
}

/// What a command of the plugin called `plugin` finds in its environment:
/// `CRYSTAL_BIN`, the crystal that runs it, and `CRYSTAL_SOCKET`, its
/// daemon, so the plugin can use crystal's own commands; `CRYSTAL_PLUGIN`,
/// its name, and its directories, `CRYSTAL_PLUGIN_DIR`, the plugin's own,
/// `CRYSTAL_PLUGIN_CONFIG_DIR` and `CRYSTAL_PLUGIN_STATE_DIR`; and then
/// `context`.
pub fn env(socket: &Path, plugin: &str, context: &Context) -> Vec<(&'static str, Option<String>)> {
    let crystal = std::env::current_exe().ok();
    let path = |path: PathBuf| Some(path.display().to_string());
    let mut env = vec![
        ("CRYSTAL_BIN", crystal.and_then(path)),
        ("CRYSTAL_SOCKET", path(socket.to_path_buf())),
        ("CRYSTAL_PLUGIN", Some(plugin.to_string())),
        ("CRYSTAL_PLUGIN_DIR", path(plugins_dir().join(plugin))),
        ("CRYSTAL_PLUGIN_CONFIG_DIR", path(config_dir(plugin))),
        (
            "CRYSTAL_PLUGIN_STATE_DIR",
            path(own_state_dir(socket, plugin)),
        ),
    ];
    env.extend(context.vars());
    env
}

/// Makes the plugin's state directory on the daemon at `socket`, before
/// something of the plugin's runs there. One that can't be made is the
/// plugin's to find missing.
pub fn make_state_dir(socket: &Path, plugin: &str) {
    let _ = fs::create_dir_all(own_state_dir(socket, plugin));
}

/// Where installed plugins live: `$XDG_CONFIG_HOME/crystal/plugins`, or
/// `~/.config/crystal/plugins`, beside the config file.
pub fn plugins_dir() -> PathBuf {
    config_root().join("plugins")
}

/// Where the plugin called `plugin` keeps its user's settings, like a
/// token: beside the config file, apart from the plugin's own files, so
/// installing it again keeps them. It's made as the plugin is installed.
pub fn config_dir(plugin: &str) -> PathBuf {
    config_root().join("plugin-config").join(plugin)
}

/// The directory of crystal's config file.
fn config_root() -> PathBuf {
    let config = crate::config::path();
    config.parent().unwrap_or(Path::new(".")).to_path_buf()
}

/// An installed plugin: its directory, and its manifest, or why the
/// manifest can't be used.
pub struct Installed {
    pub name: String,
    pub dir: PathBuf,
    pub manifest: Result<Manifest, String>,
}

impl Installed {
    /// Why the plugin can't run here, whatever the config says: a manifest
    /// that doesn't make sense, or doesn't fit this crystal or this system,
    /// or a build that failed.
    pub fn blocked(&self) -> Option<String> {
        let manifest = match &self.manifest {
            Ok(manifest) => manifest,
            Err(why) => return Some(why.clone()),
        };
        manifest.unfit().or_else(|| {
            let failed = build_failed(&self.name)?;
            let why = failed.lines().next().unwrap_or_default();
            Some(format!(
                "{why}; `crystal plugin build {}` tries again",
                self.name
            ))
        })
    }
}

/// The plugins installed in [`plugins_dir`], by name.
pub fn installed() -> Vec<Installed> {
    let dir = plugins_dir();
    installed_names()
        .into_iter()
        .map(|name| {
            let dir = dir.join(&name);
            let manifest = read_manifest(&dir, &name);
            Installed {
                name,
                dir,
                manifest,
            }
        })
        .collect()
}

/// The names of the plugins installed, in order: the directories in
/// [`plugins_dir`] with a `plugin.toml`, whether it makes sense or not.
pub fn installed_names() -> Vec<String> {
    let Ok(entries) = fs::read_dir(plugins_dir()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().join("plugin.toml").is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// The installed plugin called `name`, if there is one.
pub fn find(name: &str) -> Option<Installed> {
    installed().into_iter().find(|plugin| plugin.name == name)
}

/// The installed plugins that are on, can run here, and aren't paused for
/// failing, in order by name.
pub fn running(config: &Config, socket: &Path) -> Vec<(PathBuf, Manifest)> {
    installed()
        .into_iter()
        .filter(|plugin| enabled(config, &plugin.name) && paused(socket, &plugin.name).is_none())
        .filter(|plugin| plugin.blocked().is_none())
        .filter_map(|plugin| Some((plugin.dir, plugin.manifest.ok()?)))
        .collect()
}

/// The plugin, and its action, that opens `url` in place of the browser:
/// the first running plugin by name with a link handler that takes it,
/// its handlers tried in their order.
pub fn link_handler(config: &Config, socket: &Path, url: &str) -> Option<(String, String)> {
    running(config, socket)
        .into_iter()
        .find_map(|(_, manifest)| {
            let handler = manifest
                .link_handlers
                .iter()
                .find(|handler| handler.takes(url))?;
            Some((manifest.name.clone(), handler.action.clone()))
        })
}

fn read_manifest(dir: &Path, name: &str) -> Result<Manifest, String> {
    let text = fs::read_to_string(dir.join("plugin.toml")).map_err(|err| err.to_string())?;
    Manifest::parse(&text, name).map_err(|err| format!("{err:#}"))
}

/// Where the daemon at `socket` keeps what it knows about plugins: their
/// logs, which it paused for failing, and a directory each for what they
/// keep.
pub fn state_dir(socket: &Path) -> PathBuf {
    state::plugins_dir(socket)
}

/// Where the plugin called `plugin` keeps what it needs as it runs on the
/// daemon at `socket`.
pub fn own_state_dir(socket: &Path, plugin: &str) -> PathBuf {
    state_dir(socket).join(plugin)
}

/// Why the plugin called `name` failed to build, if its last build did.
fn build_failed(name: &str) -> Option<String> {
    fs::read_to_string(unbuilt_path(name)).ok()
}

/// Notes why the plugin called `name` failed to build, or with `None` that
/// it built.
pub fn set_build_failed(name: &str, why: Option<&str>) -> Result<()> {
    let path = unbuilt_path(name);
    match why {
        Some(why) => {
            fs::write(&path, why).with_context(|| format!("couldn't write {}", path.display()))?
        }
        None => {
            let _ = fs::remove_file(&path);
        }
    }
    Ok(())
}

/// Where a plugin's failed build is noted: beside the plugin, since its
/// files are every server's, out of the way of its own.
fn unbuilt_path(name: &str) -> PathBuf {
    plugins_dir().join(format!(".{name}.unbuilt"))
}

/// The log of the plugin called `name`: what its commands printed.
pub fn log_path(socket: &Path, name: &str) -> PathBuf {
    state_dir(socket).join(format!("{name}.log"))
}

/// How big a log may grow before it starts over.
const LOG_MAX: u64 = 1 << 20;

/// The plugin's log, open to add to, made if there's none yet. One that
/// has grown past [`LOG_MAX`] starts over.
pub fn open_log(socket: &Path, name: &str) -> Result<fs::File> {
    fs::create_dir_all(state_dir(socket))?;
    let path = log_path(socket, name);
    let full = fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_MAX);
    let file = fs::OpenOptions::new()
        .create(true)
        .append(!full)
        .write(true)
        .truncate(full)
        .open(&path)
        .with_context(|| format!("couldn't open {}", path.display()))?;
    Ok(file)
}

/// Adds a line to the plugin's log, after the time. What can't be written
/// is lost: a log is never worth failing over.
pub fn log(socket: &Path, name: &str, line: &str) {
    use std::io::Write;
    if let Ok(mut file) = open_log(socket, name) {
        let _ = writeln!(file, "[{}] {line}", local_time());
    }
}

/// The time now, on this machine's clock: `2026-10-03 14:05:09`.
fn local_time() -> String {
    // SAFETY: time with a null pointer only returns the time.
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    // SAFETY: an all-zero tm is a valid value for localtime_r to fill in,
    // and both pointers live for the whole call.
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut local) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday,
        local.tm_hour,
        local.tm_min,
        local.tm_sec
    )
}

/// Why the plugin called `name` was paused, if it was: its event hooks
/// failed too many times in a row. Turning it on again unpauses it.
pub fn paused(socket: &Path, name: &str) -> Option<String> {
    fs::read_to_string(pause_path(socket, name)).ok()
}

pub fn pause(socket: &Path, name: &str, why: &str) -> Result<()> {
    fs::create_dir_all(state_dir(socket))?;
    fs::write(pause_path(socket, name), why)?;
    Ok(())
}

pub fn unpause(socket: &Path, name: &str) {
    let _ = fs::remove_file(pause_path(socket, name));
}

fn pause_path(socket: &Path, name: &str) -> PathBuf {
    state_dir(socket).join(format!("{name}.paused"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn with(plugins: &[(&str, bool)]) -> Config {
        let plugins: BTreeMap<String, bool> = plugins
            .iter()
            .map(|(name, on)| (name.to_string(), *on))
            .collect();
        Config {
            plugins,
            ..Config::default()
        }
    }

    #[test]
    fn crystals_own_plugins_are_on_until_switched_off() {
        assert!(enabled(&Config::default(), "memory"));
        assert!(!enabled(&with(&[("memory", false)]), "memory"));
        assert!(enabled(&with(&[("memory", false)]), "backlog"));
    }

    #[test]
    fn installed_plugins_are_off_until_switched_on() {
        assert!(!enabled(&Config::default(), "notes"));
        assert!(enabled(&with(&[("notes", true)]), "notes"));
    }

    #[test]
    fn being_off_says_how_to_turn_it_on() {
        let err = ensure_enabled(&with(&[("backlog", false)]), "backlog").unwrap_err();
        assert_eq!(
            err.to_string(),
            "the backlog plugin is off: turn it on with `crystal plugin enable backlog`"
        );
    }

    #[test]
    fn switching_a_plugin_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let socket = dir.path().join("crystal.sock");
        fs::write(&path, "# mine\nnotify = false # quiet\n").unwrap();
        set_enabled(&path, &socket, "memory", false).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            text.replace("memory = false", "memory = false # for now"),
        )
        .unwrap();
        set_enabled(&path, &socket, "backlog", false).unwrap();
        set_enabled(&path, &socket, "memory", true).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# mine\nnotify = false # quiet\n"),
            "{text}"
        );
        assert!(text.contains("memory = true # for now\n"), "{text}");
        let config = config::from_text(&text).unwrap();
        assert!(enabled(&config, "memory"));
        assert!(!enabled(&config, "backlog"));

        forget(&path, "memory").unwrap();
        forget(&path, "backlog").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, "# mine\nnotify = false # quiet\n");
    }

    fn installed(name: &str, key: &str) -> Installed {
        let text = format!(
            "name = \"{name}\"\nversion = \"1\"\n[[actions]]\nid = \"a\"\ntitle = \"A\"\n\
             command = [\"true\"]\nkey = \"{key}\"\n"
        );
        Installed {
            name: name.into(),
            dir: PathBuf::from(name),
            manifest: Manifest::parse(&text, name).map_err(|err| err.to_string()),
        }
    }

    #[test]
    fn two_plugins_cant_share_a_key() {
        let notes = installed("notes", "N");
        let manifest = notes.manifest.as_ref().unwrap();
        let others = [installed("news", "N"), installed("todo", "T")];
        assert_eq!(
            key_taken(manifest, &others),
            Some(("N".to_string(), "news".to_string()))
        );
        assert_eq!(key_taken(manifest, &others[1..]), None);
        // Its own key isn't taken from itself.
        assert_eq!(key_taken(manifest, &[installed("notes", "N")]), None);
        // Nor can one's key be the first of another's two.
        let two = [installed("news", "N t")];
        assert_eq!(
            key_taken(manifest, &two),
            Some(("N".to_string(), "news".to_string()))
        );
        let pair = installed("todo", "N u");
        let pair = pair.manifest.as_ref().unwrap();
        assert_eq!(key_taken(pair, &two), None);
        let chord = installed("todo", "ctrl+alt+n");
        assert_eq!(key_taken(chord.manifest.as_ref().unwrap(), &others), None);
    }

    #[test]
    fn a_plugin_that_cant_run_here_or_wants_a_taken_key_cant_be_turned_on() {
        // A name no plugin of the machine's has, since a failed build is
        // noted beside the installed plugins.
        let name = "crystal-test-only";
        let mut future = installed(name, "Q");
        if let Ok(manifest) = &mut future.manifest {
            manifest.min_crystal_version = Some("99.0".into());
        }
        let err = check_can_enable(&future, &[]).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("crystal-test-only can't be turned on: needs crystal 99.0"),
            "{err}"
        );
        let wanting = installed(name, "Q");
        assert!(check_can_enable(&wanting, &[installed("other", "R")]).is_ok());
        let err = check_can_enable(&wanting, &[installed("other", "Q")]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "crystal-test-only wants the key Q, which the other plugin has"
        );
    }

    #[test]
    fn switching_a_plugin_on_lets_a_paused_one_run_again() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("crystal.sock");
        pause(&socket, "notes", "it failed").unwrap();
        assert_eq!(paused(&socket, "notes").as_deref(), Some("it failed"));
        set_enabled(&dir.path().join("config.toml"), &socket, "notes", true).unwrap();
        assert_eq!(paused(&socket, "notes"), None);
    }

    #[test]
    fn a_command_runs_from_its_plugins_directory_with_what_its_about() {
        let context = Context {
            session: Some("claude".into()),
            project: Some("/code/app".into()),
            ..Context::default()
        };
        let words = ["./run.sh".to_string(), "now".to_string()];
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.sock");
        let command = command(
            "notes",
            Path::new("/plugins/notes"),
            &words,
            &socket,
            &context,
        );
        assert_eq!(command.get_program(), "/plugins/notes/./run.sh");
        assert_eq!(command.get_current_dir(), Some(Path::new("/plugins/notes")));
        let env: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(key, value)| {
                let value = value.map(|value| value.to_string_lossy().into_owned());
                (key.to_string_lossy().into_owned(), value)
            })
            .collect();
        assert!(env.contains(&("CRYSTAL_SESSION".into(), Some("claude".into()))));
        assert!(env.contains(&("CRYSTAL_SESSION_ID".into(), None)));
        assert!(env.contains(&("CRYSTAL_PROJECT".into(), Some("/code/app".into()))));
        assert!(env.contains(&("CRYSTAL_LINK".into(), None)));
        let socket_var = Some(socket.display().to_string());
        assert!(env.contains(&("CRYSTAL_SOCKET".into(), socket_var)));
        assert!(env.contains(&("CRYSTAL_PLUGIN".into(), Some("notes".into()))));
        // Its state is the server's, and made before it runs.
        let state = dir.path().join("s.plugins/notes");
        let state_var = Some(state.display().to_string());
        assert!(env.contains(&("CRYSTAL_PLUGIN_STATE_DIR".into(), state_var)));
        assert!(state.is_dir());
    }
}
