//! `crystal wiki index`: the index of a project's code, as `crystal wiki
//! build` builds it, built on its own, to see how each language of the
//! project is indexed and why, and what a code span would link to.

use super::{Index, Lookup};
use crate::config::Config;
use crate::output::outln;
use crate::{git, plugins, project, wiki};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::path::{Path, PathBuf};

/// What `crystal wiki index` was asked.
pub struct Asked {
    /// The project's directory, or the current one.
    pub dir: Option<PathBuf>,
    /// The commit to index, or the project's `HEAD`.
    pub commit: Option<String>,
    /// Spans to look up.
    pub spans: Vec<String>,
    /// The files the spans are about, preferred among several.
    pub near: Vec<String>,
    pub json: bool,
}

/// Builds the index of the project `asked.dir` is in, in its wiki's
/// directory, and prints how each language was indexed and what each span
/// links to.
pub fn run(socket: &Path, asked: Asked) -> Result<()> {
    let config = Config::load()?;
    plugins::ensure_enabled(&config, "wiki")?;
    let dir = match asked.dir {
        Some(dir) => dir,
        None => std::env::current_dir().context("couldn't tell the current directory")?,
    };
    let project = project::of(&dir);
    let commit = match asked.commit {
        Some(commit) => commit,
        None => git::head(&project.path),
    };
    if commit.is_empty() {
        bail!("{} has no commit to index", project.name);
    }
    let cache = wiki::index_dir(socket, &project.path);
    let index = Index::build(&project.path, &commit, &cache, &config.wiki.index)?;
    let looked: Vec<(&String, Lookup)> = (asked.spans.iter())
        .map(|span| (span, index.lookup(span, &asked.near)))
        .collect();
    if asked.json {
        let lookups: Vec<serde_json::Value> = looked
            .iter()
            .map(|(span, found)| match found {
                Lookup::Unique(def) => json!({"span": span, "found": "unique", "defs": [def]}),
                Lookup::Ambiguous(defs) => {
                    json!({"span": span, "found": "ambiguous", "defs": defs})
                }
                Lookup::Missing => json!({"span": span, "found": "missing", "defs": []}),
            })
            .collect();
        let report = json!({
            "commit": commit,
            "files": index.files.len(),
            "definitions": index.defs.len(),
            "seconds": index.took.as_secs_f64(),
            "languages": index.report(),
            "lookups": lookups,
        });
        outln!("{}", serde_json::to_string_pretty(&report)?)?;
        return Ok(());
    }
    if looked.is_empty() {
        for line in index.summary() {
            outln!("{line}")?;
        }
    }
    for (span, found) in &looked {
        match found {
            Lookup::Unique(def) => {
                outln!("{span} → {} ({} {})", def.target(), def.kind, def.qualified)?
            }
            Lookup::Ambiguous(defs) => {
                let some: Vec<String> = defs.iter().take(5).map(|def| def.target()).collect();
                let more = match defs.len() > 5 {
                    true => format!(", and {} more", defs.len() - 5),
                    false => String::new(),
                };
                outln!("{span}: could be {}{more}", some.join(", "))?
            }
            Lookup::Missing => outln!("{span}: nothing here")?,
        }
    }
    Ok(())
}
