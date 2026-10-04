//! `crystal update`: crystal bringing itself up to date from its releases on
//! GitHub, the way `install.sh` installs it, and the look the TUI takes once
//! a day to say a newer one is out.
//!
//! The latest release is the one GitHub's `releases/latest` redirects to: a
//! plain web redirect, so no token and no API quota. `curl` asks for it and
//! downloads, as the install script and the model's download (`embed.rs`)
//! do. The archive is checked against its published SHA-256, unpacked with
//! `tar`, run once to see that it runs here, and only then put in place of
//! this crystal, with a rename: whatever is running the old file, a daemon
//! among them, goes on running it.
//!
//! The new crystal is the one that then restarts each running daemon and
//! installs the skill, as `restart-server` and `skill --install` do: a
//! daemon hands over only to a crystal that reads what it hands over, and
//! the skill to install is the new crystal's.
//!
//! An update keeps the new release's notes, its body on GitHub, in the state
//! directory, and the TUI shows them, once, the first time it opens on the
//! new crystal; updated some other way, the TUI asks for them itself.

use crate::{embed, printable, server_cli, shell, skill, socket, state};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::IsTerminal;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where crystal's releases are, unless `CRYSTAL_RELEASES` says otherwise:
/// a mirror, or a test's own.
const RELEASES: &str = "https://github.com/gabalexander/crystal/releases";

/// This crystal's version.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How often the TUI looks for a newer crystal, in seconds: once a day.
const LOOK_EVERY: u64 = 24 * 60 * 60;

/// The most asking which release is the latest may take, in seconds.
const ASK_TIMEOUT: &str = "15";

/// What a release's notes are called among its files, for releases kept
/// anywhere but GitHub, which has them as the release's body.
const NOTES_FILE: &str = "release-notes.md";

/// `crystal update`: installs `version`, or else the latest release when
/// it's newer than this crystal; with `check`, only says whether it is;
/// with `notes`, prints what `version`, or this crystal, changed.
pub fn run(socket: &Path, version: Option<String>, check: bool, notes: bool) -> Result<()> {
    let version = version
        .map(|version| {
            let version = version.trim_start_matches('v');
            ensure!(
                numbers(version).is_some(),
                "{version} isn't a release's version, like 0.4.0"
            );
            Ok(version.to_string())
        })
        .transpose()?;
    if notes {
        let version = version.as_deref().unwrap_or(VERSION);
        let body = kept_notes(version)
            .or_else(|| fetch_notes(version))
            .with_context(|| format!("there are no notes for crystal {version} to be found"))?;
        println!("{}", printable::text(&body).trim_end());
        return Ok(());
    }
    let wanted = match &version {
        Some(version) => version.clone(),
        None => latest()?,
    };
    if check {
        if is_newer(&wanted, VERSION) {
            println!(
                "crystal {wanted} is out, and this is {VERSION}: `crystal update` installs it"
            );
        } else {
            println!("crystal {VERSION} is the latest");
        }
        return Ok(());
    }
    // A build from past the latest release isn't taken back to it unless
    // asked for by its version.
    if version.is_none() && !is_newer(&wanted, VERSION) {
        println!("crystal {VERSION} is the latest");
        return Ok(());
    }
    if wanted == VERSION {
        println!("crystal {VERSION} is installed already");
        return Ok(());
    }

    let exe = this_crystal()?;
    let target = target()?;
    let scratch = Scratch::new()?;
    let archive = download(&wanted, &target, scratch.path())?;
    let binary = unpack(&archive, &wanted, &target, scratch.path())?;
    replace(&exe, &binary)?;
    println!(
        "updated crystal {VERSION} to {wanted} in {}",
        shell::home_relative(&exe)
    );
    // What's new, for the TUI to show as it next opens; none is no matter.
    if let Some(body) = fetch_notes(&wanted) {
        keep_notes(&wanted, &body);
        println!(
            "crystal's TUI shows what's new as it next opens: `crystal update --notes` prints it"
        );
    }
    restart_daemons(&exe, socket);
    install_skill(&exe);
    Ok(())
}

/// The latest release's version, like `0.4.0`.
pub fn latest() -> Result<String> {
    let releases = releases();
    let out = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--head",
            "--max-time",
            ASK_TIMEOUT,
        ])
        .args(["--output", "/dev/null", "--write-out", "%{redirect_url}"])
        .arg(format!("{releases}/latest"))
        .stdin(Stdio::null())
        .output()
        .context("couldn't run curl, which asks which crystal is the latest")?;
    ensure!(
        out.status.success(),
        "couldn't reach {releases}: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let redirect = String::from_utf8_lossy(&out.stdout);
    tag_version(redirect.trim()).with_context(|| format!("there's no release at {releases} yet"))
}

/// What the TUI says when a newer crystal than this one is out, after
/// asking which is the latest. Not being able to ask is no news.
pub fn newer_notice() -> Option<String> {
    let latest = latest().ok()?;
    is_newer(&latest, VERSION)
        .then(|| format!("crystal {latest} is out: `crystal update` installs it"))
}

/// When the TUI last looked for a newer crystal, as it keeps it.
#[derive(Debug, Serialize, Deserialize)]
pub struct Looked {
    /// Seconds since the Unix epoch.
    pub at: u64,
}

/// Whether the TUI is due to look for a newer crystal at `now`, seconds
/// since the epoch, given what it kept of its last look.
pub fn due(kept: Option<&str>, now: u64) -> bool {
    let last = kept.and_then(|json| serde_json::from_str::<Looked>(json).ok());
    last.is_none_or(|last| now.saturating_sub(last.at) >= LOOK_EVERY)
}

/// What a release's notes were kept as, by the crystal that updated to it.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct KeptNotes {
    version: String,
    body: String,
}

/// The crystal whose TUI last opened, as the TUI keeps it: the notes of one
/// it hasn't opened as yet are news.
#[derive(Debug, Serialize, Deserialize)]
pub struct Opened {
    pub version: String,
}

impl Opened {
    /// This crystal.
    pub fn now() -> Opened {
        Opened {
            version: VERSION.to_string(),
        }
    }
}

/// Whether what this crystal changed is news to whoever opens its TUI,
/// given the crystal it last opened as, as the TUI kept it, and whether it
/// had opened before at all: a crystal from before this was kept had. A
/// first TUI ever has nothing to compare with, and no news.
pub fn has_news(kept: Option<&str>, opened_before: bool) -> bool {
    let last = kept.and_then(|json| serde_json::from_str::<Opened>(json).ok());
    match last {
        Some(last) => last.version != VERSION,
        None => opened_before,
    }
}

/// What a page of this crystal's notes is titled.
pub fn notes_title() -> String {
    format!("what's new in crystal {VERSION}")
}

/// This crystal's notes, as an update kept them.
pub fn this_crystals_kept_notes() -> Option<String> {
    kept_notes(VERSION)
}

/// This crystal's notes, asked of where the releases are. Not being able
/// to ask is no notes.
pub fn this_crystals_notes() -> Option<String> {
    fetch_notes(VERSION)
}

/// Keeps `body` as the notes of `version`, for the TUI of the crystal it
/// is to show. Not being able to is no matter: the TUI asks for them.
fn keep_notes(version: &str, body: &str) {
    let path = state::release_notes_path();
    let kept = KeptNotes {
        version: version.to_string(),
        body: body.to_string(),
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(&kept) {
        let _ = fs::write(path, json);
    }
}

/// The notes of `version`, if an update kept them.
fn kept_notes(version: &str) -> Option<String> {
    let json = fs::read_to_string(state::release_notes_path()).ok()?;
    let kept: KeptNotes = serde_json::from_str(&json).ok()?;
    (kept.version == version).then_some(kept.body)
}

/// The notes of the release `version`, from where the releases are: none
/// when it has none, or they can't be had.
fn fetch_notes(version: &str) -> Option<String> {
    let (url, json) = notes_url(&releases(), version);
    let mut command = Command::new("curl");
    command.args([
        "--fail",
        "--silent",
        "--location",
        "--max-time",
        ASK_TIMEOUT,
    ]);
    if json {
        command.args(["--header", "Accept: application/vnd.github+json"]);
    }
    let out = command.arg(&url).stdin(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let body = if json {
        release_body(&text)?
    } else {
        text.into_owned()
    };
    let body = body.replace("\r\n", "\n");
    (!body.trim().is_empty()).then(|| body.trim().to_string())
}

/// Where the notes of the release `version` are, and whether they're the
/// body of a release as GitHub's API gives it, in JSON: for releases on
/// GitHub, that; anywhere else, [`NOTES_FILE`] among the release's files.
fn notes_url(releases: &str, version: &str) -> (String, bool) {
    let repo = releases
        .strip_prefix("https://github.com/")
        .and_then(|rest| rest.strip_suffix("/releases"))
        .filter(|repo| repo.split('/').count() == 2);
    match repo {
        Some(repo) => (
            format!("https://api.github.com/repos/{repo}/releases/tags/v{version}"),
            true,
        ),
        None => (
            format!("{releases}/download/v{version}/{NOTES_FILE}"),
            false,
        ),
    }
}

/// The body of a release, from GitHub's JSON for it.
fn release_body(json: &str) -> Option<String> {
    let release: serde_json::Value = serde_json::from_str(json).ok()?;
    Some(release.get("body")?.as_str()?.to_string())
}

/// Where the releases are: GitHub's, or `CRYSTAL_RELEASES`.
fn releases() -> String {
    let releases = std::env::var("CRYSTAL_RELEASES")
        .ok()
        .filter(|releases| !releases.is_empty())
        .unwrap_or_else(|| RELEASES.to_string());
    releases.trim_end_matches('/').to_string()
}

/// The version a release's page is for: `0.4.0` for `…/releases/tag/v0.4.0`.
fn tag_version(url: &str) -> Option<String> {
    let (_, tag) = url.rsplit_once("/tag/")?;
    let version = tag.strip_prefix('v')?;
    numbers(version)?;
    Some(version.to_string())
}

/// A release's version as its numbers: `0.4.0` is `[0, 4, 0]`. Releases are
/// only ever numbers, so anything else isn't one.
fn numbers(version: &str) -> Option<Vec<u64>> {
    version.split('.').map(|part| part.parse().ok()).collect()
}

/// Whether `version` comes after `than`.
fn is_newer(version: &str, than: &str) -> bool {
    match (numbers(version), numbers(than)) {
        (Some(version), Some(than)) => version > than,
        _ => false,
    }
}

/// The release built for this machine, like `aarch64-apple-darwin`, as the
/// install script picks it.
fn target() -> Result<String> {
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-musl",
        other => bail!("there's no release for {other}; build crystal from source instead"),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        // An Intel build run by Rosetta on Apple silicon is better off as
        // the machine's own.
        "x86_64" if os == "apple-darwin" && under_rosetta() => "aarch64",
        "x86_64" => "x86_64",
        other => bail!("there's no release for {other}; build crystal from source instead"),
    };
    Ok(format!("{arch}-{os}"))
}

fn under_rosetta() -> bool {
    Command::new("sysctl")
        .args(["-n", "sysctl.proc_translated"])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "1")
}

/// The crystal to replace: this one, its links followed, when it's one an
/// update can replace.
fn this_crystal() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("can't tell where this crystal is")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    if let Some(how) = updated_by(&exe) {
        bail!("{} {how}", exe.display());
    }
    let dir = exe.parent().context("this crystal isn't in a directory")?;
    let probe = staged_path(&exe);
    match fs::File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            Ok(exe)
        }
        Err(err) => bail!(
            "can't write in {} ({err}): update crystal as whoever installed it there, or install \
             it into a directory of yours with the install script",
            dir.display()
        ),
    }
}

/// How the crystal at `exe` is updated instead, when something that keeps
/// track of it put it there: replacing it behind its back would leave that
/// thinking it's still the old one.
fn updated_by(exe: &Path) -> Option<String> {
    let has = |name: &str| exe.components().any(|part| part.as_os_str() == name);
    let parent = exe.parent()?;
    let cargo_bin = std::env::var_os("CARGO_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .map(|home| home.join("bin"));
    let how = if exe.starts_with("/nix/store") {
        "is in the Nix store: update it through Nix"
    } else if has("Cellar") || has(".linuxbrew") {
        "was installed by Homebrew: `brew upgrade crystal` updates it"
    } else if exe.to_string_lossy().contains("/mise/installs/") {
        "was installed by mise: `mise upgrade crystal` updates it"
    } else if cargo_bin.is_some_and(|bin| same_dir(parent, &bin)) {
        "was installed by cargo: `cargo install --git https://github.com/gabalexander/crystal \
         --force` updates it"
    } else if has("target") && ["debug", "release"].iter().any(|dir| parent.ends_with(dir)) {
        "is a build from source: `git pull`, then `make install`, updates it"
    } else {
        return None;
    };
    Some(how.to_string())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || b.canonicalize().is_ok_and(|b| a == b)
}

/// Downloads the release `version` for `target` into `dir`, and checks it
/// against its checksum.
fn download(version: &str, target: &str, dir: &Path) -> Result<PathBuf> {
    let name = format!("crystal-{version}-{target}.tar.gz");
    let url = format!("{}/download/v{version}/{name}", releases());
    let archive = dir.join(&name);
    let sum = dir.join(format!("{name}.sha256"));
    println!("downloading crystal {version} for {target}");
    fetch(&url, &archive, std::io::stderr().is_terminal())?;
    fetch(&format!("{url}.sha256"), &sum, false)?;
    let expected = fs::read_to_string(&sum)?
        .split_whitespace()
        .next()
        .map(str::to_lowercase)
        .with_context(|| format!("{url}.sha256 is empty"))?;
    let actual = embed::sha256_of(&archive)?;
    ensure!(
        actual == expected,
        "the download doesn't match its checksum, so nothing was changed; try again"
    );
    Ok(archive)
}

/// Downloads `url` to `to`, showing how it goes when `progress` says to.
fn fetch(url: &str, to: &Path, progress: bool) -> Result<()> {
    let status = Command::new("curl")
        .args(["--fail", "--location", "--retry", "3", "--show-error"])
        .arg(if progress {
            "--progress-bar"
        } else {
            "--silent"
        })
        .arg("--output")
        .arg(to)
        .arg(url)
        .stdin(Stdio::null())
        .status()
        .context("couldn't run curl, which downloads crystal")?;
    ensure!(status.success(), "couldn't download {url}");
    Ok(())
}

/// Unpacks `archive` in `dir`, and returns the crystal in it.
fn unpack(archive: &Path, version: &str, target: &str, dir: &Path) -> Result<PathBuf> {
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .stdin(Stdio::null())
        .status()
        .context("couldn't run tar, which unpacks the download")?;
    ensure!(status.success(), "couldn't unpack {}", archive.display());
    let binary = dir
        .join(format!("crystal-{version}-{target}"))
        .join("crystal");
    ensure!(binary.is_file(), "the download has no crystal in it");
    Ok(binary)
}

/// Where the new crystal waits beside `exe` to take its place: in the same
/// directory, so a rename can put it there.
fn staged_path(exe: &Path) -> PathBuf {
    exe.with_file_name(format!(".crystal-update-{}", std::process::id()))
}

/// Puts `binary` in place of `exe`: copied beside it, run there once to see
/// that it runs on this machine, then renamed over it. A rename leaves the
/// old file as it was for whatever is running it, where writing over it
/// would have macOS kill them.
fn replace(exe: &Path, binary: &Path) -> Result<()> {
    let staged = staged_path(exe);
    let replaced = (|| {
        fs::copy(binary, &staged)
            .with_context(|| format!("couldn't write {}", staged.display()))?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
        runs(&staged)?;
        fs::rename(&staged, exe).with_context(|| format!("couldn't replace {}", exe.display()))
    })();
    if replaced.is_err() {
        let _ = fs::remove_file(&staged);
    }
    replaced
}

/// Whether the crystal at `binary` runs here and says it's crystal.
fn runs(binary: &Path) -> Result<()> {
    let out = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .context("the new crystal doesn't run on this machine, so nothing was changed")?;
    ensure!(
        out.status.success() && out.stdout.starts_with(b"crystal "),
        "the new crystal doesn't run on this machine, so nothing was changed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// Has the new crystal at `exe` restart every daemon running: `socket`'s,
/// and every server's. Each is handed over, its sessions carrying on, as
/// `crystal restart-server` does.
fn restart_daemons(exe: &Path, socket: &Path) {
    for daemon in running_daemons(socket) {
        let server = socket::server_of(&daemon).filter(|server| server != socket::DEFAULT);
        let restarted = Command::new(exe)
            .arg("--socket")
            .arg(&daemon)
            .arg("restart-server")
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output();
        match (restarted, &server) {
            (Ok(out), None) if out.status.success() => {
                print!("{}", String::from_utf8_lossy(&out.stdout));
            }
            (Ok(out), Some(server)) if out.status.success() => {
                print!("{server}: {}", String::from_utf8_lossy(&out.stdout));
            }
            (_, None) => eprintln!(
                "crystal: couldn't restart the daemon at {}: run `crystal -S {} restart-server`",
                daemon.display(),
                shell::quote(&daemon.to_string_lossy())
            ),
            (_, Some(server)) => eprintln!(
                "crystal: couldn't restart the server {server}: run `crystal -L {server} \
                 restart-server`"
            ),
        }
    }
}

/// The sockets of the daemons running now, `socket`'s first, then every
/// server's, each once.
fn running_daemons(socket: &Path) -> Vec<PathBuf> {
    let servers = server_cli::names()
        .into_iter()
        .filter_map(|name| socket::of_server(&name).ok());
    let mut seen = Vec::new();
    let mut running = Vec::new();
    for daemon in std::iter::once(socket.to_path_buf()).chain(servers) {
        let same = daemon.canonicalize().unwrap_or_else(|_| daemon.clone());
        if seen.contains(&same) || UnixStream::connect(&daemon).is_err() {
            continue;
        }
        seen.push(same);
        running.push(daemon);
    }
    running
}

/// Has the new crystal at `exe` install its skill, as the install script
/// does: where Claude Code is, unless `CRYSTAL_NO_SKILL=1`. A skill the user
/// changed is kept, and its error says so.
fn install_skill(exe: &Path) {
    if std::env::var_os("CRYSTAL_NO_SKILL").is_some_and(|no| no == "1") || !claude_code_here() {
        return;
    }
    let _ = Command::new(exe)
        .args(["skill", "--install"])
        .stdin(Stdio::null())
        .status();
}

/// Whether Claude Code is on this machine: its command, or its config.
fn claude_code_here() -> bool {
    let on_path = std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("claude").is_file()));
    let config = skill::claude_config_dir(
        std::env::var_os("CLAUDE_CONFIG_DIR"),
        std::env::var_os("HOME"),
    );
    on_path || config.is_some_and(|dir| dir.is_dir())
}

/// A directory of the update's own, for the download, removed once it's
/// done with, however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Scratch> {
        let dir = std::env::temp_dir().join(format!("crystal-update-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).with_context(|| format!("couldn't make {}", dir.display()))?;
        Ok(Scratch(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latest_is_the_tag_its_page_is_for() {
        let page = "https://github.com/gabalexander/crystal/releases/tag/v0.4.0";
        assert_eq!(tag_version(page).as_deref(), Some("0.4.0"));
        // No release yet: GitHub sends you to the list.
        assert_eq!(
            tag_version("https://github.com/gabalexander/crystal/releases"),
            None
        );
        assert_eq!(tag_version(""), None);
        assert_eq!(tag_version("https://example.com/tag/nightly"), None);
    }

    #[test]
    fn a_releases_notes_are_its_body_on_github_and_a_file_beside_it_elsewhere() {
        assert_eq!(
            notes_url("https://github.com/gabalexander/crystal/releases", "0.4.0"),
            (
                "https://api.github.com/repos/gabalexander/crystal/releases/tags/v0.4.0".into(),
                true
            )
        );
        assert_eq!(
            notes_url("http://127.0.0.1:9/mirror", "0.4.0"),
            (
                "http://127.0.0.1:9/mirror/download/v0.4.0/release-notes.md".into(),
                false
            )
        );
        let json = r###"{"tag_name": "v0.4.0", "body": "## Added\r\n- phones"}"###;
        assert_eq!(release_body(json).as_deref(), Some("## Added\r\n- phones"));
        assert_eq!(release_body(r#"{"message": "Not Found"}"#), None);
    }

    #[test]
    fn whats_new_is_news_once_after_an_update_and_never_on_a_first_tui() {
        let opened = |version: &str| {
            serde_json::to_string(&Opened {
                version: version.into(),
            })
            .unwrap()
        };
        assert!(has_news(Some(&opened("0.0.1")), true));
        assert!(!has_news(Some(&opened(VERSION)), true));
        // A TUI opened before, by a crystal that kept no version.
        assert!(has_news(None, true));
        assert!(!has_news(None, false));
    }

    #[test]
    fn versions_are_compared_by_their_numbers() {
        assert!(is_newer("0.4.0", "0.3.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("0.3.0", "0.3.0"));
        assert!(!is_newer("0.2.9", "0.3.0"));
        assert!(!is_newer("0.4.0-rc.1", "0.3.0"));
    }

    #[test]
    fn the_tui_looks_once_a_day() {
        assert!(due(None, 1_000_000));
        assert!(due(Some("not json"), 1_000_000));
        let kept = serde_json::to_string(&Looked { at: 1_000_000 }).unwrap();
        assert!(!due(Some(&kept), 1_000_000 + 60));
        assert!(!due(Some(&kept), 1_000_000 + LOOK_EVERY - 1));
        assert!(due(Some(&kept), 1_000_000 + LOOK_EVERY));
        // A clock set back doesn't look again.
        assert!(!due(Some(&kept), 999_000));
    }

    #[test]
    fn what_installed_crystal_updates_it() {
        let says = |path: &str| updated_by(Path::new(path)).unwrap_or_default();
        assert!(says("/opt/homebrew/Cellar/crystal/0.3.0/bin/crystal").contains("brew upgrade"));
        assert!(says("/home/linuxbrew/.linuxbrew/bin/crystal").contains("brew upgrade"));
        assert!(says("/nix/store/abc-crystal-0.3.0/bin/crystal").contains("Nix"));
        assert!(
            says("/home/me/.local/share/mise/installs/crystal/0.3.0/bin/crystal")
                .contains("mise upgrade")
        );
        assert!(says("/home/me/code/crystal/target/release/crystal").contains("make install"));
        assert!(says("/home/me/code/crystal/target/debug/crystal").contains("make install"));
        assert_eq!(updated_by(Path::new("/home/me/.local/bin/crystal")), None);
        assert_eq!(updated_by(Path::new("/usr/local/bin/crystal")), None);
    }

    #[test]
    fn a_crystal_cargo_installed_is_updated_by_cargo() {
        let home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
            .unwrap();
        let exe = home.join("bin/crystal");
        assert!(updated_by(&exe).unwrap().contains("cargo install"));
    }

    #[test]
    fn the_release_for_this_machine_is_one_that_is_built() {
        let built = [
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "x86_64-unknown-linux-musl",
            "aarch64-unknown-linux-musl",
        ];
        assert!(built.contains(&target().unwrap().as_str()));
        // The workflow builds each, and the Homebrew formula installs each,
        // by the names the install script and an update download.
        let workflow = include_str!("../.github/workflows/release.yml");
        let formula = include_str!("../packaging/homebrew/crystal.rb");
        for target in built {
            assert!(workflow.contains(&format!("target: {target}")), "{target}");
            let archive = format!("/download/v@VERSION@/crystal-@VERSION@-{target}.tar.gz\"");
            assert!(formula.contains(&archive), "{target}");
            assert!(
                formula.contains(&format!("sha256 \"@SHA256_{target}@\"")),
                "{target}"
            );
        }
    }
}
