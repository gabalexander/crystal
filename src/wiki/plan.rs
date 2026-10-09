//! The outline of a wiki: its sections, each with its subsections, each
//! subsection with the files and directories it covers. One Claude reads
//! the repository and plans it; what it answers is read and checked here:
//! titles, how many sections and subsections, paths the repository has,
//! and every source file covered by some subsection. A plan that fails is
//! sent back once with what's wrong; source files still left out then go
//! to the subsection nearest them by path.

use super::files::{self, Files};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The outline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// What the overview is to say: what the repository is, and its
    /// architecture.
    pub overview: String,
    pub sections: Vec<PlannedSection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedSection {
    pub id: String,
    pub title: String,
    /// What it covers, for whoever writes its summary.
    pub about: String,
    pub subsections: Vec<PlannedSubsection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedSubsection {
    pub id: String,
    pub title: String,
    /// What its writer is to explain.
    pub about: String,
    /// The files and directories it covers, a directory ending with `/`.
    pub files: Vec<String>,
}

/// The id the overview has on the page, which no section may take.
pub const OVERVIEW_ID: &str = "overview";

/// How many sections, and subsections in each, a repository of `lines`
/// lines of source has: a small one can't fill the large one's.
pub fn bounds(lines: u64) -> ((usize, usize), (usize, usize)) {
    if lines < 2_000 {
        ((1, 4), (1, 4))
    } else if lines < 20_000 {
        ((3, 10), (2, 7))
    } else {
        ((6, 16), (3, 9))
    }
}

impl Plan {
    /// Every subsection, with its section, in order.
    pub fn subsections(&self) -> impl Iterator<Item = (&PlannedSection, &PlannedSubsection)> {
        self.sections
            .iter()
            .flat_map(|section| section.subsections.iter().map(move |sub| (section, sub)))
    }

    /// Every anchor on the page.
    pub fn anchors(&self) -> BTreeSet<String> {
        let mut anchors: BTreeSet<String> = [OVERVIEW_ID.to_string()].into();
        for section in &self.sections {
            anchors.insert(section.id.clone());
            anchors.extend(section.subsections.iter().map(|sub| sub.id.clone()));
        }
        anchors
    }

    /// The source files no subsection covers.
    pub fn uncovered<'a>(&self, files: &'a Files) -> Vec<&'a str> {
        let entries: Vec<String> = self
            .subsections()
            .flat_map(|(_, sub)| sub.files.iter().cloned())
            .collect();
        files
            .sources()
            .filter(|file| !files::covers(&entries, &file.path))
            .map(|file| file.path.as_str())
            .collect()
    }

    /// The outline as each writer is shown it: every section and
    /// subsection by its id, title and what it covers.
    pub fn outline(&self) -> String {
        let mut out = String::new();
        for section in &self.sections {
            out.push_str(&format!(
                "- {} (#{}): {}\n",
                section.title, section.id, section.about
            ));
            for sub in &section.subsections {
                out.push_str(&format!(
                    "  - {} (#{}): {} Files: {}\n",
                    sub.title,
                    sub.id,
                    sub.about,
                    sub.files.join(", ")
                ));
            }
        }
        out
    }

    /// Gives each source file no subsection covers to the subsection
    /// nearest it: the one with a path sharing the most directories with
    /// it. Returns how many it gave.
    pub fn cover(&mut self, files: &Files) -> usize {
        let uncovered: Vec<String> = self
            .uncovered(files)
            .into_iter()
            .map(str::to_string)
            .collect();
        for path in &uncovered {
            let mut best: Option<(usize, usize, usize)> = None;
            for (s, section) in self.sections.iter().enumerate() {
                for (u, sub) in section.subsections.iter().enumerate() {
                    let shared = sub
                        .files
                        .iter()
                        .map(|entry| shared_dirs(entry, path))
                        .max()
                        .unwrap_or(0);
                    if best.is_none_or(|(most, _, _)| shared > most) {
                        best = Some((shared, s, u));
                    }
                }
            }
            if let Some((_, s, u)) = best {
                self.sections[s].subsections[u].files.push(path.clone());
            }
        }
        uncovered.len()
    }
}

/// How many directories `entry` and `path` share from the top.
fn shared_dirs(entry: &str, path: &str) -> usize {
    let dirs = |p: &str| -> Vec<String> {
        let mut parts: Vec<String> = p.split('/').map(str::to_string).collect();
        parts.pop();
        parts
    };
    dirs(entry)
        .iter()
        .zip(dirs(path).iter())
        .take_while(|(a, b)| a == b)
        .count()
}

/// The shape of the planner's answer, for `--json-schema`.
pub fn schema() -> Value {
    let text = json!({"type": "string"});
    json!({
        "type": "object",
        "properties": {
            "overview": text,
            "sections": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": text,
                        "about": text,
                        "subsections": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "title": text,
                                    "about": text,
                                    "files": {"type": "array", "items": text},
                                },
                                "required": ["title", "about", "files"],
                            },
                        },
                    },
                    "required": ["title", "about", "subsections"],
                },
            },
        },
        "required": ["overview", "sections"],
    })
}

/// What the planner is told it's doing.
pub const SYSTEM: &str = "You are planning a wiki for a software repository, in the manner of \
Google's Code Wiki: one long page that explains how the code works to an engineer who is new to \
it, section by section, each subsection with a diagram and prose that names the real functions, \
types and files. You plan; other writers write each subsection from your outline, one each, so \
the outline decides what the page covers and how a reader meets it.\n\n\
First explore: read the README and any docs or agent notes (AGENTS.md, CLAUDE.md), find the \
entry points (the main function, the CLI, the server, the public API), and read enough of the \
main modules to see what the system does and how its parts work together. Use Glob and Grep to \
find things and Read to read them; you can't run commands. The message lists every file with \
its lines.\n\n\
Then answer with the outline:\n\
- overview: two to four sentences on what the repository is and the big parts of its \
architecture, for whoever writes the overview.\n\
- sections: each a part of what the system does (a capability, a subsystem, a flow), named for \
what it does rather than for a directory: \"Sessions and the Daemon\", \"Drawing Diagrams as \
Text\", not \"src/tui\". Order them as a reader should meet them: the core first, then what's \
built on it, then tooling, testing and release. Each has an about: one or two sentences on what \
it covers.\n\
- subsections: each a specific component, mechanism or flow, with a title that says what it is \
(\"Handing the Daemon Over to a New Binary\"), an about (one or two sentences on what its writer \
is to explain: what it does, how, and how it connects to the rest), and files: the files and \
directories it covers, paths from the top of the repository, a directory ending with /. Aim for \
roughly 300 to 3000 lines of code a subsection: split a big module or file into several \
subsections by what its parts do, and join small related files into one.\n\
- Every file the message marks [src] must be covered by some subsection's files. Tests go with \
what they test, or in a section on how the project is tested. Docs, config and assets may be \
listed where they help, and needn't be covered.\n\
- Titles in Title Case, specific, without file names. No section called Overview, \
Introduction, Miscellaneous or Other: the page has its own overview, and everything has a \
place.";

/// The planner's message: the repository's files, with which are source
/// and their lines, and how many sections to plan.
pub fn message(name: &str, commit: &str, files: &Files) -> String {
    let lines: u64 = files.sources().map(|file| u64::from(file.lines)).sum();
    let ((lo, hi), (sub_lo, sub_hi)) = bounds(lines);
    format!(
        "Plan the wiki of {name}, at commit {commit}.\n\n\
         Plan {lo} to {hi} sections, each with {sub_lo} to {sub_hi} subsections. The repository \
         has {} files, {} of them source, {lines} lines of source in all.\n\n\
         Its files, from the top, each with its lines; [src] marks the source files every wiki \
         must cover:\n\n{}",
        files.files.len(),
        files.sources().count(),
        listing(files),
    )
}

/// The most files listed one a line; past it, directories are.
const MOST_LISTED: usize = 1500;

/// The files, one a line, or for a big repository, each directory with
/// how many files and lines it has and the first of its files' names.
pub fn listing(files: &Files) -> String {
    let line = |file: &files::File| {
        let src = if files::is_source(file) { " [src]" } else { "" };
        if file.text {
            format!("{} ({} lines){src}\n", file.path, file.lines)
        } else {
            format!("{} (binary or large)\n", file.path)
        }
    };
    if files.files.len() <= MOST_LISTED {
        return files.files.values().map(line).collect();
    }
    let mut dirs: std::collections::BTreeMap<&str, Vec<&files::File>> = Default::default();
    for file in files.files.values() {
        let dir = file.path.rsplit_once('/').map_or("", |(dir, _)| dir);
        dirs.entry(dir).or_default().push(file);
    }
    let mut out = String::new();
    for (dir, in_dir) in dirs {
        let lines: u64 = in_dir.iter().map(|file| u64::from(file.lines)).sum();
        let sources = in_dir.iter().filter(|file| files::is_source(file)).count();
        let names: Vec<&str> = in_dir
            .iter()
            .take(8)
            .map(|file| file.path.rsplit('/').next().unwrap_or(&file.path))
            .collect();
        let more = if in_dir.len() > names.len() {
            ", …"
        } else {
            ""
        };
        let dir = if dir.is_empty() { "." } else { dir };
        out.push_str(&format!(
            "{dir}/: {} files, {sources} [src], {lines} lines: {}{more}\n",
            in_dir.len(),
            names.join(", ")
        ));
    }
    out
}

/// The message asking the planner to put right what's wrong with `plan`.
pub fn fix_message(plan: &Value, problems: &[String], files: &Files) -> String {
    format!(
        "Your outline below has problems. Answer with the whole outline again, put right.\n\n\
         Problems:\n{}\n\nYour outline:\n{}\n\nThe repository's files:\n\n{}",
        problems
            .iter()
            .map(|problem| format!("- {problem}"))
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::to_string_pretty(plan).unwrap_or_default(),
        listing(files),
    )
}

/// The planner's answer read into a plan, its ids made, with what's
/// wrong with it. Paths the repository doesn't have are left out. It
/// fails only for an answer that isn't an outline at all.
pub fn read(answer: &Value, files: &Files) -> Result<(Plan, Vec<String>)> {
    let mut problems = Vec::new();
    let text = |value: &Value| value.as_str().unwrap_or_default().trim().to_string();
    let Some(given) = answer["sections"].as_array().filter(|s| !s.is_empty()) else {
        bail!("its outline has no sections");
    };
    let lines: u64 = files.sources().map(|file| u64::from(file.lines)).sum();
    let ((lo, hi), (sub_lo, sub_hi)) = bounds(lines);
    if given.len() < lo || given.len() > hi {
        problems.push(format!(
            "it has {} sections: plan {lo} to {hi}",
            given.len()
        ));
    }
    let mut ids = Ids::default();
    let mut sections = Vec::new();
    for section in given {
        let title = text(&section["title"]);
        if title.is_empty() {
            problems.push("a section has no title".into());
            continue;
        }
        let id = ids.take(&title, None);
        let subs = section["subsections"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if subs.len() < sub_lo || subs.len() > sub_hi {
            problems.push(format!(
                "the section {title:?} has {} subsections: give each {sub_lo} to {sub_hi}",
                subs.len()
            ));
        }
        let mut subsections = Vec::new();
        for sub in &subs {
            let sub_title = text(&sub["title"]);
            if sub_title.is_empty() {
                problems.push(format!("a subsection of {title:?} has no title"));
                continue;
            }
            let mut entries: Vec<String> = Vec::new();
            for path in sub["files"].as_array().into_iter().flatten() {
                let path = text(path);
                match files.entry(&path) {
                    Some(entry) if !entries.contains(&entry) => entries.push(entry),
                    Some(_) => {}
                    None => problems.push(format!(
                        "the subsection {sub_title:?} names {path}, which the repository \
                         doesn't have"
                    )),
                }
            }
            if entries.is_empty() {
                problems.push(format!(
                    "the subsection {sub_title:?} covers no file the repository has"
                ));
                continue;
            }
            subsections.push(PlannedSubsection {
                id: ids.take(&sub_title, Some(&id)),
                title: sub_title,
                about: text(&sub["about"]),
                files: entries,
            });
        }
        if subsections.is_empty() {
            continue;
        }
        sections.push(PlannedSection {
            id,
            title,
            about: text(&section["about"]),
            subsections,
        });
    }
    if sections.is_empty() {
        bail!("its outline has no subsection covering a file the repository has");
    }
    let plan = Plan {
        overview: text(&answer["overview"]),
        sections,
    };
    let uncovered = plan.uncovered(files);
    if !uncovered.is_empty() {
        let shown: Vec<&str> = uncovered.iter().take(60).copied().collect();
        let more = uncovered.len() - shown.len();
        problems.push(format!(
            "no subsection covers these source files: {}{}",
            shown.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        ));
    }
    Ok((plan, problems))
}

/// The ids given so far, so that each is given once.
#[derive(Default)]
struct Ids {
    taken: BTreeSet<String>,
}

impl Ids {
    /// An id for `title`, its slug, or under `parent`'s when that's taken,
    /// or numbered.
    fn take(&mut self, title: &str, parent: Option<&str>) -> String {
        let slug = slug(title);
        let slug = if slug.is_empty() {
            "part".to_string()
        } else {
            slug
        };
        let mut tries = vec![slug.clone()];
        if let Some(parent) = parent {
            tries.push(format!("{parent}-{slug}"));
        }
        let id = tries
            .into_iter()
            .find(|id| id != OVERVIEW_ID && !self.taken.contains(id))
            .unwrap_or_else(|| {
                (2..)
                    .map(|n| format!("{slug}-{n}"))
                    .find(|id| !self.taken.contains(id))
                    .expect("a number nobody has")
            });
        self.taken.insert(id.clone());
        id
    }
}

/// `title` as an anchor: lower case, words joined by `-`.
pub fn slug(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    let mut cut: String = slug.chars().take(64).collect();
    while cut.ends_with('-') {
        cut.pop();
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiki::files::File;

    fn files(paths: &[&str]) -> Files {
        let mut files = Files::default();
        for path in paths {
            files.add(File {
                path: path.to_string(),
                blob: "b".into(),
                lines: 100,
                bytes: 1000,
                text: true,
            });
        }
        files
    }

    #[test]
    fn titles_make_ids_once_each() {
        assert_eq!(
            slug("Handing the Daemon Over (to a New Binary)"),
            "handing-the-daemon-over-to-a-new-binary"
        );
        assert_eq!(slug("  C++ & Rust!  "), "c-rust");
        let mut ids = Ids::default();
        assert_eq!(ids.take("Sessions", None), "sessions");
        assert_eq!(ids.take("Sessions", Some("core")), "core-sessions");
        assert_eq!(ids.take("Sessions", Some("core")), "sessions-2");
        assert_eq!(ids.take("Overview", None), "overview-2");
        assert_eq!(ids.take("???", None), "part");
    }

    #[test]
    fn a_plan_is_read_with_what_is_wrong_with_it() {
        let files = files(&[
            "src/main.rs",
            "src/tui/app.rs",
            "src/tui/ui.rs",
            "src/db.rs",
            "README.md",
        ]);
        let answer = json!({
            "overview": "An app.",
            "sections": [
                {"title": "The Interface", "about": "What you see.", "subsections": [
                    {"title": "Drawing", "about": "How it draws.", "files": ["src/tui", "src/gone.rs"]},
                    {"title": "Starting", "about": "Main.", "files": ["./src/main.rs", "README.md"]}
                ]},
                {"title": "Nothing", "about": "", "subsections": [
                    {"title": "Ghost", "about": "", "files": ["nowhere/"]}
                ]}
            ]
        });
        let (plan, problems) = read(&answer, &files).unwrap();
        assert_eq!(plan.sections.len(), 1);
        let section = &plan.sections[0];
        assert_eq!(section.id, "the-interface");
        assert_eq!(section.subsections[0].files, ["src/tui/"]);
        assert_eq!(section.subsections[1].files, ["src/main.rs", "README.md"]);
        assert!(
            problems.iter().any(|p| p.contains("src/gone.rs")),
            "{problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("\"Ghost\" covers no file")),
            "{problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("these source files: src/db.rs")),
            "{problems:?}"
        );
        assert_eq!(plan.anchors().len(), 4);
        assert!(
            plan.outline()
                .contains("  - Drawing (#drawing): How it draws. Files: src/tui/\n")
        );
        assert!(read(&json!({"sections": []}), &files).is_err());
    }

    #[test]
    fn source_files_left_out_go_to_the_nearest_subsection() {
        let files = files(&[
            "src/tui/app.rs",
            "src/tui/sub/deep.rs",
            "src/db.rs",
            "tests/cli.rs",
        ]);
        let mut plan = Plan {
            overview: String::new(),
            sections: vec![PlannedSection {
                id: "s".into(),
                title: "S".into(),
                about: String::new(),
                subsections: vec![
                    PlannedSubsection {
                        id: "a".into(),
                        title: "A".into(),
                        about: String::new(),
                        files: vec!["src/db.rs".into()],
                    },
                    PlannedSubsection {
                        id: "b".into(),
                        title: "B".into(),
                        about: String::new(),
                        files: vec!["src/tui/app.rs".into()],
                    },
                ],
            }],
        };
        assert_eq!(plan.cover(&files), 2);
        let subs = &plan.sections[0].subsections;
        assert_eq!(subs[0].files, ["src/db.rs", "tests/cli.rs"]);
        assert_eq!(subs[1].files, ["src/tui/app.rs", "src/tui/sub/deep.rs"]);
        assert!(plan.uncovered(&files).is_empty());
    }

    #[test]
    fn a_small_repository_has_fewer_sections() {
        assert_eq!(bounds(500), ((1, 4), (1, 4)));
        assert_eq!(bounds(120_000), ((6, 16), (3, 9)));
        let message = message("app", "abc", &files(&["src/main.rs", "README.md"]));
        assert!(message.contains("Plan 1 to 4 sections"), "{message}");
        assert!(
            message.contains("src/main.rs (100 lines) [src]\n"),
            "{message}"
        );
        assert!(message.contains("README.md (100 lines)\n"), "{message}");
    }
}
