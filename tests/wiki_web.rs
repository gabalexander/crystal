//! The wiki's web page, `assets/wiki/`: that its files are all there and refer to one another, that the
//! fixture it's built against keeps to the contract a `wiki.json` follows (as does what the synthetic
//! generator makes), and, where node is installed, its markdown renderer's own checks.

use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

fn page_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/wiki")
}

fn read(name: &str) -> String {
    let path = page_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// Says a check was passed over, where `cargo test -- --nocapture` shows it.
fn skipped(why: &str) {
    let _ = writeln!(std::io::stderr(), "skipped: {why}");
}

#[test]
fn the_page_s_files_refer_to_one_another() {
    let html = read("index.html");
    let css = read("style.css");
    let app = read("app.js");
    // Everything is asked for relative to the page, so it works served at /p/<key>/ and exported.
    for asset in [
        "assets/style.css",
        "assets/markdown.js",
        "assets/app.js",
        "assets/google-sans-flex.woff2",
    ] {
        assert!(
            html.contains(&format!("\"{asset}\"")),
            "index.html doesn't load {asset}"
        );
        assert!(
            page_dir().join(&asset["assets/".len()..]).is_file(),
            "{asset} isn't in assets/wiki"
        );
    }
    assert!(
        !Regex::new(r#"(href|src)="/"#).unwrap().is_match(&html),
        "index.html asks for something by an absolute path"
    );
    // The scripts are deferred, so they find the wiki an export inlines before </head>.
    assert!(html.contains("src=\"assets/app.js\" defer"));
    assert!(html.find("assets/markdown.js").unwrap() < html.find("assets/app.js").unwrap());

    // The fonts the stylesheet names are there, with their licences.
    let fonts = Regex::new(r"url\(([a-z0-9-]+\.woff2)\)").unwrap();
    let named: Vec<&str> = fonts
        .captures_iter(&css)
        .map(|c| c.get(1).unwrap().as_str())
        .collect();
    assert_eq!(named.len(), 3, "{named:?}");
    for font in named {
        assert!(
            page_dir().join(font).is_file(),
            "style.css names {font}, which isn't there"
        );
    }
    for licence in ["OFL-google-sans-flex.txt", "OFL-google-sans-code.txt"] {
        assert!(read(licence).contains("SIL Open Font License"), "{licence}");
    }

    // Mermaid is loaded from beside app.js, whichever way the page is served.
    assert!(app.contains("new URL('mermaid.min.js', document.currentScript.src)"));
    assert!(app.contains("window.CrystalMarkdown"));
    assert!(read("markdown.js").contains("root.CrystalMarkdown = api"));

    // Every element app.js looks up, and every icon either uses, is in index.html.
    let ids: HashSet<&str> = Regex::new(r#"id="([^"]+)""#)
        .unwrap()
        .captures_iter(&html)
        .map(|c| c.get(1).unwrap().as_str())
        .collect();
    let looked_up = Regex::new(r"\$\('([a-z-]+)'\)").unwrap();
    let mut count = 0;
    for name in looked_up
        .captures_iter(&app)
        .map(|c| c.get(1).unwrap().as_str())
    {
        assert!(
            ids.contains(format!("cw-{name}").as_str()),
            "app.js looks up cw-{name}, which index.html lacks"
        );
        count += 1;
    }
    assert!(count > 30, "{count}");
    let icons = Regex::new(r"#(cw-i-[a-z-]+)").unwrap();
    for source in [&html, &app] {
        for icon in icons
            .captures_iter(source)
            .map(|c| c.get(1).unwrap().as_str())
        {
            assert!(ids.contains(icon), "the icon {icon} isn't in index.html");
        }
    }
    // The page's own ids are kept apart from a wiki's, which are its anchors.
    for id in &ids {
        assert!(
            id.starts_with("cw-") || *id == "wiki-data",
            "index.html has the id {id}, which a wiki's section could take"
        );
    }
}

/// Checks `wiki` keeps to the contract of a `wiki.json`, version 1, and gives its ids.
fn check_contract(wiki: &Value) -> HashSet<String> {
    assert_eq!(wiki["version"], 1);
    let repo = &wiki["repo"];
    for key in ["name", "root", "commit", "branch"] {
        assert!(
            repo[key].as_str().is_some_and(|s| !s.is_empty()),
            "repo.{key}"
        );
    }
    assert!(Path::new(repo["root"].as_str().unwrap()).is_absolute());
    assert!(
        Regex::new("^[0-9a-f]{40}$")
            .unwrap()
            .is_match(repo["commit"].as_str().unwrap())
    );
    match (&repo["web_url"], &repo["code_url"]) {
        (Value::Null, Value::Null) => {}
        (Value::String(web), Value::String(code)) => {
            assert!(web.starts_with("https://"), "{web}");
            assert!(
                code.starts_with(web.as_str())
                    && code.contains("{commit}")
                    && code.contains("{path}"),
                "{code}"
            );
        }
        other => panic!("web_url and code_url are both strings or both null: {other:?}"),
    }
    let generated = &wiki["generated"];
    assert!(
        Regex::new(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:\d\d)$")
            .unwrap()
            .is_match(generated["at"].as_str().unwrap())
    );
    for key in ["by", "model", "crystal"] {
        assert!(
            generated[key].as_str().is_some_and(|s| !s.is_empty()),
            "generated.{key}"
        );
    }
    assert!(
        generated["cost_usd"]
            .as_f64()
            .is_some_and(|cost| cost >= 0.0)
    );

    let mut ids = HashSet::new();
    let mut texts = vec![text(&wiki["overview"]["summary_md"])];
    check_diagram(&wiki["overview"]["diagram"]);
    let sections = wiki["sections"].as_array().expect("sections");
    assert!(!sections.is_empty());
    let slug = Regex::new("^[a-z0-9]+(-[a-z0-9]+)*$").unwrap();
    for section in sections {
        let id = text(&section["id"]);
        assert!(slug.is_match(&id), "{id}");
        assert!(ids.insert(id.clone()), "the id {id} is there twice");
        assert!(!text(&section["title"]).is_empty());
        texts.push(text(&section["summary_md"]));
        check_diagram(&section["diagram"]);
        for sub in section["subsections"].as_array().expect("subsections") {
            let id = text(&sub["id"]);
            assert!(slug.is_match(&id), "{id}");
            assert!(ids.insert(id.clone()), "the id {id} is there twice");
            assert!(!text(&sub["title"]).is_empty());
            texts.push(text(&sub["body_md"]));
            check_diagram(&sub["diagram"]);
            for file in sub["files"].as_array().expect("files") {
                check_path(file.as_str().expect("a file is a path"));
            }
        }
    }

    // Links into the code name a path in the repository, and lines as #L10 or #L10-L20; a link to
    // another part of the page names one of its ids.
    let link = Regex::new(r"\]\(([^)\s]+)\)").unwrap();
    let lines = Regex::new(r"^L(\d+)(-L(\d+))?$").unwrap();
    for text in &texts {
        for target in link.captures_iter(text).map(|c| c.get(1).unwrap().as_str()) {
            if let Some(code) = target.strip_prefix("code:") {
                let (path, fragment) = code.split_once('#').unwrap_or((code, ""));
                check_path(path);
                if !fragment.is_empty() {
                    let found = lines
                        .captures(fragment)
                        .unwrap_or_else(|| panic!("{target}: lines are #L10 or #L10-L20"));
                    let start: u32 = found[1].parse().unwrap();
                    assert!(start > 0, "{target}");
                    if let Some(end) = found.get(3) {
                        assert!(end.as_str().parse::<u32>().unwrap() >= start, "{target}");
                    }
                }
            } else if let Some(anchor) = target.strip_prefix('#') {
                assert!(ids.contains(anchor), "{target} isn't an id on the page");
            } else {
                assert!(target.starts_with("https://"), "{target}");
            }
        }
    }
    ids
}

fn text(value: &Value) -> String {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{value} isn't text"))
        .to_string()
}

fn check_path(path: &str) {
    assert!(
        !path.is_empty() && !path.starts_with('/') && !path.split('/').any(|part| part == ".."),
        "{path} isn't a path in the repository"
    );
}

fn check_diagram(diagram: &Value) {
    if diagram.is_null() {
        return;
    }
    let source = text(&diagram["mermaid"]);
    let kind = source.split_whitespace().next().unwrap_or_default();
    assert!(
        [
            "flowchart",
            "graph",
            "sequenceDiagram",
            "classDiagram",
            "stateDiagram-v2",
            "stateDiagram",
            "erDiagram"
        ]
        .contains(&kind),
        "a diagram starts with its kind: {source}"
    );
    assert!(!text(&diagram["caption"]).is_empty());
}

#[test]
fn the_fixture_keeps_to_the_contract() {
    let wiki: Value = serde_json::from_str(&read("fixture/wiki.json")).unwrap();
    let ids = check_contract(&wiki);
    let sections = wiki["sections"].as_array().unwrap();
    assert_eq!(sections.len(), 4);
    for section in sections {
        assert_eq!(
            section["subsections"].as_array().unwrap().len(),
            3,
            "{}",
            section["id"]
        );
    }
    assert_eq!(ids.len(), 16);
    // About crystal itself: its links name crystal's own files.
    let all = wiki.to_string();
    let code = Regex::new(r"\(code:([^)#]+)").unwrap();
    let mut linked = 0;
    for path in code.captures_iter(&all).map(|c| c.get(1).unwrap().as_str()) {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        assert!(
            file.exists(),
            "the fixture links to {path}, which crystal doesn't have"
        );
        linked += 1;
    }
    assert!(linked > 100, "{linked}");
}

#[test]
fn the_synthetic_wiki_keeps_to_the_contract() {
    let made = Command::new("python3")
        .arg(page_dir().join("dev/synth.py"))
        .args(["--sections", "3", "--subsections", "8"])
        .output();
    let Ok(made) = made else {
        return skipped("python3 isn't installed");
    };
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let wiki: Value = serde_json::from_slice(&made.stdout).unwrap();
    let ids = check_contract(&wiki);
    assert_eq!(ids.len(), 3 + 8);
}

#[test]
fn the_markdown_renderer_passes_its_checks() {
    let ran = Command::new("node")
        .arg(page_dir().join("test/markdown.test.js"))
        .env_remove("NODE_OPTIONS")
        .output();
    let Ok(ran) = ran else {
        return skipped("node isn't installed");
    };
    let report = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success(),
        "{report}{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(report.contains("# fail 0"), "{report}");
}
