//! The files a wiki is written about: those git has at the commit, less
//! what's excluded, each with its lines and the hash git gives its
//! content. Which of them are source files, the ones every wiki must
//! cover; what a subsection's paths, files and directories, cover; and
//! the globs `[wiki] exclude` takes.

use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;

/// What's never written about, whatever the config says: what a person
/// doesn't read to understand the code.
pub const ALWAYS_EXCLUDED: &[&str] = &[
    ".git/",
    "node_modules/",
    "vendor/",
    "third_party/",
    "*.lock",
    "*-lock.json",
    "*-lock.yaml",
    "go.sum",
    "*.min.js",
    "*.min.css",
    "*.map",
    "*.snap",
];

/// A file larger than this is listed but never read whole: it's data, or
/// generated.
pub const MAX_READ: u64 = 1024 * 1024;

/// One file at the commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// From the top of the repository.
    pub path: String,
    /// The hash git gives its content: what tells a file changed.
    pub blob: String,
    /// How many lines it has; 0 for a binary file or one too big to read.
    pub lines: u32,
    pub bytes: u64,
    /// Whether it's text: neither binary nor too big to read.
    pub text: bool,
}

/// Every file a wiki is written about, by path.
#[derive(Debug, Clone, Default)]
pub struct Files {
    pub files: BTreeMap<String, File>,
    /// Every directory that holds one of them, without a `/` at its end.
    pub dirs: BTreeSet<String>,
}

impl Files {
    /// The files git has in the checkout at `root`, less those `exclude`'s
    /// globs and [`ALWAYS_EXCLUDED`] match.
    pub fn list(root: &Path, exclude: &[String]) -> Result<Files> {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["-c", "core.quotePath=false", "ls-files", "-s", "-z"])
            .output()
            .context("couldn't run git")?;
        if !out.status.success() {
            bail!(
                "git ls-files failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let listed = String::from_utf8_lossy(&out.stdout);
        let mut files = Files::default();
        for entry in listed.split('\0').filter(|entry| !entry.is_empty()) {
            // `<mode> <blob> <stage>\t<path>`
            let Some((head, path)) = entry.split_once('\t') else {
                continue;
            };
            let mut head = head.split(' ');
            let mode = head.next().unwrap_or_default();
            let blob = head.next().unwrap_or_default();
            // A submodule is a commit, not a file.
            if mode == "160000" || excluded(path, exclude) {
                continue;
            }
            let (lines, bytes, text) = measure(&root.join(path));
            files.add(File {
                path: path.to_string(),
                blob: blob.to_string(),
                lines,
                bytes,
                text,
            });
        }
        Ok(files)
    }

    /// Adds `file`, and the directories it's in.
    pub fn add(&mut self, file: File) {
        let mut dir = file.path.as_str();
        while let Some((parent, _)) = dir.rsplit_once('/') {
            self.dirs.insert(parent.to_string());
            dir = parent;
        }
        self.files.insert(file.path.clone(), file);
    }

    pub fn get(&self, path: &str) -> Option<&File> {
        self.files.get(path)
    }

    pub fn is_dir(&self, path: &str) -> bool {
        self.dirs.contains(path.trim_end_matches('/'))
    }

    /// The source files: those every wiki must cover.
    pub fn sources(&self) -> impl Iterator<Item = &File> {
        self.files.values().filter(|file| is_source(file))
    }

    /// `path` as one of the paths a subsection covers: a file's as it is, a
    /// directory's ending with `/`; `None` when it's neither here. A `./`
    /// or `/` before it is taken off.
    pub fn entry(&self, path: &str) -> Option<String> {
        let path = path.trim().trim_start_matches("./").trim_start_matches('/');
        if self.files.contains_key(path) {
            return Some(path.to_string());
        }
        let dir = path.trim_end_matches('/');
        if dir.is_empty() || dir == "." {
            return Some(String::new());
        }
        self.is_dir(dir).then(|| format!("{dir}/"))
    }

    /// The files `entries` cover, each once, in order.
    pub fn covered<'a>(&'a self, entries: &[String]) -> Vec<&'a File> {
        self.files
            .values()
            .filter(|file| covers(entries, &file.path))
            .collect()
    }
}

/// Whether one of `entries`, files and directories ending with `/` (the
/// empty one being the whole repository), covers `path`.
pub fn covers(entries: &[String], path: &str) -> bool {
    entries.iter().any(|entry| {
        if entry.is_empty() {
            return true;
        }
        match entry.strip_suffix('/') {
            Some(dir) => path
                .strip_prefix(dir)
                .is_some_and(|rest| rest.starts_with('/')),
            None => entry == path,
        }
    })
}

/// Its lines, its size and whether it's text, read from the disk.
fn measure(path: &Path) -> (u32, u64, bool) {
    let Ok(meta) = fs::metadata(path) else {
        return (0, 0, false);
    };
    let bytes = meta.len();
    if !meta.is_file() || bytes > MAX_READ {
        return (0, bytes, false);
    }
    let mut content = Vec::new();
    if fs::File::open(path)
        .and_then(|mut file| file.read_to_end(&mut content))
        .is_err()
    {
        return (0, bytes, false);
    }
    if content.iter().take(8000).any(|&byte| byte == 0) {
        return (0, bytes, false);
    }
    let mut lines = content.iter().filter(|&&byte| byte == b'\n').count() as u32;
    if content.last().is_some_and(|&byte| byte != b'\n') {
        lines += 1;
    }
    (lines, bytes, true)
}

/// The extensions of the files a wiki must cover: code, in the languages
/// people write it in.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "go", "py", "pyi", "ts", "tsx", "js", "jsx", "mjs", "cjs", "java", "kt", "kts", "scala",
    "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "m", "mm", "swift", "rb", "sh", "bash", "zsh",
    "fish", "lua", "php", "cs", "fs", "ex", "exs", "erl", "hs", "ml", "clj", "dart", "r", "jl",
    "zig", "nim", "sql", "proto", "vue", "svelte", "nix", "pl", "ps1",
];

/// Build files named for what they are, without an extension that says.
const SOURCE_NAMES: &[&str] = &[
    "Makefile",
    "makefile",
    "GNUmakefile",
    "Dockerfile",
    "CMakeLists.txt",
    "Justfile",
    "justfile",
    "Rakefile",
    "BUILD",
    "BUILD.bazel",
];

/// Whether `file` is source: text in a language people write, or a build
/// file. Docs, data and configuration may be written about, but needn't be.
pub fn is_source(file: &File) -> bool {
    if !file.text {
        return false;
    }
    let name = file.path.rsplit('/').next().unwrap_or(&file.path);
    if SOURCE_NAMES.contains(&name) {
        return true;
    }
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => {
            SOURCE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        }
        _ => false,
    }
}

/// Whether `path` is excluded: by one of `exclude`'s globs or of
/// [`ALWAYS_EXCLUDED`].
pub fn excluded(path: &str, exclude: &[String]) -> bool {
    ALWAYS_EXCLUDED.iter().any(|glob| matches(glob, path))
        || exclude.iter().any(|glob| matches(glob, path))
}

/// Whether the glob `glob` matches `path`, the way `.gitignore` reads one:
/// `*` within a name, `**` across directories, `?` one character; one
/// with no `/` but at its end matches a name at any depth; one ending with
/// `/` matches a directory and everything in it.
pub fn matches(glob: &str, path: &str) -> bool {
    let glob = glob.trim().trim_start_matches("./");
    if glob.is_empty() {
        return false;
    }
    let (glob, dir_only) = match glob.strip_suffix('/') {
        Some(dir) => (dir, true),
        None => (glob, false),
    };
    let anchored = glob.starts_with('/') || glob.contains('/');
    let glob = glob.trim_start_matches('/');
    let parts: Vec<&str> = path.split('/').collect();
    // A directory's glob matches the directories a file is in; a file's,
    // the file or any directory it's in, as `.gitignore` excludes what's
    // under an excluded directory.
    let last = if dir_only {
        parts.len() - 1
    } else {
        parts.len()
    };
    if !anchored {
        return parts[..last].iter().any(|part| match_name(glob, part));
    }
    let glob: Vec<&str> = glob.split('/').collect();
    (1..=last).any(|end| match_parts(&glob, &parts[..end]))
}

/// Whether the glob's parts match the path's, `**` standing for any number
/// of directories.
fn match_parts(glob: &[&str], parts: &[&str]) -> bool {
    match glob.split_first() {
        None => parts.is_empty(),
        Some((&"**", rest)) => (0..=parts.len()).any(|skip| match_parts(rest, &parts[skip..])),
        Some((first, rest)) => match parts.split_first() {
            Some((part, others)) => match_name(first, part) && match_parts(rest, others),
            None => false,
        },
    }
}

/// Whether `glob`, `*` and `?` in it, matches the name `name`.
fn match_name(glob: &str, name: &str) -> bool {
    let glob: Vec<char> = glob.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut g, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if g < glob.len() && (glob[g] == '?' || glob[g] == name[n]) {
            g += 1;
            n += 1;
        } else if g < glob.len() && glob[g] == '*' {
            star = Some((g, n));
            g += 1;
        } else if let Some((at, matched)) = star {
            g = at + 1;
            n = matched + 1;
            star = Some((at, matched + 1));
        } else {
            return false;
        }
    }
    while g < glob.len() && glob[g] == '*' {
        g += 1;
    }
    g == glob.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> File {
        File {
            path: path.into(),
            blob: "b".into(),
            lines: 10,
            bytes: 100,
            text: true,
        }
    }

    #[test]
    fn globs_match_as_gitignore_reads_them() {
        assert!(matches("*.lock", "Cargo.lock"));
        assert!(matches("*.lock", "sub/yarn.lock"));
        assert!(!matches("*.lock", "src/lock.rs"));
        assert!(matches("vendor/", "vendor/a/b.go"));
        assert!(matches("vendor/", "x/vendor/a.go"));
        assert!(!matches("vendor/", "vendor"));
        assert!(matches("tests/mermaid", "tests/mermaid/a.txt"));
        assert!(matches("docs/*.md", "docs/a.md"));
        assert!(!matches("docs/*.md", "docs/sub/a.md"));
        assert!(matches("docs/**/*.md", "docs/sub/deeper/a.md"));
        assert!(matches("docs/**/*.md", "docs/a.md"));
        assert!(matches("**/fixtures/**", "a/fixtures/x.json"));
        assert!(matches("src/gen?.rs", "src/gen1.rs"));
        assert!(matches("/build", "build/out.c"));
        assert!(!matches("/build", "src/build/out.c"));
        assert!(!matches("", "a"));
    }

    #[test]
    fn what_a_subsection_covers_is_its_files_and_what_its_directories_hold() {
        let entries = vec!["src/tui/".to_string(), "src/main.rs".to_string()];
        assert!(covers(&entries, "src/tui/app.rs"));
        assert!(covers(&entries, "src/tui/app/commands.rs"));
        assert!(covers(&entries, "src/main.rs"));
        assert!(!covers(&entries, "src/tuition.rs"));
        assert!(!covers(&entries, "src/main.rs.bak"));
        assert!(covers(&[String::new()], "anything"));
    }

    #[test]
    fn source_is_code_and_build_files_not_docs_or_data() {
        assert!(is_source(&file("src/a.rs")));
        assert!(is_source(&file("Makefile")));
        assert!(is_source(&file("scripts/install.sh")));
        assert!(!is_source(&file("README.md")));
        assert!(!is_source(&file("config.toml")));
        assert!(!is_source(&file(".rs")));
        let binary = File {
            text: false,
            ..file("a.rs")
        };
        assert!(!is_source(&binary));
    }

    #[test]
    fn an_entry_is_a_file_or_a_directory_that_is_there() {
        let mut files = Files::default();
        files.add(file("src/tui/app.rs"));
        files.add(file("src/main.rs"));
        assert_eq!(files.entry("./src/main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!(files.entry("src/tui").as_deref(), Some("src/tui/"));
        assert_eq!(files.entry("/src/").as_deref(), Some("src/"));
        assert_eq!(files.entry("src/gone.rs"), None);
        assert_eq!(files.covered(&["src/tui/".into()]).len(), 1);
    }

    #[test]
    fn the_files_are_git_s_with_their_lines_less_what_is_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(root)
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        git(&["init", "-q"]);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        fs::write(root.join("src/gen.rs"), "x").unwrap();
        fs::write(root.join("Cargo.lock"), "lock").unwrap();
        fs::write(root.join("logo.png"), [0u8, 1, 2]).unwrap();
        git(&["add", "."]);
        let files = Files::list(root, &["src/gen.rs".into()]).unwrap();
        let paths: Vec<&str> = files.files.keys().map(String::as_str).collect();
        assert_eq!(paths, ["logo.png", "src/a.rs"]);
        let a = files.get("src/a.rs").unwrap();
        assert_eq!((a.lines, a.text, a.blob.len()), (2, true, 40));
        assert!(!files.get("logo.png").unwrap().text);
        assert_eq!(files.sources().count(), 1);
    }
}
