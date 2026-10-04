//! `crystal config export` and `crystal config import`: this machine's
//! settings in one file, to keep or to take to another machine, and such a
//! file merged into the settings here.
//!
//! A bundle is JSON: the config file's text as it is, comments and all, and
//! the user's own agent rule files (`agents/` beside it), by their names.
//! Nothing else goes in it: not the plugins, nor what they keep beside the
//! config (`plugin-config/`, a token for one), nor anything a server keeps.
//!
//! An import merges rather than replaces: a setting the bundle has takes
//! the place of the one here, one it doesn't have stays as it is, and the
//! `[[profile]]` and `[[flow]]` tables merge by their names, `[[project]]`
//! by its path and `[[keys.command]]` by its key. It takes a bundle, a
//! config file on its own, or a directory holding either, the way a copy of
//! `~/.config/crystal` does. The file is changed through `toml_edit`, so it
//! keeps its comments and its order, and nothing is written unless all of
//! it, merged, still makes sense.

use crate::{agent_rules, config, plugins, shell};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

/// The name `export` gives the file it writes into a directory, and the one
/// `import` looks for in a directory.
pub const FILE_NAME: &str = "crystal-settings.json";

/// What sort of bundle this crystal writes: one with a higher number was
/// made by a crystal that writes what this one can't read.
const BUNDLE: u32 = 1;

/// This crystal's version, which a bundle says it was made by.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// One machine's settings in one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    /// What sort of bundle it is: see [`BUNDLE`].
    crystal_bundle: u32,
    /// The crystal that made it.
    #[serde(default)]
    exported_by: String,
    /// The config file's text; none when there was no file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config: Option<String>,
    /// The user's agent rule files' texts, by their names in `agents/`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    agents: BTreeMap<String, String>,
}

/// `crystal config export`: the settings here as a bundle, on standard
/// output, with `-` or nothing, or else written to `to`: a file, or
/// [`FILE_NAME`] in a directory.
pub fn export(to: Option<&str>) -> Result<()> {
    let bundle = Bundle::here(&config::path(), &agent_rules::dir())?;
    let json = serde_json::to_string_pretty(&bundle)? + "\n";
    let Some(to) = to.filter(|to| *to != "-") else {
        print!("{json}");
        return Ok(());
    };
    let mut path = PathBuf::from(to);
    if path.is_dir() {
        path.push(FILE_NAME);
    }
    std::fs::write(&path, json).with_context(|| format!("couldn't write {}", path.display()))?;
    println!("exported the settings to {}", shell::home_relative(&path));
    Ok(())
}

/// `crystal config import`: merges the settings in `from`, a bundle, a
/// config file or a directory holding either, or `-` for standard input,
/// into the settings here, and says what changed.
pub fn import(from: &str) -> Result<()> {
    let bundle = read(from)?;
    let config_path = config::path();
    let agents_dir = agent_rules::dir();
    let merged = bundle.merge_into(&config_path, &agents_dir)?;
    if merged.is_empty() {
        println!("nothing to change: the settings here have all of it already");
        return Ok(());
    }
    merged.write(&config_path, &agents_dir)?;
    if !merged.changed.is_empty() {
        println!("merged into {}:", shell::home_relative(&config_path));
        for change in &merged.changed {
            println!("  {change}");
        }
    }
    for (name, _) in &merged.agents {
        println!("wrote {}", shell::home_relative(&agents_dir.join(name)));
    }
    println!(
        "the daemon goes by them as it reads its settings; start crystal's TUI again for all of \
         them to show there"
    );
    Ok(())
}

/// What an import changes, worked out before anything is written.
#[derive(Debug, Default)]
struct Merged {
    /// The config file's new text, when it changes.
    config: Option<String>,
    /// What changed in it, a line each: `theme`, `sidebar.width`, `profile
    /// review (new)`.
    changed: Vec<String>,
    /// The agent rule files to write, by name, with their texts.
    agents: Vec<(String, String)>,
}

impl Merged {
    fn is_empty(&self) -> bool {
        self.config.is_none() && self.agents.is_empty()
    }

    /// Writes it: the config file through a file beside it moved over it,
    /// so a crash can't leave half a file, and the agent rule files.
    fn write(&self, config_path: &Path, agents_dir: &Path) -> Result<()> {
        if let Some(text) = &self.config {
            if let Some(dir) = config_path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let unfinished = config_path.with_extension("toml.saving");
            std::fs::write(&unfinished, text)?;
            std::fs::rename(&unfinished, config_path)?;
        }
        if !self.agents.is_empty() {
            std::fs::create_dir_all(agents_dir)
                .with_context(|| format!("couldn't make {}", agents_dir.display()))?;
        }
        for (name, text) in &self.agents {
            let path = agents_dir.join(name);
            std::fs::write(&path, text)
                .with_context(|| format!("couldn't write {}", path.display()))?;
        }
        Ok(())
    }
}

impl Bundle {
    /// The settings in the config file at `config_path` and the agent rule
    /// files in `agents_dir`.
    fn here(config_path: &Path, agents_dir: &Path) -> Result<Bundle> {
        Ok(Bundle {
            crystal_bundle: BUNDLE,
            exported_by: VERSION.to_string(),
            config: read_if_there(config_path)?,
            agents: agent_files(agents_dir)?,
        })
    }

    /// What merging it into the config file at `config_path` and the agent
    /// rule files in `agents_dir` would change, or why it can't be: the
    /// file it makes doesn't make sense, or a rules file is broken.
    fn merge_into(&self, config_path: &Path, agents_dir: &Path) -> Result<Merged> {
        let mut merged = Merged::default();
        if let Some(imported) = &self.config {
            let imported: DocumentMut = imported
                .parse()
                .context("the config in what's imported isn't TOML")?;
            let here = read_if_there(config_path)?.unwrap_or_default();
            let mut document: DocumentMut = here
                .parse()
                .with_context(|| format!("couldn't read {}", config_path.display()))?;
            merge_tables(
                document.as_table_mut(),
                imported.as_table(),
                "",
                &mut merged.changed,
            );
            if !merged.changed.is_empty() {
                let text = document.to_string();
                let settings = config::from_text(&text)
                    .context("merged, the settings wouldn't make sense, so nothing was changed")?;
                config::check_plugins(&settings, &plugins::installed_names())
                    .context("merged, the settings wouldn't make sense, so nothing was changed")?;
                merged.config = Some(text);
            }
        }
        for (name, text) in &self.agents {
            check_file_name(name)?;
            agent_rules::check_text(text)
                .map_err(|err| anyhow::anyhow!("agents/{name}: {err}"))
                .context("nothing was changed")?;
            if read_if_there(&agents_dir.join(name))?.as_deref() != Some(text) {
                merged.agents.push((name.clone(), text.clone()));
            }
        }
        Ok(merged)
    }
}

/// The settings in `from`: a bundle, a config file, or a directory holding
/// a bundle, or else a config file, its agent rule files, or both; `-` reads
/// standard input.
fn read(from: &str) -> Result<Bundle> {
    if from == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("couldn't read standard input")?;
        return from_text(&text);
    }
    let path = Path::new(from);
    if !path.is_dir() {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("couldn't read {from}"))?;
        return from_text(&text).with_context(|| format!("in {from}"));
    }
    let bundle = path.join(FILE_NAME);
    if bundle.exists() {
        return read(&bundle.to_string_lossy());
    }
    let config = read_if_there(&path.join("config.toml"))?;
    let agents = agent_files(&path.join("agents"))?;
    ensure!(
        config.is_some() || !agents.is_empty(),
        "{from} holds neither {FILE_NAME}, config.toml nor agents/*.toml"
    );
    Ok(Bundle {
        crystal_bundle: BUNDLE,
        exported_by: String::new(),
        config,
        agents,
    })
}

/// A bundle, from its JSON, or a config file's text taken as one.
fn from_text(text: &str) -> Result<Bundle> {
    if text.trim_start().starts_with('{') {
        let bundle: Bundle = serde_json::from_str(text).context("it isn't a crystal bundle")?;
        ensure!(
            bundle.crystal_bundle <= BUNDLE,
            "it was exported by crystal {}, which writes what this one can't read: update crystal \
             first",
            bundle.exported_by
        );
        return Ok(bundle);
    }
    text.parse::<DocumentMut>()
        .context("it's neither a crystal bundle nor a config file")?;
    Ok(Bundle {
        crystal_bundle: BUNDLE,
        exported_by: String::new(),
        config: Some(text.to_string()),
        agents: BTreeMap::new(),
    })
}

/// Merges `imported` into `table`, the one at `path` (`""` for the file),
/// noting each setting that changes in `changed`: a table into a table, a
/// list of tables by what names each one, and anything else in place of
/// what's there, keeping the comment beside it.
fn merge_tables(table: &mut Table, imported: &Table, path: &str, changed: &mut Vec<String>) {
    for (key, item) in imported.iter() {
        let at = if path.is_empty() {
            key.to_string()
        } else {
            format!("{path}.{key}")
        };
        match (table.get_mut(key), item) {
            (_, Item::None) => {}
            (Some(Item::Table(here)), Item::Table(theirs)) => {
                merge_tables(here, theirs, &at, changed);
            }
            (Some(Item::ArrayOfTables(here)), Item::ArrayOfTables(theirs)) => {
                merge_lists(here, theirs, &at, changed);
            }
            (Some(Item::Value(here)), Item::Value(theirs)) => {
                if !same(&here.to_string(), &theirs.to_string()) {
                    let decor = here.decor().clone();
                    *here = theirs.clone();
                    *here.decor_mut() = decor;
                    changed.push(at);
                }
            }
            (Some(here), theirs) => {
                if !same(&here.to_string(), &theirs.to_string()) {
                    *here = theirs.clone();
                    changed.push(at);
                }
            }
            // A list of tables new here is told table by table.
            (None, Item::ArrayOfTables(theirs)) => {
                table.insert(key, Item::ArrayOfTables(ArrayOfTables::new()));
                if let Some(Item::ArrayOfTables(here)) = table.get_mut(key) {
                    merge_lists(here, theirs, &at, changed);
                }
            }
            (None, theirs) => {
                table.insert(key, theirs.clone());
                changed.push(at);
            }
        }
    }
}

/// Merges the list of tables `imported` into `list`: a table named as one
/// there takes its place, and one named as none goes at the end.
fn merge_lists(
    list: &mut ArrayOfTables,
    imported: &ArrayOfTables,
    path: &str,
    changed: &mut Vec<String>,
) {
    for theirs in imported.iter() {
        let id = identity(theirs);
        let found = list.iter_mut().find(|here| match &id {
            Some(id) => identity(here).as_ref() == Some(id),
            None => same(&here.to_string(), &theirs.to_string()),
        });
        let named = id.as_deref().map_or(String::new(), |id| format!(" {id}"));
        match found {
            Some(here) => {
                if !same(&here.to_string(), &theirs.to_string()) {
                    *here = theirs.clone();
                    changed.push(format!("{path}{named}"));
                }
            }
            None => {
                list.push(theirs.clone());
                changed.push(format!("{path}{named} (new)"));
            }
        }
    }
}

/// What tells one table of a list from the others: a profile's or a flow's
/// `name`, a project's `path`, a key command's `key`.
fn identity(table: &Table) -> Option<String> {
    ["name", "path", "key"].iter().find_map(|key| {
        let value = table.get(key)?.as_value()?;
        Some(match value.as_str() {
            Some(text) => text.to_string(),
            None => value.to_string().trim().to_string(),
        })
    })
}

/// Whether two pieces of TOML say the same, however they're laid out: as a
/// value, `x = …`, or else as a table.
fn same(a: &str, b: &str) -> bool {
    let read = |text: &str| -> Option<toml::Table> {
        toml::from_str(&format!("x = {text}"))
            .or_else(|_| toml::from_str(text))
            .ok()
    };
    match (read(a), read(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a.trim() == b.trim(),
    }
}

/// A rules file's name as `agents/` can hold it: a plain file name ending
/// in `.toml`, so a bundle can't write anywhere else.
fn check_file_name(name: &str) -> Result<()> {
    let plain = !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && name.len() > ".toml".len()
        && name.ends_with(".toml");
    if !plain {
        bail!("agents/{name} isn't a rules file's name, like claude.toml");
    }
    Ok(())
}

/// The text of the file at `path`, or `None` when there's none.
fn read_if_there(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("couldn't read {}", path.display())),
    }
}

/// The `.toml` files in `dir`, by name, with their texts.
fn agent_files(dir: &Path) -> Result<BTreeMap<String, String>> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(BTreeMap::new());
    };
    let mut files = BTreeMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if check_file_name(&name).is_err() || !entry.path().is_file() {
            continue;
        }
        let text = std::fs::read_to_string(entry.path())
            .with_context(|| format!("couldn't read {}", entry.path().display()))?;
        files.insert(name, text);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What merging `imported` into `here` makes of the file, and what it
    /// says changed.
    fn merged(here: &str, imported: &str) -> (String, Vec<String>) {
        let mut document: DocumentMut = here.parse().unwrap();
        let imported: DocumentMut = imported.parse().unwrap();
        let mut changed = Vec::new();
        merge_tables(
            document.as_table_mut(),
            imported.as_table(),
            "",
            &mut changed,
        );
        (document.to_string(), changed)
    }

    #[test]
    fn what_the_bundle_sets_replaces_whats_here_and_the_rest_stays() {
        let here = "# mine\ntheme = \"nord\" # the cold one\nnotify = false\n\n[sidebar]\nwidth = 30\nfolded = true\n";
        let imported = "theme = \"dracula\"\n\n[sidebar]\nwidth = 40\n\n[update]\ncheck = false\n";
        let (text, changed) = merged(here, imported);
        assert_eq!(changed, ["theme", "sidebar.width", "update"]);
        assert!(
            text.starts_with("# mine\ntheme = \"dracula\" # the cold one\nnotify = false\n"),
            "{text}"
        );
        let settings = config::from_text(&text).unwrap();
        assert_eq!(settings.theme.name(), "dracula");
        assert_eq!(settings.sidebar.width, 40);
        assert!(settings.sidebar.folded);
        assert!(!settings.update.check);
    }

    #[test]
    fn profiles_and_flows_merge_by_name_and_projects_by_path() {
        let here = "[[profile]]\nname = \"review\"\nagent = \"claude\"\n\n[[profile]]\nname = \"mine\"\nagent = \"codex\"\n\n[[project]]\npath = \"/work/app\"\nrun = \"make run\"\n";
        let imported = "[[profile]]\nname = \"review\"\nagent = \"claude\"\nmodel = \"opus\"\n\n[[profile]]\nname = \"new\"\nagent = \"claude\"\n\n[[project]]\npath = \"/work/app\"\nrun = \"make run\"\n";
        let (text, changed) = merged(here, imported);
        assert_eq!(changed, ["profile review", "profile new (new)"]);
        let settings = config::from_text(&text).unwrap();
        let names: Vec<&str> = settings.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["review", "mine", "new"]);
        assert_eq!(settings.profiles[0].model.as_deref(), Some("opus"));
        assert_eq!(settings.projects.len(), 1);
    }

    #[test]
    fn the_same_settings_laid_out_otherwise_change_nothing() {
        let here = "theme = 'nord'\ncolors = { accent = \"#ff0000\" }\n";
        let imported = "theme = \"nord\"\ncolors = {accent=\"#ff0000\"}\n";
        assert_eq!(merged(here, imported).1, Vec::<String>::new());
    }

    #[test]
    fn a_bundle_goes_out_and_comes_back_whole() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let agents = dir.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(&config_path, "# kept\ntheme = \"nord\"\n").unwrap();
        let claude = agent_rules::bundled("claude").unwrap();
        std::fs::write(agents.join("claude.toml"), claude).unwrap();
        std::fs::write(agents.join("notes.txt"), "not a rules file").unwrap();
        let bundle = Bundle::here(&config_path, &agents).unwrap();
        assert_eq!(bundle.config.as_deref(), Some("# kept\ntheme = \"nord\"\n"));
        assert_eq!(bundle.agents.keys().collect::<Vec<_>>(), ["claude.toml"]);
        let json = serde_json::to_string(&bundle).unwrap();
        assert_eq!(from_text(&json).unwrap(), bundle);

        // Into an empty machine, all of it; into this one, nothing.
        let other = tempfile::tempdir().unwrap();
        let (other_config, other_agents) = (other.path().join("c.toml"), other.path().join("a"));
        let merged = bundle.merge_into(&other_config, &other_agents).unwrap();
        assert_eq!(merged.changed, ["theme"]);
        merged.write(&other_config, &other_agents).unwrap();
        let written = std::fs::read_to_string(&other_config).unwrap();
        assert_eq!(config::from_text(&written).unwrap().theme.name(), "nord");
        assert_eq!(
            std::fs::read_to_string(other_agents.join("claude.toml")).unwrap(),
            claude
        );
        assert!(bundle.merge_into(&config_path, &agents).unwrap().is_empty());
    }

    #[test]
    fn nothing_is_written_unless_all_of_it_makes_sense() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let agents = dir.path().join("agents");
        let bundle = |config: &str, agents: &[(&str, &str)]| Bundle {
            crystal_bundle: BUNDLE,
            exported_by: VERSION.into(),
            config: Some(config.into()),
            agents: agents
                .iter()
                .map(|(name, text)| (name.to_string(), text.to_string()))
                .collect(),
        };
        let err = bundle("notfy = true\n", &[])
            .merge_into(&config_path, &agents)
            .unwrap_err();
        assert!(format!("{err:#}").contains("notfy"), "{err:#}");
        let err = bundle("theme = \"nord\"\n", &[("../config.toml", "x")])
            .merge_into(&config_path, &agents)
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("isn't a rules file's name"),
            "{err:#}"
        );
        let err = bundle("theme = \"nord\"\n", &[("pi.toml", "[[[")])
            .merge_into(&config_path, &agents)
            .unwrap_err();
        assert!(format!("{err:#}").contains("agents/pi.toml"), "{err:#}");
        assert!(!config_path.exists());
    }

    #[test]
    fn a_config_file_alone_or_a_bundle_from_a_newer_crystal() {
        let bundle = from_text("theme = \"nord\"\n").unwrap();
        assert_eq!(bundle.config.as_deref(), Some("theme = \"nord\"\n"));
        let newer = format!(
            "{{\"crystal_bundle\": {}, \"exported_by\": \"9.0.0\"}}",
            BUNDLE + 1
        );
        let err = from_text(&newer).unwrap_err();
        assert!(format!("{err:#}").contains("crystal 9.0.0"), "{err:#}");
        assert!(from_text("{ not json").is_err());
    }
}
