//! The repository's files at a commit, as git has them: every path with its
//! blob from `git ls-tree`, and the blobs' contents from one `git cat-file
//! --batch`; each file's language by its name; and the module or package a
//! language names what a file defines under.

use super::IndexSettings;
use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

/// A file git tracks at the commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// From the top of the repository.
    pub path: String,
    /// Its blob's hash, which what's read of it is kept by.
    pub blob: String,
    pub size: u64,
    pub lang: Option<Lang>,
}

/// The languages crystal tells files apart by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lang {
    Rust,
    Go,
    Python,
    TypeScript,
    JavaScript,
    Java,
    Kotlin,
    C,
    Cpp,
    CSharp,
    Ruby,
    Swift,
    Php,
    Shell,
    Scala,
    Lua,
    Elixir,
    Dart,
    Zig,
    Perl,
    Groovy,
}

impl Lang {
    /// A file's language by its name, or `None` for one that isn't code.
    pub fn of(path: &str) -> Option<Lang> {
        let name = path.rsplit('/').next().unwrap_or(path);
        if name.ends_with(".min.js") || name.ends_with(".d.ts") && name != "index.d.ts" {
            return None;
        }
        let extension = name.rsplit_once('.')?.1;
        Some(match extension {
            "rs" => Lang::Rust,
            "go" => Lang::Go,
            "py" | "pyi" => Lang::Python,
            "ts" | "tsx" | "mts" | "cts" => Lang::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
            "java" => Lang::Java,
            "kt" | "kts" => Lang::Kotlin,
            "c" => Lang::C,
            // A header could be either; C++'s grammar reads C's too.
            "h" | "cc" | "cpp" | "cxx" | "c++" | "hh" | "hpp" | "hxx" | "h++" | "ipp" | "tpp" => {
                Lang::Cpp
            }
            "cs" => Lang::CSharp,
            "rb" | "rake" => Lang::Ruby,
            "swift" => Lang::Swift,
            "php" => Lang::Php,
            "sh" | "bash" | "zsh" => Lang::Shell,
            "scala" | "sc" => Lang::Scala,
            "lua" => Lang::Lua,
            "ex" | "exs" => Lang::Elixir,
            "dart" => Lang::Dart,
            "zig" => Lang::Zig,
            "pl" | "pm" => Lang::Perl,
            "groovy" | "gradle" => Lang::Groovy,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "Rust",
            Lang::Go => "Go",
            Lang::Python => "Python",
            Lang::TypeScript => "TypeScript",
            Lang::JavaScript => "JavaScript",
            Lang::Java => "Java",
            Lang::Kotlin => "Kotlin",
            Lang::C => "C",
            Lang::Cpp => "C++",
            Lang::CSharp => "C#",
            Lang::Ruby => "Ruby",
            Lang::Swift => "Swift",
            Lang::Php => "PHP",
            Lang::Shell => "shell",
            Lang::Scala => "Scala",
            Lang::Lua => "Lua",
            Lang::Elixir => "Elixir",
            Lang::Dart => "Dart",
            Lang::Zig => "Zig",
            Lang::Perl => "Perl",
            Lang::Groovy => "Groovy",
        }
    }

    /// What the language puts between a name and what it's in.
    pub fn separator(self) -> &'static str {
        match self {
            Lang::Rust | Lang::C | Lang::Cpp | Lang::Ruby | Lang::Perl | Lang::Php => "::",
            _ => ".",
        }
    }
}

/// Whether the file is read for definitions: code, not too big, and not in
/// a directory the settings leave out.
pub fn readable(entry: &Entry, settings: &IndexSettings) -> bool {
    entry.size <= settings.max_file_kb * 1024 && !excluded(&entry.path, &settings.exclude)
}

/// Whether `path` is in a directory `exclude` names, by its name anywhere
/// or by its path from the top.
pub fn excluded(path: &str, exclude: &[String]) -> bool {
    let Some((dir, _)) = path.rsplit_once('/') else {
        return false;
    };
    exclude.iter().any(|left_out| {
        let left_out = left_out.trim_matches('/');
        if left_out.contains('/') {
            dir == left_out || dir.starts_with(&format!("{left_out}/"))
        } else {
            dir.split('/').any(|part| part == left_out)
        }
    })
}

/// Whether the file at `path` is a test's, by where it is and what it's
/// called in the languages crystal knows.
pub fn is_test(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.split('.').next().unwrap_or(name);
    path.split('/').rev().skip(1).any(|dir| {
        matches!(
            dir,
            "tests" | "test" | "__tests__" | "spec" | "testing" | "benches"
        )
    }) || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_spec")
        || stem.ends_with("Test")
        || stem.ends_with("Tests")
        || name.contains(".test.")
        || name.contains(".spec.")
}

/// The module or package the file at `path` defines what's in it under,
/// as its language names it: Rust's module path from where the file is
/// under `src`, Python's dotted path, Go's and Java's package as the file
/// says, a script's name.
pub fn module(lang: Lang, path: &str, package: &[String]) -> Vec<String> {
    if !package.is_empty() {
        return package.to_vec();
    }
    let parts: Vec<&str> = path.split('/').collect();
    let (stem, dirs) = match parts.split_last() {
        Some((name, dirs)) => (name.split('.').next().unwrap_or(name), dirs),
        None => return Vec::new(),
    };
    match lang {
        Lang::Rust => {
            // Under the last `src`, its path; a test's, a bench's or an
            // example's file is a crate of its own, named after it.
            let from = (dirs.iter().rposition(|dir| *dir == "src")).map_or(dirs.len(), |at| at + 1);
            let mut module: Vec<String> = dirs[from..].iter().map(|dir| dir.to_string()).collect();
            if !matches!(stem, "mod" | "lib" | "main") {
                module.push(stem.to_string());
            }
            module
        }
        Lang::Python => {
            let from = usize::from(matches!(dirs.first(), Some(&"src") | Some(&"lib")));
            let mut module: Vec<String> = dirs
                .get(from..)
                .unwrap_or_default()
                .iter()
                .map(|dir| dir.to_string())
                .collect();
            if stem != "__init__" {
                module.push(stem.to_string());
            }
            module
        }
        Lang::TypeScript | Lang::JavaScript => match stem {
            "index" => dirs
                .last()
                .map(|dir| vec![dir.to_string()])
                .unwrap_or_default(),
            _ => vec![stem.to_string()],
        },
        _ => Vec::new(),
    }
}

/// Every file git tracks at `commit` in the repository at `root`, but its
/// submodules and symbolic links.
pub fn list(root: &Path, commit: &str) -> Result<Vec<Entry>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-tree", "-r", "-z", "--full-tree", "-l", commit])
        .stderr(Stdio::piped())
        .output()
        .context("couldn't run git")?;
    if !output.status.success() {
        bail!(
            "git ls-tree {commit} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(parse_tree(&output.stdout))
}

/// Reads `git ls-tree -r -z -l`: each record `<mode> <type> <blob> <size>\t<path>`.
fn parse_tree(listed: &[u8]) -> Vec<Entry> {
    listed
        .split(|&byte| byte == 0)
        .filter_map(|record| {
            let record = std::str::from_utf8(record).ok()?;
            let (about, path) = record.split_once('\t')?;
            let mut about = about.split_whitespace();
            let (mode, kind, blob, size) =
                (about.next()?, about.next()?, about.next()?, about.next()?);
            if kind != "blob" || mode == "120000" {
                return None;
            }
            Some(Entry {
                path: path.to_string(),
                blob: blob.to_string(),
                size: size.parse().ok()?,
                lang: Lang::of(path),
            })
        })
        .collect()
}

/// Reads each of `blobs` from the repository at `root`, in their order,
/// handing each to `each` with its place in `blobs` as it comes.
pub fn each_blob(
    root: &Path,
    blobs: &[String],
    mut each: impl FnMut(usize, Vec<u8>),
) -> Result<()> {
    if blobs.is_empty() {
        return Ok(());
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("couldn't run git")?;
    let mut stdin = child.stdin.take().context("git took no input")?;
    let asked: Vec<String> = blobs.to_vec();
    // Written on a thread of its own, so neither side waits on the other.
    let writer = thread::spawn(move || {
        for blob in asked {
            if writeln!(stdin, "{blob}").is_err() {
                break;
            }
        }
    });
    let mut out = BufReader::new(child.stdout.take().context("git said nothing")?);
    let mut header = String::new();
    for at in 0..blobs.len() {
        header.clear();
        if out.read_line(&mut header)? == 0 {
            bail!("git cat-file stopped after {at} of {} blobs", blobs.len());
        }
        let mut words = header.split_whitespace();
        let (_, kind) = (words.next(), words.next());
        if kind == Some("missing") {
            continue;
        }
        let size: usize = words
            .next()
            .and_then(|size| size.parse().ok())
            .context("git cat-file said something odd")?;
        let mut content = vec![0; size + 1];
        out.read_exact(&mut content)?;
        content.pop();
        each(at, content);
    }
    let _ = writer.join();
    let _ = child.wait();
    Ok(())
}

/// The contents of each of `blobs`, in their order; an empty one for a
/// blob git doesn't have.
pub fn read_blobs(root: &Path, blobs: &[String]) -> Result<Vec<Vec<u8>>> {
    let mut contents = vec![Vec::new(); blobs.len()];
    each_blob(root, blobs, |at, content| contents[at] = content)?;
    Ok(contents)
}

/// The command lines a manifest at `path` says its package installs.
pub fn programs_in(path: &str, text: &str) -> Vec<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut programs = Vec::new();
    match name {
        "Cargo.toml" => {
            let Ok(manifest) = text.parse::<toml::Table>() else {
                return programs;
            };
            if let Some(name) = manifest
                .get("package")
                .and_then(|package| package.get("name"))
            {
                programs.extend(name.as_str().map(String::from));
            }
            for bin in manifest
                .get("bin")
                .and_then(|bin| bin.as_array())
                .into_iter()
                .flatten()
            {
                programs.extend(
                    bin.get("name")
                        .and_then(|name| name.as_str())
                        .map(String::from),
                );
            }
        }
        "package.json" => {
            let Ok(manifest) = serde_json::from_str::<serde_json::Value>(text) else {
                return programs;
            };
            match manifest.get("bin") {
                Some(serde_json::Value::Object(bins)) => programs.extend(bins.keys().cloned()),
                Some(serde_json::Value::String(_)) => {
                    let name = manifest.get("name").and_then(|name| name.as_str());
                    let name = name.map(|name| name.rsplit('/').next().unwrap_or(name));
                    programs.extend(name.map(String::from));
                }
                _ => {}
            }
        }
        "pyproject.toml" => {
            let Ok(manifest) = text.parse::<toml::Table>() else {
                return programs;
            };
            let scripts = manifest
                .get("project")
                .and_then(|project| project.get("scripts"));
            if let Some(scripts) = scripts.and_then(|scripts| scripts.as_table()) {
                programs.extend(scripts.keys().cloned());
            }
        }
        _ => {}
    }
    programs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_s_language_is_its_extension_s() {
        assert_eq!(Lang::of("src/main.rs"), Some(Lang::Rust));
        assert_eq!(Lang::of("web/App.tsx"), Some(Lang::TypeScript));
        assert_eq!(Lang::of("include/x.h"), Some(Lang::Cpp));
        assert_eq!(Lang::of("lib/x.min.js"), None);
        assert_eq!(Lang::of("types/x.d.ts"), None);
        assert_eq!(Lang::of("README.md"), None);
        assert_eq!(Lang::of("Makefile"), None);
    }

    #[test]
    fn a_tree_is_read_without_submodules_or_links() {
        let listed = b"100644 blob aaaa      12\tsrc/main.rs\0\
            160000 commit bbbb       -\tvendor/sub\0\
            120000 blob cccc      7\tlink\0\
            100755 blob dddd    300\tscripts/run.sh\0";
        let entries = parse_tree(listed);
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(paths, ["src/main.rs", "scripts/run.sh"]);
        assert_eq!((entries[0].blob.as_str(), entries[0].size), ("aaaa", 12));
        assert_eq!(entries[1].lang, Some(Lang::Shell));
    }

    #[test]
    fn directories_are_left_out_by_name_or_by_path() {
        let exclude = ["vendor".to_string(), "docs/examples/".to_string()];
        assert!(excluded("vendor/x/y.go", &exclude));
        assert!(excluded("a/vendor/y.go", &exclude));
        assert!(excluded("docs/examples/z.rs", &exclude));
        assert!(!excluded("src/docs/examples/z.rs", &exclude));
        assert!(!excluded("vendor.go", &exclude));
        assert!(!excluded("src/vendored.rs", &exclude));
    }

    #[test]
    fn tests_are_told_by_where_they_are_and_their_names() {
        assert!(is_test("tests/cli.rs"));
        assert!(is_test("pkg/server_test.go"));
        assert!(is_test("test_server.py"));
        assert!(is_test("src/app.test.ts"));
        assert!(is_test("src/__tests__/app.ts"));
        assert!(is_test("src/test/java/AppTest.java"));
        assert!(!is_test("src/testing.rs"));
        assert!(!is_test("src/attest.rs"));
    }

    #[test]
    fn a_file_s_module_is_where_it_is_as_its_language_says() {
        let module = |lang, path| module(lang, path, &[]);
        assert_eq!(module(Lang::Rust, "src/main.rs"), Vec::<String>::new());
        assert_eq!(module(Lang::Rust, "src/session.rs"), ["session"]);
        assert_eq!(module(Lang::Rust, "src/tui/app.rs"), ["tui", "app"]);
        assert_eq!(
            module(Lang::Rust, "src/wiki/index/mod.rs"),
            ["wiki", "index"]
        );
        assert_eq!(
            module(Lang::Rust, "crates/core/src/lib.rs"),
            Vec::<String>::new()
        );
        assert_eq!(module(Lang::Rust, "tests/cli.rs"), ["cli"]);
        assert_eq!(module(Lang::Python, "src/pkg/server.py"), ["pkg", "server"]);
        assert_eq!(module(Lang::Python, "pkg/__init__.py"), ["pkg"]);
        assert_eq!(module(Lang::TypeScript, "web/src/index.ts"), ["src"]);
        assert_eq!(module(Lang::Go, "pkg/x.go"), Vec::<String>::new());
        let package = ["engine".to_string()];
        assert_eq!(super::module(Lang::Go, "pkg/x.go", &package), ["engine"]);
    }

    #[test]
    fn manifests_say_what_their_programs_are_called() {
        let cargo = "[package]\nname = \"crystal\"\n[[bin]]\nname = \"crystal-helper\"\n";
        assert_eq!(
            programs_in("Cargo.toml", cargo),
            ["crystal", "crystal-helper"]
        );
        let npm = r#"{"name": "@scope/tool", "bin": "cli.js"}"#;
        assert_eq!(programs_in("web/package.json", npm), ["tool"]);
        let npm = r#"{"name": "x", "bin": {"x-run": "run.js"}}"#;
        assert_eq!(programs_in("package.json", npm), ["x-run"]);
        let python = "[project]\nname = \"p\"\n[project.scripts]\nptool = \"p:main\"\n";
        assert_eq!(programs_in("pyproject.toml", python), ["ptool"]);
        assert!(programs_in("Cargo.toml", "not toml [").is_empty());
    }
}
