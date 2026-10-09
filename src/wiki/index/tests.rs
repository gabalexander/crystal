use super::*;
use std::fs;
use std::process::Command;

/// A small Rust repository committed in a directory of its own: a session
/// with a method, a config with a table, a clap command line, and two
/// `new`s.
fn repository() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |path: &str, text: &str| {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    );
    write(
        "src/main.rs",
        "mod config;\nmod session;\n\n#[derive(Subcommand)]\nenum Command {\n    /// Send.\n    Send {\n        #[arg(long)]\n        wait: bool,\n    },\n    Wiki {\n        #[command(subcommand)]\n        command: WikiCommand,\n    },\n}\n\n#[derive(Subcommand)]\nenum WikiCommand {\n    Build,\n}\n\nfn main() {}\n",
    );
    write(
        "src/session.rs",
        "pub struct Session {\n    name: String,\n}\n\nimpl Session {\n    pub fn new() -> Session {\n        todo!()\n    }\n\n    pub fn stop(&mut self) {}\n}\n",
    );
    write(
        "src/config.rs",
        "pub struct Config {\n    pub sessions: SessionSettings,\n}\n\npub struct SessionSettings {\n    pub stop_idle_after: String,\n}\n\nimpl Config {\n    pub fn new() -> Config {\n        todo!()\n    }\n}\n",
    );
    write("README.md", "# demo\n");
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(status.status.success(), "git {args:?}");
        String::from_utf8_lossy(&status.stdout).trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "demo"]);
    let commit = git(&["rev-parse", "HEAD"]);
    (dir, commit)
}

fn settings() -> IndexSettings {
    IndexSettings {
        precise: false,
        ..IndexSettings::default()
    }
}

fn target(found: Lookup) -> String {
    match found {
        Lookup::Unique(def) => def.target(),
        Lookup::Ambiguous(defs) => format!("{} things", defs.len()),
        Lookup::Missing => "nothing".to_string(),
    }
}

#[test]
fn a_repository_s_spans_link_to_where_they_re_defined() {
    let (repo, commit) = repository();
    let cache = tempfile::tempdir().unwrap();
    let index = Index::build(repo.path(), &commit, cache.path(), &settings()).unwrap();
    let look = |span: &str, near: &[&str]| {
        let near: Vec<String> = near.iter().map(|path| path.to_string()).collect();
        target(index.lookup(span, &near))
    };
    // Qualified, unique and bare names.
    assert_eq!(look("Session::stop", &[]), "src/session.rs#L10");
    assert_eq!(look("Session::stop()", &[]), "src/session.rs#L10");
    assert_eq!(look("session::Session", &[]), "src/session.rs#L1-L3");
    assert_eq!(look("Session", &[]), "src/session.rs#L1-L3");
    assert_eq!(look("struct Session", &[]), "src/session.rs#L1-L3");
    // Several, one in the files the prose is about, or left unlinked.
    assert_eq!(look("new", &[]), "2 things");
    assert_eq!(look("new", &["src/config.rs"]), "src/config.rs#L10-L12");
    assert_eq!(look("Config::new", &[]), "src/config.rs#L10-L12");
    // Paths, and a module that's a file.
    assert_eq!(look("src/session.rs", &[]), "src/session.rs");
    assert_eq!(look("session.rs", &[]), "src/session.rs");
    assert_eq!(look("src/", &[]), "src");
    assert_eq!(look("src/session.rs:6", &[]), "src/session.rs#L6");
    assert_eq!(look("session", &[]), "src/session.rs");
    assert_eq!(look("docs/missing.md", &[]), "nothing");
    // Config keys and the command line.
    assert_eq!(look("[sessions] stop_idle_after", &[]), "src/config.rs#L6");
    assert_eq!(look("sessions.stop_idle_after", &[]), "src/config.rs#L6");
    assert_eq!(look("demo wiki build", &[]), "src/main.rs#L19");
    assert_eq!(look("demo send --wait", &[]), "src/main.rs#L9");
    assert_eq!(look("--wait", &[]), "src/main.rs#L9");
    // What isn't in the repository.
    assert_eq!(look("Vec<String>", &[]), "nothing");
    assert_eq!(look("cargo test", &[]), "nothing");
    let report = index.report();
    assert_eq!(report[0].language, "Rust");
    assert_eq!((report[0].tier, report[0].files), (Tier::Syntactic, 3));
}

#[test]
fn a_writer_s_links_are_checked_and_its_prompt_outlined() {
    let (repo, commit) = repository();
    let cache = tempfile::tempdir().unwrap();
    let index = Index::build(repo.path(), &commit, cache.path(), &settings()).unwrap();
    let at: Vec<&str> = (index.defined_at("src/session.rs", 7).into_iter())
        .map(|def| def.qualified.as_str())
        .collect();
    assert_eq!(at, ["session::Session::new"]);
    assert!(index.defined_at("src/session.rs", 4).is_empty());
    let outline: Vec<(&str, DefKind)> =
        (index.outline(&["src/session.rs".to_string()]).into_iter())
            .map(|def| (def.name.as_str(), def.kind))
            .collect();
    assert_eq!(
        outline,
        [
            ("session", DefKind::Module),
            ("Session", DefKind::Struct),
            ("name", DefKind::Field),
            ("new", DefKind::Method),
            ("stop", DefKind::Method),
        ]
    );
}

#[test]
fn building_again_reads_only_what_changed() {
    let (repo, commit) = repository();
    let cache = tempfile::tempdir().unwrap();
    Index::build(repo.path(), &commit, cache.path(), &settings()).unwrap();
    let kept = fs::read_to_string(cache.path().join("syntax.json")).unwrap();
    assert!(kept.contains("stop_idle_after"));
    // What's kept is read in place of the blob: a blob kept as having
    // something else is taken at its word.
    let changed = kept.replace("stop_idle_after", "kept_from_before");
    fs::write(cache.path().join("syntax.json"), changed).unwrap();
    let index = Index::build(repo.path(), &commit, cache.path(), &settings()).unwrap();
    assert_eq!(
        target(index.lookup("kept_from_before", &[])),
        "src/config.rs#L6"
    );
    assert!(Index::build(repo.path(), "no-such-commit", cache.path(), &settings()).is_err());
}

#[test]
fn a_link_s_target_has_the_lines_it_spans() {
    let def = |start, end| Def {
        name: "x".into(),
        qualified: "x".into(),
        kind: DefKind::Function,
        path: "src/x.rs".into(),
        start,
        end,
        precise: false,
    };
    assert_eq!(def(0, 0).target(), "src/x.rs");
    assert_eq!(def(4, 4).target(), "src/x.rs#L4");
    assert_eq!(def(4, 9).target(), "src/x.rs#L4-L9");
}
