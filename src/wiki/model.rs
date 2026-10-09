//! `wiki.json`: a project's wiki as the generator writes it and the page,
//! `crystal wiki serve` and `crystal wiki export` read it. Its shape is a
//! contract between them: a field can be added, never renamed or taken
//! away, and [`VERSION`] goes up when one changes what it means.
//!
//! The markdown is CommonMark with GitHub's tables and fenced code. A link
//! into the code is written `[label](code:PATH)`, `code:PATH#L10` or
//! `code:PATH#L10-L20`, PATH from the top of the repository, and a link to
//! another part of the page `[label](#id)`. The generator has already made
//! every code span that names something in the repository such a link, so
//! the page only renders.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// What `version` says of the shape below.
pub const VERSION: u32 = 1;

/// A repository's wiki: one page, its overview, then its sections, each
/// with its subsections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wiki {
    pub version: u32,
    pub repo: Repo,
    pub generated: Generated,
    pub overview: Overview,
    pub sections: Vec<Section>,
}

/// The repository a wiki is about, at the commit it was written from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Repo {
    /// `owner/name` on its forge, or else its directory's name.
    pub name: String,
    /// The main worktree it was made from.
    pub root: PathBuf,
    /// The commit every link points into, in full.
    pub commit: String,
    /// The default branch that commit was the tip of.
    pub branch: String,
    /// The repository on its forge, or `None` without one.
    pub web_url: Option<String>,
    /// Where a file is on the forge at the commit: `{commit}` and `{path}`
    /// filled in, a line appended as `#L10` or `#L10-L20`. `None` without
    /// a forge.
    pub code_url: Option<String>,
}

/// Who made it, when, and what it cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Generated {
    /// When it was last written, in UTC: `2026-10-09T12:00:00Z`.
    pub at: String,
    /// The model, as the page names it: `Claude Sonnet 5.5`.
    pub by: String,
    /// The model, as `claude --model` takes it.
    pub model: String,
    /// What writing it cost, every build and update of it, in US dollars.
    pub cost_usd: f64,
    /// The crystal that wrote it.
    pub crystal: String,
}

/// What comes before the sections: what the repository is, and its
/// architecture.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Overview {
    pub summary_md: String,
    pub diagram: Option<Diagram>,
}

/// A part of what the system does: a top section, its summary and diagram
/// over its subsections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    /// Its anchor on the page, unique across the page.
    pub id: String,
    pub title: String,
    pub summary_md: String,
    pub diagram: Option<Diagram>,
    pub subsections: Vec<Subsection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subsection {
    /// Its anchor on the page, unique across the page.
    pub id: String,
    pub title: String,
    pub body_md: String,
    pub diagram: Option<Diagram>,
    /// The files and directories it covers, from the top of the
    /// repository; a directory ends with `/`.
    pub files: Vec<String>,
}

/// A mermaid diagram, with the sentence under it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagram {
    pub mermaid: String,
    pub caption: String,
}

impl Wiki {
    /// The wiki in `path`.
    pub fn read(path: &Path) -> Result<Wiki> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("couldn't read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("{} isn't a wiki", path.display()))
    }

    /// Writes it to `path` in one go: whoever reads it sees the wiki before
    /// or after, never half of it.
    pub fn write(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self).expect("a wiki makes JSON");
        write_whole(path, &text)
    }

    /// How many subsections it has, across its sections.
    pub fn subsections(&self) -> usize {
        self.sections.iter().map(|s| s.subsections.len()).sum()
    }

    /// How many diagrams it has: the overview's, the sections' and the
    /// subsections'.
    pub fn diagrams(&self) -> usize {
        let section = |s: &Section| {
            usize::from(s.diagram.is_some())
                + s.subsections
                    .iter()
                    .filter(|sub| sub.diagram.is_some())
                    .count()
        };
        usize::from(self.overview.diagram.is_some())
            + self.sections.iter().map(section).sum::<usize>()
    }

    /// Every text on the page, the overview's first, then each section's
    /// and its subsections', in order.
    pub fn texts(&self) -> Vec<&str> {
        let mut texts = vec![self.overview.summary_md.as_str()];
        for section in &self.sections {
            texts.push(&section.summary_md);
            texts.extend(section.subsections.iter().map(|s| s.body_md.as_str()));
        }
        texts
    }
}

/// A link into the code, `code:PATH`, `code:PATH#L10` or
/// `code:PATH#L10-L20`, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeLink {
    /// From the top of the repository.
    pub path: String,
    pub start: Option<u32>,
    pub end: Option<u32>,
}

/// What a link into the code starts with.
pub const CODE_SCHEME: &str = "code:";

impl CodeLink {
    /// `target`, a link's destination, read as a link into the code, or
    /// `None` when it isn't one. `#L10-20` is taken for `#L10-L20`.
    pub fn parse(target: &str) -> Option<CodeLink> {
        let rest = target.strip_prefix(CODE_SCHEME)?;
        let (path, lines) = match rest.split_once('#') {
            Some((path, lines)) => (path, Some(lines)),
            None => (rest, None),
        };
        let path = path.trim().trim_start_matches("./");
        if path.is_empty() {
            return None;
        }
        let (start, end) = match lines {
            None => (None, None),
            Some(lines) => {
                let lines = lines.strip_prefix('L').unwrap_or(lines);
                let (start, end) = match lines.split_once('-') {
                    Some((start, end)) => (start, Some(end.trim_start_matches('L'))),
                    None => (lines, None),
                };
                let start: u32 = start.parse().ok()?;
                let end: Option<u32> = match end {
                    Some(end) => Some(end.parse().ok()?),
                    None => None,
                };
                (Some(start), end)
            }
        };
        Some(CodeLink {
            path: path.to_string(),
            start,
            end,
        })
    }

    /// The link as the markdown writes it.
    pub fn target(&self) -> String {
        match (self.start, self.end) {
            (Some(start), Some(end)) if end > start => {
                format!("{CODE_SCHEME}{}#L{start}-L{end}", self.path)
            }
            (Some(start), _) => format!("{CODE_SCHEME}{}#L{start}", self.path),
            _ => format!("{CODE_SCHEME}{}", self.path),
        }
    }
}

/// Writes `text` to `path` through a file beside it, renamed over it.
pub fn write_whole(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    let tmp = path.with_file_name(name);
    fs::write(&tmp, text).with_context(|| format!("couldn't write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("couldn't write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_link_reads_its_path_and_lines() {
        let link = CodeLink::parse("code:src/x.rs#L10-L20").unwrap();
        assert_eq!(
            link,
            CodeLink {
                path: "src/x.rs".into(),
                start: Some(10),
                end: Some(20)
            }
        );
        assert_eq!(link.target(), "code:src/x.rs#L10-L20");
        assert_eq!(CodeLink::parse("code:src/x.rs#L10-20"), Some(link));
        let line = CodeLink::parse("code:./src/x.rs#L7").unwrap();
        assert_eq!(
            (line.path.as_str(), line.start, line.end),
            ("src/x.rs", Some(7), None)
        );
        assert_eq!(line.target(), "code:src/x.rs#L7");
        assert_eq!(CodeLink::parse("code:src/").unwrap().target(), "code:src/");
        assert_eq!(CodeLink::parse("https://example.com"), None);
        assert_eq!(CodeLink::parse("code:src/x.rs#Lten"), None);
        assert_eq!(CodeLink::parse("code:"), None);
    }

    #[test]
    fn a_wiki_reads_back_as_it_was_written_with_its_nulls() {
        let wiki = Wiki {
            version: VERSION,
            repo: Repo {
                name: "app".into(),
                root: "/code/app".into(),
                commit: "c0ffee".into(),
                branch: "main".into(),
                web_url: None,
                code_url: None,
            },
            generated: Generated {
                at: "2026-10-09T12:00:00Z".into(),
                by: "Claude Sonnet 5.5".into(),
                model: "claude-sonnet-5-5".into(),
                cost_usd: 1.5,
                crystal: "0.3.0".into(),
            },
            overview: Overview::default(),
            sections: vec![Section {
                id: "s".into(),
                title: "S".into(),
                summary_md: String::new(),
                diagram: None,
                subsections: vec![Subsection {
                    id: "t".into(),
                    title: "T".into(),
                    body_md: "body".into(),
                    diagram: Some(Diagram {
                        mermaid: "flowchart TD\n a --> b".into(),
                        caption: "c".into(),
                    }),
                    files: vec!["src/".into()],
                }],
            }],
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w/wiki.json");
        wiki.write(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"web_url\": null"), "{text}");
        assert!(text.contains("\"diagram\": null"), "{text}");
        assert_eq!(Wiki::read(&path).unwrap(), wiki);
        assert_eq!((wiki.subsections(), wiki.diagrams()), (1, 1));
    }
}
