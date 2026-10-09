//! The wiki's page as crystal ships it, and `crystal wiki export`: the
//! page's own files, built into crystal from `assets/wiki/`, and mermaid,
//! which draws its diagrams, downloaded once into crystal's cache at a
//! pinned version, its SHA-256 checked, as the memory models are (see
//! [`crate::embed`]). `crystal wiki serve` serves both; an export writes
//! them out beside the wiki, as a site that works from `file://` and on
//! any static host.
//!
//! The page asks for everything relative to itself: `assets/<file>`,
//! `wiki.json` and `api/…`. Served, it's at `/p/<project>/`; exported, it's
//! `index.html` with `assets/` beside it, and the wiki inlined in it as
//! `<script type="application/json" id="wiki-data">`, since a page opened
//! from a file can't fetch another; a page that finds that script knows
//! nobody serves it, and that asking needs `crystal wiki serve`.

use crate::embed;
use crate::output::{errln, outln};
use crate::project;
use crate::wiki;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// The page's own files, by their names under `assets/`: the page, its
/// markdown renderer, its script and stylesheet, and its fonts with their
/// licences (Google Sans Flex and Google Sans Code, under the SIL Open Font
/// License, which asks that the licence go with them).
pub const PAGE_FILES: &[(&str, &[u8])] = &[
    ("index.html", include_bytes!("../assets/wiki/index.html")),
    ("app.js", include_bytes!("../assets/wiki/app.js")),
    ("markdown.js", include_bytes!("../assets/wiki/markdown.js")),
    ("style.css", include_bytes!("../assets/wiki/style.css")),
    (
        "google-sans-flex.woff2",
        include_bytes!("../assets/wiki/google-sans-flex.woff2"),
    ),
    (
        "google-sans-code-400.woff2",
        include_bytes!("../assets/wiki/google-sans-code-400.woff2"),
    ),
    (
        "google-sans-code-500.woff2",
        include_bytes!("../assets/wiki/google-sans-code-500.woff2"),
    ),
    (
        "OFL-google-sans-flex.txt",
        include_bytes!("../assets/wiki/OFL-google-sans-flex.txt"),
    ),
    (
        "OFL-google-sans-code.txt",
        include_bytes!("../assets/wiki/OFL-google-sans-code.txt"),
    ),
];

/// The page's own file called `name`, if there's one.
pub fn page_file(name: &str) -> Option<&'static [u8]> {
    PAGE_FILES
        .iter()
        .find(|(file, _)| *file == name)
        .map(|(_, bytes)| *bytes)
}

/// The page, `index.html`.
pub fn index() -> &'static [u8] {
    page_file("index.html").expect("the page has its index.html")
}

/// What a file is, by its name, for a `Content-Type`.
pub fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt" | "log") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The mermaid build the page draws its diagrams with: one file, which
/// sets `window.mermaid`.
pub const MERMAID: Download = Download {
    name: "mermaid.min.js",
    version: "11.17.2",
    url: "https://cdn.jsdelivr.net/npm/mermaid@11.17.2/dist/mermaid.min.js",
    size: 3_572_661,
    sha256: "581ed7d74bd9048d0e3a91363927d72ef22942d7722546b27f7cc29e35390eb8",
};

/// The variable that keeps crystal from downloading mermaid, as its tests
/// set: what isn't in the cache already is missing.
pub const NO_DOWNLOAD: &str = "CRYSTAL_NO_MERMAID_DOWNLOAD";

/// A file the page needs that crystal downloads rather than ships, at a
/// pinned version, with its size and SHA-256.
pub struct Download {
    /// Its name under `assets/`.
    pub name: &'static str,
    pub version: &'static str,
    url: &'static str,
    size: u64,
    sha256: &'static str,
}

impl Download {
    /// Where it's kept in `cache`, crystal's cache: under its version, so
    /// another never mixes with it.
    fn path(&self, cache: &Path) -> PathBuf {
        cache.join("wiki").join(self.version).join(self.name)
    }

    /// Where it's kept, when there's a cache to keep it in.
    fn kept(&self) -> Option<PathBuf> {
        Some(self.path(&embed::crystal_cache()?))
    }

    /// Whether it's at `path`, the size it should be. Its hash was checked
    /// as it was downloaded.
    fn is_at(&self, path: &Path) -> bool {
        fs::metadata(path).is_ok_and(|meta| meta.len() == self.size)
    }
}

/// Mermaid for a server's page: found in the cache, or downloaded the
/// first time it's wanted, once, whoever wants it meanwhile waiting.
pub struct Mermaid {
    /// Why it couldn't be had, once trying has failed: it isn't tried
    /// again while the server runs.
    failed: Mutex<Option<String>>,
}

impl Mermaid {
    pub fn new() -> Mermaid {
        Mermaid {
            failed: Mutex::new(None),
        }
    }

    /// The file, downloading it first if it isn't in the cache, or why it
    /// can't be had.
    pub fn get(&self) -> Result<PathBuf, String> {
        let mut failed = self.failed.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(why) = failed.as_ref() {
            return Err(why.clone());
        }
        let got = fetch(&MERMAID).map_err(|err| format!("{err:#}"));
        if let Err(why) = &got {
            *failed = Some(why.clone());
        }
        got
    }
}

/// `download`, from the cache, or downloaded into it with `curl` unless
/// [`NO_DOWNLOAD`] is set: beside its place first, and moved there only once
/// its hash is right.
fn fetch(download: &Download) -> Result<PathBuf> {
    let path = download
        .kept()
        .context("can't tell where to keep mermaid: HOME isn't set")?;
    if download.is_at(&path) {
        return Ok(path);
    }
    if std::env::var_os(NO_DOWNLOAD).is_some() {
        bail!(
            "{} isn't downloaded, and {NO_DOWNLOAD} is set",
            download.name
        );
    }
    let dir = path.parent().context("its place has a directory")?;
    fs::create_dir_all(dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    let partial = path.with_extension("part");
    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--retry",
            "3",
            "--silent",
            "--show-error",
        ])
        .arg("--output")
        .arg(&partial)
        .arg(download.url)
        .status()
        .context("couldn't run curl, which downloads mermaid")?;
    if !status.success() {
        let _ = fs::remove_file(&partial);
        bail!("couldn't download {}", download.url);
    }
    let sha256 = embed::sha256_of(&partial)?;
    if sha256 != download.sha256 {
        let _ = fs::remove_file(&partial);
        bail!(
            "{} came with the wrong SHA-256: {sha256}, not {}",
            download.url,
            download.sha256
        );
    }
    fs::rename(&partial, &path)?;
    Ok(path)
}

/// The page with `wiki`, the text of a `wiki.json`, inlined in it, for a
/// page read from a file: in a script tag of its own before `</head>`,
/// every `<` in it written `\u003c`, so nothing in the wiki's text can end
/// the tag. JSON's strings are the only place a `<` can be.
pub fn inlined(page: &str, wiki: &str) -> String {
    let data = wiki.trim_end().replace('<', "\\u003c");
    let tag = format!("<script type=\"application/json\" id=\"wiki-data\">{data}</script>\n");
    match page.find("</head>") {
        Some(at) => format!("{}{tag}{}", &page[..at], &page[at..]),
        None => format!("{tag}{page}"),
    }
}

/// `crystal wiki export`: writes the wiki of the project `dir` is in as a
/// site in `out`: `index.html` with the wiki inlined, `wiki.json` beside it,
/// and the page's files and mermaid under `assets/`. Without mermaid, which
/// can't always be downloaded, the diagrams are left as their source, and
/// the command says so.
pub fn export(socket: &Path, dir: Option<PathBuf>, out: &Path) -> Result<()> {
    let dir = match dir {
        Some(dir) => dir,
        None => std::env::current_dir().context("couldn't tell the current directory")?,
    };
    let project = project::of(&dir);
    let source = wiki::dir(socket, &project.path).join(wiki::WIKI_FILE);
    let text = match fs::read_to_string(&source) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => bail!(
            "{} has no wiki yet: `crystal wiki build` writes one",
            project.name
        ),
        Err(err) => return Err(err).with_context(|| format!("couldn't read {}", source.display())),
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .with_context(|| format!("{} isn't JSON", source.display()))?;
    let assets = out.join("assets");
    fs::create_dir_all(&assets).with_context(|| format!("couldn't make {}", assets.display()))?;
    let write = |path: PathBuf, bytes: &[u8]| {
        fs::write(&path, bytes).with_context(|| format!("couldn't write {}", path.display()))
    };
    let page = String::from_utf8_lossy(index()).into_owned();
    write(out.join("index.html"), inlined(&page, &text).as_bytes())?;
    write(out.join(wiki::WIKI_FILE), text.as_bytes())?;
    for (name, bytes) in PAGE_FILES {
        write(assets.join(name), bytes)?;
    }
    // GitHub Pages leaves out files whose names start with `_` otherwise.
    write(out.join(".nojekyll"), b"")?;
    match fetch(&MERMAID) {
        Ok(mermaid) => {
            let to = assets.join(MERMAID.name);
            fs::copy(&mermaid, &to).with_context(|| format!("couldn't write {}", to.display()))?;
        }
        Err(err) => errln!("the diagrams are left as their source: {err:#}"),
    }
    outln!(
        "wrote {}'s wiki to {}: open {}",
        project.name,
        out.display(),
        out.join("index.html").display()
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `assets/wiki/` has beside the page, for working on it, which
    /// isn't built in: its README, the fixture it's built against, its
    /// development scripts and its tests.
    const NOT_THE_PAGE: &[&str] = &["README.md", "dev", "fixture", "test"];

    #[test]
    fn every_file_in_assets_wiki_is_built_in() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/wiki");
        let mut files: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| !NOT_THE_PAGE.contains(&name.as_str()))
            .collect();
        files.sort();
        let mut built_in: Vec<String> = PAGE_FILES
            .iter()
            .map(|(name, _)| name.to_string())
            .collect();
        built_in.sort();
        assert_eq!(files, built_in, "PAGE_FILES lists assets/wiki/");
        assert!(page_file("index.html").is_some());
        assert!(page_file("../wiki.json").is_none());
    }

    #[test]
    fn a_file_s_type_is_by_its_extension() {
        assert_eq!(content_type("app.js"), "text/javascript; charset=utf-8");
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type("style.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("font.woff2"), "font/woff2");
        assert_eq!(content_type("LICENSE"), "application/octet-stream");
    }

    #[test]
    fn the_wiki_is_inlined_before_the_head_ends_and_can_t_end_its_tag() {
        let page = "<html><head><title>w</title></head><body></body></html>";
        let wiki = "{\"t\":\"</script><script>alert(1)</script>\"}\n";
        let inlined = inlined(page, wiki);
        assert_eq!(
            inlined,
            "<html><head><title>w</title><script type=\"application/json\" id=\"wiki-data\">\
             {\"t\":\"\\u003c/script>\\u003cscript>alert(1)\\u003c/script>\"}</script>\n\
             </head><body></body></html>"
        );
        let start = inlined.find("id=\"wiki-data\">").unwrap() + "id=\"wiki-data\">".len();
        let end = inlined[start..].find("</script>").unwrap();
        let data: serde_json::Value = serde_json::from_str(&inlined[start..start + end]).unwrap();
        assert_eq!(data["t"], "</script><script>alert(1)</script>");
    }

    #[test]
    fn mermaid_is_kept_under_its_version_and_known_by_its_size() {
        let cache = tempfile::tempdir().unwrap();
        let path = MERMAID.path(cache.path());
        assert_eq!(
            path,
            cache.path().join("wiki/11.17.2/mermaid.min.js"),
            "beside the models, under its version"
        );
        assert!(!MERMAID.is_at(&path));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"too short").unwrap();
        assert!(!MERMAID.is_at(&path));
        fs::write(&path, vec![b' '; MERMAID.size as usize]).unwrap();
        assert!(MERMAID.is_at(&path));
    }
}
